//! Chrome process manager.
//!
//! Spawns an **isolated** Chrome instance (separate `user-data-dir`
//! from the user's daily browser) with remote-debugging enabled,
//! then probes `http://127.0.0.1:<port>/json/version` until Chrome
//! answers with its `webSocketDebuggerUrl`.
//!
//! Why isolate the profile? Two reasons:
//! - Hermix shouldn't pollute the user's cookies, history, or
//!   extensions.
//! - Hermix shouldn't *be* affected by the user's extensions
//!   (uBlock, password managers, privacy filters), which would
//!   fight the agent's automation.
//!
//! Defaults to `%LOCALAPPDATA%\hermix\cua\chrome-profile\` for the
//! profile dir and port `9222` (overridable).
//!
//! `chrome.exe` discovery walks the standard install paths in
//! priority order, then falls back to `where chrome.exe` via PATH.
//! Users can also pass an explicit path via
//! [`LaunchOptions::chrome_path`].

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::error::CuaError;
use crate::CuaResult;

/// Default Chrome remote-debugging port.
pub const DEFAULT_PORT: u16 = 9222;

/// Default host Chrome binds to on `--remote-debugging-port`.
const LOOPBACK: &str = "127.0.0.1";

/// How long we'll wait for Chrome to publish a `webSocketDebuggerUrl`
/// before giving up. 10s covers a cold start on most machines.
pub const WAIT_FOR_WS_TIMEOUT: Duration = Duration::from_secs(10);

/// Options for launching a managed Chrome instance.
#[derive(Debug, Clone)]
pub struct LaunchOptions {
    /// Explicit path to `chrome.exe`. If `None`, we auto-discover.
    pub chrome_path: Option<PathBuf>,
    /// Remote-debugging port. `None` defaults to [`DEFAULT_PORT`].
    pub port: Option<u16>,
    /// Isolated user-data-dir. `None` defaults to
    /// `%LOCALAPPDATA%\hermix\cua\chrome-profile\`.
    pub profile_dir: Option<PathBuf>,
    /// Initial URL to open. `None` opens `about:blank`.
    pub initial_url: Option<String>,
    /// Extra Chrome flags (e.g. `--proxy-server=...`). Applied last.
    pub extra_flags: Vec<String>,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            chrome_path: None,
            port: None,
            profile_dir: None,
            initial_url: None,
            extra_flags: Vec::new(),
        }
    }
}

/// A live Chrome process we own. Drop calls [`ChromeManager::kill`]
/// (best-effort) so forgetting to kill explicitly is not a zombie
/// process.
pub struct ChromeManager {
    child: Option<Child>,
    port: u16,
    profile_dir: PathBuf,
    chrome_path: PathBuf,
}

impl ChromeManager {
    /// Spawn an isolated Chrome with remote-debugging enabled.
    pub fn launch(opts: LaunchOptions) -> CuaResult<Self> {
        let chrome_path = match opts.chrome_path.clone() {
            Some(p) => p,
            None => discover_chrome_exe().ok_or_else(|| {
                CuaError::Cdp(
                    "chrome.exe not found. Install Chrome or pass LaunchOptions::chrome_path".into(),
                )
            })?,
        };

        let port = opts.port.unwrap_or(DEFAULT_PORT);
        let profile_dir = match opts.profile_dir.clone() {
            Some(p) => p,
            None => default_profile_dir()?,
        };

        std::fs::create_dir_all(&profile_dir).map_err(|e| {
            CuaError::Cdp(format!(
                "create profile dir {}: {e}",
                profile_dir.display()
            ))
        })?;

        let initial_url = opts.initial_url.as_deref().unwrap_or("about:blank");

        // Build the argument list. We always pass:
        // - `--user-data-dir=<profile>`  → isolation
        // - `--remote-debugging-port=N`   → CDP endpoint
        // - `--remote-debugging-address=127.0.0.1`
        // → loopback only (don't
        // expose to LAN)
        // - `--no-first-run`              → skip first-run wizard
        // - `--no-default-browser-check`  → skip default-browser nag
        // - `--disable-extensions`         → hermix profile,
        // no user extensions
        // - `--disable-background-networking`
        // → no captive-portal probes
        // - `--disable-component-update`   → no background updater
        // - `--disable-features=Translate,InfiniteSessionRestore`
        // → no surprise side-effects
        let mut args: Vec<String> = vec![
            format!("--user-data-dir={}", profile_dir.display()),
            format!("--remote-debugging-port={port}"),
            "--remote-debugging-address=127.0.0.1".to_string(),
            "--no-first-run".to_string(),
            "--no-default-browser-check".to_string(),
            "--disable-extensions".to_string(),
            "--disable-background-networking".to_string(),
            "--disable-component-update".to_string(),
            "--disable-features=Translate,InfiniteSessionRestore".to_string(),
        ];
        args.extend(opts.extra_flags.iter().cloned());
        args.push(initial_url.to_string());

        let child = Command::new(&chrome_path)
            .args(&args)
            .stdin(Stdio::null())
            // Keep stdout/stderr so we can see Chrome's output for debugging.
            // In production, these can be redirected to a log file.
            .spawn()
            .map_err(|e| {
                CuaError::Cdp(format!(
                    "spawn chrome at {}: {e}",
                    chrome_path.display()
                ))
            })?;

        Ok(Self {
            child: Some(child),
            port,
            profile_dir,
            chrome_path,
        })
    }

    /// The port Chrome is listening on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The isolated profile directory.
    pub fn profile_dir(&self) -> &Path {
        &self.profile_dir
    }

    /// The `chrome.exe` binary we launched.
    pub fn chrome_path(&self) -> &Path {
        &self.chrome_path
    }

    /// HTTP GET `http://127.0.0.1:<port>/json/version` and parse
    /// the `webSocketDebuggerUrl` field out of the JSON payload.
    pub fn fetch_ws_endpoint(&self) -> CdpResult<String> {
        let url = format!("http://{LOOPBACK}:{}/json/version", self.port);
        let resp = ureq_get_blocking(&url)?;
        let parsed: VersionPayload = serde_json::from_str(&resp)
            .map_err(|e| CdpError::Decode(format!("parse /json/version: {e}")))?;
        parsed
            .web_socket_debugger_url
            .ok_or_else(|| CdpError::Decode("/json/version missing webSocketDebuggerUrl".into()))
    }

    /// Poll [`fetch_ws_endpoint`] until it succeeds or the deadline
    /// elapses. Returns the first successful `webSocketDebuggerUrl`.
    pub fn wait_for_ws_endpoint(&self, timeout: Duration) -> CdpResult<String> {
        let deadline = Instant::now() + timeout;
        let mut backoff = Duration::from_millis(50);
        loop {
            match self.fetch_ws_endpoint() {
                Ok(url) => return Ok(url),
                Err(e) => {
                    if Instant::now() >= deadline {
                        return Err(e);
                    }
                    std::thread::sleep(backoff);
                    // Exponential-ish backoff capped at 500ms.
                    backoff = (backoff * 2).min(Duration::from_millis(500));
                }
            }
        }
    }

    /// Terminate the Chrome process. Idempotent — calling twice is a
    /// no-op. Uses `Child::kill()` which on Windows maps to
    /// `TerminateProcess`.
    pub fn kill(&mut self) -> CdpResult<()> {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        Ok(())
    }

    /// Has the underlying process exited?
    pub fn is_alive(&mut self) -> bool {
        match self.child.as_mut() {
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => false,
        }
    }

    /// Borrow the underlying [`Child`] for advanced callers (e.g.
    /// `wait()` blocking until Chrome exits). Mostly for tests.
    pub fn child_mut(&mut self) -> Option<&mut Child> {
        self.child.as_mut()
    }

    /// Sentinel value: a manager with no owned child process.
    /// Used by [`BrowserHandle::attach`] (in `mod.rs`) when we
    /// connect to an externally-started Chrome — `Drop` then
    /// does nothing, and `is_alive` returns `false`.
    pub(crate) fn sentinel(port: u16) -> Self {
        Self {
            child: None,
            port,
            profile_dir: PathBuf::new(),
            chrome_path: PathBuf::new(),
        }
    }
}

impl Drop for ChromeManager {
    fn drop(&mut self) {
        // Best-effort kill. Don't propagate errors from Drop.
        let _ = self.kill();
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Default profile dir.
/// Windows: `%LOCALAPPDATA%\hermix\cua\chrome-profile\`
/// (the LOCALAPPDATA env var is always set on Win10/11, even
/// in service contexts where the `directories` crate would
/// fall back to roaming profile paths that are read-only.)
/// Non-Windows: `$XDG_DATA_HOME/hermix/cua/chrome-profile/` or
/// `$HOME/.local/share/hermix/cua/chrome-profile/`. Hermix-cua is
/// Windows-only in practice; the non-Windows branch exists only
/// so `cargo check --target x86_64-unknown-linux-gnu` succeeds
/// in CI without dragging in Windows-only deps.
fn default_profile_dir() -> CdpResult<PathBuf> {
    #[cfg(windows)]
    {
        let local = std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
            CdpError::Decode("LOCALAPPDATA not set on Windows".into())
        })?;
        Ok(PathBuf::from(local)
            .join("hermix")
            .join("cua")
            .join("chrome-profile"))
    }

    #[cfg(not(windows))]
    {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            return Ok(PathBuf::from(xdg)
                .join("hermix")
                .join("cua")
                .join("chrome-profile"));
        }
        let home = std::env::var_os("HOME")
            .ok_or_else(|| CdpError::Decode("HOME not set".into()))?;
        Ok(PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("hermix")
            .join("cua")
            .join("chrome-profile"))
    }
}

/// Try to find `chrome.exe` on the system. Returns the first match.
fn discover_chrome_exe() -> Option<PathBuf> {
    // 1. Explicit env var (so tests + power-users can override).
    if let Ok(p) = std::env::var("HERMIX_CHROME_PATH") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }

    // 2. Standard 64-bit / 32-bit install paths on Windows.
    let standard = [
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files\Chromium\Application\chrome.exe",
    ];
    for p in standard {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }

    // 3. Per-user install under `%LOCALAPPDATA%`.
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let pb = PathBuf::from(local)
            .join("Google")
            .join("Chrome")
            .join("Application")
            .join("chrome.exe");
        if pb.is_file() {
            return Some(pb);
        }
    }

    // 4. PATH lookup (last because it depends on shell env).
    if let Ok(p) = which("chrome") {
        return Some(p);
    }
    if let Ok(p) = which("chromium") {
        return Some(p);
    }

    None
}

/// Minimal `which` implementation. Avoids a `which` crate dep.
fn which(binary: &str) -> std::io::Result<PathBuf> {
    let exts = if cfg!(windows) {
        std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.BAT;.CMD".into())
    } else {
        String::new()
    };
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_var) {
        for ext in exts.split(';').filter(|s| !s.is_empty()) {
            let candidate = dir.join(format!("{binary}{ext}"));
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        // Unix also tries the bare name.
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("{binary} not found in PATH"),
    ))
}

/// Blocking HTTP GET via a tiny inline ureq-style helper. We avoid
/// pulling in `ureq`/`reqwest` here (sync HTTP is one-shot, the
/// `BrowserHandle` CDP layer is async). The 2s timeout covers a
/// hung Chrome.
fn ureq_get_blocking(url: &str) -> CdpResult<String> {
    // Raw TCP HTTP GET — avoids pulling in reqwest::blocking which
    // internally creates a tokio runtime (panics if already inside one).
    let parsed = url::Url::parse(url)
        .map_err(|e| CdpError::Decode(format!("parse url {url}: {e}")))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| CdpError::Decode(format!("no host in {url}")))?;
    let port = parsed.port_or_known_default().unwrap_or(80);
    use std::io::{Read, Write};
    use std::net::TcpStream;
    let mut stream = TcpStream::connect((host, port))
        .map_err(|e| CdpError::Http(format!("connect {host}:{port}: {e}")))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| CdpError::Http(format!("set_read_timeout: {e}")))?;
    let req = format!(
        "GET {} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n",
        parsed.path()
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| CdpError::Http(format!("write: {e}")))?;
    let mut buf = String::new();
    stream
        .read_to_string(&mut buf)
        .map_err(|e| CdpError::Http(format!("read: {e}")))?;
    // Strip HTTP response head.
    let body = buf
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .ok_or_else(|| CdpError::Decode("no CRLFCRLF in HTTP response".into()))?;
    Ok(body.to_string())
}

// ---------------------------------------------------------------------------
// /json/version payload
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct VersionPayload {
    #[serde(rename = "webSocketDebuggerUrl")]
    web_socket_debugger_url: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    browser: Option<String>,
}

// ---------------------------------------------------------------------------
// Local error type so we don't need to thread reqwest/std errors
// into the public CuaError surface.
// The `From<CdpError> for CuaError` impl lives in `cdp.rs` —
// keeping it there means the orphan rule is satisfied exactly
// once and both modules can `?`-lift CdpError into CuaError.
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum CdpError {
    Http(String),
    Decode(String),
    Timeout(String),
    Protocol(String),
}

pub type CdpResult<T> = std::result::Result<T, CdpError>;

impl std::fmt::Display for CdpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CdpError::Http(s) => write!(f, "http: {s}"),
            CdpError::Decode(s) => write!(f, "decode: {s}"),
            CdpError::Timeout(s) => write!(f, "timeout: {s}"),
            CdpError::Protocol(s) => write!(f, "protocol: {s}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_options_use_default_port() {
        let opts = LaunchOptions::default();
        assert_eq!(opts.port, None);
        assert_eq!(opts.chrome_path, None);
    }

    #[test]
    fn which_returns_err_for_missing_binary() {
        let res = which("definitely_not_a_real_binary_xyz_123");
        assert!(res.is_err());
    }

    #[test]
    fn default_profile_dir_resolves_to_a_path() {
        let p = default_profile_dir().expect("profile dir");
        assert!(p.ends_with("chrome-profile") || p.to_string_lossy().contains("chrome-profile"));
    }

    #[test]
    fn fetch_ws_endpoint_fails_when_nothing_listens() {
        // Use a port nothing should be on. We pick 1 — privileged,
        // almost certainly unbound. The probe should fail fast
        // with Http(connect: ...) or Decode.
        let mgr = ChromeManager {
            child: None,
            port: 1,
            profile_dir: PathBuf::from("."),
            chrome_path: PathBuf::from("chrome.exe"),
        };
        let res = mgr.fetch_ws_endpoint();
        assert!(res.is_err(), "expected error, got {res:?}");
    }

    #[test]
    fn wait_for_ws_endpoint_times_out_quickly_on_dead_port() {
        let mgr = ChromeManager {
            child: None,
            port: 1,
            profile_dir: PathBuf::from("."),
            chrome_path: PathBuf::from("chrome.exe"),
        };
        let start = Instant::now();
        let res = mgr.wait_for_ws_endpoint(Duration::from_millis(200));
        let elapsed = start.elapsed();
        assert!(res.is_err());
        // Bounded by ~200ms timeout + a few backoffs.
        assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
    }
}
