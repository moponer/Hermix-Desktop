//! UI automation commands: tree, find, click, focus, invoke.
use super::*;

#[cfg(windows)]
use hermix_cua::{
    safety::RiskLevel,
    trace::{TraceEvent, TraceKind},
};

/// Walk the UIA control-view tree up to `max_depth` levels and
/// return every element with its key properties (name, class name,
/// control type, bounding rect, enabled / focused flags, …).
///
/// Mirrors the LLM-facing `cua_ui_tree` tool semantics so the
/// dev console sees the same shape as the agent. The default
/// `max_depth` is **3** — desktop trees explode in size very
/// quickly (`{u32::MAX}` returns 10k+ elements and several
/// seconds; the LLM path is responsible for keeping depth low).
///
/// Each call constructs a fresh `UiaAutomation` instance inside
/// the blocking closure because the COM apartment is per-thread
/// and `spawn_blocking` gives us a fresh thread.
#[tauri::command]
pub async fn cua_ui_tree(max_depth: Option<u32>) -> Result<CuaCommandResponse, String> {
    let depth = max_depth.unwrap_or(3);
    #[cfg(windows)]
    {
        SAFETY
            .check(None, RiskLevel::Low)
            .map_err(|e| format!("cua::denied: {e}"))?;
        let started = std::time::Instant::now();
        let result: Result<Vec<hermix_cua::uia::UiElement>, hermix_cua::CuaError> =
            run_blocking(move || {
                // Re-init UIA on the worker thread (COM apartment).
                let uia = unsafe { hermix_cua::uia::UiaAutomation::new() }?;
                unsafe { uia.get_control_view_tree(depth) }
            })
            .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let elements = result.map_err(|e| e.to_string())?;
        let n = elements.len();
        let ev = TraceEvent::new(
            TraceKind::Other {
                subkind: "ui_tree".into(),
            },
            "tauri-direct",
            format!("max_depth={depth} elements={n}"),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&ev);
        let value = serde_json::json!({
            "max_depth": depth,
            "element_count": n,
            "elements": elements,
        });
        Ok(CuaCommandResponse::ok("cua_ui_tree", elapsed_ms, value))
    }
    #[cfg(not(windows))]
    {
        let _ = depth;
        Err("cua_ui_tree requires Windows".into())
    }
}

// ===========================================================================
// 10) cua_ui_find
// ===========================================================================

/// Find UIA elements matching `name`, `automation_id`, or
/// `class_name`. Multiple criteria are AND-ed together. Mirrors
/// the LLM-facing `cua_ui_find` tool. At least one criterion is
/// required.
#[tauri::command]
pub async fn cua_ui_find(
    name: Option<String>,
    automation_id: Option<String>,
    class_name: Option<String>,
    max_results: Option<usize>,
) -> Result<CuaCommandResponse, String> {
    if name.is_none() && automation_id.is_none() && class_name.is_none() {
        return Err(
            "cua::invalid_params: at least one of name/automation_id/class_name is required"
                .into(),
        );
    }
    let cap = max_results.unwrap_or(50);
    #[cfg(windows)]
    {
        SAFETY
            .check(None, RiskLevel::Low)
            .map_err(|e| format!("cua::denied: {e}"))?;
        let started = std::time::Instant::now();
        let n_name = name.clone();
        let n_automation_id = automation_id.clone();
        let n_class_name = class_name.clone();
        let result: Result<Vec<hermix_cua::uia::UiElement>, hermix_cua::CuaError> =
            run_blocking(move || {
                use hermix_cua::uia::UiaCondition;
                let mut parts: Vec<UiaCondition> = Vec::new();
                if let Some(n) = n_name {
                    parts.push(UiaCondition::Name(n));
                }
                if let Some(a) = n_automation_id {
                    parts.push(UiaCondition::AutomationId(a));
                }
                if let Some(c) = n_class_name {
                    parts.push(UiaCondition::ClassName(c));
                }
                let cond = if parts.len() == 1 {
                    parts.into_iter().next().unwrap()
                } else {
                    UiaCondition::And(parts)
                };
                let uia = unsafe { hermix_cua::uia::UiaAutomation::new() }?;
                unsafe { uia.find_all(&cond) }
            })
            .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let found = result.map_err(|e| e.to_string())?;
        let truncated = found.len() > cap;
        let slice: Vec<_> = found.into_iter().take(cap).collect();
        let n = slice.len();
        let ev = TraceEvent::new(
            TraceKind::Other {
                subkind: "ui_find".into(),
            },
            "tauri-direct",
            format!(
                "name={name:?} auto_id={automation_id:?} class={class_name:?} found={n}"
            ),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&ev);
        let value = serde_json::json!({
            "count": n,
            "truncated": truncated,
            "elements": slice,
        });
        Ok(CuaCommandResponse::ok("cua_ui_find", elapsed_ms, value))
    }
    #[cfg(not(windows))]
    {
        let _ = (name, automation_id, class_name, cap);
        Err("cua_ui_find requires Windows".into())
    }
}

// ===========================================================================
// 11) cua_ui_click
// ===========================================================================

/// Find a UIA element by `name` and click its clickable point via
/// `SendInput`. Mirrors the LLM-facing `cua_ui_click` tool.
#[tauri::command]
pub async fn cua_ui_click(name: String) -> Result<CuaCommandResponse, String> {
    #[cfg(windows)]
    {
        SAFETY
            .check(None, RiskLevel::Low)
            .map_err(|e| format!("cua::denied: {e}"))?;
        let started = std::time::Instant::now();
        let safety = &*SAFETY;
        let trace = &*TRACE;
        let session = "tauri-direct".to_string();
        let closure_name = name.clone();
        let result: Result<(i32, i32), hermix_cua::CuaError> = run_blocking(move || {
            let uia = unsafe { hermix_cua::uia::UiaAutomation::new() }?;
            let (cx, cy) = unsafe { hermix_cua::uia::click_by_name(&uia, &closure_name) }?;
            hermix_cua::action::click(
                safety,
                trace,
                &session,
                cx,
                cy,
                hermix_cua::action::MouseButton::Left,
            )?;
            Ok((cx, cy))
        })
        .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let (cx, cy) = result.map_err(|e| e.to_string())?;
        let ev = TraceEvent::new(
            TraceKind::Other {
                subkind: "ui_click".into(),
            },
            "tauri-direct",
            format!("name={name} clicked_at=({cx},{cy})"),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&ev);
        let value = serde_json::json!({
            "name": name,
            "clicked_at": { "x": cx, "y": cy },
        });
        Ok(CuaCommandResponse::ok("cua_ui_click", elapsed_ms, value))
    }
    #[cfg(not(windows))]
    {
        let _ = name;
        Err("cua_ui_click requires Windows".into())
    }
}

// ===========================================================================
// 12) cua_ui_focus
// ===========================================================================

/// Find a UIA element by `name` and set keyboard focus via
/// `IUIAutomationElement::SetFocus`. Mirrors the LLM-facing
/// `cua_ui_focus` tool.
#[tauri::command]
pub async fn cua_ui_focus(name: String) -> Result<CuaCommandResponse, String> {
    #[cfg(windows)]
    {
        SAFETY
            .check(None, RiskLevel::Low)
            .map_err(|e| format!("cua::denied: {e}"))?;
        let started = std::time::Instant::now();
        let closure_name = name.clone();
        let result: Result<(), hermix_cua::CuaError> = run_blocking(move || {
            let uia = unsafe { hermix_cua::uia::UiaAutomation::new() }?;
            unsafe { hermix_cua::uia::focus_by_name(&uia, &closure_name) }
        })
        .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        result.map_err(|e| e.to_string())?;
        let ev = TraceEvent::new(
            TraceKind::Other {
                subkind: "ui_focus".into(),
            },
            "tauri-direct",
            format!("name={name}"),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&ev);
        let value = serde_json::json!({
            "name": name,
        });
        Ok(CuaCommandResponse::ok("cua_ui_focus", elapsed_ms, value))
    }
    #[cfg(not(windows))]
    {
        let _ = name;
        Err("cua_ui_focus requires Windows".into())
    }
}

// ===========================================================================
// 13) cua_ui_invoke
// ===========================================================================

/// Find a UIA element by `name` and invoke its default action via
/// the `IUIAutomationInvokePattern`. Mirrors the LLM-facing
/// `cua_ui_invoke` tool.
#[tauri::command]
pub async fn cua_ui_invoke(name: String) -> Result<CuaCommandResponse, String> {
    #[cfg(windows)]
    {
        SAFETY
            .check(None, RiskLevel::Low)
            .map_err(|e| format!("cua::denied: {e}"))?;
        let started = std::time::Instant::now();
        let closure_name = name.clone();
        let result: Result<(), hermix_cua::CuaError> = run_blocking(move || {
            let uia = unsafe { hermix_cua::uia::UiaAutomation::new() }?;
            unsafe { hermix_cua::uia::invoke_by_name(&uia, &closure_name) }
        })
        .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        result.map_err(|e| e.to_string())?;
        let ev = TraceEvent::new(
            TraceKind::Other {
                subkind: "ui_invoke".into(),
            },
            "tauri-direct",
            format!("name={name}"),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&ev);
        let value = serde_json::json!({
            "name": name,
        });
        Ok(CuaCommandResponse::ok("cua_ui_invoke", elapsed_ms, value))
    }
    #[cfg(not(windows))]
    {
        let _ = name;
        Err("cua_ui_invoke requires Windows".into())
    }
}
