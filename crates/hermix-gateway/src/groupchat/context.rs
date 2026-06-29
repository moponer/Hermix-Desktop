//! S1: ContextAssembler
//!
//! Builds the per-call prompt context using three layers:
//!  - Layer A: Identity (always on, fixed budget)
//!  - Layer B: Working memory (sliding window of recent turns)
//!  - Layer C: Reference (only when @ref requested)
//!
//! All three layers are **cheap** (no LLM calls) — Layer A and B
//! are content that already exists; Layer C is loaded on demand
//! from `TaskNode.output` archives.

use crate::groupchat::task_types::{TaskNode, TokenBudget};

/// Identity slice — what the agent always sees regardless of budget.
#[derive(Debug, Clone)]
pub struct IdentitySlice {
    pub agent_id: String,
    pub agent_name: String,
    pub role_description: String,
    pub room_name: String,
    pub room_rules: String,
    pub teammates: Vec<TeammateCard>,
    pub rank_status: String,
    pub survival_rules: String,
    /// Current task spec — if any. Empty if no active task.
    pub current_spec: String,
}

#[derive(Debug, Clone)]
pub struct TeammateCard {
    pub id: String,
    pub name: String,
    pub role: String,
}

/// A recent turn in working memory.
#[derive(Debug, Clone)]
pub struct Turn {
    pub sender: String,
    pub content: String,
    /// Token estimate — caller is responsible for accuracy.
    pub tokens: u32,
    /// Whether this is a system-injected message (rules, errors, etc.)
    pub system: bool,
}

/// Reference material — `@ref:task_xxx` triggers full output load.
#[derive(Debug, Clone)]
pub struct Reference {
    pub task_id: String,
    pub title: String,
    pub content: String,
    pub tokens: u32,
}

/// Assembled prompt — passed to the LLM call.
#[derive(Debug, Clone)]
pub struct AssembledContext {
    pub identity: String,
    pub working_memory: String,
    pub references: Vec<String>,
    pub task_prompt: String,
    /// Sum of `identity + working_memory + references + task_prompt`.
    pub total_tokens: u32,
    /// The budget we trimmed against.
    pub budget: u32,
}

/// Tracks which references were already loaded this session so we
/// don't re-fetch them every call.
#[derive(Debug, Default, Clone)]
pub struct ReferenceCache {
    pub loaded: std::collections::HashSet<String>,
}

impl ReferenceCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mark_loaded(&mut self, task_id: &str) {
        self.loaded.insert(task_id.to_string());
    }

    pub fn is_loaded(&self, task_id: &str) -> bool {
        self.loaded.contains(task_id)
    }
}

pub struct ContextAssembler<'a> {
    pub cache: &'a mut ReferenceCache,
}

impl<'a> ContextAssembler<'a> {
    pub fn new(cache: &'a mut ReferenceCache) -> Self {
        Self { cache }
    }

    /// Top-level entry: build a context given identity, recent turns,
    /// the live spec, the task prompt, and a token budget.
    pub fn assemble(
        &mut self,
        identity: &IdentitySlice,
        turns: &[Turn],
        live_spec: &str,
        task_prompt: &str,
        requested_refs: &[Reference],
        budget: TokenBudget,
    ) -> AssembledContext {
        let effective_budget = budget.effective();

        // 1. Identity (Layer A) — always full.
        let identity_text = render_identity(identity);

        // Reserve 60% of budget for identity + task_prompt.
        let reserved = (effective_budget as f64 * 0.6) as u32;
        let remaining = effective_budget.saturating_sub(estimate_tokens(&identity_text) + estimate_tokens(task_prompt) + 100);

        // 2. Live spec — already part of Layer A; we embed directly.
        let _ = live_spec; // not separately rendered for now
        let working_budget = remaining.min(reserved);

        // 3. Working memory — keep most recent turns first.
        let working_memory = render_working_memory(turns, working_budget);

        // 4. References — only new ones (not in cache).
        let mut refs_text: Vec<String> = Vec::new();
        let mut refs_used_tokens = 0u32;
        for r in requested_refs {
            if self.cache.is_loaded(&r.task_id) {
                continue;
            }
            let r_tokens = r.tokens;
            // Don't load a reference if it would bust the budget.
            let identity_wm_tokens = estimate_tokens(&identity_text) + estimate_tokens(&working_memory);
            if identity_wm_tokens + refs_used_tokens + r_tokens > effective_budget {
                continue;
            }
            refs_text.push(format!("\n--- @ref:{} ({}) ---\n{}\n", r.task_id, r.title, r.content));
            refs_used_tokens += r_tokens;
            self.cache.mark_loaded(&r.task_id);
        }

        let refs_combined: String = refs_text.join("\n");
        let total = estimate_tokens(&identity_text)
            + estimate_tokens(&working_memory)
            + estimate_tokens(&refs_combined)
            + estimate_tokens(task_prompt);

        AssembledContext {
            identity: identity_text,
            working_memory,
            references: refs_text,
            task_prompt: task_prompt.to_string(),
            total_tokens: total,
            budget: effective_budget,
        }
    }

    /// Build a context from a task's self-summary (Layer A only)
    /// when no live working memory is available.
    pub fn assemble_from_task(
        &mut self,
        identity: &IdentitySlice,
        task: &TaskNode,
        budget: TokenBudget,
    ) -> AssembledContext {
        let identity_text = render_identity(identity);
        let mut prompt_parts = Vec::new();
        prompt_parts.push(format!("📋 当前任务\n{}\n", task.spec));
        prompt_parts.push(format!(
            "📦 你之前的产出摘要（~50字）：{}\n",
            task.summary_a.as_deref().unwrap_or("（尚无）"),
        ));
        // P3 — Layer B: include stored key points when available
        if let Some(ref b) = task.summary_b {
            prompt_parts.push(format!("📌 关键点（~200字）：{}\n", b));
        }
        let task_prompt = prompt_parts.join("\n");
        let total = estimate_tokens(&identity_text) + estimate_tokens(&task_prompt);
        AssembledContext {
            identity: identity_text,
            working_memory: String::new(),
            references: vec![],
            task_prompt,
            total_tokens: total,
            budget: budget.effective(),
        }
    }
}

// ── Rendering helpers ─────────────────────────────────

fn render_identity(slice: &IdentitySlice) -> String {
    let teammates = if slice.teammates.is_empty() {
        "（无）".to_string()
    } else {
        slice
            .teammates
            .iter()
            .map(|t| format!("  - {} ({}) — {}", t.name, t.role, t.id))
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n\
         👤 身份：{name} [{id}]\n\
         📍 房间：{room}\n\
         📜 角色：{role}\n\n\
         🏅 段位状态：\n{rank}\n\n\
         📏 房间规则：\n{rules}\n\n\
         👥 当前成员：\n{teammates}\n\n\
         ⚠️  生存法则：\n{survival}\n\
         ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n",
        name = slice.agent_name,
        id = slice.agent_id,
        room = slice.room_name,
        role = slice.role_description,
        rank = slice.rank_status,
        rules = slice.room_rules,
        teammates = teammates,
        survival = slice.survival_rules,
    )
}

fn render_working_memory(turns: &[Turn], budget: u32) -> String {
    // Drop earliest turns until we fit budget.
    let mut kept: Vec<&Turn> = turns.iter().collect();
    while !kept.is_empty() {
        let total: u32 = kept.iter().map(|t| t.tokens).sum();
        if total <= budget {
            break;
        }
        kept.remove(0); // drop oldest
    }
    if kept.is_empty() {
        return String::new();
    }
    let mut out = String::from("━━━━━━ 最近工作记忆 ━━━━━━\n");
    for t in &kept {
        if t.system {
            out.push_str(&format!("[SYS] {}\n", t.content));
        } else {
            out.push_str(&format!("[{}] {}\n", t.sender, t.content));
        }
    }
    out.push_str("━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    out
}

/// Crude token estimator: ~3 chars/token for mixed CJK/Latin.
/// Production code would use tiktoken, but we don't pull that dep.
pub fn estimate_tokens(s: &str) -> u32 {
    // Round-up division: ceil(s.len() / 3).
    ((s.chars().count() as u32) + 2) / 3
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> IdentitySlice {
        IdentitySlice {
            agent_id: "writer-1".into(),
            agent_name: "李白".into(),
            role_description: "负责写作章节内容".into(),
            room_name: "长篇小说《长安》".into(),
            room_rules: "风格：仿古风。每章 ≥ 2000 字。".into(),
            teammates: vec![TeammateCard {
                id: "reviewer".into(),
                name: "杜甫".into(),
                role: "审稿".into(),
            }],
            rank_status: "🥈 白银II (积分: 1240)\n  Token 预算: 12K/轮\n  距下一段位: 还差 60 分到黄金I".into(),
            survival_rules:
                "积分 <= 0 → 开除（离开房间，积分清零）。\n\
                 任务完成 +10，review 通过 +5，驳回 -5，失败 -15。"
                    .into(),
            current_spec: "写第一章：主角初登场".into(),
        }
    }

    #[test]
    fn identity_is_always_full() {
        let mut cache = ReferenceCache::new();
        let mut a = ContextAssembler::new(&mut cache);
        let result = a.assemble(
            &identity(),
            &[],
            "",
            "请开始第一章。",
            &[],
            TokenBudget {
                base: 12_000,
                performance_bonus: 0,
                consecutive_bonus: 0,
            },
        );
        // Identity slice always present, regardless of budget.
        assert!(result.identity.contains("李白"));
        assert!(result.identity.contains("生存法则"));
        // Identity chars are concentrated — should easily clear 100 tokens.
        assert!(result.identity.chars().count() > 100);
    }

    #[test]
    fn drops_oldest_turns_when_over_budget() {
        let mut cache = ReferenceCache::new();
        let mut a = ContextAssembler::new(&mut cache);
        let turns: Vec<Turn> = (0..20)
            .map(|i| Turn {
                sender: format!("user-{i}"),
                content: "x".repeat(500), // ~167 tokens each
                tokens: 200,
                system: false,
            })
            .collect();
        let result = a.assemble(
            &identity(),
            &turns,
            "",
            "下一段。",
            &[],
            TokenBudget {
                base: 6_000,
                performance_bonus: 0,
                consecutive_bonus: 0,
            },
        );
        // Should have dropped earlier turns, kept some recent ones.
        assert!(result.total_tokens <= result.budget + 200);
        assert!(result.working_memory.contains("user-19")); // last kept
        assert!(!result.working_memory.contains("user-0")); // first dropped
    }

    #[test]
    fn references_are_cached_across_calls() {
        let mut cache = ReferenceCache::new();
        let r1 = Reference {
            task_id: "task-1".into(),
            title: "第一章".into(),
            content: "曾梦想仗剑走天涯".into(),
            tokens: 50,
        };
        let r2 = Reference {
            task_id: "task-1".into(),
            title: "第一章".into(),
            content: "曾梦想仗剑走天涯".into(),
            tokens: 50,
        };

        {
            let mut a = ContextAssembler::new(&mut cache);
            let res = a.assemble(&identity(), &[], "", "p", &[r1], TokenBudget { base: 12_000, performance_bonus: 0, consecutive_bonus: 0 });
            assert_eq!(res.references.len(), 1);
        }
        {
            let mut a = ContextAssembler::new(&mut cache);
            let res = a.assemble(&identity(), &[], "", "p", &[r2], TokenBudget { base: 12_000, performance_bonus: 0, consecutive_bonus: 0 });
            assert_eq!(res.references.len(), 0, "second call should skip cached ref");
        }
    }

    #[test]
    fn assemble_from_task_emits_summary() {
        let mut cache = ReferenceCache::new();
        let mut a = ContextAssembler::new(&mut cache);
        let task = TaskNode {
            id: "t1".into(),
            room_id: "r".into(),
            parent_id: None,
            phase: crate::groupchat::task_types::TaskPhase::Executing,
            description: "第一章".into(),
            spec: "主角登场".into(),
            assigned_agent: "writer".into(),
            output: "曾梦想...".into(),
            review: None,
            summary_a: Some("主角李白出场".into()),
            summary_b: None,
            sort_order: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let res = a.assemble_from_task(&identity(), &task, TokenBudget { base: 12_000, performance_bonus: 0, consecutive_bonus: 0 });
        assert!(res.task_prompt.contains("主角登场"));
        assert!(res.task_prompt.contains("主角李白出场"));
    }

    #[test]
    fn summary_layer_is_empty() {
        use crate::groupchat::SummaryLayer;
        let empty = SummaryLayer { a: None, b: None, c: None };
        assert!(empty.is_empty());
        let partial = SummaryLayer { a: Some("hi".into()), b: None, c: None };
        assert!(!partial.is_empty());
    }
}