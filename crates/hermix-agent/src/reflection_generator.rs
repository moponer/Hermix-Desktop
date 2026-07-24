//! Phase 19 (E): memory evolution — reflective session-end notes.
//!
//! The agent loop calls [`generate_reflections`] from
//! `memory_on_session_end` (memory_prefetch.rs). The output is a
//! small set of `Reflection` rows written to the `reflections`
//! table via `EmbeddedMemoryBackend::add_reflection`, then emitted
//! as `StreamEvent::MemoryUpdate { kind: "reflection" }` so the
//! user can see what the agent thinks it learned.
//!
//! ## Why no LLM
//!
//! Memory evolution here means "make the existing signals legible
//! to the user", not "summarize with a model". Calling an LLM per
//! session for reflections would burn tokens for what is already
//! present in `facts` + `tool_errors` + `user.style`. The four
//! reflection kinds are template-driven so they're stable,
//! deterministic, and cheap.
//!
//! ## What we DON'T generate
//!
//! - No retry/reflexion-style analysis of failures (that's the
//!   `failure_pattern` facts path — separate).
//! - No "what would I do differently" reasoning — that requires
//!   chain-of-thought we don't have. We only describe what
//!   happened.

use crate::memory_prefetch::emit_memory_update_event;
use hermix_focus::EmbeddedMemoryBackend;

/// What we believe about the session. One row per kind.
#[derive(Debug, Clone, PartialEq)]
pub struct Reflection {
    pub kind: &'static str,
    pub content: String,
    pub salience: f32,
}

/// Inputs the loop already has on hand — we don't make the
/// generator hit SQLite again. Callers pass pre-aggregated numbers.
#[derive(Debug, Clone, Default)]
pub struct ReflectionInputs {
    pub session_id: String,
    pub turn_count: u32,
    pub tool_error_count: u32,
    pub session_fact_count: u32,
    /// Output of `user_style::detect_user_style` aggregated across
    /// the session — we surface the most-frequent style string
    /// (e.g. `"direct"`, `"exploratory"`, or `None`).
    pub dominant_user_style: Option<String>,
}

const KIND_WHAT_WENT_WELL: &str = "what_went_well";
const KIND_WHAT_TO_IMPROVE: &str = "what_to_improve";
const KIND_USER_PREFERENCES: &str = "user_preferences_observed";
const KIND_NEXT_SESSION_HINT: &str = "next_session_hint";

/// Deterministic reflection generator.
///
/// Returns up to four reflections. Each kind is independent — we
/// emit it only if its input signal is non-trivial (e.g. no
/// `what_to_improve` if errors == 0).
pub fn generate_reflections(inputs: &ReflectionInputs) -> Vec<Reflection> {
    let mut out = Vec::with_capacity(4);

    // 1. what_went_well — only when there were no tool errors AND
    // we actually did something. Suppress on trivial sessions
    // (1 turn, 0 facts) to avoid noise.
    if inputs.tool_error_count == 0 && (inputs.turn_count >= 2 || inputs.session_fact_count >= 1) {
        let content = match (inputs.turn_count, inputs.session_fact_count) {
            (t, 0) => format!("本次会话 {} 轮，无工具调用错误。", t),
            (t, f) => format!(
                "本次会话 {} 轮、提取 {} 条新事实，无工具调用错误。",
                t, f
            ),
        };
        out.push(Reflection {
            kind: KIND_WHAT_WENT_WELL,
            content,
            salience: 0.4,
        });
    }

    // 2. what_to_improve — surface tool errors so the user knows
    // we noticed (and so next-session reflection can avoid the
    // same path).
    if inputs.tool_error_count > 0 {
        out.push(Reflection {
            kind: KIND_WHAT_TO_IMPROVE,
            content: format!(
                "本次会话出现 {} 次工具调用错误，下一轮会更仔细检查输入格式、参数类型与前置条件。",
                inputs.tool_error_count
            ),
            salience: 0.7,
        });
    }

    // 3. user_preferences_observed — only when we have a stable
    // signal (the dialectic detector must have fired at least
    // once in the session).
    if let Some(style) = inputs.dominant_user_style.as_deref() {
        let human = match style {
            "direct" => "直接、命令式",
            "exploratory" => "探索式、提问式",
            "tutorial" => "教学式、逐步讲解式",
            "debug" => "调试式、关注错误细节",
            other => other,
        };
        out.push(Reflection {
            kind: KIND_USER_PREFERENCES,
            content: format!(
                "观察到用户偏好风格：{}（{}）。后续回复会按此风格适配。",
                human, style
            ),
            salience: 0.6,
        });
    }

    // 4. next_session_hint — only when we learned something worth
    // resuming (3+ facts) or talked long enough that the user
    // is likely to come back (10+ turns).
    if inputs.session_fact_count >= 3 || inputs.turn_count >= 10 {
        let hint = if inputs.session_fact_count >= 3 {
            format!(
                "下次可延续：基于本次提取的 {} 条事实继续推进。",
                inputs.session_fact_count
            )
        } else {
            format!(
                "下次可延续：会话较长（{} 轮），可总结上下文或继续当前话题。",
                inputs.turn_count
            )
        };
        out.push(Reflection {
            kind: KIND_NEXT_SESSION_HINT,
            content: hint,
            salience: 0.5,
        });
    }

    out
}

/// Write reflections to the embedded backend and emit one
/// `memory_update` event per reflection.
///
/// Returns the number of reflections written + emitted.
///
/// Honors `quiet_mode`: callers should pre-check the flag and skip
/// this function entirely when set. (We don't re-check here so the
/// generator itself stays pure.)
pub fn write_and_emit_reflections(
    backend: &EmbeddedMemoryBackend,
    callbacks: Option<&crate::agent_helpers::AgentCallbacks>,
    inputs: &ReflectionInputs,
) -> usize {
    let refs = generate_reflections(inputs);
    let count = refs.len();
    for r in &refs {
        let id = match backend.add_reflection(
            &inputs.session_id,
            r.kind,
            &r.content,
            r.salience,
            &[format!("reflection:{}", r.kind), format!("session:{}", inputs.session_id)],
            inputs.turn_count,
            "reflection_generator",
        ) {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!(
                    target: "hermix.agent",
                    session_id = %inputs.session_id,
                    kind = r.kind,
                    error = %e,
                    "reflection_generator: add_reflection failed"
                );
                continue;
            }
        };

        let record = crate::memory_prefetch::MemoryWriteRecord {
            action: crate::memory_prefetch::MemoryAction::Summarize,
            memory_type: crate::memory_prefetch::MemoryKind::Reflection,
            key: format!("reflection:{}:{}", r.kind, id),
            summary: format!("[{}] {}", reflection_label(r.kind), r.content),
            count: Some(1),
        };
        emit_memory_update_event(callbacks, &record);
    }
    count
}

/// Short Chinese label for the UI badge.
pub fn reflection_label(kind: &str) -> &'static str {
    match kind {
        KIND_WHAT_WENT_WELL => "做得好的",
        KIND_WHAT_TO_IMPROVE => "要改进的",
        KIND_USER_PREFERENCES => "观察到的偏好",
        KIND_NEXT_SESSION_HINT => "下次可延续",
        _ => "反思",
    }
}

/// P4-T1 (reflection → 行为改写闭环): render a cross-session list
/// of reflections into a compact markdown block that the agent loop
/// prepends to the next session's system prompt.
///
/// Budget per item: `per_item_char_budget` (default 200). Max items:
/// `max_items` (default 2). Caps items so a noisy session can't blow
/// out the system prompt. Returns `None` when the list is empty so
/// callers can chain.
pub fn build_prior_session_reflection_block(
    reflections: &[Reflection],
    per_item_char_budget: usize,
    max_items: usize,
) -> Option<String> {
    if reflections.is_empty() || max_items == 0 {
        return None;
    }
    let mut lines: Vec<String> = Vec::with_capacity(max_items + 1);
    lines.push("[上次反思] 以下是从最近会话反思提取的行为改进提示，请纳入本轮回复决策。".to_string());
    for r in reflections.iter().take(max_items) {
        let truncated: String = if r.content.chars().count() > per_item_char_budget {
            // Truncate at the character boundary just before the
            // budget so we don't chop mid-character and so per-item
            // length matches the budget on common cases.
            let mut out = String::with_capacity(per_item_char_budget);
            for c in r.content.chars() {
                if out.chars().count() + 1 > per_item_char_budget {
                    break;
                }
                out.push(c);
            }
            out.push('…');
            out
        } else {
            r.content.clone()
        };
        lines.push(format!("- [{}] {}", reflection_label(r.kind), truncated));
    }
    Some(lines.join("\n"))
}

/// Default per-item char budget for `build_prior_session_reflection_block`.
pub const REFLECTION_BLOCK_PER_ITEM_CHARS: usize = 200;
/// Default max items in the block.
pub const REFLECTION_BLOCK_MAX_ITEMS: usize = 2;

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_inputs() -> ReflectionInputs {
        ReflectionInputs {
            session_id: "s1".into(),
            turn_count: 0,
            tool_error_count: 0,
            session_fact_count: 0,
            dominant_user_style: None,
        }
    }

    #[test]
    fn trivial_session_emits_no_reflections() {
        // 1 turn, 0 facts, 0 errors → no signal worth surfacing
        let inputs = ReflectionInputs {
            turn_count: 1,
            ..empty_inputs()
        };
        assert!(generate_reflections(&inputs).is_empty());
    }

    #[test]
    fn clean_long_session_emits_went_well_and_hint() {
        let inputs = ReflectionInputs {
            turn_count: 12,
            session_fact_count: 0,
            ..empty_inputs()
        };
        let r = generate_reflections(&inputs);
        assert!(r.iter().any(|r| r.kind == KIND_WHAT_WENT_WELL));
        assert!(r.iter().any(|r| r.kind == KIND_NEXT_SESSION_HINT));
        assert!(!r.iter().any(|r| r.kind == KIND_WHAT_TO_IMPROVE));
    }

    #[test]
    fn tool_errors_surface_improvement_reflection() {
        let inputs = ReflectionInputs {
            turn_count: 5,
            tool_error_count: 3,
            ..empty_inputs()
        };
        let r = generate_reflections(&inputs);
        assert!(r.iter().any(|r| r.kind == KIND_WHAT_TO_IMPROVE));
        // errors suppress "went_well" — we'd be lying otherwise
        assert!(!r.iter().any(|r| r.kind == KIND_WHAT_WENT_WELL));
    }

    #[test]
    fn user_style_emits_preference_reflection() {
        let inputs = ReflectionInputs {
            turn_count: 4,
            dominant_user_style: Some("direct".into()),
            ..empty_inputs()
        };
        let r = generate_reflections(&inputs);
        assert!(r.iter().any(|r| r.kind == KIND_USER_PREFERENCES));
        let pref = r
            .iter()
            .find(|r| r.kind == KIND_USER_PREFERENCES)
            .unwrap();
        assert!(pref.content.contains("直接"));
    }

    #[test]
    fn fact_heavy_session_emits_hint() {
        let inputs = ReflectionInputs {
            turn_count: 4,
            session_fact_count: 5,
            ..empty_inputs()
        };
        let r = generate_reflections(&inputs);
        assert!(r.iter().any(|r| r.kind == KIND_NEXT_SESSION_HINT));
        let hint = r
            .iter()
            .find(|r| r.kind == KIND_NEXT_SESSION_HINT)
            .unwrap();
        assert!(hint.content.contains("5"));
    }

    #[test]
    fn reflection_labels_are_chinese() {
        assert_eq!(reflection_label(KIND_WHAT_WENT_WELL), "做得好的");
        assert_eq!(reflection_label(KIND_WHAT_TO_IMPROVE), "要改进的");
        assert_eq!(reflection_label(KIND_USER_PREFERENCES), "观察到的偏好");
        assert_eq!(reflection_label(KIND_NEXT_SESSION_HINT), "下次可延续");
        assert_eq!(reflection_label("unknown"), "反思");
    }

    // ---------- P4-T1: cross-session reflection block ----------

    fn rkind(kind: &'static str, content: &str) -> Reflection {
        Reflection {
            kind,
            content: content.to_string(),
            salience: 0.5,
        }
    }

    #[test]
    fn prior_reflection_block_returns_none_on_empty() {
        let out = build_prior_session_reflection_block(&[], 200, 2);
        assert!(out.is_none());
    }

    #[test]
    fn prior_reflection_block_respects_max_items() {
        let r = vec![
            rkind(KIND_WHAT_TO_IMPROVE, "a"),
            rkind(KIND_WHAT_TO_IMPROVE, "b"),
            rkind(KIND_WHAT_TO_IMPROVE, "c"),
        ];
        let out = build_prior_session_reflection_block(&r, 200, 2).unwrap();
        // header + exactly 2 bullets
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[1].contains("a"));
        assert!(lines[2].contains("b"));
    }

    #[test]
    fn prior_reflection_block_truncates_oversized_items() {
        let long = "x".repeat(1_000);
        let r = vec![rkind(KIND_WHAT_TO_IMPROVE, &long)];
        let out = build_prior_session_reflection_block(&r, 50, 5).unwrap();
        // The bullet line should be ≤ 50 chars + budget for the
        // "[要改进的] " prefix + "…". We accept any compressed view
        // that's much shorter than the original.
        assert!(out.len() < 200);
        assert!(out.contains('…'));
    }

    #[test]
    fn prior_reflection_block_disabled_when_max_items_zero() {
        let r = vec![rkind(KIND_WHAT_TO_IMPROVE, "x")];
        assert!(build_prior_session_reflection_block(&r, 200, 0).is_none());
    }
}