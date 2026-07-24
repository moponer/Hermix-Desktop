//! Budget enforcement for tool results.
//!
//! Ensures that tool output doesn't overwhelm the context window by
//! truncating individual results and enforcing aggregate size limits.

use hermix_core::{BudgetConfig, ToolResult};

/// Truncate a single result string to `max_chars`.
/// /// Counts **characters** (not bytes) so that CJK / emoji payloads are
/// safe — `content.len() <= max_chars` and `&content[..max_chars]` would
/// panic on multi-byte boundaries. If the content exceeds `max_chars`,
/// it is truncated and a sentinel message is appended indicating how
/// many characters were removed.
pub fn truncate_result(content: &str, max_chars: usize) -> String {
    if content.chars().count() <= max_chars {
        return content.to_string();
    }
    // char_indices yields (byte_offset, char); the (max_chars)th entry's
    // byte_offset is the safe slice end.
    let end_byte = content
        .char_indices()
        .nth(max_chars)
        .map(|(byte, _)| byte)
        .unwrap_or(content.len());
    let truncated = &content[..end_byte];
    let removed = content.chars().count() - max_chars;
    format!("{truncated}\n\n[... truncated {removed} characters ...]")
}

/// Enforce budget on a batch of tool results.
/// /// - Each result is individually truncated to `budget.max_result_size_chars`.
/// - If the aggregate size exceeds `budget.max_aggregate_chars`, results are
/// further truncated proportionally until the total fits.
pub fn enforce_budget(results: &mut [ToolResult], budget: &BudgetConfig) {
    // Step 1: Per-result truncation
    for result in results.iter_mut() {
        result.content = truncate_result(&result.content, budget.max_result_size_chars);
    }

    // Step 2: Aggregate truncation (also char-safe; ratio applied to char count).
    let total_chars: usize = results.iter().map(|r| r.content.chars().count()).sum();
    if total_chars <= budget.max_aggregate_chars {
        return;
    }

    let ratio = budget.max_aggregate_chars as f64 / total_chars as f64;

    for result in results.iter_mut() {
        let target_len = ((result.content.chars().count() as f64) * ratio) as usize;
        result.content = truncate_result(&result.content, target_len.max(200));
    }
}

/// Check whether the aggregate size of tool results is within budget.
/// /// Returns `true` if the total character count of all results does not
/// exceed `max_aggregate`.
pub fn check_aggregate_budget(results: &[ToolResult], max_aggregate: usize) -> bool {
    let total: usize = results.iter().map(|r| r.content.chars().count()).sum();
    total <= max_aggregate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_result_under_limit() {
        let content = "hello world";
        let truncated = truncate_result(content, 100);
        assert_eq!(truncated, "hello world");
    }

    #[test]
    fn test_truncate_result_over_limit() {
        let content = "a".repeat(200);
        let truncated = truncate_result(&content, 100);
        assert!(truncated.len() < 300); // original + suffix
        assert!(truncated.contains("truncated"));
        assert!(truncated.starts_with(&"a".repeat(100)));
    }

    #[test]
    fn test_truncate_result_cjk_does_not_panic() {
        // Each Chinese char is 3 bytes in UTF-8. Truncating at byte offset
        // 100 used to panic (mid-character). Now we count chars so it's safe.
        let content: String = "中".repeat(200); // 600 bytes, 200 chars
        let truncated = truncate_result(&content, 100);
        assert_eq!(truncated.chars().filter(|c| *c == '中').count(), 100);
        assert!(truncated.contains("truncated 100 characters"));
    }

    #[test]
    fn test_truncate_result_emoji_does_not_panic() {
        // 4-byte chars. Old code would have panicked.
        let content: String = "🎉".repeat(50);
        let truncated = truncate_result(&content, 10);
        assert_eq!(truncated.chars().filter(|c| *c == '🎉').count(), 10);
    }

    #[test]
    fn test_enforce_budget_aggregate_cjk() {
        // Make sure aggregate path also handles multi-byte correctly.
        let mut results = vec![
            ToolResult::ok("1", "文".repeat(500)),
            ToolResult::ok("2", "字".repeat(500)),
        ];
        let budget = BudgetConfig {
            max_result_size_chars: 1_000_000,
            max_aggregate_chars: 600, // forces aggregate truncation
        };
        enforce_budget(&mut results, &budget);
        // Total chars after enforce should be ≤ 600 (slight overshoot from
        // footer) and no panic on byte-vs-char mismatch.
        let total_chars: usize = results.iter().map(|r| r.content.chars().count()).sum();
        assert!(total_chars <= 700, "aggregate overshoot too large: {total_chars}");
    }

    #[test]
    fn test_enforce_budget_per_result() {
        let mut results = vec![
            ToolResult::ok("1", "x".repeat(500)),
            ToolResult::ok("2", "y".repeat(200)),
        ];
        let budget = BudgetConfig {
            max_result_size_chars: 300,
            max_aggregate_chars: 1_000_000,
        };
        enforce_budget(&mut results, &budget);
        assert!(results[0].content.len() <= 350);
        assert_eq!(results[1].content.len(), 200);
    }

    #[test]
    fn test_check_aggregate_budget_pass() {
        let results = vec![ToolResult::ok("1", "hello"), ToolResult::ok("2", "world")];
        assert!(check_aggregate_budget(&results, 100));
    }

    #[test]
    fn test_check_aggregate_budget_fail() {
        let results = vec![ToolResult::ok("1", "x".repeat(500))];
        assert!(!check_aggregate_budget(&results, 100));
    }
}
