//! Budget enforcement for tool results.
//!
//! Ensures that tool output doesn't overwhelm the context window by
//! truncating individual results and enforcing aggregate size limits.

use hermix_core::{BudgetConfig, ToolCall, ToolResult};

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

/// 一条结果进上下文时允许保留的字符上限。
///
/// - 有工具名 → [`hermix_tools::result_budget::model_visible_cap`]（分档表）
///   与配置里的全局上限取小；
/// - 无工具名（测试、一次性调用）→ 直接沿用全局上限，保持旧语义。
///
/// 分档的意义：全局一刀切要么把 `terminal` 的失败尾巴掐掉，要么放任
/// `list_files` 这种「一次列一整棵树」的工具稳定顶到上限 —— 实测该工具
/// 5 次调用产出 43.5 K 字符（均值 8.7 K），而它重跑一次的成本几乎为零。
fn cap_for(tool: Option<&str>, budget: &BudgetConfig) -> usize {
    match tool {
        Some(name) => {
            hermix_tools::result_budget::model_visible_cap(name)
                .min(budget.max_result_size_chars)
        }
        None => budget.max_result_size_chars,
    }
}

fn tool_name_of<'a>(tool_calls: &'a [ToolCall], tool_call_id: &str) -> Option<&'a str> {
    tool_calls
        .iter()
        .find(|tc| tc.id == tool_call_id)
        .map(|tc| tc.function.name.as_str())
}

/// Enforce budget on a batch of tool results.
/// - Each result is individually truncated to its per-tool cap (falling back to
///   `budget.max_result_size_chars`).
/// - If the aggregate size exceeds `budget.max_aggregate_chars`, results are
///   further truncated proportionally until the total fits.
pub fn enforce_budget(results: &mut [ToolResult], budget: &BudgetConfig) {
    enforce_budget_with_spill(results, budget, None, &[]);
}

/// Same as [`enforce_budget`], but oversized results are written to disk
/// first (see [`crate::spill`]) so the model gets a retrievable path
/// instead of a lossy "truncated N characters" marker.
///
/// Spilling happens **before** truncation: once the content is replaced by
/// "preview + locator" it is already under the inline ceiling, so the
/// truncation passes below become no-ops for spilled results.
///
/// `tool_calls` 只用于把 `tool_call_id` 映射到工具名（分档表要按工具名查）；
/// 传空切片等价于「全部走全局上限」。
pub fn enforce_budget_with_spill(
    results: &mut [ToolResult],
    budget: &BudgetConfig,
    spill: Option<&crate::spill::SpillSink>,
    tool_calls: &[ToolCall],
) {
    let cap_of = |tool_call_id: &str| cap_for(tool_name_of(tool_calls, tool_call_id), budget);

    // Step 0: spill-to-disk（可选）。未配置 sink 时行为与旧路径完全一致。
    //
    // ★ Skip results that already carry a spill pointer. The agent
    // layer spills twice (`enforce_budget_with_spill` here, then
    // `push_tool_result`); the second spill used the same
    // `tool_call_id` and therefore **overwrote the full result with
    // its own preview** — so the pointer the model was handed led
    // back to the preview instead of the missing bytes. Spilling once
    // is enough.
    //
    // ★ 阈值取「本工具的上限」而不是固定的 20 K：既然超出上限的中间部分
    // 反正会被截掉，就不如直接落盘给指针 —— 模型丢了正文但拿得到取回路径。
    if let Some(sink) = spill {
        for result in results.iter_mut() {
            if crate::spill::is_spilled(&result.content) {
                continue;
            }
            let threshold = cap_of(&result.tool_call_id).min(crate::spill::SPILL_INLINE_CHARS);
            result.content =
                sink.maybe_spill(&result.tool_call_id, &result.content, threshold);
        }
    }

    // Step 1: Per-result truncation.
    //
    // ★ 落盘过的结果跳过：它的正文已经换成「预览 + 指针」，中间截断会把
    // 指针（在尾巴上）一起切掉，模型就只剩一段没有出处的片段。预览本身
    // 已经按本工具的上限裁过，超出部分只是指针文案的几十个字符。
    for result in results.iter_mut() {
        if crate::spill::is_spilled(&result.content) {
            continue;
        }
        let cap = cap_of(&result.tool_call_id);
        result.content = truncate_result(&result.content, cap);
    }

    // Step 2: Aggregate truncation (also char-safe; ratio applied to char count).
    let total_chars: usize = results.iter().map(|r| r.content.chars().count()).sum();
    if total_chars <= budget.max_aggregate_chars {
        return;
    }

    let ratio = budget.max_aggregate_chars as f64 / total_chars as f64;

    // 聚合裁剪也要先落盘：这一刀是「本轮的多数结果都合格、但加起来超标」，
    // 纯截断会把中间段丢成不可取回（注册表那层注释抱怨的正是这种丢法）。
    // 已经带指针的结果不能再动，否则指针会被切掉。
    for result in results.iter_mut() {
        if crate::spill::is_spilled(&result.content) {
            continue;
        }
        let target_len = ((result.content.chars().count() as f64) * ratio) as usize;
        let target_len = target_len.max(200);
        result.content = match spill {
            Some(sink) => sink.maybe_spill(&result.tool_call_id, &result.content, target_len),
            None => truncate_result(&result.content, target_len),
        };
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

    /// ★ Regression: the agent layer spills at two points (here and in
    /// `push_tool_result`). The second one used the same
    /// `tool_call_id` and overwrote the full result **with its own
    /// preview** — so the path the model was told to read back led to
    /// the preview, and the missing bytes were gone.
    ///
    /// The file must still hold the **complete** result after the
    /// budget pass.
    #[test]
    fn spill_is_not_repeated_so_the_file_keeps_the_full_result() {
        let dir = std::env::temp_dir().join("hermix-budget-spill-test");
        let _ = std::fs::remove_dir_all(&dir);
        let sink = crate::spill::SpillSink::new(dir.clone());

        // 5,000 lines ≈ 48K chars — comfortably over the 20K inline
        // budget, so the spill actually fires.
        let full: String = (0..5_000).map(|i| format!("line-{i}\n")).collect();
        let mut results = vec![ToolResult::ok("call_1", full.clone())];
        let budget = BudgetConfig {
            max_result_size_chars: 1_000_000,
            max_aggregate_chars: 1_000_000,
        };
        enforce_budget_with_spill(&mut results, &budget, Some(&sink), &[]);

        // The context now carries a preview plus a pointer …
        assert!(
            results[0].content.len() < full.len(),
            "上下文里应该是预览而不是全文"
        );
        assert!(
            crate::spill::is_spilled(&results[0].content),
            "预览必须带取回指引"
        );

        // … and the file the pointer names holds the **full** result.
        let on_disk = std::fs::read_to_string(dir.join("call_1.txt")).unwrap();
        assert_eq!(
            on_disk, full,
            "落盘文件必须是完整结果，不能被预览覆盖"
        );
    }
}
