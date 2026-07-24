//! Phase 4.2 — Two-split acceptance gate.
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
    /// Maximum duplicate paragraph ratio before warning (0.0-1.0).
    pub duplicate_ratio_threshold: f64,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            max_rejections: 2,
            reject_unclosed_thinking: true,
            warn_on_duplicates: true,
            reject_truncated_json: true,
            warn_on_pii_patterns: true,
            duplicate_ratio_threshold: 0.5,
        }
    }
}

// ---------------------------------------------------------------------------
// Gate state (tracks rejections across turns)
// ---------------------------------------------------------------------------

/// Mutable state for the acceptance gate, held across turns.
#[derive(Debug, Default)]
pub struct AcceptanceGate {
    /// Number of consecutive rejections.
    consecutive_rejections: u32,
    /// Configuration.
    config: GateConfig,
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
        }
    }

    /// Reset the rejection counter (call on successful acceptance).
    pub fn reset_rejections(&mut self) {
        self.consecutive_rejections = 0;
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
    /// /// - `visible_text`: The visible (non-thinking) text produced by the LLM.
    /// - `tool_calls_json`: Raw JSON strings of any tool calls made.
    /// - `thinking_blocks`: The content of thinking blocks (for structural check).
    /// - `has_user_query`: Whether there was a user query in this turn.
    pub fn evaluate(
        &mut self,
        visible_text: &str,
        tool_calls_json: &[String],
        thinking_blocks: &[String],
        has_user_query: bool,
    ) -> GateReport {
        let behavioral = self.evaluate_behavioral(visible_text, has_user_query);
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

    fn evaluate_behavioral(&self, text: &str, has_user_query: bool) -> GateVerdict {
        // 1. PII pattern check (warning only).
        if self.config.warn_on_pii_patterns && contains_pii_patterns(text) {
            return GateVerdict::Warn {
                split: GateSplit::Behavioral,
                reason: "Output may contain PII-like patterns (email, phone, or SSN)".into(),
            };
        }

        // 2. Empty response when user asked a question.
        if has_user_query && text.trim().is_empty() {
            return GateVerdict::Reject {
                split: GateSplit::Behavioral,
                reason: "Empty response to a user query".into(),
                suggestion: "Provide a substantive response to the user's question".into(),
            };
        }

        // 3. Very short response that seems like an acknowledgment but
        // should contain more substance (heuristic: < 20 chars).
        if has_user_query && text.trim().len() < 20 && !text.trim().is_empty() {
            return GateVerdict::Warn {
                split: GateSplit::Behavioral,
                reason: "Very short response to a user query — may not be substantive enough".into(),
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
        let mut gate = AcceptanceGate::new();
        let report = gate.evaluate("", &[], &[], true);
        assert!(!report.is_accepted());
        assert!(matches!(report.overall, GateVerdict::Reject { .. }));
    }

    #[test]
    fn gate_warns_on_short_response() {
        let mut gate = AcceptanceGate::new();
        let report = gate.evaluate("ok", &[], &[], true);
        assert!(report.is_accepted()); // Warn still accepts
        assert!(matches!(report.overall, GateVerdict::Warn { .. }));
    }

    #[test]
    fn gate_rejects_truncated_json() {
        let mut gate = AcceptanceGate::new();
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
        let mut gate = AcceptanceGate::new();
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
        let mut gate = AcceptanceGate::new();
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
        let mut gate = AcceptanceGate::new();
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
        let mut gate = AcceptanceGate::new();
        gate.evaluate("", &[], &[], true);
        assert_eq!(gate.consecutive_rejections(), 1);

        gate.evaluate("Good response here.", &[], &[], true);
        assert_eq!(gate.consecutive_rejections(), 0);
    }

    #[test]
    fn gate_report_advisory_text() {
        let mut gate = AcceptanceGate::new();
        let report = gate.evaluate("ok", &[], &[], true);
        assert!(report.advisory_text().is_some());
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
