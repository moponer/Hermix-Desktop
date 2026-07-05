//! Agent configuration types.
//!
//! Extracted from `agent_loop.rs` on 2026-06-30 (REFACTOR_PLAN §4 / P2.2.4).
//! The config struct has grown to ~270 fields across 470 lines, so the agent
//! loop file no longer carries the definition. The runtime semantics stay
//! identical — only the module location changes.
//!
//! ## Backwards-compat re-exports
//! - `hermix_agent::AgentConfig` (top-level): unchanged via `lib.rs`
//! - `hermix_agent::agent_loop::AgentConfig` (path): preserved via
//!   `pub use` in `agent_loop.rs`, so two existing test imports
//!   (`tests/prop_max_turns.rs`, `tests/parity_self_evolution_fixtures.rs`)
//!   continue to compile without modification.

use std::collections::HashMap;

use hermix_core::traits::PermissionLevel;
use hermix_core::{BudgetConfig, BudgetConfigError};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::smart_model_routing::{ApiMode, SmartModelRoutingConfig};

// ---------------------------------------------------------------------------
// RetryConfig
// ---------------------------------------------------------------------------

/// Retry / failover configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    /// Maximum retries before giving up.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// Base delay for exponential backoff (ms).
    #[serde(default = "default_base_delay_ms")]
    pub base_delay_ms: u64,
    /// Maximum backoff cap (ms).
    #[serde(default = "default_max_delay_ms")]
    pub max_delay_ms: u64,
    /// Optional fallback model identifier (tried after all retries on the primary model fail).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_model: Option<String>,
}

fn default_max_retries() -> u32 {
    3
}
fn default_base_delay_ms() -> u64 {
    1000
}
fn default_max_delay_ms() -> u64 {
    30_000
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: default_max_retries(),
            base_delay_ms: default_base_delay_ms(),
            max_delay_ms: default_max_delay_ms(),
            fallback_model: None,
        }
    }
}

// ---------------------------------------------------------------------------
// RuntimeProviderConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RuntimeProviderConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Optional external process command for provider runtimes (Python parity metadata).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Optional argv tail for external process runtimes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// OAuth2 token endpoint for refresh flows (provider config centre).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_token_url: Option<String>,
    /// OAuth2 client_id for refresh flows (provider config centre).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_client_id: Option<String>,
}

// ---------------------------------------------------------------------------
// AgentConfig
// ---------------------------------------------------------------------------

/// Configuration for the agent loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Maximum number of LLM → tool → LLM iterations.
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,

    /// When `true`, the agent loop injects the autonomous-execution guidance
    /// into the system prompt and re-drives the LLM whenever it outputs plain
    /// text without tool calls (up to `default_max_autonomous_nudges` times).
    /// This prevents the model from stopping mid-task to ask the user for
    /// information it could obtain via tools.
    #[serde(default = "default_autonomous_mode")]
    pub autonomous_mode: bool,

    /// Budget settings for truncating tool output.
    #[serde(default)]
    pub budget: BudgetConfig,

    /// Model identifier (e.g. "gpt-4o", "claude-3-5-sonnet").
    #[serde(default = "default_model")]
    pub model: String,

    /// API mode — selects the request format for the LLM provider.
    #[serde(default)]
    pub api_mode: ApiMode,

    /// Retry / failover configuration.
    #[serde(default)]
    pub retry: RetryConfig,

    /// Optional system prompt prepended to every conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,

    /// Optional personality overlay appended to the system prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality: Option<String>,

    /// Extra JSON body fields forwarded to the provider on every request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_body: Option<Value>,

    /// Whether to use streaming mode by default.
    #[serde(default)]
    pub stream: bool,

    /// Suppress user-facing lifecycle/status output for this agent session.
    /// Intended for child/delegated/background agents to match Python quiet_mode semantics.
    #[serde(default)]
    pub quiet_mode: bool,

    /// Temperature for LLM sampling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,

    /// Maximum tokens for LLM completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    /// Maximum number of concurrent delegate_task tool calls.
    #[serde(default = "default_max_concurrent_delegates")]
    pub max_concurrent_delegates: u32,

    /// Flush memories every N turns.
    #[serde(default = "default_memory_flush_interval")]
    pub memory_flush_interval: u32,

    /// Session identifier — used for memory and persistence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    /// HERMIX_HOME path — used by memory plugins for config resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hermix_home: Option<String>,

    /// Skip memory integration (read prefetch + write hooks).
    /// The write-path methods iterate the memory router and fan out to each
    /// provider's on_* hooks. Set `true` to short-circuit both
    /// the read path and the write fan-out.
    #[serde(default)]
    pub skip_memory: bool,

    /// M2: hard char cap on the `## Memory` block injected into the
    /// system prompt per turn. Hits below `memory_min_salience` are
    /// dropped first; the remaining list is appended top-down until
    /// the budget is exhausted. Default `4000` chars ≈ 1.5-2K tokens
    /// for mixed CJK/ASCII content — was 12000 (~6K tokens), which
    /// dominated context windows without improving recall quality.
    /// Set to `0` to disable the cap entirely (legacy behaviour).
    #[serde(default = "default_memory_char_budget")]
    pub memory_char_budget: usize,

    /// M2: noise floor for memory recall. Hits with `salience < this`
    /// are dropped before the char budget is applied. Default `0.30`
    /// filters out low-signal fragments (e.g. session boilerplate,
    /// generated text), letting the budget spend tokens on facts the
    /// user actually cares about. Set to `0.0` to disable.
    #[serde(default = "default_memory_min_salience")]
    pub memory_min_salience: f32,

    /// M2: cap on subsequent-turn recall size (after the first full
    /// recall has seeded the context). Default `5` — keeps the
    /// `## Memory` block small for ongoing turns since the LLM
    /// already has prior memory in the active context window.
    #[serde(default = "default_memory_recall_limit_subsequent")]
    pub memory_recall_limit_subsequent: usize,

    /// M2: cap on first-turn recall size (before any context seeding).
    /// Default `50` — wide net to surface everything relevant at
    /// session start.
    #[serde(default = "default_memory_recall_limit_first")]
    pub memory_recall_limit_first: usize,

    /// Optional cheap-vs-strong per-turn routing.
    #[serde(default)]
    pub smart_model_routing: SmartModelRoutingConfig,

    /// Provider hint (e.g. "openai", "anthropic", "openrouter").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,

    /// Custom LLM provider configs cloned from `GatewayConfig.llm_providers`.
    /// Forwarded to `ContextManager::for_model_with_providers` so per-model
    /// `context_window` / `model_contexts` overrides win over the static
    /// catalog when the agent loop is driving a `custom-{id}:{model}` model.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub llm_providers: HashMap<String, hermix_config::LlmProviderConfig>,

    /// Optional platform hint key (e.g. "cli", "telegram", "discord").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,

    /// Optional platform hint override text. When set, replaces the
    /// built-in platform hint for the active platform. Supports Hermes
    /// Agent-style `append` / `replace` semantics: if the value starts
    /// with `+`, it is appended to the built-in hint; otherwise it
    /// replaces it entirely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_hint_override: Option<String>,

    /// Include session_id in system prompt timestamp block.
    #[serde(default)]
    pub pass_session_id: bool,

    /// Runtime provider credentials/endpoints keyed by provider name.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub runtime_providers: HashMap<String, RuntimeProviderConfig>,

    /// Ephemeral system prompt appended at API-call time only.
    /// This is intentionally not persisted in context history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ephemeral_system_prompt: Option<String>,

    /// Per-turn workspace root (canonical absolute path). Tool dispatchers
    /// (terminal, file) constrain all relative path resolution against this
    /// root. `None` disables the workspace boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,

    /// Per-turn permission policy. `Default` triggers a `RequestApproval`
    /// stream event before risk-bearing tool calls; `Full` skips the gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<PermissionLevel>,

    /// Per-turn ephemeral skill set. Names of skills to surface in the
    /// system prompt on top of any server-side enabled skills.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ephemeral_skills: Vec<String>,

    /// Session-level hard spend limit in USD. When reached, the loop trips
    /// the cost gate and returns early with a summary system message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_usd: Option<f64>,

    /// Ratio (0.0-1.0) at which to proactively degrade to a cheaper model.
    #[serde(default = "default_cost_guard_degrade_at_ratio")]
    pub cost_guard_degrade_at_ratio: f64,

    /// Optional explicit cheaper model to use after crossing the degrade ratio.
    /// If unset, falls back to `retry.fallback_model` then a built-in cheap default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_guard_degrade_model: Option<String>,

    /// Optional per-million-token prompt price used when provider does not
    /// return `usage.estimated_cost`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cost_per_million_usd: Option<f64>,

    /// Optional per-million-token completion price used when provider does not
    /// return `usage.estimated_cost`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_cost_per_million_usd: Option<f64>,

    /// Auto-checkpoint interval in turns. `0` disables automatic checkpoints.
    #[serde(default = "default_checkpoint_interval_turns")]
    pub checkpoint_interval_turns: u32,

    /// If a single turn generates at least this many tool errors, rollback
    /// to the latest checkpoint and continue. `0` disables rollback.
    #[serde(default = "default_rollback_on_tool_error_threshold")]
    pub rollback_on_tool_error_threshold: u32,

    /// External process / ACP command (Python primary `command` in `resolve_turn_route` signature).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acp_command: Option<String>,

    /// External process / ACP argv tail (Python primary `args`).
    #[serde(default)]
    pub acp_args: Vec<String>,

    /// User-turn cadence for memory background review ticks (`0` disables interval).
    #[serde(default = "default_memory_nudge_interval")]
    pub memory_nudge_interval: u32,

    /// Tool-loop iterations without `skill_manage` before skill background review (`0` disables).
    #[serde(default = "default_skill_creation_nudge_interval")]
    pub skill_creation_nudge_interval: u32,

    /// Run Python-style background memory/skill review after a session (extra LLM calls).
    /// Python has no master off-switch; matches default-on (`cli-config.yaml` can override via `agent`).
    #[serde(default = "default_background_review_enabled")]
    pub background_review_enabled: bool,

    /// Emit background review metrics snapshots (`debug` tracing only).
    /// Child sessions disable this for stricter quiet-mode parity.
    #[serde(default = "default_background_review_metrics_enabled")]
    pub background_review_metrics_enabled: bool,

    /// Enable the auto-skill learning loop. When `true`
    /// (default), tasks that complete ≥ `skill_creation_nudge_interval`
    /// iters prompt the LLM to extract a reusable skill via
    /// `skill_manage(action='auto_extract_from_conversation')`. Set
    /// to `false` in `~/.hermix/config.yaml` to opt out without
    /// having to also disable `background_review_enabled`.
    ///
    /// Off by default in child sessions (`spawn_background_review`
    /// forces it false there) so sub-agent chatter doesn't pollute
    /// the user's primary skill library.
    #[serde(default = "default_auto_skill_extraction")]
    pub auto_skill_extraction: bool,

    /// Exact system prompt from SQLite when continuing a session (stable Anthropic prefix cache).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stored_system_prompt: Option<String>,

    /// Progress ratio (0–1) at which Python emits a *caution* budget nudge (`_budget_caution_threshold`).
    #[serde(default = "default_budget_caution_threshold")]
    pub budget_caution_threshold: f64,

    /// Progress ratio (0–1) at which Python emits an urgent budget warning (`_budget_warning_threshold`).
    #[serde(default = "default_budget_warning_threshold")]
    pub budget_warning_threshold: f64,

    /// When false, skip `_get_budget_warning`-style injection entirely.
    #[serde(default = "default_budget_pressure_enabled")]
    pub budget_pressure_enabled: bool,

    /// Retries when the model returns empty assistant text with no tools (Python `_empty_content_retries`, max 3).
    #[serde(default = "default_empty_content_max_retries")]
    pub empty_content_max_retries: u32,

    /// Thinking-only prefill rounds (Python `_thinking_prefill_retries`, max 2).
    #[serde(default = "default_thinking_prefill_max_retries")]
    pub thinking_prefill_max_retries: u32,

    /// Run one compression pass before the first LLM call if context is already over threshold.
    #[serde(default = "default_preflight_context_compress")]
    pub preflight_context_compress: bool,

    /// Optional replacement for the **persisted** last user message (API-facing user text may differ).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persist_user_message: Option<String>,

    /// Max retries when the model emits unknown tool names.
    #[serde(default = "default_invalid_tool_call_max_retries")]
    pub invalid_tool_call_max_retries: u32,

    /// Max retries when tool call arguments are invalid JSON.
    #[serde(default = "default_invalid_tool_json_max_retries")]
    pub invalid_tool_json_max_retries: u32,

    /// Max retries when streaming assembles truncated tool arguments (`finish_reason=length` parity).
    #[serde(default = "default_truncated_tool_call_max_retries")]
    pub truncated_tool_call_max_retries: u32,

    /// Seconds of silence before the **first** (and only, until reset) "still waiting" activity
    /// notice while collecting a stream. Resets on every chunk. `0` disables all stream activity
    /// notices (including `stream_activity_stall_secs`).
    #[serde(default = "default_stream_activity_interval_secs")]
    pub stream_activity_interval_secs: u64,

    /// After the first activity notice, if still no chunk for this many seconds, emit a second
    /// `"activity"` notice (likely stuck). `0` disables the second notice. Ignored when
    /// `stream_activity_interval_secs` is `0`.
    #[serde(default = "default_stream_activity_stall_secs")]
    pub stream_activity_stall_secs: u64,

    /// Max tools to surface to the LLM after tool-routing (Phase 7 H1).
    /// `0` or unset means "no routing, send full registry" (legacy default).
    #[serde(default = "default_max_routed_tools")]
    pub max_routed_tools: usize,

    /// Active toolset names (used by ToolsetRouter for boosting).
    /// Empty = no toolset-based filter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_toolsets: Vec<String>,
}

// ---------------------------------------------------------------------------
// Default value functions
// ---------------------------------------------------------------------------

/// N-07 (startup guard): validate the assembled `AgentConfig` before the
/// loop starts running. Returns the first `BudgetConfig` error so the
/// caller can convert it into a config-load error. Currently delegates to
/// `BudgetConfig::validate()` — additional checks (e.g. `max_cost_usd`
/// positivity) can be appended here as the config schema grows.
pub fn validate(config: &AgentConfig) -> Result<(), BudgetConfigError> {
    config.budget.validate()?;
    if let Some(cap) = config.max_cost_usd {
        if cap.is_nan() || cap < 0.0 {
            tracing::warn!(
                target: "hermix.agent",
                max_cost_usd = cap,
                "AgentConfig::max_cost_usd is NaN or negative; cost guard will misbehave"
            );
        }
    }
    Ok(())
}

fn default_max_turns() -> u32 {
    30
}

fn default_autonomous_mode() -> bool {
    true
}

fn default_max_routed_tools() -> usize {
    50
}

fn default_model() -> String {
    "gpt-4o".to_string()
}

fn default_max_concurrent_delegates() -> u32 {
    1
}

fn default_memory_flush_interval() -> u32 {
    5
}

fn default_cost_guard_degrade_at_ratio() -> f64 {
    0.8
}

fn default_checkpoint_interval_turns() -> u32 {
    3
}

fn default_rollback_on_tool_error_threshold() -> u32 {
    3
}

fn default_memory_nudge_interval() -> u32 {
    10
}

/// M2: hard char cap on `## Memory` block per turn. Was 12000 (~6K tokens)
/// which dominated context windows. 4000 chars ≈ 1.5-2K tokens is enough
/// for the 5-8 highest-salience hits the LLM actually uses.
fn default_memory_char_budget() -> usize {
    4000
}

/// M2: noise floor — drops hits with salience below this before budgeting.
/// 0.30 keeps only meaningful facts; legacy behaviour was to inject everything.
fn default_memory_min_salience() -> f32 {
    0.30
}

/// M2: cap on subsequent-turn recall. Down from 10 — the LLM already has
/// the prior turn's memory in active context.
fn default_memory_recall_limit_subsequent() -> usize {
    5
}

/// M2: first-turn recall size. Stays at 50 — wide net to surface everything.
fn default_memory_recall_limit_first() -> usize {
    50
}

fn default_skill_creation_nudge_interval() -> u32 {
    10
}

fn default_background_review_enabled() -> bool {
    true
}

fn default_background_review_metrics_enabled() -> bool {
    true
}

fn default_auto_skill_extraction() -> bool {
    true
}

fn default_budget_caution_threshold() -> f64 {
    0.7
}

fn default_budget_warning_threshold() -> f64 {
    0.9
}

fn default_budget_pressure_enabled() -> bool {
    true
}

fn default_empty_content_max_retries() -> u32 {
    3
}

fn default_thinking_prefill_max_retries() -> u32 {
    2
}

fn default_preflight_context_compress() -> bool {
    true
}

fn default_invalid_tool_call_max_retries() -> u32 {
    3
}

fn default_invalid_tool_json_max_retries() -> u32 {
    3
}

fn default_truncated_tool_call_max_retries() -> u32 {
    3
}

fn default_stream_activity_interval_secs() -> u64 {
    12
}

fn default_stream_activity_stall_secs() -> u64 {
    90
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: default_max_turns(),
            budget: BudgetConfig::default(),
            model: default_model(),
            api_mode: ApiMode::default(),
            retry: RetryConfig::default(),
            system_prompt: None,
            personality: None,
            extra_body: None,
            stream: false,
            quiet_mode: false,
            temperature: None,
            max_tokens: None,
            max_concurrent_delegates: default_max_concurrent_delegates(),
            memory_flush_interval: default_memory_flush_interval(),
            session_id: None,
            hermix_home: None,
            skip_memory: false,
            memory_char_budget: default_memory_char_budget(),
            memory_min_salience: default_memory_min_salience(),
            memory_recall_limit_subsequent: default_memory_recall_limit_subsequent(),
            memory_recall_limit_first: default_memory_recall_limit_first(),
            smart_model_routing: SmartModelRoutingConfig::default(),
            provider: None,
            platform: None,
            platform_hint_override: None,
            pass_session_id: false,
            runtime_providers: HashMap::new(),
            ephemeral_system_prompt: None,
            max_cost_usd: None,
            cost_guard_degrade_at_ratio: default_cost_guard_degrade_at_ratio(),
            cost_guard_degrade_model: None,
            prompt_cost_per_million_usd: None,
            completion_cost_per_million_usd: None,
            checkpoint_interval_turns: default_checkpoint_interval_turns(),
            rollback_on_tool_error_threshold: default_rollback_on_tool_error_threshold(),
            acp_command: None,
            acp_args: Vec::new(),
            memory_nudge_interval: default_memory_nudge_interval(),
            skill_creation_nudge_interval: default_skill_creation_nudge_interval(),
            background_review_enabled: default_background_review_enabled(),
            background_review_metrics_enabled: default_background_review_metrics_enabled(),
            auto_skill_extraction: default_auto_skill_extraction(),
            stored_system_prompt: None,
            budget_caution_threshold: default_budget_caution_threshold(),
            budget_warning_threshold: default_budget_warning_threshold(),
            budget_pressure_enabled: default_budget_pressure_enabled(),
            empty_content_max_retries: default_empty_content_max_retries(),
            thinking_prefill_max_retries: default_thinking_prefill_max_retries(),
            preflight_context_compress: default_preflight_context_compress(),
            persist_user_message: None,
            invalid_tool_call_max_retries: default_invalid_tool_call_max_retries(),
            invalid_tool_json_max_retries: default_invalid_tool_json_max_retries(),
            truncated_tool_call_max_retries: default_truncated_tool_call_max_retries(),
            stream_activity_interval_secs: default_stream_activity_interval_secs(),
            stream_activity_stall_secs: default_stream_activity_stall_secs(),
            workspace: None,
            permission: None,
            llm_providers: HashMap::new(),
            ephemeral_skills: Vec::new(),
            max_routed_tools: default_max_routed_tools(),
            active_toolsets: Vec::new(),
            autonomous_mode: default_autonomous_mode(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// M2 (memory injection budget): the four new AgentConfig fields
    /// must default to conservative values so production sessions
    /// don't accidentally re-introduce the 6K-token memory block.
    #[test]
    fn m2_memory_defaults_are_conservative() {
        let c = AgentConfig::default();
        assert_eq!(
            c.memory_char_budget, 4000,
            "char budget must default to 4000 (was hardcoded 12000)"
        );
        assert!(
            (c.memory_min_salience - 0.30).abs() < 1e-5,
            "salience floor must default to 0.30, got {}",
            c.memory_min_salience
        );
        assert_eq!(
            c.memory_recall_limit_subsequent, 5,
            "subsequent recall must cap at 5 (was 10)"
        );
        assert_eq!(
            c.memory_recall_limit_first, 50,
            "first-turn recall must stay at 50 for cold-start coverage"
        );
    }

    /// M2: the new fields must round-trip through serde without
    /// losing precision or being silently dropped.
    #[test]
    fn m2_memory_fields_serde_roundtrip() {
        let mut c = AgentConfig::default();
        c.memory_char_budget = 8000;
        c.memory_min_salience = 0.45;
        c.memory_recall_limit_first = 80;
        c.memory_recall_limit_subsequent = 12;

        let json = serde_json::to_string(&c).expect("serialize");
        let back: AgentConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.memory_char_budget, 8000);
        assert!((back.memory_min_salience - 0.45).abs() < 1e-5);
        assert_eq!(back.memory_recall_limit_first, 80);
        assert_eq!(back.memory_recall_limit_subsequent, 12);
    }

    /// M2: 0 means "disable the cap" — used by tests and by power
    /// users who want the legacy behaviour. Must round-trip as 0.
    #[test]
    fn m2_zero_budget_roundtrips_as_zero() {
        let mut c = AgentConfig::default();
        c.memory_char_budget = 0;
        c.memory_min_salience = 0.0;
        let json = serde_json::to_string(&c).unwrap();
        let back: AgentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.memory_char_budget, 0);
        assert_eq!(back.memory_min_salience, 0.0);
    }

    /// M2: backwards-compat — existing configs without the new fields
    /// (or with them missing) must deserialise with the new defaults.
    #[test]
    fn m2_old_config_without_new_fields_uses_defaults() {
        // Strip the new fields from a serialised config.
        let c = AgentConfig::default();
        let mut v: serde_json::Value = serde_json::to_value(&c).unwrap();
        v.as_object_mut().unwrap().remove("memory_char_budget");
        v.as_object_mut().unwrap().remove("memory_min_salience");
        v.as_object_mut().unwrap().remove("memory_recall_limit_subsequent");
        v.as_object_mut().unwrap().remove("memory_recall_limit_first");
        let back: AgentConfig = serde_json::from_value(v).unwrap();
        // All four must come back as defaults, not zero/None.
        assert_eq!(back.memory_char_budget, 4000);
        assert!((back.memory_min_salience - 0.30).abs() < 1e-5);
        assert_eq!(back.memory_recall_limit_subsequent, 5);
        assert_eq!(back.memory_recall_limit_first, 50);
    }
}