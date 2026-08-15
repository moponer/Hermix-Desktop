//! Text helpers: CJK bigram segmentation and session-note building.
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::focus_stack::FrameState;

pub(crate) fn build_session_note(messages: &[serde_json::Value], session_id: &str) -> String {
    let last_user = messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(|v| v.as_str()) == Some("user"))
        .and_then(|m| m.get("content").and_then(|v| v.as_str()));
    let last_assistant = messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(|v| v.as_str()) == Some("assistant"))
        .and_then(|m| m.get("content").and_then(|v| v.as_str()));
    let user_excerpt = last_user
        .map(|s| s.chars().take(300).collect::<String>())
        .unwrap_or_default();
    let asst_excerpt = last_assistant
        .map(|s| s.chars().take(500).collect::<String>())
        .unwrap_or_default();
    if user_excerpt.is_empty() && asst_excerpt.is_empty() {
        return String::new();
    }
    format!(
        "[session:{session_id}] user: {user_excerpt}\n[session:{session_id}] asst: {asst_excerpt}",
    )
}

/// Parse an RFC3339 timestamp; on error, return the Unix epoch.
pub(crate) fn parse_dt(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_else(|| Utc::now()))
}

pub(crate) fn state_to_str(s: FrameState) -> &'static str {
    match s {
        FrameState::Created => "created",
        FrameState::Pushed => "pushed",
        FrameState::Kept => "kept",
        FrameState::Returned => "returned",
        FrameState::Cleared => "cleared",
    }
}

pub(crate) fn str_to_state(s: &str) -> FrameState {
    match s {
        "pushed" => FrameState::Pushed,
        "kept" => FrameState::Kept,
        "returned" => FrameState::Returned,
        "cleared" => FrameState::Cleared,
        _ => FrameState::Created,
    }
}

pub(crate) fn extract_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(extract_text).collect::<Vec<_>>().join(" "),
        Value::Object(o) => o
            .values()
            .map(extract_text)
            .collect::<Vec<_>>()
            .join(" "),
        _ => v.to_string(),
    }
}

/// CJK bigram preprocessor for FTS5 indexing.
/// SQLite's bundled FTS5 (without `SQLITE_ENABLE_FTS5_TOKENIZER_TRIGRAM`)
/// uses the `unicode61` tokenizer which splits on whitespace — a CJK
/// string like `"我喜欢 Rust"` tokenizes as `["我喜欢", "Rust"]`. Querying
/// for just `"喜欢"` returns 0 hits because there's no `喜欢` token.
/// Application-layer fix: insert overlapping bigrams separated by spaces
/// before passing to FTS5. `"我喜欢"` becomes `"我 喜欢"` (one bigram). The
/// query side needs the same bigram expansion to match.
/// Idempotency: re-running on already-bigrammed text produces single-char
/// "words" but no new bigrams — safe to call repeatedly.
pub(crate) fn cjk_bigram_segment(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(text.len() + chars.len());
    let mut prev: Option<char> = None;
    for &c in &chars {
        if is_cjk(c) {
            // Emit bigram (prev+c) if we have a previous CJK char.
            if let Some(p) = prev {
                if is_cjk(p) {
                    out.push(p);
                    out.push(c);
                    out.push(' ');
                }
            }
            // Emit single-char CJK token too (queries like "我" should match).
            out.push(c);
            out.push(' ');
            prev = Some(c);
        } else {
            // Non-CJK: emit as-is (FTS5 unicode61 will tokenize on whitespace).
            out.push(c);
            prev = None;
        }
    }
    out
}

/// True if `c` is CJK Unified Ideograph, Hiragana, Katakana, or Hangul.
/// FTS5 unicode61 without bigram preprocessor can't index these well.
pub(crate) fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}' |    // CJK Unified Ideographs
        '\u{3400}'..='\u{4DBF}' |    // CJK Extension A
        '\u{F900}'..='\u{FAFF}' |    // CJK Compatibility Ideographs
        '\u{3040}'..='\u{309F}' |    // Hiragana
        '\u{30A0}'..='\u{30FF}' |    // Katakana
        '\u{AC00}'..='\u{D7AF}'      // Hangul Syllables
    )
}
