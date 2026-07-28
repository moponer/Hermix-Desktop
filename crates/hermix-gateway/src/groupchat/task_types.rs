//! Data structures for the group chat task engine.
//!
//! S0 foundation: TaskNode, GlobalRank, TokenBudget, YanluoConfig,
//! ViolationRecord, SummaryLayer — everything the engine needs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── Task Phase & Node ─────────────────────────────────

/// Lifecycle phase of a single task node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPhase {
    /// No active task — room is idle.
    Idle,
    /// Leader drafting plan, team discussing.
    Planning,
    /// Subtask in progress (agent is working).
    Executing,
    /// Output awaiting review.
    Review,
    /// Review rejected — agent revising.
    Revising,
    /// All subtasks done.
    Completed,
    /// User cancelled.
    Cancelled,
    /// Multi-agent discussion in progress.
    Discussing,
}

/// A single node in the task tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskNode {
    pub id: String,
    pub room_id: String,
    pub parent_id: Option<String>,
    pub phase: TaskPhase,
    /// Human-readable description.
    pub description: String,
    /// Detailed spec (user may update this mid-task).
    pub spec: String,
    /// Which agent this is assigned to.
    pub assigned_agent: String,
    /// Most recent output from the agent.
    pub output: String,
    /// Latest review result (None = not yet reviewed).
    pub review: Option<ReviewResult>,
    /// Layer-A summary (agent self-summary, ~50 chars).
    pub summary_a: Option<String>,
    /// Layer-B summary (phase summary, ~200 chars).
    pub summary_b: Option<String>,
    /// Ordered index within parent.
    pub sort_order: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Outcome of a review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewResult {
    pub passed: bool,
    pub comment: String,
    pub reviewer_id: String,
    pub reviewed_at: DateTime<Utc>,
}

// ── Global Rank ────────────────────────────────────────

/// Global rank that follows the agent across all rooms.
/// Persisted alongside `AgentPersona` — deleted only when the
/// agent itself is deleted from the system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalRank {
    pub agent_id: String,
    pub score: i32,
    pub total_tasks_completed: u32,
    pub consecutive_passes: u32,
    pub max_streak: u32,
    pub created_at: DateTime<Utc>,
    pub last_updated: DateTime<Utc>,
}

/// Per-room offset applied on top of `GlobalRank.score`.
/// A member entering a room starts with `global + delta`.
/// When score <= 0 they are expelled — global score stays.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomRankShadow {
    pub agent_id: String,
    pub room_id: String,
    /// Bonus or penalty accumulated in this room.
    pub delta: i32,
    pub entered_at: DateTime<Utc>,
    pub last_active: DateTime<Utc>,
}

/// Effective score = global.score + room.delta
impl RoomRankShadow {
    pub fn effective_score(&self, global: &GlobalRank) -> i32 {
        global.score + self.delta
    }
}

// ── Token Budget ───────────────────────────────────────

/// Token budget for a single agent, computed from rank.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBudget {
    /// Base budget determined by tier.
    pub base: u32,
    /// Performance adjustment.
    pub performance_bonus: i32,
    /// Consecutive-pass bonus.
    pub consecutive_bonus: u32,
}

impl TokenBudget {
    /// Floor: 4K, ceiling: 48K.
    pub fn effective(&self) -> u32 {
        let raw = self.base as i32 + self.performance_bonus + self.consecutive_bonus as i32;
        raw.clamp(4_000, 48_000) as u32
    }
}

impl From<&super::rank::RankTier> for TokenBudget {
    fn from(tier: &super::rank::RankTier) -> Self {
        use super::rank::RankTier::*;
        let base = match tier {
            Bronze => 6_000,
            Silver => 12_000,
            Gold => 20_000,
            Platinum => 24_000,
            Diamond => 28_000,
            Star => 32_000,
            King => 36_000,
            KingLegend => 40_000,
        };
        Self {
            base,
            performance_bonus: 0,
            consecutive_bonus: 0,
        }
    }
}

// ── Summary Layers ─────────────────────────────────────

/// Lightweight summary archive for an agent's session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryLayer {
    /// Layer A — instant summary, ~50 chars.
    pub a: Option<String>,
    /// Layer B — phase summary, ~200 chars.
    pub b: Option<String>,
    /// Layer C — full project archive, ~500 chars.
    pub c: Option<String>,
}

impl SummaryLayer {
    pub fn is_empty(&self) -> bool {
        self.a.is_none() && self.b.is_none() && self.c.is_none()
    }
}

// ── Yanluo (The Auditor) ───────────────────────────────

/// Per-room configuration for Yanluo, the audit and judgment engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YanluoConfig {
    /// Master switch.
    pub enabled: bool,
    /// LLM model to use for semantic-heavy checks.
    pub model: Option<String>,
    /// Individual detector toggles.
    pub detectors: DetectorSwitches,
    /// Penalty multiplier (1.0 = normal).
    pub penalty_multiplier: f64,
    /// Warning → Severe threshold.
    pub warning_to_severe: u32,
    /// Severe → Critical threshold.
    pub severe_to_critical: u32,
    /// Whether to broadcast on every violation.
    pub notify_on_violation: bool,
    /// Whether to broadcast on every penalty.
    pub notify_on_penalty: bool,
    /// Allow permanent ban.
    pub allow_permanent_ban: bool,
}

impl Default for YanluoConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model: None,
            detectors: DetectorSwitches::default(),
            penalty_multiplier: 1.0,
            warning_to_severe: 3,
            severe_to_critical: 2,
            notify_on_violation: true,
            notify_on_penalty: true,
            allow_permanent_ban: false,
        }
    }
}

/// Per-detector on/off switches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectorSwitches {
    pub content_quality: bool,
    pub summary_consistency: bool,
    pub loop_detector: bool,
    pub action_validator: bool,
    pub cross_room_guard: bool,
    pub tone_analyzer: bool,
}

impl Default for DetectorSwitches {
    fn default() -> Self {
        Self {
            content_quality: true,
            summary_consistency: true,
            loop_detector: true,
            action_validator: true,
            cross_room_guard: true,
            tone_analyzer: true,
        }
    }
}

// ── Violation Record ───────────────────────────────────

/// Severity level of a detected violation (Yanluo-specific).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViolationLevel {
    Warning,
    Severe,
    Critical,
    RedLine,
}

/// A single violation event recorded by Yanluo.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViolationRecord {
    pub id: String,
    pub room_id: String,
    pub agent_id: String,
    pub violation_type: String,
    pub severity: ViolationLevel,
    pub deduction: i32,
    pub score_before: i32,
    pub score_after: i32,
    pub evidence: String,
    pub detail: Option<String>,
    pub status: RecordStatus,
    pub created_at: DateTime<Utc>,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub review_comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordStatus {
    Pending,
    Confirmed,
    Disputed,
    Overturned,
}
