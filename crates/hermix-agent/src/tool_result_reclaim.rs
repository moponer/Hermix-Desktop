//! 历史里旧工具结果的回收（ZCode `microcompact` 的 hermix 版）。
//!
//! 「发过即冻结」保证了前缀缓存，但也意味着工具结果一旦进历史就要**每轮
//! 重发**。回收是唯一能压体积的出口，代价是一次全量缓存失效 —— 所以它必须
//! 是**高水位 + 批量 + 用户消息边界**的低频事件。
//!
//! 反面教材是 2026-10-29 拆掉的 `rollup_tool_results`：按「距尾部多少条」
//! 做滑窗，于是每轮都有一条**已发过**的结果被改写，命中率被代数式地钉死在
//! `1 − 窗口/prompt`（实测 77%）。滑窗式回收在任何实现里都会重犯这个错，
//! 因此这里只在 `prepare_run_context`（一次 run 的开始）调用，且用高水位
//! 触发、一次性回收一大批。
//!
//! 与 ZCode 的差别：那边的 microcompact 默认关闭，这里默认开启但门槛很高
//! （[`RECLAIM_TRIGGER_CHARS`] = 150 K 字符）。工具结果合计 150 K 字符的会话
//! 已经不是"正常"，而是实打实的膨胀（2026-10-30 实测其中一个会话 340 K）。

use hermix_core::{Message, MessageRole};

/// 占位符前缀。用它判断「这条已经被回收过」，保证重复调用幂等
/// （幂等很重要：run 失败重试会再走一次这个函数）。
const CLEARED_MARKER: &str = "[old tool result cleared";

/// 一次回收的结果。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReclaimStats {
    /// 被置换成占位符的结果条数。
    pub cleared: usize,
    /// 回收前的工具结果字符合计。
    pub chars_before: usize,
    /// 回收后的工具结果字符合计。
    pub chars_after: usize,
}

impl ReclaimStats {
    pub fn fired(&self) -> bool {
        self.cleared > 0
    }

    pub fn saved_chars(&self) -> usize {
        self.chars_before.saturating_sub(self.chars_after)
    }
}

fn content_chars(msg: &Message) -> usize {
    // 与 `budget` 层同一口径（字符而非字节）：中文/emoji 的字节数会是字符数的
    // 3–4 倍，两处用不同单位会让「合计超 150 K」这条线随语言漂移。
    msg.content
        .as_deref()
        .map(|c| c.chars().count())
        .unwrap_or(0)
}

/// 把最旧的一批工具结果换成一行占位符。
///
/// 规则（缺一不可）：
/// - 只回收 `Tool` 角色 —— 用户/助手正文是任务本身，永远不动；
/// - 保留最近 [`hermix_tools::result_budget::RECLAIM_KEEP_RESULTS`] 条原文；
/// - 小于 [`hermix_tools::result_budget::RECLAIM_MIN_RESULT_CHARS`] 的不动
///   （占位符本身就要几十个字符，回收它们纯亏）；
/// - 合计未达 [`hermix_tools::result_budget::RECLAIM_TRIGGER_CHARS`] 时整体不动。
pub fn reclaim_old_tool_results(messages: &mut [Message]) -> ReclaimStats {
    use hermix_tools::result_budget::{
        RECLAIM_KEEP_RESULTS, RECLAIM_MIN_RESULT_CHARS, RECLAIM_TRIGGER_CHARS,
    };

    let before: usize = messages
        .iter()
        .filter(|m| m.role == MessageRole::Tool)
        .map(content_chars)
        .sum();
    let mut stats = ReclaimStats {
        cleared: 0,
        chars_before: before,
        chars_after: before,
    };
    if before <= RECLAIM_TRIGGER_CHARS {
        return stats;
    }

    // 从尾部往前数：越过 KEEP 条原文之后，剩下的候选从旧到新回收。
    let mut remaining_keep = RECLAIM_KEEP_RESULTS;
    let mut victims: Vec<usize> = Vec::new();
    for (idx, msg) in messages.iter().enumerate().rev() {
        if msg.role != MessageRole::Tool {
            continue;
        }
        let content = msg.content.as_deref().unwrap_or("");
        if content.starts_with(CLEARED_MARKER) {
            continue;
        }
        if remaining_keep > 0 {
            remaining_keep -= 1;
            continue;
        }
        if content.chars().count() < RECLAIM_MIN_RESULT_CHARS {
            continue;
        }
        victims.push(idx);
    }
    if victims.is_empty() {
        return stats;
    }

    let mut after = before;
    for idx in victims {
        let tool = messages[idx]
            .name
            .clone()
            .unwrap_or_else(|| "tool".to_string());
        let n = content_chars(&messages[idx]);
        // 占位符要回答三个问题：删了什么、多大、怎么拿回来。
        messages[idx].content = Some(format!(
            "{CLEARED_MARKER}: {n} chars of `{tool}` output removed to keep the request small. \
             需要原文就重新执行一次该调用（结果可能已变）；这是历史清理，不是错误。]"
        ));
        after = after.saturating_sub(n);
        after += content_chars(&messages[idx]);
        stats.cleared += 1;
    }
    stats.chars_after = after;
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_msg(id: &str, body: &str) -> Message {
        Message::tool_result(id, body, Some("terminal"))
    }

    /// 未达高水位 → 一条都不动。
    #[test]
    fn below_watermark_is_a_noop() {
        let mut msgs = vec![tool_msg("a", &"x".repeat(5_000)); 3];
        let stats = reclaim_old_tool_results(&mut msgs);
        assert_eq!(stats.cleared, 0);
        assert!(msgs[0].content.as_deref().unwrap().starts_with('x'));
    }

    /// 越过水位 → 旧的一批变占位符，最近的 KEEP 条保留原文。
    #[test]
    fn oldest_results_are_cleared_and_newest_kept() {
        let big = "y".repeat(20_000);
        // 必须多于 KEEP 条，否则没有可回收的候选；60 × 20 K = 1.2 M > 150 K 水位。
        let mut msgs: Vec<Message> = (0..60).map(|i| tool_msg(&format!("c{i}"), &big)).collect();
        let stats = reclaim_old_tool_results(&mut msgs);
        assert!(stats.fired());
        assert!(stats.saved_chars() > 0);
        let kept = msgs
            .iter()
            .filter(|m| m.content.as_deref().unwrap().starts_with('y'))
            .count();
        assert_eq!(
            kept,
            hermix_tools::result_budget::RECLAIM_KEEP_RESULTS,
            "只应保留最近 N 条原文"
        );
        // 用户/助手正文不受影响（这里没有它们，断言占位符形态）。
        assert!(msgs[0]
            .content
            .as_deref()
            .unwrap()
            .starts_with(CLEARED_MARKER));
    }

    /// 幂等：第二次调用不会再动已经回收过的行，也不会重复计数。
    #[test]
    fn second_pass_is_idempotent() {
        let big = "z".repeat(20_000);
        let mut msgs: Vec<Message> = (0..60).map(|i| tool_msg(&format!("d{i}"), &big)).collect();
        let first = reclaim_old_tool_results(&mut msgs);
        assert!(first.fired());
        let snapshot: Vec<String> = msgs
            .iter()
            .map(|m| m.content.clone().unwrap_or_default())
            .collect();
        let second = reclaim_old_tool_results(&mut msgs);
        assert_eq!(second.cleared, 0, "已回收的行必须被跳过");
        let after: Vec<String> = msgs
            .iter()
            .map(|m| m.content.clone().unwrap_or_default())
            .collect();
        assert_eq!(snapshot, after, "第二次调用不得改动任何字节");
    }
}
