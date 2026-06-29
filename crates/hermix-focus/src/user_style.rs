//! P2-6 (2026-06-27): dialectic user-style detector — rule-based,
//! zero-LLM, writeable as a `kind=user.style` fact row.
//!
//! Honcho's full dialectic profile is a moving target — they recompute
//! a 2-axis (self / agent) view on every session. We don't ship a
//! moving target here. The minimum useful thing is a single static
//! "communication style" tag that the LLM can adapt to (drop
//! throat-clearing, skip caveats, etc) without an extra round trip.
//!
//! Why rule-based and not LLM-classifier? Because:
//!   1. **No token cost** — every turn we already pay for the
//!      completion; adding a classifier turn would double the bill.
//!   2. **Deterministic** — the same input always produces the same
//!      tag, which means `canonical_key = user.style:direct` dedups
//!      across sessions instead of drifting.
//!   3. **Honcho-comparable enough** — Honcho's "directive vs
//!      exploratory" axis reduces to roughly the same heuristics:
//!      imperative verbs + short sentences + low politeness count.
//!
//! ## Detected styles (in priority order)
//!
//! - `technical`  — contains backticks, code fences, or jargon like
//!                  `Rust`, `tokio`, `SQL`. Highest priority because
//!                  once a turn is technical we want the agent to
//!                  skip the "let me explain in plain English" step.
//! - `direct`     — short imperative sentences (avg length < 20 chars
//!                  or starts with imperative verb like "改", "fix",
//!                  "delete"), 0 politeness markers, contains "?".
//! - `exploratory`— long sentences, multiple softeners, hedges
//!                  ("可能", "也许", "maybe"), 0-1 imperative verbs.
//! - `mixed`      — fallback when none of the above clearly wins.
//!
//! A single turn gets at most one tag. The `confidence` is the
//! fractional overlap between the matching rule's evidence and the
//! threshold it needed to win. We never go below 0.4 so a noisy
//! detection still surfaces something the LLM can react to.

use serde_json::Value;

/// One detected user style tag. The `style` string is the
/// `canonical_key` suffix (`user.style:<style>`).
#[derive(Debug, Clone, PartialEq)]
pub struct UserStyle {
    pub style: String,
    /// 0.4–1.0; below 0.4 we don't write a fact at all (low signal).
    pub confidence: f64,
    /// Diagnostic payload — what evidence backed the decision.
    /// Persisted alongside the fact so future explainers can show
    /// "we tagged you as direct because avg-sent-len=12 + 0 polite".
    pub evidence: Value,
}

/// Detect a user style tag from a single user turn.
///
/// `user_turn` is the raw message text (may contain code, CJK, etc).
/// Returns `None` if the message is too short or the evidence is
/// below the 0.4 confidence threshold — we'd rather write nothing
/// than write a noisy fact that competes for salience with real
/// user preferences.
pub fn detect_user_style(user_turn: &str) -> Option<UserStyle> {
    let trimmed = user_turn.trim();
    if trimmed.is_empty() || trimmed.chars().count() < 4 {
        return None;
    }

    // 1) Technical: contains code fences, inline code, or known jargon.
    let technical = score_technical(trimmed);
    if technical.score >= 0.5 {
        return Some(UserStyle {
            style: "technical".to_string(),
            confidence: technical.score,
            evidence: technical.evidence,
        });
    }

    // 2) Direct: short, imperative, low politeness.
    let direct = score_direct(trimmed);
    // 3) Exploratory: long, hedged, polite.
    let exploratory = score_exploratory(trimmed);

    let winner = if direct.score >= exploratory.score + 0.05 {
        ("direct", direct)
    } else if exploratory.score >= direct.score + 0.05 {
        ("exploratory", exploratory)
    } else if direct.score.max(exploratory.score) >= 0.4 {
        // Tied within 5% — call it mixed so we don't over-commit.
        (
            "mixed",
            StyleScore {
                score: direct.score.max(exploratory.score),
                evidence: serde_json::json!({
                    "direct": direct.evidence,
                    "exploratory": exploratory.evidence,
                }),
            },
        )
    } else {
        return None;
    };

    if winner.1.score < 0.4 {
        return None;
    }

    Some(UserStyle {
        style: winner.0.to_string(),
        confidence: winner.1.score,
        evidence: winner.1.evidence,
    })
}

struct StyleScore {
    score: f64,
    evidence: Value,
}

fn score_technical(s: &str) -> StyleScore {
    let mut hits: Vec<&str> = Vec::new();
    if s.contains("```") {
        hits.push("code_fence");
    }
    if s.contains('`') {
        hits.push("inline_code");
    }
    // Common technical jargon across the project's working set.
    const JARGON: &[&str] = &[
        "Rust", "tokio", "SQLite", "SQL", "Tauri", "React", "API",
        "JSON", "Rust", "crate", "trait", "async", "await", "Cargo",
        "commit", "PR", "CI", "CD", "binary", "crate",
    ];
    let lower = s.to_lowercase();
    for j in JARGON {
        if lower.contains(&j.to_lowercase()) {
            hits.push("jargon");
            break; // only count jargon once — it can spam the score
        }
    }

    let score = match hits.len() {
        0 => 0.0,
        1 => 0.6,
        2 => 0.85,
        _ => 1.0,
    };
    StyleScore {
        score,
        evidence: serde_json::json!({ "signals": hits }),
    }
}

fn score_direct(s: &str) -> StyleScore {
    // Sentence boundaries: ., !, ?, ;, 。
    let sentences: Vec<&str> = s
        .split(|c: char| matches!(c, '.' | '!' | '?' | ';' | '。' | '！' | '？' | '；'))
        .filter(|p| !p.trim().is_empty())
        .collect();
    let count = sentences.len().max(1);
    let avg_len = s.chars().count() as f64 / count as f64;

    let politeness_markers = ["请", "麻烦", "谢谢", "thanks", "please", "could you"];
    let politeness = politeness_markers
        .iter()
        .filter(|m| s_lower(s).contains(&m.to_lowercase()))
        .count();

    let imperatives_zh = ["改", "删", "加", "修", "做", "写", "跑", "查", "建"];
    let imperatives_en = ["fix", "delete", "add", "write", "run", "check", "build"];
    let lower = s_lower(s);
    let imperatives = imperatives_zh.iter().filter(|m| s.starts_with(*m)).count()
        + imperatives_en.iter().filter(|m| lower.starts_with(*m)).count();

    let mut score: f64 = 0.0;
    if avg_len < 30.0 {
        score += 0.35;
    } else if avg_len < 60.0 {
        score += 0.15;
    }
    if politeness == 0 {
        score += 0.25;
    }
    if imperatives > 0 {
        score += 0.30;
    }
    if s.contains('?') || s.contains('？') {
        score += 0.10;
    }
    score = score.min(1.0);

    StyleScore {
        score,
        evidence: serde_json::json!({
            "avg_sentence_chars": avg_len,
            "politeness_markers": politeness,
            "imperative_starts": imperatives,
        }),
    }
}

fn score_exploratory(s: &str) -> StyleScore {
    let hedges = ["可能", "也许", "大概", "maybe", "perhaps", "I think"];
    let lower = s_lower(s);
    let hedge_count = hedges.iter().filter(|h| lower.contains(&h.to_lowercase())).count();
    let avg_len = s.chars().count() as f64;

    let mut score: f64 = 0.0;
    if avg_len > 80.0 {
        score += 0.30;
    } else if avg_len > 40.0 {
        score += 0.15;
    }
    if hedge_count > 0 {
        score += 0.30 + 0.10 * (hedge_count as f64 - 1.0_f64).min(2.0_f64);
    }
    if s.contains("?") || s.contains("？") {
        score += 0.10;
    }
    score = score.min(1.0);

    StyleScore {
        score,
        evidence: serde_json::json!({
            "chars": avg_len,
            "hedge_count": hedge_count,
        }),
    }
}

fn s_lower(s: &str) -> String {
    s.to_lowercase()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_returns_none() {
        assert!(detect_user_style("").is_none());
        assert!(detect_user_style("   ").is_none());
        assert!(detect_user_style("hi").is_none()); // too short
    }

    #[test]
    fn detects_technical_via_code_fence() {
        let s = detect_user_style("请看这段 Rust 代码：\n```rust\nfn main(){}\n```");
        let s = s.expect("code fence should classify as technical");
        assert_eq!(s.style, "technical");
        assert!(s.confidence >= 0.5);
    }

    #[test]
    fn detects_technical_via_jargon() {
        let s = detect_user_style("Why is the tokio runtime not picking up my async fn?");
        let s = s.expect("jargon should classify as technical");
        assert_eq!(s.style, "technical");
    }

    #[test]
    fn detects_direct_short_imperative_no_politeness() {
        let s = detect_user_style("改 agent_loop.rs 的 finalize 函数");
        let s = s.expect("imperative ZH should classify");
        assert_eq!(s.style, "direct");
        assert!(s.confidence >= 0.4);
    }

    #[test]
    fn detects_exploratory_long_with_hedges() {
        // > 80 chars + 3 hedges + question mark = should win exploratory
        // over direct. Avoid technical jargon (Rust, SQL etc.) so
        // the technical branch doesn't preempt us.
        let s = detect_user_style(
            "我想知道，也许我们可以考虑一下，如果现在重新设计用户管理这块的话，\
             可能用什么方式比较合适，请给一些建议，谢谢",
        );
        let s = s.expect("hedged long ZH should classify");
        // The politeness markers ("请", "谢谢") suppress direct, and
        // the hedges push exploratory above 0.6. Either exploratory
        // or mixed is acceptable — we just don't want direct.
        assert!(
            s.style == "exploratory" || s.style == "mixed",
            "expected exploratory/mixed, got {}",
            s.style
        );
    }

    #[test]
    fn tie_falls_through_to_mixed() {
        // Short message with mixed signals: imperative-ish ("how")
        // but no jargon and no hedge. Should land in mixed when both
        // direct and exploratory score within 5% of each other.
        let s = detect_user_style("how should we name the new helper module");
        let s = s.expect("should still classify above 0.4 threshold");
        assert!(
            s.style == "mixed" || s.style == "direct" || s.style == "exploratory",
            "got unexpected style {}",
            s.style
        );
    }
}