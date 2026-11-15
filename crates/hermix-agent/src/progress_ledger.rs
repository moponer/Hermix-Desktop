//! 进展账本（L2）：以「世界状态有没有推进」为唯一判据的防空转机制。
//!
//! ## 为什么是这一层
//!
//! 之前所有守卫都是在**代理指标**上做文章，每一条都能被真实空转漏掉：
//!
//! | 守卫 | 判据 | 被什么漏掉 |
//! |---|---|---|
//! | `ErrorLoop` | 工具**报错** | 工具谎报成功（页面被踢到登录页仍返回 200） |
//! | `RepeatGuard` | 整批结果**逐字**相同 | 每轮换一段 JS → 结果逐字不同 |
//! | `SimilarContent` | 助手**正文**相似 | thinking 模型正文恒空 |
//! | 轮次上限 | 累计轮数 | 与「有没有推进」无关，误杀长任务 |
//!
//! 空转的定义只有一条，与工具、模型、任务类型都无关：
//!
//! > **循环在转，但世界状态没有变化。**
//!
//! 所以判据也只有一个：把每轮的[世界状态签名](crate::tool_state::probe)
//! 与本次 run 见过的集合比对，**没有任何新签名**就是零推进。不管工具报的
//! 是成功还是失败，不管换了几个工具，不管是 A→B→A→B 还是同一个调用重复。
//!
//! 命中后的反应是**注入诊断，不停止 run**：停止解决不了「模型不知道该换
//! 哪条路」，把它已经走过的死路与判断依据递到面前才行。

/// 连续多少轮「零新状态」才注入诊断。
///
/// 4 轮：够「重试一次 + 换个参数再试一次」，又不至于让 agent 在同一个死
/// 路口烧掉十几轮。诊断每个 run 只报一次。
pub(crate) const ZERO_PROGRESS_STREAK: u32 = 4;

/// 本 run 的状态账本。
#[derive(Debug, Default)]
pub(crate) struct ProgressLedger {
    /// 本 run 见过的所有世界状态签名。
    seen_states: std::collections::HashSet<String>,
    /// 连续「一个新高状态都没有」的轮数。
    zero_progress_streak: u32,
    /// 是否已经报过诊断（每个 run 一次）。
    warned: bool,
}

impl ProgressLedger {
    /// 记录一轮的世界状态签名，返回零推进诊断文本。
    ///
    /// 调用方只传**能探测到状态**的工具签名（`tool_state::probe` 的 `Some`
    /// 分支）。一轮里一个签名都没有（纯记忆/待办类调用）时不计入 —— 那些
    /// 调用本来就不改变外部世界，拿它们判空转只会误报。
    pub(crate) fn observe_batch(&mut self, signatures: &[String]) -> Option<String> {
        if signatures.is_empty() {
            return None;
        }

        let mut fresh = 0usize;
        for sig in signatures {
            if self.seen_states.insert(sig.clone()) {
                fresh += 1;
            }
        }

        if fresh > 0 {
            self.zero_progress_streak = 0;
            return None;
        }

        self.zero_progress_streak = self.zero_progress_streak.saturating_add(1);
        if self.warned || self.zero_progress_streak < ZERO_PROGRESS_STREAK {
            return None;
        }
        self.warned = true;

        Some(format!(
            "[ZERO-PROGRESS] 最近 {streak} 轮，你调用工具后世界状态**没有任何变化**——\
             拿到的都是本次任务里已经见过的状态。\
             \n继续用同样的方式调用不会改变这一点。按顺序检查：\
             \n1) 前置条件是否成立（登录态 / 权限 / 目标页面或文件是否真的存在）；\
             \n2) 这个状态是否已经说明目标不可达（无权限、账号被禁用、文件不存在、\
             页面提示错误）——如果是，直接告诉用户卡在哪、需要什么，不要继续试；\
             \n3) 换一条路径：不同的工具、不同的入口、不同的作用域（例如换 URL / \
             换 profile / 换目录 / 换命令），或在等待异步结果时改用等待机制而不是重复轮询。",
            streak = self.zero_progress_streak,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(s: &str) -> Vec<String> {
        vec![s.to_string()]
    }

    #[test]
    fn identical_state_reports_once_after_threshold() {
        let mut ledger = ProgressLedger::default();
        let page = "web:url=https://i.chaoxing.com/base;title=温馨提示";
        assert!(ledger.observe_batch(&sig(page)).is_none()); // 首次见到：新状态
        assert!(ledger.observe_batch(&sig(page)).is_none()); // 1
        assert!(ledger.observe_batch(&sig(page)).is_none()); // 2
        assert!(ledger.observe_batch(&sig(page)).is_none()); // 3
        let hit = ledger
            .observe_batch(&sig(page))
            .expect("第 4 次重复必须报");
        assert!(hit.contains("ZERO-PROGRESS"));
        // 只报一次。
        assert!(ledger.observe_batch(&sig(page)).is_none());
    }

    #[test]
    fn new_state_resets_the_streak_across_tools() {
        let mut ledger = ProgressLedger::default();
        ledger.observe_batch(&sig("web:url=a"));
        ledger.observe_batch(&sig("web:url=a"));
        ledger.observe_batch(&sig("web:url=a"));
        // 换到另一条路并拿到了新状态 → 清零，不报。
        assert!(ledger.observe_batch(&sig("fs:D:\\a.txt;size=5")).is_none());
        assert!(ledger.observe_batch(&sig("web:url=a")).is_none());
    }

    #[test]
    fn batches_without_state_signatures_are_ignored() {
        let mut ledger = ProgressLedger::default();
        for _ in 0..10 {
            assert!(ledger.observe_batch(&[]).is_none());
        }
    }

    #[test]
    fn alternating_states_still_count_as_zero_progress() {
        // A→B→A→B：整批结果从不「逐字相同」，RepeatGuard 会漏，这里抓得住。
        let mut ledger = ProgressLedger::default();
        let a = sig("web:url=login");
        let b = sig("web:url=notice");
        ledger.observe_batch(&a);
        ledger.observe_batch(&b);
        assert!(ledger.observe_batch(&a).is_none()); // 1
        assert!(ledger.observe_batch(&b).is_none()); // 2
        assert!(ledger.observe_batch(&a).is_none()); // 3
        let hit = ledger.observe_batch(&b).expect("交替重复也属于零推进");
        assert!(hit.contains("ZERO-PROGRESS"));
    }
}
