//! 会话消息的纯函数变换（无 IO、无 `self`）。
//!
//! 从 `local_agent_service`（原本 5600 行的组合根）搬出来的第一批。这些函数
//! 只做「消息数组 → 消息数组」的纯变换，独立成模块后可以直接单测，读组合根
//! 时也不必再穿过 190 行实现细节。

use hermix_core::{Message, MessageRole, ToolCall};

/// 去掉上一轮遗留的"瞬时提示"（nudge）system 消息。
///
/// 这些文本由 `empty_response_handler` 定义，前缀与正文必须同步 —— 所以判定
/// 也走同一个 `is_transient_nudge_text`，不在这里另写一份匹配。必须在合并
/// 相邻 assistant 之前跑，否则残留的 nudge 会把两段回答隔开。
pub(super) fn strip_transient_nudges(messages: Vec<Message>) -> Vec<Message> {
    use crate::empty_response_handler::is_transient_nudge_text;
    messages
        .into_iter()
        .filter(|m| {
            !(m.role == MessageRole::System
                && m.content
                    .as_deref()
                    .map(is_transient_nudge_text)
                    .unwrap_or(false))
        })
        .collect()
}

/// 把相邻的 assistant 消息合成一条。
///
/// 一个回合里可能产生多条 assistant 行（think-only 重试留下空占位、工具轮之间
/// 各一段），前端会把它们渲染成 N 个气泡。合并时保留最后一条的角色字段，
/// 内容/工具调用/推理各自拼接。
pub(super) fn merge_consecutive_assistant_messages(messages: Vec<Message>) -> Vec<Message> {
    use MessageRole::Assistant;

    fn is_empty(m: &Message) -> bool {
        let has_content = m
            .content
            .as_deref()
            .map(|c| !c.trim().is_empty())
            .unwrap_or(false);
        let has_tools = m
            .tool_calls
            .as_deref()
            .map(|tc| !tc.is_empty())
            .unwrap_or(false);
        let has_reasoning = m
            .reasoning_content
            .as_deref()
            .map(|c| !c.trim().is_empty())
            .unwrap_or(false);
        !has_content && !has_tools && !has_reasoning
    }

    fn flush_run(run: Vec<Message>, out: &mut Vec<Message>) {
        let non_empty: Vec<&Message> = run.iter().filter(|m| !is_empty(m)).collect();
        if non_empty.is_empty() {
            return;
        }
        if non_empty.len() == 1 {
            out.push(non_empty[0].clone());
            return;
        }
        let merged_content: String = non_empty
            .iter()
            .filter_map(|m| m.content.as_deref())
            .filter(|c| !c.is_empty())
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join("\n\n");
        let merged_tools: Vec<ToolCall> = non_empty
            .iter()
            .filter_map(|m| m.tool_calls.clone())
            .flat_map(|tc| tc.into_iter())
            .collect();
        let merged_reasoning: String = non_empty
            .iter()
            .filter_map(|m| m.reasoning_content.as_deref())
            .filter(|c| !c.is_empty())
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join("\n\n");
        let last = non_empty.last().unwrap();
        let merged = Message {
            content: if merged_content.is_empty() {
                last.content.clone()
            } else {
                Some(merged_content)
            },
            tool_calls: if merged_tools.is_empty() {
                last.tool_calls.clone()
            } else {
                Some(merged_tools)
            },
            reasoning_content: if merged_reasoning.is_empty() {
                last.reasoning_content.clone()
            } else {
                Some(merged_reasoning)
            },
            ..(*last).clone()
        };
        out.push(merged);
    }

    let mut out: Vec<Message> = Vec::with_capacity(messages.len());
    let mut run: Vec<Message> = Vec::new();
    for m in messages {
        if m.role == Assistant {
            run.push(m);
        } else {
            if !run.is_empty() {
                let drained: Vec<Message> = std::mem::take(&mut run);
                flush_run(drained, &mut out);
            }
            out.push(m);
        }
    }
    if !run.is_empty() {
        flush_run(run, &mut out);
    }
    out
}

/// `AgentOverrides.attached_file_paths` → user 消息后缀。
///
/// 附件说明写在**持久化的用户消息**上，所以重载后历史里仍能看到"这条消息带了
/// 哪些文件"；模型用 `read_file` / `read_pdf` 自己去读（大文件仍受系统提示里
/// 的上下文膨胀规则约束）。
pub(super) fn with_attached_files_note<'a>(
    text: &'a str,
    paths: Option<&'a [String]>,
) -> std::borrow::Cow<'a, str> {
    match paths {
        Some(paths) if !paths.is_empty() => std::borrow::Cow::Owned(format!(
            "{text}\n\n[Attached files: {} — read them with read_file / read_pdf]",
            paths.join(", ")
        )),
        _ => std::borrow::Cow::Borrowed(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant_with(content: &str) -> Message {
        Message::assistant(content)
    }

    #[test]
    fn merges_adjacent_assistants_and_drops_empty_ones() {
        let out = merge_consecutive_assistant_messages(vec![
            Message::user("q"),
            Message::assistant(""),
            assistant_with("a1"),
            assistant_with("a2"),
            Message::user("q2"),
        ]);
        assert_eq!(out.len(), 3, "空的 assistant 被丢，两条合并成一条");
        assert_eq!(out[1].role, MessageRole::Assistant);
        assert_eq!(out[1].content.as_deref(), Some("a1\n\na2"));
        assert_eq!(out[2].role, MessageRole::User);
    }

    #[test]
    fn attached_files_note_is_appended_only_when_present() {
        let text = "hi";
        assert_eq!(with_attached_files_note(text, None), "hi");
        assert_eq!(with_attached_files_note(text, Some(&[])), "hi");
        let paths = vec!["C:/a.pdf".to_string()];
        let with = with_attached_files_note(text, Some(&paths));
        assert!(with.contains("a.pdf"));
        assert!(with.starts_with("hi\n\n"));
    }
}
