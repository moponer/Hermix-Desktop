//! Lenient readers for model-supplied tool parameters.
//!
//! LLM tool calls routinely encode scalars as strings even when the
//! schema says otherwise — `{"confirm": "true"}`, `{"overwrite": "1"}`.
//! `serde_json::Value::as_bool` accepts **only** a JSON boolean, so the
//! naive
//!
//! ```ignore
//! params.get("x").and_then(|v| v.as_bool()).unwrap_or(default)
//! ```
//!
//! silently maps "the model said true, as a string" onto `default`.
//! Two distinct failure modes follow, both observed in the wild:
//!
//!   - **silent non-execution** — `move_file(overwrite="true")` reports
//!     "目标已存在，请设置 overwrite: true" for a call that visibly set
//!     it, and the model retries the same broken shape forever.
//!   - **silent wrong-direction action** — `voice_mode(toggle, enabled=
//!     "false")` falls through to `unwrap_or(!previous)`, *inverting*
//!     the intent. The user asks to turn voice off; it turns on.
//!
//! These helpers fix the type mismatch without weakening the value
//! check: only an explicit affirmative counts as `true`.
//!
//! One distinction the old `.unwrap_or(default)` chain erased, and these
//! helpers keep: **absent** and **present-but-wrong-type** are different
//! situations. A missing key means "caller decides" (use `default`); a
//! key that is present but is not an affirmative means "the model said
//! something, and it was not yes" (return `false`). Collapsing the two
//! is what let `{"overwrite": 1}` take a destructive `default` branch.

use serde_json::{Map, Value};

/// Read a boolean parameter, accepting the string forms models emit.
///
/// Accepts as `true`:
///   - JSON `true`
///   - `"true"` / `"TRUE"` / `" true "` (trimmed, case-insensitive)
///   - `"1"`
///
/// Everything else is `false` — including `"false"`, `"yes"`, `""`,
/// `1`, `null`, and any non-string type. The narrowness is the point:
/// these parameters gate deletion and overwrite, so "close enough to
/// true" is exactly the wrong policy. A truthy cast here would be a
/// bypass of the confirmation interlock.
pub fn bool_param(params: &Value, key: &str, default: bool) -> bool {
    match params.get(key) {
        // Present: only an explicit affirmative counts. Anything else —
        // including a number or a nested object — is `false`, NOT the
        // default. Falling back to `default` here would mean
        // `{"overwrite": 1}` silently took the destructive branch whenever
        // the caller's default was `true`.
        Some(v) => truthy_affirmative(v),
        // Absent: the caller decides.
        None => default,
    }
}

/// The single place that decides "is this value an explicit yes?".
///
/// Kept separate so every reader in the crate applies exactly the same
/// rule — a helper that is strict in one call site and lax in another
/// is worse than no helper at all.
fn truthy_affirmative(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::String(s) => {
            let t = s.trim();
            t.eq_ignore_ascii_case("true") || t == "1"
        }
        _ => false,
    }
}

/// Like [`bool_param`] but for genuinely optional flags where "absent"
/// and "present but unparseable" must stay distinguishable.
///
/// Returns `None` when the key is absent (the caller should leave the
/// existing setting alone) and `Some(false)` for anything that is
/// present but not an explicit affirmative. Use this instead of
/// `Option::and_then(as_bool)`, which cannot tell those two apart.
pub fn opt_bool_param(params: &Value, key: &str) -> Option<bool> {
    match params.get(key) {
        None => None,
        Some(_) => Some(bool_param(params, key, false)),
    }
}

/// [`bool_param`] for handlers iterating a JSON object directly
/// (`skill_utils` walks a nested schema's entries, so it holds a
/// `Map<String, Value>` rather than a `Value`).
pub fn bool_param_in_map(params: &Map<String, Value>, key: &str, default: bool) -> bool {
    match params.get(key) {
        Some(v) => truthy_affirmative(v),
        None => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn explicit_affirmatives_are_true() {
        for v in [json!(true), json!("true"), json!("TRUE"), json!(" true "), json!("1")] {
            let params = json!({ "k": v });
            assert!(
                bool_param(&params, "k", false),
                "{:?} should count as true",
                v
            );
        }
    }

    #[test]
    fn near_misses_are_false_not_default() {
        // Two invariants in one test, because they are the same rule seen
        // from two sides:
        //   1. a present-but-not-affirmative value is never true
        //      (no truthy cast);
        //   2. it does NOT fall back to `default` either — `default`
        //      is for an ABSENT key only. This is the safety property:
        //      `{"overwrite": 1}` must not take a `default=true`
        //      destructive branch.
        for v in [
            json!("false"),
            json!("yes"),
            json!("on"),
            json!(""),
            json!(1),
            json!(0),
            json!(null),
            json!("是"),
            json!({}),
            json!([]),
        ] {
            let params = json!({ "k": v });
            assert!(
                !bool_param(&params, "k", true),
                "{:?} must not be treated as true even with default=true",
                v
            );
        }
    }

    #[test]
    fn absent_falls_back_to_default() {
        assert!(bool_param(&json!({}), "k", true));
        assert!(!bool_param(&json!({}), "k", false));
        // Explicit false must beat a true default.
        assert!(!bool_param(&json!({ "k": false }), "k", true));
    }

    #[test]
    fn opt_variant_distinguishes_absent_from_false() {
        assert_eq!(opt_bool_param(&json!({}), "k"), None);
        assert_eq!(opt_bool_param(&json!({ "k": false }), "k"), Some(false));
        assert_eq!(opt_bool_param(&json!({ "k": true }), "k"), Some(true));
        // Present but wrong type is still "we were told something",
        // so it must not read as "leave it alone".
        assert_eq!(opt_bool_param(&json!({ "k": "false" }), "k"), Some(false));
    }
}
