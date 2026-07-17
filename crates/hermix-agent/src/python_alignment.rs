//! Python `run_agent.py` alignment helpers (budget pressure, history hygiene).
//!
//! # Phase A — contract scenarios (mirrors Python tests)
//!
//! 1. **New session**: empty `stored_system_prompt`, fresh `build_system_prompt`, `on_session_start` may fire.
//! 2. **Continue session**: `stored_system_prompt` from SQLite matches prior turn — stable Anthropic prefix cache.
//! 3. **Budget caution (70%)**: `_get_budget_warning` returns `[BUDGET: ...]` injected into last tool JSON as `_budget_warning`.
//! 4. **Budget warning (90%)**: `[BUDGET WARNING: ...]` same injection path.
//! 5. **History replay**: `_strip_budget_warnings_from_history` removes `_budget_warning` / `[BUDGET` tails from tool messages.
//! 6. **Preflight compress**: when context chars exceed threshold before first LLM, compress once (see `AgentLoop`).
//! 7. **Empty LLM reply**: retry up to N times without appending assistant (consumes turn budget like Python).
//! 8. **Streaming**: deltas forwarded; interrupt checked during stream collection.
//! 9. **Hooks**: `pre_llm_call` / `post_llm_call` / tool hooks / `on_session_start` for new sessions.
//! 10. **Result**: `AgentResult` carries `session_cost_usd` / `interrupted` when applicable.

use std::borrow::Cow;

use regex::Regex;
use serde_json::Value;

use hermix_core::{Message, MessageRole, ToolResult};

/// Synthetic user nudge after a Codex intermediate ack (Python `run_agent.py`).
pub const CODEX_CONTINUE_USER_MESSAGE: &str = "[System: Continue now. Execute the required tool calls and only send your final answer after completing the task.]";

lazy_static::lazy_static! {
    /// Mirrors Python `_BUDGET_WARNING_RE` in `run_agent.py`.
    static ref BUDGET_WARNING_TEXT_RE: Regex = Regex::new(
        r"(?s)\[\s*BUDGET(?:\s+WARNING)?:\s*Iteration\s+\d+/\d+\..*?\]"
    )
    .expect("valid regex");
    static ref CODEX_FUTURE_ACK_RE: Regex = Regex::new(
        r"(?i)\b(i[''']ll|i will|let me|i can do that|i can help with that)\b"
    )
    .expect("valid regex");
    static ref ACK_RED: Regex =
        Regex::new(r"(?s)<redacted_thinking>.*?</redacted_thinking>").expect("regex");
    // Match both `<thinking>...</thinking>` (with trailing `g`) and
    // `<think>...</think>` (bare, no `g`). The closing tag is always `</think>`
    // for both flavours. The `(?:ing)?` matches `ing` as a group or nothing,
    // so `<think` (5 chars) and `<thinking` (8 chars) both work — the old
    // `<thinking?` required at least 8 chars (`<thinkin`) and silently failed
    // on `<think>`.
    static ref ACK_THINK: Regex =
        Regex::new(r"(?is)<think(?:ing)?[^>]*>.*?</think\s*>").expect("regex");
    static ref ACK_REASON: Regex = Regex::new(r"(?s)<reasoning>.*?</reasoning>").expect("regex");
    // Strip `<plan>...</plan>` blocks — models that output structured
    // task plans often wrap them in this tag, and the content is just
    // for internal routing, not for the user to read.
    static ref ACK_PLAN: Regex =
        Regex::new(r"(?s)<plan>.*?</plan>").expect("regex");
}

/// Strip turn-scoped budget pressure markers from tool results (Python `_strip_budget_warnings_from_history`).
pub fn strip_budget_warnings_from_messages(messages: &mut [Message]) {
    for msg in messages.iter_mut() {
        if msg.role != MessageRole::Tool {
            continue;
        }
        let Some(content) = msg.content.as_mut() else {
            continue;
        };
        if !content.contains("_budget_warning") && !content.contains("[BUDGET") {
            continue;
        }
        if let Ok(mut parsed) = serde_json::from_str::<serde_json::Map<String, Value>>(content) {
            if parsed.remove("_budget_warning").is_some() {
                *content = serde_json::to_string(&parsed).unwrap_or_else(|_| content.clone());
                continue;
            }
        }
        let cleaned = BUDGET_WARNING_TEXT_RE
            .replace_all(content, "")
            .trim()
            .to_string();
        if cleaned != *content {
            *content = cleaned;
        }
    }
}

/// Two-tier budget pressure string matching Python `_get_budget_warning(api_call_count)`.
pub fn budget_pressure_text(
    api_call_count: u32,
    max_iterations: u32,
    caution_threshold: f64,
    warning_threshold: f64,
    enabled: bool,
) -> Option<Cow<'static, str>> {
    if !enabled || max_iterations == 0 {
        return None;
    }
    let progress = api_call_count as f64 / max_iterations as f64;
    let remaining = max_iterations.saturating_sub(api_call_count);
    if progress >= warning_threshold {
        return Some(Cow::Owned(format!(
            "[BUDGET WARNING: Iteration {api_call_count}/{max_iterations}. \
             Only {remaining} iteration(s) left. \
             Provide your final response NOW. No more tool calls unless absolutely critical.]"
        )));
    }
    if progress >= caution_threshold {
        return Some(Cow::Owned(format!(
            "[BUDGET: Iteration {api_call_count}/{max_iterations}. \
             {remaining} iterations left. Start consolidating your work.]"
        )));
    }
    None
}

/// Inject budget pressure into the last tool result (Python tool-loop tail).
pub fn inject_budget_pressure_into_last_tool_result(
    results: &mut [ToolResult],
    warning: Option<&str>,
) {
    let Some(w) = warning else {
        return;
    };
    if w.is_empty() {
        return;
    }
    let Some(last) = results.last_mut() else {
        return;
    };
    if let Ok(val) = serde_json::from_str::<Value>(&last.content) {
        if let Value::Object(mut map) = val {
            map.insert("_budget_warning".to_string(), Value::String(w.to_string()));
            if let Ok(s) = serde_json::to_string(&map) {
                last.content = s;
                return;
            }
        }
    }
    last.content = format!("{}\n\n{}", last.content, w);
}

fn is_utf16_surrogate_scalar(c: char) -> bool {
    matches!(c as u32, 0xD800..=0xDFFF)
}

/// Replace lone surrogate code points with U+FFFD (Python `_sanitize_surrogates`).
/// Strip common reasoning / thinking wrappers for Codex ack heuristics (subset of Python `_strip_think_blocks`).
///
/// This is the **post-hoc** cleaner used when we already have the full assistant content.
/// For **streaming** content where `<think>...</think>` may straddle two chunks, use
/// [`ThinkBlockParser`] instead.
///
/// `ACK_THINK` matches both `<thinking>...</thinking>` and the bare
/// `<think>...</think>` form used by MiniMax M2.7 / Qwen / DeepSeek-R1.
pub fn strip_think_blocks_for_ack(content: &str) -> String {
    let mut c = content.to_string();
    c = ACK_RED.replace_all(&c, "").to_string();
    c = ACK_THINK.replace_all(&c, "").to_string();
    c = ACK_REASON.replace_all(&c, "").to_string();
    c = ACK_PLAN.replace_all(&c, "").to_string();
    // 2026-07-17: models sometimes ignore the system prompt and emit an
    // unwrapped reasoning monologue ("I think...", "trying to...",
    // "probably...") as the visible reply. Treat the whole blob as
    // internal reasoning so it doesn't leak into the user-facing chat.
    if looks_like_untagged_reasoning(&c) {
        return String::new();
    }
    c
}

/// Heuristic: detect a reasoning monologue that the model forgot to wrap
/// in a think block. These are internal monologues that should render as
/// collapsed reasoning blocks, not as user-visible reply text.
///
/// Only triggers when:
///   * there is no ` <think` / `</think` tag anywhere in the text (so the
///     normal parser already had its chance), and
///   * the text is non-trivial, and
///   * the opening words read like self-directed analysis rather than an
///     answer to the user.
///
/// This is intentionally conservative — false positives would hide real
/// user-facing answers. It targets the common failure mode where a model
/// emits "I think..." / "trying to reconstruct..." before any visible reply.
pub fn looks_like_untagged_reasoning(content: &str) -> bool {
    if content.contains("<think") || content.contains("</think") {
        return false;
    }
    let trimmed = content.trim();
    if trimmed.len() < 40 {
        return false;
    }
    let head = trimmed.chars().take(240).collect::<String>().to_lowercase();
    const SIGNALS: &[&str] = &[
        "trying to",
        "i think",
        "i see",
        "probably",
        "could mean",
        "might be",
        "reconstruct",
        "let me trace",
        "let me check",
        "actually,",
        "seems like",
        "looking at",
        "the diff shows",
        "the issue is",
        "i believe",
        "i suspect",
        "i'm trying",
        "i am trying",
    ];
    SIGNALS.iter().any(|s| head.contains(s))
}

// ---------------------------------------------------------------------------
// Streaming `<think>` splitter
// ---------------------------------------------------------------------------
//
// Some providers (notably MiniMax M2.7, Qwen, DeepSeek-R1 distilled) emit their
// reasoning as a `<think>...</think>` block inside `delta.content` rather than in a
// separate `delta.extra.thinking` field. The block can straddle multiple chunks,
// so we need a small state machine to:
//
//   * keep `<think>` (no trailing `g`) and `<thinking>` (with `g`) as separators,
//   * tolerate a chunk boundary that lands mid-tag (e.g. chunk N ends with `<th`,
//     chunk N+1 starts with `ink>...`),
//   * emit everything outside the block as `content`, everything inside as
//     `reasoning_content`, in real time, with at most ~`MAX_PENDING_CHARS` of
//     look-ahead held in `pending`.
//
// The state machine is intentionally allocation-light: we keep `pending` short,
// and we never copy the full chunk more than once.

/// Maximum chars we will hold back to disambiguate a partial tag.
/// `<redacted_thinking>` is the longest known opening tag at 19 chars; we add slack.
const MAX_PENDING_CHARS: usize = 32;

/// Safety net for models that forget to emit `</think>`.
///
/// If the parser has already emitted more than this many reasoning chars
/// without seeing the close tag, it forcibly exits think mode on the next
/// `feed()` call. This prevents the model from swallowing the entire visible
/// reply as invisible reasoning just because it dropped the closing tag.
///
/// (The previous `THINK_FALLBACK_CHARS` force-close safety net was removed:
/// the front-end now renders think content inline with the visible reply,
/// so silently re-routing a never-closed block to the reply stream would
/// jumble the two. If the model forgets `</think>`, the user simply sees
/// the think text followed by the reply — same shape as WorkBuddy.)
/// State for splitting `<think>...</think>` from a streaming content stream.
#[derive(Debug, Clone, Default)]
pub struct ThinkBlockParser {
    /// Whether the cursor is currently inside a `<think>` block.
    in_think: bool,
    /// Look-ahead buffer: at most a partial tag (≤ [`MAX_PENDING_CHARS`]).
    pending: String,
}

impl ThinkBlockParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns true if the parser is currently inside a `<think>` block.
    pub fn is_in_think(&self) -> bool {
        self.in_think
    }

    /// Feed one chunk of assistant content. Returns `(content_part, reasoning_part)`:
    ///
    /// * `content_part` is text that belongs to the user-visible reply.
    /// * `reasoning_part` is text that belongs to the hidden `<think>` block.
    ///
    /// Chars that look like the start of a tag may be held back briefly to
    /// disambiguate; call [`Self::flush`] at end-of-stream to drain any leftover.
    pub fn feed(&mut self, chunk: &str) -> (String, String) {
        if chunk.is_empty() {
            return (String::new(), String::new());
        }

        let mut combined = std::mem::take(&mut self.pending);
        combined.push_str(chunk);

        // Safety valve: ONLY applies when we are already inside a think
        // block and the buffered contents have grown well past the longest
        // possible close tag. We intentionally do NOT trigger this from
        // NORMAL state on a large first chunk — doing so would skip the
        // `<think` open-tag detection in the loop below, which is exactly
        // the failure mode behind the "空白消息" bug: a 140-char chunk
        // starting with `<think>\n…` would short-circuit here, leave
        // `in_think = false`, and dump the whole blob into `content`.
        if self.in_think && combined.chars().count() > 4 * MAX_PENDING_CHARS {
            let out_content = String::new();
            let mut out_reasoning = String::new();
            out_reasoning.push_str(&combined);
            // Force-exit think mode so subsequent chunks land in
            // `content` instead of re-triggering this safety valve
            // on every feed.
            self.in_think = false;
            self.pending.clear();
            return (out_content, out_reasoning);
        }

        let mut content = String::new();
        let mut reasoning = String::new();
        let mut rest: &str = &combined;

        loop {
            if self.in_think {
                // Look for the close tag.
                match rest.find("</think") {
                    Some(rel) => {
                        let (before, after_open) = rest.split_at(rel);
                        reasoning.push_str(before);
                        // Need a `>` to actually close the tag.
                        match after_open.find('>') {
                            Some(gt) => {
                                rest = &after_open[gt + 1..];
                                self.in_think = false;
                                // Continue processing whatever comes after the close.
                            }
                            None => {
                                // `</think` landed at the tail with no `>` yet — hold it.
                                self.pending = after_open.to_string();
                                return (content, reasoning);
                            }
                        }
                    }
                    None => {
                        // No close tag in this segment. Hold back the longest suffix
                        // that is a prefix of `</think` so we don't emit a partial close.
                        let hold = longest_prefix_suffix(rest, "</think");
                        if hold > 0 {
                            let split_at = rest.len() - hold;
                            let (safe, held) = rest.split_at(split_at);
                            reasoning.push_str(safe);
                            self.pending = held.to_string();
                        } else {
                            reasoning.push_str(rest);
                        }
                        return (content, reasoning);
                    }
                }
            } else {
                // Outside a think block — look for the open tag.
                match rest.find("<think") {
                    Some(rel) => {
                        let (before, after_match) = rest.split_at(rel);
                        content.push_str(before);
                        // `after_match` still starts with the matched `<think`; skip past it
                        // before deciding which variant of the tag this is.
                        let after_tag = &after_match["<think".len()..];
                        if after_tag.starts_with('>') {
                            // `<think>` (no trailing `g`)
                            rest = &after_tag[1..];
                            self.in_think = true;
                        } else if after_tag.starts_with("ing>") {
                            // `<thinking>`
                            rest = &after_tag["ing>".len()..];
                            self.in_think = true;
                        } else if after_tag.starts_with([' ', '\t', '\n', '\r']) {
                            // LLM emitted a malformed open tag like `<think>\n…`,
                            // `<think> ` or `<think\t…` — the open bracket is
                            // not properly closed, but the model clearly intended
                            // a think block. Treat the whitespace-terminated
                            // `<think` as a hard separator and enter think mode.
                            //
                            // Without this, the parser would hold the entire
                            // stream in `pending` waiting for a `>` that never
                            // comes. At end-of-stream `flush()` would then dump
                            // the whole held blob (think text **plus** the
                            // visible reply) into `content`, leaving the user
                            // with a `<think>…用户输入` blob. The frontend's
                            // ORPHAN regex on the read path would then strip
                            // everything down to an empty message — the
                            // recurring "空白消息" bug observed in
                            // session 84d3a6c7-c266-4bed-a6e4-0fd115f18f11.
                            rest = after_tag;
                            self.in_think = true;
                        } else {
                            // Could be `<thinkX…` where X is not a tag char yet
                            // (e.g. chunk N ended at `<thinkA`, N+1 starts with
                            // `>`). Hold the whole match for the next chunk so
                            // we don't mis-attribute it.
                            self.pending = after_match.to_string();
                            return (content, reasoning);
                        }
                    }
                    None => {
                        // No opening tag in this segment. Hold back the longest suffix
                        // that is a prefix of `<think` so we don't emit a partial open.
                        let hold = longest_prefix_suffix(rest, "<think");
                        if hold > 0 {
                            let split_at = rest.len() - hold;
                            let (safe, held) = rest.split_at(split_at);
                            content.push_str(safe);
                            self.pending = held.to_string();
                        } else {
                            content.push_str(rest);
                        }
                        return (content, reasoning);
                    }
                }
            }
        }
    }

    /// Drain any leftover buffer at end-of-stream and return it bucketed by state.
    /// Call this once after the stream is complete; anything still pending is
    /// attributed to the current state (think vs. content).
    pub fn flush(&mut self) -> (String, String) {
        let pending = std::mem::take(&mut self.pending);
        if pending.is_empty() {
            return (String::new(), String::new());
        }
        if self.in_think {
            (String::new(), pending)
        } else {
            (pending, String::new())
        }
    }
}

/// Return the largest `n` (1..=max) such that the last `n` chars of `text` are
/// exactly the first `n` chars of `tag`. Used to detect partial-tag suffixes
/// that must be held back to avoid emitting half a tag.
fn longest_prefix_suffix(text: &str, tag: &str) -> usize {
    let max = tag.chars().count().min(text.chars().count());
    if max == 0 {
        return 0;
    }
    let text_chars: Vec<char> = text.chars().collect();
    let tag_chars: Vec<char> = tag.chars().collect();
    for len in (1..=max).rev() {
        let text_suffix = &text_chars[text_chars.len() - len..];
        let tag_prefix = &tag_chars[..len];
        if text_suffix == tag_prefix {
            return len;
        }
    }
    0
}

/// Detect a planning/ack assistant reply that should get a continuation nudge (Codex Responses API).
pub fn looks_like_codex_intermediate_ack(
    user_message: &str,
    assistant_content: &str,
    history_includes_tool_messages: bool,
) -> bool {
    if history_includes_tool_messages {
        return false;
    }
    let assistant_text = strip_think_blocks_for_ack(assistant_content);
    let assistant_text = assistant_text.trim().to_lowercase();
    if assistant_text.is_empty() || assistant_text.len() > 1200 {
        return false;
    }
    if !CODEX_FUTURE_ACK_RE.is_match(&assistant_text) {
        return false;
    }
    let action_markers: &[&str] = &[
        "look into",
        "look at",
        "inspect",
        "scan",
        "check",
        "analyz",
        "review",
        "explore",
        "read",
        "open",
        "run",
        "test",
        "fix",
        "debug",
        "search",
        "find",
        "walkthrough",
        "report back",
        "summarize",
    ];
    let workspace_markers: &[&str] = &[
        "directory",
        "current directory",
        "current dir",
        "cwd",
        "repo",
        "repository",
        "codebase",
        "project",
        "folder",
        "filesystem",
        "file tree",
        "files",
        "path",
    ];
    let user_text = user_message.trim().to_lowercase();
    let user_targets_workspace = workspace_markers.iter().any(|m| user_text.contains(*m))
        || user_text.contains("~/")
        || user_text.contains('/');
    let assistant_mentions_action = action_markers.iter().any(|m| assistant_text.contains(*m));
    let assistant_targets_workspace = workspace_markers
        .iter()
        .any(|m| assistant_text.contains(*m));
    (user_targets_workspace || assistant_targets_workspace) && assistant_mentions_action
}

pub fn sanitize_surrogates(s: &str) -> Cow<'_, str> {
    if !s.chars().any(is_utf16_surrogate_scalar) {
        return Cow::Borrowed(s);
    }
    let t: String = s
        .chars()
        .map(|c| {
            if is_utf16_surrogate_scalar(c) {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect();
    Cow::Owned(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_core::Message;

    #[test]
    fn strip_json_budget_warning_key() {
        let mut messages = vec![Message {
            role: MessageRole::Tool,
            content: Some(r#"{"ok":true,"_budget_warning":"[BUDGET: x]"}"#.to_string()),
            content_parts: None,
            tool_calls: None,
            tool_call_id: Some("1".into()),
            name: None,
            reasoning_content: None,
            cache_control: None,
            execution_timeline: None,
            ephemeral: false,
        }];
        strip_budget_warnings_from_messages(&mut messages);
        let parsed: serde_json::Value =
            serde_json::from_str(messages[0].content.as_ref().unwrap()).unwrap();
        assert!(!parsed.as_object().unwrap().contains_key("_budget_warning"));
    }

    #[test]
    fn budget_pressure_tiers_match_python_ratios() {
        let max = 60u32;
        // 41/60 ≈ 0.683 — below 0.7 → none
        assert!(budget_pressure_text(41, max, 0.7, 0.9, true).is_none());
        // 42/60 = 0.7 — caution
        let c = budget_pressure_text(42, max, 0.7, 0.9, true).unwrap();
        assert!(c.contains("[BUDGET:") && !c.contains("BUDGET WARNING"));
        // 54/60 = 0.9 — warning
        let w = budget_pressure_text(54, max, 0.7, 0.9, true).unwrap();
        assert!(w.contains("BUDGET WARNING"));
    }

    #[test]
    fn inject_budget_into_json_tool_result() {
        let mut results = vec![ToolResult::ok("tc1", r#"{"a":1}"#)];
        inject_budget_pressure_into_last_tool_result(
            &mut results,
            Some("[BUDGET: Iteration 1/10. 9 iterations left.]"),
        );
        let v: serde_json::Value = serde_json::from_str(&results[0].content).unwrap();
        assert!(v["_budget_warning"].as_str().unwrap().contains("BUDGET"));
    }

    // -----------------------------------------------------------------------
    // ThinkBlockParser tests
    // -----------------------------------------------------------------------

    fn feed_all(p: &mut ThinkBlockParser, chunks: &[&str]) -> (String, String) {
        let mut c = String::new();
        let mut r = String::new();
        for ch in chunks {
            let (cc, rr) = p.feed(ch);
            c.push_str(&cc);
            r.push_str(&rr);
        }
        let (fc, fr) = p.flush();
        c.push_str(&fc);
        r.push_str(&fr);
        (c, r)
    }

    #[test]
    fn think_parser_single_chunk_full() {
        let mut p = ThinkBlockParser::new();
        let (c, r) = feed_all(
            &mut p,
            &["<think>用户问我是谁，我介绍自己。</think>\n\n你好！我是 Hermes Agent。"],
        );
        assert_eq!(r, "用户问我是谁，我介绍自己。");
        assert_eq!(c, "\n\n你好！我是 Hermes Agent。");
    }

    #[test]
    fn think_parser_no_think_blocks() {
        let mut p = ThinkBlockParser::new();
        let (c, r) = feed_all(&mut p, &["你好！我是 Hermes Agent。"]);
        assert_eq!(c, "你好！我是 Hermes Agent。");
        assert_eq!(r, "");
    }

    #[test]
    fn think_parser_split_open_tag_across_chunks() {
        // Chunk 1 ends with `<th`, chunk 2 starts with `ink>...`.
        let mut p = ThinkBlockParser::new();
        let (c1, r1) = p.feed("我先想一下，<th");
        assert_eq!(c1, "我先想一下，");
        assert_eq!(r1, "");
        let (c2, r2) = p.feed("ink>思考中...</think>\n\n实际回复。");
        assert_eq!(c2, "\n\n实际回复。");
        assert_eq!(r2, "思考中...");
    }

    #[test]
    fn think_parser_split_close_tag_across_chunks() {
        let mut p = ThinkBlockParser::new();
        let (c1, r1) = p.feed("<think>思考了一半，剩</th");
        assert_eq!(c1, "");
        assert_eq!(r1, "思考了一半，剩");
        let (c2, r2) = p.feed("ink>\n\n正文。");
        assert_eq!(c2, "\n\n正文。");
        assert_eq!(r2, "");
    }

    #[test]
    fn think_parser_multiple_think_blocks() {
        let mut p = ThinkBlockParser::new();
        let (c, r) = feed_all(
            &mut p,
            &[
                "<think>A</think>中间<think>B</think>末尾",
            ],
        );
        assert_eq!(r, "AB");
        assert_eq!(c, "中间末尾");
    }

    #[test]
    fn think_parser_thinking_with_g() {
        // The older `<thinking>...</thinking>` form must also work.
        let mut p = ThinkBlockParser::new();
        let (c, r) = feed_all(
            &mut p,
            &["<thinking>old-style</thinking>正文"],
        );
        assert_eq!(r, "old-style");
        assert_eq!(c, "正文");
    }

    #[test]
    fn think_parser_partial_at_boundary_recovers() {
        // Chunk ends with `<thi` (3 chars prefix), next chunk starts with `s>not-a-tag`.
        // The parser should hold the partial tag and on the next chunk realize it's
        // not `<think>`, then emit the held `<thi` as content.
        let mut p = ThinkBlockParser::new();
        let (c1, r1) = p.feed("前文<thi");
        assert_eq!(c1, "前文");
        assert_eq!(r1, "");
        // After this feed the parser holds `<thi` and `s` does not complete
        // `<think>` nor `ing>`, so the held text is emitted back as content.
        let (c2, r2) = p.feed("s不是标签的剩余");
        assert_eq!(c2, "<this不是标签的剩余");
        assert_eq!(r2, "");
    }

    #[test]
    fn think_parser_unterminated_think_flushes_as_reasoning() {
        // The model forgot `</think>`; the entire content after the open tag
        // should land in `reasoning` once we flush.
        let mut p = ThinkBlockParser::new();
        let (c, r) = feed_all(&mut p, &["<think>模型忘了写结束"]);
        assert_eq!(r, "模型忘了写结束");
        assert_eq!(c, "");
    }

    #[test]
    fn think_parser_realistic_minimax_chunks() {
        // Simulate what MiniMax M2.7 actually streams: ~5 chunks per response,
        // each with a chunk of think text followed by the close + actual reply.
        // The model rehearses the *whole* reply inside <think> (including `**bold**`
        // markdown and the greeting), so reasoning will be long. Content should
        // be only what comes *after* `</think>`.
        let mut p = ThinkBlockParser::new();
        let chunks: Vec<&str> = vec![
            "<think>用户问我是谁，我需要介绍自己，",
            "用中文回答，列一些我能做的事，",
            "保持简洁。- 💻 ",
            "**编程协助** — 快速原型和代码审查",
            "\n- 🔍 **信息检索** — ",
            "快速定位知识\n- ✍️ **内容创作** — 文章/报告",
            " — 等等。</think>\n\n你好！我是 Hermes Agent，一个由 MiniMax 训练的 AI 助手。\n\n",
            "我能帮你做：\n",
            "1. 编程与代码审查\n",
            "2. 信息检索与总结\n",
            "3. 内容创作与改写\n",
            "4. 数据分析\n",
            "5. 日常问答",
        ];
        let (c, r) = feed_all(&mut p, &chunks);
        // Reasoning must contain the rehearsal (with bold markers) but NOT any
        // stray `<think>` / `</think>` tag that wasn't matched.
        assert!(r.contains("用户问我是谁"));
        assert!(r.contains("**编程协助**"));
        assert!(!r.contains("<think>"));
        assert!(!r.contains("</think>"));
        // Content must contain the real reply and must NOT contain the think tag.
        assert!(c.starts_with("\n\n你好！我是 Hermes Agent"));
        assert!(c.contains("1. 编程与代码审查"));
        assert!(!c.contains("<think>"));
        assert!(!c.contains("</think>"));
        // Total bytes: reasoning should be the rehearsal, content the reply.
        assert!(r.len() > 50);
        assert!(c.len() > 50);
    }

    /// Regression: `<think>\n…` (whitespace-terminated open, not `>` or
    /// `ing>`) must still enter think mode. Otherwise the whole stream
    /// gets held in `pending` and at flush() time the parser dumps the
    /// think text + the visible reply both into `content`, which the
    /// frontend's ORPHAN regex then strips to an empty message
    /// (the recurring "空白消息" bug).
    #[test]
    fn think_parser_whitespace_open_enters_think_mode_single_chunk() {
        let mut p = ThinkBlockParser::new();
        let (c, r) = feed_all(
            &mut p,
            &["<think>\nThe user is greeting me. I should respond."],
        );
        // Reasoning gets the think body; content stays empty (no `>` close
        // ever arrived, so the visible reply is not part of `content`).
        assert!(r.contains("The user is greeting me"));
        assert_eq!(c, "");
        assert!(p.is_in_think(), "parser must be in_think after `<think>\\n…`");
    }

    /// Regression: a chunk boundary that lands between `<think` and `\n…`
    /// (i.e. the malformed open spans two chunks) must still resolve into
    /// think mode on the second chunk.
    #[test]
    fn think_parser_whitespace_open_multi_chunk_boundary() {
        let mut p = ThinkBlockParser::new();
        let (c, r) = feed_all(
            &mut p,
            // Realistic M3 streaming: chunk 1 ends right after `<think`,
            // chunk 2 starts with `\n<model thought>`.
            &["<think", "\n用户问我是谁", "，用中文回答"],
        );
        assert!(r.contains("用户问我是谁"));
        assert!(r.contains("用中文回答"));
        assert_eq!(c, "");
        assert!(p.is_in_think());
    }

    /// Regression: full unclosed-think scenario from
    /// `session 84d3a6c7-c266-4bed-a6e4-0fd115f18f11` — `<think>\n` open
    /// followed by a multi-chunk rehearsal and a visible reply that
    /// never gets a `</think>`. The reply must NOT leak into `content`
    /// (otherwise the persisted message stores the `<think>…` blob and
    /// the frontend's ORPHAN regex blanks it out).
    #[test]
    fn think_parser_unclosed_think_minimax_m3_shape() {
        let mut p = ThinkBlockParser::new();
        let (c, r) = feed_all(
            &mut p,
            &[
                "<think>\nThe user is greeting me. I should respond with a friendly greeting in Chinese since they greeted in Chinese.\n\n你好!有什么可以帮到你的?",
            ],
        );
        // The whole blob lands in reasoning (model never closed the block).
        // Content stays empty so the persisted message does NOT contain
        // the leaked `<think>…` blob.
        assert!(r.contains("你好!有什么可以帮到你的"));
        assert!(!r.contains("<think>"));
        assert_eq!(c, "");
        assert!(p.is_in_think());
    }

    /// Regression: models that ignore the system prompt and emit an
    /// unwrapped reasoning monologue ("I think...", "trying to...")
    /// must be detected as reasoning, not user-visible content.
    #[test]
    fn detects_untagged_reasoning_monologue() {
        let text = "trying to reconstruct what's actually there. The pattern max(1) could mean unwrap_or(1) or something else entirely. I think the issue is that .max(1) is actually a typo.";
        assert!(looks_like_untagged_reasoning(text));
    }

    #[test]
    fn rejects_wrapped_thinking_as_untagged() {
        let text = format!("{open}I should think about this.{close}Here is the answer.", open = "<think>", close = "</think>");
        assert!(!looks_like_untagged_reasoning(&text));
    }

    #[test]
    fn rejects_normal_user_reply_as_untagged_reasoning() {
        assert!(!looks_like_untagged_reasoning("你好！有什么可以帮你的？"));
        assert!(!looks_like_untagged_reasoning("Here is the fixed code."));
        assert!(!looks_like_untagged_reasoning("ok"));
    }

    #[test]
    fn strip_think_blocks_for_ack_wipes_untagged_reasoning() {
        let text = "I think the issue is the missing close tag. Let me check again.";
        let stripped = strip_think_blocks_for_ack(text);
        assert_eq!(stripped, "");
    }
}

