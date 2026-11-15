//! 执行时间线的体积上限。
//!
//! 起因（2026-10-29 实测）：一个 30 多次工具调用的回合，`execution_timeline` +
//! `unified_steps` 落库后单条消息 **2.2MB**、单会话 5.7MB。构成几乎全是
//! `tool_stdout`：每次工具调用按 16KB 一片投递完整输出、每片一个事件，
//! 一次 `terminal` 输出 50KB 就是好几个事件，还和 `tool_complete` 的预览
//! 重复存一遍。后果是切会话要 `JSON.parse` 几 MB、parsed 对象常驻几十 MB，
//! 而用户在折叠态只看得到首行、展开时才需要正文。
//!
//! 策略：**每次调用只留一个 stdout 事件，超限时保留头尾、掐中间**。
//! 头尾都要：错误信息常在尾部（`stderr` 最后几行），上下文在头部。
//! 被掐掉的部分不静默丢弃 —— 中间插一行说明省略了多少字符。

use hermix_core::types::ExecutionTimelineEvent;

/// 单次工具调用在时间线里保留的 stdout 上限（**字符**）。
///
/// 24K 字符覆盖常规命令输出；再大就只留头尾。
pub(super) const STDOUT_CAP_PER_CALL: usize = 24 * 1024;

/// 头尾各留一半。
fn split_budget(cap: usize) -> (usize, usize) {
    let head = cap / 2;
    (head, cap - head)
}

fn clip_to_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

fn tail_chars(s: &str, max: usize) -> String {
    let total = s.chars().count();
    if total <= max {
        return s.to_string();
    }
    s.chars().skip(total - max).collect()
}

/// 把 `old + new` 压到 `cap` 字符以内：超限则保留头尾、中间留一行省略说明。
fn merge_keep_head_tail(old: &str, new: &str, cap: usize) -> String {
    let total = old.chars().count() + new.chars().count();
    let joined = format!("{old}{new}");
    if total <= cap {
        return joined;
    }
    let (head_budget, tail_budget) = split_budget(cap);
    let head: String = joined.chars().take(head_budget).collect();
    let tail = tail_chars(&joined, tail_budget);
    format!(
        "{head}\n…（中间已省略 {} 字符，保留头尾）\n{tail}",
        total.saturating_sub(cap)
    )
}

/// 追加（或合并）一条 `tool_stdout` 到时间线，按**单次调用**限制总量。
///
/// - 同一次调用（`tool_call_id` 相同）只保留**一个** stdout 事件：后续分片
///   追加进这一条，既省事件数也省前端 `groupTimeline` 的遍历量；
/// - 超过 `cap` 时保留头尾、掐中间；
/// - 非 `tool_stdout` 事件不经过这里（调用方自己 push）。
pub(super) fn push_stdout_capped(
    buf: &mut Vec<ExecutionTimelineEvent>,
    mut event: ExecutionTimelineEvent,
    cap: usize,
) {
    debug_assert_eq!(event.event_type, "tool_stdout");
    let key = event.tool_call_id.clone().unwrap_or_default();
    let chunk = event.content.take().unwrap_or_default();

    if let Some(slot) = buf.iter_mut().rev().find(|e| {
        e.event_type == "tool_stdout" && e.tool_call_id.clone().unwrap_or_default() == key
    }) {
        let merged = merge_keep_head_tail(slot.content.as_deref().unwrap_or(""), &chunk, cap);
        slot.content = Some(merged);
        // 分片计数保持单调递增；前端不依赖它的绝对值，只用它判断"还有没有后续"。
        let next = slot.chunk_total.unwrap_or(1) + 1;
        slot.chunk_total = Some(next);
        slot.chunk_index = Some(next);
        return;
    }

    event.content = Some(clip_to_chars(&chunk, cap));
    event.chunk_total = Some(event.chunk_total.unwrap_or(1));
    event.chunk_index = Some(event.chunk_index.unwrap_or(1));
    buf.push(event);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdout(call: &str, content: &str) -> ExecutionTimelineEvent {
        ExecutionTimelineEvent {
            event_type: "tool_stdout".to_string(),
            tool: Some("terminal".to_string()),
            content: Some(content.to_string()),
            arguments: None,
            chunk_index: Some(1),
            chunk_total: Some(1),
            created_at: "2026-10-29T00:00:00Z".to_string(),
            tool_call_id: Some(call.to_string()),
            file_path: None,
            change_type: None,
            file_summary: None,
            is_error: None,
        }
    }

    #[test]
    fn same_call_merges_into_one_event() {
        let mut buf = Vec::new();
        for part in ["aa", "bb", "cc"] {
            push_stdout_capped(&mut buf, stdout("c1", part), 1024);
        }
        assert_eq!(buf.len(), 1, "同一次调用只应留一个 stdout 事件");
        assert_eq!(buf[0].content.as_deref(), Some("aabbcc"));
    }

    #[test]
    fn different_calls_stay_separate() {
        let mut buf = Vec::new();
        push_stdout_capped(&mut buf, stdout("c1", "one"), 1024);
        push_stdout_capped(&mut buf, stdout("c2", "two"), 1024);
        assert_eq!(buf.len(), 2);
        assert_eq!(buf[0].content.as_deref(), Some("one"));
        assert_eq!(buf[1].content.as_deref(), Some("two"));
    }

    #[test]
    fn over_cap_keeps_head_and_tail_with_marker() {
        let mut buf = Vec::new();
        let head = "H".repeat(20);
        let middle = "M".repeat(100);
        let tail = "T".repeat(20);
        push_stdout_capped(&mut buf, stdout("c1", &head), 40);
        push_stdout_capped(&mut buf, stdout("c1", &middle), 40);
        push_stdout_capped(&mut buf, stdout("c1", &tail), 40);

        let body = buf[0].content.clone().unwrap();
        assert!(body.chars().count() <= 40 + 64, "超出上限: {}", body.chars().count());
        assert!(!body.contains('M'), "中间部分应被掐掉: {body}");
        assert!(body.starts_with('H'), "头部要保留: {body}");
        assert!(body.ends_with('T'), "尾部要保留: {body}");
        assert!(body.contains("已省略"), "必须说明省略了多少: {body}");
    }

    #[test]
    fn cap_does_not_count_bytes_of_cjk() {
        // 中文按字符算上限，不能按字节（24KB 字节只够 8K 汉字，会过度截断）
        let mut buf = Vec::new();
        push_stdout_capped(&mut buf, stdout("c1", &"中".repeat(30_000)), STDOUT_CAP_PER_CALL);
        let n = buf[0].content.as_deref().unwrap().chars().count();
        assert!(n <= STDOUT_CAP_PER_CALL + 64, "汉字被过度截断: {n}");
    }
}
