//! Discussion prompt builders — construct the message text for each
//! participant in a multi-round discussion.
//!
//! The prompts are injected as the `message` parameter to
//! `dispatch_subtask_stream`, so they don't need to touch system prompts
//! or pollute the agent's original session.

use super::discussion::{DiscussionMessage, ModerationAction};

/// Build the message for a discussion participant.
///
/// Includes: topic, recent discussion history (sliding window), and
/// speaking instructions.
pub fn build_discussion_message(
    agent_name: &str,
    topic: &str,
    discussion_messages: &[DiscussionMessage],
    context_window: usize,
    round: u8,
) -> String {
    let mut parts = Vec::new();

    // 1. Topic
    parts.push(format!("📋 讨论话题：{}", topic));

    // 2. Recent discussion history (sliding window — P4.1)
    let recent: Vec<&DiscussionMessage> = if discussion_messages.len() > context_window {
        // Compress: summarize older messages, keep recent ones verbatim
        let split_point = discussion_messages.len() - context_window;
        let older = &discussion_messages[..split_point];
        let recent_slice = &discussion_messages[split_point..];

        let summary = compress_older_messages(older);
        parts.push(format!("📦 早期讨论摘要：\n{}", summary));

        recent_slice.iter().collect()
    } else {
        discussion_messages.iter().collect()
    };

    if !recent.is_empty() {
        parts.push("━━━ 最近讨论记录 ━━━".to_string());
        for msg in &recent {
            parts.push(format!("[{}] {}", msg.agent_name, msg.content));
        }
        parts.push("━━━━━━━━━━━━━━━━━━".to_string());
    }

    // 3. Speaking instructions
    parts.push(format!(
        "你是{}，这是第{}轮讨论。请针对上述话题发表你的观点。\n\
         要求：\n\
         - 直接陈述你的观点，不要复述别人的话\n\
         - 如果你同意或不同意某人的观点，说明理由\n\
         - 保持简洁（不超过 800 字）\n\
         - 如果你确实没有要补充的，回复「PASS」",
        agent_name,
        round,
    ));

    parts.join("\n\n")
}

/// Build the moderation message for the leader.
///
/// Asks the leader to decide: continue / conclude / redirect.
pub fn build_moderation_message(
    topic: &str,
    round: u8,
    round_messages: &[DiscussionMessage],
    max_rounds: u8,
) -> String {
    let mut parts = Vec::new();

    parts.push(format!("📋 讨论话题：{}", topic));
    parts.push(format!("🔄 当前轮次：{}/{}", round, max_rounds));

    parts.push("\n━━━ 本轮发言 ━━━".to_string());
    for msg in round_messages {
        parts.push(format!("[{}] {}", msg.agent_name, msg.content));
    }
    parts.push("━━━━━━━━━━━━━━".to_string());

    parts.push(
        "你是项目总管。请决定讨论的下一步：\n\
         \n\
         严格按以下 JSON 格式回复，不要输出其他内容：\n\
         \n\
         ```json\n\
         {\n\
           \"action\": \"continue\" | \"conclude\" | \"redirect\",\n\
           \"guidance\": \"下一轮引导（仅 continue/redirect 时填写，否则 null）\",\n\
           \"round_summary\": \"本轮讨论摘要（1-2 句话，给用户看）\"\n\
         }\n\
         ```\n\
         \n\
         规则：\n\
         - \"continue\": 讨论还有价值，继续下一轮\n\
         - \"conclude\": 已达成共识或充分讨论，结束讨论\n\
         - \"redirect\": 需要转向新话题（在 guidance 中写明新方向）\n\
         - 如果成员们只是互相附和没有新观点，选 \"conclude\""
    .to_string(),
    );

    parts.join("\n\n")
}

/// Build the final conclusion message for the leader.
pub fn build_conclusion_message(
    topic: &str,
    all_messages: &[DiscussionMessage],
    total_rounds: u8,
) -> String {
    let mut parts = Vec::new();

    parts.push(format!("📋 讨论话题：{}", topic));
    parts.push(format!("📊 总轮次：{}", total_rounds));

    // Compress if too many messages
    if all_messages.len() > 12 {
        let older = &all_messages[..all_messages.len() - 8];
        let recent = &all_messages[all_messages.len() - 8..];

        let summary = compress_older_messages(older);
        parts.push(format!("📦 早期讨论摘要：\n{}", summary));
        parts.push("\n━━━ 最近讨论记录 ━━━".to_string());
        for msg in recent {
            parts.push(format!("[第{}轮 · {}] {}", msg.round, msg.agent_name, msg.content));
        }
    } else {
        parts.push("\n━━━ 完整讨论记录 ━━━".to_string());
        for msg in all_messages {
            parts.push(format!(
                "[第{}轮 · {}] {}",
                msg.round, msg.agent_name, msg.content
            ));
        }
    }
    parts.push("━━━━━━━━━━━━━━━━━━".to_string());

    parts.push(
        "讨论已结束。请作为项目总管总结讨论成果：\n\
         \n\
         1. 提炼核心观点和共识\n\
         2. 列出分歧点（如有）\n\
         3. 给出你的结论和建议\n\
         \n\
         直接输出总结内容，不要 JSON 格式。"
        .to_string(),
    );

    parts.join("\n\n")
}

/// Compress older messages into a brief summary (P4.1 sliding window).
///
/// Instead of dropping old messages entirely, we generate a one-line
/// summary per message so context isn't completely lost.
fn compress_older_messages(messages: &[DiscussionMessage]) -> String {
    messages
        .iter()
        .map(|m| {
            let content = &m.content;
            let truncated: String = content.chars().take(60).collect();
            let suffix = if content.chars().count() > 60 { "..." } else { "" };
            format!("- [R{} · {}] {}{}", m.round, m.agent_name, truncated, suffix)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Check if a message content is a PASS (P4.2).
pub fn is_pass(content: &str) -> bool {
    let trimmed = content.trim();
    trimmed.eq_ignore_ascii_case("pass") || trimmed == "PASS" || trimmed == "跳过"
}

/// Check if a round of messages contains any non-PASS content (P4.2).
pub fn has_substantive_content(messages: &[DiscussionMessage]) -> bool {
    messages.iter().any(|m| !is_pass(&m.content))
}

/// Format a ModerationAction as a user-visible string.
pub fn moderation_action_label(action: &ModerationAction) -> String {
    match action {
        ModerationAction::Continue => "继续讨论".to_string(),
        ModerationAction::Conclude => "讨论结束".to_string(),
        ModerationAction::Redirect(topic) => format!("转向新话题: {}", topic),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_msg(agent_name: &str, content: &str, round: u8) -> DiscussionMessage {
        DiscussionMessage {
            id: format!("msg-{}", round),
            agent_id: format!("agent-{}", agent_name),
            agent_name: agent_name.to_string(),
            content: content.to_string(),
            round,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn build_message_includes_topic() {
        let msg = build_discussion_message("Alice", "OAuth vs JWT", &[], 8, 1);
        assert!(msg.contains("OAuth vs JWT"));
        assert!(msg.contains("Alice"));
        assert!(msg.contains("PASS"));
    }

    #[test]
    fn build_message_includes_history() {
        let history = vec![
            make_msg("Alice", "我认为应该用 OAuth", 1),
            make_msg("Bob", "同意，OAuth 更安全", 1),
        ];
        let msg = build_discussion_message("Charlie", "OAuth", &history, 8, 2);
        assert!(msg.contains("Alice"));
        assert!(msg.contains("Bob"));
        assert!(msg.contains("OAuth 更安全"));
    }

    #[test]
    fn build_message_compresses_old_messages() {
        let mut history = Vec::new();
        for i in 0..20 {
            history.push(make_msg(
                &format!("Agent{}", i % 4),
                &format!("这是第{}条消息", i),
                (i / 4) as u8 + 1,
            ));
        }
        let msg = build_discussion_message("Alice", "topic", &history, 8, 5);
        // Should contain summary of old messages
        assert!(msg.contains("早期讨论摘要"));
        // Should contain recent messages
        assert!(msg.contains("最近讨论记录"));
    }

    #[test]
    fn build_moderation_includes_round_info() {
        let round_msgs = vec![make_msg("Alice", "好主意", 1)];
        let msg = build_moderation_message("话题", 1, &round_msgs, 5);
        assert!(msg.contains("1/5"));
        assert!(msg.contains("continue"));
        assert!(msg.contains("conclude"));
    }

    #[test]
    fn build_conclusion_includes_all_messages() {
        let msgs = vec![
            make_msg("Alice", "观点A", 1),
            make_msg("Bob", "观点B", 1),
        ];
        let msg = build_conclusion_message("话题", &msgs, 1);
        assert!(msg.contains("观点A"));
        assert!(msg.contains("观点B"));
        assert!(msg.contains("讨论已结束"));
    }

    #[test]
    fn is_pass_detection() {
        assert!(is_pass("PASS"));
        assert!(is_pass("pass"));
        assert!(is_pass("  PASS  "));
        assert!(is_pass("跳过"));
        assert!(!is_pass("我有不同意见"));
        assert!(!is_pass(""));
    }

    #[test]
    fn has_substantive_content_works() {
        let all_pass = vec![
            make_msg("A", "PASS", 1),
            make_msg("B", "PASS", 1),
        ];
        assert!(!has_substantive_content(&all_pass));

        let mixed = vec![
            make_msg("A", "PASS", 1),
            make_msg("B", "我有话说", 1),
        ];
        assert!(has_substantive_content(&mixed));
    }

    #[test]
    fn compression_truncates_long_messages() {
        let long_msg = make_msg("Alice", &"很长的消息".repeat(50), 1);
        let summary = compress_older_messages(&[long_msg]);
        assert!(summary.contains("..."));
        assert!(summary.len() < 200);
    }
}
