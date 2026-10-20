//! 本地文件索引（类 Everything）的客户端。
//!
//! 索引服务是独立提权进程 `hermix-filesearch.exe`（MFT 建索引需要管理员），
//! 跨提权边界继承不了 stdio 管道 → 走 127.0.0.1 回环 TCP + token 握手，
//! 端口与 token 由它写进 `%LOCALAPPDATA%\hermix\filesearch\endpoint.json`。
//! 协议与 `crates/hermix-server/src/filesearch.rs` 完全一致（同一个 helper、
//! 同一份 JSONL 帧），这里把「HTTP 端点」换成 Tauri 命令，供路径栏的
//! 模糊搜索直接调用。
//!
//! **为什么 viewer 不直接读索引快照**（`indexes\<VOL>.bin`）：快照 150–250MB，
//! 读一次尖峰就把查看器的内存吃满，且索引有 30s 滞后；走 helper 才有实时
//! 的排序（精确名 > 前缀 > 其他子串，目录优先）与过滤。
//!
//! **为什么不让 viewer 自己建索引**：目录遍历回退（`core/scanner.rs`）在
//! 上游是没有调用点的死代码，MFT 路径又必须管理员 —— 让现成的提权 helper
//! 干这件事，viewer 只当客户端。

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};

use crate::commands::DirEntry;

/// helper 数据目录（与 `hermix-filesearch` 的 default_data_dir 一致）。
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

/// 定位 helper exe：env 覆盖 → dev 的 target 目录 → 当前 exe 同目录。
///
/// 三级候选与 `hermix-server/src/filesearch.rs` 同款：dev 下 helper 在工作区
/// target 里，release 下它随桌面端一起发在 exe 同目录（见 ensure-sidecar）。
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
                "找不到 hermix-filesearch.exe（先构建：cargo build -p hermix-filesearch --bin hermix-filesearch）"
                    .to_string()
            });
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default();
    Ok(exe_dir.join("hermix-filesearch.exe"))
}

#[derive(Debug, Clone, serde::Deserialize)]
struct Endpoint {
    port: u16,
    token: String,
    pid: u32,
}

fn read_endpoint() -> Result<Endpoint, String> {
    let raw = std::fs::read_to_string(endpoint_path())
        .map_err(|_| "endpoint.json 不存在（索引服务未启动）".to_string())?;
    serde_json::from_str::<Endpoint>(&raw).map_err(|error| format!("endpoint.json 解析失败：{error}"))
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // windows-sys 的 OpenProcess 返回 HANDLE（0 = 失败），不是 Result。
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle == 0 {
        return false;
    }
    unsafe {
        let _ = CloseHandle(handle);
    }
    true
}

#[cfg(not(windows))]
fn process_alive(_pid: u32) -> bool {
    false
}

/// 提权拉起 helper（UAC 弹一次；已是管理员则静默）。
///
/// 与 `hermix-server/src/filesearch.rs` 同款：`ShellExecuteExW("runas")`。
/// 失败时 `hInstApp` 承载 SE_ERR_* 代码（5 = 用户拒绝了 UAC）——这是最常见
/// 的失败原因，必须如实报给用户，不能笼统说「启动失败」。
#[cfg(windows)]
fn spawn_elevated(exe: &Path, args: &str) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SHELLEXECUTEINFOW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let exe_w: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
    let args_w: Vec<u16> = std::ffi::OsStr::new(args)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let verb_w: Vec<u16> = "runas\0".encode_utf16().collect();

    let mut sei: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    sei.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    // 刻意**不**要 SEE_MASK_NOCLOSEPROCESS：我们要的是「把 helper 拉起来」，
    // 不是「拿到它的进程句柄去操控它」——后者在行为监控眼里是可疑信号。
    sei.fMask = SEE_MASK_FLAG_NO_UI;
    sei.lpVerb = verb_w.as_ptr();
    sei.lpFile = exe_w.as_ptr();
    sei.lpParameters = args_w.as_ptr();
    sei.nShow = SW_SHOWNORMAL;
    unsafe {
        // windows-sys 的 ShellExecuteExW 返回 BOOL（0 = 失败），不是 Result。
        if ShellExecuteExW(&mut sei) == 0 {
            return Err("ShellExecuteEx 失败（系统拒绝启动）".into());
        }
    }
    // windows-sys 的 HINSTANCE 是 isize（windows crate 里才是结构体）。
    let hinst = sei.hInstApp;
    if hinst <= 32 {
        let reason = match hinst {
            5 => "用户拒绝了管理员授权",
            2 => "文件未找到",
            3 => "路径未找到",
            _ => "ShellExecute 启动失败",
        };
        return Err(format!("拉起 hermix-filesearch 失败：{reason}（hInstApp={hinst}）"));
    }
    Ok(())
}

#[cfg(not(windows))]
fn spawn_elevated(_exe: &Path, _args: &str) -> Result<(), String> {
    Err("仅支持 Windows（项目边界）".into())
}

/// 确保 helper 在跑；没跑就提权拉起并轮询 endpoint.json（覆盖 UAC 确认耗时）。
fn ensure_helper() -> Result<Endpoint, String> {
    if let Ok(endpoint) = read_endpoint() {
        if process_alive(endpoint.pid) {
            return Ok(endpoint);
        }
    }
    let _ = std::fs::remove_file(endpoint_path());

    let exe = locate_exe()?;
    let args = format!("--serve --data-dir \"{}\"", data_dir().display());
    spawn_elevated(&exe, &args)?;

    // 90s：UAC 确认 + helper 启动 + 首卷快照装载。
    for _ in 0..90 {
        std::thread::sleep(Duration::from_secs(1));
        if let Ok(endpoint) = read_endpoint() {
            if process_alive(endpoint.pid) {
                return Ok(endpoint);
            }
        }
    }
    Err("等待索引服务启动超时（90s）——管理员授权可能未确认，或 helper 崩溃。".into())
}

/// 单次请求：连接 → 发一行 JSON → 收一行响应（请求级生命周期，无长连接）。
fn request(endpoint: &Endpoint, method: &str, params: Value) -> Result<Value, String> {
    let stream = TcpStream::connect(("127.0.0.1", endpoint.port))
        .map_err(|error| format!("连接索引服务失败：{error}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .map_err(|error| format!("设置读取超时失败：{error}"))?;
    let mut writer = stream
        .try_clone()
        .map_err(|error| format!("复制连接失败：{error}"))?;
    let payload = json!({
        "id": 1,
        "method": method,
        "params": params,
        "token": endpoint.token,
    });
    writer
        .write_all(format!("{payload}\n").as_bytes())
        .map_err(|error| format!("发送请求失败：{error}"))?;
    writer.flush().ok();

    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|error| format!("读取响应失败：{error}"))?;
    let value: Value =
        serde_json::from_str(line.trim()).map_err(|error| format!("响应解析失败：{error}"))?;
    if let Some(error) = value.get("error").and_then(|v| v.as_str()) {
        return Err(error.to_string());
    }
    Ok(value.get("result").cloned().unwrap_or(Value::Null))
}

/// 全局搜索的结果 + 索引元信息（给前端展示「搜的是全盘」）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct GlobalSearchResult {
    pub entries: Vec<DirEntry>,
    pub total_matches: usize,
    pub returned: usize,
    pub elapsed_ms: u64,
}

/// helper 的 FileEntry → viewer 的 DirEntry。
///
/// `size` 在 MFT 路径下是 None（helper 不读 $DATA 区），显示 0；`mtime` 是
/// unix 毫秒，转成前端直接吃的 RFC 3339 字符串。
fn to_dir_entry(raw: &Value) -> Option<DirEntry> {
    let name = raw.get("name")?.as_str()?.to_string();
    let path = raw.get("path")?.as_str()?.to_string();
    let is_dir = raw.get("isDir").and_then(|v| v.as_bool()).unwrap_or(false);
    let size = raw.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
    let modified = raw.get("modified").and_then(|v| v.as_u64()).unwrap_or(0);
    let attributes = raw.get("attributes").and_then(|v| v.as_u64()).unwrap_or(0);
    let extension = raw
        .get("extension")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    Some(DirEntry {
        name,
        path,
        is_dir,
        size,
        mtime: crate::commands::ms_to_rfc3339(modified),
        extension,
        // FILE_ATTRIBUTE_HIDDEN = 0x2
        hidden: attributes & 0x2 != 0,
    })
}

/// 全盘模糊搜索（类 Everything）：文件名子串匹配，跨所有已索引卷。
///
/// 走 helper 而不是直接扫盘：扫盘是 O(全盘)，一次几十秒到几分钟，而且
/// 每次搜都扫一遍。helper 持有 MFT 索引，毫秒级返回。
pub fn global_search(query: &str, limit: usize) -> Result<GlobalSearchResult, String> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(GlobalSearchResult {
            entries: Vec::new(),
            total_matches: 0,
            returned: 0,
            elapsed_ms: 0,
        });
    }
    let endpoint = ensure_helper()?;
    let result = request(
        &endpoint,
        "search",
        json!({"query": q, "limit": limit}),
    )?;
    let raw_results = result
        .get("results")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "索引服务返回格式异常（缺 results）".to_string())?;
    let entries: Vec<DirEntry> = raw_results.iter().filter_map(to_dir_entry).collect();
    Ok(GlobalSearchResult {
        returned: result
            .get("returned")
            .and_then(|v| v.as_u64())
            .unwrap_or(entries.len() as u64) as usize,
        total_matches: result
            .get("totalMatches")
            .and_then(|v| v.as_u64())
            .unwrap_or(entries.len() as u64) as usize,
        elapsed_ms: result
            .get("elapsedMs")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        entries,
    })
}

/// Tauri 命令：全盘模糊搜索。
///
/// 整个流程（可能包含提权拉起 helper + 最多 90s 等待）都在阻塞线程上跑——
/// 绝不能占 Tauri 主线程，否则窗口白屏。
#[tauri::command(async)]
pub async fn viewer_search_global(
    query: String,
    limit: Option<usize>,
) -> Result<GlobalSearchResult, String> {
    let limit = limit.unwrap_or(300).clamp(1, 5_000);
    tauri::async_runtime::spawn_blocking(move || global_search(&query, limit))
        .await
        .map_err(|error| format!("搜索任务失败：{error}"))?
}

/// MFT 目录体积节点（helper 的 `diskUsage` 返回）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DiskUsageNode {
    pub file_ref: u64,
    pub parent_ref: u64,
    pub name: String,
    /// 子树总字节。
    pub bytes: u64,
    pub files: u64,
    pub depth: usize,
}

/// MFT 目录体积结果。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DiskUsageResult {
    pub volume: String,
    pub nodes: Vec<DiskUsageNode>,
    /// 全卷文件体积之和。
    pub total_bytes: u64,
    pub elapsed_ms: u64,
}

/// MFT 目录体积：秒级出结果（walkdir 要几十秒到几分钟）。
///
/// 走 helper：MFT 读取需要管理员权限，而 helper 已经是提权进程；索引里的
/// 体积来自 file record 的 `$DATA`。
pub fn disk_usage(
    volume: &str,
    parent_ref: Option<u64>,
    limit: usize,
) -> Result<DiskUsageResult, String> {
    let endpoint = ensure_helper()?;
    let result = request(
        &endpoint,
        "diskUsage",
        json!({"volume": volume, "parentRef": parent_ref, "limit": limit}),
    )?;
    let nodes = result
        .get("nodes")
        .and_then(|value| value.as_array())
        .map(|array| {
            array
                .iter()
                .filter_map(|raw| {
                    Some(DiskUsageNode {
                        file_ref: raw.get("fileRef")?.as_u64()?,
                        parent_ref: raw.get("parentRef")?.as_u64()?,
                        name: raw.get("name")?.as_str()?.to_string(),
                        bytes: raw.get("bytes")?.as_u64()?,
                        files: raw.get("files")?.as_u64()?,
                        depth: raw.get("depth")?.as_u64()? as usize,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(DiskUsageResult {
        volume: result
            .get("volume")
            .and_then(|value| value.as_str())
            .unwrap_or(volume)
            .to_string(),
        nodes,
        total_bytes: result
            .get("totalBytes")
            .and_then(|value| value.as_u64())
            .unwrap_or(0),
        elapsed_ms: result
            .get("elapsedMs")
            .and_then(|value| value.as_u64())
            .unwrap_or(0),
    })
}

/// Tauri 命令：MFT 目录体积（清理页的 Treemap 数据源）。
#[tauri::command(async)]
pub async fn viewer_disk_usage(
    volume: String,
    parent_ref: Option<u64>,
    limit: Option<usize>,
) -> Result<DiskUsageResult, String> {
    let limit = limit.unwrap_or(300).clamp(1, 2000);
    tauri::async_runtime::spawn_blocking(move || disk_usage(&volume, parent_ref, limit))
        .await
        .map_err(|error| format!("磁盘体积分析失败：{error}"))?
}

/// 索引服务状态（给「搜索」面板显示是否已就绪）。
#[tauri::command(async)]
pub async fn viewer_filesearch_status() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let Ok(endpoint) = read_endpoint() else {
            return Ok(json!({"running": false, "reason": "索引服务未启动"}));
        };
        if !process_alive(endpoint.pid) {
            return Ok(json!({"running": false, "reason": "索引服务进程已退出"}));
        }
        let result = request(&endpoint, "status", json!({})).unwrap_or(Value::Null);
        Ok(json!({"running": true, "status": result}))
    })
    .await
    .map_err(|error| format!("状态查询失败：{error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_helper_entry_to_viewer_entry() {
        let raw = json!({
            "name": "report.pdf",
            "path": "C:\\docs\\report.pdf",
            "isDir": false,
            "size": 1024,
            "modified": 1_700_000_000_000u64,
            "attributes": 0,
            "extension": "pdf",
        });
        let entry = to_dir_entry(&raw).expect("entry");
        assert_eq!(entry.name, "report.pdf");
        assert_eq!(entry.extension, "pdf");
        assert!(!entry.is_dir);
        assert!(!entry.hidden);
        assert_eq!(entry.size, 1024);
        assert!(entry.mtime.is_some());
    }

    #[test]
    fn hidden_attribute_maps_to_hidden_flag() {
        let raw = json!({
            "name": "secret.txt",
            "path": "C:\\secret.txt",
            "isDir": false,
            "size": null,
            "modified": 0,
            "attributes": 2,
            "extension": "txt",
        });
        let entry = to_dir_entry(&raw).expect("entry");
        assert!(entry.hidden);
        assert_eq!(entry.size, 0);
    }

    #[test]
    fn empty_query_returns_empty_result_without_touching_helper() {
        let result = global_search("   ", 100).expect("empty query must not fail");
        assert!(result.entries.is_empty());
        assert_eq!(result.total_matches, 0);
    }
}
