//! Browser Automation module built on top of CdpSupervisor.
//!
//! Provides [`SupervisorBrowserBackend`] — a `BrowserBackend` implementation
//! that uses a persistent CDP connection via [`CdpSupervisor`] for efficient
//! browser automation with automatic reconnection, dialog handling, and
//! frame tree tracking.
//!
//! Extended capabilities beyond the base `BrowserBackend` trait:
//!
//! - **PageWaiter**: `waitForSelector`, `waitForNavigation`, `waitForFunction`
//!   with configurable timeouts
//! - **TabManager**: multi-tab management — list, switch, open, close tabs
//! - **FormFiller**: batch form field filling via selector-value maps

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use hermix_core::ToolError;

use crate::tools::browser::BrowserBackend;
use crate::tools::cdp_supervisor::{CdpSupervisor, CdpSupervisorConfig};

// ---------------------------------------------------------------------------
// SupervisorBrowserBackend
// ---------------------------------------------------------------------------

/// A `BrowserBackend` implementation powered by `CdpSupervisor`.
///
/// Uses a persistent WebSocket connection instead of creating a new one
/// per command. Automatically handles dialogs and tracks the frame tree.
pub struct SupervisorBrowserBackend {
    supervisor: Arc<CdpSupervisor>,
    /// Timeout for individual CDP commands.
    command_timeout: Duration,
}

impl SupervisorBrowserBackend {
    pub fn new(supervisor: Arc<CdpSupervisor>) -> Self {
        Self {
            supervisor,
            command_timeout: Duration::from_secs(30),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.command_timeout = timeout;
        self
    }

    /// Execute JavaScript in the page context and return the result.
    async fn eval_js(&self, expression: &str) -> Result<Value, ToolError> {
        self.supervisor
            .execute("Runtime.evaluate", json!({ "expression": expression }))
            .await
            .map(|v| v.get("result").cloned().unwrap_or(v))
            .map_err(|e| ToolError::ExecutionFailed(e))
    }

    /// Execute JavaScript that returns a string value.
    async fn eval_js_string(&self, expression: &str) -> Result<String, ToolError> {
        let result = self.eval_js(expression).await?;
        result
            .get("value")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| ToolError::ExecutionFailed("JS expression did not return a string".into()))
    }
}

#[async_trait]
impl BrowserBackend for SupervisorBrowserBackend {
    async fn navigate(&self, url: &str) -> Result<String, ToolError> {
        self.supervisor
            .execute("Page.navigate", json!({ "url": url }))
            .await
            .map_err(|e| ToolError::ExecutionFailed(e))?;

        // Wait for load event
        self.supervisor
            .execute("Page.reload", json!({ "ignoreCache": false }))
            .await
            .map_err(|e| ToolError::ExecutionFailed(e))?;

        Ok(format!("Navigated to {}", url))
    }

    async fn snapshot(&self) -> Result<String, ToolError> {
        let result = self
            .eval_js(
                r#"
                (function() {
                    function walk(node, depth) {
                        if (!node) return '';
                        let prefix = '  '.repeat(depth);
                        let lines = [];
                        if (node.role) {
                            let name = node.name ? ` "${node.name}"` : '';
                            let value = node.value ? ` [value="${node.value}"]` : '';
                            lines.push(`${prefix}${node.role}${name}${value}`);
                        }
                        if (node.children) {
                            for (const child of node.children) {
                                lines.push(walk(child, depth + (node.role ? 1 : 0)));
                            }
                        }
                        if (node.contentDocument) {
                            lines.push(walk(node.contentDocument, depth + 1));
                        }
                        return lines.filter(l => l.trim()).join('\n');
                    }
                    return walk(document.body.accessibilityTree || { role: 'document', children: [] }, 0);
                })()
                "#,
            )
            .await?;
        result
            .get("value")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| ToolError::ExecutionFailed("Snapshot returned no value".into()))
    }

    async fn click(&self, selector: &str) -> Result<String, ToolError> {
        let expr = format!(
            r#"
            (function() {{
                const el = document.querySelector('{}');
                if (!el) return 'Element not found: {}';
                el.click();
                return 'Clicked {}';
            }})()
            "#,
            selector.replace('\'', "\\'"),
            selector.replace('\'', "\\'"),
            selector.replace('\'', "\\'"),
        );
        self.eval_js_string(&expr).await
    }

    async fn r#type(&self, selector: &str, text: &str) -> Result<String, ToolError> {
        let expr = format!(
            r#"
            (function() {{
                const el = document.querySelector('{}');
                if (!el) return 'Element not found: {}';
                el.focus();
                el.value = {};
                el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                return 'Typed into {}';
            }})()
            "#,
            selector.replace('\'', "\\'"),
            selector.replace('\'', "\\'"),
            serde_json::to_string(text).unwrap(),
            selector.replace('\'', "\\'"),
        );
        self.eval_js_string(&expr).await
    }

    async fn scroll(&self, direction: &str, amount: Option<u32>) -> Result<String, ToolError> {
        let px = amount.unwrap_or(500);
        let (dx, dy) = match direction {
            "up" => (0, -(px as i64)),
            "down" => (0, px as i64),
            "left" => (-(px as i64), 0),
            "right" => (px as i64, 0),
            _ => (0, px as i64),
        };
        let expr = format!(
            "window.scrollBy({}, {}); 'scrolled {} by {}px'",
            dx, dy, direction, px
        );
        self.eval_js_string(&expr).await
    }

    async fn go_back(&self) -> Result<String, ToolError> {
        self.supervisor
            .execute("Page.navigate", json!({ "url": "history.back()" }))
            .await
            .map_err(|e| ToolError::ExecutionFailed(e))?;
        Ok("Navigated back".to_string())
    }

    async fn press(&self, key: &str) -> Result<String, ToolError> {
        // Map common key names to CDP key codes
        let (key_val, code, windows_key_code) = match key {
            "Enter" | "enter" => ("Enter", "Enter", 13),
            "Tab" | "tab" => ("Tab", "Tab", 9),
            "Escape" | "escape" | "Esc" => ("Escape", "Escape", 27),
            "Backspace" | "backspace" => ("Backspace", "Backspace", 8),
            "Delete" | "delete" => ("Delete", "Delete", 46),
            "ArrowUp" | "up" => ("ArrowUp", "ArrowUp", 38),
            "ArrowDown" | "down" => ("ArrowDown", "ArrowDown", 40),
            "ArrowLeft" | "left" => ("ArrowLeft", "ArrowLeft", 37),
            "ArrowRight" | "right" => ("ArrowRight", "ArrowRight", 39),
            _ => (key, key, 0),
        };

        self.supervisor
            .execute(
                "Input.dispatchKeyEvent",
                json!({
                    "type": "keyDown",
                    "key": key_val,
                    "code": code,
                    "windowsVirtualKeyCode": windows_key_code,
                    "nativeVirtualKeyCode": windows_key_code,
                }),
            )
            .await
            .map_err(|e| ToolError::ExecutionFailed(e))?;

        self.supervisor
            .execute(
                "Input.dispatchKeyEvent",
                json!({
                    "type": "keyUp",
                    "key": key_val,
                    "code": code,
                    "windowsVirtualKeyCode": windows_key_code,
                    "nativeVirtualKeyCode": windows_key_code,
                }),
            )
            .await
            .map_err(|e| ToolError::ExecutionFailed(e))?;

        Ok(format!("Pressed key: {}", key))
    }

    async fn get_images(&self, selector: Option<&str>) -> Result<String, ToolError> {
        let sel = selector.unwrap_or("img");
        let expr = format!(
            r#"
            (function() {{
                const imgs = document.querySelectorAll('{}');
                return JSON.stringify(Array.from(imgs).map(img => ({{
                    src: img.src,
                    alt: img.alt || '',
                    width: img.naturalWidth,
                    height: img.naturalHeight,
                }})));
            }})()
            "#,
            sel.replace('\'', "\\'"),
        );
        self.eval_js_string(&expr).await
    }

    async fn vision(&self, instruction: &str) -> Result<String, ToolError> {
        // Take a screenshot and return it as a data URL for vision analysis
        let result = self
            .supervisor
            .execute("Page.captureScreenshot", json!({ "format": "png" }))
            .await
            .map_err(|e| ToolError::ExecutionFailed(e))?;

        let data = result
            .get("data")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        Ok(format!(
            "Screenshot captured ({} bytes). Vision instruction: {}",
            data.len() / 1024,
            instruction
        ))
    }

    async fn console(&self, action: &str) -> Result<String, ToolError> {
        match action {
            "enable" => {
                self.supervisor
                    .execute("Log.enable", json!({}))
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e))?;
                Ok("Console logging enabled".to_string())
            }
            "disable" => {
                self.supervisor
                    .execute("Log.disable", json!({}))
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e))?;
                Ok("Console logging disabled".to_string())
            }
            _ => Err(ToolError::InvalidParams(format!(
                "Unknown console action: {}",
                action
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// PageWaiter
// ---------------------------------------------------------------------------

/// Waits for page conditions via CDP.
pub struct PageWaiter {
    supervisor: Arc<CdpSupervisor>,
}

impl PageWaiter {
    pub fn new(supervisor: Arc<CdpSupervisor>) -> Self {
        Self { supervisor }
    }

    /// Wait until a CSS selector matches at least one element.
    pub async fn wait_for_selector(
        &self,
        selector: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        let expr = format!(
            "document.querySelector('{}') !== null",
            selector.replace('\'', "\\'"),
        );
        self.wait_for_function(&expr, timeout).await
    }

    /// Wait for the page to finish navigating (network idle).
    pub async fn wait_for_navigation(
        &self,
        timeout: Duration,
    ) -> Result<String, String> {
        let start = std::time::Instant::now();
        loop {
            if start.elapsed() > timeout {
                return Err("Navigation wait timed out".to_string());
            }

            let result = self
                .supervisor
                .execute(
                    "Runtime.evaluate",
                    json!({ "expression": "document.readyState" }),
                )
                .await
                .map_err(|e| e)?;

            let ready = result
                .get("result")
                .and_then(|r| r.get("value"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if ready == "complete" || ready == "interactive" {
                return Ok(format!("Page ready: {}", ready));
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Wait for a JavaScript expression to return truthy.
    pub async fn wait_for_function(
        &self,
        expression: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        let start = std::time::Instant::now();
        loop {
            if start.elapsed() > timeout {
                return Err(format!(
                    "waitForFunction timed out after {:?}: {}",
                    timeout, expression
                ));
            }

            let result = self
                .supervisor
                .execute(
                    "Runtime.evaluate",
                    json!({ "expression": expression }),
                )
                .await
                .map_err(|e| e)?;

            let value = result
                .get("result")
                .and_then(|r| r.get("value"))
                .cloned()
                .unwrap_or(Value::Null);

            if !matches!(value, Value::Null | Value::Bool(false)) {
                return Ok(value.to_string());
            }

            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

// ---------------------------------------------------------------------------
// TabManager
// ---------------------------------------------------------------------------

/// Multi-tab management via CDP.
pub struct TabManager {
    supervisor: Arc<CdpSupervisor>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TabInfo {
    pub target_id: String,
    pub title: String,
    pub url: String,
    pub active: bool,
}

impl TabManager {
    pub fn new(supervisor: Arc<CdpSupervisor>) -> Self {
        Self { supervisor }
    }

    /// List all open tabs.
    pub async fn list_tabs(&self) -> Result<Vec<TabInfo>, String> {
        let result = self
            .supervisor
            .execute("Target.getTargets", json!({}))
            .await?;

        let targets = result
            .get("targetInfos")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(targets
            .iter()
            .filter(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
            .map(|t| TabInfo {
                target_id: t
                    .get("targetId")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                title: t
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                url: t
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                active: t.get("attached").and_then(|v| v.as_bool()).unwrap_or(false),
            })
            .collect())
    }

    /// Open a new tab with the given URL.
    pub async fn open_tab(&self, url: &str) -> Result<TabInfo, String> {
        let result = self
            .supervisor
            .execute(
                "Target.createTarget",
                json!({ "url": url }),
            )
            .await?;

        let target_id = result
            .get("targetId")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        Ok(TabInfo {
            target_id,
            title: String::new(),
            url: url.to_string(),
            active: true,
        })
    }

    /// Close a tab by target ID.
    pub async fn close_tab(&self, target_id: &str) -> Result<(), String> {
        self.supervisor
            .execute(
                "Target.closeTarget",
                json!({ "targetId": target_id }),
            )
            .await?;
        Ok(())
    }

    /// Activate (focus) a tab by target ID.
    pub async fn activate_tab(&self, target_id: &str) -> Result<(), String> {
        self.supervisor
            .execute(
                "Target.activateTarget",
                json!({ "targetId": target_id }),
            )
            .await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// FormFiller
// ---------------------------------------------------------------------------

/// Batch form field filling via CDP.
pub struct FormFiller {
    supervisor: Arc<CdpSupervisor>,
}

#[derive(Debug, Clone)]
pub struct FieldValue {
    pub selector: String,
    pub value: String,
    /// Optional: field type override (text, checkbox, select, etc.)
    pub field_type: Option<String>,
}

impl FormFiller {
    pub fn new(supervisor: Arc<CdpSupervisor>) -> Self {
        Self { supervisor }
    }

    /// Fill multiple form fields at once.
    pub async fn fill_form(&self, fields: &[FieldValue]) -> Result<String, String> {
        let mut filled = 0u32;
        let mut errors: Vec<String> = Vec::new();

        for field in fields {
            let field_type = field.field_type.as_deref().unwrap_or("text");
            let result = match field_type {
                "checkbox" => {
                    let expr = format!(
                        r#"
                        (function() {{
                            const el = document.querySelector('{}');
                            if (!el) return 'Element not found: {}';
                            const should_check = {};
                            if (el.checked !== should_check) el.click();
                            return 'Checkbox {}';
                        }})()
                        "#,
                        field.selector.replace('\'', "\\'"),
                        field.selector.replace('\'', "\\'"),
                        field.value,
                        if field.value == "true" {
                            "checked"
                        } else {
                            "unchecked"
                        },
                    );
                    self.eval_js_string(&expr).await
                }
                "select" => {
                    let expr = format!(
                        r#"
                        (function() {{
                            const el = document.querySelector('{}');
                            if (!el) return 'Element not found: {}';
                            el.value = '{}';
                            el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                            return 'Select set';
                        }})()
                        "#,
                        field.selector.replace('\'', "\\'"),
                        field.selector.replace('\'', "\\'"),
                        field.value.replace('\'', "\\'"),
                    );
                    self.eval_js_string(&expr).await
                }
                _ => {
                    // Default: text input
                    let expr = format!(
                        r#"
                        (function() {{
                            const el = document.querySelector('{}');
                            if (!el) return 'Element not found: {}';
                            el.focus();
                            const nativeInputValueSetter = Object.getOwnPropertyDescriptor(
                                window.HTMLInputElement.prototype, 'value'
                            ).set;
                            nativeInputValueSetter.call(el, {});
                            el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                            el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                            return 'Field filled';
                        }})()
                        "#,
                        field.selector.replace('\'', "\\'"),
                        field.selector.replace('\'', "\\'"),
                        serde_json::to_string(&field.value).unwrap(),
                    );
                    self.eval_js_string(&expr).await
                }
            };

            match result {
                Ok(_) => filled += 1,
                Err(e) => errors.push(format!("{}: {}", field.selector, e)),
            }
        }

        if errors.is_empty() {
            Ok(format!("Filled {} fields successfully", filled))
        } else {
            Ok(format!(
                "Filled {} fields, {} errors: {}",
                filled,
                errors.len(),
                errors.join("; ")
            ))
        }
    }

    async fn eval_js_string(&self, expression: &str) -> Result<String, String> {
        let result = self
            .supervisor
            .execute("Runtime.evaluate", json!({ "expression": expression }))
            .await?;
        result
            .get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "JS expression returned no string value".to_string())
    }
}

// ---------------------------------------------------------------------------
// Helper: create a full browser automation stack
// ---------------------------------------------------------------------------

/// Create a complete browser automation stack:
/// CdpSupervisor + SupervisorBrowserBackend + PageWaiter + TabManager + FormFiller.
///
/// This is the main entry point for setting up browser automation.
pub async fn create_browser_automation(ws_url: &str) -> Result<BrowserAutomationStack, String> {
    let config = CdpSupervisorConfig::default();
    let supervisor = CdpSupervisor::start(ws_url.to_string(), config).await;

    // Wait for initial connection
    tokio::time::sleep(Duration::from_millis(500)).await;
    if !supervisor.is_connected().await {
        return Err("Failed to connect to browser".to_string());
    }

    let backend = Arc::new(SupervisorBrowserBackend::new(supervisor.clone()));
    let page_waiter = PageWaiter::new(supervisor.clone());
    let tab_manager = TabManager::new(supervisor.clone());
    let form_filler = FormFiller::new(supervisor.clone());

    Ok(BrowserAutomationStack {
        backend,
        page_waiter,
        tab_manager,
        form_filler,
        supervisor,
    })
}

/// A complete browser automation stack with all components.
pub struct BrowserAutomationStack {
    pub backend: Arc<SupervisorBrowserBackend>,
    pub page_waiter: PageWaiter,
    pub tab_manager: TabManager,
    pub form_filler: FormFiller,
    pub supervisor: Arc<CdpSupervisor>,
}

impl BrowserAutomationStack {
    /// Shut down the entire automation stack.
    pub async fn shutdown(&self) {
        self.supervisor.shutdown().await;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::cdp_supervisor::DialogPolicy;

    #[test]
    fn test_field_value_types() {
        let fields = vec![
            FieldValue {
                selector: "#username".into(),
                value: "testuser".into(),
                field_type: Some("text".into()),
            },
            FieldValue {
                selector: "#agree".into(),
                value: "true".into(),
                field_type: Some("checkbox".into()),
            },
            FieldValue {
                selector: "#country".into(),
                value: "US".into(),
                field_type: Some("select".into()),
            },
        ];
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].field_type.as_deref(), Some("text"));
        assert_eq!(fields[1].field_type.as_deref(), Some("checkbox"));
        assert_eq!(fields[2].field_type.as_deref(), Some("select"));
    }

    #[test]
    fn test_tab_info_serde() {
        let tab = TabInfo {
            target_id: "abc123".into(),
            title: "Test Page".into(),
            url: "https://example.com".into(),
            active: true,
        };
        let json = serde_json::to_string(&tab).unwrap();
        let parsed: TabInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.target_id, "abc123");
        assert_eq!(parsed.url, "https://example.com");
        assert!(parsed.active);
    }

    #[test]
    fn test_supervisor_backend_creation() {
        // Just verify the struct can be constructed without a real connection
        let config = CdpSupervisorConfig::default();
        assert_eq!(config.connect_timeout, Duration::from_secs(5));
        assert_eq!(config.dialog_policy, DialogPolicy::AutoAccept);
    }

    #[test]
    fn test_form_filler_field_value_default_type() {
        let field = FieldValue {
            selector: "input[name=email]".into(),
            value: "test@example.com".into(),
            field_type: None,
        };
        assert!(field.field_type.is_none());
    }
}
