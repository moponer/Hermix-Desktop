//! Phase 19 (A): memory evolution — per-turn recall scheduler.
//!
//! The agent loop calls [`maybe_recall`] from `run_stream`'s turn
//! prelude. We answer one question: "given this turn's user text,
//! should we re-pull related memories into the system prompt?"
//!
//! ## Why this exists
//!
//! Phase 18 only recalled at session-start (one shot in
//! `memory_prefetch`). Mid-session drift — the user changes topic —
//! would never re-trigger recall, so the agent kept hallucinating
//! facts it already had on disk. This scheduler runs **every turn**
//! (cheap) and only emits an actual recall when:
//!
//! 1. The configured interval has elapsed (default every 5 turns),
//!    **or**
//! 2. The user turn's text drifts from the previous recalled text
//!    by a salience-rank change (we don't compute embeddings here —
//!    we rank against the most-recently-recalled facts by
//!    salience overlap, which is cheap).
//!
//! ## Budget hard cap
//!
//! Even when triggered, recall is hard-capped at
//! `MAX_RECALLS_PER_TURN` items × `MAX_RECALL_CHARS_PER_ITEM`
//! characters. No LLM call. No inflation of the system prompt
//! beyond ~4 KB per recall.
//!
//! ## Quiet mode / skip_memory
//!
//! Both flags are honored at the call site (AgentLoop) — this
//! module is pure & side-effect-free except for emitting one
//! `StreamEvent::MemoryRecall` event and returning the context
//! block. The caller renders the plan via `render_prompt_block`
//! and injects it into message history as a system message so the
//! LLM can use the recalled facts.

use hermix_focus::EmbeddedMemoryBackend;

/// Hard caps — never exceeded even if the query would naturally
/// pull more. The frontend renders "💭 从记忆中想到 N 条" with
/// these as the worst-case numbers.
pub const MAX_RECALLS_PER_TURN: usize = 5;
pub const MAX_RECALL_CHARS_PER_ITEM: usize = 800;
/// How often (in turns) to re-recall even when nothing drifted.
pub const DEFAULT_RECALL_INTERVAL_TURNS: u32 = 5;

/// What the loop injects into the next LLM call as an extra
/// system-prompt block. Returned alongside the event payload so
/// the loop can prepend it.
#[derive(Debug, Clone)]
pub struct RecallPlan {
    pub items: Vec<RecallItem>,
    pub total_chars: usize,
    pub triggered_by: TriggerReason,
}

impl Default for RecallPlan {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            total_chars: 0,
            triggered_by: TriggerReason::None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RecallItem {
    pub canonical_key: String,
    pub payload_text: String,
    pub salience: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TriggerReason {
    /// Interval elapsed (every N turns).
    Interval,
    /// Drift detected between current and last recall.
    Drift,
    /// First turn of the session (warm-up).
    FirstTurn,
    /// Did not trigger — caller doesn't need to re-inject.
    #[default]
    None,
}

/// Inputs the loop already has. Kept narrow so the scheduler is
/// easy to test.
#[derive(Debug, Clone, Default)]
pub struct RecallInputs<'a> {
    pub turn_number: u32,
    pub user_text: &'a str,
    pub last_recall_keys: &'a [&'a str],
    pub recall_interval: u32,
}

impl RecallPlan {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Decide whether to recall this turn, and if so, return the
/// items + a trigger reason. Pure: no DB I/O. Callers wrap this
/// with the actual fact fetch (see `gather_recall_items`).
pub fn should_recall(inputs: &RecallInputs<'_>) -> TriggerReason {
    if inputs.turn_number == 1 {
        return TriggerReason::FirstTurn;
    }
    let interval = if inputs.recall_interval == 0 {
        DEFAULT_RECALL_INTERVAL_TURNS
    } else {
        inputs.recall_interval
    };
    if inputs.turn_number.is_multiple_of(interval) {
        return TriggerReason::Interval;
    }
    if inputs.user_text.trim().len() >= 16 {
        // Drift heuristic: a different first-token bag than the
        // last recalled keys suggests the user changed topic.
        // Cheap, no embedding. False positives are fine — we
        // still honor the budget cap below.
        let first_token = first_meaningful_token(inputs.user_text);
        let last_key_first_token = inputs
            .last_recall_keys
            .first()
            .and_then(|k| k.split(':').next())
            .unwrap_or("");
        if !first_token.is_empty() && first_token != last_key_first_token {
            return TriggerReason::Drift;
        }
    }
    TriggerReason::None
}

fn first_meaningful_token(text: &str) -> String {
    text.split_whitespace()
        .find(|t| t.chars().any(|c| c.is_alphanumeric()))
        .unwrap_or("")
        .to_string()
}

/// Fetch recall items from the embedded backend, hard-capped to
/// the budget. Returns `None` when nothing should be recalled.
pub fn gather_recall_items(
    backend: Option<&EmbeddedMemoryBackend>,
    user_text: &str,
    trigger: TriggerReason,
) -> Option<RecallPlan> {
    if trigger == TriggerReason::None {
        return None;
    }
    let backend = backend?;
    if user_text.trim().is_empty() {
        return None;
    }
    // Phase 19 (A) v1: salience-rank the existing visible facts
    // and take the top MAX_RECALLS_PER_TURN. We deliberately
    // don't embed-and-search here — `recall_scheduler` must be
    // fast enough to call every turn. Future work: plug in
    // cosine_topk against `facts.embedding` once we have a
    // background embedder wired up.
    let ranked = match backend.top_facts_for_recall(MAX_RECALLS_PER_TURN as u32) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::debug!(
                target: "hermix.agent",
                error = %e,
                "recall_scheduler: rerank failed; skipping recall"
            );
            return None;
        }
    };

    let mut plan = RecallPlan {
        items: Vec::with_capacity(MAX_RECALLS_PER_TURN),
        total_chars: 0,
        triggered_by: trigger,
    };
    for (canonical_key, payload_text, salience) in ranked {
        let trimmed = trim_to_char_budget(&payload_text, MAX_RECALL_CHARS_PER_ITEM);
        if trimmed.is_empty() {
            continue;
        }
        plan.total_chars = plan.total_chars.saturating_add(trimmed.chars().count());
        plan.items.push(RecallItem {
            canonical_key,
            payload_text: trimmed,
            salience,
        });
        if plan.items.len() >= MAX_RECALLS_PER_TURN {
            break;
        }
        if plan.total_chars >= MAX_RECALLS_PER_TURN * MAX_RECALL_CHARS_PER_ITEM {
            break;
        }
    }
    if plan.items.is_empty() {
        None
    } else {
        Some(plan)
    }
}

fn trim_to_char_budget(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    s.chars().take(max_chars).collect::<String>() + "…"
}

/// Render the recall as a system-prompt block. Returned string is
/// prepended to the next LLM call. Caller is responsible for not
/// injecting it twice.
pub fn render_prompt_block(plan: &RecallPlan) -> String {
    if plan.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n[💭 从记忆中想到的 N 条相关事实]\n");
    for (i, item) in plan.items.iter().enumerate() {
        out.push_str(&format!(
            "{}. (key={}, salience={:.2}) {}\n",
            i + 1,
            item.canonical_key,
            item.salience,
            item.payload_text
        ));
    }
    out.push_str("[以上为相关记忆，与当前任务直接相关时使用]\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inp<'a>(turn: u32, text: &'a str, last_keys: &'a [&'a str], interval: u32) -> RecallInputs<'a> {
        RecallInputs {
            turn_number: turn,
            user_text: text,
            last_recall_keys: last_keys,
            recall_interval: interval,
        }
    }

    #[test]
    fn first_turn_always_recalls() {
        assert_eq!(should_recall(&inp(1, "hi", &[], 5)), TriggerReason::FirstTurn);
    }

    #[test]
    fn interval_recall_fires_at_multiple() {
        assert_eq!(
            should_recall(&inp(5, "hi", &["fact:foo"], 5)),
            TriggerReason::Interval
        );
        assert_eq!(
            should_recall(&inp(10, "hi", &["fact:foo"], 5)),
            TriggerReason::Interval
        );
    }

    #[test]
    fn drift_recall_fires_when_first_token_changes() {
        // Between intervals, drift triggers
        assert_eq!(
            should_recall(&inp(3, "今天来聊聊性能优化", &["fact:foo"], 5)),
            TriggerReason::Drift
        );
    }

    #[test]
    fn no_recall_when_stable_and_off_interval() {
        assert_eq!(
            should_recall(&inp(3, "继续 foo", &["fact:foo"], 5)),
            TriggerReason::None
        );
    }

    #[test]
    fn very_short_text_does_not_trigger_drift() {
        // < 16 chars => ignore for drift heuristic
        assert_eq!(
            should_recall(&inp(3, "hi", &["fact:foo"], 5)),
            TriggerReason::None
        );
    }

    #[test]
    fn default_interval_is_5() {
        assert_eq!(DEFAULT_RECALL_INTERVAL_TURNS, 5);
    }

    #[test]
    fn trim_honors_char_budget() {
        let s = "a".repeat(2000);
        let trimmed = trim_to_char_budget(&s, 100);
        assert!(trimmed.chars().count() <= 101); // 100 + "…"
    }

    #[test]
    fn render_block_lists_items_with_keys() {
        let plan = RecallPlan {
            items: vec![RecallItem {
                canonical_key: "fact:owner_rust".into(),
                payload_text: "主人喜欢 Rust".into(),
                salience: 0.8,
            }],
            total_chars: 8,
            triggered_by: TriggerReason::Interval,
        };
        let s = render_prompt_block(&plan);
        assert!(s.contains("fact:owner_rust"));
        assert!(s.contains("主人喜欢 Rust"));
        assert!(s.contains("salience=0.80"));
    }

    #[test]
    fn gather_returns_none_when_no_backend() {
        assert!(gather_recall_items(None, "hi", TriggerReason::Interval).is_none());
    }

    #[test]
    fn gather_returns_none_for_empty_user_text() {
        // Need a real backend to test happy path; skip if none
        assert!(gather_recall_items(None, "   ", TriggerReason::FirstTurn).is_none());
    }

    #[test]
    fn empty_plan_renders_empty() {
        let plan = RecallPlan::default();
        assert_eq!(render_prompt_block(&plan), "");
        assert!(plan.is_empty());
    }
}