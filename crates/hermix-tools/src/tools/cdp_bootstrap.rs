//! CDP browser bootstrap
//!
//! The browser tools (browser_navigate, browser_click, ...) talk to a
//! Chrome DevTools Protocol endpoint, typically at
//! `http://127.0.0.1:9222/json/version`. If no Chrome is listening,
//! the first `browser_navigate` call would fail with
//! `"error sending request for url (http://localhost:9222/json)"` —
//! opaque and noisy.
//!
//! This module provides a single [`ensure_browser_running`] entry point
//! that:
//!
//! 1. Probes the configured CDP port with a 200ms timeout.
//! 2. If unreachable, scans well-known Windows browser binaries
//!    (Edge, Chrome via PATH, Chrome via registry) and spawns the
//!    first match with `--remote-debugging-port=9222`.
//! 3. Polls the CDP endpoint for up to 3 seconds, returning Ok as soon
//!    as it answers.
//! 4. Returns a structured error if no browser can be started — the
//!    tool layer is then free to fall back to `web_fetch`.
//!
//! No portable Chromium download (kept out of the hot path to keep
//! the binary slim and not assume the user wants to side-load a
//! second browser).
//!
//! The bootstrap is **stateless from the caller's perspective**: the
//! returned `BrowserStatus` only describes what we ended up with.
//! Process lifecycle (kill-on-drop) is left to the caller via the
//! `BrowserProcess` handle.

use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::process::Command;

use hermix_core::ToolError;

/// Default CDP port — Chrome's `--remote-debugging-port` flag.
pub const DEFAULT_CDP_PORT: u16 = 9222;

/// How long to wait for the CDP endpoint to come up after we spawn
/// the browser.
pub const CDP_READY_TIMEOUT: Duration = Duration::from_secs(3);

/// How long each probe should wait before giving up.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(200);

/// Configuration for the bootstrap. Lets callers pass in custom port +
/// headless preference (typically read from `config.yaml`).
#[derive(Debug, Clone)]
pub struct BootstrapConfig {
    /// CDP port to listen on / probe.
    pub port: u16,
    /// Whether to launch the browser with `--headless=new`.
    /// `None` = honour `HERMIX_BROWSER_HEADLESS` env var, default `true`.
    pub headless: Option<bool>,
}

impl BootstrapConfig {
    /// Resolve the effective `headless` flag by reading the env var and
    /// defaulting to `true` (matches vercel-labs/agent-browser behaviour).
    pub fn resolve_headless(&self) -> bool {
        if let Some(v) = self.headless {
            return v;
        }
        match std::env::var("HERMIX_BROWSER_HEADLESS") {
            Ok(s) => {
                let t = s.trim().to_ascii_lowercase();
                !matches!(t.as_str(), "0" | "false" | "no" | "off")
            }
            Err(_) => true,
        }
    }
}

/// Outcome of the bootstrap — returned even on failure, so the tool
/// layer can produce a specific error message ("Edge not installed"
/// vs. "Chrome failed to start" vs. "CDP endpoint not responding").
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BrowserStatus {
    /// The browser was already running and CDP is responsive.
    AlreadyRunning { version: String },
    /// The browser was started by us. `process` is a handle the
    /// caller can keep alive (drop = kill) or ignore (orphaned).
    Started { version: String, browser: String },
    /// No browser was found at any of the well-known paths.
    NotInstalled,
    /// A browser was found but spawning it failed (UAC, AV, etc).
    SpawnFailed { browser: String, reason: String },
    /// A browser was started but the CDP endpoint never answered
    /// within [`CDP_READY_TIMEOUT`].
    CdpUnresponsive { browser: String },
    /// CDP port is occupied by something that isn't a browser.
    PortOccupied { detail: String },
}

/// Errors raised by the bootstrap. Wrapped into a `ToolError` at the
/// call site.
#[derive(Debug, Error)]
pub enum BootstrapError {
    #[error("browser not installed and no portable fallback configured")]
    NotInstalled,
    #[error("found {browser} but spawn failed: {reason}")]
    SpawnFailed { browser: String, reason: String },
    #[error("started {browser} but CDP did not respond within {timeout:?}")]
    CdpTimeout { browser: String, timeout: Duration },
}

/// Ensure a browser is listening on the configured CDP port.
///
/// # Flow
/// 1. `GET http://127.0.0.1:{port}/json/version` — 200ms timeout.
/// 2. If 2xx, parse `Browser` field and return `AlreadyRunning`.
/// 3. If connection refused, run [`find_browser_binary`].
/// 4. If found, [`spawn_browser`] and poll the same URL until ready
///    or [`CDP_READY_TIMEOUT`] elapses.
/// 5. Otherwise return the structured [`BrowserStatus`] describing
///    why we couldn't satisfy the request.
pub async fn ensure_browser_running(port: u16) -> BrowserStatus {
    ensure_browser_running_with(BootstrapConfig {
        port,
        headless: None,
    })
    .await
}

/// Same as [`ensure_browser_running`] but takes a full config so
/// the headless flag is honoured.
pub async fn ensure_browser_running_with(cfg: BootstrapConfig) -> BrowserStatus {
    // 1) Probe.
    if let Some(status) = probe_cdp(cfg.port).await {
        return status;
    }

    // 2) Find a browser to launch.
    let headless = cfg.resolve_headless();
    let Some(mut candidate) = find_browser_binary() else {
        return BrowserStatus::NotInstalled;
    };
    if headless {
        candidate.extra_args.push("--headless=new".into());
    }

    // 3) Spawn.
    let browser_label = candidate.label.clone();
    if let Err(reason) = spawn_browser(&candidate, cfg.port) {
        return BrowserStatus::SpawnFailed {
            browser: browser_label,
            reason,
        };
    }

    // 4) Wait for CDP to come up.
    let deadline = Instant::now() + CDP_READY_TIMEOUT;
    while Instant::now() < deadline {
        if let Some(status) = probe_cdp(cfg.port).await {
            // We started it — convert "AlreadyRunning" into "Started".
            return match status {
                BrowserStatus::AlreadyRunning { version } => BrowserStatus::Started {
                    version,
                    browser: browser_label,
                },
                other => other,
            };
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    BrowserStatus::CdpUnresponsive {
        browser: browser_label,
    }
}

/// Convert a [`BrowserStatus`] into either Ok (with a one-line
/// description) or a `ToolError` with a clear remediation hint.
///
/// Used by the tool handler to compose its error response.
pub fn status_to_tool_result(status: &BrowserStatus) -> Result<String, ToolError> {
    match status {
        BrowserStatus::AlreadyRunning { version } => Ok(format!("browser ready ({version})")),
        BrowserStatus::Started { version, browser } => Ok(format!(
            "{browser} started, CDP ready ({version})"
        )),
        BrowserStatus::NotInstalled => Err(ToolError::ExecutionFailed(
            "BROWSER UNAVAILABLE: No Chrome/Edge found on this machine. Do NOT retry \
             browser_navigate / browser_click / browser_snapshot. Use web_fetch instead \
             for static pages, or ask the user to install Microsoft Edge or Google Chrome.".into(),
        )),
        BrowserStatus::SpawnFailed { browser, reason } => Err(ToolError::ExecutionFailed(format!(
            "BROWSER UNAVAILABLE: Found {browser} at its expected path but failed to launch \
             ({reason}). The browser tools are NOT usable on this machine right now. Do NOT \
             retry browser_navigate / browser_click / browser_snapshot — fall back to web_fetch, \
             or ask the user to start one manually with --remote-debugging-port={DEFAULT_CDP_PORT}."
        ))),
        BrowserStatus::CdpUnresponsive { browser } => Err(ToolError::ExecutionFailed(format!(
            "BROWSER UNAVAILABLE: {browser} was started but its CDP endpoint at \
             :{DEFAULT_CDP_PORT} did not respond within {CDP_READY_TIMEOUT:?}. Likely a \
             firewall or a second browser instance is holding the port. Do NOT retry \
             browser_navigate / browser_click / browser_snapshot."
        ))),
        BrowserStatus::PortOccupied { detail } => Err(ToolError::ExecutionFailed(format!(
            "Port {DEFAULT_CDP_PORT} is occupied by something that isn't a Chrome-like browser \
             ({detail}). Either stop that process or set browser.cdp_url to a different port."
        ))),
    }
}

// ---------------------------------------------------------------------------
// CDP probe
// ---------------------------------------------------------------------------

async fn probe_cdp(port: u16) -> Option<BrowserStatus> {
    let url = format!("http://127.0.0.1:{port}/json/version");
    let client = match reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(_) => return None,
    };
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
            // Parse `Browser` field for a friendly version string.
            let body: Value = resp.json().await.ok()?;
            let version = body
                .get("Browser")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            Some(BrowserStatus::AlreadyRunning { version })
        }
        Ok(resp) => Some(BrowserStatus::PortOccupied {
            detail: format!("HTTP {}", resp.status()),
        }),
        Err(e) => {
            // Connect refused / timeout / DNS — treat as "not running".
            // If the error is a "weird" one (TLS handshake to a non-TLS
            // port, etc.) we surface it as PortOccupied so the caller
            // can react.
            if e.is_connect() || e.is_timeout() || e.is_request() {
                None
            } else {
                Some(BrowserStatus::PortOccupied {
                    detail: e.to_string(),
                })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Browser candidate discovery
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct BrowserCandidate {
    /// Human-friendly label (used in error messages).
    label: String,
    /// Absolute path to the executable.
    path: String,
    /// Extra args we should pass alongside `--remote-debugging-port`.
    /// Empty for most cases; `--headless=new` if the user opts in.
    extra_args: Vec<String>,
}

fn find_browser_binary() -> Option<BrowserCandidate> {
    // 1) Microsoft Edge (preinstalled on Win10/11, most common).
    if let Some(path) = known_edge_path() {
        return Some(BrowserCandidate {
            label: "Microsoft Edge".into(),
            path,
            extra_args: Vec::new(),
        });
    }

    // 2) Chrome on PATH.
    if let Ok(path) = which("chrome") {
        return Some(BrowserCandidate {
            label: "Google Chrome".into(),
            path,
            extra_args: Vec::new(),
        });
    }

    // 3) Chrome via registry.
    if let Some(path) = chrome_via_registry() {
        return Some(BrowserCandidate {
            label: "Google Chrome".into(),
            path,
            extra_args: Vec::new(),
        });
    }

    None
}

#[cfg(target_os = "windows")]
fn known_edge_path() -> Option<String> {
    let candidates = [
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
    ];
    candidates
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .map(|s| s.to_string())
}

#[cfg(not(target_os = "windows"))]
fn known_edge_path() -> Option<String> {
    None
}

#[cfg(target_os = "windows")]
fn chrome_via_registry() -> Option<String> {
    // Avoid the `winreg` crate — just call reg.exe directly.
    // We probe two well-known paths (64-bit and 32-bit install).
    for key in [
        r"HKLM\SOFTWARE\Google\Chrome\BLBeacon",
        r"HKLM\SOFTWARE\WOW6432Node\Google\Chrome\BLBeacon",
    ] {
        if let Some(path) = reg_query_string(key, "InstallDir") {
            let exe = std::path::Path::new(&path).join("chrome.exe");
            if exe.exists() {
                return Some(exe.to_string_lossy().to_string());
            }
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn reg_query_string(key_path: &str, value_name: &str) -> Option<String> {
    let output = std::process::Command::new("reg")
        .args(["query", key_path, "/v", value_name])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The output line looks like:
    //     InstallDir    REG_SZ    C:\Program Files\Google\Chrome\Application
    for line in stdout.lines() {
        if line.contains(value_name) && line.contains("REG_SZ") {
            if let Some(idx) = line.find("REG_SZ") {
                let rest = line[idx + "REG_SZ".len()..].trim();
                if !rest.is_empty() {
                    return Some(rest.to_string());
                }
            }
        }
    }
    None
}

#[cfg(not(target_os = "windows"))]
fn chrome_via_registry() -> Option<String> {
    None
}

/// Minimal `which`-style PATH lookup. We avoid pulling in the
/// `which` crate just for this one call.
fn which(exe: &str) -> Result<String, ()> {
    let exts: Vec<&str> = if cfg!(target_os = "windows") {
        vec![".exe", ".cmd", ".bat", ""]
    } else {
        vec![""]
    };
    let path = std::env::var_os("PATH").ok_or(())?;
    for dir in std::env::split_paths(&path) {
        for ext in exts.iter() {
            let candidate = dir.join(format!("{exe}{ext}"));
            if candidate.is_file() {
                return Ok(candidate.to_string_lossy().to_string());
            }
        }
    }
    Err(())
}

// ---------------------------------------------------------------------------
// Spawn
// ---------------------------------------------------------------------------

/// Spawn Chrome/Edge with the given config. On success, the PID is
/// recorded at `<hermix_home>/runtime/browser.pid` so [`kill_spawned_chrome`]
/// can clean it up on shutdown.
///
/// We persist the PID to disk (not in memory) because hermix-desktop
/// can crash or be killed by the OS — only the PID file lets us find
/// the orphaned browser on next launch (when "port 9222 already in use"
/// appears because the previous Chrome is still running).
fn spawn_browser(candidate: &BrowserCandidate, port: u16) -> Result<u32, String> {
    // Fixed, hermix-owned user-data-dir. Chrome/Edge + --remote-debugging-port
    // REQUIRE a writable, exclusive profile dir on Windows — otherwise they
    // either fail to spawn or silently use %TEMP%\scoped_dir{N} and exit
    // because another instance holds it. Keeping the profile inside hermix_home
    // also means a re-launch reuses cookies / login state across restarts.
    let user_data_dir = hermix_config::paths::hermix_home()
        .join("runtime")
        .join("browser-profile");
    if let Err(e) = std::fs::create_dir_all(&user_data_dir) {
        return Err(format!("create user-data-dir failed: {e}"));
    }

    let mut cmd = Command::new(&candidate.path);
    cmd.arg(format!("--remote-debugging-port={port}"));
    cmd.arg(format!("--user-data-dir={}", user_data_dir.display()));
    cmd.arg("--no-first-run");
    cmd.arg("--no-default-browser-check");
    // --no-sandbox: hermix runs as a sidecar (no Chromium sandbox semantics),
    // and many managed environments block the sandbox outright.
    cmd.arg("--no-sandbox");
    for a in &candidate.extra_args {
        cmd.arg(a);
    }
    // Don't allocate a console window on Windows.
    #[cfg(target_os = "windows")]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    // Detach from our stdio so a misbehaving browser doesn't block
    // our tokio runtime.
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());

    let child = cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?;
    let pid = child.id().unwrap_or(0);
    // Persist the PID for later cleanup. Best-effort: a failure to write
    // doesn't block the spawn (the user can still talk to Chrome; they
    // just can't get a clean shutdown from us).
    let pid_path = hermix_config::paths::hermix_home()
        .join("runtime")
        .join("browser.pid");
    if let Err(e) = std::fs::write(&pid_path, pid.to_string()) {
        tracing::warn!(
            target: "hermix.tools",
            "Could not persist browser PID to {}: {e} — clean shutdown \
             on hermix exit will not be available for this browser",
            pid_path.display()
        );
    }
    Ok(pid)
}

// ---------------------------------------------------------------------------
// Clean shutdown
// ---------------------------------------------------------------------------

/// Best-effort: kill the Chrome process we previously spawned, identified
/// by `<hermix_home>/runtime/browser.pid`. We don't track a `Child` handle
/// because Chrome is intentionally detached (so a misbehaving browser
/// can't deadlock the sidecar). The PID file is the only handle we have.
///
/// On Windows, we shell out to `taskkill /F /T /PID <pid>`. `/T` makes
/// sure any subprocesses (renderer, GPU process, ...) die with the
/// browser. `/F` is the "force" flag — Chrome on Windows doesn't always
/// honour WM_CLOSE from a non-parent process, so a polite shutdown
/// silently fails and we end up with the same orphan-socket problem
/// we're trying to fix.
///
/// Safe to call when no PID file exists (returns Ok(())). Safe to call
/// when the PID file refers to a process that's already gone (the
/// `taskkill` exit code is non-zero in that case but we don't propagate
/// it — the goal is "try once, log if it fails, move on").
pub fn kill_spawned_chrome() {
    let pid_path = hermix_config::paths::hermix_home()
        .join("runtime")
        .join("browser.pid");
    let pid_str = match std::fs::read_to_string(&pid_path) {
        Ok(s) => s,
        Err(_) => return, // No PID file — never spawned one in this hermix run.
    };
    let pid = match pid_str.trim().parse::<u32>() {
        Ok(n) => n,
        Err(_) => {
            tracing::warn!(
                target: "hermix.tools",
                "browser.pid contains non-numeric content: {pid_str:?} — removing it"
            );
            let _ = std::fs::remove_file(&pid_path);
            return;
        }
    };
    if pid == 0 {
        let _ = std::fs::remove_file(&pid_path);
        return;
    }
    let result = std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match result {
        Ok(status) if status.success() => {
            tracing::info!(
                target: "hermix.tools",
                "Cleaned up orphan Chrome (PID {pid}) on shutdown"
            );
        }
        Ok(status) => {
            tracing::debug!(
                target: "hermix.tools",
                "taskkill for Chrome (PID {pid}) returned {status} — \
                 the process may already be gone"
            );
        }
        Err(e) => {
            tracing::warn!(
                target: "hermix.tools",
                "Could not run taskkill for Chrome (PID {pid}): {e}"
            );
        }
    }
    let _ = std::fs::remove_file(&pid_path);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `HERMIX_BROWSER_HEADLESS` is process-global env state;
    /// concurrent tests reading/writing it race. Lock the entire
    /// test body to keep them serialised.
    static HEADLESS_LOCK: Mutex<()> = Mutex::new(());

    #[tokio::test]
    async fn probe_cdp_returns_none_for_unused_port() {
        // Port 1 is reserved and never used in practice.
        let result = probe_cdp(1).await;
        // Either None (connection refused) or PortOccupied
        // (if the test runner is on a weird network) — both
        // are valid "not our browser" signals.
        assert!(result.is_none() || matches!(result, Some(BrowserStatus::PortOccupied { .. })));
    }

    #[tokio::test]
    async fn status_to_tool_result_not_installed_is_error() {
        let result = status_to_tool_result(&BrowserStatus::NotInstalled);
        assert!(result.is_err());
    }

    #[test]
    fn known_edge_path_returns_none_on_non_windows() {
        #[cfg(not(target_os = "windows"))]
        assert!(known_edge_path().is_none());
    }

    /// `resolve_headless`: explicit `Some(true)` wins over env var.
    #[test]
    fn resolve_headless_explicit_true_overrides_env_false() {
        let _g = HEADLESS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("HERMIX_BROWSER_HEADLESS", "0");
        let cfg = BootstrapConfig {
            port: 9222,
            headless: Some(true),
        };
        assert!(cfg.resolve_headless());
        std::env::remove_var("HERMIX_BROWSER_HEADLESS");
    }

    /// `resolve_headless`: explicit `Some(false)` wins over env var.
    #[test]
    fn resolve_headless_explicit_false_overrides_env_true() {
        let _g = HEADLESS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("HERMIX_BROWSER_HEADLESS", "1");
        let cfg = BootstrapConfig {
            port: 9222,
            headless: Some(false),
        };
        assert!(!cfg.resolve_headless());
        std::env::remove_var("HERMIX_BROWSER_HEADLESS");
    }

    /// `resolve_headless`: `None` reads env var, defaults to `true`.
    #[test]
    fn resolve_headless_none_defaults_true() {
        let _g = HEADLESS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("HERMIX_BROWSER_HEADLESS");
        let cfg = BootstrapConfig {
            port: 9222,
            headless: None,
        };
        assert!(cfg.resolve_headless(), "no config, no env → default true");
    }

    /// `resolve_headless`: `None` + env=0 returns `false`.
    #[test]
    fn resolve_headless_env_zero_returns_false() {
        let _g = HEADLESS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("HERMIX_BROWSER_HEADLESS", "0");
        let cfg = BootstrapConfig {
            port: 9222,
            headless: None,
        };
        assert!(!cfg.resolve_headless());
        std::env::remove_var("HERMIX_BROWSER_HEADLESS");
    }
}
