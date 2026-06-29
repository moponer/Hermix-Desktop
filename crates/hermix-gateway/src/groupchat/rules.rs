
use chrono::Utc;

use super::persistence::*;
use super::persistence::RoomRules;

/// Rule engine for monitoring agent behavior and enforcing permissions.
pub struct RuleEngine;

/// Behavior record for scoring.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentBehavior {
    pub agent_id: String,
    pub room_id: String,
    pub score: i32,
    pub violations: Vec<Violation>,
    pub reflections: Vec<String>,
    pub is_active: bool,
    pub fired_at: Option<String>,
    pub fired_reason: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Violation {
    pub rule: String,
    pub severity: ViolationSeverity,
    pub deduction: i32,
    pub detected_at: String,
    pub context: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum ViolationSeverity {
    Warning,
    Minor,
    Major,
    Critical,
}

impl ViolationSeverity {
    pub fn deduction(&self) -> i32 {
        match self {
            ViolationSeverity::Warning => 1,
            ViolationSeverity::Minor => 2,
            ViolationSeverity::Major => 3,
            ViolationSeverity::Critical => 5,
        }
    }
}

impl AgentBehavior {
    pub fn new(agent_id: &str, room_id: &str, initial_score: i32) -> Self {
        Self {
            agent_id: agent_id.to_string(),
            room_id: room_id.to_string(),
            score: initial_score,
            violations: Vec::new(),
            reflections: Vec::new(),
            is_active: true,
            fired_at: None,
            fired_reason: None,
        }
    }

    pub fn add_violation(&mut self, rule: &str, severity: ViolationSeverity, context: &str) {
        let deduction = severity.deduction();
        self.violations.push(Violation {
            rule: rule.to_string(),
            severity,
            deduction,
            detected_at: Utc::now().to_rfc3339(),
            context: context.to_string(),
        });
        self.score -= deduction;
        if self.score < 0 {
            self.score = 0;
        }
    }

    pub fn needs_reflection(&self, threshold: i32) -> bool {
        self.score < threshold && self.is_active
    }

    pub fn pass_reflection(&mut self, bonus: i32) {
        self.score += bonus;
        self.reflections.push(format!(
            "反思通过，获得 {} 分奖励",
            bonus
        ));
    }

    pub fn fire(&mut self, reason: &str) {
        self.is_active = false;
        self.fired_at = Some(Utc::now().to_rfc3339());
        self.fired_reason = Some(reason.to_string());
    }
}

impl RuleEngine {
    pub fn new() -> Self {
        Self
    }

    /// Check if an agent's action violates its role permissions.
    pub fn check_violation(
        &self,
        agent_id: &str,
        tool_name: &str,
        rules: &RoomRules,
        role: &AgentRole,
    ) -> Option<Violation> {
        let denied = rules.effective_denied_tools(agent_id, role);

        if denied.iter().any(|d| d == tool_name) {
            // Determine severity based on where the restriction comes from
            let is_builtin = builtin_role_denied_tools(role).contains(&tool_name);
            let severity = if is_builtin {
                ViolationSeverity::Major
            } else {
                ViolationSeverity::Minor
            };

            return Some(Violation {
                rule: format!("{} 规则禁止使用 {}", role, tool_name),
                severity: severity.clone(),
                deduction: severity.deduction(),
                detected_at: Utc::now().to_rfc3339(),
                context: format!("Agent {} ({}) tried to use {}", agent_id, role, tool_name),
            });
        }

        // Check whitelist mode
        if rules.tool_policy.mode == ToolPolicyMode::Whitelist {
            if !rules.tool_policy.whitelisted.is_empty()
                && !rules.tool_policy.whitelisted.contains(&tool_name.to_string())
            {
                return Some(Violation {
                    rule: format!("白名单模式：{} 不在允许列表中", tool_name),
                    severity: ViolationSeverity::Minor,
                    deduction: 1,
                    detected_at: Utc::now().to_rfc3339(),
                    context: format!("Agent {} tried to use {}", agent_id, tool_name),
                });
            }
        }

        None
    }

    /// Check if an agent needs reflection based on its score.
    pub fn needs_reflection(&self, behavior: &AgentBehavior, threshold: i32) -> bool {
        behavior.needs_reflection(threshold)
    }
}
