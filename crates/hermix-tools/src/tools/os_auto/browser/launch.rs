//! Find the system Edge, start it, and learn the debugging port it actually
//! bound.
//!
//! Pitfall F1 — **hardcoding 9222**. The deleted `cdp_bootstrap` opened a fixed
//! port, so a second hermix (or a user's own `--remote-debugging-port=9222`)
//! silently took over the session and hermix ended up driving *someone else's*
//! browser. `--remote-debugging-port=0` makes the kernel pick a free port and
//! Edge writes it to `DevToolsActivePort` inside the profile it was given, so
//! the port is discoverable per-process and cannot collide.
//!
//! Pitfall F2 — **never attach to the daily Edge profile**. Edge is
//! single-instance per `--user-data-dir`: launching a second process against
//! the user's real profile makes it hand the URL to the already-running daily
//! Edge and exit, which looks exactly like a browser that refuses to start.
//! Every session therefore gets a throwaway profile under hermix' home.
//!
//! Pitfall F3 — **declare the viewport**. Headless defaults to an 800x600
//! window, so responsive layouts lay out differently from what the operator
//! sees; `--window-size` is what makes a snapshot reproducible.
//!
//! **Always headless, permanently.** The browser is never meant to appear as a
//! real window: a headed Edge steals focus, lands in Alt+Tab, and can be closed
//! by a mis-click. The operator watches the session through the screencast fed
//! `browser-view` sub-window instead. There is deliberately no headed path here
//! — the schema is the model's behavioural boundary, so a headed mode must not
//! be re-added without a product decision (see the proposal's escape hatch).

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use windows::Win32::System::Threading::CREATE_BREAKAWAY_FROM_JOB;

use super::job::{spawn_is_access_denied, Job};

/// Edge writes `DevToolsActivePort` asynchronously; 100 ms keeps startup
/// latency under a round-trip without busy-spinning a core.
const PORT_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Generous because a cold first start on a spinning disk pays for AV
/// scanning; past this we report rather than hang the tool call.
const PORT_POLL_TIMEOUT: Duration = Duration::from_secs(20);

/// Probed once. A miss is cached too — re-stat'ing three paths on every tool
/// call is waste, and Edge appearing mid-session is not a real scenario.
static EDGE_EXE: OnceLock<Option<PathBuf>> = OnceLock::new();
/// Distinguishes two profiles launched inside the same clock second.
static PROFILE_SEQ: AtomicU64 = AtomicU64::new(0);

/// A running browser plus everything needed to make sure it does not outlive us.
pub struct LaunchedBrowser {
    pub child: Child,
    /// Dropping this closes the job handle, which kills the whole Edge tree.
    pub job: Job,
    /// Fully-formed `ws://127.0.0.1:<port><path>` for `Browser::connect`.
    pub ws_url: String,
    pub user_data_dir: PathBuf,
}

/// Locate the system Edge, or the binary `HERMIX_BROWSER_PATH` points at.
pub fn resolve_edge_exe() -> Option<PathBuf> {
    EDGE_EXE.get_or_init(probe_edge_exe).clone()
}

fn probe_edge_exe() -> Option<PathBuf> {
    // The env override exists so a non-Edge Chromium build (and CI) can be
    // driven without editing code.
    if let Some(custom) = std::env::var_os("HERMIX_BROWSER_PATH") {
        let path = PathBuf::from(custom);
        if path.is_file() {
            return Some(path);
        }
        tracing::warn!(path = %path.display(), "HERMIX_BROWSER_PATH is not a file; falling back to probing Edge");
    }

    // Per-user install first: that is where Edge lands by default, and probing
    // Program Files first would pick a system-wide build that predates the
    // user's own and therefore has no `--headless=new`.
    [
        ("LOCALAPPDATA", r"Microsoft\Edge\Application\msedge.exe"),
        ("ProgramFiles(x86)", r"Microsoft\Edge\Application\msedge.exe"),
        ("ProgramFiles", r"Microsoft\Edge\Application\msedge.exe"),
    ]
    .into_iter()
    .filter_map(|(var, rel)| {
        std::env::var_os(var).map(|root| PathBuf::from(root).join(rel))
    })
    .find(|candidate| candidate.is_file())
}

/// Parse the two-line `DevToolsActivePort` file.
///
/// Pure so it can be unit-tested: the file is written non-atomically, so a
/// poll can catch it with only the first line flushed. Returning `None` there
/// is the correct answer — the caller keeps polling.
pub fn parse_dev_tools_active_port(raw: &str) -> Option<(u16, String)> {
    let mut lines = raw.lines();
    let port: u16 = lines.next()?.trim().parse().ok()?;
    let path = lines.next().unwrap_or_default().trim();
    // Port 0 never happens for a real bound port; treating it as success would
    // hand `Browser::connect` a useless URL.
    if port == 0 || path.is_empty() {
        return None;
    }
    Some((port, path.to_string()))
}

/// Start Edge in a throwaway profile and wait for its CDP endpoint.
///
/// Always headless — see the module docs for why there is no headed variant.
///
/// Every failure path drops the [`Job`], which is what guarantees there is no
/// orphan: we never leave an Edge running that hermix can no longer reach.
pub async fn launch() -> Result<LaunchedBrowser, String> {
    let exe = resolve_edge_exe().ok_or_else(|| {
        "未找到 Microsoft Edge 可执行文件；请安装 Edge 或设置 HERMIX_BROWSER_PATH 指向 Chromium 内核浏览器".to_string()
    })?;

    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let seq = PROFILE_SEQ.fetch_add(1, Ordering::Relaxed);
    let user_data_dir = hermix_config::paths::hermix_home()
        .join("browser")
        .join(format!("profile-{stamp}-{seq}"));
    std::fs::create_dir_all(&user_data_dir)
        .map_err(|e| format!("创建浏览器 profile 目录失败 ({}): {e}", user_data_dir.display()))?;

    let mut cmd = Command::new(&exe);
    cmd.arg("--remote-debugging-port=0") // F1: never a fixed port
        .arg(format!("--user-data-dir={}", user_data_dir.display())) // F2: never the daily profile
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-background-networking")
        .arg("--disable-sync")
        .arg("--disable-features=Translate,MediaRouter")
        .arg("--window-size=1280,800") // F3
        .arg("--headless=new");
    // A piped stderr that nobody reads fills the pipe buffer and deadlocks the
    // browser the moment it logs a warning; Edge chatters constantly.
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let job = Job::new().map_err(|e| format!("创建 Job Object 失败: {e}"))?;

    let mut child = match cmd
        .creation_flags(CREATE_BREAKAWAY_FROM_JOB.0)
        .spawn()
    {
        Ok(child) => child,
        // P1: hermix is itself inside a job that forbids breakaway. Retrying
        // without the flag nests us instead — acceptable, because assignment
        // to *our* job below is what actually enforces no-orphan.
        Err(e) if spawn_is_access_denied(&e) => {
            tracing::debug!(
                error = %e,
                "CREATE_BREAKAWAY_FROM_JOB denied by an outer job; retrying nested"
            );
            cmd.creation_flags(0)
                .spawn()
                .map_err(|e| format!("启动 Edge 失败 ({}): {e}", exe.display()))?
        }
        Err(e) => return Err(format!("启动 Edge 失败 ({}): {e}", exe.display())),
    };

    // Every `?` from here on drops `job`, whose `KILL_ON_JOB_CLOSE` limit tears
    // the whole Edge tree down — that is the no-orphan guarantee, not a
    // best-effort cleanup somebody can forget.
    if let Err(e) = job.assign_child(&child) {
        return Err(format!("把 Edge 加入 Job Object 失败: {e}"));
    }
    let ws_url = wait_for_devtools_port(&mut child, &user_data_dir).await?;
    Ok(LaunchedBrowser {
        child,
        job,
        ws_url,
        user_data_dir,
    })
}

/// Poll `DevToolsActivePort` until it names a bound port and websocket path.
async fn wait_for_devtools_port(
    child: &mut Child,
    user_data_dir: &PathBuf,
) -> Result<String, String> {
    let port_file = user_data_dir.join("DevToolsActivePort");
    let deadline = tokio::time::Instant::now() + PORT_POLL_TIMEOUT;

    loop {
        if let Ok(raw) = std::fs::read_to_string(&port_file) {
            if let Some((port, path)) = parse_dev_tools_active_port(&raw) {
                return Ok(format!("ws://127.0.0.1:{port}{path}"));
            }
        }
        // A dead browser would otherwise cost the full 20 s before we notice.
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("Edge 启动后立即退出 (code={status})，请检查 --user-data-dir 是否可写"));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "等待 Edge 调试端口超时 ({}s)，未生成 {}",
                PORT_POLL_TIMEOUT.as_secs(),
                port_file.display()
            ));
        }
        tokio::time::sleep(PORT_POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_well_formed_port_file() {
        let raw = "41234\r\n/devtools/browser/9f8a-4c2b\r\n";
        assert_eq!(
            parse_dev_tools_active_port(raw),
            Some((41234, "/devtools/browser/9f8a-4c2b".to_string()))
        );
    }

    #[test]
    fn rejects_a_half_written_file() {
        // Edge writes the two lines separately, so a poll can see only the
        // first one. Treating that as success would connect to `ws://...:0`.
        assert_eq!(parse_dev_tools_active_port("41234\r\n"), None);
        assert_eq!(parse_dev_tools_active_port(""), None);
    }

    #[test]
    fn rejects_a_non_numeric_port() {
        assert_eq!(
            parse_dev_tools_active_port("not-a-port\n/devtools/browser/x"),
            None
        );
    }

    #[test]
    fn rejects_port_zero_and_empty_path() {
        assert_eq!(parse_dev_tools_active_port("0\n/devtools/browser/x"), None);
        assert_eq!(parse_dev_tools_active_port("41234\n   \n"), None);
    }
}
