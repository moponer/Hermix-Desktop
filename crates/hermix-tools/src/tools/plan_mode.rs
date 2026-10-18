//! Plan mode — read-only research phase with an explicit exit gate
//! (ZCode `EnterPlanMode` / `ExitPlanMode` analog).
//!
//! When active, the agent loop denies mutating tools at dispatch time
//! (see [`is_blocked_in_plan_mode`]) while leaving every read-only tool
//! available. The flag lives on a shared `Arc<RwLock<bool>>` owned by the
//! registry — the same pattern as `terminal_workspace` — so these tools can
//! flip it without a `&mut` handle on the loop.
//!
//! Design notes:
//! - **Deny-list, not allow-list.** Unknown/new tools stay callable in plan
//!   mode; only the known mutators are blocked. An allow-list would silently
//!   brick every future read tool (browser snapshot, MCP reads, …) until
//!   someone remembered to register it here.
//! - The denial happens in `do_tool_turn` BEFORE dispatch, so no approval
//!   card is ever shown for a blocked call.
//! - A system reminder is injected while the mode is on so the model cannot
//!   forget it after compression drops the `enter_plan_mode` result.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::Value;
use std::sync::Arc;

use hermix_core::{tool_schema, traits::RiskLevel, JsonSchema, ToolError, ToolHandler, ToolSchema};

use crate::ToolRegistry;

/// Tools denied while plan mode is active. Prefixes ending in `*` match a
/// whole family (`win_*`). Keep this list about *side effects on the user's
/// machine*: file writes, command execution, automation, message sending.
const PLAN_MODE_BLOCKED: &[&str] = &[
    // File/shell mutations
    "terminal",
    "execute_code",
    "write_file",
    "patch",
    "move_file",
    "file_converter",
    "office_export",
    "html_to_docx",
    // Automation / scheduling
    "cronjob",
    "workflow",
    "custom_app_manager",
    "kanban",
    // Maintenance / mutation that also touches disk or config
    "disk_cleanup",
    "storage_clean",
    "skill_manage",
    "memory_store",
    "memory_delete",
    "workbench_manager",
    "workbench_write",
    "vendor_install",
    "send_message",
    "bbs_post",
    // Desktop automation families — write actions only. Read-only `win_*`
    // (list windows / find elements / screenshot) stays available so plan
    // mode can still inspect the screen.
    "win_click",
    "win_type",
    "win_key",
    "win_launch_app",
    "win_focus_window",
    "win_set_window_state",
    "win_close_window",
    "win_drag",
    // Browser interactions with side effects (navigation/snapshot/screenshot
    // remain allowed — they are read-only)
    "browser_click",
    "browser_type",
    // Credential mutation
    "vault_import",
    "vault_update",
    "vault_delete",
];

/// `true` when `tool_name` must be denied under plan mode.
pub fn is_blocked_in_plan_mode(tool_name: &str) -> bool {
    PLAN_MODE_BLOCKED.iter().any(|pat| match pat.strip_suffix('*') {
        Some(prefix) => tool_name.starts_with(prefix),
        None => *pat == tool_name,
    })
}

/// Register `enter_plan_mode` + `exit_plan_mode`.
pub fn register(registry: &ToolRegistry) {
    let handle = registry.plan_mode_handle();
    let enter = Arc::new(PlanModeHandler {
        active: handle.clone(),
        enter: true,
    });
    let schema = enter.schema();
    registry.register_with_risk(
        "enter_plan_mode",
        "meta",
        schema,
        enter,
        Arc::new(|| true),
        vec![],
        false,
        "Enter PLAN MODE: a read-only research phase. Mutating tools (terminal, write_file/patch, automation, browser clicks, …) are denied until you call exit_plan_mode, while all read tools keep working. Use it when the task needs a plan the user should approve before any writes happen.",
        "📋",
        None,
        RiskLevel::Safe,
        false,
    );

    let exit = Arc::new(PlanModeHandler {
        active: handle,
        enter: false,
    });
    let schema = exit.schema();
    registry.register_with_risk(
        "exit_plan_mode",
        "meta",
        schema,
        exit,
        Arc::new(|| true),
        vec![],
        false,
        "Leave PLAN MODE and restore the full toolset. Call it right before executing the plan you produced (the user has approved it, or you finished the research and the plan is in your message).",
        "▶️",
        None,
        RiskLevel::Safe,
        false,
    );
}

struct PlanModeHandler {
    active: Arc<std::sync::RwLock<bool>>,
    enter: bool,
}

#[async_trait]
impl ToolHandler for PlanModeHandler {
    async fn execute(&self, _params: Value) -> Result<String, ToolError> {
        let mut flag = self
            .active
            .write()
            .map_err(|_| ToolError::ExecutionFailed("plan mode lock poisoned".into()))?;
        *flag = self.enter;
        Ok(if self.enter {
            "已进入计划模式（只读调研）。写文件、执行命令、自动化类工具现在会被拒绝；\
             先用读取/搜索/网页类工具完成调研，给出清晰计划，然后调用 exit_plan_mode 继续执行。"
                .to_string()
        } else {
            "已退出计划模式，全部工具恢复可用。按计划执行。".to_string()
        })
    }

    fn schema(&self) -> ToolSchema {
        tool_schema(
            if self.enter { "enter_plan_mode" } else { "exit_plan_mode" },
            if self.enter {
                "Enter plan mode: read-only research. Mutating tools are denied until you call exit_plan_mode."
            } else {
                "Exit plan mode: restore the full toolset and execute the plan."
            },
            JsonSchema::object(IndexMap::new(), vec![]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn blocked_list_covers_mutators_but_not_readers() {
        for blocked in [
            "terminal",
            "write_file",
            "patch",
            "cronjob",
            "browser_click",
            "win_click",
            "disk_cleanup",
        ] {
            assert!(is_blocked_in_plan_mode(blocked), "{blocked} must be blocked");
        }
        for allowed in [
            "read_file",
            "search_files",
            "read_image",
            "web_search",
            "web_extract",
            "browser_snapshot",
            "browser_navigate",
            "skills_list",
            "todo",
            "enter_plan_mode",
            "exit_plan_mode",
            "session_search",
            // Read-only desktop inspection stays available (screenshot /
            // window list / element find); only the write actions are denied.
            "win_find_elements",
            "win_list_windows",
        ] {
            assert!(
                !is_blocked_in_plan_mode(allowed),
                "{allowed} must stay available in plan mode"
            );
        }
    }

    #[tokio::test]
    async fn enter_and_exit_toggle_the_shared_flag() {
        let flag = Arc::new(std::sync::RwLock::new(false));
        let enter = PlanModeHandler {
            active: flag.clone(),
            enter: true,
        };
        enter.execute(json!({})).await.unwrap();
        assert!(*flag.read().unwrap());

        let exit = PlanModeHandler {
            active: flag.clone(),
            enter: false,
        };
        exit.execute(json!({})).await.unwrap();
        assert!(!*flag.read().unwrap());
    }
}
