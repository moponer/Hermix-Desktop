//! 「空回复」兜底：从已记录的 `execution_timeline` 合成一段诚实的工具摘要。
//!
//! 从 `local_agent_service` 搬出的第二批纯函数（P1-13）。输入只有消息数组，
//! 输出是字符串 —— 不碰状态、不发请求，可以直接单测。

use hermix_core::{Message, MessageRole};

/// LAST-RESORT GUARD for empty assistant replies.
///
/// When the model stops without producing any visible text AND with no
/// `reasoning_content` to fall back on, the user would otherwise get a
/// completely blank assistant bubble — a dead-end with no summary of
/// what happened. This scans the conversation for the last assistant
/// message's `execution_timeline` (the real, recorded tool-run events)
/// and synthesizes a short, honest status report from it: how many tools
/// ran, and which (if any) failed. The report is built purely from
/// recorded events — it never invents tool outcomes.
///
/// Returns an empty string when no tool activity is found, letting the
/// caller fall through to a generic "done" line.
pub(super) fn summarize_tool_results_for_empty_reply(messages: &[Message]) -> String {
    // Find the LAST assistant message that carries a timeline.
    let timeline = messages
        .iter()
        .rev()
        .find_map(|m| {
            if m.role == MessageRole::Assistant {
                m.execution_timeline.as_ref()
            } else {
                None
            }
        });

    let events = match timeline {
        Some(events) => events,
        None => return String::new(),
    };

    // Collect tool_start events and their completion status.
    struct ToolRun {
        tool: String,
        failed: bool,
        detail: String,
    }
    let mut runs: Vec<ToolRun> = Vec::new();
    let mut failed_names: std::collections::HashSet<String> = std::collections::HashSet::new();

    for ev in events {
        match ev.event_type.as_str() {
            "tool_start" => {
                if let Some(tool) = &ev.tool {
                    runs.push(ToolRun {
                        tool: tool.clone(),
                        failed: false,
                        detail: String::new(),
                    });
                }
            }
            "tool_complete" => {
                if let Some(tool) = &ev.tool {
                    let content = ev.content.clone().unwrap_or_default();
                    let is_err = content.contains("[ERROR]")
                        || content.contains("failed")
                        || content.to_lowercase().contains("error");
                    if is_err {
                        failed_names.insert(tool.clone());
                    }
                    // Attach detail to the most recent matching run.
                    if let Some(run) = runs.iter_mut().rev().find(|r| r.tool == *tool) {
                        run.failed = is_err;
                        run.detail = content;
                    }
                }
            }
            _ => {}
        }
    }

    if runs.is_empty() {
        return String::new();
    }

    let total = runs.len();
    let failed: Vec<&ToolRun> = runs.iter().filter(|r| r.failed).collect();

    let mut out = String::new();
    out.push_str(&format!(
        "## 执行摘要\n\n本次共调用工具 {} 个",
        total
    ));
    if failed.is_empty() {
        out.push_str("，全部成功完成。");
    } else {
        out.push_str(&format!(
            "，其中 {} 个失败：\n",
            failed.len()
        ));
        for f in &failed {
            out.push_str(&format!("- `{}`", f.tool));
            let d = f.detail.trim();
            if !d.is_empty() {
                // Keep the failure reason concise.
                let head: String = d.chars().take(160).collect();
                out.push_str(&format!(": {}", head));
            }
            out.push('\n');
        }
    }
    out
}
