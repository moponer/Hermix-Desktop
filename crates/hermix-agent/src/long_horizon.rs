//! Long-horizon task briefing (Phase 1.1).
//!
//! When an agent task is expected to take more than ~10 turns or the
//! user explicitly requests long-running execution, we generate a
//! `TaskBrief` that codifies success criteria, audit gates, and
//! effort floors. This prevents the agent from spinning endlessly
//! on ambiguous tasks.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// A structured brief for a long-running task. Created *before* the
/// agent starts execution so that every turn can check progress
/// against well-defined criteria.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskBrief {
    /// Precise success predicate — a human-readable condition that
    /// must be true for the task to be considered complete.
    pub success_predicate: String,
    /// Outcomes that do NOT count toward success (e.g. "compiles but
    /// test suite fails", "refactored but broke public API").
    pub non_counting_outcomes: Vec<String>,
    /// How the agent should persist state across turns.
    pub persistence_rule: PersistenceRule,
    /// Condition under which the agent should pause and wait for
    /// human review before continuing.
    pub audit_gate: AuditGate,
    /// Minimum effort the agent must invest before it's allowed to
    /// declare failure. Prevents premature giving-up on hard tasks.
    #[serde(with = "duration_secs")]
    pub effort_floor: Duration,
    /// Optional strategy for exploring diverse approaches when the
    /// task supports parallel execution paths.
    pub diversity_policy: Option<DiversityPolicy>,
    /// When this brief was created.
    pub created_at: DateTime<Utc>,
}

/// How the agent persists intermediate state across turns.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PersistenceRule {
    /// Write progress to a scratch file each turn.
    ScratchFile(String),
    /// Store in memory.db with a given key prefix.
    MemoryKey(String),
    /// No persistence — the brief is stateless.
    None,
}

/// When the agent should pause for human review.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuditGate {
    /// Check every N turns.
    pub check_interval: u32,
    /// If true, pause and wait for human approval after each check.
    pub require_approval: bool,
    /// Custom condition that triggers an audit (e.g. "tests pass").
    pub condition: Option<String>,
}

impl AuditGate {
    /// Check whether an audit should happen at the given turn number.
    pub fn should_audit(&self, turn: u32) -> bool {
        turn > 0 && turn % self.check_interval == 0
    }
}

/// Strategy for exploring diverse execution paths.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DiversityPolicy {
    /// Try N different approaches and pick the best.
    BestOfN { n: usize },
    /// Branch and merge: spawn sub-agents, then merge results.
    BranchAndMerge,
    /// No diversity — single-path execution.
    None,
}

// ---------------------------------------------------------------------------
// Duration serde helper
// ---------------------------------------------------------------------------

mod duration_secs {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(dur: &Duration, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_u64(dur.as_secs())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Duration, D::Error> {
        let secs = u64::deserialize(de)?;
        Ok(Duration::from_secs(secs))
    }
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Fluent builder for `TaskBrief`.
#[derive(Debug, Clone)]
pub struct TaskBriefBuilder {
    success_predicate: String,
    non_counting_outcomes: Vec<String>,
    persistence_rule: PersistenceRule,
    audit_gate: AuditGate,
    effort_floor: Duration,
    diversity_policy: Option<DiversityPolicy>,
}

impl TaskBriefBuilder {
    /// Start building a brief with the given success predicate.
    pub fn new(success_predicate: impl Into<String>) -> Self {
        Self {
            success_predicate: success_predicate.into(),
            non_counting_outcomes: Vec::new(),
            persistence_rule: PersistenceRule::None,
            audit_gate: AuditGate {
                check_interval: 5,
                require_approval: false,
                condition: None,
            },
            effort_floor: Duration::from_secs(0),
            diversity_policy: None,
        }
    }

    /// Add a non-counting outcome.
    pub fn non_counting_outcome(mut self, outcome: impl Into<String>) -> Self {
        self.non_counting_outcomes.push(outcome.into());
        self
    }

    /// Set the persistence rule.
    pub fn persistence_rule(mut self, rule: PersistenceRule) -> Self {
        self.persistence_rule = rule;
        self
    }

    /// Set the audit gate check interval (in turns).
    pub fn audit_interval(mut self, interval: u32) -> Self {
        self.audit_gate.check_interval = interval.max(1);
        self
    }

    /// Require human approval at each audit gate.
    pub fn require_audit_approval(mut self) -> Self {
        self.audit_gate.require_approval = true;
        self
    }

    /// Set a custom audit condition.
    pub fn audit_condition(mut self, condition: impl Into<String>) -> Self {
        self.audit_gate.condition = Some(condition.into());
        self
    }

    /// Set the effort floor (minimum time before giving up).
    pub fn effort_floor(mut self, duration: Duration) -> Self {
        self.effort_floor = duration;
        self
    }

    /// Set the diversity policy.
    pub fn diversity_policy(mut self, policy: DiversityPolicy) -> Self {
        self.diversity_policy = Some(policy);
        self
    }

    /// Build the `TaskBrief`.
    pub fn build(self) -> TaskBrief {
        TaskBrief {
            success_predicate: self.success_predicate,
            non_counting_outcomes: self.non_counting_outcomes,
            persistence_rule: self.persistence_rule,
            audit_gate: self.audit_gate,
            effort_floor: self.effort_floor,
            diversity_policy: self.diversity_policy,
            created_at: Utc::now(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_brief_roundtrip_json() {
        let brief = TaskBriefBuilder::new("All tests pass")
            .non_counting_outcome("Compiles but tests fail")
            .persistence_rule(PersistenceRule::ScratchFile("task.md".to_string()))
            .audit_interval(5)
            .require_audit_approval()
            .effort_floor(Duration::from_secs(300))
            .diversity_policy(DiversityPolicy::BestOfN { n: 3 })
            .build();

        let json = serde_json::to_string(&brief).unwrap();
        let deserialized: TaskBrief = serde_json::from_str(&json).unwrap();

        assert_eq!(brief.success_predicate, deserialized.success_predicate);
        assert_eq!(brief.non_counting_outcomes, deserialized.non_counting_outcomes);
        assert_eq!(brief.persistence_rule, deserialized.persistence_rule);
    }

    #[test]
    fn audit_gate_check_interval() {
        let gate = AuditGate {
            check_interval: 5,
            require_approval: false,
            condition: None,
        };
        assert!(!gate.should_audit(0));
        assert!(!gate.should_audit(3));
        assert!(gate.should_audit(5));
        assert!(!gate.should_audit(7));
        assert!(gate.should_audit(10));
    }

    #[test]
    fn builder_defaults() {
        let brief = TaskBriefBuilder::new("Done").build();
        assert_eq!(brief.success_predicate, "Done");
        assert!(brief.non_counting_outcomes.is_empty());
        assert_eq!(brief.persistence_rule, PersistenceRule::None);
        assert_eq!(brief.audit_gate.check_interval, 5);
        assert!(!brief.audit_gate.require_approval);
        assert!(brief.audit_gate.condition.is_none());
        assert_eq!(brief.effort_floor, Duration::from_secs(0));
        assert!(brief.diversity_policy.is_none());
    }
}
