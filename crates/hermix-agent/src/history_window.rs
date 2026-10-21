//! 有界历史窗口 —— 长跑任务不必每轮重发整段会话历史。
//!
//! # 为什么需要
//!
//! 定时任务每轮都往**同一个会话**追加消息。磁盘上的会话照旧完整（UI 时间线、
//! 归档、检索都不受影响），但**送给模型的那一份**如果没有界，历史就会随轮次
//! 线性膨胀，每一轮都为越来越长的历史重新付费。
//!
//! 实测（2026-10-21，一条刷课任务跑 41 轮）：
//!
//! | 组成 | 体量 |
//! |---|---|
//! | 会话历史（assistant + tool 消息） | 3,718 条 / 6.1M 字符 |
//! | 系统提示 | 69K 字符（**占 1%**） |
//! | 单次请求（provider 实测） | ~1M token（模型窗口 1M） |
//!
//! 上下文压缩要等到逼近窗口才触发，所以前面每一轮都在为整段历史付费。
//!
//! # 切点规则（两条都是「不合法就会被上游拒收」）
//!
//! 1. 切点不能落在 **tool 结果**上 —— tool 结果必须跟在它的 `tool_calls`
//!    之后，孤立的 tool 结果会让 OpenAI 兼容端点报 400。
//! 2. 末尾若是一条**带 tool_calls 却没有对应结果**的 assistant 消息，把它丢掉。

use hermix_core::{Message, MessageRole};

/// 从尾部往前保留，直到累计估算 token 超过 `budget`。
///
/// 返回值永远是一段**合法**的消息序列（见模块文档的切点规则）。
/// `budget = 0` 或消息为空时返回空向量（调用方自己决定要不要兜底）。
pub fn trim_to_token_budget(messages: Vec<Message>, budget: usize) -> Vec<Message> {
    if messages.is_empty() || budget == 0 {
        return Vec::new();
    }

    // 1) 从尾部往前累加，确定保留区间的起点。
    let mut used = 0usize;
    let mut cut = messages.len();
    for i in (0..messages.len()).rev() {
        let t = crate::context::estimate_message_tokens(&messages[i]);
        // 最后一条永远保留：哪怕它自己就超了预算，也比什么都不给强。
        if used + t > budget && i + 1 < messages.len() {
            cut = i + 1;
            break;
        }
        used += t;
        cut = i;
    }

    // 2) 切点不能落在 tool 结果上 —— 往前挪到一条非 tool 消息。
    while cut < messages.len() && messages[cut].role == MessageRole::Tool {
        cut += 1;
    }
    if cut >= messages.len() {
        // 全是 tool 结果（病态情况）：宁可原样返回，也不交一份上游会拒收的载荷。
        return messages;
    }

    let mut kept = messages[cut..].to_vec();

    // 3) 末尾悬空的 tool_calls 同样非法 —— 它承诺了结果但结果不在窗口里。
    if let Some(last) = kept.last() {
        let has_calls = last
            .tool_calls
            .as_ref()
            .map(|c| !c.is_empty())
            .unwrap_or(false);
        if last.role == MessageRole::Assistant && has_calls {
            kept.pop();
        }
    }

    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_core::types::{FunctionCall, ToolCall};

    fn user(text: &str) -> Message {
        Message::user(text)
    }

    fn assistant(text: &str) -> Message {
        Message::assistant(text)
    }

    fn tool(text: &str) -> Message {
        Message {
            role: MessageRole::Tool,
            content: Some(text.to_string()),
            ..Message::default()
        }
    }

    fn assistant_with_call(text: &str) -> Message {
        Message {
            role: MessageRole::Assistant,
            content: Some(text.to_string()),
            tool_calls: Some(vec![ToolCall {
                id: "call_1".to_string(),
                function: FunctionCall {
                    name: "webview_eval".to_string(),
                    arguments: "{}".to_string(),
                },
            }]),
            ..Message::default()
        }
    }

    #[test]
    fn 预算够大时原样返回() {
        let msgs = vec![user("问"), assistant("答")];
        let kept = trim_to_token_budget(msgs.clone(), 1_000_000);
        assert_eq!(kept.len(), msgs.len());
    }

    #[test]
    fn 预算小时只留尾部() {
        let msgs = vec![
            user("问1"),
            assistant("答1"),
            user("问2"),
            assistant("答2"),
            user("问3"),
            assistant("答3"),
        ];
        // 预算只够最后一条
        let kept = trim_to_token_budget(msgs, 1);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept.last().unwrap().content.as_deref(), Some("答3"));
    }

    #[test]
    fn 切点不落在_tool_结果上() {
        let msgs = vec![
            user("问"),
            assistant_with_call("调工具"),
            tool("结果"),
            user("下一问"),
        ];
        // 预算小到只能留最后两条 —— 切点会落在 tool 结果上，必须前移。
        let kept = trim_to_token_budget(msgs, 1);
        assert!(
            !kept.first().unwrap().role.eq(&MessageRole::Tool),
            "第一条不能是 tool 结果（没有对应的 tool_call，上游会拒收）"
        );
    }

    #[test]
    fn 末尾悬空的_tool_call_被丢掉() {
        let msgs = vec![user("问"), assistant_with_call("调工具")];
        // 预算够大 → 全保留，但末尾这条 assistant 带 tool_calls 却没结果 → 丢。
        let kept = trim_to_token_budget(msgs, 1_000_000);
        assert!(
            !kept
                .iter()
                .any(|m| m.tool_calls.as_ref().map(|c| !c.is_empty()).unwrap_or(false)),
            "末尾悬空的 tool_call 必须被丢掉"
        );
    }

    #[test]
    fn 空输入返回空() {
        assert!(trim_to_token_budget(Vec::new(), 100).is_empty());
        assert!(trim_to_token_budget(vec![user("x")], 0).is_empty());
    }
}
