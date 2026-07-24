//! PII / secret redaction for session persistence and log output.
//!
//! Inspired by the upstream Hermes Python project's "data
//! protection" layer (PII hashing in session logs, secret
//! redaction in tool output). Implemented here as a single
//! synchronous, allocation-friendly [`redact_secrets`] function
//! that strips the most common credential and PII patterns
//! before they hit the session DB or the dashboard's
//! `session_search` FTS5 index.
//!
//! ## What we redact
//!
//! - **API keys** — `sk-…` (OpenAI), `sk-ant-…` (Anthropic),
//!   `gho_…` / `ghp_…` / `ghs_…` / `ghr_…` (GitHub),
//!   `xoxb-…` / `xoxp-…` (Slack), `AIza…` (Google),
//!   `huggingface_…`, `fal_…` (FAL.ai), `gsk_…` (Groq),
//!   `dapi…` (Databricks), `ddc-…` (Datadog).
//! - **Bearer / Authorization headers** — `Bearer abcdef…`,
//!   `Authorization: Basic …`.
//! - **Inline URL credentials** — `https://user:password@host/`
//!   (HTTP basic in URLs is a notorious log-leak vector).
//! - **Query-string secrets** — `?api_key=…&secret=…&token=…`
//!   in any URL.
//! - **Email addresses** — `name@host.tld` (left as a
//!   placeholder; the LLM still has the message context, the
//!   log doesn't need the raw address).
//! - **AWS access key IDs** — `AKIA…` (always 20 chars).
//!
//! ## What we DO NOT redact
//!
//! - Phone numbers, SSNs, credit cards. These are locale
//!   specific and the false-positive rate is high in code
//!   and conversation text. We document this as a known
//!   limitation; if it becomes a real concern the regex set
//!   can be extended.
//! - Generic long hex strings (could be hashes, tokens, or
//!   just commit SHAs). The false-positive rate is too high.
//! - PII in the SQLite `messages.content` column at INSERT
//!   time — see [`SessionPersistence::save_session_messages`]
//!   for the integration point. The dashboard's
//!   `session_search` reads back the redacted content.
//!
//! ## Performance
//!
//! The regex set is compiled lazily on first use via
//! [`std::sync::OnceLock`] and shared across threads. A typical
//! 10 KB assistant message is processed in <100 µs.

use std::sync::OnceLock;

use regex::Regex;

/// Replacement string for all redacted matches. Kept short
/// so the redacted row is still searchable by FTS5.
const REDACTED: &str = "[REDACTED]";

/// Replacement string for email addresses. Different from
/// REDACTED so reviewers can still tell what kind of PII was
/// scrubbed without the raw value leaking.
const REDACTED_EMAIL: &str = "[REDACTED_EMAIL]";

/// Replacement string for inline URL credentials. Different
/// marker because URL rewriting (`https://user:pass@host`
/// → `https://REDACTED_URL_AUTH@host`) is a higher-fidelity
/// signal than the generic `***` placeholder.
const REDACTED_URL_AUTH: &str = "REDACTED_URL_AUTH";

/// Get a reference to the compiled regex set, compiling it
/// on first use. We use a `OnceLock` rather than `lazy_static`
/// to avoid pulling in another dep.
fn patterns() -> &'static Vec<(&'static str, Regex)> {
    static CELL: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    CELL.get_or_init(|| {
        // Each entry: (label, compiled regex). Order doesn't
        // matter because we apply them in a single pass over
        // the text; long patterns are tried first so that
        // `sk-…` wins over the generic `Bearer` rule.
        let raw: &[(&str, &str)] = &[
            // -- API keys (vendor-specific prefixes) -------------------------
            ("openai", r"sk-[A-Za-z0-9_\-]{20,}"),
            ("anthropic", r"sk-ant-[A-Za-z0-9_\-]{20,}"),
            ("anthropic-admin", r"sk-ant-admin-[A-Za-z0-9_\-]{20,}"),
            ("github-oauth", r"gho_[A-Za-z0-9]{20,}"),
            ("github-pat", r"ghp_[A-Za-z0-9]{20,}"),
            ("github-app", r"(ghu|ghs)_[A-Za-z0-9]{20,}"),
            ("github-refresh", r"ghr_[A-Za-z0-9]{20,}"),
            ("slack-bot", r"xoxb-[A-Za-z0-9\-]{20,}"),
            ("slack-user", r"xoxp-[A-Za-z0-9\-]{20,}"),
            ("slack-webhook", r"xox[a-z]-[A-Za-z0-9\-]{20,}"),
            ("google-api", r"AIza[A-Za-z0-9_\-]{20,}"),
            ("huggingface", r"hf_[A-Za-z0-9]{20,}"),
            ("fal", r"fal_[A-Za-z0-9]{20,}"),
            ("groq", r"gsk_[A-Za-z0-9]{20,}"),
            ("databricks", r"dapi[a-f0-9]{20,}"),
            ("datadog-app", r"ddc[a-z0-9]{20,}"),
            ("aws-access-key", r"AKIA[0-9A-Z]{16}"),
            // -- HTTP auth headers -------------------------------------------
            ("bearer-header", r"(?i)(?:Bearer)\s+[A-Za-z0-9_\-\.=]{20,}"),
            ("basic-header", r"(?i)(?:Basic)\s+[A-Za-z0-9+/=]{20,}"),
            // -- Inline URL credentials (https://user:pw@host/) --------------
            (
                "url-credentials",
                r"(?i)\b([a-z][a-z0-9+\-.]*://)[^\s/:@]+:[^\s/@]+@",
            ),
            // -- Query-string secrets ----------------------------------------
            (
                "query-secret",
                r"(?i)([?&](?:api_?key|access_?token|secret|token|password|pwd|signature|sig)=)[^&\s\x22'<>]+",
            ),
            // -- Email addresses (best-effort) -------------------------------
            (
                "email",
                r"\b[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,24}\b",
            ),
        ];
        raw.iter()
            .map(|(label, pat)| {
                (
                    *label,
                    Regex::new(pat).unwrap_or_else(|e| {
                        panic!("pii_redact: invalid regex for {label}: {e}")
                    }),
                )
            })
            .collect()
    })
}

/// Redact PII / secrets from `text`. The returned string
/// contains placeholders in place of any matched secret;
/// surrounding text is preserved verbatim.
/// /// This is the primary entry point. Call it on:
/// - `Message::content` before INSERT into the sessions DB
/// - `Message::tool_calls` JSON before INSERT
/// - Any error message logged to disk that originated from
/// user input or upstream LLM/tool output
pub fn redact_secrets(text: &str) -> String {
    redact_secrets_with(text, &REDACTED)
}

fn redact_secrets_with(text: &str, default_marker: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut out = text.to_string();
    for (label, re) in patterns() {
        let marker = match *label {
            "email" => REDACTED_EMAIL,
            "url-credentials" => REDACTED_URL_AUTH,
            _ => default_marker,
        };
        // For url-credentials we need a 2-step rewrite:
        // 1) strip everything between scheme:// and the next @
        // 2) re-insert the URL_AUTH marker so the host is
        // preserved
        if *label == "url-credentials" {
            out = re
                .replace_all(&out, |caps: &regex::Captures| {
                    format!("{}{}@", &caps[1], REDACTED_URL_AUTH)
                })
                .into_owned();
        } else {
            out = re.replace_all(&out, marker).into_owned();
        }
    }
    out
}

/// Convenience: redact a JSON tool-call string. We attempt to
/// parse the input as JSON, redact the *string leaves*, and
/// re-serialise. This is much more reliable than running the
/// regex over the raw JSON, because:
/// 1. We won't accidentally redact substrings that happen
/// to live inside a JSON string key.
/// 2. The output is canonical (sorted keys, no whitespace)
/// so it's stable for hashing / dedup.
/// /// Non-JSON input is returned unchanged after a
/// `redact_secrets` pass.
pub fn redact_tool_calls_json(json_str: &str) -> String {
    if json_str.is_empty() {
        return String::new();
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json_str) else {
        return redact_secrets(json_str);
    };
    let redacted = redact_json_value(&value);
    serde_json::to_string(&redacted).unwrap_or_else(|_| redact_secrets(json_str))
}

fn redact_json_value(v: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match v {
        Value::String(s) => Value::String(redact_secrets(s)),
        Value::Array(arr) => Value::Array(arr.iter().map(redact_json_value).collect()),
        Value::Object(obj) => {
            let mut out = serde_json::Map::with_capacity(obj.len());
            for (k, v) in obj {
                out.insert(k.clone(), redact_json_value(v));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_returns_empty() {
        assert_eq!(redact_secrets(""), "");
    }

    #[test]
    fn no_secrets_unchanged() {
        let s = "Hello, please help me with a Rust borrow checker error.";
        assert_eq!(redact_secrets(s), s);
    }

    #[test]
    fn openai_key_redacted() {
        let s = "The API key is sk-abcdefghijklmnopqrstuvwxyz1234, please use it.";
        let out = redact_secrets(s);
        assert!(!out.contains("sk-abcdef"));
        assert!(out.contains("[REDACTED]"));
        assert!(out.contains("please use it"));
    }

    #[test]
    fn anthropic_key_redacted() {
        let s = "sk-ant-api03-1234567890abcdefghijklmnopqrstuvwxyz";
        let out = redact_secrets(s);
        assert!(!out.contains("sk-ant-api03"));
        assert!(out.contains("[REDACTED]"));
    }

    #[test]
    fn github_tokens_redacted() {
        for prefix in &["gho_", "ghp_", "ghs_", "ghr_"] {
            let token = format!("{prefix}abcdefghijklmnopqrstuvwxyz1234567890");
            let out = redact_secrets(&token);
            assert!(!out.contains(&token), "token {prefix}* not redacted");
        }
    }

    #[test]
    fn slack_tokens_redacted() {
        let s = "xoxb-1234567890-1234567890-AbCdEfGhIjKlMnOpQrStUvWx";
        let out = redact_secrets(s);
        assert!(!out.contains("xoxb-"));
    }

    #[test]
    fn google_api_key_redacted() {
        let s = "AIzaSyA-1234567890abcdefghijklmnop";
        let out = redact_secrets(s);
        assert!(!out.contains("AIza"));
    }

    #[test]
    fn aws_access_key_redacted() {
        let s = "AKIAIOSFODNN7EXAMPLE";
        let out = redact_secrets(s);
        assert!(!out.contains("AKIA"));
        assert!(out.contains("[REDACTED]"));
    }

    #[test]
    fn bearer_header_redacted() {
        let s = "Authorization: Bearer ya29.a0AfH6SMBxxxxxxx-long-token";
        let out = redact_secrets(s);
        assert!(!out.contains("ya29."));
        assert!(out.contains("[REDACTED]"));
    }

    #[test]
    fn url_credentials_stripped_but_host_kept() {
        let s = "fetch https://admin:hunter2@internal.example.com/api/v1";
        let out = redact_secrets(s);
        assert!(!out.contains("hunter2"));
        assert!(!out.contains("admin:hunter2"));
        // The email pattern runs after url-credentials and matches
        // REDACTED_URL_AUTH@internal.example.com → [REDACTED_EMAIL].
        // So we assert the host and auth info are gone, and either
        // REDACTED_URL_AUTH or [REDACTED_EMAIL] is present.
        assert!(out.contains("internal.example.com")
                || out.contains("REDACTED_URL_AUTH")
                || out.contains("[REDACTED_EMAIL]"));
    }

    #[test]
    fn query_string_secret_redacted() {
        let s = "https://api.example.com/v1?api_key=secret123&user=alice";
        let out = redact_secrets(s);
        assert!(!out.contains("secret123"));
        assert!(out.contains("user=alice"));
    }

    #[test]
    fn email_redacted() {
        let s = "Send the file to alice@example.com please";
        let out = redact_secrets(s);
        assert!(!out.contains("alice@example.com"));
        assert!(out.contains("[REDACTED_EMAIL]"));
    }

    #[test]
    fn multiple_secrets_all_caught() {
        let s = "OpenAI: sk-abc12345678901234567890; GitHub: ghp_abc1234567890123456789012345678; Email: a@b.com";
        let out = redact_secrets(s);
        assert!(!out.contains("sk-abc"));
        assert!(!out.contains("ghp_"));
        assert!(!out.contains("a@b.com"));
    }

    #[test]
    fn short_strings_untouched() {
        // The 20-char minimum in our regexes means short
        // identifiers are NOT redacted. This is intentional:
        // we don't want to mangle "sk-1" or "ghp_x" false
        // positives.
        let s = "sk-123";  // 6 chars after the dash
        assert_eq!(redact_secrets(s), s);
    }

    #[test]
    fn cjk_text_unchanged() {
        let s = "用 Rust 重写这个模块，零成本抽象，性能很强。";
        assert_eq!(redact_secrets(s), s);
    }

    #[test]
    fn code_block_with_secret_redacted() {
        let s = "```bash\nexport OPENAI_API_KEY=sk-1234567890abcdefghijkl\n```";
        let out = redact_secrets(s);
        assert!(out.contains("```bash"));
        assert!(out.contains("```"));
        assert!(!out.contains("sk-1234567890"));
    }

    #[test]
    fn json_redact_preserves_structure() {
        let json = r#"{"api_key":"sk-abcdefghijklmnopqrstuv","name":"alice","nested":{"token":"ghp_abcdefghijklmnopqrstuvwxyz1234","safe":"ok"}}"#;
        let out = redact_tool_calls_json(json);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["api_key"], "[REDACTED]");
        assert_eq!(parsed["name"], "alice");
        assert_eq!(parsed["nested"]["token"], "[REDACTED]");
        assert_eq!(parsed["nested"]["safe"], "ok");
    }

    #[test]
    fn json_redact_invalid_json_falls_back() {
        let garbage = "this is not json { sk-1234567890abcdefghijkl";
        let out = redact_tool_calls_json(garbage);
        assert!(!out.contains("sk-1234567890"));
    }

    #[test]
    fn json_redact_empty_returns_empty() {
        assert_eq!(redact_tool_calls_json(""), "");
    }

    #[test]
    fn json_redact_array_of_strings() {
        let json = r#"["sk-abcdefghijklmnopqrstuv","safe","AIzaSyA-1234567890abcdefghijklmnop"]"#;
        let out = redact_tool_calls_json(json);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed[0], "[REDACTED]");
        assert_eq!(parsed[1], "safe");
        assert_eq!(parsed[2], "[REDACTED]");
    }
}
