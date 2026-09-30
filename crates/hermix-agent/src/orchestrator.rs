//! Orchestration stage names — the agent loop's 9 `LoopPhase`s mapped to
//! three outward-facing stages for `TurnReport` / WS events.
//!
//! 2026-09-29 清理：原 `ModeGate` / `ModeGateDecision` / `is_write_tool` /
//! `safe_write_override`（Craft/Plan/Ask 执行门禁）整簇删除 —— 它们的唯一
//! 写入方 `hermix_agent::mode_composition` 从无非测试调用方（整模块死代码
//! 已删）。这里只剩被 `turn_report` 与 `agent_loop::types` 使用的阶段映射。

// ---------------------------------------------------------------------------
// Orchestration stage
// ---------------------------------------------------------------------------

/// Coarse-grained orchestration stage, mapped from the fine-grained
/// `LoopPhase` (9 phases). Used for observability and UI progress display.
/// The four stages correspond to the perception-reasoning-action-observation
/// cycle:
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;


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
