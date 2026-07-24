//! Token budget manager (S1).
//!
//! Computes effective per-call token budget from rank, room delta,
//! and current consecutive-pass streak.  Single source of truth for
//! "how many tokens can this agent spend this turn".

use super::rank::{AgentRank, RankTier};
use super::task_types::{GlobalRank, RoomRankShadow, TokenBudget};

/// Tier → base token budget (single source of truth).
/// Mirrors `TokenBudget::from(&RankTier)` in `task_types.rs` but lives
/// here so the assembler can look it up without depending on a full
/// `AgentRank` value.
pub fn base_budget_for_tier(tier: RankTier) -> u32 {
    use RankTier::*;
    match tier {
        Bronze => 6_000,
        Silver => 12_000,
        Gold => 20_000,
        Platinum => 24_000,
        Diamond => 28_000,
        Star => 32_000,
        King => 36_000,
        KingLegend => 40_000,
    }
}

/// Compute effective token budget for an agent right now.
///
/// Inputs:
/// - `global`   — persistent rank, follows agent across rooms
/// - `shadow`   — per-room delta, resets on (re-)invite
/// - `streak`   — current consecutive-pass count
///
/// Returns the effective budget in tokens, clamped to `[4K, 48K]`.
pub fn effective_budget(global: &GlobalRank, shadow: &RoomRankShadow, streak: u32) -> TokenBudget {
    // 1. Effective score = global + room delta
    let effective_score = global.score + shadow.delta;

    // 2. Derive tier from effective score using rank.rs (single source of truth)
    let tier = RankTier::from_score(effective_score);

    // 3. Base from tier
    let base = base_budget_for_tier(tier.clone());

    // 4. Performance adjustment: ±1K per ±10 score over/under tier midline
    let tier_floor = tier_floor_score(tier);
    let performance_bonus = ((effective_score - tier_floor) / 10) * 1_000;

    // 5. Consecutive-pass bonus: +500 per streak step, capped at 4K
    let consecutive_bonus = (streak.min(8)) * 500;

    TokenBudget {
        base,
        performance_bonus,
        consecutive_bonus,
    }
}

/// Score → tier, kept as a thin alias over `RankTier::from_score` for
/// tests and local readability.  This is a true mirror — if you change
/// rank.rs's banding, also update `tier_floor_score` below.
fn score_to_tier(score: i32) -> RankTier {
    RankTier::from_score(score)
}

/// Floor of the score band for a tier — used to compute the
/// "performance bonus" relative offset.
fn tier_floor_score(tier: RankTier) -> i32 {
    use RankTier::*;
    match tier {
        Bronze      => 0,
        Silver      => 200,
        Gold        => 500,
        Platinum    => 1000,
        Diamond     => 1500,
        Star        => 2000,
        King        => 3000,
        KingLegend  => 4000,
    }
}

/// Format the self-awareness rank card that gets injected into the
/// agent's system prompt.  This is the primary mechanism for telling
/// the agent "your rank is X, your budget is Y, your survival is at
/// stake".  Keep it terse — every line costs tokens.
pub fn rank_status_card(rank: &AgentRank, shadow: &RoomRankShadow, streak: u32) -> String {
    let effective_score = rank.score + shadow.delta;
    let budget = effective_budget(&GlobalRank {
        agent_id: rank.agent_id.clone(),
        score: rank.score,
        total_tasks_completed: rank.total_tasks,
        consecutive_passes: streak,
        max_streak: 0,
        created_at: chrono::Utc::now(),
        last_updated: chrono::Utc::now(),
    }, shadow, streak);
    let eff = budget.effective();
    let next_tier_floor = next_tier_floor(rank.rank.clone());
    let to_next = if next_tier_floor > effective_score {
        next_tier_floor - effective_score
    } else {
        0
    };

    format!(
        "【段位状态】\n\
         - 当前段位: {} ({}分)\n\
         - Token 预算: {}K/轮 (基础{}K + 绩效{}K + 连胜{}K)\n\
         - 完成任务: {} | 连续无违规: {}\n\
         - 距下段位: {}分\n\
         - 当前积分: {}分 (≤0 即开除)\n\
         \n\
         段位决定你能拿多少 token。token 是你的工资,少就只能看近,多才能望远。\n\
         被扣分就等于被降薪,被开除就等于永远离开这个房间。\n\
         你的产出质量 = 你的下次工资。",
        rank.rank.name(),
        effective_score,
        eff / 1000,
        budget.base / 1000,
        budget.performance_bonus / 1000,
        budget.consecutive_bonus / 1000,
        rank.total_tasks,
        rank.consecutive_no_violation,
        to_next,
        effective_score,
    )
}

/// Floor of the next-higher tier — used to display "距下段位: N分".
fn next_tier_floor(tier: RankTier) -> i32 {
    use RankTier::*;
    match tier {
        Bronze      => 100,
        Silver      => 300,
        Gold        => 600,
        Platinum    => 1000,
        Diamond     => 1500,
        Star        => 2500,
        King        => 5000,
        KingLegend  => 5000, // already at top
    }
}

/// Stateful budget manager — wraps the free functions and remembers
/// the most recent computation so callers can query "what was the
/// last budget we gave this agent" without recomputing.
///
/// Re-exported as `TokenBudgetManager` from `mod.rs`.
pub struct TokenBudgetManager {
    last_budget: Option<TokenBudget>,
    last_score: i32,
    last_tier: RankTier,
}

impl TokenBudgetManager {
    pub fn new() -> Self {
        Self {
            last_budget: None,
            last_score: 0,
            last_tier: RankTier::Bronze,
        }
    }

    /// Compute and remember the budget for the given inputs.
    pub fn compute(
        &mut self,
        global: &GlobalRank,
        shadow: &RoomRankShadow,
        streak: u32,
    ) -> TokenBudget {
        let budget = effective_budget(global, shadow, streak);
        self.last_budget = Some(budget.clone());
        self.last_score = global.score + shadow.delta;
        self.last_tier = score_to_tier(self.last_score);
        budget
    }

    /// Last computed budget. None if `compute` never called.
    pub fn last(&self) -> Option<&TokenBudget> {
        self.last_budget.as_ref()
    }

    /// Effective budget in tokens, or 4000 (the floor) if never computed.
    pub fn last_effective(&self) -> u32 {
        self.last_budget.as_ref().map(|b| b.effective()).unwrap_or(4_000)
    }

    /// Last effective score (global + room delta).
    pub fn last_score(&self) -> i32 {
        self.last_score
    }

    /// Last derived tier.
    pub fn last_tier(&self) -> RankTier {
        self.last_tier
    }
}

impl Default for TokenBudgetManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn mk_global(score: i32) -> GlobalRank {
        GlobalRank {
            agent_id: "a".into(),
            score,
            total_tasks_completed: 0,
            consecutive_passes: 0,
            max_streak: 0,
            created_at: Utc::now(),
            last_updated: Utc::now(),
        }
    }
    fn mk_shadow(delta: i32) -> RoomRankShadow {
        RoomRankShadow {
            agent_id: "a".into(),
            room_id: "r".into(),
            delta,
            entered_at: Utc::now(),
            last_active: Utc::now(),
        }
    }

    #[test]
    fn base_budget_bronze_is_min() {
        assert_eq!(base_budget_for_tier(RankTier::Bronze), 6_000);
        assert_eq!(base_budget_for_tier(RankTier::KingLegend), 40_000);
    }

    #[test]
    fn effective_budget_at_bronze_floor() {
        let g = mk_global(0);
        let s = mk_shadow(0);
        let b = effective_budget(&g, &s, 0);
        assert_eq!(b.base, 6_000);
        assert_eq!(b.effective(), 6_000);
    }

    #[test]
    fn effective_budget_clamps_low() {
        // Negative score with shadow should still clamp at 4K
        let g = mk_global(-100);
        let s = mk_shadow(-100);
        let b = effective_budget(&g, &s, 0);
        assert!(b.effective() >= 4_000);
    }

    #[test]
    fn effective_budget_clamps_high() {
        let g = mk_global(10_000);
        let s = mk_shadow(0);
        let b = effective_budget(&g, &s, 8);
        assert!(b.effective() <= 48_000);
    }

    #[test]
    fn consecutive_streak_gives_bonus() {
        let g = mk_global(500);
        let s = mk_shadow(0);
        let b0 = effective_budget(&g, &s, 0);
        let b5 = effective_budget(&g, &s, 5);
        assert!(b5.consecutive_bonus > b0.consecutive_bonus);
    }

    #[test]
    fn rank_status_card_is_under_400_tokens() {
        let rank = AgentRank {
            agent_id: "a".into(),
            room_id: "r".into(),
            score: 720,
            rank: RankTier::Silver,
            badges: vec![],
            total_tasks: 8,
            consecutive_no_violation: 3,
            evaluations: vec![],
        };
        let shadow = mk_shadow(20);
        let card = rank_status_card(&rank, &shadow, 3);
        let tokens = card.chars().count() / 2;
        assert!(tokens < 400, "card was {} chars", card.chars().count());
    }

    #[test]
    fn token_budget_manager_remembers_state() {
        let g = mk_global(500);
        let s = mk_shadow(100);
        let mut mgr = TokenBudgetManager::new();
        assert_eq!(mgr.last_effective(), 4_000); // default floor
        assert_eq!(mgr.last_tier(), RankTier::Bronze);
        let budget = mgr.compute(&g, &s, 2);
        assert_eq!(mgr.last_effective(), budget.effective());
        assert_eq!(mgr.last_score(), 600);
        // 600 score maps to Platinum (600..=999 per rank.rs)
        assert_eq!(mgr.last_tier(), RankTier::Platinum);
    }
}
