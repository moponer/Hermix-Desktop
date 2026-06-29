use chrono::Utc;

/// Agent rank tier (王者荣耀 style).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RankTier {
    Bronze,
    Silver,
    Gold,
    Platinum,
    Diamond,
    Star,
    King,
    KingLegend,
}

impl RankTier {
    pub fn from_score(score: i32) -> Self {
        match score {
            0..=99 => RankTier::Bronze,
            100..=299 => RankTier::Silver,
            300..=599 => RankTier::Gold,
            600..=999 => RankTier::Platinum,
            1000..=1499 => RankTier::Diamond,
            1500..=2499 => RankTier::Star,
            2500..=4999 => RankTier::King,
            _ => RankTier::KingLegend,
        }
    }

    pub fn badge(&self) -> &'static str {
        match self {
            RankTier::Bronze => "🥉",
            RankTier::Silver => "🥈",
            RankTier::Gold => "🥇",
            RankTier::Platinum => "💎",
            RankTier::Diamond => "🔷",
            RankTier::Star => "⭐",
            RankTier::King => "👑",
            RankTier::KingLegend => "👑✨",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            RankTier::Bronze => "青铜",
            RankTier::Silver => "白银",
            RankTier::Gold => "黄金",
            RankTier::Platinum => "铂金",
            RankTier::Diamond => "钻石",
            RankTier::Star => "星耀",
            RankTier::King => "王者",
            RankTier::KingLegend => "王者·传奇",
        }
    }
}

impl std::fmt::Display for RankTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.badge(), self.name())
    }
}

/// Agent rank record.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentRank {
    pub agent_id: String,
    pub room_id: String,
    pub score: i32,
    pub rank: RankTier,
    pub badges: Vec<String>,
    pub total_tasks: u32,
    pub consecutive_no_violation: u32,
    pub evaluations: Vec<Evaluation>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Evaluation {
    pub evaluator: String,
    pub score: i32,
    pub comment: String,
    pub created_at: String,
}

/// Rank manager — handles scoring, ranking, badges, and evaluations.
pub struct RankManager;

impl RankManager {
    pub fn new() -> Self {
        Self
    }

    /// Create a new agent rank with initial score.
    pub fn create_rank(agent_id: &str, room_id: &str) -> AgentRank {
        AgentRank {
            agent_id: agent_id.to_string(),
            room_id: room_id.to_string(),
            score: 10,
            rank: RankTier::Bronze,
            badges: vec!["🥉".to_string()],
            total_tasks: 0,
            consecutive_no_violation: 0,
            evaluations: Vec::new(),
        }
    }

    /// Add score from task completion.
    pub fn add_task_score(rank: &mut AgentRank, quality_bonus: bool, extra_bonus: bool) {
        rank.score += 10;
        rank.total_tasks += 1;
        rank.consecutive_no_violation += 1;
        if quality_bonus {
            rank.score += 5;
        }
        if extra_bonus {
            rank.score += 3;
        }
        // Consecutive no-violation bonus
        if rank.consecutive_no_violation >= 5 {
            rank.score += 2;
        }
        rank.rank = RankTier::from_score(rank.score);
    }

    /// Record a violation and return new score.
    pub fn record_violation(rank: &mut AgentRank, deduction: i32) {
        rank.score -= deduction;
        if rank.score < 0 {
            rank.score = 0;
        }
        rank.consecutive_no_violation = 0;
        rank.rank = RankTier::from_score(rank.score);
    }

    /// Record evaluation from leader.
    pub fn record_evaluation(rank: &mut AgentRank, evaluator: &str, score: i32, comment: &str) {
        rank.evaluations.push(Evaluation {
            evaluator: evaluator.to_string(),
            score,
            comment: comment.to_string(),
            created_at: Utc::now().to_rfc3339(),
        });
        // Bonus for high evaluation
        if score >= 4 {
            rank.score += 5;
            rank.rank = RankTier::from_score(rank.score);
        }
    }

    /// Reset rank after firing (score to 0, keep history).
    pub fn reset_after_firing(rank: &mut AgentRank) {
        rank.score = 0;
        rank.rank = RankTier::Bronze;
        rank.consecutive_no_violation = 0;
        // Keep badges as historical record
    }
}
