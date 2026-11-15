//! — tool error demotion hints.
//!
//! When the same tool errored 2+ times in a session, the agent loop
//! should know — but not as a hard ban. We expose the failed tools
//! to the model so it can choose an alternative path on its own.
//!
//! ## Design
//!
//! - [`ToolErrorCounter`] is a thin wrapper over `HashMap<String, u32>`
//!   that is cheap to read on every turn (a HashMap snapshot) and
//!   never blocks.
//! - [`build_tool_error_demotion_block`] turns the counter into a
//!   compact markdown bullet list, only including tools at/above
//!   `threshold` errors. Returns `None` when no tool qualifies so
//!   callers can chain unconditionally.
//! - The block is appended to the system prompt via a
//!   `DirRoute::Always` direction, so it ships unconditionally —
//!   `None` body means the direction stays effectively empty and the
//!   gateway audit logs still record the route was checked.

use std::collections::HashMap;

/// Default threshold beyond which a tool is "demoted" — surfaced as a
/// soft hint rather than banned. The model may still call it if it
/// genuinely needs to; we just stop pretending the previous failures
/// didn't happen.
pub const DEFAULT_DEMOTION_THRESHOLD: usize = 2;

/// Maximum number of demoted tools surfaced per turn — caps the
/// block so a long noisy session can't spam the system prompt.
pub const DEFAULT_MAX_DEMOTED_TOOLS: usize = 5;

/// Per-tool error counter. Cheap, no locking — wrap in `Mutex` /
/// `RwLock` at the call site if read across async tasks.
#[derive(Debug, Default, Clone)]
pub struct ToolErrorCounter {
    counts: HashMap<String, u32>,
}

impl ToolErrorCounter {
    /// Build an empty counter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a counter from a snapshot. Useful for restoring
    /// persisted state or seeding tests.
    pub fn from_snapshot(snapshot: HashMap<String, u32>) -> Self {
        Self { counts: snapshot }
    }

    /// Bump the failure count for `tool_name`. Idempotent — calling
    /// with `tool_name == ""` is a no-op (empty tool names happen on
    /// malformed assistant output and should never poison the map).
    pub fn record_error(&mut self, tool_name: &str) {
        if tool_name.is_empty() {
            return;
        }
        *self.counts.entry(tool_name.to_string()).or_insert(0) += 1;
    }

    /// Immutable snapshot copy. Caller can iterate without holding
    /// the counter's lock.
    pub fn snapshot(&self) -> HashMap<String, u32> {
        self.counts.clone()
    }

    /// Reset every counter — used by the agent loop on session end
    /// so a stale run doesn't poison the next session.
    pub fn clear(&mut self) {
        self.counts.clear();
    }
}

/// Render the counter into a system-prompt hint block.
///
/// - `threshold`: tools with count `< threshold` are skipped (default 2).
/// - `max_items`: cap on tool rows so the block stays short (default 5).
///
/// Returns `None` when no tool qualifies or `threshold == 0` (caller
/// disabled the feature). Bullets include the tool name, the count,
/// and a gentle suggestion to consider alternatives.
pub fn build_tool_error_demotion_block(
    counts: &HashMap<String, u32>,
    threshold: usize,
    max_items: usize,
) -> Option<String> {
    if threshold == 0 || max_items == 0 {
        return None;
    }
    let mut sorted: Vec<(&str, u32)> = counts
        .iter()
        .filter_map(|(k, v)| if *v as usize >= threshold {
            Some((k.as_str(), *v))
        } else {
            None
        })
        .collect();
    if sorted.is_empty() {
        return None;
    }
    // Highest count first so the noisy tool lands at the top.
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    sorted.truncate(max_items);

    let mut lines = Vec::with_capacity(sorted.len() + 1);
    lines.push(format!(
        "以下工具在本会话中已连续失败 ≥{} 次，可以考虑改用替代方案或先确认输入：",
        threshold
    ));
    for (name, n) in sorted {
        lines.push(format!("- {} (失败 {} 次)", name, n));
    }
    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_records_and_snapshots() {
        let mut c = ToolErrorCounter::new();
        c.record_error("terminal");
        c.record_error("terminal");
        c.record_error("fs_read");
        let snap = c.snapshot();
        assert_eq!(snap.get("terminal"), Some(&2));
        assert_eq!(snap.get("fs_read"), Some(&1));
    }

    #[test]
    fn counter_ignores_empty_tool_names() {
        let mut c = ToolErrorCounter::new();
        c.record_error("");
        c.record_error("");
        assert!(c.snapshot().is_empty());
    }

    #[test]
    fn counter_clear_resets_state() {
        let mut c = ToolErrorCounter::new();
        c.record_error("x");
        c.clear();
        assert!(c.snapshot().is_empty());
    }

    #[test]
    fn block_returns_none_below_threshold() {
        let mut counts = HashMap::new();
        counts.insert("terminal".into(), 1);
        assert!(build_tool_error_demotion_block(&counts, 2, 5).is_none());
    }

    #[test]
    fn block_returns_none_at_zero_counts() {
        let counts = HashMap::new();
        assert!(build_tool_error_demotion_block(&counts, 2, 5).is_none());
    }

    #[test]
    fn block_includes_above_threshold_tools() {
        let mut counts = HashMap::new();
        counts.insert("terminal".into(), 5);
        counts.insert("fs_read".into(), 2);
        counts.insert("ok_tool".into(), 1);
        let block = build_tool_error_demotion_block(&counts, 2, 5).unwrap();
        assert!(block.contains("terminal"));
        assert!(block.contains("fs_read"));
        assert!(!block.contains("ok_tool"));
        // 'terminal' (5 failures) should come first as it has higher count.
        let t_pos = block.find("terminal").unwrap();
        let f_pos = block.find("fs_read").unwrap();
        assert!(t_pos < f_pos);
    }

    #[test]
    fn block_respects_max_items() {
        let mut counts = HashMap::new();
        for i in 0..10 {
            counts.insert(format!("tool_{i}"), 5);
        }
        let block = build_tool_error_demotion_block(&counts, 2, 3).unwrap();
        // Exactly 3 bullets + 1 header line.
        assert_eq!(block.lines().count(), 4);
    }

    #[test]
    fn block_disabled_when_threshold_zero() {
        let mut counts = HashMap::new();
        counts.insert("x".into(), 99);
        assert!(build_tool_error_demotion_block(&counts, 0, 5).is_none());
    }

    #[test]
    fn block_disabled_when_max_items_zero() {
        let mut counts = HashMap::new();
        counts.insert("x".into(), 99);
        assert!(build_tool_error_demotion_block(&counts, 2, 0).is_none());
    }
}
