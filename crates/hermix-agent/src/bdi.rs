//! BDI (Belief-Desire-Intention) cognitive modeling .
//!
//! Implements a classical BDI architecture where:
//! - **Beliefs**: Facts the agent knows about the world, with confidence
//! - **Desires**: Goals the agent wants to achieve, with priority
//! - **Intentions**: Committed plans the agent is executing
//!
//! The `MentalState::deliberate()` function derives intentions from
//! beliefs + desires using a simple priority-based selection strategy.
//!
//! Persistence: beliefs and desires are stored in SQLite via the
//! `hermix-focus` memory layer, allowing cross-session recovery.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// A belief about the world. Modeled as an RDF-like triple with confidence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Belief {
    /// The entity this belief is about.
    pub subject: String,
    /// The relationship or property.
    pub predicate: String,
    /// The value or target entity.
    pub object: String,
    /// Confidence in [0.0, 1.0].
    pub confidence: f32,
    /// When this belief was last updated.
    pub updated_at: DateTime<Utc>,
}

/// A desire (goal) the agent wants to achieve.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Desire {
    /// Human-readable goal description.
    pub goal: String,
    /// Priority in [0.0, 1.0] — higher = more important.
    pub priority: f32,
    /// Optional deadline for achieving this goal.
    pub deadline: Option<DateTime<Utc>>,
    /// When this desire was created.
    pub created_at: DateTime<Utc>,
}

/// A committed plan step within an intention.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlanStep {
    /// Description of the step.
    pub action: String,
    /// Whether this step has been completed.
    pub completed: bool,
}

/// An intention: a committed plan to achieve a goal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Intention {
    /// The goal this intention pursues.
    pub goal: String,
    /// The planned steps.
    pub plan: Vec<PlanStep>,
    /// When this intention was committed to.
    pub committed_at: DateTime<Utc>,
    /// Current status of the intention.
    pub status: IntentionStatus,
}

/// Status of an intention.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum IntentionStatus {
    /// Currently being executed.
    Active,
    /// Paused pending some condition.
    Suspended,
    /// Successfully completed.
    Achieved,
    /// Failed or abandoned.
    Abandoned,
}

// ---------------------------------------------------------------------------
// MentalState
// ---------------------------------------------------------------------------

/// The complete cognitive state of the agent at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MentalState {
    /// Current beliefs about the world.
    pub beliefs: Vec<Belief>,
    /// Current desires (goals).
    pub desires: Vec<Desire>,
    /// Current intentions (committed plans).
    pub intentions: Vec<Intention>,
}

impl MentalState {
    /// Create an empty mental state.
    pub fn new() -> Self {
        Self::default()
    }

    // -----------------------------------------------------------------------
    // Belief management
    // -----------------------------------------------------------------------

    /// Add or update a belief. If a belief with the same subject+predicate
    /// already exists, its object and confidence are updated.
    pub fn add_belief(
        &mut self,
        subject: impl Into<String>,
        predicate: impl Into<String>,
        object: impl Into<String>,
        confidence: f32,
    ) {
        let subject = subject.into();
        let predicate = predicate.into();
        let object = object.into();
        let confidence = confidence.clamp(0.0, 1.0);

        if let Some(existing) = self
            .beliefs
            .iter_mut()
            .find(|b| b.subject == subject && b.predicate == predicate)
        {
            existing.object = object;
            existing.confidence = confidence;
            existing.updated_at = Utc::now();
        } else {
            self.beliefs.push(Belief {
                subject,
                predicate,
                object,
                confidence,
                updated_at: Utc::now(),
            });
        }
    }

    /// Remove a belief by subject+predicate.
    pub fn remove_belief(&mut self, subject: &str, predicate: &str) {
        self.beliefs
            .retain(|b| !(b.subject == subject && b.predicate == predicate));
    }

    /// Query beliefs by subject.
    pub fn beliefs_about(&self, subject: &str) -> Vec<&Belief> {
        self.beliefs.iter().filter(|b| b.subject == subject).collect()
    }

    /// Query beliefs by predicate type.
    pub fn beliefs_with_predicate(&self, predicate: &str) -> Vec<&Belief> {
        self.beliefs
            .iter()
            .filter(|b| b.predicate == predicate)
            .collect()
    }

    // -----------------------------------------------------------------------
    // Desire management
    // -----------------------------------------------------------------------

    /// Add a desire. If a desire with the same goal already exists,
    /// its priority is updated.
    pub fn add_desire(
        &mut self,
        goal: impl Into<String>,
        priority: f32,
        deadline: Option<DateTime<Utc>>,
    ) {
        let goal = goal.into();
        let priority = priority.clamp(0.0, 1.0);

        if let Some(existing) = self.desires.iter_mut().find(|d| d.goal == goal) {
            existing.priority = priority;
            existing.deadline = deadline;
        } else {
            self.desires.push(Desire {
                goal,
                priority,
                deadline,
                created_at: Utc::now(),
            });
        }
    }

    /// Remove a desire by goal.
    pub fn remove_desire(&mut self, goal: &str) {
        self.desires.retain(|d| d.goal != goal);
    }

    /// Get desires sorted by priority (highest first).
    pub fn prioritized_desires(&self) -> Vec<&Desire> {
        let mut desires: Vec<&Desire> = self.desires.iter().collect();
        desires.sort_by(|a, b| {
            b.priority
                .partial_cmp(&a.priority)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        desires
    }

    // -----------------------------------------------------------------------
    // Intention management
    // -----------------------------------------------------------------------

    /// Commit to an intention (adopt a plan for a goal).
    pub fn commit_intention(&mut self, goal: impl Into<String>, plan: Vec<PlanStep>) {
        let goal = goal.into();
        // Remove any existing active intention for the same goal.
        self.intentions
            .retain(|i| i.goal != goal || i.status != IntentionStatus::Active);

        self.intentions.push(Intention {
            goal,
            plan,
            committed_at: Utc::now(),
            status: IntentionStatus::Active,
        });
    }

    /// Mark the next incomplete step in an intention as done.
    pub fn advance_intention(&mut self, goal: &str) -> bool {
        if let Some(intention) = self
            .intentions
            .iter_mut()
            .find(|i| i.goal == goal && i.status == IntentionStatus::Active)
        {
            if let Some(step) = intention.plan.iter_mut().find(|s| !s.completed) {
                step.completed = true;
                // If all steps are done, mark as achieved.
                if intention.plan.iter().all(|s| s.completed) {
                    intention.status = IntentionStatus::Achieved;
                }
                return true;
            }
        }
        false
    }

    /// Abandon an intention.
    pub fn abandon_intention(&mut self, goal: &str) {
        if let Some(intention) = self
            .intentions
            .iter_mut()
            .find(|i| i.goal == goal && i.status == IntentionStatus::Active)
        {
            intention.status = IntentionStatus::Abandoned;
        }
    }

    /// Get all active intentions.
    pub fn active_intentions(&self) -> Vec<&Intention> {
        self.intentions
            .iter()
            .filter(|i| i.status == IntentionStatus::Active)
            .collect()
    }

    // -----------------------------------------------------------------------
    // Deliberation
    // -----------------------------------------------------------------------

    /// Derive intentions from beliefs + desires.
    /// Strategy:
    /// 1. Sort desires by priority (highest first).
    /// 2. For each desire, check if there's a supporting belief.
    /// 3. If not already committed, create a single-step intention.
    /// 4. Limit the number of active intentions to `max_active`.
    pub fn deliberate(&mut self, max_active: usize) -> Vec<String> {
        let mut new_intentions = Vec::new();
        let current_active = self.active_intentions().len();

        if current_active >= max_active {
            return new_intentions;
        }

        // Collect desires first to avoid borrow conflict with commit_intention.
        let desires: Vec<Desire> = self.prioritized_desires().into_iter().cloned().collect();

        for desire in desires {
            if current_active + new_intentions.len() >= max_active {
                break;
            }

            // Check if we already have an active intention for this goal.
            if self
                .intentions
                .iter()
                .any(|i| i.goal == desire.goal && i.status == IntentionStatus::Active)
            {
                continue;
            }

            // Check if the goal is already achieved (belief supports it).
            if self.beliefs.iter().any(|b| {
                b.predicate == "achieved" && b.object == desire.goal && b.confidence > 0.8
            }) {
                continue;
            }

            // Commit a single-step intention.
            let goal = desire.goal.clone();
            self.commit_intention(
                &goal,
                vec![PlanStep {
                    action: format!("Achieve: {}", goal),
                    completed: false,
                }],
            );
            new_intentions.push(goal);
        }

        new_intentions
    }

    // -----------------------------------------------------------------------
    // Belief update from tool results
    // -----------------------------------------------------------------------

    /// Update beliefs based on tool execution results. This is called
    /// at the end of each agent turn to keep the mental state current.
    pub fn update_from_tool_result(
        &mut self,
        tool_name: &str,
        result_summary: &str,
        success: bool,
    ) {
        let confidence = if success { 0.9 } else { 0.3 };
        self.add_belief(
            format!("tool:{}", tool_name),
            "last_result",
            result_summary.to_string(),
            confidence,
        );

        if !success {
            self.add_belief(
                format!("tool:{}", tool_name),
                "failure_count",
                "incremented",
                0.7,
            );
        }
    }

    // -----------------------------------------------------------------------
    // Persistence helpers
    // -----------------------------------------------------------------------

    /// Serialize the mental state to JSON for storage.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Deserialize from JSON.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Create a summary of the mental state for context injection.
    pub fn context_summary(&self) -> String {
        let mut summary = String::new();

        if !self.beliefs.is_empty() {
            summary.push_str("## Current Beliefs\n");
            for b in &self.beliefs {
                summary.push_str(&format!(
                    "- {} {} {} (confidence: {:.0}%)\n",
                    b.subject,
                    b.predicate,
                    b.object,
                    b.confidence * 100.0
                ));
            }
        }

        if !self.desires.is_empty() {
            summary.push_str("\n## Active Goals\n");
            for d in self.prioritized_desires() {
                summary.push_str(&format!(
                    "- {} (priority: {:.0}%)\n",
                    d.goal,
                    d.priority * 100.0
                ));
            }
        }

        if !self.intentions.is_empty() {
            let active: Vec<_> = self.active_intentions();
            if !active.is_empty() {
                summary.push_str("\n## Committed Intentions\n");
                for i in active {
                    let done = i.plan.iter().filter(|s| s.completed).count();
                    let total = i.plan.len();
                    summary.push_str(&format!(
                        "- {} ({}/{} steps done)\n",
                        i.goal, done, total
                    ));
                }
            }
        }

        summary
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_and_query_beliefs() {
        let mut state = MentalState::new();
        state.add_belief("project", "language", "Rust", 0.95);
        state.add_belief("project", "status", "building", 0.8);

        let beliefs = state.beliefs_about("project");
        assert_eq!(beliefs.len(), 2);

        let lang_beliefs = state.beliefs_with_predicate("language");
        assert_eq!(lang_beliefs.len(), 1);
    }

    #[test]
    fn update_existing_belief() {
        let mut state = MentalState::new();
        state.add_belief("task", "status", "running", 0.8);
        state.add_belief("task", "status", "completed", 0.95);

        let beliefs = state.beliefs_about("task");
        assert_eq!(beliefs.len(), 1);
        assert_eq!(beliefs[0].object, "completed");
    }

    #[test]
    fn remove_belief() {
        let mut state = MentalState::new();
        state.add_belief("x", "y", "z", 0.5);
        state.remove_belief("x", "y");
        assert!(state.beliefs.is_empty());
    }

    #[test]
    fn add_and_prioritize_desires() {
        let mut state = MentalState::new();
        state.add_desire("Fix bug", 0.5, None);
        state.add_desire("Ship release", 0.9, None);
        state.add_desire("Write docs", 0.3, None);

        let prioritized = state.prioritized_desires();
        assert_eq!(prioritized[0].goal, "Ship release");
        assert_eq!(prioritized[1].goal, "Fix bug");
        assert_eq!(prioritized[2].goal, "Write docs");
    }

    #[test]
    fn commit_and_advance_intention() {
        let mut state = MentalState::new();
        state.commit_intention(
            "Refactor module",
            vec![
                PlanStep {
                    action: "Split file".to_string(),
                    completed: false,
                },
                PlanStep {
                    action: "Add tests".to_string(),
                    completed: false,
                },
            ],
        );

        assert_eq!(state.active_intentions().len(), 1);

        // Advance one step.
        assert!(state.advance_intention("Refactor module"));
        let intention = &state.active_intentions()[0];
        assert!(intention.plan[0].completed);
        assert!(!intention.plan[1].completed);

        // Advance to completion.
        assert!(state.advance_intention("Refactor module"));
        // Should be achieved, not active.
        assert!(state.active_intentions().is_empty());
    }

    #[test]
    fn deliberate_derives_intentions() {
        let mut state = MentalState::new();
        state.add_desire("Fix bug #42", 0.9, None);
        state.add_desire("Add logging", 0.5, None);

        let new = state.deliberate(5);
        assert_eq!(new.len(), 2);
        assert_eq!(state.active_intentions().len(), 2);
    }

    #[test]
    fn deliberate_respects_max_active() {
        let mut state = MentalState::new();
        state.add_desire("Goal A", 0.9, None);
        state.add_desire("Goal B", 0.8, None);
        state.add_desire("Goal C", 0.7, None);

        let new = state.deliberate(2);
        assert_eq!(new.len(), 2);
    }

    #[test]
    fn deliberate_skips_achieved_goals() {
        let mut state = MentalState::new();
        state.add_desire("Write tests", 0.8, None);
        state.add_belief("tool:test_runner", "achieved", "Write tests", 0.95);

        let new = state.deliberate(5);
        assert!(new.is_empty(), "should skip already-achieved goals");
    }

    #[test]
    fn update_from_tool_result() {
        let mut state = MentalState::new();
        state.update_from_tool_result("bash", "exit code 0", true);
        state.update_from_tool_result("edit_file", "file not found", false);

        let bash_beliefs = state.beliefs_about("tool:bash");
        assert_eq!(bash_beliefs.len(), 1);
        assert!(bash_beliefs[0].confidence > 0.8);
    }

    #[test]
    fn context_summary_not_empty() {
        let mut state = MentalState::new();
        state.add_belief("project", "language", "Rust", 0.9);
        state.add_desire("Ship v1.0", 0.8, None);
        state.commit_intention(
            "Ship v1.0",
            vec![PlanStep {
                action: "Cut release".to_string(),
                completed: false,
            }],
        );

        let summary = state.context_summary();
        assert!(summary.contains("Current Beliefs"));
        assert!(summary.contains("Active Goals"));
        assert!(summary.contains("Committed Intentions"));
    }

    #[test]
    fn serialization_roundtrip() {
        let mut state = MentalState::new();
        state.add_belief("x", "y", "z", 0.5);
        state.add_desire("Goal", 0.8, None);
        state.commit_intention(
            "Goal",
            vec![PlanStep {
                action: "Step 1".to_string(),
                completed: false,
            }],
        );

        let json = state.to_json().unwrap();
        let restored = MentalState::from_json(&json).unwrap();

        assert_eq!(state.beliefs.len(), restored.beliefs.len());
        assert_eq!(state.desires.len(), restored.desires.len());
        assert_eq!(state.intentions.len(), restored.intentions.len());
    }
}
