//! Embedded browser panel bridge.
//!
//! The desktop UI can host a **visible browser panel** (a WebView2 view the
//! user watches) instead of a separate Edge window. For the agent tools to
//! drive that panel they only need one thing: its CDP debugging port on
//! loopback. This module owns that handshake.
//!
//! Flow:
//! 1. UI opens the panel window with
//!    `--remote-debugging-port=<port>` (WebView2 `additionalBrowserArguments`);
//! 2. UI calls [`register`] so the tools know the port;
//! 3. `ensure()` in `mod.rs` tries [`discover_ws_url`] first and attaches via
//!    `Browser::connect` — the panel's own page becomes the automation page;
//! 4. UI closes the panel → [`clear`], tools fall back to a fresh Edge.
//!
//! Everything here is best-effort: no panel, no port, dead port — the caller
//! degrades to the standalone Edge path and the tools keep working.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Persisted panel description (`<hermix_home>/browser_panel.json`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PanelState {
    /// Loopback CDP port the panel was started with.
    pub port: u16,
    /// RFC3339 timestamp — purely diagnostic ("when did this panel open").
    pub opened_at: String,
    /// Optional URL the panel was opened at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

fn state_path_in(dir: &Path) -> PathBuf {
    dir.join("browser_panel.json")
}

/// Record the panel's port (called by the desktop shell).
pub fn register(port: u16, url: Option<String>) -> std::io::Result<()> {
    register_at(&hermix_config::paths::hermix_home(), port, url)
}

/// [`register`] with an explicit home (testable).
pub fn register_at(dir: &Path, port: u16, url: Option<String>) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let state = PanelState {
        port,
        opened_at: chrono::Utc::now().to_rfc3339(),
        url,
    };
    let body = serde_json::to_string_pretty(&state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(state_path_in(dir), body)
}

/// Forget the panel (window closed / app shutting down).
pub fn clear() {
    let _ = clear_at(&hermix_config::paths::hermix_home());
}

pub fn clear_at(dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(state_path_in(dir)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// The currently registered panel, if any.
pub fn current() -> Option<PanelState> {
    current_at(&hermix_config::paths::hermix_home())
}

pub fn current_at(dir: &Path) -> Option<PanelState> {
    let raw = std::fs::read_to_string(state_path_in(dir)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Ask the panel's DevTools endpoint for its **browser-level** websocket URL.
///
/// Uses a raw HTTP/1.1 GET on purpose: one request per session creation does
/// not justify pulling an HTTP client dependency into this module.
pub async fn discover_ws_url(port: u16) -> Result<String, String> {
    // Hard deadline: the caller holds the global session lock while this
    // runs, so a half-dead WebView2 (port open, no response) would otherwise
    // stall EVERY browser tool in the process forever.
    match tokio::time::timeout(
        std::time::Duration::from_millis(800),
        discover_ws_url_inner(port),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(format!("面板调试端口 {port} 800ms 无响应（面板可能已卡死）")),
    }
}

async fn discover_ws_url_inner(port: u16) -> Result<String, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .map_err(|e| format!("连接面板调试端口 {port} 失败: {e}"))?;
    let request = format!(
        "GET /json/version HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| format!("向面板发送发现请求失败: {e}"))?;

    let mut buf = Vec::new();
    // DevTools answers this in a few hundred bytes; cap defensively so a
    // misbehaving listener cannot make us allocate unboundedly.
    let mut limited = (&mut stream).take(64 * 1024);
    limited
        .read_to_end(&mut buf)
        .await
        .map_err(|e| format!("读取面板发现响应失败: {e}"))?;


    let text = String::from_utf8_lossy(&buf);
    let body = text
        .split("\r\n\r\n")
        .nth(1)
        .ok_or_else(|| "面板调试端点返回了空响应".to_string())?;
    parse_ws_url(body)
}

/// Extract `webSocketDebuggerUrl` from a `/json/version` body (testable).
fn parse_ws_url(body: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(body.trim()).map_err(|e| format!("面板调试端点返回非法 JSON: {e}"))?;
    value
        .get("webSocketDebuggerUrl")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "面板调试端点缺少 webSocketDebuggerUrl".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_current_clear_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(current_at(tmp.path()).is_none());
        register_at(tmp.path(), 9333, Some("https://example.com".into())).unwrap();
        let state = current_at(tmp.path()).expect("registered");
        assert_eq!(state.port, 9333);
        assert_eq!(state.url.as_deref(), Some("https://example.com"));
        assert!(!state.opened_at.is_empty());
        clear_at(tmp.path()).unwrap();
        assert!(current_at(tmp.path()).is_none());
        // Clearing twice is fine (window may close twice).
        clear_at(tmp.path()).unwrap();
    }

    #[test]
    fn parses_devtools_version_body() {
        let body = r#"{
            "Browser": "Edg/131.0.2903.86",
            "webSocketDebuggerUrl": "ws://127.0.0.1:9333/devtools/browser/abc-123"
        }"#;
        assert_eq!(
            parse_ws_url(body).unwrap(),
            "ws://127.0.0.1:9333/devtools/browser/abc-123"
        );
        assert!(parse_ws_url(r#"{"Browser":"x"}"#).is_err());
        assert!(parse_ws_url("not json").is_err());
    }
}
