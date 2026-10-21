//! Budget enforcement for tool results.
//!
//! Ensures that tool output doesn't overwhelm the context window by
//! truncating individual results and enforcing aggregate size limits.

use hermix_core::{BudgetConfig, ToolResult};

/// Truncate a single result string to `max_chars`.
///
/// Delegates to [`hermix_tools::registry::truncate_middle_chars`] so there is
/// **one** truncation rule in the codebase: keep head *and* tail, char-safe.
/// The old head-only cut here discarded the tail — which for command output is
/// where the exit status, the failing assertion and the final error line live.
/// Same reasoning as the Codex `truncate_middle` contract.
///
/// Counts **characters** (not bytes) so CJK / emoji payloads cannot panic on a
/// multi-byte boundary.
pub fn truncate_result(content: &str, max_chars: usize) -> String {
    hermix_tools::registry::truncate_middle_chars(content, max_chars)
}

/// Enforce budget on a batch of tool results.
/// - Each result is individually truncated to `budget.max_result_size_chars`.
/// - If the aggregate size exceeds `budget.max_aggregate_chars`, results are
/// further truncated proportionally until the total fits.
pub fn enforce_budget(results: &mut [ToolResult], budget: &BudgetConfig) {
    enforce_budget_with_spill(results, budget, None);
}

/// Same as [`enforce_budget`], but oversized results are written to disk
/// first (see [`crate::spill`]) so the model gets a retrievable path
/// instead of a lossy "truncated N characters" marker.
///
/// Spilling happens **before** truncation: once the content is replaced by
/// "preview + locator" it is already under the inline ceiling, so the
/// truncation passes below become no-ops for spilled results.
pub fn enforce_budget_with_spill(
    results: &mut [ToolResult],
    budget: &BudgetConfig,
    spill: Option<&crate::spill::SpillSink>,
) {
    // Step 0: spill-to-disk（可选）。未配置 sink 时行为与旧路径完全一致。
    if let Some(sink) = spill {
        for result in results.iter_mut() {
            result.content =
                sink.maybe_spill(&result.tool_call_id, &result.content, crate::spill::SPILL_INLINE_CHARS);
        }
    }

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
/// Returns `true` if the total character count of all results does not
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
        // Head **and** tail survive: a pure head cut dropped the tail, which
        // is where command summaries, exit codes and stack frames live.
        assert!(truncated.starts_with("aaaaaaaa"));
        assert!(truncated.ends_with(&"a".repeat(20)));
    }

    #[test]
    fn test_truncate_result_keeps_the_tail() {
        let mut content = "x".repeat(10_000);
        content.push_str("FINAL: exit code 1");
        let truncated = truncate_result(&content, 1_000);
        assert!(
            truncated.contains("FINAL: exit code 1"),
            "the tail carries the verdict for command output"
        );
    }

    #[test]
    fn test_truncate_result_cjk_does_not_panic() {
        // Each Chinese char is 3 bytes in UTF-8. Truncating at a byte offset
        // used to panic (mid-character). Now we count chars so it's safe.
        let content: String = "中".repeat(200); // 600 bytes, 200 chars
        let truncated = truncate_result(&content, 100);
        assert!(truncated.contains('中'));
        assert!(truncated.contains("truncated"));
    }

    #[test]
    fn test_truncate_result_emoji_does_not_panic() {
        // 4-byte chars. Old code would have panicked.
        let content: String = "🎉".repeat(50);
        let truncated = truncate_result(&content, 10);
        assert!(truncated.starts_with('🎉'));
        assert!(truncated.contains("truncated"));
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
        // ≤ 600 plus the per-result truncation footer (the middle-truncation
        // marker is longer than the old head-only one: it names what was cut
        // and states that head and tail survived).
        let total_chars: usize = results.iter().map(|r| r.content.chars().count()).sum();
        assert!(total_chars <= 800, "aggregate overshoot too large: {total_chars}");
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
        // cap + marker; the marker is ~70 chars and must not be counted as
        // content the budget failed to enforce.
        assert!(
            results[0].content.chars().count() <= 400,
            "{}",
            results[0].content.chars().count()
        );
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
