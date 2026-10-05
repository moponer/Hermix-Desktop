//! CuaBrowserBackend — -E 自研 CDP 客户端后端。
//!
//! 使用 `hermix-cua::browser::BrowserHandle` 实现 `BrowserBackend` trait。
//! 这是 -E 的官方实现（零云服务，自研为主）。

use std::sync::Arc;

use async_trait::async_trait;
use tokio::task::spawn_blocking;

use hermix_cua::browser::{BrowserHandle, LaunchOptions};
use crate::tools::browser::BrowserBackend;
use hermix_core::ToolError;

/// Browser backend powered by `hermix-cua`'s self-developed CDP client.
///
/// Wraps `BrowserHandle` (which manages Chrome process + CDP WebSocket
/// connection) and implements the `BrowserBackend` trait.
pub struct CuaBrowserBackend {
    handle: Arc<BrowserHandle>,
    /// Cached session id for the active tab.
    session_id: String,
}

impl CuaBrowserBackend {
    /// Launch a new isolated Chrome instance.
    pub async fn launch() -> std::result::Result<Self, ToolError> {
        spawn_blocking(Self::launch_blocking)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("spawn_blocking: {e}")))?
    }

    /// Connect to an already-running Chrome.
    pub async fn attach(ws_url: &str) -> std::result::Result<Self, ToolError> {
        let url = ws_url.to_string();
        spawn_blocking(move || Self::attach_blocking(&url))
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("spawn_blocking: {e}")))?
    }

    /// Ensure a browser is running (lazy bootstrap).
    /// If no Chrome is running on the default CDP port (9222),
    /// spawns one.
    pub async fn ensure_running() -> std::result::Result<Self, ToolError> {
        spawn_blocking(Self::ensure_running_blocking)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("spawn_blocking: {e}")))?
    }

    // ── Blocking bodies ────────────────────────────────────────────
    //
    // `BrowserHandle::launch` / `::attach` reach `CdpClient::connect`,
    // which builds a fresh tokio runtime and calls `block_on` on it
    // (`hermix-cua/src/browser/cdp.rs:78,84`). Invoking that from a
    // thread already driving a runtime panics with "Cannot start a
    // runtime from within a runtime" — the crash recorded in the
    // 2026-10-05 toolchain report. These three constructors were the
    // only entries in this impl that skipped `spawn_blocking`; every
    // `BrowserBackend` method below already used it. Moving the boundary
    // up to the constructors means no caller can get this wrong by
    // forgetting to wrap, which is exactly how the bug survived review.

    fn launch_blocking() -> std::result::Result<Self, ToolError> {
        let handle = BrowserHandle::launch(LaunchOptions::default())
            .map_err(|e| ToolError::ExecutionFailed(format!("launch Chrome: {}", e)))?;
        let target_id = handle.new_tab("about:blank")
            .map_err(|e| ToolError::ExecutionFailed(format!("new tab: {}", e)))?;
        let session_id = handle.attach_to_target(&target_id)
            .map_err(|e| ToolError::ExecutionFailed(format!("attach to target: {}", e)))?;
        Ok(Self {
            handle: Arc::new(handle),
            session_id,
        })
    }

    fn attach_blocking(ws_url: &str) -> std::result::Result<Self, ToolError> {
        let handle = BrowserHandle::attach(ws_url)
            .map_err(|e| ToolError::ExecutionFailed(format!("attach to Chrome: {}", e)))?;
        // Try to find an existing tab, or create one.
        let targets = handle.list_targets()
            .map_err(|e| ToolError::ExecutionFailed(format!("list targets: {}", e)))?;
        let target_id = targets.iter()
            .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
            .or_else(|| targets.first())
            .cloned();
        let target_id = match target_id {
            Some(t) => t.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            None => handle.new_tab("about:blank")
                .map_err(|e| ToolError::ExecutionFailed(format!("new tab: {}", e)))?,
        };
        let session_id = handle.attach_to_target(&target_id)
            .map_err(|e| ToolError::ExecutionFailed(format!("attach to target: {}", e)))?;
        Ok(Self {
            handle: Arc::new(handle),
            session_id,
        })
    }

    fn ensure_running_blocking() -> std::result::Result<Self, ToolError> {
        // Try to attach first.
        match Self::attach_blocking("ws://127.0.0.1:9222/devtools/browser") {
            Ok(backend) => Ok(backend),
            Err(_) => Self::launch_blocking(),
        }
    }
}

#[async_trait]
impl BrowserBackend for CuaBrowserBackend {
    async fn navigate(&self, url: &str) -> Result<String, ToolError> {
        let handle = self.handle.clone();
        let session_id = self.session_id.clone();
        let url = url.to_string();
        spawn_blocking(move || {
            handle.navigate(&session_id, &url)
                .map(|_| format!("Navigated to {}", url))
                .map_err(|e| ToolError::ExecutionFailed(format!("navigate: {}", e)))
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("spawn_blocking: {}", e)))?
    }

    async fn snapshot(&self) -> Result<String, ToolError> {
        let handle = self.handle.clone();
        let session_id = self.session_id.clone();
        spawn_blocking(move || {
            // Use Runtime.evaluate to get the page text content.
            let result = handle
                .evaluate(&session_id, "document.body.innerText")
                .map_err(|e| ToolError::ExecutionFailed(format!("evaluate: {}", e)))?;
            let text = result
                .get("result")
                .and_then(|r| r.get("value"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            Ok(format!("Page snapshot:\n{}", text))
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("spawn_blocking: {}", e)))?
    }

    async fn click(&self, selector: &str) -> Result<String, ToolError> {
        let handle = self.handle.clone();
        let session_id = self.session_id.clone();
        let selector = selector.to_string();
        spawn_blocking(move || {
            // Use Runtime.evaluate to click via JS.
            let js = format!("document.querySelector('{}').click()", selector);
            handle
                .evaluate(&session_id, &js)
                .map_err(|e| ToolError::ExecutionFailed(format!("evaluate: {}", e)))?;
            Ok(format!("Clicked {}", selector))
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("spawn_blocking: {}", e)))?
    }

    async fn r#type(&self, selector: &str, text: &str) -> Result<String, ToolError> {
        let handle = self.handle.clone();
        let session_id = self.session_id.clone();
        let selector = selector.to_string();
        let text = text.to_string();
        spawn_blocking(move || {
            // Use Runtime.evaluate to type via JS.
            let js = format!("document.querySelector('{}').value = '{}'", selector, text);
            handle
                .evaluate(&session_id, &js)
                .map_err(|e| ToolError::ExecutionFailed(format!("evaluate: {}", e)))?;
            Ok(format!("Typed '{}' into {}", text, selector))
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("spawn_blocking: {}", e)))?
    }

    async fn scroll(&self, direction: &str, amount: Option<u32>) -> Result<String, ToolError> {
        let handle = self.handle.clone();
        let session_id = self.session_id.clone();
        let direction = direction.to_string();
        let amount = amount.unwrap_or(500);
        spawn_blocking(move || {
            let js = match direction.as_str() {
                "up" => format!("window.scrollBy(0, -{})", amount),
                "left" => format!("window.scrollBy(-{}, 0)", amount),
                "right" => format!("window.scrollBy({}, 0)", amount),
                _ => format!("window.scrollBy(0, {})", amount), // "down"
            };
            handle
                .evaluate(&session_id, &js)
                .map_err(|e| ToolError::ExecutionFailed(format!("evaluate: {}", e)))?;
            Ok(format!("Scrolled {} by {}px", direction, amount))
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("spawn_blocking: {}", e)))?
    }

    async fn go_back(&self) -> Result<String, ToolError> {
        let handle = self.handle.clone();
        let session_id = self.session_id.clone();
        spawn_blocking(move || {
            handle
                .evaluate(&session_id, "history.back()")
                .map_err(|e| ToolError::ExecutionFailed(format!("evaluate: {}", e)))?;
            Ok("Navigated back".into())
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("spawn_blocking: {}", e)))?
    }

    async fn press(&self, key: &str) -> Result<String, ToolError> {
        let handle = self.handle.clone();
        let session_id = self.session_id.clone();
        let key = key.to_string();
        spawn_blocking(move || {
            // Use Runtime.evaluate to dispatch keyboard event.
            let js = format!("document.dispatchEvent(new KeyboardEvent('keydown', {{key: '{}'}}))", key);
            handle
                .evaluate(&session_id, &js)
                .map_err(|e| ToolError::ExecutionFailed(format!("evaluate: {}", e)))?;
            Ok(format!("Pressed {}", key))
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("spawn_blocking: {}", e)))?
    }

    async fn get_images(&self, selector: Option<&str>) -> Result<String, ToolError> {
        let handle = self.handle.clone();
        let session_id = self.session_id.clone();
        let selector = selector.map(|s| s.to_string());
        spawn_blocking(move || {
            let js = match selector {
                Some(sel) => format!("Array.from(document.querySelectorAll('{}')).map(img => img.src).join('\\n')", sel),
                None => "Array.from(document.querySelectorAll('img')).map(img => img.src).join('\\n')".into(),
            };
            let result = handle
                .evaluate(&session_id, &js)
                .map_err(|e| ToolError::ExecutionFailed(format!("evaluate: {}", e)))?;
            let text = result
                .get("result")
                .and_then(|r| r.get("value"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            Ok(format!("Images:\n{}", text))
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("spawn_blocking: {}", e)))?
    }

    async fn vision(&self, instruction: &str) -> Result<String, ToolError> {
        // Take a screenshot and return it for vision analysis.
        let handle = self.handle.clone();
        let session_id = self.session_id.clone();
        let instruction = instruction.to_string();
        spawn_blocking(move || {
            let png_bytes = handle
                .screenshot(&session_id, false)
                .map_err(|e| ToolError::ExecutionFailed(format!("screenshot: {}", e)))?;
            Ok(format!("Vision analysis for '{}': [{} PNG bytes captured]", instruction, png_bytes.len()))
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("spawn_blocking: {}", e)))?
    }

    async fn console(&self, action: &str) -> Result<String, ToolError> {
        let handle = self.handle.clone();
        let session_id = self.session_id.clone();
        let action = action.to_string();
        spawn_blocking(move || {
            match action.as_str() {
                "clear" => {
                    handle
                        .evaluate(&session_id, "console.clear()")
                        .map_err(|e| ToolError::ExecutionFailed(format!("evaluate: {}", e)))?;
                    Ok("Console cleared".into())
                }
                _ => {
                    // Enable Runtime console to capture logs.
                    // For now, return a placeholder.
                    Ok("[console output not yet implemented]".into())
                }
            }
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("spawn_blocking: {}", e)))?
    }
}

#[cfg(test)]
mod tests {
    // Note: These tests require a running Chrome instance.
    // Run with `cargo test --features browser-tests` when Chrome is available.
}
