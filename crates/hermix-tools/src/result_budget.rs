//! Central result budgets (ZCode declarative `resultBudget` analog).
//!
//! Every tool result eventually becomes model context, so the size caps are a
//! **product decision**, not an implementation detail. They used to be
//! scattered literals (`300_000` in `file.rs`, two separate `100_000`s in
//! `terminal.rs`, `50_000` in `pdf.rs`, `20_000` in the desktop snapshot).
//! Keeping them here means:
//!  - one place to answer "how big can a tool result get?";
//!  - the numbers are reviewable in a diff without hunting 40 files;
//!  - per-tool *semantic* limits (rows, depth, node counts) stay with their
//!    tool — only the **character budget handed to the model** lives here.
//!
//! The registry-level `max_result_size_chars` remains the outer safety net
//! (default [`crate::registry::DEFAULT_MAX_RESULT_CHARS`], overridable per
//! registration); these constants govern the point where a tool decides to
//! stop *building* a result, which is earlier and cheaper than truncating
//! afterwards.

/// `read_file` body cap.
///
/// 80 K chars ≈ 20 K tokens. It used to be 300 K (≈ 75 K tokens): one read of
/// a large lockfile or minified bundle could put a 128 K-token window at the
/// compression threshold by itself, and the read that did it was invisible to
/// the model's own planning. `read_file` pages (`next_offset`), so a big file
/// costs one extra call instead of a third of the window — the paging path is
/// cheaper than the truncation path was.
pub const READ_FILE_CHARS: usize = 80_000;

/// `terminal` output cap (stdout+stderr combined) — 60 K chars ≈ 15 K tokens.
/// Commands that dump more should be piped through `Select-String`/`head`; the
/// truncation note says so, and the head/tail split keeps a build failure's
/// final status visible. Lowered from 100 K for the same reason as
/// [`READ_FILE_CHARS`]: one result should not own a sixth of the window.
pub const TERMINAL_OUTPUT_CHARS: usize = 60_000;

/// `pdf` extracted-text cap per call.
pub const PDF_TEXT_CHARS: usize = 50_000;

/// Desktop automation snapshot cap (`win_*` element dumps).
pub const DESKTOP_SNAPSHOT_CHARS: usize = 20_000;

// ---------------------------------------------------------------------------
// Per-tool *model-visible* caps (what actually enters the request)
// ---------------------------------------------------------------------------
//
// 上面那几个常量管的是「工具造结果时最多造多长」；这一节管的是「进上下文
// 之前最多留多长」。两者不是一回事：`read_file` 能造 80 K，而请求里只允许
// 16 K，中间那层中间截断是**有损**的。
//
// 一张表切所有工具会两头出错：`terminal` 掐太狠会丢掉失败状态的尾巴，
// `list_files` 这种「一次列一整棵树」的工具则会稳定顶到上限（2026-10-30
// 实测：5 次调用 43.5 K 字符，均值 8.7 K）。分档的意义是让单个工具的输出
// 与它承载的信息量匹配。

/// 未单独声明的工具的默认上限。
pub const MODEL_VISIBLE_DEFAULT_CHARS: usize = 16_000;

/// 某条工具结果进上下文时允许保留的字符上限。
///
/// 调用方还有自己的全局上限（`BudgetConfig::max_result_size_chars`），
/// 两者取小 —— 用户收紧配置时这个表不能把它放宽。
pub fn model_visible_cap(tool: &str) -> usize {
    match tool {
        // 分页工具：页大小必须等于本上限，否则 agent 层的中间截断会把
        // 「下一页从哪开始」提示之外的正文整段吞掉（模型以为读完了）。
        "read_file" => 16_000,
        // 长输出类：头尾各半地保留，构建失败的最后几行一定在尾巴上。
        "terminal" | "execute_code" | "code_execution" | "shell" => 16_000,
        // 检索 / 列举类：输出可再生（重跑一次即可），不值得整棵树进上下文。
        "list_files" | "list_dir" | "glob" => 3_000,
        "search_files" | "grep" => 4_000,
        // 元工具：结果本身就是目录，按需再查。`list_tools` 例外地给足：
        // 工具全量清单 ≈12K 字符（81 工具实测），6K 会把字母序尾段
        // （terminal…web_search…）再次截掉——模型据此认定"没有 web 工具"
        // （2026-11-10 实测，与 limit 无 offset 是同族坑）。
        "tool_search" | "tool_describe" => 6_000,
        "list_tools" | "tool_call" => 16_000,
        "skills_list" | "skill_manage" => 6_000,
        // `skill_view` 是**全文读取通道**（自动匹配的大 skill 只给指针，靠它
        // 取正文）。这一档给足：压到 6 K 会让 33 K 的技能永远读不全，而那
        // 正是它存在的理由。超出 16 K 的部分由 spill 提供「预览 + 分页指针」。
        "skill_view" => 16_000,
        // 子代理：约定是回结论，超过这个长度说明它没按约定收敛。
        "delegate_task" | "get_subagent_result" => 12_000,
        _ => MODEL_VISIBLE_DEFAULT_CHARS,
    }
}

// ---------------------------------------------------------------------------
// Old-result reclaim (ZCode `microcompact` analog)
// ---------------------------------------------------------------------------
//
// 「发过即冻结」意味着工具结果一旦进历史就永远要重发。要控体积只剩一条路：
// 在**用户消息边界**做一次性回收，把最旧的一批换成一行占位。回收会改写历史
// → 一次全量缓存失效，所以它必须是高水位 + 批量的低频事件，不能做成滑窗
// （滑窗会让每次请求都改写一次 = 缓存永久为零，这正是 2026-10-29 拆掉的
// `rollup_tool_results` 的错误形状）。

/// 触发线：历史里工具结果合计超过它，才开始回收。
pub const RECLAIM_TRIGGER_CHARS: usize = 150_000;

/// 回收后保留最近多少条工具结果原文。
pub const RECLAIM_KEEP_RESULTS: usize = 40;

/// 小于这个长度的结果不值得回收（占位符本身就要几十个字符）。
pub const RECLAIM_MIN_RESULT_CHARS: usize = 1_500;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budgets_are_ordered_by_expected_payload() {
        // The ordering encodes intent: file bodies may be biggest, desktop
        // snapshots must stay small (they are re-taken frequently).
        assert!(READ_FILE_CHARS > TERMINAL_OUTPUT_CHARS);
        assert!(TERMINAL_OUTPUT_CHARS > PDF_TEXT_CHARS);
        assert!(PDF_TEXT_CHARS > DESKTOP_SNAPSHOT_CHARS);
        assert!(DESKTOP_SNAPSHOT_CHARS >= 10_000);
    }

    #[test]
    fn the_registry_default_is_the_outer_safety_net() {
        // The registry cap must not preempt a per-tool budget: when it
        // did (50 000 vs `terminal` 60 000 / `read_file` 80 000) the
        // registry's unrecoverable middle-cut fired first and the
        // spill-backed per-tool caps never bound anything.
        //
        // It compares against the **real** default now. The previous
        // version asserted against a locally written `REGISTRY_DEFAULT =
        // 50_000`, i.e. a constant that had silently drifted from the
        // implementation: it stayed green after the default moved to
        // 80 000 and could never catch a regression.
        const REGISTRY_DEFAULT: usize = crate::registry::DEFAULT_MAX_RESULT_CHARS;
        assert!(
            REGISTRY_DEFAULT >= READ_FILE_CHARS,
            "注册表默认 {REGISTRY_DEFAULT} 低于 read_file 预算 {READ_FILE_CHARS}"
        );
        assert!(
            REGISTRY_DEFAULT >= TERMINAL_OUTPUT_CHARS,
            "注册表默认 {REGISTRY_DEFAULT} 低于 terminal 预算 {TERMINAL_OUTPUT_CHARS}"
        );
        // `pdf` deliberately keeps its own, smaller cap — it stops
        // building earlier, which the registry must not override.
        assert!(PDF_TEXT_CHARS <= READ_FILE_CHARS);
    }
}
