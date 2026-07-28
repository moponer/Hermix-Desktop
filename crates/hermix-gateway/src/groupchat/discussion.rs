//! DiscussionEngine — multi-agent discussion orchestration.
//!
//! When the leader decides that a topic needs multi-round discussion
//! rather than direct task delegation, it returns a `DiscussionConfig`
//! in its `LeaderDecision`. The runtime spawns a `DiscussionEngine`
//! that runs the discussion loop:
//!
//! 1. Each round: sequentially dispatch each participant with the
//!    accumulated context (topic + recent messages).
//! 2. After each round: ask the leader to moderate (continue / conclude
//!    / redirect).
//! 3. On conclusion: ask the leader for a final summary.
//!
//! Key design decisions (see `docs/design/DISCUSSION_MODE_DESIGN.md`):
//! - **Serialized speaking**: participants speak one at a time, each
//!   seeing the messages of those who spoke before them in the same round.
//! - **Independent session**: uses `{room_id}-discussion-{id}` as session
//!   ID so the discussion doesn't pollute agents' original sessions.
//! - **Hard ceiling**: `max_rounds` (default 5, max 10) prevents infinite loops.
//! - **PASS detection** (P4.2): if all participants PASS in a round,
//!   the discussion ends early.
//! - **Sliding window** (P4.1): context is compressed when it exceeds
//!   `context_window` messages, keeping prompt size bounded.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::dispatcher::{AgentDispatcher, StreamDelta};
use super::discussion_prompt::{
    build_conclusion_message, build_discussion_message, build_moderation_message,
    has_substantive_content, is_pass, moderation_action_label,
};
use super::persistence::MessageType;

// ── Configuration ──────────────────────────────────────────

/// Discussion mode configuration — specified by the Leader in
/// `LeaderDecision.discussion`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscussionConfig {
    /// The topic to discuss.
    pub topic: String,
    /// Maximum number of rounds (hard ceiling). Default 5, max 10.
    #[serde(default = "default_max_rounds")]
    pub max_rounds: u8,
    /// Participating agent IDs (not including the leader).
    #[serde(default)]
    pub participants: Vec<String>,
    /// Number of recent messages to keep verbatim in context. Older
    /// messages are compressed to one-line summaries. Default 8.
    #[serde(default = "default_context_window")]
    pub context_window: usize,
}

fn default_max_rounds() -> u8 {
    5
}

fn default_context_window() -> usize {
    8
}

impl Default for DiscussionConfig {
    fn default() -> Self {
        Self {
            topic: String::new(),
            max_rounds: default_max_rounds(),
            participants: Vec::new(),
            context_window: default_context_window(),
        }
    }
}

// ── State ──────────────────────────────────────────────────

/// Running state of a discussion instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscussionState {
    pub discussion_id: String,
    pub room_id: String,
    pub node_id: String,
    pub config: DiscussionConfig,
    pub current_round: u8,
    pub messages: Vec<DiscussionMessage>,
    pub status: DiscussionStatus,
    pub triggered_by: String,
    /// Final conclusion text (set when discussion concludes).
    #[serde(default)]
    pub conclusion: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DiscussionStatus {
    Pending,
    Active,
    AwaitingModeration,
    Concluded,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscussionMessage {
    pub id: String,
    pub agent_id: String,
    pub agent_name: String,
    pub content: String,
    pub round: u8,
    pub timestamp: DateTime<Utc>,
}

// ── Moderation ─────────────────────────────────────────────

/// Leader's decision after a round.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModerationDecision {
    pub action: ModerationAction,
    pub guidance: Option<String>,
    pub round_summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ModerationAction {
    Continue,
    Conclude,
    Redirect(String),
}

// ── Active discussion handle ───────────────────────────────

/// Tracks an in-flight discussion so it can be cancelled (P2.1).
pub struct ActiveDiscussion {
    pub discussion_id: String,
    pub cancel_token: CancellationToken,
}

// ── Discussion Engine ──────────────────────────────────────

/// Orchestrates a multi-round discussion.
///
/// One instance per discussion. The engine is spawned on a `tokio::spawn`
/// task by the runtime and runs autonomously until completion or
/// cancellation.
pub struct DiscussionEngine {
    manager: Arc<super::manager::GroupChatManager>,
    /// Callback to publish RoomEvents to the WS broadcast channel.
    event_publisher: Arc<dyn DiscussionEventPublisher>,
}

/// Trait abstracting event publication so the engine can be unit-tested
/// without a full `TaskEngineRuntime`.
pub trait DiscussionEventPublisher: Send + Sync {
    fn publish_discussion_started(
        &self,
        room_id: &str,
        discussion_id: &str,
        topic: &str,
        participants: &[String],
        max_rounds: u8,
    );
    fn publish_discussion_chunk(
        &self,
        room_id: &str,
        agent_id: &str,
        agent_name: &str,
        round: u8,
        delta: &str,
    );
    fn publish_discussion_message(
        &self,
        room_id: &str,
        discussion_id: &str,
        agent_id: &str,
        agent_name: &str,
        content: &str,
        round: u8,
    );
    fn publish_round_summary(
        &self,
        room_id: &str,
        discussion_id: &str,
        round: u8,
        summary: &str,
        next_action: &str,
    );
    fn publish_discussion_concluded(
        &self,
        room_id: &str,
        discussion_id: &str,
        total_rounds: u8,
    );
    fn publish_soft_error(&self, room_id: &str, code: &str, message: &str);
}

impl DiscussionEngine {
    pub fn new(
        manager: Arc<super::manager::GroupChatManager>,
        event_publisher: Arc<dyn DiscussionEventPublisher>,
    ) -> Self {
        Self {
            manager,
            event_publisher,
        }
    }

    /// Run the complete discussion flow. Returns the final state.
    pub async fn run(
        self: Arc<Self>,
        room_id: &str,
        node_id: &str,
        mut config: DiscussionConfig,
        trigger_message: &str,
        cancel_token: CancellationToken,
    ) -> Result<DiscussionState, String> {
        // Clamp max_rounds to [1, 10].
        config.max_rounds = config.max_rounds.clamp(1, 10);
        if config.context_window == 0 {
            config.context_window = 8;
        }

        let discussion_id = Uuid::new_v4().to_string();
        let mut state = DiscussionState {
            discussion_id: discussion_id.clone(),
            room_id: room_id.to_string(),
            node_id: node_id.to_string(),
            config: config.clone(),
            current_round: 1,
            messages: Vec::new(),
            status: DiscussionStatus::Active,
            triggered_by: trigger_message.to_string(),
            conclusion: None,
        };

        // Announce discussion start.
        self.event_publisher.publish_discussion_started(
            room_id,
            &discussion_id,
            &config.topic,
            &config.participants,
            config.max_rounds,
        );

        // Main loop.
        while state.current_round <= state.config.max_rounds {
            // Check for cancellation (P2.1).
            if cancel_token.is_cancelled() {
                state.status = DiscussionStatus::Cancelled;
                tracing::info!(
                    discussion = %discussion_id,
                    "discussion cancelled by user"
                );
                return Ok(state);
            }

            // ── Run one round ──
            // run_round takes &mut state.messages so later agents in
            // the same round see earlier agents' messages.
            let round_messages = self
                .run_round(&mut state, &cancel_token)
                .await;

            // Check for cancellation during the round.
            if cancel_token.is_cancelled() {
                state.status = DiscussionStatus::Cancelled;
                return Ok(state);
            }

            // P4.2: PASS detection — if all participants passed, end early.
            if !has_substantive_content(&round_messages) {
                tracing::info!(
                    discussion = %discussion_id,
                    round = state.current_round,
                    "all participants passed, ending discussion early"
                );
                break;
            }

            // ── Moderation ──
            state.status = DiscussionStatus::AwaitingModeration;
            let decision = self
                .request_moderation(&state, &round_messages)
                .await;

            match decision {
                Ok(dec) => {
                    let next_label = moderation_action_label(&dec.action);
                    self.event_publisher.publish_round_summary(
                        room_id,
                        &discussion_id,
                        state.current_round,
                        &dec.round_summary,
                        &next_label,
                    );

                    match &dec.action {
                        ModerationAction::Continue => {
                            state.current_round += 1;
                            state.status = DiscussionStatus::Active;
                        }
                        ModerationAction::Conclude => {
                            state.status = DiscussionStatus::Concluded;
                            break;
                        }
                        ModerationAction::Redirect(new_topic) => {
                            state.config.topic = new_topic.clone();
                            state.current_round += 1;
                            state.status = DiscussionStatus::Active;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        discussion = %discussion_id,
                        error = %e,
                        "moderation failed, continuing discussion"
                    );
                    // If moderation fails and we're at the last round,
                    // just conclude. Otherwise continue.
                    if state.current_round >= state.config.max_rounds {
                        state.status = DiscussionStatus::Concluded;
                        break;
                    }
                    state.current_round += 1;
                    state.status = DiscussionStatus::Active;
                }
            }
        }

        // Hard ceiling reached.
        if state.status == DiscussionStatus::Active {
            state.status = DiscussionStatus::Concluded;
        }

        // ── Final conclusion ──
        let conclusion = self.request_conclusion(&state).await;
        match conclusion {
            Ok(text) => {
                // Save as a leader message in the room.
                let _ = self
                    .manager
                    .save_agent_message(
                        room_id,
                        "__leader__",
                        "项目总管",
                        &text,
                        MessageType::Discussion,
                    )
                    .await;
                state.conclusion = Some(text);
            }
            Err(e) => {
                tracing::warn!(
                    discussion = %discussion_id,
                    error = %e,
                    "failed to generate conclusion"
                );
                self.event_publisher.publish_soft_error(
                    room_id,
                    "discussion_conclusion_failed",
                    &format!("讨论总结生成失败: {}", e),
                );
            }
        }

        self.event_publisher.publish_discussion_concluded(
            room_id,
            &discussion_id,
            state.current_round,
        );

        Ok(state)
    }

    /// Execute one round: sequentially dispatch each participant.
    ///
    /// Takes `&mut state` so it can push messages into `state.messages`
    /// as they arrive — later agents in the same round see earlier
    /// agents' messages.
    async fn run_round(
        &self,
        state: &mut DiscussionState,
        cancel_token: &CancellationToken,
    ) -> Vec<DiscussionMessage> {
        let round = state.current_round;
        let mut round_messages = Vec::new();

        let dispatcher = match self.manager.dispatcher() {
            Some(d) => d,
            None => {
                self.event_publisher.publish_soft_error(
                    &state.room_id,
                    "no_dispatcher",
                    "没有可用的 agent dispatcher",
                );
                return round_messages;
            }
        };

        let session_id = format!("{}-discussion-{}", state.room_id, state.discussion_id);

        for agent_id in &state.config.participants {
            // Check cancellation between agents.
            if cancel_token.is_cancelled() {
                break;
            }

            let agent_name = self.manager.agent_display_name(&state.room_id, agent_id);

            // Build context message.
            let message = build_discussion_message(
                &agent_name,
                &state.config.topic,
                &state.messages,
                state.config.context_window,
                round,
            );

            let system_prompt = format!(
                "你是{}，群聊讨论的参与者。你正在参与一场团队讨论。\n\
                 请基于讨论记录发表你的真实观点。简洁、有料、有立场。",
                agent_name
            );

            // Streaming chunk callback.
            let publisher = self.event_publisher.clone();
            let room_id_owned = state.room_id.clone();
            let agent_id_owned = agent_id.clone();
            let agent_name_owned = agent_name.clone();
            let on_chunk: Box<dyn Fn(StreamDelta) + Send + Sync> =
                Box::new(move |delta: StreamDelta| {
                    publisher.publish_discussion_chunk(
                        &room_id_owned,
                        &agent_id_owned,
                        &agent_name_owned,
                        round,
                        &delta.content,
                    );
                });

            let result = dispatcher
                .dispatch_subtask_stream(
                    &state.room_id,
                    agent_id,
                    &session_id,
                    &system_prompt,
                    &message,
                    "member",
                    on_chunk,
                )
                .await;

            match result {
                Ok(content) => {
                    let clean = content.trim().to_string();
                    let msg = DiscussionMessage {
                        id: Uuid::new_v4().to_string(),
                        agent_id: agent_id.clone(),
                        agent_name: agent_name.clone(),
                        content: clean,
                        round,
                        timestamp: Utc::now(),
                    };

                    // Publish the message event.
                    self.event_publisher.publish_discussion_message(
                        &state.room_id,
                        &state.discussion_id,
                        agent_id,
                        &agent_name,
                        &msg.content,
                        round,
                    );

                    // Persist to room messages.
                    let _ = self
                        .manager
                        .save_agent_message(
                            &state.room_id,
                            agent_id,
                            &agent_name,
                            &msg.content,
                            MessageType::Discussion,
                        )
                        .await;

                    round_messages.push(msg.clone());
                    // Push to state.messages so later agents in the
                    // same round see this message.
                    state.messages.push(msg);
                }
                Err(e) => {
                    tracing::warn!(
                        agent = %agent_id,
                        round = round,
                        error = %e,
                        "discussion dispatch failed, skipping agent"
                    );
                    // F7: Skip failed agent, continue round.
                }
            }
        }

        round_messages
    }

    /// Request moderation from the leader.
    async fn request_moderation(
        &self,
        state: &DiscussionState,
        round_messages: &[DiscussionMessage],
    ) -> Result<ModerationDecision, String> {
        let dispatcher = self.manager.dispatcher().ok_or("no dispatcher")?;
        let session_id = format!("{}-discussion-{}", state.room_id, state.discussion_id);

        let message = build_moderation_message(
            &state.config.topic,
            state.current_round,
            round_messages,
            state.config.max_rounds,
        );

        let system_prompt =
            "你是项目总管，正在主持一场团队讨论。请决定讨论的下一步。".to_string();

        let raw = dispatcher
            .dispatch_to_agent(
                &state.room_id,
                "__leader__",
                &session_id,
                &system_prompt,
                &message,
                "",
            )
            .await?;

        parse_moderation_decision(&raw)
    }

    /// Request the final conclusion from the leader.
    async fn request_conclusion(&self, state: &DiscussionState) -> Result<String, String> {
        let dispatcher = self.manager.dispatcher().ok_or("no dispatcher")?;
        let session_id = format!("{}-discussion-{}", state.room_id, state.discussion_id);

        let message =
            build_conclusion_message(&state.config.topic, &state.messages, state.current_round);

        let system_prompt = "你是项目总管。讨论已结束，请总结讨论成果。".to_string();

        dispatcher
            .dispatch_to_agent(
                &state.room_id,
                "__leader__",
                &session_id,
                &system_prompt,
                &message,
                "",
            )
            .await
    }
}

/// Parse the leader's moderation decision JSON.
pub fn parse_moderation_decision(raw: &str) -> Result<ModerationDecision, String> {
    let trimmed = raw.trim();

    // Fast path: clean JSON.
    if let Ok(dec) = serde_json::from_str::<ModerationDecision>(trimmed) {
        return Ok(dec);
    }

    // Tolerant path: strip code fences and find JSON object.
    let body = strip_code_fence(trimmed);
    let start = body.find('{').ok_or("no JSON object found in moderation response")?;
    let end = body.rfind('}').ok_or("no closing brace in moderation response")?;
    let block = &body[start..=end];

    #[derive(Deserialize)]
    #[serde(rename_all = "snake_case")]
    struct RawModeration {
        action: String,
        #[serde(default)]
        guidance: Option<String>,
        round_summary: String,
    }

    let raw_dec: RawModeration =
        serde_json::from_str(block).map_err(|e| format!("parse moderation decision: {}", e))?;

    let guidance = raw_dec.guidance;
    let action = match raw_dec.action.to_lowercase().as_str() {
        "continue" => ModerationAction::Continue,
        "conclude" => ModerationAction::Conclude,
        "redirect" => {
            let new_topic = guidance.clone().unwrap_or_default();
            if new_topic.is_empty() {
                return Err("redirect action requires non-empty guidance".to_string());
            }
            ModerationAction::Redirect(new_topic)
        }
        other => return Err(format!("unknown moderation action: {}", other)),
    };

    Ok(ModerationDecision {
        action,
        guidance,
        round_summary: raw_dec.round_summary,
    })
}

/// Strip ```json ... ``` code fences if present.
fn strip_code_fence(s: &str) -> String {
    let open = match s.find("```") {
        Some(i) => i,
        None => return s.to_string(),
    };
    let after_open = open + 3;
    let after_lang = s[after_open..]
        .find('\n')
        .map(|p| after_open + p + 1)
        .unwrap_or(after_open);
    if let Some(close_rel) = s[after_lang..].find("```") {
        return s[after_lang..after_lang + close_rel].trim().to_string();
    }
    s.to_string()
}

// ── P4.3: Discussion quality detection ─────────────────────

/// Quality issue detected in a discussion message.
#[derive(Debug, Clone, PartialEq)]
pub enum DiscussionQualityIssue {
    /// Message is too short (< 20 chars, excluding PASS).
    TooShort,
    /// Message is a near-duplicate of a previous message in the same round.
    NearDuplicate,
    /// Message is pure agreement without adding new content ("同意", "说的对").
    PureEcho,
    /// Message is off-topic (contains none of the topic keywords).
    OffTopic,
}

/// Check the quality of a discussion message. Returns the first issue
/// found, or `None` if the message passes all checks.
///
/// This is a lightweight heuristic check — not a full LLM-based audit.
/// It runs synchronously after each participant's message is saved.
pub fn check_discussion_quality(
    content: &str,
    topic: &str,
    round_messages: &[DiscussionMessage],
) -> Option<DiscussionQualityIssue> {
    let text = content.trim();

    // PASS is always acceptable.
    if is_pass(text) {
        return None;
    }

    // 1. Too short (excluding PASS, which is already handled above).
    let chars = text.chars().count();
    if chars < 20 {
        return Some(DiscussionQualityIssue::TooShort);
    }

    // 2. Pure echo — check against common agreement phrases.
    let echo_phrases = [
        "同意", "说的对", "没错", "确实", "有道理", "赞同", "对", "好的",
        "我同意", "说得对", "是的", "嗯", "没错", "我也觉得",
    ];
    let lower = text.to_lowercase();
    for phrase in &echo_phrases {
        if lower == *phrase || lower == format!("{}。", phrase) || lower == format!("{}！", phrase) {
            return Some(DiscussionQualityIssue::PureEcho);
        }
    }

    // 3. Near-duplicate — check if >80% similar to any message in the same round.
    for prev in round_messages {
        if prev.content.trim() == text {
            return Some(DiscussionQualityIssue::NearDuplicate);
        }
        // Simple word overlap check.
        let prev_words: std::collections::HashSet<&str> =
            prev.content.split_whitespace().collect();
        let curr_words: std::collections::HashSet<&str> =
            text.split_whitespace().collect();
        if !prev_words.is_empty() && !curr_words.is_empty() {
            let overlap = prev_words.intersection(&curr_words).count();
            let min_len = prev_words.len().min(curr_words.len());
            if min_len > 3 && overlap as f64 / min_len as f64 > 0.8 {
                return Some(DiscussionQualityIssue::NearDuplicate);
            }
        }
    }

    // 4. Off-topic — check if any topic keyword appears in the message.
    // Only check if the topic has meaningful keywords (>2 chars).
    let topic_keywords: Vec<&str> = topic
        .split_whitespace()
        .filter(|w| w.chars().count() > 2)
        .collect();
    if !topic_keywords.is_empty() {
        let has_keyword = topic_keywords.iter().any(|kw| text.contains(kw));
        if !has_keyword && chars < 200 {
            return Some(DiscussionQualityIssue::OffTopic);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_moderation_continue() {
        let raw = r#"{"action":"continue","guidance":null,"round_summary":"讨论了 OAuth"}"#;
        let dec = parse_moderation_decision(raw).unwrap();
        assert_eq!(dec.action, ModerationAction::Continue);
        assert!(dec.guidance.is_none());
        assert_eq!(dec.round_summary, "讨论了 OAuth");
    }

    #[test]
    fn parse_moderation_conclude() {
        let raw = r#"{"action":"conclude","guidance":null,"round_summary":"达成共识"}"#;
        let dec = parse_moderation_decision(raw).unwrap();
        assert_eq!(dec.action, ModerationAction::Conclude);
    }

    #[test]
    fn parse_moderation_redirect() {
        let raw = r#"{"action":"redirect","guidance":"讨论 2FA","round_summary":"跑题了"}"#;
        let dec = parse_moderation_decision(raw).unwrap();
        match dec.action {
            ModerationAction::Redirect(topic) => assert_eq!(topic, "讨论 2FA"),
            other => panic!("expected Redirect, got {:?}", other),
        }
    }

    #[test]
    fn parse_moderation_with_code_fence() {
        let raw = r#"```json
{"action":"continue","guidance":null,"round_summary":"ok"}
```"#;
        let dec = parse_moderation_decision(raw).unwrap();
        assert_eq!(dec.action, ModerationAction::Continue);
    }

    #[test]
    fn parse_moderation_with_prose() {
        let raw = r#"好的，我来决定。
{"action":"conclude","guidance":null,"round_summary":"结束了"}
希望对你有帮助。"#;
        let dec = parse_moderation_decision(raw).unwrap();
        assert_eq!(dec.action, ModerationAction::Conclude);
    }

    #[test]
    fn parse_moderation_invalid_action() {
        let raw = r#"{"action":"explode","guidance":null,"round_summary":"x"}"#;
        assert!(parse_moderation_decision(raw).is_err());
    }

    #[test]
    fn parse_moderation_redirect_empty_guidance() {
        let raw = r#"{"action":"redirect","guidance":"","round_summary":"x"}"#;
        assert!(parse_moderation_decision(raw).is_err());
    }

    #[test]
    fn parse_moderation_garbage() {
        assert!(parse_moderation_decision("not json at all").is_err());
    }

    #[test]
    fn discussion_config_default() {
        let config = DiscussionConfig::default();
        assert_eq!(config.max_rounds, 5);
        assert_eq!(config.context_window, 8);
        assert!(config.participants.is_empty());
    }

    #[test]
    fn strip_code_fence_no_fence() {
        let result = strip_code_fence("plain text");
        assert_eq!(result, "plain text");
    }

    #[test]
    fn strip_code_fence_with_fence() {
        let result = strip_code_fence("```json\n{\"key\": \"value\"}\n```");
        assert_eq!(result, r#"{"key": "value"}"#);
    }

    // ── P4.3: Discussion quality detection tests ──

    fn make_discussion_msg(agent_name: &str, content: &str, round: u8) -> DiscussionMessage {
        DiscussionMessage {
            id: format!("dmsg-{}", round),
            agent_id: format!("agent-{}", agent_name),
            agent_name: agent_name.to_string(),
            content: content.to_string(),
            round,
            timestamp: chrono::Utc::now(),
        }
    }

    #[test]
    fn quality_check_passes_good_message() {
        let result = check_discussion_quality(
            "我认为应该使用 OAuth 2.0，因为它提供了更好的安全性",
            "OAuth 安全性",
            &[],
        );
        assert!(result.is_none());
    }

    #[test]
    fn quality_check_detects_too_short() {
        let result = check_discussion_quality("短", "话题", &[]);
        assert_eq!(result, Some(DiscussionQualityIssue::TooShort));
    }

    #[test]
    fn quality_check_passes_pass_message() {
        let result = check_discussion_quality("PASS", "话题", &[]);
        assert!(result.is_none());
    }

    #[test]
    fn quality_check_detects_pure_echo() {
        let result = check_discussion_quality("同意", "OAuth", &[]);
        assert_eq!(result, Some(DiscussionQualityIssue::PureEcho));
    }

    #[test]
    fn quality_check_detects_near_duplicate() {
        let prev = make_discussion_msg(
            "Alice",
            "I think we should use OAuth for security reasons here",
            1,
        );
        let result = check_discussion_quality(
            "I think we should use OAuth for security reasons here",
            "OAuth",
            &[prev],
        );
        assert_eq!(result, Some(DiscussionQualityIssue::NearDuplicate));
    }

    #[test]
    fn quality_check_detects_off_topic() {
        let result = check_discussion_quality(
            "今天天气不错啊",
            "OAuth 安全性 认证",
            &[],
        );
        assert_eq!(result, Some(DiscussionQualityIssue::OffTopic));
    }
}
