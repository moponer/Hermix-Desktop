//! 本地文件搜索（hermix-filesearch.exe 提权守护进程）的宿主侧客户端。
//!
//! 生命周期：启动预热 / 首次请求时若 helper 未运行 → `ShellExecuteExW("runas")`
//! 拉起（UAC 弹一次）→ helper bind 127.0.0.1:0 并把 port+token 写进
//! `%LOCALAPPDATA%\hermix\filesearch\endpoint.json` → 本模块轮询该文件握手。
//! 跨提权边界无法继承 stdio 管道，因此走回环 TCP + token（仅本机同用户可达）。

use std::{
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    time::Duration,
};

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

/// helper 数据目录（与 daemon.rs 的 default_data_dir 保持一致）。
fn data_dir() -> PathBuf {
    std::env::var("LOCALAPPDATA")
        .ok()
        .filter(|local| !local.is_empty())
        .map(|local| PathBuf::from(local).join("hermix").join("filesearch"))
        .unwrap_or_else(|| PathBuf::from(".hermix-filesearch-data"))
}

fn endpoint_path() -> PathBuf {
    data_dir().join("endpoint.json")
}

/// 定位 helper exe（与 desktop sidecar.rs 同款三级候选：env → dev target → 安装目录旁）。
fn locate_exe() -> Result<PathBuf, String> {
    if let Ok(path) = std::env::var("HERMIX_FILESEARCH_BIN") {
        if !path.is_empty() {
            return Ok(PathBuf::from(path));
        }
    }
    if cfg!(debug_assertions) {
        let ws_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let candidates = [
            ws_root.join("target/debug/hermix-filesearch.exe"),
            ws_root.join("target/release/hermix-filesearch.exe"),
        ];
        return candidates
            .iter()
            .filter(|p| p.exists())
            .max_by_key(|p| {
                p.metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
            })
            .cloned()
            .ok_or_else(|| {
                "hermix-filesearch.exe 不存在；先构建：cargo build -p hermix-filesearch --bin hermix-filesearch".to_string()
            });
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default();
    Ok(exe_dir.join("hermix-filesearch.exe"))
}

#[derive(Debug, Clone, Deserialize)]
struct Endpoint {
    port: u16,
    token: String,
    pid: u32,
}

fn read_endpoint() -> Result<Endpoint, String> {
    let raw = std::fs::read_to_string(endpoint_path())
        .map_err(|_| "endpoint.json 不存在（helper 未启动）".to_string())?;
    serde_json::from_str::<Endpoint>(&raw).map_err(|error| format!("endpoint.json 解析失败：{error}"))
}

/// pid 探活 + 端口握手。
async fn endpoint_alive(endpoint: &Endpoint) -> bool {
    if !process_alive(endpoint.pid) {
        return false;
    }
    tokio::net::TcpStream::connect(("127.0.0.1", endpoint.port))
        .await
        .is_ok()
}

fn process_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(handle) => {
                let _ = CloseHandle(handle);
                true
            }
            Err(_) => false,
        }
    }
}

/// 拉起提权 helper（UAC 弹一次；已是管理员则静默）。SW_HIDE：控制台子系统
/// 的 exe 默认会亮出自己的黑框——用户看到黑框只会去关它，关掉 = daemon 被
/// 杀（daemon 侧 serve 模式还有自藏控制台的二道保险）。
fn spawn_elevated(exe: &Path, args: &str) -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let exe_w: Vec<u16> = exe
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let args_w: Vec<u16> = std::ffi::OsStr::new(args)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let verb_w: Vec<u16> = "runas\0".encode_utf16().collect();

    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI,
        lpVerb: PCWSTR(verb_w.as_ptr()),
        lpFile: PCWSTR(exe_w.as_ptr()),
        lpParameters: PCWSTR(args_w.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut sei)
            .map_err(|error| format!("ShellExecuteEx 失败：{error}"))?;
    }
    // 失败时 hInstApp 承载 SE_ERR_* 代码（5 = 用户拒绝了 UAC）。
    let hinst = sei.hInstApp.0 as isize;
    if hinst <= 32 {
        let reason = match hinst {
            5 => "用户拒绝了 UAC 授权",
            2 => "文件未找到",
            3 => "路径未找到",
            _ => "ShellExecute 启动失败",
        };
        return Err(format!("拉起 hermix-filesearch 失败：{reason}（hInstApp={hinst}）"));
    }
    Ok(())
}

/// 开机自启是否已注册过（进程级一次；daemon 侧 schtasks /F 幂等）。
static AUTOSTART_INSTALLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// 单飞锁：「探活 → 拉起 → 90s 握手轮询」全程只允许一个并发。
/// 搜索窗每 2s 轮询一次 status——没有这把锁时，每个发现 helper 未就绪的
/// 请求都会删掉 endpoint.json 再拉一个提权实例，而新实例的交接逻辑
/// （stop-request）会请走正在装载快照的旧实例：连环互杀、黑框连环弹、
/// 索引永远建不完（2026-10-25 用户实测）。锁内复查探活——前一个持锁者
/// 可能刚把 helper 拉起来。
static ENSURE_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

/// 上次拉起失败的 Unix 秒。冷却期内直接复用失败结论——UAC 被拒后若不加
/// 冷却，2s 一发的 status 轮询等于「拒一次弹一次」。
static LAST_SPAWN_FAILURE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
const SPAWN_FAILURE_COOLDOWN_SECS: u64 = 60;

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 确保 helper 在跑；没跑就提权拉起并轮询 endpoint.json（覆盖 UAC 确认耗时）。
/// 握手成功后 fire-and-forget 发一次 `installTask`（在提权的 daemon 里注册
/// 开机自启计划任务）——索引开机即就绪，用户不必手动启动服务。
pub async fn ensure_helper() -> Result<(), String> {
    if let Ok(endpoint) = read_endpoint() {
        if endpoint_alive(&endpoint).await {
            install_autostart_once().await;
            return Ok(());
        }
    }
    let _guard = ENSURE_LOCK.lock().await;
    // 拿到锁后复查：前面排队的调用可能刚完成一次成功的拉起。
    if let Ok(endpoint) = read_endpoint() {
        if endpoint_alive(&endpoint).await {
            install_autostart_once().await;
            return Ok(());
        }
    }
    if unix_now().saturating_sub(LAST_SPAWN_FAILURE.load(std::sync::atomic::Ordering::Relaxed))
        < SPAWN_FAILURE_COOLDOWN_SECS
    {
        return Err(format!(
            "索引服务未启动：上次拉起失败（UAC 可能被拒），{SPAWN_FAILURE_COOLDOWN_SECS} 秒后自动重试"
        ));
    }

    let _ = std::fs::remove_file(endpoint_path());
    let exe = locate_exe()?;
    let args = format!("--serve --data-dir \"{}\"", data_dir().display());
    if let Err(error) = spawn_elevated(&exe, &args) {
        LAST_SPAWN_FAILURE.store(unix_now(), std::sync::atomic::Ordering::Relaxed);
        return Err(error);
    }

    for _ in 0..90 {
        tokio::time::sleep(Duration::from_millis(1000)).await;
        if let Ok(endpoint) = read_endpoint() {
            if endpoint_alive(&endpoint).await {
                LAST_SPAWN_FAILURE.store(0, std::sync::atomic::Ordering::Relaxed);
                install_autostart_once().await;
                return Ok(());
            }
        }
    }
    LAST_SPAWN_FAILURE.store(unix_now(), std::sync::atomic::Ordering::Relaxed);
    Err("等待 hermix-filesearch 启动超时（90s）——UAC 可能未确认，或 helper 崩溃。".to_string())
}

/// 请求 daemon 注册开机自启（daemon 是提权的，schtasks 才不会拒绝）。
/// 失败静默——自启只是体验优化，按需拉起仍然兜底。
async fn install_autostart_once() {
    use std::sync::atomic::Ordering;
    if AUTOSTART_INSTALLED.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Err(error) = request("installTask", json!({})).await {
        tracing::debug!(error = %error, "filesearch autostart install skipped");
        // 失败允许重试（下次 ensure 再试一次）。
        AUTOSTART_INSTALLED.store(false, Ordering::Release);
    }
}

/// 单次请求：连接 → 发 JSONL → 收一行响应（无长连接，请求级生命周期）。
async fn request(method: &str, params: Value) -> Result<Value, String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let endpoint = read_endpoint()?;
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", endpoint.port))
        .await
        .map_err(|error| format!("连接 helper 失败：{error}"))?;

    let payload = json!({"id": 1, "method": method, "params": params, "token": endpoint.token});
    stream
        .write_all(format!("{payload}\n").as_bytes())
        .await
        .map_err(|error| format!("发送请求失败：{error}"))?;

    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(15), async {
        BufReader::new(stream).read_line(&mut line).await
    })
    .await
    .map_err(|_| "helper 响应超时（15s）".to_string())?
    .map_err(|error| format!("读取响应失败：{error}"))?;

    let value: Value =
        serde_json::from_str(line.trim()).map_err(|error| format!("响应解析失败：{error}"))?;
    if let Some(error) = value.get("error").and_then(|v| v.as_str()) {
        return Err(error.to_string());
    }
    Ok(value.get("result").cloned().unwrap_or(Value::Null))
}

fn api_error(error: String) -> (StatusCode, String) {
    tracing::warn!(error = %error, "filesearch request failed");
    (StatusCode::BAD_GATEWAY, error)
}

// ── HTTP 端点 ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub query: String,
    pub limit: Option<u32>,
}

pub async fn search(
    Query(params): Query<SearchQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    ensure_helper()
        .await
        .map_err(|error| api_error(error))?;
    let result = request(
        "search",
        json!({"query": params.query, "limit": params.limit}),
    )
    .await
    .map_err(api_error)?;
    Ok(Json(result))
}

pub async fn status() -> Result<impl IntoResponse, (StatusCode, String)> {
    ensure_helper().await.map_err(api_error)?;
    let result = request("status", json!({})).await.map_err(api_error)?;
    Ok(Json(result))
}

/// 下发过滤规则（FilterRules，结构见 hermix-filesearch core/model.rs）。
/// helper 重启后规则回到默认值，桌面端负责在启动时回放。
pub async fn filter(
    Json(rules): Json<Value>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    ensure_helper().await.map_err(api_error)?;
    let result = request("setFilter", rules).await.map_err(api_error)?;
    Ok(Json(result))
}

pub async fn shutdown() -> Result<impl IntoResponse, (StatusCode, String)> {
    let result = request("shutdown", json!({})).await.map_err(api_error)?;
    let _ = std::fs::remove_file(endpoint_path());
    Ok(Json(result))
}
