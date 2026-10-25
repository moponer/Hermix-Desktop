//! — Two-split acceptance gate.
//!
//! When the agent loop produces output (visible text + tool results), this
//! module provides a **two-split acceptance gate** that must pass before
//! the output is committed to the conversation history.
//!
//! ## Two splits
//!
//! 1. **Behavioral split**: Does the output behave as expected?
//!    - No harmful content (PII, injections)
//!    - Follows tool usage conventions
//!    - Responds to the actual user query (not hallucinated)
//!
//! 2. **Structural split**: Is the output structurally sound?
//!    - No truncated JSON in tool calls
//!    - No orphaned thinking blocks
//!    - No duplicate/redundant content
//!    - Markdown is well-formed
//!
//! ## Gate outcomes
//!
//! ```text
//! Pass     → output is accepted as-is
//! Warn     → output accepted with advisory (injected into next turn context)
//! Reject   → output is rejected; the loop retries or falls back
//! ```
//!
//! ## Integration
//!
//! The gate is called after LLM response collection but before the
//! response is committed. Rejections trigger a retry with the gate
//! feedback injected into the system prompt. Two consecutive rejections
//! cause a hard stop to avoid infinite retry loops.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Gate verdict
// ---------------------------------------------------------------------------

/// Result of the acceptance gate evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GateVerdict {
    /// Output passes both splits — accept as-is.
    Pass,
    /// Output has minor issues — accept but inject advisory.
    Warn {
        /// Which split produced the warning.
        split: GateSplit,
        /// Human-readable description of the issue.
        reason: String,
    },
    /// Output fails a split — reject and retry.
    Reject {
        /// Which split produced the rejection.
        split: GateSplit,
        /// Human-readable description of the issue.
        reason: String,
        /// Suggested correction for the retry.
        suggestion: String,
    },
}

/// Which split produced the verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GateSplit {
    Behavioral,
    Structural,
}

// ---------------------------------------------------------------------------
// Gate report
// ---------------------------------------------------------------------------

/// Full report from the acceptance gate, including both splits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateReport {
    /// Behavioral split verdict.
    pub behavioral: GateVerdict,
    /// Structural split verdict.
    pub structural: GateVerdict,
    /// Overall verdict: the worse of the two splits.
    pub overall: GateVerdict,
    /// Number of consecutive rejections (for retry budget).
    pub consecutive_rejections: u32,
}

impl GateReport {
    /// Whether the overall verdict allows the output to proceed.
    pub fn is_accepted(&self) -> bool {
        !matches!(self.overall, GateVerdict::Reject { .. })
    }

    /// Collect all warnings (from both splits) into a single advisory string.
    pub fn advisory_text(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let GateVerdict::Warn { reason, .. } = &self.behavioral {
            parts.push(format!("[Behavioral] {}", reason));
        }
        if let GateVerdict::Warn { reason, .. } = &self.structural {
            parts.push(format!("[Structural] {}", reason));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n"))
        }
    }
}

// ---------------------------------------------------------------------------
// Gate configuration
// ---------------------------------------------------------------------------

/// Configuration for the acceptance gate.
#[derive(Debug, Clone)]
pub struct GateConfig {
    /// Maximum consecutive rejections before hard stop.
    pub max_rejections: u32,
    /// Whether to reject outputs with unclosed thinking blocks.
    pub reject_unclosed_thinking: bool,
    /// Whether to warn on outputs with duplicate paragraphs.
    pub warn_on_duplicates: bool,
    /// Whether to reject outputs with truncated JSON in tool calls.
    pub reject_truncated_json: bool,
    /// Whether to warn on PII-like patterns.
    pub warn_on_pii_patterns: bool,
    /// Whether to reject an empty reply to a user query.
    pub reject_empty_response: bool,
    /// Whether to warn on a reply shorter than 20 characters.
    pub warn_on_short_response: bool,
    /// Maximum duplicate paragraph ratio before warning (0.0-1.0).
    pub duplicate_ratio_threshold: f64,
    /// Whether to warn when the model asserts a completed action that
    /// no tool call backs. See [`TurnEvidence`] for the three signals
    /// this needs.
    pub check_unbacked_completion_claim: bool,
}

impl Default for GateConfig {
    /// **Production profile.** Every check that another layer already
    /// owns is off, so the gate cannot duplicate an existing decision
    /// or spend an extra API call re-deciding it:
    ///
    /// | check | off because |
    /// |---|---|
    /// | `reject_unclosed_thinking` | `ThinkBlockParser::flush` already routes an unclosed `<think>` into reasoning, which empties the visible text and lands in `empty_response_handler`. The generic XML scan also false-positives on prose containing `<x>`. |
    /// | `reject_truncated_json` | `tool_call_validator` rejects unparseable tool arguments before they reach the gate. |
    /// | `warn_on_pii_patterns` | The email/SSN heuristics fire on legitimate output — this agent routinely writes email addresses and `@`-bearing paths. |
    /// | `reject_empty_response` | `empty_response_handler::decide_empty_response` owns the empty-reply case (Codex ack continuation, autonomous nudge, empty-stop retry). Two owners for one decision means the gate could reject a reply the loop was about to handle. |
    /// | `warn_on_short_response` | A short reply is often the *right* reply — the report discipline explicitly allows "极简单行回复直接说人话". Flagging every "好的" would fire on most short exchanges and bury the real advisory. |
    ///
    /// What stays on is the check nothing else owns: the model
    /// asserting an action it never performed.
    ///
    /// **The production profile has no `Reject` check enabled** — the
    /// gate is advisory there. It never drops a reply, never rewrites
    /// one, and never spends an extra API call re-deciding something
    /// another layer already decided.
    fn default() -> Self {
        Self {
            max_rejections: 2,
            reject_unclosed_thinking: false,
            warn_on_duplicates: true,
            reject_truncated_json: false,
            warn_on_pii_patterns: false,
            reject_empty_response: false,
            warn_on_short_response: false,
            duplicate_ratio_threshold: 0.5,
            check_unbacked_completion_claim: true,
        }
    }
}

impl GateConfig {
    /// The strict profile: every check armed, including the ones the
    /// production profile leaves off because another layer owns them.
    ///
    /// Kept as a reference implementation and for the unit tests that
    /// exercise the individual checks. **Do not wire this into the
    /// agent loop** — it would re-decide decisions
    /// `tool_call_validator` and `empty_response_handler` already made,
    /// and its unclosed-tag scan false-positives on ordinary prose.
    pub fn strict() -> Self {
        Self {
            max_rejections: 2,
            reject_unclosed_thinking: true,
            warn_on_duplicates: true,
            reject_truncated_json: true,
            warn_on_pii_patterns: true,
            reject_empty_response: true,
            warn_on_short_response: true,
            duplicate_ratio_threshold: 0.5,
            check_unbacked_completion_claim: false,
        }
    }
}

/// Evidence about the turn that the gate cannot see from the reply
/// text alone.
///
/// The "unbacked completion claim" check needs three independent
/// signals before it fires. Requiring all three is what keeps its
/// false-positive rate near zero: any one of them alone is common.
#[derive(Debug, Clone, Copy, Default)]
pub struct TurnEvidence<'a> {
    /// Did any tool actually run — a tool result in the session
    /// history, or a tool call in this turn? Without it the model
    /// cannot have performed anything, whatever it says.
    pub has_tool_evidence: bool,
    /// The user's message asked for a mutating action (delete,
    /// create, send, …). If the user only asked a question, a
    /// completion-sounding sentence is not a claim about the agent's
    /// own work.
    pub user_requested_action: bool,
    /// The user's message text (kept for the advisory wording).
    pub user_message: &'a str,
}

// ---------------------------------------------------------------------------
// Gate state (tracks rejections across turns)
// ---------------------------------------------------------------------------

/// The prefix of the advisory message the gate injects into the
/// context.
///
/// Exposed so the wiring site and the tests recognise an advisory by a
/// single marker instead of matching on prose.
pub const ADVISORY_MESSAGE_PREFIX: &str = "[验收提醒] ";

/// Hard cap on how many advisories one session may carry.
///
/// The advisory is a *state* correction, not a stream of feedback: once
/// the model has been told that its claim is unbacked, repeating the
/// same words adds nothing. The cap is the backstop for the case where
/// the text differs slightly each time (the advisory quotes the user's
/// message, so a session with several user messages could otherwise
/// accumulate one advisory per message).
pub const MAX_ADVISORIES_PER_SESSION: usize = 3;

/// Is this message one of the gate's advisories?
pub fn is_advisory_message(text: &str) -> bool {
    text.starts_with(ADVISORY_MESSAGE_PREFIX)
}

/// Mutable state for the acceptance gate, held across turns.
#[derive(Debug, Default)]
pub struct AcceptanceGate {
    /// Number of consecutive rejections.
    consecutive_rejections: u32,
    /// Configuration.
    config: GateConfig,
    /// Advisory waiting for a place in the context.
    ///
    /// It is **queued**, not injected on the spot: the gate runs inside
    /// `process_post_response`, which happens *before* the assistant
    /// reply is committed to the context (`prepare_assistant_turn` is
    /// what pushes it). Injecting there put the reminder — "your
    /// previous reply claimed X" — *in front of* the very reply it is
    /// talking about. The loop flushes it right after the assistant
    /// message lands.
    pending_advisory: Option<String>,
}

impl AcceptanceGate {
    /// Create a new gate with default configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new gate with custom configuration.
    pub fn with_config(config: GateConfig) -> Self {
        Self {
            consecutive_rejections: 0,
            config,
            pending_advisory: None,
        }
    }

    /// Reset the rejection counter (call on successful acceptance).
    pub fn reset_rejections(&mut self) {
        self.consecutive_rejections = 0;
    }

    /// Queue an advisory for the loop to inject after the assistant
    /// reply has been committed to the context.
    pub fn queue_advisory(&mut self, text: String) {
        self.pending_advisory = Some(text);
    }

    /// Take the queued advisory, if any. `None` means there is nothing
    /// to inject (the usual case).
    pub fn take_pending_advisory(&mut self) -> Option<String> {
        self.pending_advisory.take()
    }

    /// Get the current number of consecutive rejections.
    pub fn consecutive_rejections(&self) -> u32 {
        self.consecutive_rejections
    }

    /// Whether the gate has hit the rejection limit.
    pub fn is_exhausted(&self) -> bool {
        self.consecutive_rejections >= self.config.max_rejections
    }

    // -----------------------------------------------------------------------
    // Main evaluation
    // -----------------------------------------------------------------------

    /// Evaluate an LLM output through both splits.
    /// - `visible_text`: The visible (non-thinking) text produced by the LLM.
    /// - `tool_calls_json`: Raw JSON strings of any tool calls made.
    /// - `thinking_blocks`: The content of thinking blocks (for structural check).
    /// - `has_user_query`: Whether there was a user query in this turn.
    ///
    /// Equivalent to [`Self::evaluate_turn`] with empty evidence — i.e.
    /// "no tool ran, the user asked for nothing". Use
    /// [`Self::evaluate_turn`] in production so the claim check sees
    /// what actually happened.
    pub fn evaluate(
        &mut self,
        visible_text: &str,
        tool_calls_json: &[String],
        thinking_blocks: &[String],
        has_user_query: bool,
    ) -> GateReport {
        self.evaluate_turn(
            visible_text,
            tool_calls_json,
            thinking_blocks,
            has_user_query,
            &TurnEvidence::default(),
        )
    }

    /// Evaluate an LLM output through both splits, with turn-level
    /// evidence for the claim check.
    pub fn evaluate_turn(
        &mut self,
        visible_text: &str,
        tool_calls_json: &[String],
        thinking_blocks: &[String],
        has_user_query: bool,
        evidence: &TurnEvidence<'_>,
    ) -> GateReport {
        let behavioral = self.evaluate_behavioral(visible_text, has_user_query, evidence);
        let structural = self.evaluate_structural(visible_text, tool_calls_json, thinking_blocks);

        let overall = Self::worse_verdict(&behavioral, &structural);

        // Track consecutive rejections.
        match &overall {
            GateVerdict::Reject { .. } => {
                self.consecutive_rejections = self.consecutive_rejections.saturating_add(1);
            }
            _ => {
                self.consecutive_rejections = 0;
            }
        }

        GateReport {
            behavioral,
            structural,
            overall,
            consecutive_rejections: self.consecutive_rejections,
        }
    }

    // -----------------------------------------------------------------------
    // Behavioral split
    // -----------------------------------------------------------------------

    fn evaluate_behavioral(
        &self,
        text: &str,
        has_user_query: bool,
        evidence: &TurnEvidence<'_>,
    ) -> GateVerdict {
        // 1. PII pattern check (warning only).
        if self.config.warn_on_pii_patterns && contains_pii_patterns(text) {
            return GateVerdict::Warn {
                split: GateSplit::Behavioral,
                reason: "Output may contain PII-like patterns (email, phone, or SSN)".into(),
            };
        }

        // 2. Empty response when user asked a question.
        if self.config.reject_empty_response && has_user_query && text.trim().is_empty() {
            return GateVerdict::Reject {
                split: GateSplit::Behavioral,
                reason: "Empty response to a user query".into(),
                suggestion: "Provide a substantive response to the user's question".into(),
            };
        }

        // 3. Very short response that seems like an acknowledgment but
        // should contain more substance (heuristic: < 20 chars).
        if self.config.warn_on_short_response
            && has_user_query
            && text.trim().len() < 20
            && !text.trim().is_empty()
        {
            return GateVerdict::Warn {
                split: GateSplit::Behavioral,
                reason: "Very short response to a user query — may not be substantive enough".into(),
            };
        }

        // 4. Completion claim with nothing behind it.
        //
        // Three independent signals must all hold (see [`TurnEvidence`]):
        // the user asked for a mutating action, the reply asserts that
        // action was performed, and no tool ever ran. Any one alone is
        // ordinary; together they mean the model narrated an outcome it
        // could not have produced.
        //
        // Advisory only — the reply is not rewritten and no extra API
        // call is spent. The advisory goes into the next turn's context
        // so the model can correct itself.
        if self.config.check_unbacked_completion_claim
            && !evidence.has_tool_evidence
            && evidence.user_requested_action
            && claims_completed_action(text)
        {
            return GateVerdict::Warn {
                split: GateSplit::Behavioral,
                // Chinese, to match the rest of the system prompt — this
                // string is injected verbatim into the model's context.
                reason: format!(
                    "回复声称已完成某个动作，但本次会话**没有任何工具执行过**，该声明没有工具结果支撑（用户原话：{}）",
                    truncate_for_advisory(evidence.user_message)
                ),
            };
        }

        GateVerdict::Pass
    }

    // -----------------------------------------------------------------------
    // Structural split
    // -----------------------------------------------------------------------

    fn evaluate_structural(
        &self,
        text: &str,
        tool_calls_json: &[String],
        thinking_blocks: &[String],
    ) -> GateVerdict {
        // 1. Unclosed thinking blocks.
        if self.config.reject_unclosed_thinking {
            for block in thinking_blocks {
                if has_unclosed_tags(block) {
                    return GateVerdict::Reject {
                        split: GateSplit::Structural,
                        reason: "Unclosed tags in thinking block".into(),
                        suggestion: "Ensure all XML/HTML-style tags are properly closed".into(),
                    };
                }
            }
        }

        // 2. Truncated JSON in tool calls.
        if self.config.reject_truncated_json {
            for json in tool_calls_json {
                if is_truncated_json(json) {
                    return GateVerdict::Reject {
                        split: GateSplit::Structural,
                        reason: "Truncated JSON in tool call".into(),
                        suggestion: "Complete the JSON before making the tool call".into(),
                    };
                }
            }
        }

        // 3. Duplicate paragraph detection.
        if self.config.warn_on_duplicates && text.len() > 100 {
            let ratio = duplicate_paragraph_ratio(text);
            if ratio > self.config.duplicate_ratio_threshold {
                return GateVerdict::Warn {
                    split: GateSplit::Structural,
                    reason: format!(
                        "High duplicate paragraph ratio ({:.0}%) — output may be redundant",
                        ratio * 100.0
                    ),
                };
            }
        }

        GateVerdict::Pass
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Return the worse of two verdicts (Reject > Warn > Pass).
    fn worse_verdict(a: &GateVerdict, b: &GateVerdict) -> GateVerdict {
        fn severity(v: &GateVerdict) -> u8 {
            match v {
                GateVerdict::Pass => 0,
                GateVerdict::Warn { .. } => 1,
                GateVerdict::Reject { .. } => 2,
            }
        }
        if severity(a) >= severity(b) {
            a.clone()
        } else {
            b.clone()
        }
    }
}

// ---------------------------------------------------------------------------
// Detection helpers
// ---------------------------------------------------------------------------

/// Check for PII-like patterns (email, phone, SSN-like sequences).
fn contains_pii_patterns(text: &str) -> bool {
    // Email pattern: simple heuristic.
    if let Some(at_pos) = text.find('@') {
        let before = &text[..at_pos];
        let after = &text[at_pos + 1..];
        if before.len() >= 2 && after.contains('.') {
            return true;
        }
    }

    // SSN-like: XXX-XX-XXXX.
    let bytes = text.as_bytes();
    if bytes.len() >= 11 {
        for window in bytes.windows(11) {
            if window[3] == b'-' && window[6] == b'-' {
                let is_digit = |b: u8| b.is_ascii_digit();
                if is_digit(window[0]) && is_digit(window[1]) && is_digit(window[2])
                    && is_digit(window[4]) && is_digit(window[5])
                    && is_digit(window[7]) && is_digit(window[8]) && is_digit(window[9])
                    && is_digit(window[10])
                {
                    return true;
                }
            }
        }
    }

    false
}

/// Check for unclosed XML-style tags in text.
fn has_unclosed_tags(text: &str) -> bool {
    let mut open_tags: Vec<String> = Vec::new();
    let mut i = 0;
    let bytes = text.as_bytes();

    while i < bytes.len() {
        if bytes[i] == b'<' {
            // Find the end of the tag.
            if let Some(end) = text[i..].find('>') {
                let tag_content = &text[i + 1..i + end];
                let tag_content = tag_content.trim();

                if tag_content.starts_with('/') {
                    // Closing tag.
                    let tag_name = tag_content[1..].split_whitespace().next().unwrap_or("");
                    if let Some(pos) = open_tags.iter().rposition(|t| t == tag_name) {
                        open_tags.remove(pos);
                    }
                } else if !tag_content.ends_with('/') && !tag_content.starts_with('!') && !tag_content.starts_with('?') {
                    // Opening tag (not self-closing, not comment, not processing instruction).
                    let tag_name = tag_content.split_whitespace().next().unwrap_or("");
                    if !tag_name.is_empty() {
                        open_tags.push(tag_name.to_string());
                    }
                }

                i += end + 1;
                continue;
            }
        }
        i += 1;
    }

    !open_tags.is_empty()
}

/// Check if a JSON string appears truncated.
fn is_truncated_json(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() {
        return false;
    }

    // Must start with { or [.
    if !s.starts_with('{') && !s.starts_with('[') {
        return false;
    }

    // Try to parse; if it fails, likely truncated.
    if serde_json::from_str::<serde_json::Value>(s).is_err() {
        // But also check that it looks like it was *meant* to be JSON
        // (not just a random string starting with {).
        let open_braces = s.chars().filter(|&c| c == '{').count();
        let close_braces = s.chars().filter(|&c| c == '}').count();
        let open_brackets = s.chars().filter(|&c| c == '[').count();
        let close_brackets = s.chars().filter(|&c| c == ']').count();

        if open_braces > close_braces || open_brackets > close_brackets {
            return true;
        }
    }

    false
}

/// Calculate the ratio of duplicate paragraphs in text.
fn duplicate_paragraph_ratio(text: &str) -> f64 {
    let paragraphs: Vec<&str> = text
        .split("\n\n")
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();

    if paragraphs.len() <= 1 {
        return 0.0;
    }

    let mut seen = std::collections::HashSet::new();
    let mut duplicates = 0usize;

    for p in &paragraphs {
        let normalized = p.to_lowercase();
        if seen.contains(&normalized) {
            duplicates += 1;
        } else {
            seen.insert(normalized);
        }
    }

    duplicates as f64 / paragraphs.len() as f64
}

// ---------------------------------------------------------------------------
// Action-claim detection
// ---------------------------------------------------------------------------

/// Mutating verbs (Chinese). These name an action that changes the
/// world, so "已经做了" is a factual assertion about what happened —
/// not a description of the subject matter.
const ACTION_VERBS_ZH: &[&str] = &[
    "删除", "重命名", "改名", "创建", "新建", "写入", "保存", "发送", "部署", "修改", "更新", "修复",
    "生成", "下载", "安装", "配置", "发布", "提交", "移动", "复制",
];

/// Mutating verbs (English, base form) — used to detect that the
/// *user* asked for an action.
const ACTION_VERBS_EN: &[&str] = &[
    "delete", "create", "send", "save", "update", "modify", "deploy", "install", "configure",
    "rename", "generate", "download", "commit", "publish", "copy", "move",
];

/// Past-tense forms (English) — used to detect a *claim* that the
/// action was performed.
const ACTION_PAST_EN: &[&str] = &[
    "deleted", "created", "sent", "saved", "updated", "modified", "deployed", "installed",
    "configured", "renamed", "generated", "downloaded", "committed", "published", "copied", "moved",
];

/// Does the user's message ask for a mutating action?
///
/// A **question** that merely mentions the verb is not a request: 删除的
/// 文件还能恢复吗？ / 文档保存了吗？ both name an action without asking
/// for one, and the natural reply describes state (已删除的文件…). In
/// that case an imperative frame is required (帮我 / 请 / please …).
pub fn user_requests_action(text: &str) -> bool {
    let has_verb = if ACTION_VERBS_ZH.iter().any(|v| text.contains(v)) {
        true
    } else {
        let words = ascii_words(text);
        ACTION_VERBS_EN.iter().any(|v| words.iter().any(|w| w == v))
    };
    if !has_verb {
        return false;
    }
    if is_question(text) && !has_imperative_frame(text) {
        return false;
    }
    true
}

/// Interrogative framing (zh + en).
fn is_question(text: &str) -> bool {
    if text.contains('?') || text.contains('？') || text.contains('吗') {
        return true;
    }
    if ["怎么", "如何", "为什么", "是否", "能否", "可不可以", "什么时候", "能不能"]
        .iter()
        .any(|k| text.contains(k))
    {
        return true;
    }
    let words = ascii_words(text);
    ["how", "what", "why", "whether", "does", "is", "can", "should"]
        .iter()
        .any(|k| words.iter().any(|w| w == k))
}

/// Imperative framing (zh + en) — "do this for me".
fn has_imperative_frame(text: &str) -> bool {
    if ["帮我", "请", "给我", "麻烦", "替我", "帮忙"]
        .iter()
        .any(|k| text.contains(k))
    {
        return true;
    }
    let words = ascii_words(text);
    ["please", "can", "could", "would"]
        .iter()
        .any(|k| words.iter().any(|w| w == k))
}

/// Does the reply assert that such an action was performed?
///
/// Chinese frames: `已{verb}` / `已经{verb}` / `{verb}了`.
/// English frames: `successfully {past}` / `i've {past}` /
/// `i have {past}` / `{past} successfully`.
///
/// Two exclusions, both learned from false positives:
/// - **Attributive** `已{verb}的…` describes a noun, it does not assert
///   that the assistant did anything: 已删除的文件可以从回收站恢复。
/// - **Interrogative** `{verb}了吗？/了？` asks, it does not claim.
/// - English `has been {past}` is dropped: it is passive and routinely
///   describes state in documentation ("the config has been updated").
pub fn claims_completed_action(text: &str) -> bool {
    for verb in ACTION_VERBS_ZH {
        for frame in [
            format!("已{verb}"),
            format!("已经{verb}"),
            format!("{verb}了"),
        ] {
            if let Some(hit) = matched_claim_frame(text, &frame) {
                return hit;
            }
        }
    }
    let lower = text.to_lowercase();
    for past in ACTION_PAST_EN {
        if lower.contains(&format!("successfully {past}"))
            || lower.contains(&format!("i've {past}"))
            || lower.contains(&format!("i have {past}"))
            || lower.contains(&format!("{past} successfully"))
        {
            return true;
        }
    }
    false
}

/// Scan every occurrence of `frame`; return `Some(true)` if at least one
/// occurrence is an assertion rather than an attributive/interrogative
/// mention, `None` if the frame never occurs.
fn matched_claim_frame(text: &str, frame: &str) -> Option<bool> {
    let mut from = 0;
    let mut seen = false;
    while let Some(pos) = text[from..].find(frame) {
        seen = true;
        let end = from + pos + frame.len();
        let tail = &text[end..];
        let attributive = tail.starts_with('的');
        // 「删除了吗？」 / 「删除了？」 ask; they do not claim.
        let interrogative = tail.starts_with('吗')
            || tail.starts_with('？')
            || tail.starts_with('?')
            || tail.starts_with("没有");
        if !attributive && !interrogative {
            return Some(true);
        }
        from = end;
    }
    seen.then_some(false)
}

/// Split ASCII words out of arbitrary text for token comparison.
fn ascii_words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// Cap the user-message excerpt used in an advisory line.
fn truncate_for_advisory(text: &str) -> String {
    let t = text.trim();
    if t.chars().count() <= 60 {
        return t.to_string();
    }
    let head: String = t.chars().take(60).collect();
    format!("{}…", head)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_passes_clean_output() {
        let mut gate = AcceptanceGate::new();
        let report = gate.evaluate(
            "Here is the answer to your question.",
            &[],
            &[],
            true,
        );
        assert!(report.is_accepted());
        assert!(matches!(report.overall, GateVerdict::Pass));
    }

    #[test]
    fn gate_rejects_empty_response_to_query() {
        // Empty-reply rejection belongs to `empty_response_handler` in
        // production; the strict profile still exercises the check.
        let mut gate = AcceptanceGate::with_config(GateConfig::strict());
        let report = gate.evaluate("", &[], &[], true);
        assert!(!report.is_accepted());
        assert!(matches!(report.overall, GateVerdict::Reject { .. }));
    }

    #[test]
    fn gate_warns_on_short_response() {
        let mut gate = AcceptanceGate::with_config(GateConfig::strict());
        let report = gate.evaluate("ok", &[], &[], true);
        assert!(report.is_accepted()); // Warn still accepts
        assert!(matches!(report.overall, GateVerdict::Warn { .. }));
    }

    #[test]
    fn gate_rejects_truncated_json() {
        let mut gate = AcceptanceGate::with_config(GateConfig::strict());
        let truncated = r#"{"tool": "shell", "command": "ls"#.to_string();
        let report = gate.evaluate(
            "Running command...",
            &[truncated],
            &[],
            true,
        );
        assert!(!report.is_accepted());
        if let GateVerdict::Reject { split, .. } = &report.overall {
            assert_eq!(*split, GateSplit::Structural);
        } else {
            panic!("expected Reject");
        }
    }

    #[test]
    fn gate_rejects_unclosed_thinking_tags() {
        let mut gate = AcceptanceGate::with_config(GateConfig::strict());
        let thinking = "Let me think <analysis>about this problem".to_string();
        let report = gate.evaluate(
            "Here is my answer.",
            &[],
            &[thinking],
            true,
        );
        assert!(!report.is_accepted());
    }

    #[test]
    fn gate_warns_on_pii_email() {
        let mut gate = AcceptanceGate::with_config(GateConfig::strict());
        let report = gate.evaluate(
            "Contact john@example.com for details.",
            &[],
            &[],
            true,
        );
        assert!(report.is_accepted());
        if let GateVerdict::Warn { split, .. } = &report.behavioral {
            assert_eq!(*split, GateSplit::Behavioral);
        } else {
            panic!("expected Warn");
        }
    }

    #[test]
    fn gate_warns_on_duplicate_paragraphs() {
        let mut gate = AcceptanceGate::new();
        // Text needs to be > 100 chars for the duplicate check to trigger.
        let para = "This is a repeated paragraph that is long enough to pass the hundred character threshold for duplicate detection in the acceptance gate.";
        let text = format!("{}\n\n{}\n\n{}", para, para, para);
        let report = gate.evaluate(&text, &[], &[], true);
        assert!(report.is_accepted()); // Warn still accepts
        if let GateVerdict::Warn { split, .. } = &report.structural {
            assert_eq!(*split, GateSplit::Structural);
        } else {
            panic!("expected Warn, got {:?}", report.structural);
        }
    }

    #[test]
    fn gate_tracks_consecutive_rejections() {
        let mut gate = AcceptanceGate::with_config(GateConfig::strict());
        assert_eq!(gate.consecutive_rejections(), 0);

        // First rejection.
        let report = gate.evaluate("", &[], &[], true);
        assert!(!report.is_accepted());
        assert_eq!(gate.consecutive_rejections(), 1);

        // Second rejection.
        let _report = gate.evaluate("", &[], &[], true);
        assert_eq!(gate.consecutive_rejections(), 2);
        assert!(gate.is_exhausted());
    }

    #[test]
    fn gate_resets_on_acceptance() {
        let mut gate = AcceptanceGate::with_config(GateConfig::strict());
        gate.evaluate("", &[], &[], true);
        assert_eq!(gate.consecutive_rejections(), 1);

        gate.evaluate("Good response here.", &[], &[], true);
        assert_eq!(gate.consecutive_rejections(), 0);
    }

    #[test]
    fn gate_report_advisory_text() {
        let mut gate = AcceptanceGate::with_config(GateConfig::strict());
        let report = gate.evaluate("ok", &[], &[], true);
        assert!(report.advisory_text().is_some());
    }

    // --- unbacked completion claim --------------------------------------

#[test]
    fn warns_on_completion_claim_without_tool_evidence() {
        let mut gate = AcceptanceGate::new();
        let evidence = TurnEvidence {
            has_tool_evidence: false,
            user_requested_action: true,
            user_message: "帮我把桌面上的临时文件删除",
        };
        let report = gate.evaluate_turn(
            "已删除桌面上的临时文件，共 3 个。",
            &[],
            &[],
            true,
            &evidence,
        );
        assert!(report.is_accepted(), "advisory must not drop the reply");
        if let GateVerdict::Warn { split, .. } = &report.behavioral {
            assert_eq!(*split, GateSplit::Behavioral);
        } else {
            panic!("expected Warn, got {:?}", report.behavioral);
        }
        assert!(report.advisory_text().is_some());
    }

    #[test]
    fn no_warning_when_a_tool_actually_ran() {
        // Same reply, but a tool ran — the claim is backed.
        let mut gate = AcceptanceGate::new();
        let evidence = TurnEvidence {
            has_tool_evidence: true,
            user_requested_action: true,
            user_message: "帮我把桌面上的临时文件删除",
        };
        let report = gate.evaluate_turn(
            "已删除桌面上的临时文件，共 3 个。",
            &[],
            &[],
            true,
            &evidence,
        );
        assert!(matches!(report.behavioral, GateVerdict::Pass));
    }

    #[test]
    fn no_warning_when_user_only_asked_a_question() {
        // The user asked about project state, not for an action.
        let mut gate = AcceptanceGate::new();
        let evidence = TurnEvidence {
            has_tool_evidence: false,
            user_requested_action: false,
            user_message: "这个项目已完成的功能有哪些？",
        };
        let report = gate.evaluate_turn(
            "已完成的功能包括登录、导出和同步。",
            &[],
            &[],
            true,
            &evidence,
        );
        assert!(matches!(report.behavioral, GateVerdict::Pass));
    }

    #[test]
    fn no_warning_when_reply_makes_no_completion_claim() {
        let mut gate = AcceptanceGate::new();
        let evidence = TurnEvidence {
            has_tool_evidence: false,
            user_requested_action: true,
            user_message: "帮我删除 a.txt",
        };
        let report = gate.evaluate_turn(
            "我需要先确认文件存在，请提供完整路径。",
            &[],
            &[],
            true,
            &evidence,
        );
        assert!(matches!(report.behavioral, GateVerdict::Pass));
    }

    #[test]
    fn detects_english_completion_claim() {
        assert!(claims_completed_action("I have deleted the temp files."));
        assert!(claims_completed_action("Successfully created the report."));
        assert!(claims_completed_action("The file was successfully renamed."));
        assert!(!claims_completed_action("I will delete the temp files."));
    }

    /// The claim detector must not fire on attributive / interrogative
    /// mentions. These are the exact inputs that made the production
    /// profile inject an advisory with no basis: the user asked *about*
    /// a deletion, and the reply *described* deleted files.
    #[test]
    fn attributive_and_interrogative_frames_are_not_claims() {
        assert!(
            !claims_completed_action("已删除的文件通常可以从回收站恢复。"),
            "「已删除的X」是定语，不是完成声明"
        );
        assert!(!claims_completed_action("文件删除了吗？"), "反问不是声明");
        // …while the real claim still fires.
        assert!(claims_completed_action("已删除桌面上的临时文件，共 3 个。"));
    }

    /// A question that merely names the verb is not an action request.
    /// Without this, 「删除的文件还能恢复吗？」 + 「已删除的文件…」 tripped
    /// all three signals and produced a pointless advisory.
    #[test]
    fn a_question_that_mentions_the_verb_is_not_an_action_request() {
        assert!(!user_requests_action("删除的文件还能恢复吗？"));
        assert!(!user_requests_action("文档保存了吗"));
        assert!(!user_requests_action("how do I update the config?"));
        // An imperative still counts, with or without the question mark.
        assert!(user_requests_action("帮我删除这些临时文件"));
        assert!(user_requests_action("please delete the temp files"));
        assert!(user_requests_action("帮我把配置更新一下"));
    }

    #[test]
    fn detects_action_requests_in_both_languages() {
        assert!(user_requests_action("帮我删除这些文件"));
        assert!(user_requests_action("请创建一份报告"));
        assert!(user_requests_action("please delete the temp files"));
        assert!(!user_requests_action("这个项目是做什么的？"));
    }

    #[test]
    fn production_profile_has_no_enabled_reject_check() {
        // The production profile must never reject: it is advisory
        // only, so it cannot drop a reply or spend a retry.
        let config = GateConfig::default();
        assert!(!config.reject_empty_response);
        assert!(!config.reject_unclosed_thinking);
        assert!(!config.reject_truncated_json);
        assert!(!config.warn_on_pii_patterns);
        // A short reply is often the right reply — flagging every "好的"
        // would bury the advisory that actually matters.
        assert!(!config.warn_on_short_response);
    }

    #[test]
    fn production_profile_does_not_flag_short_replies() {
        // Regression guard: a terse but correct answer must not produce
        // an advisory (the report discipline allows one-line replies).
        let mut gate = AcceptanceGate::new();
        let report = gate.evaluate("好的", &[], &[], true);
        assert!(matches!(report.behavioral, GateVerdict::Pass));
        assert!(report.advisory_text().is_none());
    }

    #[test]
    fn worse_verdict_ordering() {
        let pass = GateVerdict::Pass;
        let warn = GateVerdict::Warn {
            split: GateSplit::Behavioral,
            reason: "test".into(),
        };
        let reject = GateVerdict::Reject {
            split: GateSplit::Structural,
            reason: "test".into(),
            suggestion: "fix it".into(),
        };

        assert!(matches!(AcceptanceGate::worse_verdict(&pass, &warn), GateVerdict::Warn { .. }));
        assert!(matches!(AcceptanceGate::worse_verdict(&warn, &reject), GateVerdict::Reject { .. }));
        assert!(matches!(AcceptanceGate::worse_verdict(&pass, &reject), GateVerdict::Reject { .. }));
    }

    #[test]
    fn pii_detection_email() {
        assert!(contains_pii_patterns("Email: test@example.com"));
        assert!(!contains_pii_patterns("No email here"));
    }

    #[test]
    fn pii_detection_ssn() {
        assert!(contains_pii_patterns("SSN: 123-45-6789"));
        assert!(!contains_pii_patterns("Not an SSN: 123-456-7890"));
    }

    #[test]
    fn unclosed_tags_detected() {
        assert!(has_unclosed_tags("<analysis>this is open"));
        assert!(!has_unclosed_tags("<analysis>closed</analysis>"));
        assert!(!has_unclosed_tags("no tags here"));
        assert!(!has_unclosed_tags("<!-- comment -->"));
    }

    #[test]
    fn truncated_json_detected() {
        assert!(is_truncated_json(r#"{"key": "val"#));
        assert!(!is_truncated_json(r#"{"key": "val"}"#));
        assert!(!is_truncated_json("not json at all"));
        assert!(!is_truncated_json(""));
    }

    #[test]
    fn duplicate_ratio_zero_for_unique() {
        let text = "Paragraph one.\n\nParagraph two.\n\nParagraph three.";
        assert_eq!(duplicate_paragraph_ratio(text), 0.0);
    }

    #[test]
    fn duplicate_ratio_full_for_identical() {
        let text = "Same.\n\nSame.\n\nSame.";
        let ratio = duplicate_paragraph_ratio(text);
        assert!(ratio > 0.5);
    }
}
