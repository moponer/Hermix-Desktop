//! Automatic context-window compression for long conversations.
//!
//! Port of Python `agent.context_compressor.ContextCompressor` (738 LoC).
//! See the Python source for a detailed algorithm description; the Rust
//! version mirrors the Python compressor step for step:
//!
//! 1. **Prune** old `tool` results (cheap pre-pass, no LLM call)
//! 2. **Protect** head messages (system prompt + first exchange)
//! 3. **Find tail boundary** by token budget (~20K tokens of recent context)
//! 4. **Summarise** middle turns via the [`AuxiliaryClient`] using a
//!    structured Goal / Progress / Decisions / Files / Next-Steps prompt
//! 5. **Sanitise** orphaned tool_call / tool_result pairs so the API never
//!    receives mismatched IDs
//!
//! Iterative summary updates preserve information across multiple
//! compactions: the previous summary is fed back into the prompt and the
//! model is told to update rather than rewrite.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use hermix_core::{
    AgentError, CompressionEngine, CompressionEngineMetrics, CompressionEngineSnapshot,
    CompressionEngineState, LlmProvider, Message, MessageRole,
};
use hermix_core::traits::CoreContextEngine;
use hermix_intelligence::auxiliary::{
    AuxiliaryClient, AuxiliaryError,
};

// ---------------------------------------------------------------------------
// Constants — kept aligned with Python module-level globals.
// ---------------------------------------------------------------------------

/// Banner prepended to every compaction summary so a downstream agent knows
/// some history has been compacted into prose.
pub const SUMMARY_PREFIX: &str =
    "[CONTEXT COMPACTION] Earlier turns in this conversation were compacted \
     to save context space. The summary below describes work that was \
     already completed, and the current session state may still reflect \
     that work (for example, files may already be changed). Use the summary \
     and the current state to continue from where things left off, and \
     avoid repeating work.\n\n\
     **TASK RECOVERY INSTRUCTIONS:**\n\
     • Read the summary carefully — it contains your task context.\n\
     • Check 'In Progress' and 'Blocked' sections — resume from that point.\n\
     • Check 'Next Steps' — execute those unless the user provides new instructions.\n\
     • SUPERSEDED OPTIONS ARE OBSOLETE: if a recent user message rejected, revised, or \
     redirected away from options/plans listed in 'Next Steps' or 'In Progress', treat those \
     items as superseded — follow the user's most recent instruction instead. Do NOT re-issue, \
     re-echo, or execute options the user has already moved past.\n\
     • Do NOT restart completed work listed in 'Done' section.\n\
     • If the summary mentions specific files, verify their current state before acting.\n\n\
     IMPORTANT — read before responding:\n\
     • Do NOT echo, quote or re-issue any inline placeholders you see in \
     the recent messages below (e.g. @image#1, @file#2, image#1: ...). \
     Those were inline attachments handled by the UI and have already \
     been consumed. If you reproduce them, the user will see them again \
     as if you just produced them.\n\
     • This banner is a system-level metadata marker, not a user turn. \
     Never treat it as a new question or task.";

/// Older banner from v1 — stripped before re-applying [`SUMMARY_PREFIX`] so
/// iterative updates don't accumulate prefixes.
pub const LEGACY_SUMMARY_PREFIX: &str = "[CONTEXT SUMMARY]:";

/// Placeholder substituted for old, oversized tool results.
pub const PRUNED_TOOL_PLACEHOLDER: &str = "[ALREADY EXECUTED] [Old tool output cleared to save context space]";

/// Regex patterns for sensitive information that must not leak into summaries.
static SENSITIVE_PATTERNS: once_cell::sync::Lazy<Vec<(regex::Regex, &'static str)>> =
    once_cell::sync::Lazy::new(|| {
        vec![
            (regex::Regex::new(r"sk-[A-Za-z0-9]{20,}").unwrap(), "[REDACTED_KEY]"),
            (regex::Regex::new(r"sk-ant-[A-Za-z0-9]{20,}").unwrap(), "[REDACTED_KEY]"),
            (regex::Regex::new(r"gh[pousr]_[A-Za-z0-9]{36,}").unwrap(), "[REDACTED_TOKEN]"),
            (regex::Regex::new(r"eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]+").unwrap(), "[REDACTED_JWT]"),
            (regex::Regex::new("(?i)api[_-]?key\\s*[=:]\\s*[\"']?[A-Za-z0-9\\-._]{16,}").unwrap(), "api_key=[REDACTED]"),
            (regex::Regex::new("(?i)password\\s*[=:]\\s*[\"']?[^\\s\"']{4,}").unwrap(), "password=[REDACTED]"),
            (regex::Regex::new("(?i)secret\\s*[=:]\\s*[\"']?[^\\s\"']{4,}").unwrap(), "secret=[REDACTED]"),
            (regex::Regex::new(r"Bearer\s+[A-Za-z0-9\-._~+/]+=*").unwrap(), "Bearer [REDACTED]"),
        ]
    });

/// Replace sensitive information (API keys, tokens, passwords) with redacted placeholders.
pub fn redact_sensitive_text(text: &str) -> String {
    if text.is_empty() {
        return text.to_string();
    }
    let mut result = text.to_string();
    for (re, replacement) in SENSITIVE_PATTERNS.iter() {
        result = re.replace_all(&result, *replacement).to_string();
    }
    result
}

/// Hard ceiling on the rendered summary length. If the LLM-emitted
/// summary exceeds this, the safety net truncates it at the nearest
/// paragraph / line boundary. 8 KiB is generous for a 3000-char
/// instruction-budget summary and well under one context block.
pub const MAX_SUMMARY_SAFETY_NET_CHARS: usize = 8 * 1024;

/// Patterns that look like prompt-injection attempts: if the LLM
/// faithfully echoes these phrases from a captured tool result, they
/// could otherwise steer downstream turns. Matched case-insensitively.
pub const PROMPT_INJECTION_MARKERS: &[&str] = &[
    "ignore previous instructions",
    "ignore all previous instructions",
    "ignore the above",
    "disregard previous",
    "disregard all previous",
    "forget everything",
    "new instructions:",
    "system: you are",
    "<|im_start|>",
    "<|im_end|>",
    "### instruction",
    "### system",
];

/// Replace obvious prompt-injection phrases with a sentinel so the
/// downstream model can clearly see the marker and ignore the rest.
/// Detection is conservative — we only flag phrases that appear as a
/// standalone directive (start of input, start of line, or after a
/// sentence boundary), not substrings buried inside another phrase.
/// The replacement preserves the surrounding punctuation so the
/// downstream text remains readable.
pub fn redact_prompt_injection(text: &str) -> String {
    let mut redacted = text.to_string();
    for marker in PROMPT_INJECTION_MARKERS {
        // Match the marker preceded by a sentence boundary (start of
        // input, start of line, or `. `) — use a non-capturing group
        // and put it back via $1 so the boundary survives the
        // replacement. The `\s*` between boundary and marker eats any
        // incidental leading whitespace.
        let anchored = format!(
            r"(?im)((?:^|\.\s|\n)\s*){}",
            regex::escape(marker)
        );
        if let Ok(re) = regex::Regex::new(anchored.as_str()) {
            redacted = re
                .replace_all(&redacted, "${1}[REDACTED_INJECTION]")
                .to_string();
        }
        // Bare word-boundary fallback for markers that did not match
        // the anchored form (e.g. inside a sentence). Conservative
        // word boundaries avoid eating punctuation around the phrase.
        let bare = format!(r"(?i)\b{}\b", regex::escape(marker));
        if let Ok(re) = regex::Regex::new(bare.as_str()) {
            redacted = re
                .replace_all(&redacted, "[REDACTED_INJECTION]")
                .to_string();
        }
    }
    redacted
}

/// Truncate the summary to the hard ceiling, preferring to cut at a
/// paragraph or sentence boundary so the tail stays coherent.
/// Uses character-based counting to avoid panicking on multi-byte
/// UTF-8 characters (e.g. CJK). The old byte-based slicing would
/// panic if `max_chars` fell in the middle of a multi-byte character.
pub fn truncate_summary_at_boundary(text: &str, max_chars: usize) -> String {
    // Find the byte position corresponding to max_chars characters
    let max_bytes = text
        .char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(text.len());

    if max_bytes >= text.len() {
        return text.to_string();
    }

    let cut = &text[..max_bytes];
    // Walk back to the nearest paragraph break, then to a sentence break,
    // then to a line break, then to a word break. Whichever is closest wins.
    // Use char_indices-based search to stay on character boundaries.
    let boundary = ["\n\n", ". ", "\n", " "]
        .iter()
        .filter_map(|needle| {
            cut.rfind(needle).and_then(|byte_pos| {
                // Ensure the boundary is at a char boundary
                if cut.is_char_boundary(byte_pos) {
                    Some(byte_pos)
                } else {
                    // Walk back to the nearest char boundary
                    cut[..byte_pos]
                        .char_indices()
                        .last()
                        .map(|(i, _)| i)
                }
            })
        })
        .max()
        .unwrap_or(max_bytes);
    let keep = &cut[..boundary];
    let suffix = if keep.trim_end().is_empty() {
        ""
    } else {
        "\n\n[…summary truncated by safety net…]"
    };
    format!("{keep}{suffix}")
}

/// (safety net): apply all three layers of defence to a freshly
/// generated summary, in order:
/// 1. **Sensitive data** — strip API keys, JWTs, passwords, bearer
/// tokens via [`redact_sensitive_text`].
/// 2. **Prompt injection** — collapse obvious injection phrases via
/// [`redact_prompt_injection`].
/// 3. **Length cap** — truncate to [`MAX_SUMMARY_SAFETY_NET_CHARS`]
/// via [`truncate_summary_at_boundary`], preferring paragraph /
/// sentence / line / word boundaries so the tail is readable.
/// Runs only on the LLM-derived summary path; the static fallback
/// marker is hard-coded and assumed safe.
pub fn apply_summary_safety_net(text: String) -> String {
    let redacted = redact_sensitive_text(&text);
    let injection_safe = redact_prompt_injection(&redacted);
    truncate_summary_at_boundary(&injection_safe, MAX_SUMMARY_SAFETY_NET_CHARS)
}

/// Strip image / file / clipboard placeholders from tool output content.
/// The vision pipeline (read_image results, the supervisor variant, etc.)
/// emits
/// `image#1`, `image#2`, `@file#3`, `@clipboard#1`, … placeholders
/// in their textual summaries. These strings reference in-flight
/// attachments that the *frontend* renders but that have no meaning
/// to the LLM. If we leave them in the compressed message list, the
/// downstream agent can echo them back as if they were valid
/// content, and the frontend keeps the corresponding `ToolEventCard`
/// in a permanent "running" state because the placeholder never
/// resolves to a completed attachment id.
/// We strip both the leading form (`@image#1`, `image#1: ...`) and
/// the trailing markdown attachment form (`![image](image:1)`,
/// `<attachment id="image:1">`). Counts are reported so the summary
/// keeps the size signal.
pub fn strip_image_placeholders(content: &str) -> (String, usize) {
    let mut stripped = content.to_string();
    let mut count = 0usize;

    // Markdown image syntax pointing at inline attachments.
    let md_re = regex::Regex::new(r"!\[[^\]]*\]\(image:\d+\)").unwrap();
    for _ in md_re.find_iter(&stripped) {
        count += 1;
    }
    stripped = md_re.replace_all(&stripped, "[image]").to_string();

    // Generic attachment XML — sometimes emitted by the desktop bridge.
    let att_re = regex::Regex::new(r#"<attachment[^>]*id\s*=\s*["']image:\d+["'][^>]*/?\s*>"#).unwrap();
    for _ in att_re.find_iter(&stripped) {
        count += 1;
    }
    stripped = att_re.replace_all(&stripped, "[image]").to_string();

    // @image#N, @file#N, @clipboard#N (with optional trailing text).
    let at_re = regex::Regex::new(r"@(image|file|clipboard)#\d+(:[^\s]*)?(\s|$)").unwrap();
    for _ in at_re.find_iter(&stripped) {
        count += 1;
    }
    stripped = at_re.replace_all(&stripped, "[image] ").to_string();

    // Bare image#N: ...  (no leading @) used in some legacy outputs.
    let bare_re = regex::Regex::new(r"\b(image|file|clipboard)#\d+(:[^\s]*)?(\s|$)").unwrap();
    for _ in bare_re.find_iter(&stripped) {
        count += 1;
    }
    stripped = bare_re.replace_all(&stripped, "[image] ").to_string();

    (stripped, count)
}

/// Create an informative 1-line summary of a tool call + result.
/// Returns strings like:
/// - `[terminal] ran \`npm test\` -> exit 0, 47 lines output`
/// - `[read_file] read config.py from line 1 (1,200 chars)`
/// - `[search_files] content search for 'compress' in agent/ -> 12 matches`
/// As of this also strips inline image / file / clipboard
/// placeholders before summarizing (see [`strip_image_placeholders`]).
/// Without that step, compressed messages kept the raw `@image#1`
/// references which made the frontend `ToolEventCard` permanently
/// show "running" for those tool calls — see
/// `MEMORY.md` §"image-placeholder residue" for the full report.
pub fn summarize_tool_result(tool_name: &str, tool_args: &str, tool_content: &str) -> String {
    let (stripped_content, image_count) = strip_image_placeholders(tool_content);
    let tool_content = stripped_content.as_str();
    let content_len = tool_content.len();
    let line_count = if tool_content.trim().is_empty() {
        0
    } else {
        tool_content.matches('\n').count() + 1
    };

    let args: serde_json::Value = serde_json::from_str(tool_args).unwrap_or(serde_json::Value::Null);

    let get_str = |key: &str| -> String {
        args.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
    };

    let t = tool_name.to_lowercase();

    // All summarized tool results are prefixed with
    // `[ALREADY EXECUTED]` so the LLM knows this tool was already called
    // in a previous turn. Without this prefix, the LLM may see the
    // truncated summary and think the tool hasn't been called yet,
    // leading it to re-issue the same call — the root cause of the
    // "duplicate tool call" bug.
    if t.contains("terminal") || t.contains("bash") || t.contains("exec") {
        let cmd = get_str("command");
        let cmd = if cmd.len() > 80 { format!("{}...", &cmd[..cmd.floor_char_boundary(77)]) } else { cmd };
        return format!("[ALREADY EXECUTED] [terminal] ran `{cmd}` -> {line_count} lines output");
    }
    if t.contains("read_file") {
        let path = get_str("path");
        let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(1);
        return format!("[ALREADY EXECUTED] [read_file] read {path} from line {offset} ({content_len} chars)");
    }
    if t.contains("write_file") {
        let path = get_str("path");
        return format!("[ALREADY EXECUTED] [write_file] wrote to {path} ({line_count} lines)");
    }
    if t.contains("search_files") {
        let pattern = get_str("pattern");
        let path = if args.get("path").and_then(|v| v.as_str()).is_some() {
            get_str("path")
        } else {
            get_str("directory")
        };
        let count = args.get("total_count").and_then(|v| v.as_u64()).unwrap_or(0);
        return format!("[ALREADY EXECUTED] [search_files] search for '{pattern}' in {path} -> {count} matches");
    }
    if t.contains("patch") || t.contains("edit") {
        let path = get_str("path");
        let mode = if args.get("mode").and_then(|v| v.as_str()).is_some() {
            get_str("mode")
        } else {
            "replace".to_string()
        };
        return format!("[ALREADY EXECUTED] [patch] {mode} in {path} ({content_len} chars)");
    }
    if t.contains("web_search") {
        let query = get_str("query");
        return format!("[ALREADY EXECUTED] [web_search] query='{query}' ({content_len} chars)");
    }
    if t.contains("web_extract") || t.contains("fetch") {
        return format!("[ALREADY EXECUTED] [{tool_name}] ({content_len} chars)");
    }
    if t.contains("delegate_task") {
        let goal = get_str("goal");
        let goal = if goal.len() > 60 { format!("{}...", &goal[..goal.floor_char_boundary(57)]) } else { goal };
        return format!("[ALREADY EXECUTED] [delegate_task] '{goal}' ({content_len} chars)");
    }
    if t.contains("execute_code") || t.contains("code_execution") {
        let code = get_str("code");
        let preview: String = code.chars().take(60).collect();
        return format!("[ALREADY EXECUTED] [execute_code] `{preview}` ({line_count} lines)");
    }
    if t.contains("skill") {
        let name = get_str("name");
        return format!("[ALREADY EXECUTED] [{tool_name}] name={name} ({content_len} chars)");
    }
    if t.contains("memory") {
        let action = get_str("action");
        let target = get_str("target");
        return format!("[ALREADY EXECUTED] [memory] {action} on {target}");
    }
    if t.contains("todo") {
        return "[ALREADY EXECUTED] [todo] updated task list".to_string();
    }

    // Generic fallback
    let mut first_args = Vec::new();
    for (k, v) in args.as_object().map(|o| o.iter()).into_iter().flatten().take(2) {
        if let Some(s) = v.as_str() {
            let sv = if s.len() > 40 { format!("{}...", &s[..s.floor_char_boundary(37)]) } else { s.to_string() };
            first_args.push(format!("{k}={sv}"));
        }
    }
    let args_str = first_args.join(" ");
    let suffix = if image_count > 0 {
        format!(" ({content_len} chars, {image_count} inline image(s) stripped)")
    } else {
        format!(" ({content_len} chars)")
    };
    format!("[ALREADY EXECUTED] [{tool_name}] {args_str}{suffix}")
}

/// Roll up stale / giant tool-result bodies so context pressure drains
/// continuously instead of spiking to the compression threshold.
/// Idempotent: already-short bodies (incl. previously rolled-up rows)
/// pass through unchanged.
///
/// Two tiers:
/// - messages older than `protect_last_n` from the tail: bodies over
///   [`ROLLUP_MIN_CHARS`] are replaced with a `summarize_tool_result`
///   one-liner — the "之前轮次" continuous drain (Pass1 in `compress()`
///   only runs at trigger time; this runs every turn).
/// - in-window giants over `giant_chars` are also rolled up, except the
///   last [`ROLLUP_PROTECT_LAST_ROWS`] tool rows whose exact output the
///   agent may still reference (compression-trigger tier).
///
/// Returns `(message index, original content)` for every replaced row
/// so callers can archive the originals before they vanish from ctx
/// (the compression path already archives via `archive_context_section`
/// first, so it can ignore the return value).
pub fn rollup_tool_results(
    messages: &mut Vec<Message>,
    protect_last_n: usize,
    giant_chars: usize,
) -> Vec<(usize, String)> {
    const ROLLUP_MIN_CHARS: usize = 400;
    const ROLLUP_PROTECT_LAST_ROWS: usize = 2;

    let mut rolled = Vec::new();
    let n = messages.len();
    if n == 0 {
        return rolled;
    }
    // Map tool_call_id -> (tool name, args) from assistant tool_calls so
    // rolled-up rows keep their real identity in the one-line summary.
    let mut call_meta: std::collections::HashMap<String, (String, String)> =
        std::collections::HashMap::new();
    for msg in messages.iter() {
        if msg.role == MessageRole::Assistant {
            if let Some(tcs) = msg.tool_calls.as_ref() {
                for tc in tcs {
                    call_meta.insert(
                        tc.id.clone(),
                        (tc.function.name.clone(), tc.function.arguments.clone()),
                    );
                }
            }
        }
    }
    for (i, msg) in messages.iter_mut().enumerate() {
        if msg.role != MessageRole::Tool {
            continue;
        }
        let content = match msg.content.as_deref() {
            Some(c) => c,
            None => continue,
        };
        let len = content.chars().count();
        let dist_from_tail = n.saturating_sub(i);
        let stale = dist_from_tail > protect_last_n;
        let roll = if stale {
            len > ROLLUP_MIN_CHARS
        } else {
            len > giant_chars && dist_from_tail > ROLLUP_PROTECT_LAST_ROWS
        };
        if !roll {
            continue;
        }
        let (name, args) = msg
            .tool_call_id
            .as_deref()
            .and_then(|id| call_meta.get(id))
            .cloned()
            .unwrap_or_else(|| ("tool".to_string(), "{}".to_string()));
        rolled.push((i, content.to_string()));
        msg.content = Some(summarize_tool_result(&name, &args, content));
    }
    rolled
}

/// Shrink long string values inside a tool-call arguments JSON blob while
/// preserving JSON validity. Non-string values (paths, ints, booleans) are
/// preserved intact. If the arguments are not valid JSON, the original string
/// is returned unchanged.
fn truncate_tool_call_args_json(args: &str) -> String {
    let parsed: serde_json::Value = match serde_json::from_str(args) {
        Ok(v) => v,
        Err(_) => return args.to_string(),
    };

    fn shrink(obj: serde_json::Value, head_chars: usize) -> serde_json::Value {
        match obj {
            serde_json::Value::String(s) => {
                if s.len() > head_chars {
                    serde_json::Value::String(format!("{}...[truncated]", &s[..head_chars.min(s.len())]))
                } else {
                    serde_json::Value::String(s)
                }
            }
            serde_json::Value::Object(map) => {
                let shrunk = map
                    .into_iter()
                    .map(|(k, v)| (k, shrink(v, head_chars)))
                    .collect();
                serde_json::Value::Object(shrunk)
            }
            serde_json::Value::Array(arr) => {
                serde_json::Value::Array(arr.into_iter().map(|v| shrink(v, head_chars)).collect())
            }
            other => other,
        }
    }

    let shrunken = shrink(parsed, 200);
    serde_json::to_string(&shrunken).unwrap_or_else(|_| args.to_string())
}

const CHARS_PER_TOKEN: usize = 4;
const SUMMARY_FAILURE_COOLDOWN: Duration = Duration::from_secs(600);
/// Short cooldown used when the auxiliary chain only has one registered
/// candidate — see [`ContextCompressor::arm_cooldown`] for rationale.
const SUMMARY_FAILURE_COOLDOWN_SHORT: Duration = Duration::from_secs(30);
/// Hard deadline for the LLM summary round-trip inside `compress()`.
/// The serialized prompt can reach ~150K tokens, so generation
/// legitimately needs 60-180s — while each auxiliary candidate only
/// gets its per-task timeout (45s) before the chain failover. Without
/// this bound the stream freezes for N×45s. On expiry `compress()`
/// falls back to the static marker (the designed safety net).
const SUMMARY_LLM_TIMEOUT: Duration = Duration::from_secs(60);

/// Count tokens in text using tiktoken (cl100k_base encoding, used by
/// GPT-4/3.5 and most modern models). Falls back to chars/4 if the
/// tokenizer fails to initialize or the input is too large.
fn count_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    // For very large inputs (>100K chars), tiktoken-rs may stack overflow.
    // Fall back to the chars/4 approximation for these cases.
    if text.len() > 100_000 {
        return text.len() / CHARS_PER_TOKEN;
    }
    // Try to use tiktoken for accurate counting.
    match tiktoken_rs::cl100k_base() {
        Ok(encoder) => encoder.encode_ordinary(text).len(),
        Err(_) => text.len() / CHARS_PER_TOKEN,
    }
}

/// Estimate token count for a message (content + tool call overhead).
fn estimate_message_tokens(msg: &Message) -> usize {
    let content = msg.content.as_deref().unwrap_or("");
    let mut tokens = count_tokens(content) + 10; // 10 tokens overhead per message
    if let Some(tcs) = msg.tool_calls.as_ref() {
        for tc in tcs {
            tokens += count_tokens(&tc.function.name);
            tokens += count_tokens(&tc.function.arguments);
        }
    }
    tokens
}

/// Appended to every summary message so the model has an unambiguous boundary.
const SUMMARY_END_MARKER: &str =
    "\n\n--- END OF CONTEXT SUMMARY — respond to the message below, not the summary above ---";

/// Regex for MEDIA delivery directives that must not leak into summaries.
static MEDIA_DIRECTIVE_RE: once_cell::sync::Lazy<regex::Regex> =
    once_cell::sync::Lazy::new(|| regex::Regex::new(r"MEDIA:\S+").unwrap());

// ---------------------------------------------------------------------------
// Compression Metrics
// ---------------------------------------------------------------------------

/// Statistics for a single compression operation.
/// `#[must_use]` because `last_metrics` is what the persistence path
/// reads to populate `compression_summary_history`. Dropping it loses
/// the compaction trail that the dashboard / next-session recall
/// rely on.
#[must_use = "CompressionMetrics feeds compression_summary_history; dropping it loses the compaction trail and per-pass metrics"]
#[derive(Debug, Clone, Default)]
pub struct CompressionMetrics {
    /// Original token count before compression.
    pub original_tokens: u64,
    /// Token count after compression.
    pub compressed_tokens: u64,
    /// Tokens saved (original - compressed).
    pub tokens_saved: u64,
    /// Compression ratio (compressed / original).
    pub compression_ratio: f64,
    /// Number of messages before compression.
    pub original_turns: usize,
    /// Number of messages after compression.
    pub compressed_turns: usize,
    /// Number of turns removed by summarization.
    pub turns_removed: usize,
    /// Number of old tool results pruned.
    pub tool_results_pruned: usize,
    /// Number of duplicate tool results deduplicated.
    pub deduplication_count: usize,
    /// number of user-text adjacent dedups
    /// (Pass 0 in `prune_old_tool_results`). 0 means none.
    pub user_dedup_count: usize,
    /// number of messages preserved by pin
    /// filter (skipped every compression stage).
    pub pin_count: usize,
    /// number of messages dropped by
    /// `TokenScorer` (score below `drop_score_threshold`).
    pub dropped_count: usize,
    /// what triggered this compression —
    /// `"threshold"`, `"explicit"`, etc. Empty means unknown.
    pub trigger_reason: String,
    /// Number of summary API calls made.
    pub summary_api_calls: u32,
    /// Number of summary API errors.
    pub summary_errors: u32,
    /// Whether anti-thrashing was triggered.
    pub anti_thrashing_triggered: bool,
}

impl std::fmt::Display for CompressionMetrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Compression: {} -> {} messages ({:.1}% savings), {} tools pruned, {} deduped",
            self.original_turns,
            self.compressed_turns,
            if self.original_tokens > 0 {
                (self.tokens_saved as f64 / self.original_tokens as f64) * 100.0
            } else {
                0.0
            },
            self.tool_results_pruned,
            self.deduplication_count,
        )
    }
}

impl CompressionMetrics {
    /// (B): project into `hermix_core::CompressionMetricsSnapshot`
    /// so `AgentResult` (which lives in `hermix-core` and cannot
    /// depend on `hermix-agent`) can carry the metrics without a
    /// circular dep. The fields are 1:1 — `to_snapshot` is just a
    /// deref over a different `crate` path.
    pub fn to_snapshot(&self) -> hermix_core::types::CompressionMetricsSnapshot {
        hermix_core::types::CompressionMetricsSnapshot {
            original_tokens: self.original_tokens,
            compressed_tokens: self.compressed_tokens,
            tokens_saved: self.tokens_saved,
            compression_ratio: self.compression_ratio,
            original_turns: self.original_turns,
            compressed_turns: self.compressed_turns,
            turns_removed: self.turns_removed,
            tool_results_pruned: self.tool_results_pruned,
            deduplication_count: self.deduplication_count,
            summary_api_calls: self.summary_api_calls,
            summary_errors: self.summary_errors,
            anti_thrashing_triggered: self.anti_thrashing_triggered,
        }
    }
}

// Truncation limits for the summariser input (per Python class constants).
const CONTENT_MAX: usize = 6_000;
const CONTENT_HEAD: usize = 4_000;
const CONTENT_TAIL: usize = 1_500;
const TOOL_ARGS_MAX: usize = 1_500;
const TOOL_ARGS_HEAD: usize = 1_200;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors returned by [`ContextCompressor`] operations.
#[derive(Debug, thiserror::Error)]
pub enum CompressionError {
    /// The auxiliary call could not produce a summary and the cooldown is
    /// active. The caller should fall through to the static fallback marker.
    #[error("summarisation cooldown active for another {0:?}")]
    CooldownActive(Duration),

    /// The underlying auxiliary client failed in a non-retryable way.
    #[error("auxiliary client error: {0}")]
    Auxiliary(#[from] AuxiliaryError),

    /// The LLM provider returned an error during the summarisation call.
    #[error("LLM provider error: {0}")]
    Llm(#[from] AgentError),
}

// ---------------------------------------------------------------------------
// Backwards-compatible helper kept from the old stub.
// ---------------------------------------------------------------------------

/// Build a compact summary text from older messages via an LLM provider.
/// Lightweight one-shot helper retained for callers that don't want the full
/// [`ContextCompressor`] state machine.
pub async fn summarize_messages_with_llm(
    provider: &dyn LlmProvider,
    messages: &[Message],
    model: Option<&str>,
) -> Result<String, AgentError> {
    let mut prompt_messages = Vec::with_capacity(2 + messages.len());
    prompt_messages.push(Message::system(
        "Summarize the conversation into concise bullets. Preserve facts, decisions, todos, file paths, and unresolved questions.",
    ));
    prompt_messages.push(Message::user(
        "Return only the summary text. Keep it under 3000 characters.",
    ));
    prompt_messages.extend_from_slice(messages);

    let resp = provider
        .chat_completion(&prompt_messages, &[], Some(700), Some(0.1), model, None)
        .await?;

    Ok(resp
        .message
        .content
        .unwrap_or_else(|| "[summary unavailable]".to_string()))
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Tunable options for [`ContextCompressor`]. All fields default to the
/// Python implementation's defaults.
#[derive(Debug, Clone)]
pub struct CompressorConfig {
    /// Total context window (in tokens) of the *primary* model the
    /// compressor protects.
    pub context_length: u64,
    /// Compression triggers when prompt tokens exceed this fraction of
    /// `context_length`.
    pub threshold_percent: f64,
    /// Pressure-warning ratio: emit `Context pressure N%` warnings when
    /// prompt tokens cross this fraction of `context_length`.
    /// Distinct from `threshold_percent`: that one drives the *compressor*
    /// itself (LLM-powered summary); this one drives the user-visible
    /// `Context pressure` warning ladder (`>= 0.95` high, `>= 0.85` medium).
    /// Defaults to `0.90`.
    /// Day 3 lift: `ContextManager::for_model_with` accepts an
    /// `output_reserve_tokens` argument, and `ContextManager::pressure_threshold_tokens`
    /// returns `(max - reserve) * pressure_threshold_percent`. The agent loop
    /// passes this value through, so changing it here changes the runtime
    /// warning threshold without touching the constant.
    pub pressure_threshold_percent: f64,
    /// Tokens reserved for the LLM's response. Subtracts from the effective
    /// window *before* `pressure_threshold_percent` is applied, so we don't
    /// fire the pressure warning while the model still has legitimate
    /// headroom to emit its response.
    /// Mirrors `context::DEFAULT_OUTPUT_RESERVE_TOKENS` (4096). Kept as a
    /// config field so power users with small models can shrink it
    /// (e.g. 8B local models that don't need 4k of headroom).
    pub output_reserve_tokens: usize,
    /// Number of head messages always preserved verbatim (system prompt +
    /// first turns).
    pub protect_first_n: usize,
    /// Hard minimum number of tail messages to protect; the token-budget
    /// cut may keep more.
    pub protect_last_n: usize,
    /// Fraction of `threshold_tokens` to spend on the summary itself.
    pub summary_target_ratio: f64,
    /// Optional override of the auxiliary model used for summarisation.
    pub summary_model_override: Option<String>,
    /// Suppress `tracing::info!` chatter — useful for batch / test runs.
    pub quiet_mode: bool,
    /// wire: minimum message count before a flush sentinel may be
    /// injected. Defaults to 6 so short sessions skip the pre-compression
    // rescue entirely (the rescue costs a turn of context).
    pub flush_sentinel_min_turns: usize,
    /// Minimum tokens for a summary to be considered valid.
    pub min_summary_tokens: u64,
    /// Maximum tokens for summary output.
    pub summary_tokens_ceiling: u64,
    /// Characters per token approximation ratio.
    pub chars_per_token: usize,
    /// roles that should be excluded from all
    /// compression stages. Matched case-sensitively against
    /// `role_label(role)` (`"system"` / `"user"` / `"assistant"` /
    /// `"tool"`). Empty by default — opt-in.
    pub pin_roles: Vec<String>,
    /// case-insensitive substrings. A message
    /// is pinned if its content (or any of its tool-call args)
    /// contains any of these substrings. Empty by default — opt-in.
    pub pin_keywords: Vec<String>,
    /// score threshold for the pre-summary
    /// drop pass. Messages scoring strictly below this value are
    /// removed from context (unless pinned). Set to 0.0 to disable.
    pub drop_score_threshold: f64,
}

impl Default for CompressorConfig {
    fn default() -> Self {
        Self {
            context_length: 200_000,
            threshold_percent: 0.90,
            pressure_threshold_percent: 0.90,
            output_reserve_tokens: 4096,
            protect_first_n: 3,
            protect_last_n: 20,
            summary_target_ratio: 0.20,
            summary_model_override: None,
            quiet_mode: false,
            flush_sentinel_min_turns: 6,
            min_summary_tokens: 2_000,
            summary_tokens_ceiling: 12_000,
            chars_per_token: 4,
            pin_roles: Vec::new(),
            pin_keywords: Vec::new(),
            drop_score_threshold: 0.30,
        }
    }
}

impl CompressorConfig {
    /// Validate that the LLM-powered summariser has a chance of
    /// succeeding at runtime.
    /// Returns `Ok(())` when **either**:
    /// - `summary_model_override` is set to a non-empty model name
    /// (the compressor will use it directly, no chain needed), or
    /// - the auxiliary client has at least one registered provider
    /// (`chain_len() > 0`).
    /// Returns `Err(msg)` when neither is true. In that case the
    /// caller should fall back to the simple character-based
    /// compressor and emit a clear warn-level message — otherwise
    /// every compression attempt will silently fail at runtime (the
    /// LLM call in `summarize_messages_with_llm` returns an error,
    /// `compress()` propagates it, and `auto_compress_if_over_threshold`
    /// only logs `summary_errors = N` without aborting the loop).
    /// Added so wiring a compressor without an LLM source
    /// is detected once at startup instead of once per compression
    /// attempt.
    pub fn validate(&self, auxiliary: &AuxiliaryClient) -> Result<(), String> {
        if self
            .summary_model_override
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
        {
            return Ok(());
        }
        if auxiliary.chain_len() == 0 {
            return Err(format!(
                "CompressorConfig.validate: no `summary_model_override` set and \
                 auxiliary client has 0 registered providers — LLM summarisation \
                 will fail at every compression attempt. Set \
                 `summary_model_override` on CompressorConfig or wire an \
                 auxiliary API key (HERMIX_AUX_*)."
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ContextCompressor
// ---------------------------------------------------------------------------

/// Compresses conversation context when approaching the model's context
/// limit. Mirrors `agent.context_compressor.ContextCompressor`.
pub struct ContextCompressor {
    pub(crate) config: CompressorConfig,
    pub(crate) auxiliary: Arc<AuxiliaryClient>,

    // Derived budgets, fixed at construction time.
    pub(crate) threshold_tokens: u64,
    pub(crate) tail_token_budget: u64,
    pub(crate) max_summary_tokens: u64,

    // Dynamic state.
    pub(crate) compression_count: u64,
    pub(crate) last_prompt_tokens: u64,
    pub(crate) previous_summary: Option<String>,
    pub(crate) failure_cooldown_until: Option<Instant>,
    /// Anti-thrashing: count of consecutive compressions that saved <10%.
    pub(crate) ineffective_compression_count: u32,
    /// Metrics from the most recent compression operation.
    pub(crate) last_metrics: CompressionMetrics,
    /// number of adjacent user-text dedups
    /// in the most recent compress() call (Pass 0).
    pub(crate) last_user_dedup_count: usize,
    /// number of messages preserved by pin
    /// filter in the most recent compress() call.
    pub(crate) last_pin_count: usize,
    /// trigger reason for the most recent
    /// compress() call (`"threshold"`, `"explicit"`, …).
    pub(crate) last_trigger_reason: String,
    /// structured CompressionReport from the
    /// most recent compress() call.
    pub(crate) last_compression_report: Option<crate::compression_pipeline::CompressionReport>,
    /// the raw `turns_to_summarize` slice from the
    /// most recent compression. The compressor folds this into a
    /// handoff summary and the in-memory conversation then drops it
    /// — without this stash, the original wording would be gone
    /// forever the moment `compress()` returns. `agent_loop.rs`
    /// pulls it back out and hands it to
    /// `SessionPersistence::archive_compressed_turns` so a future
    /// cross-session recall path can recover it.
    pub(crate) last_archived_turns: Vec<Message>,
    /// Messages dropped by (`drop_low_score`). Stashed
    /// here so can extend `last_archived_turns` with them —
    /// making the previously-lossy drop path lossless. Reset every
    /// `compress` call (either refilled in or cleared
    /// when `drop_score_threshold <= 0.0`).
    pub(crate) last_dropped_turns: Vec<Message>,
    /// when true, `compress()` skips the LLM
    /// summary call and falls back to the static marker. Set by
    /// [`Self::compress_fast`] for the moderate-pressure path
    /// (50–85%) so the agent loop doesn't block a full turn on an
    /// LLM round-trip. `compress_fast` resets it to `false` in its
    /// own frame after `compress` returns (including early-return
    /// paths — `compress_fast` awaits `compress` and runs the
    /// reset unconditionally afterwards).
    pub(crate) fast_mode: bool,

    // ---- Circuit breaker (Day 4) ----
    /// Consecutive hard LLM failures (auxiliary call errored, not just
    /// low-effectiveness). Distinct from `ineffective_compression_count`
    /// which tracks "saved <10%" soft failures. Together they feed
    /// [`Self::breaker_state`] via [`Self::breaker_threshold`].
    pub(crate) consecutive_llm_failures: u32,
    /// Number of consecutive LLM failures that trips the breaker open.
    /// Mirrors `CompactionConfig::circuit_breaker_threshold` (default 3,
    /// Anthropic's "1279 sessions / 250K API/day" remediation number).
    pub(crate) breaker_threshold: u32,
    /// Wall-clock duration to wait in `BreakerState::Open` before
    /// transitioning to `HalfOpen` for a probe attempt.
    pub(crate) breaker_cooldown: Duration,
    /// Wall-clock instant at which the breaker transitions Open → HalfOpen.
    /// `None` means Closed (or never tripped).
    pub(crate) breaker_open_until: Option<Instant>,
    /// Last breaker state — exposed via [`Self::breaker_state`] so the
    /// agent loop can render the CompactionStatusBanner. Defaults to
    /// [`BreakerState::Closed`].
    pub(crate) last_breaker_state: BreakerState,
    /// wall-clock time of the most recent breaker state
    /// transition, in Unix milliseconds. `None` when the breaker has
    /// never transitioned away from `Closed`. Forwarded to the
    /// `CompactionStatusBanner` as `lastChangedAt` so the UI can show
    /// a "since 3m ago" hint next to the cooldown countdown.
    pub(crate) last_state_change_unix_ms: Option<i64>,
}

// ---------------------------------------------------------------------------
// BreakerState (Day 4) — public, serializable, mirrors the front-end
// `CompactionState` union in `CompactionStatusBanner.tsx` so the agent
// loop can stream it as a WS event without re-shaping.
// ---------------------------------------------------------------------------

/// State of the LLM-compression circuit breaker.
/// State machine (see [`ContextCompressor::record_compression_outcome`]):
/// ```text
/// Closed ──(llm_failure × breaker_threshold)──▶ Open
/// Open   ──(cooldown elapses)────────────────▶ HalfOpen
/// HalfOpen ──(success)───────────────────────▶ Closed
/// HalfOpen ──(failure)────────────────────────▶ Open (reset cooldown)
/// ```
/// `Fallback` is **not** an automatic state — the agent loop transitions
/// to it when it decides to invoke `prune_old_tool_results` instead of
/// `generate_summary` because the breaker is Open. The compressor itself
/// never sets `Fallback`; it just reports `Open` and the caller chooses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakerState {
    /// Breaker healthy: LLM-driven compression runs normally.
    Closed,
    /// Cooldown elapsed, the next compression attempt is a probe.
    /// A single failure re-opens the breaker for another full cooldown.
    HalfOpen,
    /// Breaker tripped: skip LLM compression until cooldown elapses.
    /// The agent loop should either sit idle (next prompt will simply
    /// overflow) or switch to the lightweight `prune_old_tool_results`
    /// path (i.e. set its own UI state to `Fallback`).
    Open,
}

impl Default for BreakerState {
    fn default() -> Self {
        BreakerState::Closed
    }
}

/// Default breaker threshold (Anthropic's incident-recovery number:
/// 3 consecutive LLM failures is enough to stop wasting tokens).
pub const DEFAULT_BREAKER_THRESHOLD: u32 = 3;
/// Default breaker cooldown (10 minutes — long enough that the upstream
/// provider / network issue likely clears, short enough that the user
/// isn't stuck without compression forever).
pub const DEFAULT_BREAKER_COOLDOWN_SECS: u64 = 600;

/// Snapshot of the breaker's current state — serialisable so the agent
/// loop can forward it to the front-end as a `compaction_status` WS event
/// without having to reach into private fields.
/// extended with the pressure / context-window fields the
/// `CompactionStatusBanner` needs to render its "145k / 160k tokens (90%)"
/// footer, plus `last_failure_reason` so the banner can show *why* the
/// last compression attempt was rejected. All new fields are additive —
/// older code paths that call [`BreakerSnapshot::default()`] or
/// serialise through `serde_json` still round-trip cleanly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BreakerSnapshot {
    pub state: BreakerState,
    pub consecutive_llm_failures: u32,
    pub breaker_threshold: u32,
    /// Seconds remaining until the breaker can transition Open → HalfOpen.
    /// `0` when the state is not Open.
    pub cooldown_remaining_secs: u64,
    /// Total compressions attempted (success + failure).
    pub compression_count: u64,
    /// Last compression's measured savings ratio (0.0–1.0). `None` if
    /// no compression has happened yet.
    pub last_savings_ratio: Option<f64>,
    // -- additions: pressure / banner fields ---------------
    /// Tokens currently consumed by the conversation that triggered
    /// the snapshot. `0` when the snapshot is produced outside a
    /// compression attempt (e.g. a periodic heartbeat).
    pub used_tokens: u64,
    /// Token budget at which compression fires
    /// (`context_length * threshold_percent`). `0` when unknown.
    pub threshold_tokens: u64,
    /// Total context window (`context_length`). Used by the front-end
    /// progress bar and banner so the same denominator is shown everywhere.
    pub max_tokens: u64,
    /// Display unit for `used_tokens` / `threshold_tokens`. Always
    /// `"tokens"` (Day 3 renamed the underlying budget from chars to
    /// tokens). The field is a `String` so future i18n can swap in a
    /// localised label without a wire-format break.
    #[serde(default = "default_unit")]
    pub unit: String,
    /// Short, safe-to-display reason for the most recent LLM
    /// compression failure. `None` when the last attempt succeeded
    /// or no attempt has been made yet.
    pub last_failure_reason: Option<String>,
    /// Wall-clock time of the most recent state change, in Unix
    /// milliseconds. `None` when the breaker has never transitioned
    /// away from `Closed`.
    pub last_changed_at_unix_ms: Option<i64>,
    // -- additions: compression process phase ----------
    /// Process phase of the compression attempt that produced this
    /// snapshot. Surfaces to the front-end `CompactionStatusBanner`
    /// so it can show "正在准备压缩" → "正在压缩中" → "压缩完成" /
    /// "压缩失败" instead of jumping straight from the pressure
    /// warning to the post-attempt result.
    /// Recognised values (front-end mirrors them in
    /// `CompactionStatusBanner.tsx`):
    /// - `"idle"`         — no compression in flight (default)
    /// - `"starting"`     — pressure detected, about to begin work
    /// - `"in_progress"`  — archive / score-drop / LLM summary running
    /// - `"complete"`     — last attempt finished successfully
    /// - `"failed"`       — last attempt finished with errors
    /// `#[serde(default)]` keeps older snapshots that don't carry
    /// the field deserialising as `"idle"`.
    #[serde(default = "default_phase")]
    pub phase: String,
}

fn default_phase() -> String {
    "idle".to_string()
}

fn default_unit() -> String {
    "tokens".to_string()
}

impl Default for BreakerSnapshot {
    fn default() -> Self {
        Self {
            state: BreakerState::Closed,
            consecutive_llm_failures: 0,
            breaker_threshold: DEFAULT_BREAKER_THRESHOLD,
            cooldown_remaining_secs: 0,
            compression_count: 0,
            last_savings_ratio: None,
            used_tokens: 0,
            threshold_tokens: 0,
            max_tokens: 0,
            unit: default_unit(),
            last_failure_reason: None,
            last_changed_at_unix_ms: None,
            phase: default_phase(),
        }
    }
}


pub mod core;
pub mod prune;
pub mod summary;
pub mod engine;
#[cfg(test)]
mod tests;

#[async_trait::async_trait]
impl CoreContextEngine for ContextCompressor {
    fn should_compress(&self, current_tokens: u64) -> bool {
        self.should_compress(Some(current_tokens))
    }

    async fn compress(&mut self, messages: Vec<Message>) -> Vec<Message> {
        self.compress(messages, None).await
    }

    // override the fast path so the
    // moderate-pressure route skips the LLM summary round-trip.
    async fn compress_fast(&mut self, messages: Vec<Message>) -> Vec<Message> {
        self.compress_fast(messages, None).await
    }

    fn on_session_reset(&mut self) {
        self.reset_session_state();
    }

    fn on_session_end(&mut self, _session_id: &str) {
        // Clear per-session iterative summary state to prevent
        // a background/cron session's summary from leaking into
        // the next live session via the generate_summary path.
        self.previous_summary = None;
    }
}

// (): adapter so the built-in LLM-powered
// compressor plugs into the new `CompressionEngine` trait
// without forcing plugin authors to implement the
// breaker/metrics surface from scratch. Each method is a thin
// forwarder onto the existing `ContextCompressor` API.
impl CompressionEngine for ContextCompressor {
    fn previous_summary(&self) -> Option<&str> {
        self.previous_summary.as_deref()
    }

    fn last_archived_turns(&self) -> &[Message] {
        &self.last_archived_turns
    }

    fn last_metrics(&self) -> CompressionEngineMetrics {
        CompressionEngineMetrics {
            tokens_saved: self.last_metrics.tokens_saved,
            summary_errors: self.last_metrics.summary_errors as u64,
            compression_count: self.compression_count,
            // propagate the per-pass
            // counters through the engine-agnostic trait surface
            // so `compression_ops::auto_compress_if_over_threshold`
            // can persist them instead of hard-coding zero. The
            // `tracing::info!` line emitted inside `compress()`
            // already had them; this closes the loop for the
            // dashboard / `compression_summary_history` write.
            user_dedup_count: self.last_metrics.user_dedup_count as u64,
            pin_count: self.last_metrics.pin_count as u64,
            dropped_count: self.last_metrics.dropped_count as u64,
            trigger_reason: self.last_metrics.trigger_reason.clone(),
        }
    }

    fn breaker_snapshot(
        &mut self,
        current_tokens: u64,
        threshold_tokens: u64,
        max_tokens: u64,
        failure_reason: Option<String>,
    ) -> CompressionEngineSnapshot {
        let snap = self.breaker_snapshot(current_tokens, threshold_tokens, max_tokens, failure_reason);
        CompressionEngineSnapshot {
            state: match snap.state {
                BreakerState::Closed => CompressionEngineState::Closed,
                BreakerState::HalfOpen => CompressionEngineState::HalfOpen,
                BreakerState::Open => CompressionEngineState::Open,
            },
            current_tokens: snap.used_tokens,
            threshold_tokens: snap.threshold_tokens,
            max_tokens: snap.max_tokens,
            failure_reason: snap.last_failure_reason,
            // `breaker_snapshot` (no phase) defaults to
            // `"idle"`. Callers that want a specific phase should
            // use `breaker_snapshot_with_phase` below.
            phase: "idle".to_string(),
        }
    }

    fn breaker_snapshot_with_phase(
        &mut self,
        current_tokens: u64,
        threshold_tokens: u64,
        max_tokens: u64,
        failure_reason: Option<String>,
        phase: &str,
    ) -> CompressionEngineSnapshot {
        let snap = self.breaker_snapshot_with_phase(
            current_tokens,
            threshold_tokens,
            max_tokens,
            failure_reason,
            phase,
        );
        CompressionEngineSnapshot {
            state: match snap.state {
                BreakerState::Closed => CompressionEngineState::Closed,
                BreakerState::HalfOpen => CompressionEngineState::HalfOpen,
                BreakerState::Open => CompressionEngineState::Open,
            },
            current_tokens: snap.used_tokens,
            threshold_tokens: snap.threshold_tokens,
            max_tokens: snap.max_tokens,
            failure_reason: snap.last_failure_reason,
            phase: snap.phase,
        }
    }

    fn record_outcome(&mut self, success: bool) -> CompressionEngineState {
        match self.record_compression_outcome(success) {
            BreakerState::Closed => CompressionEngineState::Closed,
            BreakerState::HalfOpen => CompressionEngineState::HalfOpen,
            BreakerState::Open => CompressionEngineState::Open,
        }
    }
}

// ---------------------------------------------------------------------------
// Free helpers.
// ---------------------------------------------------------------------------

const COMPACTION_TEMPLATE: &str = "## Goal\n[What the user is trying to accomplish]\n\n\
## Constraints & Preferences\n[User preferences, coding style, constraints, important decisions]\n\n\
## Progress\n### Done\n[Completed work — include specific file paths, commands run, results obtained]\n### In Progress\n[Work currently underway]\n### Blocked\n[Any blockers or issues encountered]\n\n\
## Key Decisions\n[Important technical decisions and why they were made]\n\n\
## Relevant Files\n[Files read, modified, or created — with brief note on each]\n\n\
## Next Steps\n[What needs to happen next to continue the work]\n\n\
## Critical Context\n[Any specific values, error messages, configuration details, or data that would be lost without explicit preservation]\n\n\
## Tools & Patterns\n[Which tools were used, how they were used effectively, and any tool-specific discoveries]";

/// Normalise summary text to the current compaction handoff format,
/// stripping any legacy / current prefix already present.
pub fn with_summary_prefix(summary: &str) -> String {
    let trimmed = summary.trim();
    let stripped = if let Some(rest) = trimmed.strip_prefix(LEGACY_SUMMARY_PREFIX) {
        rest.trim_start()
    } else if let Some(rest) = trimmed.strip_prefix(SUMMARY_PREFIX) {
        rest.trim_start()
    } else {
        trimmed
    };
    if stripped.is_empty() {
        SUMMARY_PREFIX.to_string()
    } else {
        format!("{SUMMARY_PREFIX}\n{stripped}")
    }
}

fn role_label(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
        MessageRole::Automation => "automation",
    }
}

/// Estimate total tokens for a slice of messages.
fn estimate_messages_tokens(messages: &[Message]) -> u64 {
    let mut total: usize = 0;
    for msg in messages {
        total += estimate_message_tokens(msg);
    }
    total as u64
}

fn truncate_middle(text: &str, max: usize, head: usize, tail: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let head_str = take_chars(text, head);
    let tail_str = take_chars_back(text, tail);
    format!("{head_str}\n...[truncated]...\n{tail_str}")
}

fn take_chars(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut end = n;
    while !s.is_char_boundary(end) && end < s.len() {
        end += 1;
    }
    s[..end.min(s.len())].to_string()
}

fn take_chars_back(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut start = s.len() - n;
    while !s.is_char_boundary(start) && start > 0 {
        start -= 1;
    }
    s[start..].to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

