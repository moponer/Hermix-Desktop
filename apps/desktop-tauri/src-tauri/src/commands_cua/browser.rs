//! Browser automation commands: navigate, screenshot, evaluate, click.
use super::*;


/// Bootstrap a Chrome instance and navigate to `url`.
///
/// Front-end: `invoke<CuaCommandResponse>("cua_browser_navigate", { url })`.
#[tauri::command]
pub async fn cua_browser_navigate(url: String) -> Result<CuaCommandResponse, String> {
    #[cfg(windows)]
    {
        let started = std::time::Instant::now();
        let url_c = url.clone();
        let result: Result<(String, String), String> =
            tokio::task::spawn_blocking(move || {
                let handle = hermix_cua::browser::BrowserHandle::launch(
                    hermix_cua::browser::LaunchOptions::default(),
                )
                .or_else(|_| {
                    hermix_cua::browser::BrowserHandle::attach(
                        "ws://127.0.0.1:9222/devtools/browser",
                    )
                })
                .map_err(|e| format!("browser bootstrap failed: {e}"))?;

                let target_id = handle
                    .new_tab(&url_c)
                    .map_err(|e| format!("new_tab: {e}"))?;
                let session_id = handle
                    .attach_to_target(&target_id)
                    .map_err(|e| format!("attach_to_target: {e}"))?;
                handle
                    .navigate(&session_id, &url_c)
                    .map_err(|e| format!("navigate: {e}"))?;

                Ok((target_id, session_id))
            })
            .await
            .map_err(|e| format!("spawn_blocking: {e}"))?;

        let elapsed_ms = started.elapsed().as_millis() as u64;
        let (_target_id, session_id) = result?;
        let value = serde_json::json!({
            "url": url,
            "sessionId": session_id,
        });
        Ok(CuaCommandResponse::ok("cua_browser_navigate", elapsed_ms, value))
    }
    #[cfg(not(windows))]
    {
        let _ = url;
        Err("cua_browser_navigate requires Windows".into())
    }
}

/// Take a PNG screenshot of the first page tab via CDP.
///
/// Front-end: `invoke<CuaCommandResponse>("cua_browser_screenshot", {})`.
#[tauri::command]
pub async fn cua_browser_screenshot() -> Result<CuaCommandResponse, String> {
    #[cfg(windows)]
    {
        let started = std::time::Instant::now();
        let result: Result<Vec<u8>, String> =
            tokio::task::spawn_blocking(move || {
                let handle = hermix_cua::browser::BrowserHandle::attach(
                    "ws://127.0.0.1:9222/devtools/browser",
                )
                .map_err(|e| format!("browser not running: {e}"))?;

                let targets = handle
                    .list_targets()
                    .map_err(|e| format!("list_targets: {e}"))?;
                let first_page = targets
                    .iter()
                    .find(|t| {
                        t.get("type").and_then(|v| v.as_str()) == Some("page")
                    })
                    .ok_or_else(|| "no page target found".to_string())?;
                let target_id = first_page
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "target has no id".to_string())?;
                let session_id = handle
                    .attach_to_target(target_id)
                    .map_err(|e| format!("attach_to_target: {e}"))?;

                handle
                    .screenshot(&session_id, false)
                    .map_err(|e| format!("screenshot: {e}"))
            })
            .await
            .map_err(|e| format!("spawn_blocking: {e}"))?;

        let elapsed_ms = started.elapsed().as_millis() as u64;
        let png_bytes = result?;
        let png_base64 =
            base64::engine::general_purpose::STANDARD.encode(&png_bytes);
        let value = serde_json::json!({
            "pngBase64": png_base64,
        });
        Ok(CuaCommandResponse::ok("cua_browser_screenshot", elapsed_ms, value))
    }
    #[cfg(not(windows))]
    {
        Err("cua_browser_screenshot requires Windows".into())
    }
}

/// Evaluate a JavaScript expression in the first page tab.
///
/// Front-end: `invoke<CuaCommandResponse>("cua_browser_evaluate", { expression })`.
#[tauri::command]
pub async fn cua_browser_evaluate(expression: String) -> Result<CuaCommandResponse, String> {
    #[cfg(windows)]
    {
        let started = std::time::Instant::now();
        let expr = expression.clone();
        let result: Result<serde_json::Value, String> =
            tokio::task::spawn_blocking(move || {
                let handle = hermix_cua::browser::BrowserHandle::attach(
                    "ws://127.0.0.1:9222/devtools/browser",
                )
                .map_err(|e| format!("browser not running: {e}"))?;

                let targets = handle
                    .list_targets()
                    .map_err(|e| format!("list_targets: {e}"))?;
                let first_page = targets
                    .iter()
                    .find(|t| {
                        t.get("type").and_then(|v| v.as_str()) == Some("page")
                    })
                    .ok_or_else(|| "no page target found".to_string())?;
                let target_id = first_page
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "target has no id".to_string())?;
                let session_id = handle
                    .attach_to_target(target_id)
                    .map_err(|e| format!("attach_to_target: {e}"))?;

                handle
                    .evaluate(&session_id, &expr)
                    .map_err(|e| format!("evaluate: {e}"))
            })
            .await
            .map_err(|e| format!("spawn_blocking: {e}"))?;

        let elapsed_ms = started.elapsed().as_millis() as u64;
        let value = result?;
        let payload = serde_json::json!({
            "result": value,
        });
        Ok(CuaCommandResponse::ok("cua_browser_evaluate", elapsed_ms, payload))
    }
    #[cfg(not(windows))]
    {
        let _ = expression;
        Err("cua_browser_evaluate requires Windows".into())
    }
}

/// Click a DOM element matching `selector` via `document.querySelector`.
///
/// Front-end: `invoke<CuaCommandResponse>("cua_browser_click", { selector })`.
#[tauri::command]
pub async fn cua_browser_click(selector: String) -> Result<CuaCommandResponse, String> {
    #[cfg(windows)]
    {
        let started = std::time::Instant::now();
        let sel = selector.clone();
        let result: Result<(), String> =
            tokio::task::spawn_blocking(move || {
                let handle = hermix_cua::browser::BrowserHandle::attach(
                    "ws://127.0.0.1:9222/devtools/browser",
                )
                .map_err(|e| format!("browser not running: {e}"))?;

                let targets = handle
                    .list_targets()
                    .map_err(|e| format!("list_targets: {e}"))?;
                let first_page = targets
                    .iter()
                    .find(|t| {
                        t.get("type").and_then(|v| v.as_str()) == Some("page")
                    })
                    .ok_or_else(|| "no page target found".to_string())?;
                let target_id = first_page
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "target has no id".to_string())?;
                let session_id = handle
                    .attach_to_target(target_id)
                    .map_err(|e| format!("attach_to_target: {e}"))?;

                let js = format!("document.querySelector('{sel}').click()");
                handle
                    .evaluate(&session_id, &js)
                    .map_err(|e| format!("click: {e}"))?;
                Ok(())
            })
            .await
            .map_err(|e| format!("spawn_blocking: {e}"))?;

        let elapsed_ms = started.elapsed().as_millis() as u64;
        result?;
        let value = serde_json::json!({
            "selector": selector,
        });
        Ok(CuaCommandResponse::ok("cua_browser_click", elapsed_ms, value))
    }
    #[cfg(not(windows))]
    {
        let _ = selector;
        Err("cua_browser_click requires Windows".into())
    }
}
