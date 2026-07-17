//! Compression pipeline primitives — Phase A + Phase B (2026-07-17).
//!
//! Designed after reading the Headroom article
//! (<https://www.chenxutan.com/d/4104.html>) but adapted to a Rust
//! single-binary Windows agent without an embedding model.
//!
//! Items living here:
//! - `PerRolePolicy` + `policy_for()` — single source of truth for
//!   which compression stages apply to which role.
//! - `is_pinned()` — pin-keyword / pin-role filter.
//! - `dedup_adjacent_user()` — Pass 0 user-text dedup.
//! - `TokenScorer` + `score()` — 4-dim heuristic scorer
//!   (recency / role_position / frequency / instructional_marker).
//! - `drop_low_score()` — Phase B pre-summary drop.
//! - `CompressionReport` — structured log of one compression pass.
//!
//! **Design constraint:** we do NOT introduce an embedding model.
//! All scoring is over metadata already on `Message` (role,
//! `tool_call_id`, content patterns). This keeps the agent a
//! single Rust binary without 80-420 MB of ONNX / Python deps.

use hermix_core::{Message, MessageRole};

/// Action a single message should receive during compression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionAction {
    /// Never touch this message (pinned or system-role).
    Preserve,
    /// Drop this message entirely (low score, non-pinned).
    Drop,
    /// Replace with `[Duplicate …]` marker if an adjacent twin exists.
    Dedup,
    /// Pass through to the LLM summary stage.
    Summary,
}

/// Per-role compression policy. One variant per role, no extra
/// branches — keeps dispatch a single match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerRolePolicy {
    /// System prompt — never compressed, never dropped.
    Preserve,
    /// User text — adjacent-dedup → if remaining, summary.
    DedupThenSummary,
    /// Assistant text — straight to LLM summary.
    Summary,
    /// Tool result — adjacent-dedup → prune → if remaining, summary.
    DedupPruneThenSummary,
}

impl PerRolePolicy {
    pub fn policy_for(role: MessageRole) -> PerRolePolicy {
        match role {
            MessageRole::System => PerRolePolicy::Preserve,
            MessageRole::User => PerRolePolicy::DedupThenSummary,
            MessageRole::Assistant => PerRolePolicy::Summary,
            MessageRole::Tool => PerRolePolicy::DedupPruneThenSummary,
        }
    }
}

// ---------------------------------------------------------------------------
// Pin configuration
// ---------------------------------------------------------------------------

/// Pin settings (loaded from `CompressorConfig::pin_*`).
///
/// Pin mechanism is *opt-in*: with both fields empty (the default),
/// no message is ever pinned and the pipeline behaves identically to
/// before this module landed.
#[derive(Debug, Clone, Default)]
pub struct PinConfig {
    /// Role names that should never be touched by compression.
    /// Compared case-sensitively against `role_label(role)`.
    /// Recognized values: `"system"`, `"user"`, `"assistant"`, `"tool"`.
    pub pin_roles: Vec<String>,
    /// Case-insensitive substrings. If any substring matches anywhere
    /// in a message's content (or its tool-call args), the message is
    /// pinned. Useful for "DO NOT compress" / "安全规则" / "## Goals".
    pub pin_keywords: Vec<String>,
}

impl PinConfig {
    /// Build from `CompressorConfig` fields. Returns `None` if no
    /// pinning is configured — callers may then skip the pin
    /// partition entirely.
    pub fn from_compressor_config(cfg: &crate::compression::CompressorConfig) -> Option<Self> {
        if cfg.pin_roles.is_empty() && cfg.pin_keywords.is_empty() {
            return None;
        }
        // Keywords are stored as-is; `is_pinned` lowercases both
        // sides at compare time so direct construction (used in
        // tests) and `from_compressor_config` agree on case
        // insensitivity.
        Some(Self {
            pin_roles: cfg.pin_roles.clone(),
            pin_keywords: cfg.pin_keywords.clone(),
        })
    }

    /// Returns `true` if this message should be preserved verbatim
    /// regardless of any other stage.
    pub fn is_pinned(&self, msg: &Message) -> bool {
        if self.pin_roles.iter().any(|r| r == role_label(msg.role)) {
            return true;
        }
        if self.pin_keywords.is_empty() {
            return false;
        }
        let hay = msg_content_lowercase(msg);
        self.pin_keywords
            .iter()
            .any(|needle| !needle.is_empty() && hay.contains(&needle.to_lowercase()))
    }
}

fn role_label(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

fn msg_content_lowercase(msg: &Message) -> String {
    let mut buf = String::new();
    if let Some(c) = msg.content.as_deref() {
        buf.push_str(&c.to_lowercase());
    }
    if let Some(tcs) = msg.tool_calls.as_ref() {
        for tc in tcs {
            buf.push('\n');
            buf.push_str(&tc.function.arguments.to_lowercase());
        }
    }
    buf
}

// ---------------------------------------------------------------------------
// Pass 0: adjacent user-text dedup
// ---------------------------------------------------------------------------

/// Adjacent user-text deduplication (Pass 0).
///
/// Mirrors the existing tool-text dedup in `prune_old_tool_results`:
/// when two consecutive `User` messages have the same trimmed content,
/// the older one is replaced with a `[Duplicate user message]`
/// marker and counted.
///
/// Returns `(messages, dedup_count)`.
pub fn dedup_adjacent_user(messages: Vec<Message>) -> (Vec<Message>, usize) {
    if messages.is_empty() {
        return (messages, 0);
    }
    let mut out: Vec<Message> = Vec::with_capacity(messages.len());
    let mut deduped = 0usize;
    let mut prev_user_hash: Option<String> = None;
    for mut msg in messages.into_iter() {
        if msg.role == MessageRole::User && !is_already_a_dedup_marker(&msg) {
            let content = msg.content.as_deref().unwrap_or("").trim();
            if content.is_empty() {
                out.push(msg);
                prev_user_hash = None;
                continue;
            }
            let hash = format!("{:016x}", md5::compute(content.as_bytes()));
            if prev_user_hash.as_deref() == Some(&hash) {
                msg.content = Some(
                    "[ALREADY EXECUTED] [Duplicate user message — same content as a more recent turn]"
                        .into(),
                );
                deduped += 1;
                out.push(msg);
                // Do NOT update prev_user_hash: a third identical
                // message should also be deduped against the original,
                // not against the marker we just inserted.
                continue;
            }
            prev_user_hash = Some(hash);
        } else {
            prev_user_hash = None;
        }
        out.push(msg);
    }
    (out, deduped)
}

fn is_already_a_dedup_marker(msg: &Message) -> bool {
    msg.content
        .as_deref()
        .map(|c| c.starts_with("[ALREADY EXECUTED]"))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// TokenScorer — 4-dim heuristic
// ---------------------------------------------------------------------------

/// 4-dim heuristic scorer. **Not** an embedding model.
///
/// Dimensions:
/// - `recency(0.35)` — position-based; later messages score higher.
///   We have no per-message timestamp (`Message::execution_timeline`
///   carries *events* during a turn, not the turn's own wall clock),
///   so recency is approximated by message index.
/// - `role_position(0.30)` — `system=1.0`, `tool=0.7`,
///   `user=0.5`, `assistant=0.4`. Reflects that system messages
///   carry rules the LLM must keep following.
/// - `frequency(0.15)` — same `tool_call_id` referenced by ≥ 1
///   other message → bump. Catches "important tool result that
///   downstream turns keep referring to".
/// - `instructional_marker(0.20)` — content matches imperative
///   patterns (`!`-commands, `run …`, `git …`, file paths ending
///   in action verbs, `请`, `不要`, `must`, `never`). Hits a
///   1.0 if the message looks like an instruction to the agent.
#[derive(Debug, Clone)]
pub struct TokenScorer;

impl TokenScorer {
    pub fn new() -> Self {
        Self
    }

    /// Score a single message in the context of the full message
    /// list. Score is in `[0.0, 1.0]`.
    pub fn score(&self, msg: &Message, all: &[Message], idx: usize) -> f64 {
        let n = all.len().max(1);
        // recency: linear ramp from 0.0 (oldest) to 1.0 (newest).
        let recency = idx as f64 / (n.saturating_sub(1).max(1)) as f64;
        // role_position: per-role base score.
        let role_pos = match msg.role {
            MessageRole::System => 1.0,
            MessageRole::Tool => 0.7,
            MessageRole::User => 0.5,
            MessageRole::Assistant => 0.4,
        };
        // frequency: same tool_call_id used N times → bump up to 0.5.
        let frequency = if let Some(tcid) = msg.tool_call_id.as_deref() {
            let refs = all
                .iter()
                .filter(|m| m.tool_call_id.as_deref() == Some(tcid))
                .count();
            if refs >= 2 {
                ((refs - 1) as f64).min(4.0) / 4.0
            } else {
                0.0
            }
        } else {
            0.0
        };
        // instructional_marker: cheap pattern match.
        let instructional = if msg.role == MessageRole::System {
            // System messages are always considered "instructional".
            1.0
        } else {
            let c = msg.content.as_deref().unwrap_or("");
            looks_like_instruction(c) as i32 as f64
        };

        let score = 0.35 * recency + 0.30 * role_pos + 0.15 * frequency + 0.20 * instructional;
        score.clamp(0.0, 1.0)
    }
}

fn looks_like_instruction(content: &str) -> bool {
    let lower = content.to_lowercase();
    let patterns = [
        "!",
        "/run ",
        "/cmd ",
        "please ",
        "must ",
        "never ",
        "always ",
        "do not ",
        "don't ",
        "禁止",
        "请",
        "不要",
        "必须",
        "执行",
    ];
    // Require a content length floor so trivial short messages
    // ("ok", "yes") don't count as instructions.
    if content.len() < 8 {
        return false;
    }
    patterns.iter().any(|p| lower.contains(p))
}

/// Drop messages with score strictly below `threshold`. Pinned
/// messages are never dropped regardless of score.
///
/// Returns `(kept, dropped_count)`. Drops are silent in terms of
/// content (no marker inserted) — the breaker / CompressionReport
/// counts them so the dashboard can surface them.
pub fn drop_low_score(
    messages: Vec<Message>,
    scorer: &TokenScorer,
    pin: Option<&PinConfig>,
    threshold: f64,
) -> (Vec<Message>, usize) {
    if messages.is_empty() {
        return (messages, 0);
    }
    // Capture an immutable snapshot of the message list so we can
    // score each message against the full context (recency,
    // frequency) while still moving each message out by ownership.
    let all = messages.clone();
    let mut out = Vec::with_capacity(messages.len());
    let mut dropped = 0usize;
    for (idx, msg) in messages.into_iter().enumerate() {
        let is_pin = pin.map(|p| p.is_pinned(&msg)).unwrap_or(false);
        let score = scorer.score(&msg, &all, idx);
        if !is_pin && score < threshold {
            dropped += 1;
            continue;
        }
        out.push(msg);
    }
    (out, dropped)
}

// ---------------------------------------------------------------------------
// CompressionReport
// ---------------------------------------------------------------------------

/// Structured summary of one compression pass. Emitted as a single
/// `tracing::info!` line and persisted into the compression metrics
/// slot so the dashboard can show "what happened this turn".
#[derive(Debug, Clone, Default)]
pub struct CompressionReport {
    /// Token count before any compression.
    pub pre_tokens: u64,
    /// Token count after all passes.
    pub post_tokens: u64,
    /// Message count before.
    pub pre_count: usize,
    /// Message count after.
    pub post_count: usize,
    /// Number of messages pinned (skipped every stage).
    pub pin_count: usize,
    /// Number of messages deduped (Pass 0 user + Pass 1 tool).
    pub dedup_count: usize,
    /// Number of tool results pruned to 1-line summary.
    pub pruned_count: usize,
    /// Number of messages dropped by score threshold.
    pub dropped_count: usize,
    /// Final summary token count (if a summary was generated).
    pub summary_tokens: Option<u64>,
    /// What triggered compression: "threshold" / "explicit" / etc.
    pub trigger_reason: String,
    /// Breakdown of pre-count by role for quick inspection.
    pub per_role_pre: std::collections::BTreeMap<String, usize>,
    /// Breakdown of post-count by role.
    pub per_role_post: std::collections::BTreeMap<String, usize>,
}

impl CompressionReport {
    pub fn compute_per_role(messages: &[Message]) -> std::collections::BTreeMap<String, usize> {
        let mut m = std::collections::BTreeMap::new();
        for msg in messages {
            *m.entry(role_label(msg.role).to_string()).or_insert(0) += 1;
        }
        m
    }

    /// Render as a single-line tracing message.
    pub fn format_line(&self) -> String {
        format!(
            "compression report: pre={}msg/{}tok post={}msg/{}tok pin={} dedup={} pruned={} dropped={} summary_tokens={:?} trigger={} per_role_pre={:?} per_role_post={:?}",
            self.pre_count,
            self.pre_tokens,
            self.post_count,
            self.post_tokens,
            self.pin_count,
            self.dedup_count,
            self.pruned_count,
            self.dropped_count,
            self.summary_tokens,
            self.trigger_reason,
            self.per_role_pre,
            self.per_role_post,
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_core::{FunctionCall, ToolCall};

    fn m_user(c: &str) -> Message {
        Message {
            role: MessageRole::User,
            content: Some(c.into()),
            content_parts: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
            execution_timeline: None,
            ephemeral: false,
        }
    }

    fn m_assistant(c: &str) -> Message {
        Message {
            role: MessageRole::Assistant,
            content: Some(c.into()),
            content_parts: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
            execution_timeline: None,
            ephemeral: false,
        }
    }

    fn m_tool(call_id: &str, c: &str) -> Message {
        Message {
            role: MessageRole::Tool,
            content: Some(c.into()),
            content_parts: None,
            tool_calls: None,
            tool_call_id: Some(call_id.into()),
            name: None,
            reasoning_content: None,
            cache_control: None,
            execution_timeline: None,
            ephemeral: false,
        }
    }

    fn m_system(c: &str) -> Message {
        Message {
            role: MessageRole::System,
            content: Some(c.into()),
            content_parts: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
            execution_timeline: None,
            ephemeral: false,
        }
    }

    // ---------- PerRolePolicy ----------

    #[test]
    fn per_role_policy_is_static() {
        assert_eq!(
            PerRolePolicy::policy_for(MessageRole::System),
            PerRolePolicy::Preserve
        );
        assert_eq!(
            PerRolePolicy::policy_for(MessageRole::User),
            PerRolePolicy::DedupThenSummary
        );
        assert_eq!(
            PerRolePolicy::policy_for(MessageRole::Assistant),
            PerRolePolicy::Summary
        );
        assert_eq!(
            PerRolePolicy::policy_for(MessageRole::Tool),
            PerRolePolicy::DedupPruneThenSummary
        );
    }

    // ---------- dedup_adjacent_user ----------

    #[test]
    fn dedup_adjacent_user_merges_three_identical() {
        let msgs = vec![
            m_user("hello world"),
            m_user("hello world"),
            m_user("hello world"),
            m_user("different message"),
        ];
        let (out, count) = dedup_adjacent_user(msgs);
        assert_eq!(count, 2, "expected 2 dups out of 3 identical");
        assert_eq!(out.len(), 4);
        // First kept, second + third replaced with marker.
        assert_eq!(out[0].content.as_deref(), Some("hello world"));
        assert!(out[1]
            .content
            .as_deref()
            .unwrap()
            .starts_with("[ALREADY EXECUTED]"));
        assert!(out[2]
            .content
            .as_deref()
            .unwrap()
            .starts_with("[ALREADY EXECUTED]"));
        // Different message is untouched.
        assert_eq!(out[3].content.as_deref(), Some("different message"));
    }

    #[test]
    fn dedup_adjacent_user_breaks_on_role_change() {
        // user -> assistant -> user should NOT collapse the two users.
        let msgs = vec![
            m_user("same"),
            m_assistant("response"),
            m_user("same"),
        ];
        let (out, count) = dedup_adjacent_user(msgs);
        assert_eq!(count, 0, "non-adjacent users must not dedup");
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].content.as_deref(), Some("same"));
        assert_eq!(out[2].content.as_deref(), Some("same"));
    }

    #[test]
    fn dedup_adjacent_user_trims_whitespace() {
        let msgs = vec![m_user("hello"), m_user("  hello  "), m_user("hello")];
        let (_out, count) = dedup_adjacent_user(msgs);
        assert_eq!(count, 2);
    }

    // ---------- PinConfig ----------

    #[test]
    fn pin_by_role() {
        let cfg = PinConfig {
            pin_roles: vec!["system".into()],
            pin_keywords: vec![],
        };
        assert!(cfg.is_pinned(&m_system("anything")));
        assert!(!cfg.is_pinned(&m_user("anything")));
    }

    #[test]
    fn pin_by_keyword_case_insensitive() {
        let cfg = PinConfig {
            pin_roles: vec![],
            pin_keywords: vec!["DO NOT compress".into()],
        };
        assert!(cfg.is_pinned(&m_user("This rule: DO NOT COMPRESS the safety checks.")));
        assert!(!cfg.is_pinned(&m_user("This rule: do compress when needed.")));
    }

    #[test]
    fn pin_keyword_matches_tool_call_args() {
        let mut msg = m_assistant("calling tool");
        msg.tool_calls = Some(vec![ToolCall {
            id: "c1".into(),
            function: FunctionCall {
                name: "shell".into(),
                arguments: r#"{"cmd":"DO NOT COMPRESS critical script"}"#.into(),
            },
        }]);
        let cfg = PinConfig {
            pin_roles: vec![],
            pin_keywords: vec!["do not compress".into()],
        };
        assert!(cfg.is_pinned(&msg));
    }

    #[test]
    fn pin_empty_matches_nothing() {
        let cfg = PinConfig::default();
        assert!(!cfg.is_pinned(&m_system("important")));
        assert!(!cfg.is_pinned(&m_user("important")));
    }

    // ---------- TokenScorer ----------

    #[test]
    fn scorer_recency_is_position_based() {
        let s = TokenScorer::new();
        let msgs = vec![m_user("old"), m_user("middle"), m_user("newest")];
        let score_old = s.score(&msgs[0], &msgs, 0);
        let score_new = s.score(&msgs[2], &msgs, 2);
        assert!(score_new > score_old, "newer messages must score higher");
    }

    #[test]
    fn scorer_role_position_system_highest() {
        let s = TokenScorer::new();
        // Same index isolates role_position from recency.
        let sys = m_system("rules");
        let tool = m_tool("c1", "result");
        let user = m_user("hi");
        let asst = m_assistant("ok");
        let all = vec![sys.clone(), tool.clone(), user.clone(), asst.clone()];
        let sys_score = s.score(&sys, &all, 0);
        let tool_score = s.score(&tool, &all, 0);
        let user_score = s.score(&user, &all, 0);
        let asst_score = s.score(&asst, &all, 0);
        assert!(sys_score > tool_score, "sys={sys_score} tool={tool_score}");
        assert!(tool_score > user_score, "tool={tool_score} user={user_score}");
        assert!(user_score > asst_score, "user={user_score} asst={asst_score}");
    }

    #[test]
    fn scorer_frequency_bumps_reused_tool_call_id() {
        let s = TokenScorer::new();
        // Same index to isolate frequency from recency. c1 is
        // reused (appears at idx 0 + idx 1) so the score at idx 1
        // gets a frequency bonus; c2 is unique.
        let a_first = m_tool("c1", "first call");
        let c = m_tool("c2", "lonely");
        let a_second = m_tool("c1", "second call");
        let all = vec![a_first.clone(), c.clone(), a_second.clone()];
        let reuse_score = s.score(&a_second, &all, 2);
        let lonely_score = s.score(&c, &all, 1);
        assert!(
            reuse_score > lonely_score,
            "reuse={reuse_score} vs lonely={lonely_score}"
        );
    }

    #[test]
    fn scorer_instructional_marker_detects_imperative() {
        let s = TokenScorer::new();
        // Compare at SAME index so recency is constant and we
        // isolate the instructional_marker dimension.
        let cmd = m_user("Please run the tests now");
        let filler = m_user("ok sounds good");
        let all = vec![cmd.clone(), filler.clone()];
        let cmd_score = s.score(&cmd, &all, 1);
        let filler_score = s.score(&filler, &all, 1);
        assert!(cmd_score > filler_score, "{cmd_score} vs {filler_score}");
    }

    // ---------- drop_low_score ----------

    #[test]
    fn drop_low_score_respects_pin() {
        let s = TokenScorer::new();
        let pin = PinConfig {
            pin_roles: vec!["system".into()],
            pin_keywords: vec![],
        };
        // System at index 0 → very low recency, but role_position
        // + instructional bumps it well above any threshold. The
        // important thing here is "never dropped" — verify by
        // asking for a threshold of 0.99.
        let sys = m_system("rules that must survive");
        let filler = m_assistant("ok");
        let msgs = vec![sys.clone(), filler.clone()];
        let (out, dropped) = drop_low_score(msgs, &s, Some(&pin), 0.99);
        assert!(out.iter().any(|m| m.role == MessageRole::System));
        // Filler at index 1 has recency=1.0 but role_position=0.4
        // and instructional=0 → score = 0.35*1.0 + 0.30*0.4 = 0.47
        // < 0.99, so it should be dropped.
        assert_eq!(dropped, 1, "low-score assistant dropped");
    }

    #[test]
    fn drop_low_score_no_pin_drops_chaff() {
        let s = TokenScorer::new();
        let filler = m_assistant("ok");
        let msgs = vec![filler];
        let (out, dropped) = drop_low_score(msgs, &s, None, 0.5);
        assert!(out.is_empty());
        assert_eq!(dropped, 1);
    }

    // ---------- CompressionReport ----------

    #[test]
    fn report_format_line_has_all_fields() {
        let r = CompressionReport {
            pre_tokens: 1000,
            post_tokens: 400,
            pre_count: 10,
            post_count: 4,
            pin_count: 2,
            dedup_count: 1,
            pruned_count: 1,
            dropped_count: 2,
            summary_tokens: Some(200),
            trigger_reason: "threshold".into(),
            per_role_pre: [("user".into(), 5), ("assistant".into(), 5)]
                .into_iter()
                .collect(),
            per_role_post: [("user".into(), 2), ("assistant".into(), 2)]
                .into_iter()
                .collect(),
        };
        let line = r.format_line();
        assert!(line.contains("pre=10msg/1000tok"));
        assert!(line.contains("post=4msg/400tok"));
        assert!(line.contains("pin=2"));
        assert!(line.contains("dedup=1"));
        assert!(line.contains("pruned=1"));
        assert!(line.contains("dropped=2"));
        assert!(line.contains("trigger=threshold"));
    }

    #[test]
    fn report_compute_per_role_counts_correctly() {
        let msgs = vec![m_user("a"), m_user("b"), m_assistant("c"), m_tool("c1", "d")];
        let r = CompressionReport::compute_per_role(&msgs);
        assert_eq!(r.get("user").copied(), Some(2));
        assert_eq!(r.get("assistant").copied(), Some(1));
        assert_eq!(r.get("tool").copied(), Some(1));
        assert!(r.get("system").is_none());
    }
}