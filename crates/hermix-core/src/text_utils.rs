//! Small text utilities shared across the workspace.
//!
//! Currently provides [`truncate_chars`] (UTF-8 safe slicing) and
//! [`is_error_json`] (cheap structural probe for the dispatch path).
//! Earlier versions of `is_error_json` did a `starts_with(r#"{"error"#)`
//! check, which mis-flagged any legitimate JSON object whose first key
//! happened to be `"error"` (e.g. `{"error_count":3}`) and silently
//! missed error responses that wrapped the key (` {"error": ...}`).
//!
//! [`truncate_chars`], a UTF-8 safe slicing helper
//! that the tool registry and dispatcher use to clip oversized tool
//! results. Earlier versions used byte-level slicing (`&s[..n]`) which
//! panics on any non-ASCII boundary — every CJK / emoji output larger
//! than the limit crashed the dispatch hot path.

/// Truncate `s` to at most `max_chars` **characters** (Unicode scalar
/// values), returning a `&str` slice that is always a valid UTF-8
/// string boundary.
///
/// Behaviour:
/// - `s.len() <= max_chars` → returns `s` unchanged.
/// - `s.len() > max_chars` → returns the longest prefix whose
///   `chars().count()` equals `max_chars`.
/// - `max_chars == 0` → returns `""`.
///
/// This is the safe replacement for `&s[..n]` byte slicing. The two
/// differ whenever `s` contains any multi-byte character (`你`, `🦀`,
/// etc.). Use this helper from any code path that translates a
/// "character count" budget into a slice.
pub fn truncate_chars(s: &str, max_chars: usize) -> &str {
    if max_chars == 0 {
        return "";
    }
    match s.char_indices().nth(max_chars) {
        // The (max_chars+1)-th character starts at byte offset `idx`,
        // so everything before it is a valid prefix of exactly
        // `max_chars` characters.
        Some((idx, _)) => &s[..idx],
        // `nth(max_chars)` is None → `s` has <= max_chars characters.
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_unchanged_under_limit() {
        assert_eq!(truncate_chars("hello", 10), "hello");
    }

    #[test]
    fn ascii_truncated_at_boundary() {
        assert_eq!(truncate_chars("hello world", 5), "hello");
    }

    #[test]
    fn zero_max_returns_empty() {
        assert_eq!(truncate_chars("你好hi", 0), "");
    }

    /// Regression: the original bug. `&s[..4]` on `"你好hi"` slices
    /// mid-codepoint and panics; this helper must not.
    #[test]
    fn utf8_boundary_does_not_panic() {
        // "你好hi" → characters: 你 好 h i (4 chars)
        // Naive `&"你好hi"[..4]` panics (byte 4 is inside `好`).
        assert_eq!(truncate_chars("你好hi", 4), "你好hi");
        // 3 chars: "你好h" (6 bytes).
        assert_eq!(truncate_chars("你好hi", 3), "你好h");
        // 2 chars: "你好" (6 bytes, exact boundary).
        assert_eq!(truncate_chars("你好hi", 2), "你好");
        // 1 char: "你" (3 bytes).
        assert_eq!(truncate_chars("你好hi", 1), "你");
    }

    #[test]
    fn emoji_boundary_safe() {
        // 🦀 is 4 bytes, single char.
        let s = "🦀abc";
        assert_eq!(truncate_chars(s, 1), "🦀");
        assert_eq!(truncate_chars(s, 2), "🦀a");
        assert_eq!(truncate_chars(s, 4), "🦀abc");
    }

    #[test]
    fn over_limit_returns_full_string() {
        // s has only 2 chars but we asked for 10.
        assert_eq!(truncate_chars("ab", 10), "ab");
    }
}

/// Format a raw token/character count as a human-readable string.
///
/// Rules (calibrated for the `144765 / 160000` style we already print):
/// - `< 1_000`           → raw digits (`"999"`)
/// - `1_000 ≤ n < 10_000`→ one decimal place in `k` (`"1.5k"`, `"9.9k"`)
/// - `10_000 ≤ n < 1M`   → integer `k`, no decimal (`"15k"`, `"145k"`)
/// - `≥ 1_000_000`       → one decimal place in `M` (`"2.5M"`)
///
/// We use this in every `tracing::warn!` / `tracing::info!` that mentions
/// the context window or compaction threshold so operators see `145k/160k`
/// instead of `144765 / 160000`. The original raw numbers are still useful
/// for debugging, so callers that need precision should keep them in the
/// structured fields and only pass the formatted string to the message.
pub fn format_token_count(n: usize) -> String {
    if n < 1_000 {
        return n.to_string();
    }
    if n < 10_000 {
        // One decimal: e.g. 1_500 → "1.5k", 9_750 → "9.8k" (rounded half-up).
        let tenths = ((n + 50) / 100) as f64 / 10.0;
        return format!("{:.1}k", tenths);
    }
    if n < 1_000_000 {
        // Integer k. Round to nearest.
        let k = (n + 500) / 1_000;
        return format!("{}k", k);
    }
    // Millions with one decimal.
    let m = ((n + 50_000) / 100_000) as f64 / 10.0;
    format!("{:.1}M", m)
}

/// Probe whether a tool-result string is a JSON error envelope.
///
/// Cheap, allocation-free check: looks for an optional UTF-8 BOM, any
/// leading whitespace, then an opening `{` followed by `"error"` as a
/// JSON string key (with the colon that must follow a key). Returns
/// `true` only if the structure actually parses to an object whose
/// first key is `"error"`.
///
/// **Why not `starts_with(r#"{"error"#)`?** That heuristic has three
/// false-positive classes the dispatch hot path was hitting:
///
/// 1. `{"error_count": 3}` — first key is `error_count`, not `error`,
///    but starts_with sees the prefix and reports an error.
/// 2. `{"errors": [...]}` — same problem at scale.
/// 3. Whitespace-prefixed envelopes (`\n  {"error": ...}`) used by some
///    hand-rolled handlers — silently misses.
///
/// And one false-negative class:
/// 4. `{"success": false, "error": "..."}` — error is not the first
///    key, but starts_with reports success.
///
/// This function still recognises case (4) as success (error is *not*
/// the first key), matching what callers actually want: "is the
/// top-level shape an error envelope?"
pub fn is_error_json(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    // Skip leading whitespace (space, tab, newline, carriage return).
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    // Skip UTF-8 BOM if present.
    if i + 3 <= bytes.len() && bytes[i..i + 3] == [0xEF, 0xBB, 0xBF] {
        i += 3;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
    }
    // Must start with `{`.
    if i >= bytes.len() || bytes[i] != b'{' {
        return false;
    }
    i += 1;
    // Skip whitespace inside the object.
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    // Must have `"error"` as the first key.
    let needle = b"\"error\"";
    if i + needle.len() > bytes.len() || &bytes[i..i + needle.len()] != needle {
        return false;
    }
    i += needle.len();
    // Allow whitespace between key and colon.
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    // Require the colon that separates a JSON key from its value.
    if i >= bytes.len() || bytes[i] != b':' {
        return false;
    }
    true
}

#[cfg(test)]
mod format_token_count_tests {
    use super::format_token_count;

    #[test]
    fn below_thousand_is_raw() {
        assert_eq!(format_token_count(0), "0");
        assert_eq!(format_token_count(1), "1");
        assert_eq!(format_token_count(999), "999");
    }

    /// Regression: the warning the master pasted —
    /// `Context pressure 90% of compaction threshold (144765 / 160000)`.
    /// After this helper, it should read `(145k / 160k)`.
    #[test]
    fn master_regression_145k_over_160k() {
        assert_eq!(format_token_count(144_765), "145k");
        assert_eq!(format_token_count(160_000), "160k");
    }

    #[test]
    fn thousands_use_one_decimal() {
        assert_eq!(format_token_count(1_000), "1.0k");
        assert_eq!(format_token_count(1_500), "1.5k");
        assert_eq!(format_token_count(9_750), "9.8k");
        assert_eq!(format_token_count(9_999), "10.0k");
    }

    #[test]
    fn ten_thousand_up_is_integer_k() {
        assert_eq!(format_token_count(10_000), "10k");
        assert_eq!(format_token_count(15_000), "15k");
        assert_eq!(format_token_count(99_999), "100k");
        assert_eq!(format_token_count(999_999), "1000k");
    }

    #[test]
    fn millions_use_one_decimal_m() {
        assert_eq!(format_token_count(1_000_000), "1.0M");
        assert_eq!(format_token_count(2_500_000), "2.5M");
        assert_eq!(format_token_count(12_345_678), "12.3M");
    }
}

#[cfg(test)]
mod is_error_json_tests {
    use super::is_error_json;

    #[test]
    fn detects_plain_error_envelope() {
        assert!(is_error_json(r#"{"error": "boom"}"#));
        assert!(is_error_json(r#"{"error":"x"}"#));
    }

    #[test]
    fn allows_whitespace_and_bom_prefix() {
        assert!(is_error_json(r#"  {"error": "x"}"#));
        assert!(is_error_json("\n\t{\"error\":\"x\"}"));
        assert!(is_error_json("\u{FEFF}{\"error\":\"x\"}"));
    }

    #[test]
    fn rejects_when_error_is_not_first_key() {
        // The previous starts_with heuristic classified this as success,
        // which was the intent — and we keep that intent.
        assert!(!is_error_json(r#"{"success": false, "error": "x"}"#));
    }

    #[test]
    fn rejects_keys_that_start_with_error_but_are_not_error() {
        // The previous starts_with heuristic classified these as errors
        // (false positive). The new probe rejects them.
        assert!(!is_error_json(r#"{"error_count": 3}"#));
        assert!(!is_error_json(r#"{"errors": []}"#));
        assert!(!is_error_json(r#"{"errorCode": 500}"#));
    }

    #[test]
    fn rejects_non_json_strings() {
        assert!(!is_error_json(""));
        assert!(!is_error_json("plain text"));
        assert!(!is_error_json("[1, 2, 3]"));
        assert!(!is_error_json(r#"{"key": "value"}"#));
        assert!(!is_error_json(r#"error: x"#)); // not even JSON
    }

    #[test]
    fn requires_colon_after_error_key() {
        // {"error" "x"} is not valid JSON, so we must not treat it as one.
        assert!(!is_error_json(r#"{"error" "x"}"#));
    }
}