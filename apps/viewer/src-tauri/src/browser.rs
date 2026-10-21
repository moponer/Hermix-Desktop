//! 系统浏览器通道（**通道 B**）：系统 Edge / Chrome + CDP（Chrome DevTools
//! Protocol）。让 **agent**（经 MCP 接入的任意外部 AI）能真的开网页、点按钮、
//! 读内容、截图；用户可在设置里切「有头 / 无头」。
//!
//! **隔离**：每个会话一个**独立 `--user-data-dir`** → 独立进程 + 独立
//! cookie / storage / 权限。多 agent 各用一个会话 id → 互不干扰（用户诉求：
//! 「多个界面多个 agent 同时操作不会乱」）。
//!
//! **三个已知的坑**（来自 hermix-tools 的实测，本模块照抄）：
//!   - **F1 端口写死会撞用户自己的浏览器** → 用 `--remote-debugging-port=0`，
//!     真实端口写进 profile 的 `DevToolsActivePort` 文件里。
//!   - **F2 绝不能用用户日常 Edge profile** → Edge 单实例会把 URL 交给已运行
//!     的实例然后自己退出，我们连不上。独立 profile 没有这个问题。
//!   - **F3 headless 默认 800×600** → 响应式布局会变形 → 显式声明窗口尺寸。
//!
//! **有头 / 无头**：`--headless=new` 就是无头（不弹窗口）；不加就是有头
//! （真实 Edge 窗口，用户看得见也能接管）。这与仓库 2026-10-14「浏览器永不
//! 现形」的裁决**范围不同**——那条裁的是桌面端 agent 浏览器面板，这里是
//! 查看器里用户显式选择的浏览模式。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::cdp::Cdp;

/// 同时最多几个浏览器会话（每个会话 = 一个 Edge 进程，内存线性增长）。
const MAX_SESSIONS: usize = 8;
/// 等浏览器调试端口就绪的上限。
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------
// 会话
// ---------------------------------------------------------------------------

/// 一个浏览器会话（= 一个独立 Edge 进程 + 一个独立 profile）。
struct Session {
    id: String,
    headed: bool,
    profile_dir: PathBuf,
    port: u16,
    ws_path: String,
    target_id: String,
    url: String,
    /// 浏览器主进程 pid（CDP 关闭失败时的兜底）。
    pid: u32,
    /// 创建序号（超上限时关掉最老的）。
    seq: u64,
}

/// 会话信息（对外可见的形状）。
#[derive(serde::Serialize, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub headed: bool,
    pub url: String,
    pub pid: u32,
}

fn store() -> &'static Mutex<HashMap<String, Session>> {
    static S: OnceLock<Mutex<HashMap<String, Session>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

fn seq_counter() -> &'static AtomicU64 {
    static S: OnceLock<AtomicU64> = OnceLock::new();
    S.get_or_init(|| AtomicU64::new(0))
}

/// 会话 id → 目录名（只留安全字符）。
fn sanitize(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "default".to_string()
    } else {
        trimmed.chars().take(48).collect()
    }
}

/// profile 根目录：会话之间、两次启动之间都分开。
fn profile_root() -> PathBuf {
    hermix_config::paths::hermix_home().join("browser-profiles")
}

// ---------------------------------------------------------------------------
// 浏览器定位与启动
// ---------------------------------------------------------------------------

/// 找系统浏览器：环境变量优先，其次 Edge，最后 Chrome。
fn browser_exe() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("HERMIX_BROWSER_PATH") {
        let p = PathBuf::from(custom);
        if p.is_file() {
            return Some(p);
        }
    }
    let local = std::env::var("LOCALAPPDATA").ok().map(PathBuf::from);
    let pf = std::env::var("ProgramFiles").ok().map(PathBuf::from);
    let pf86 = std::env::var("ProgramFiles(x86)").ok().map(PathBuf::from);
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(local) = local.as_ref() {
        candidates.push(local.join("Microsoft/Edge/Application/msedge.exe"));
        candidates.push(local.join("Google/Chrome/Application/chrome.exe"));
    }
    if let Some(pf) = pf.as_ref() {
        candidates.push(pf.join("Microsoft/Edge/Application/msedge.exe"));
        candidates.push(pf.join("Google/Chrome/Application/chrome.exe"));
    }
    if let Some(pf86) = pf86.as_ref() {
        candidates.push(pf86.join("Microsoft/Edge/Application/msedge.exe"));
        candidates.push(pf86.join("Google/Chrome/Application/chrome.exe"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// 启动浏览器，返回 pid。
///
/// **必须走 ShellExecuteExW**：viewer 自己 CreateProcess 会被 360 判成木马
/// （2026-10-20 教训，见 `commands.rs::shell_exec`）。
#[cfg(windows)]
fn launch_browser(exe: &PathBuf, args: &str) -> Result<u32, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError};
    use windows_sys::Win32::System::Threading::GetProcessId;
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let file_w: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
    let args_w: Vec<u16> = args.encode_utf16().chain(Some(0)).collect();

    // 三种写法依次试：不同安全软件 / 系统策略对「谁拉起浏览器」的限制
    // 不一样（动词为空 / 动词 open / 不要进程句柄）。哪一步成功用哪一步。
    //   A：默认动词 + 进程句柄（拿得到 pid，兜底能杀进程）
    //   B：open 动词 + 进程句柄
    //   C：open 动词，不要进程句柄（拿不到 pid，只能靠 CDP 关）
    let variants: [(&str, u32); 3] = [
        ("", SEE_MASK_NOCLOSEPROCESS),
        ("open", SEE_MASK_NOCLOSEPROCESS),
        ("open", 0),
    ];
    let mut last_error = 0u32;
    for (verb, mask) in variants {
        // 动词缓冲区必须活过 ShellExecuteExW 调用（临时 Vec 会当场析构）。
        let verb_w: Vec<u16> = if verb.is_empty() {
            Vec::new()
        } else {
            verb.encode_utf16().chain(Some(0)).collect()
        };
        let mut sei: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        sei.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        sei.fMask = mask;
        sei.lpVerb = if verb_w.is_empty() {
            std::ptr::null()
        } else {
            verb_w.as_ptr()
        };
        sei.lpFile = file_w.as_ptr();
        sei.lpParameters = args_w.as_ptr();
        sei.lpDirectory = std::ptr::null();
        sei.nShow = SW_SHOWNORMAL;

        let ok = unsafe { ShellExecuteExW(&mut sei) };
        if ok == 0 {
            // ShellExecuteEx 失败时真正的原因在 GetLastError 里（返回值只是
            // BOOL）。记下来，别让排查只能靠猜。
            last_error = unsafe { GetLastError() };
            continue;
        }
        let pid = if mask != 0 && sei.hProcess != 0 {
            let pid = unsafe { GetProcessId(sei.hProcess) };
            unsafe {
                CloseHandle(sei.hProcess);
            }
            pid
        } else {
            0
        };
        return Ok(pid);
    }
    Err(format!(
        "启动浏览器失败：{}（ShellExecuteEx 错误码 {last_error}）",
        exe.display()
    ))
}

#[cfg(not(windows))]
fn launch_browser(_exe: &PathBuf, _args: &str) -> Result<u32, String> {
    Err("系统浏览器通道只在 Windows 上可用".to_string())
}

/// 等 profile 里的 `DevToolsActivePort` 出现 → (端口, ws 路径)。
/// 端口写死会撞用户自己的浏览器（hermix-tools 的 F1 教训），所以用
/// `--remote-debugging-port=0`，真实端口由浏览器进程写进这个文件。
fn wait_for_port(profile: &PathBuf) -> Result<(u16, String), String> {
    let file = profile.join("DevToolsActivePort");
    let deadline = Instant::now() + LAUNCH_TIMEOUT;
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(&file) {
            let mut lines = text.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                if let Ok(port) = port.trim().parse::<u16>() {
                    if !path.trim().is_empty() {
                        return Ok((port, path.trim().to_string()));
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(120));
    }
    Err("等待浏览器调试端口超时（浏览器可能启动失败或被安全软件拦截）".to_string())
}

// ---------------------------------------------------------------------------
// CDP 客户端在 `cdp.rs`（浏览器通道与内嵌网页视图共用同一份实现）
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// 对外的会话操作
// ---------------------------------------------------------------------------

/// 打开（或复用）一个会话并导航到指定网址。
pub fn open(session: &str, url: &str, headed: bool) -> Result<SessionInfo, String> {
    let target = crate::webview_host::normalize_url(url)?;
    let id = session.trim().to_string();
    let id = if id.is_empty() {
        "default".to_string()
    } else {
        id
    };

    // 已有会话 → 直接复用（有头 / 无头不一致则重建）。
    // 注意：判断必须在**不持锁**的时候做——close() 自己要拿同一把锁。
    let reusable = store()
        .lock()
        .unwrap()
        .get(&id)
        .map(|s| s.headed == headed)
        .unwrap_or(false);
    if reusable {
        navigate(&id, target.as_str())?;
        return info(&id);
    }
    if store().lock().unwrap().contains_key(&id) {
        let _ = close(&id);
    }

    let exe = browser_exe().ok_or_else(|| {
        "找不到系统浏览器（Edge / Chrome）。可用环境变量 HERMIX_BROWSER_PATH 指定可执行文件。"
            .to_string()
    })?;

    let profile = profile_root().join(sanitize(&id));
    // 每次开新会话 = **全新 profile**：上次的 cookie / 缓存不留（隐私第一），
    // 也避免读到旧 profile 里那个已经失效的 DevToolsActivePort（端口是死
    // 的 → 连接被拒）。
    let _ = std::fs::remove_dir_all(&profile);
    std::fs::create_dir_all(&profile).map_err(|e| format!("创建浏览器 profile 失败：{e}"))?;

    let args = format!(
        "--remote-debugging-port=0 --user-data-dir=\"{}\" --no-first-run \
         --no-default-browser-check --disable-background-networking --window-size=1440,900 \
         {} about:blank",
        profile.display(),
        if headed { "" } else { "--headless=new" }
    );
    let pid = launch_browser(&exe, &args)?;
    let (port, ws_path) = wait_for_port(&profile)?;

    // 建一个页面 target（后续所有操作都挂在它上面）。
    // 注意：**不能**带 width/height —— CDP 报 "Target position can only be
    // set for new windows"。窗口尺寸由启动参数 `--window-size` 决定。
    let mut cdp = Cdp::connect(port, &ws_path)?;
    let created = cdp.call("Target.createTarget", json!({"url": "about:blank"}), None)?;
    let target_id = created
        .get("targetId")
        .and_then(|t| t.as_str())
        .ok_or_else(|| "浏览器没有返回 targetId".to_string())?
        .to_string();

    let seq = seq_counter().fetch_add(1, Ordering::Relaxed);
    let mut map = store().lock().unwrap();
    enforce_limit(&mut map);
    map.insert(
        id.clone(),
        Session {
            id: id.clone(),
            headed,
            profile_dir: profile,
            port,
            ws_path,
            target_id,
            url: target.as_str().to_string(),
            pid,
            seq,
        },
    );
    drop(map);

    navigate(&id, target.as_str())?;
    info(&id)
}

/// 超上限时关掉最老的会话。
fn enforce_limit(map: &mut HashMap<String, Session>) {
    while map.len() >= MAX_SESSIONS {
        let Some(oldest) = map.values().min_by_key(|s| s.seq).map(|s| s.id.clone()) else {
            break;
        };
        let session = map.remove(&oldest);
        if let Some(session) = session {
            let _ = shutdown(&session);
        }
    }
}

/// 关闭浏览器进程（CDP 优先，失败再按 pid 杀）。
fn shutdown(session: &Session) {
    if let Ok(mut cdp) = Cdp::connect(session.port, &session.ws_path) {
        let _ = cdp.call("Browser.close", json!({}), None);
    }
    // CDP 的 Browser.close 是异步的，等一小会儿再看进程还在不在。
    std::thread::sleep(Duration::from_millis(400));
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, TerminateProcess, PROCESS_TERMINATE,
        };
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, session.pid);
            if handle != 0 {
                let _ = TerminateProcess(handle, 1);
                let _ = CloseHandle(handle);
            }
        }
    }
}

/// 会话信息。
pub fn info(session: &str) -> Result<SessionInfo, String> {
    let map = store().lock().unwrap();
    let s = map
        .get(session)
        .ok_or_else(|| format!("没有这个浏览器会话：{session}"))?;
    Ok(SessionInfo {
        id: s.id.clone(),
        headed: s.headed,
        url: s.url.clone(),
        pid: s.pid,
    })
}

/// 列出全部会话。
pub fn list() -> Vec<SessionInfo> {
    let map = store().lock().unwrap();
    let mut items: Vec<SessionInfo> = map
        .values()
        .map(|s| SessionInfo {
            id: s.id.clone(),
            headed: s.headed,
            url: s.url.clone(),
            pid: s.pid,
        })
        .collect();
    items.sort_by(|a, b| a.id.cmp(&b.id));
    items
}

/// 导航到新网址。
pub fn navigate(session: &str, url: &str) -> Result<String, String> {
    let target = crate::webview_host::normalize_url(url)?;
    let (port, ws_path, target_id) = {
        let map = store().lock().unwrap();
        let s = map
            .get(session)
            .ok_or_else(|| format!("没有这个浏览器会话：{session}"))?;
        (s.port, s.ws_path.clone(), s.target_id.clone())
    };
    let mut cdp = Cdp::connect(port, &ws_path)?;
    let sid = cdp.attach(&target_id)?;
    cdp.call("Page.navigate", json!({ "url": target.as_str() }), Some(&sid))?;
    wait_ready(&mut cdp, &sid)?;
    let final_url = page_url(&mut cdp, &sid).unwrap_or_else(|| target.as_str().to_string());
    {
        let mut map = store().lock().unwrap();
        if let Some(s) = map.get_mut(session) {
            s.url = final_url.clone();
        }
    }
    Ok(final_url)
}

/// 等页面加载完（readyState = complete）。
fn wait_ready(cdp: &mut Cdp, sid: &str) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let result = cdp.call(
            "Runtime.evaluate",
            json!({"expression": "document.readyState", "returnByValue": true}),
            Some(sid),
        )?;
        let state = result
            .get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if state == "complete" {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    Ok(())
}

/// 当前页面 URL。
fn page_url(cdp: &mut Cdp, sid: &str) -> Option<String> {
    let result = cdp
        .call(
            "Runtime.evaluate",
            json!({"expression": "location.href", "returnByValue": true}),
            Some(sid),
        )
        .ok()?;
    result
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// 页面可见文本（比截图省 token，agent 读页面首选）。
pub fn page_text(session: &str) -> Result<String, String> {
    let (port, ws_path, target_id) = endpoints(session)?;
    let mut cdp = Cdp::connect(port, &ws_path)?;
    let sid = cdp.attach(&target_id)?;
    let result = cdp.call(
        "Runtime.evaluate",
        json!({
            "expression": "(document.body && document.body.innerText) || ''",
            "returnByValue": true
        }),
        Some(&sid),
    )?;
    let text = result
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok(crate::mcp_server::truncate_chars(&text, 6000))
}

/// 执行一段 JS，返回结果（按值返回）。
pub fn eval(session: &str, expression: &str) -> Result<String, String> {
    let (port, ws_path, target_id) = endpoints(session)?;
    let mut cdp = Cdp::connect(port, &ws_path)?;
    let sid = cdp.attach(&target_id)?;
    crate::cdp::evaluate(&mut cdp, &sid, expression)
}

/// 点击页面坐标（CSS 像素，相对视口左上角）。
pub fn click(session: &str, x: f64, y: f64) -> Result<String, String> {
    let (port, ws_path, target_id) = endpoints(session)?;
    let mut cdp = Cdp::connect(port, &ws_path)?;
    let sid = cdp.attach(&target_id)?;
    for pressed in [true, false] {
        cdp.call(
            "Input.dispatchMouseEvent",
            json!({
                "type": if pressed { "mousePressed" } else { "mouseReleased" },
                "x": x,
                "y": y,
                "button": "left",
                "clickCount": 1
            }),
            Some(&sid),
        )?;
    }
    std::thread::sleep(Duration::from_millis(250));
    Ok(format!("已点击 ({x}, {y})"))
}

/// 截图：返回 (落盘路径, 缩略图 base64, mime)。
pub fn screenshot(session: &str) -> Result<(String, String, String), String> {
    let (port, ws_path, target_id) = endpoints(session)?;
    let mut cdp = Cdp::connect(port, &ws_path)?;
    let sid = cdp.attach(&target_id)?;
    let result = cdp.call(
        "Page.captureScreenshot",
        json!({"format": "png", "captureBeyondViewport": false}),
        Some(&sid),
    )?;
    let data = result
        .get("data")
        .and_then(|d| d.as_str())
        .ok_or_else(|| "浏览器没有返回截图数据".to_string())?;
    let png = crate::commands::base64_decode(data)
        .ok_or_else(|| "截图数据不是合法 base64".to_string())?;

    // 落盘（用户能直接打开看）+ 给模型看的缩略图。
    crate::cdp::save_and_thumbnail(&png, &sanitize(session))
}

/// 关闭会话（并删掉它的 profile：cookie / 缓存不留痕）。
pub fn close(session: &str) -> Result<(), String> {
    let removed = {
        let mut map = store().lock().unwrap();
        map.remove(session)
    };
    let Some(s) = removed else {
        return Err(format!("没有这个浏览器会话：{session}"));
    };
    shutdown(&s);
    let _ = std::fs::remove_dir_all(&s.profile_dir);
    Ok(())
}

/// 取会话的连接信息。
fn endpoints(session: &str) -> Result<(u16, String, String), String> {
    let map = store().lock().unwrap();
    let s = map
        .get(session)
        .ok_or_else(|| format!("没有这个浏览器会话：{session}"))?;
    Ok((s.port, s.ws_path.clone(), s.target_id.clone()))
}

// ---------------------------------------------------------------------------
// Tauri 命令（前端「浏览器」面板用）
// ---------------------------------------------------------------------------

/// 打开（或复用）一个浏览器会话。
#[tauri::command]
pub fn browser_open(session: String, url: String, headed: bool) -> Result<SessionInfo, String> {
    open(&session, &url, headed)
}

/// 列出全部浏览器会话。
#[tauri::command]
pub fn browser_sessions() -> Vec<SessionInfo> {
    list()
}

/// 关闭一个浏览器会话（并清掉它的 profile）。
#[tauri::command]
pub fn browser_close(session: String) -> Result<(), String> {
    close(&session)
}

#[cfg(test)]
mod tests {
    use super::{sanitize, MAX_SESSIONS};

    #[test]
    fn session_id_is_sanitized_into_a_safe_dir_name() {
        assert_eq!(sanitize("agent-a"), "agent-a");
        assert_eq!(sanitize("agent a/b"), "agent_a_b");
        assert_eq!(sanitize("../evil"), "evil");
        assert_eq!(sanitize(""), "default");
        assert_eq!(sanitize("///"), "default");
    }

    #[test]
    fn session_cap_is_bounded() {
        assert!(MAX_SESSIONS <= 16, "会话上限不宜过大：每个会话一个浏览器进程");
    }
}
