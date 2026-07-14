//! Orchestrator mode gate — Task-level Craft/Plan/Ask enforcement.
//!
//! (Phase 2 — P2-2)
//!
//! The mode gate sits between the agent loop and the tool execution layer.
//! It enforces the Task's working mode:
//!
//! - **Craft** — no restrictions. Tools execute immediately, no plan
//!   confirmation step. The agent loop runs as-is.
//! - **Plan** — the agent first produces a plan (visible text describing
//!   what it intends to do). The loop pauses for user confirmation before
//!   executing any tools. After confirmation, the task transitions
//!   `planning → in_progress` and subsequent turns run without pausing.
//! - **Ask** — read-only mode. Write-class tools are filtered out of the
//!   tool schema list sent to the LLM, so the model physically cannot
//!   call them. If a write tool somehow slips through (e.g. via a plugin),
//!   the gate blocks it at execution time.
//!
//! The gate is designed to be non-invasive: it wraps the existing
//! `AgentLoop` rather than modifying its internals. The `AgentLoop` calls
//! `ModeGate::filter_tool_schemas()` before sending tools to the LLM, and
//! `ModeGate::check_tool_execution()` before dispatching each tool call.

use std::collections::HashSet;

use hermix_core::{TaskMode, ToolCall, ToolSchema};
#[cfg(test)]
use hermix_core::JsonSchema;

// ---------------------------------------------------------------------------
// Write-class tool classification
// ---------------------------------------------------------------------------

/// Tool names that perform filesystem mutations, shell execution, or other
/// side effects. In `Ask` mode, these are blocked.
///
/// The list is intentionally conservative — if a tool isn't in this list,
/// it's allowed in Ask mode. Better to allow a harmless read tool than to
/// block a legitimate one.
const WRITE_CLASS_TOOLS: &[&str] = &[
    "write_file",
    "patch",
    "apply_patch",
    "edit_file",
    "terminal",
    "terminal_run",
    "delete_file",
    "move_file",
    "copy_file",
    "create_directory",
    "mkdir",
    "rm",
    "git_commit",
    "git_push",
    "npm_install",
    "pip_install",
    "cargo_run",
    "code_execution",
    "execute_code",
];

/// Check whether a tool name is a write-class tool.
///
/// Write-class tools mutate the filesystem, execute shell commands, or
/// have other persistent side effects. In `Ask` mode, these are blocked.
pub fn is_write_tool(name: &str) -> bool {
    let lower = name.to_lowercase();
    WRITE_CLASS_TOOLS.iter().any(|&w| w == lower)
}

// ---------------------------------------------------------------------------
// Orchestration stage (P3-1)
// ---------------------------------------------------------------------------

/// Coarse-grained orchestration stage, mapped from the fine-grained
/// `LoopPhase` (9 phases). Used for observability and UI progress display.
///
/// The four stages correspond to the perception-reasoning-action-observation
/// cycle:
///
/// 1. **Perception** — setup, context gathering, prefetch
/// 2. **Reasoning** — LLM call and post-response processing
/// 3. **Action** — tool validation and execution
/// 4. **Observation** — post-tool iteration, hooks, finalization
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationStage {
    Perception,
    Reasoning,
    Action,
    Observation,
}

impl OrchestrationStage {
    /// Short label for tracing/UI.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Perception => "perception",
            Self::Reasoning => "reasoning",
            Self::Action => "action",
            Self::Observation => "observation",
        }
    }
}

impl std::fmt::Display for OrchestrationStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Mode gate
// ---------------------------------------------------------------------------

/// Result of a mode gate check on a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModeGateDecision {
    /// The tool call is allowed to proceed.
    Allow,
    /// The tool call is blocked (Ask mode + write tool).
    Block {
        reason: String,
    },
}

/// Task-level mode gate. Enforces Craft/Plan/Ask semantics.
///
/// Created per-run (or per-task) and consulted by the agent loop at
/// two points:
/// 1. Before sending tool schemas to the LLM — `filter_tool_schemas()`
///    removes write tools in Ask mode.
/// 2. Before executing each tool call — `check_tool_call()` blocks
///    any write tool that slipped through (defense in depth).
///
/// For Plan mode, the gate tracks whether the plan has been confirmed.
/// `should_pause_for_confirmation()` returns true until
/// `mark_plan_confirmed()` is called.
#[derive(Debug, Clone)]
pub struct ModeGate {
    /// The working mode for this task.
    mode: TaskMode,
    /// For Plan mode: whether the user has confirmed the plan.
    /// Once confirmed, the gate becomes permissive (like Craft).
    plan_confirmed: bool,
    /// Cached set of write-class tool names for fast lookup.
    write_tools: HashSet<String>,
    /// L1 Domain Mode override: write-class tools that are safe to
    /// allow in Ask mode for the current domain.
    ///
    /// Example: Code+Ask allows `write_file` and `patch` because
    /// the user might want to see code and make small edits without
    /// switching to Craft mode. Research+Ask has no overrides.
    ///
    /// Source: `DomainPreset::safe_write_override`.
    safe_write_override: HashSet<String>,
}

impl ModeGate {
    /// Create a new mode gate for the given task mode.
    pub fn new(mode: TaskMode) -> Self {
        let write_tools = WRITE_CLASS_TOOLS
            .iter()
            .map(|s| s.to_string())
            .collect();
        Self {
            mode,
            plan_confirmed: false,
            write_tools,
            safe_write_override: HashSet::new(),
        }
    }

    /// Create a Craft mode gate (no restrictions).
    pub fn craft() -> Self {
        Self::new(TaskMode::Craft)
    }

    /// Set the safe-write override list (from `DomainPreset`).
    ///
    /// Tools in this list are allowed even in Ask mode.
    /// Typically set from `DomainPreset::safe_write_override`.
    pub fn with_safe_write_override(mut self, tools: &[&str]) -> Self {
        self.safe_write_override = tools.iter().map(|s| s.to_lowercase()).collect();
        self
    }

    /// Set the safe-write override from owned strings.
    pub fn set_safe_write_override(&mut self, tools: Vec<String>) {
        self.safe_write_override = tools.into_iter().map(|s| s.to_lowercase()).collect();
    }

    /// Returns the current working mode.
    pub fn mode(&self) -> TaskMode {
        self.mode
    }

    /// Returns true if the gate is in Plan mode and the plan has not
    /// yet been confirmed. The agent loop should pause and wait for
    /// user confirmation before executing tools.
    pub fn should_pause_for_confirmation(&self) -> bool {
        self.mode == TaskMode::Plan && !self.plan_confirmed
    }

    /// Mark the plan as confirmed. After this call, Plan mode behaves
    /// like Craft mode (no pausing).
    pub fn mark_plan_confirmed(&mut self) {
        self.plan_confirmed = true;
        tracing::info!("Plan mode: user confirmed the plan, proceeding to execution");
    }

    /// Filter tool schemas based on the mode. In Ask mode, write-class
    /// tools are removed from the list sent to the LLM — unless they
    /// appear in the `safe_write_override` list (L1 Domain Mode).
    pub fn filter_tool_schemas(&self, schemas: &[ToolSchema]) -> Vec<ToolSchema> {
        if self.mode != TaskMode::Ask {
            return schemas.to_vec();
        }
        schemas
            .iter()
            .filter(|s| {
                let lower = s.name.to_lowercase();
                // Block write tools unless they're in the override list
                !self.write_tools.contains(&lower) || self.safe_write_override.contains(&lower)
            })
            .cloned()
            .collect()
    }

    /// Check whether a specific tool call is allowed under the current
    /// mode. This is a defense-in-depth check — even if a write tool
    /// somehow appears in the LLM's response (e.g. via a plugin that
    /// registered it after schema filtering), this method will block it.
    ///
    /// L1 Domain Mode override: tools in `safe_write_override` are
    /// allowed even in Ask mode (e.g. `write_file` in Code+Ask).
    pub fn check_tool_call(&self, call: &ToolCall) -> ModeGateDecision {
        if self.mode == TaskMode::Ask
            && self.is_write_tool_name(&call.function.name)
            && !self.is_safe_override(&call.function.name)
        {
            return ModeGateDecision::Block {
                reason: format!(
                    "Ask mode blocks write-class tool '{}'. \
                     The task is in read-only advisory mode and cannot \
                     execute tools with side effects.",
                    call.function.name
                ),
            };
        }
        ModeGateDecision::Allow
    }

    /// Check a batch of tool calls. Returns (allowed, blocked).
    /// Blocked calls are returned with their denial reasons.
    pub fn check_tool_calls(&self, calls: &[ToolCall]) -> (Vec<ToolCall>, Vec<(ToolCall, String)>) {
        let mut allowed = Vec::new();
        let mut blocked = Vec::new();
        for call in calls {
            match self.check_tool_call(call) {
                ModeGateDecision::Allow => allowed.push(call.clone()),
                ModeGateDecision::Block { reason } => {
                    tracing::warn!(
                        "Mode gate blocked tool '{}': {}",
                        call.function.name,
                        reason
                    );
                    blocked.push((call.clone(), reason));
                }
            }
        }
        (allowed, blocked)
    }

    /// Internal: check if a tool name is write-class (case-insensitive).
    fn is_write_tool_name(&self, name: &str) -> bool {
        self.write_tools.contains(&name.to_lowercase())
    }

    /// Internal: check if a tool name is in the safe-write override list.
    fn is_safe_override(&self, name: &str) -> bool {
        self.safe_write_override.contains(&name.to_lowercase())
    }
}

impl Default for ModeGate {
    fn default() -> Self {
        Self::craft()
    }
}


// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_write_tool ──────────────────────────────────────

    #[test]
    fn test_is_write_tool_write_file() {
        assert!(is_write_tool("write_file"));
    }

    #[test]
    fn test_is_write_tool_terminal() {
        assert!(is_write_tool("terminal"));
    }

    #[test]
    fn test_is_write_tool_patch() {
        assert!(is_write_tool("patch"));
        assert!(is_write_tool("apply_patch"));
    }

    #[test]
    fn test_is_write_tool_case_insensitive() {
        assert!(is_write_tool("WRITE_FILE"));
        assert!(is_write_tool("Terminal"));
    }

    #[test]
    fn test_is_write_tool_read_tools_not_write() {
        assert!(!is_write_tool("read_file"));
        assert!(!is_write_tool("read_pdf"));
        assert!(!is_write_tool("list_files"));
        assert!(!is_write_tool("search_files"));
        assert!(!is_write_tool("memory"));
        assert!(!is_write_tool("session_search"));
    }

    // ── OrchestrationStage ─────────────────────────────────

    #[test]
    fn test_orchestration_stage_as_str() {
        assert_eq!(OrchestrationStage::Perception.as_str(), "perception");
        assert_eq!(OrchestrationStage::Reasoning.as_str(), "reasoning");
        assert_eq!(OrchestrationStage::Action.as_str(), "action");
        assert_eq!(OrchestrationStage::Observation.as_str(), "observation");
    }

    // ── ModeGate: Craft ────────────────────────────────────

    #[test]
    fn test_craft_mode_allows_all_tools() {
        let gate = ModeGate::craft();
        assert!(!gate.should_pause_for_confirmation());

        let call = ToolCall {
            id: "1".into(),
            function: hermix_core::FunctionCall {
                name: "write_file".into(),
                arguments: "{}".into(),
            },
        };
        assert_eq!(gate.check_tool_call(&call), ModeGateDecision::Allow);
    }

    #[test]
    fn test_craft_mode_no_schema_filtering() {
        let gate = ModeGate::craft();
        let schemas = vec![
            ToolSchema::new("read_file", "Read", JsonSchema::new("object")),
            ToolSchema::new("write_file", "Write", JsonSchema::new("object")),
            ToolSchema::new("terminal", "Shell", JsonSchema::new("object")),
        ];
        let filtered = gate.filter_tool_schemas(&schemas);
        assert_eq!(filtered.len(), 3);
    }

    // ── ModeGate: Ask ──────────────────────────────────────

    #[test]
    fn test_ask_mode_blocks_write_tools_in_schemas() {
        let gate = ModeGate::new(TaskMode::Ask);
        let schemas = vec![
            ToolSchema::new("read_file", "Read", JsonSchema::new("object")),
            ToolSchema::new("write_file", "Write", JsonSchema::new("object")),
            ToolSchema::new("terminal", "Shell", JsonSchema::new("object")),
            ToolSchema::new("list_files", "List", JsonSchema::new("object")),
        ];
        let filtered = gate.filter_tool_schemas(&schemas);
        assert_eq!(filtered.len(), 2);
        assert!(filtered.iter().all(|s| !is_write_tool(&s.name)));
    }

    #[test]
    fn test_ask_mode_blocks_write_tool_call() {
        let gate = ModeGate::new(TaskMode::Ask);
        let call = ToolCall {
            id: "1".into(),
            function: hermix_core::FunctionCall {
                name: "write_file".into(),
                arguments: "{}".into(),
            },
        };
        match gate.check_tool_call(&call) {
            ModeGateDecision::Block { reason } => {
                assert!(reason.contains("Ask mode"));
                assert!(reason.contains("write_file"));
            }
            ModeGateDecision::Allow => panic!("write_file should be blocked in Ask mode"),
        }
    }

    #[test]
    fn test_ask_mode_allows_read_tool_call() {
        let gate = ModeGate::new(TaskMode::Ask);
        let call = ToolCall {
            id: "1".into(),
            function: hermix_core::FunctionCall {
                name: "read_file".into(),
                arguments: "{}".into(),
            },
        };
        assert_eq!(gate.check_tool_call(&call), ModeGateDecision::Allow);
    }

    #[test]
    fn test_ask_mode_check_batch() {
        let gate = ModeGate::new(TaskMode::Ask);
        let calls = vec![
            ToolCall {
                id: "1".into(),
                function: hermix_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: "{}".into(),
                },
            },
            ToolCall {
                id: "2".into(),
                function: hermix_core::FunctionCall {
                    name: "write_file".into(),
                    arguments: "{}".into(),
                },
            },
            ToolCall {
                id: "3".into(),
                function: hermix_core::FunctionCall {
                    name: "terminal".into(),
                    arguments: "{}".into(),
                },
            },
        ];
        let (allowed, blocked) = gate.check_tool_calls(&calls);
        assert_eq!(allowed.len(), 1);
        assert_eq!(blocked.len(), 2);
    }

    // ── ModeGate: Plan ─────────────────────────────────────

    #[test]
    fn test_plan_mode_pauses_until_confirmed() {
        let mut gate = ModeGate::new(TaskMode::Plan);
        assert!(gate.should_pause_for_confirmation());

        gate.mark_plan_confirmed();
        assert!(!gate.should_pause_for_confirmation());
    }

    #[test]
    fn test_plan_mode_allows_tools_after_confirmation() {
        let mut gate = ModeGate::new(TaskMode::Plan);
        gate.mark_plan_confirmed();

        let call = ToolCall {
            id: "1".into(),
            function: hermix_core::FunctionCall {
                name: "write_file".into(),
                arguments: "{}".into(),
            },
        };
        assert_eq!(gate.check_tool_call(&call), ModeGateDecision::Allow);
    }

    #[test]
    fn test_plan_mode_does_not_filter_schemas() {
        let gate = ModeGate::new(TaskMode::Plan);
        let schemas = vec![
            ToolSchema::new("read_file", "Read", JsonSchema::new("object")),
            ToolSchema::new("write_file", "Write", JsonSchema::new("object")),
        ];
        let filtered = gate.filter_tool_schemas(&schemas);
        assert_eq!(filtered.len(), 2);
    }

    // ── ModeGate: safe_write_override (L1 Domain Mode) ───────────────

    #[test]
    fn test_ask_mode_with_safe_write_override_allows_write_file() {
        let gate = ModeGate::new(TaskMode::Ask)
            .with_safe_write_override(&["write_file", "patch"]);
        let call = ToolCall {
            id: "1".into(),
            function: hermix_core::FunctionCall {
                name: "write_file".into(),
                arguments: "{}".into(),
            },
        };
        assert_eq!(gate.check_tool_call(&call), ModeGateDecision::Allow);
    }

    #[test]
    fn test_ask_mode_with_safe_write_override_still_blocks_terminal() {
        let gate = ModeGate::new(TaskMode::Ask)
            .with_safe_write_override(&["write_file", "patch"]);
        let call = ToolCall {
            id: "1".into(),
            function: hermix_core::FunctionCall {
                name: "terminal".into(),
                arguments: "{}".into(),
            },
        };
        // terminal is NOT in the override list, should be blocked
        assert!(matches!(gate.check_tool_call(&call), ModeGateDecision::Block { .. }));
    }

    #[test]
    fn test_ask_mode_filter_schemas_with_override() {
        let gate = ModeGate::new(TaskMode::Ask)
            .with_safe_write_override(&["write_file"]);
        let schemas = vec![
            ToolSchema::new("read_file", "Read", JsonSchema::new("object")),
            ToolSchema::new("write_file", "Write", JsonSchema::new("object")),
            ToolSchema::new("terminal", "Shell", JsonSchema::new("object")),
        ];
        let filtered = gate.filter_tool_schemas(&schemas);
        // read_file (not write) + write_file (override) = 2
        // terminal (write, no override) = blocked
        assert_eq!(filtered.len(), 2);
        assert!(filtered.iter().any(|s| s.name == "read_file"));
        assert!(filtered.iter().any(|s| s.name == "write_file"));
        assert!(!filtered.iter().any(|s| s.name == "terminal"));
    }

    #[test]
    fn test_ask_mode_no_override_blocks_all_write_tools() {
        let gate = ModeGate::new(TaskMode::Ask);
        let schemas = vec![
            ToolSchema::new("read_file", "Read", JsonSchema::new("object")),
            ToolSchema::new("write_file", "Write", JsonSchema::new("object")),
        ];
        let filtered = gate.filter_tool_schemas(&schemas);
        assert_eq!(filtered.len(), 1);
        assert!(filtered.iter().all(|s| s.name == "read_file"));
    }

    #[test]
    fn test_set_safe_write_override_mutates() {
        let mut gate = ModeGate::new(TaskMode::Ask);
        // Before: write_file is blocked
        let call = ToolCall {
            id: "1".into(),
            function: hermix_core::FunctionCall {
                name: "write_file".into(),
                arguments: "{}".into(),
            },
        };
        assert!(matches!(gate.check_tool_call(&call), ModeGateDecision::Block { .. }));
        // After set: write_file is allowed
        gate.set_safe_write_override(vec!["write_file".into()]);
        assert_eq!(gate.check_tool_call(&call), ModeGateDecision::Allow);
    }

// ── as_orchestration_stage ─────────────────────────────────────

#[test]
fn test_phase_to_stage_mapping() {
    use crate::agent_loop::LoopPhase;
    assert_eq!(
        LoopPhase::Init.as_orchestration_stage(),
        OrchestrationStage::Perception
    );
    assert_eq!(
        LoopPhase::PreTurnSetup.as_orchestration_stage(),
        OrchestrationStage::Perception
    );
    assert_eq!(
        LoopPhase::Prefetch.as_orchestration_stage(),
        OrchestrationStage::Perception
    );
    assert_eq!(
        LoopPhase::LlmCall.as_orchestration_stage(),
        OrchestrationStage::Reasoning
    );
    assert_eq!(
        LoopPhase::PostLlmCall.as_orchestration_stage(),
        OrchestrationStage::Reasoning
    );
    assert_eq!(
        LoopPhase::ToolValidation.as_orchestration_stage(),
        OrchestrationStage::Action
    );
    assert_eq!(
        LoopPhase::ToolExecution.as_orchestration_stage(),
        OrchestrationStage::Action
    );
    assert_eq!(
        LoopPhase::PostToolExec.as_orchestration_stage(),
        OrchestrationStage::Observation
    );
    assert_eq!(
        LoopPhase::Finalize.as_orchestration_stage(),
        OrchestrationStage::Observation
    );
}
}
