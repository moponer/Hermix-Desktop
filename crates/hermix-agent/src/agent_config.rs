//! Agent configuration types.
//!
//! Extracted from `agent_loop.rs` on (REFACTOR_PLAN §4 / P2.2.4).
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
use std::path::PathBuf;

use hermix_core::traits::PermissionLevel;
use hermix_core::{BudgetConfig, BudgetConfigError};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use hermix_core::PermissionProfile;
use hermix_intelligence::DomainMode;
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
// WriteRetryConfig
// ---------------------------------------------------------------------------

/// Configuration for SQLite write retry with jitter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteRetryConfig {
    /// Maximum number of retries before giving up.
    #[serde(default = "default_write_max_retries")]
    pub max_retries: u32,
    /// Minimum retry delay in milliseconds.
    #[serde(default = "default_write_retry_min_ms")]
    pub min_ms: u64,
    /// Maximum retry delay in milliseconds.
    #[serde(default = "default_write_retry_max_ms")]
    pub max_ms: u64,
}

fn default_write_max_retries() -> u32 { 15 }
fn default_write_retry_min_ms() -> u64 { 20 }
fn default_write_retry_max_ms() -> u64 { 150 }

impl Default for WriteRetryConfig {
    fn default() -> Self {
        Self {
            max_retries: default_write_max_retries(),
            min_ms: default_write_retry_min_ms(),
            max_ms: default_write_retry_max_ms(),
        }
    }
}

// ---------------------------------------------------------------------------
// SubAgentConfig
// ---------------------------------------------------------------------------

/// Configuration for sub-agent delegation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentConfig {
    /// Timeout in seconds for a single sub-agent execution.
    #[serde(default = "default_sub_agent_timeout_secs")]
    pub timeout_secs: u64,
    /// Minimum turns for a sub-agent (floor).
    #[serde(default = "default_sub_agent_min_turns")]
    pub min_turns: u32,
    /// Maximum turns cap for a sub-agent (ceiling).
    #[serde(default = "default_sub_agent_max_turns_cap")]
    pub max_turns_cap: u32,
    /// Default max iterations for delegation.
    #[serde(default = "default_delegation_max_iterations")]
    pub delegation_max_iterations: u32,
}

fn default_sub_agent_timeout_secs() -> u64 {
    // 300s → 1800s：子 agent 的存在意义就是"派出去干长活"。构建、
    // 调研、批量搜索轻松超过 5 分钟；旧默认值使 delegate_task 的
    // 长任务场景形同虚设。orchestrator 自身有协作取消（interrupt），
    // 超时后结果会作为 SubAgentError::Timeout 返回父 loop。
    1800
}
fn default_sub_agent_min_turns() -> u32 { 2 }
fn default_sub_agent_max_turns_cap() -> u32 { 12 }
fn default_delegation_max_iterations() -> u32 { 50 }

impl Default for SubAgentConfig {
    fn default() -> Self {
        Self {
            timeout_secs: default_sub_agent_timeout_secs(),
            min_turns: default_sub_agent_min_turns(),
            max_turns_cap: default_sub_agent_max_turns_cap(),
            delegation_max_iterations: default_delegation_max_iterations(),
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

/// Session-id prefix of persona-entity sessions (阿黎 = `twin:ali`).
/// These are independent individuals, not style overlays on the main
/// assistant: their memory scope starts blank and never falls back to
/// the global `<hermix_home>/memories`. Defined here because the
/// dependency direction is server → agent (the wire constant lives in
/// `hermix-server::mobile`).
pub const TWIN_SESSION_PREFIX: &str = "twin:";

/// Session id → filesystem-safe directory component.
///
/// Session ids are wire values (`twin:ali`, device ids, UUIDs) and may
/// contain `:` — **illegal in Windows path components** (reserved for
/// drive letters). `create_dir_all` on such a path fails with
/// ERROR_INVALID_NAME, and every best-effort caller swallows that error —
/// so a twin session silently lost all memory persistence on Windows.
/// Map anything outside `[A-Za-z0-9._-]` to `_`; ids that were already
/// safe (UUIDs, timestamps) pass through unchanged.
pub(crate) fn fs_safe_session_id(sid: &str) -> String {
    sid.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Configuration for the agent loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Maximum number of LLM → tool → LLM iterations.
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,

    /// Maximum number of tool calls per turn. Prevents infinite tool loops.
    /// 0 means no limit.
    #[serde(default = "default_max_tool_calls_per_turn")]
    pub max_tool_calls_per_turn: u32,

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

    /// Write retry configuration for SQLite contention.
    #[serde(default)]
    pub write_retry: WriteRetryConfig,

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

    /// This agent is a `delegate_task` child, not a top-level session.
    ///
    /// The zero-context-overhead image model (AGENTS.md) strips `read_image`
    /// base64 in the **main** context and tells the model to delegate instead.
    /// A child is already the isolation boundary that rule exists to create —
    /// its context is destroyed when it returns — so stripping there buys
    /// nothing and costs the whole capability: the child sees a placeholder,
    /// is told to call `delegate_task`, and `delegate_task` is stripped from
    /// its registry by `BLOCKED_TOOLS_FOR_SUBAGENT`. That is a closed loop.
    #[serde(default)]
    pub is_sub_agent: bool,

    /// Temperature for LLM sampling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,

    /// Maximum tokens for LLM completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    /// Maximum number of concurrent delegate_task tool calls.
    #[serde(default = "default_max_concurrent_delegates")]
    pub max_concurrent_delegates: u32,

    /// Sub-agent configuration.
    #[serde(default)]
    pub sub_agent: SubAgentConfig,

    /// Flush memories every N turns.
    #[serde(default = "default_memory_flush_interval")]
    pub memory_flush_interval: u32,

    /// Session identifier — used for memory and persistence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    /// HERMIX_HOME path — used by memory plugins for config resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hermix_home: Option<String>,

    /// Scoped memory directory for this agent's memory backend.
    ///
    /// When `Some(dir)`, the `memory` tool and the memory prompt/
    /// prefetch injections read/write under `dir` instead of the
    /// (private 1:1 chat) and per-override-scope memory isolation,
    /// so a scoped agent cannot read memories that were stored in
    /// another scope, and different overrides cannot read each
    /// other's memories.
    ///
    /// `None` keeps the legacy behaviour: fall back to the global
    /// `<hermix_home>/memories` directory (single shared scope).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_dir: Option<String>,

    /// Skip memory integration (read prefetch + write hooks).
    /// The write-path methods iterate the memory router and fan out to each
    /// provider's on_* hooks. Set `true` to short-circuit both
    /// the read path and the write fan-out.
    #[serde(default)]
    pub skip_memory: bool,

    /// hard char cap on the `## Memory` block injected into the
    /// system prompt per turn. Hits below `memory_min_salience` are
    /// dropped first; the remaining list is appended top-down until
    /// the budget is exhausted. Default `4000` chars ≈ 1.5-2K tokens
    /// for mixed CJK/ASCII content — was 12000 (~6K tokens), which
    /// dominated context windows without improving recall quality.
    /// Set to `0` to disable the cap entirely (legacy behaviour).
    #[serde(default = "default_memory_char_budget")]
    pub memory_char_budget: usize,

    /// noise floor for memory recall. Hits with `salience < this`
    /// are dropped before the char budget is applied. Default `0.30`
    /// filters out low-signal fragments (e.g. session boilerplate,
    /// generated text), letting the budget spend tokens on facts the
    /// user actually cares about. Set to `0.0` to disable.
    #[serde(default = "default_memory_min_salience")]
    pub memory_min_salience: f32,

    /// cap on subsequent-turn recall size (after the first full
    /// recall has seeded the context). Default `5` — keeps the
    /// `## Memory` block small for ongoing turns since the LLM
    /// already has prior memory in the active context window.
    #[serde(default = "default_memory_recall_limit_subsequent")]
    pub memory_recall_limit_subsequent: usize,

    /// cap on first-turn recall size (before any context seeding).
    /// Default `50` — wide net to surface everything relevant at
    /// session start.
    #[serde(default = "default_memory_recall_limit_first")]
    pub memory_recall_limit_first: usize,

    /// (reflection → 行为改写闭环): number of recent
    /// cross-session reflections to surface into the `## Prior
    /// Reflections` block of `memory_prefetch`. Set to 0 to disable.
    /// Default `2`.
    #[serde(default = "default_prior_reflection_max_items")]
    pub prior_reflection_max_items: usize,

    /// per-item character budget in the prior reflection block.
    /// Caps each reflection body so a noisy session can't blow out
    /// the system prompt. Default `200`.
    #[serde(default = "default_prior_reflection_per_item_chars")]
    pub prior_reflection_per_item_chars: usize,

    /// (tool error demotion): failure threshold above which a
    /// tool is surfaced as a soft-hint in the system prompt. Set
    /// to `0` to disable the feature entirely. Default `2`.
    #[serde(default = "default_tool_error_demotion_threshold")]
    pub tool_error_demotion_threshold: usize,

    /// cap on rows in the tool-error demotion block. Default
    /// `5` — keeps the block short even when many tools have failed.
    #[serde(default = "default_tool_error_demotion_max_items")]
    pub tool_error_demotion_max_items: usize,

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

    /// Pressure ratio (0.0–1.0) at which auto-compression and context-pressure
    /// warnings fire. Forwarded from `compaction.pressure_threshold_percent` in
    /// config.yaml. `None` falls back to the hardcoded
    /// [`crate::context::CONTEXT_PRESSURE_TRIGGER_RATIO`] (0.90).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_pressure_ratio: Option<f64>,

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

    /// 会话起始时间（RFC3339），在 per-request config 组装时写一次。
    /// 系统提示尾部的 "Conversation started" 块用它而非 `now()`：
    /// 每 turn 变化的时间戳会让 provider 的自动前缀缓存（DeepSeek /
    /// MiniMax 等）从差异字节起全部 miss——tools 块 + 整段消息历史
    /// 每 turn 重付全价。会话内恒定 = 前缀稳定 = 缓存命中。
    /// `None` 时回退到 `now()`（单 turn 一次性场景无差异）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_started_at: Option<String>,

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

    /// P6 : declarative permission profile. Before dispatching a
    /// `file`/`terminal` tool, `decide_pre_tool_dispatch` checks the target
    /// resource against `granted_paths` / `granted_commands`; anything
    /// outside scope triggers an approval prompt so the user can grant
    /// access — instead of the agent silently hitting `access denied` and
    /// fabricating a generic reply. Empty = safe default (everything outside
    /// an explicit grant is subject to the prompt).
    #[serde(default)]
    pub permission_profile: PermissionProfile,

    /// Per-turn permission policy. `Default` triggers a `RequestApproval`
    /// stream event before risk-bearing tool calls; `Full` skips the gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<PermissionLevel>,

    /// Per-turn ephemeral skill set. Names of skills to surface in the
    /// system prompt on top of any server-side enabled skills.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ephemeral_skills: Vec<String>,

    /// Workbench bound to this session. `Some` gates the `workbench.context`
    /// direction (module index / vendor / manifest injected per-turn) —
    /// ordinary chats never see workbench context. Wired from
    /// `SendMessageRequest.workbench_id`; see WORKBENCH_CHAT_REDESIGN_PLAN B.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workbench_id: Option<String>,

    /// L1 domain-mode pin (`DomainMode`). `Some` forces the domain the user
    /// picked (e.g. `Code` for "编码模式"); `None` leaves it to auto-detection
    /// (`DomainModeDetector`). Wired from `AgentOverrides.domain_override`
    /// (parsed from string in `apply_overrides`). The resolved mode is what
    /// `prompt_assembly` feeds into `RouteContext.domain_mode`, gating the
    /// per-domain `system_directive` block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain_override: Option<DomainMode>,

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

    /// Maximum characters per turn for the auto-activated skills
    /// injection block . The loader caps each individual
    /// SKILL.md body at 500 lines, and this field caps the joined
    /// block so even many-matched-skills can't blow past the soft
    /// context budget. Excess is truncated; not an error.
    #[serde(default = "default_skill_injection_char_budget")]
    pub skill_injection_char_budget: usize,

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

    /// Hard outer timeout (seconds) for a single streaming LLM completion.
    /// `collect_stream_llm_response` would otherwise block indefinitely
    /// on `stream.next().await` if the upstream provider opens the
    /// SSE / chunked connection but never sends any chunk (network
    /// hang, API outage, silently dropped TCP). The inner
    /// `silence_watch` only emits activity notices — it does not
    /// abort the stream — so without this outer timeout the agent
    /// loop and the UI both hang forever on a dead provider.
    /// When the deadline elapses, the collector assembles whatever
    /// content / reasoning / tool_calls were accumulated so far,
    /// stamps `finish_reason = "timeout"`, and returns
    /// [`StreamCollectOutcome::Complete`]. The agent loop then routes
    /// through `decide_empty_response` exactly like any other
    /// thinking-only reply, so a stuck provider degrades into a
    /// visible post-tool nudge (or finalizes if all nudges are
    /// exhausted) instead of pinning the UI at "正在思考".
    /// `0` disables the hard timeout (legacy behaviour). The default
    /// 180 s mirrors the frontend `STALE_STREAM_TIMEOUT_MS` budget
    /// while leaving room for genuinely long tool-heavy turns.
    #[serde(default = "default_stream_overall_timeout_secs")]
    pub stream_overall_timeout_secs: u64,

    /// hard deadline (seconds) for the **first chunk** of a
    /// streaming completion to arrive.
    /// Background: providers like minimax/MiniMax-M3 and similar
    /// occasionally open the SSE / chunked connection successfully but
    /// never push the first chunk (TCP keepalive on but no payload).
    /// The previous `silence_watch` only emitted "still waiting"
    /// notices — it didn't *abort* — so the agent loop and the
    /// "正在思考…" indicator both stayed pinned until the outer
    /// `stream_overall_timeout_secs` (default 180s) fired.
    /// This setting adds a tighter, **first-chunk-only** deadline: if
    /// no chunk has arrived within `stream_first_chunk_timeout_secs`,
    /// `collect_stream_inner` returns early with
    /// `finish_reason = "no_first_chunk"`. Subsequent chunks are not
    /// gated by this — once the stream is producing data, normal
    /// `silence_watch` + `stream_activity_stall_secs` notice logic
    /// takes over.
    /// Default 30 s is enough for slow providers (network hitches,
    /// cold caches) but bounded enough that an Open SSE / no-data
    /// hang gets surfaced to `decide_empty_response` within 30 s
    /// instead of the full 180 s outer watchdog. `0` opts out (legacy
    /// behaviour — only the outer hard timeout applies).
    #[serde(default = "default_stream_first_chunk_timeout_secs")]
    pub stream_first_chunk_timeout_secs: u64,

    /// Inter-chunk idle deadline (seconds) — the **post-first-chunk**
    /// counterpart of `stream_first_chunk_timeout_secs`.
    /// The first-chunk tripwire only covers "SSE opened, no data ever".
    /// Once data starts flowing the stream could still die silently
    /// (provider stops pushing, TCP stays open): previously the idle arm
    /// degraded to a 24h sleep + a notice, so the turn hung until the
    /// outer 180 s watchdog — which then returned an **empty** reply and
    /// threw away the partial text already shown to the user.
    /// With this deadline, a gap longer than `stream_idle_timeout_secs`
    /// between two chunks aborts collection and returns the partial
    /// content as `Interrupted(finish_reason="stream_idle")`, so the
    /// already-visible text is persisted and never replayed.
    /// Default 120 s (≈4× the default 30 s activity cadence);
    /// `0` opts out (legacy: notify forever, outer timeout decides).
    #[serde(default = "default_stream_idle_timeout_secs")]
    pub stream_idle_timeout_secs: u64,

    /// hard deadline (seconds) for the gap between the end
    /// of one LLM turn's tool-execution bookkeeping and the start of
    /// the next turn's stream-collection call. Watched by the
    /// `run`/`run_stream` main loops via `last_progress_at`. If the
    /// main loop has not crossed a progress checkpoint within this
    /// window, it logs a `turn_transition_stall` warning and emits a
    /// terminal `Done` event so the WS client recovers (rather than
    /// the user being stuck on the "正在思考…" indicator).
    /// Root cause that motivated this setting: the `b35998b` SSE
    /// first-chunk watchdog (above) only fixes "no chunk after the
    /// TCP open". It does **not** cover the second class of stall
    /// observed in production on — turn 1 finishes with
    /// `finish_reason="tool_calls"`, the user clicks "允许", the tool
    /// executes and `do_post_tool_iteration` returns, but turn 2
    /// never starts. The watchdog covers the *gap* between
    /// `do_post_tool_iteration` and the next `do_pre_turn_setup`.
    /// 90 s is wide enough to absorb a benign stall (e.g. an SQLite
    /// write under contention, a slow `auto_compress_if_over_threshold`
    /// spawn), but short enough that the user sees recovery before
    /// the outer `stream_overall_timeout_secs` (180 s) would have
    /// tripped on a stale stream anyway. `0` opts out (legacy
    /// behaviour — no watchdog).
    #[serde(default = "default_turn_transition_deadline_secs")]
    pub turn_transition_deadline_secs: u64,

    /// Max tools to surface to the LLM after tool-routing .
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

/// (startup guard): validate the assembled `AgentConfig` before the
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
    // 0 = unlimited. The agent loop relies on:
    // 1. ErrorLoop hard stop (convergence detector: 8 consecutive all-error turns)
    // 2. Cost Guard (max_cost_usd, if configured)
    // 3. LLM natural stop (no tool calls → finalize)
    // max_turns > 0 can be set in config.yaml as a last-resort safety net.
    0
}

fn default_max_tool_calls_per_turn() -> u32 {
    20
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

/// hard char cap on `## Memory` block per turn. Was 12000 (~6K tokens)
/// which dominated context windows. 4000 chars ≈ 1.5-2K tokens is enough
/// for the 5-8 highest-salience hits the LLM actually uses.
fn default_memory_char_budget() -> usize {
    4000
}

/// noise floor — drops hits with salience below this before budgeting.
/// 0.30 keeps only meaningful facts; legacy behaviour was to inject everything.
fn default_memory_min_salience() -> f32 {
    0.30
}

/// cap on subsequent-turn recall. Down from 10 — the LLM already has
/// the prior turn's memory in active context.
fn default_memory_recall_limit_subsequent() -> usize {
    5
}

/// first-turn recall size. Stays at 50 — wide net to surface everything.
fn default_memory_recall_limit_first() -> usize {
    50
}

/// max cross-session reflections injected into the system
/// prompt per turn. `2` covers "improvement + preference" most turns.
fn default_prior_reflection_max_items() -> usize {
    2
}

/// per-item char budget on the prior reflection block.
/// `200` keeps one reflection line short while still surfacing the
/// actionable sentence.
fn default_prior_reflection_per_item_chars() -> usize {
    200
}

/// tool error demotion threshold. `2` means a tool gets
/// the soft-hint treatment after the second failed call.
fn default_tool_error_demotion_threshold() -> usize {
    2
}

/// max rows in the demotion block.
fn default_tool_error_demotion_max_items() -> usize {
    5
}

fn default_skill_creation_nudge_interval() -> u32 {
    10
}

/// default auto-skill-injection budget.
/// 16,000 chars ≈ 4,000 tokens — enough for 3 skills at the 500-line
/// soft cap, well under any modern context window. Operators can
/// raise / lower this via `cli-config.yaml`.
fn default_skill_injection_char_budget() -> usize {
    16_000
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

/// Default hard outer timeout for a single streaming completion.
/// 180 s (3 min) gives the agent loop room for a long tool-heavy
/// turn while still bounding the worst case when a provider silently
/// drops the connection. Tunable via `stream_overall_timeout_secs`
/// in `AgentConfig`.
fn default_stream_overall_timeout_secs() -> u64 {
    180
}

/// Default deadline (seconds) for the *first chunk* of a streaming
/// completion to arrive. Acts as a tighter tripwire than the
/// 180 s `stream_overall_timeout_secs` for providers that open the
/// SSE connection but never push data — see
/// [`AgentConfig::stream_first_chunk_timeout_secs`].
fn default_stream_first_chunk_timeout_secs() -> u64 {
    30
}

/// Post-first-chunk idle deadline — see
/// [`AgentConfig::stream_idle_timeout_secs`].
fn default_stream_idle_timeout_secs() -> u64 {
    120
}

/// Default watchdog (seconds) for the window spanning tool execution
/// (including the approval wait) through the next turn's pre-turn
/// setup. See [`AgentConfig::turn_transition_deadline_secs`].
fn default_turn_transition_deadline_secs() -> u64 {
    // The armed window now covers tool execution INCLUDING the approval
    // wait (run_loop arms before do_tool_turn). The deadline must exceed
    // the largest legitimate wait inside that window: approval timeout
    // (ApprovalConfig.timeout_secs, default 600s) + batch tool budget
    // (sum-based, easily > 300s for multi-call batches). 1800s is the
    // last-resort net above those bounds — hitting it means every
    // bounded await has failed and the run is genuinely stuck. The old
    // 30s default assumed the window was post-execution bookkeeping
    // only and would false-positive on every legitimate long
    // approval/tool wait.
    1800
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: default_max_turns(),
            max_tool_calls_per_turn: default_max_tool_calls_per_turn(),
            budget: BudgetConfig::default(),
            model: default_model(),
            api_mode: ApiMode::default(),
            retry: RetryConfig::default(),
            write_retry: WriteRetryConfig::default(),
            sub_agent: SubAgentConfig::default(),
            system_prompt: None,
            personality: None,
            extra_body: None,
            stream: false,
            quiet_mode: false,
            is_sub_agent: false,
            temperature: None,
            max_tokens: None,
            max_concurrent_delegates: default_max_concurrent_delegates(),
            memory_flush_interval: default_memory_flush_interval(),
            session_id: None,
            hermix_home: None,
            memory_dir: None,
            skip_memory: false,
            memory_char_budget: default_memory_char_budget(),
            memory_min_salience: default_memory_min_salience(),
            memory_recall_limit_subsequent: default_memory_recall_limit_subsequent(),
            memory_recall_limit_first: default_memory_recall_limit_first(),
            prior_reflection_max_items: default_prior_reflection_max_items(),
            prior_reflection_per_item_chars: default_prior_reflection_per_item_chars(),
            tool_error_demotion_threshold: default_tool_error_demotion_threshold(),
            tool_error_demotion_max_items: default_tool_error_demotion_max_items(),
            smart_model_routing: SmartModelRoutingConfig::default(),
            provider: None,
            platform: None,
            platform_hint_override: None,
            pass_session_id: false,
            session_started_at: None,
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
            skill_injection_char_budget: default_skill_injection_char_budget(),
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
            stream_overall_timeout_secs: default_stream_overall_timeout_secs(),
            stream_idle_timeout_secs: default_stream_idle_timeout_secs(),
            stream_first_chunk_timeout_secs: default_stream_first_chunk_timeout_secs(),
            turn_transition_deadline_secs: default_turn_transition_deadline_secs(),
            workspace: None,
            permission_profile: PermissionProfile::default(),
            permission: None,
            llm_providers: HashMap::new(),
            context_pressure_ratio: None,
            ephemeral_skills: Vec::new(),
            workbench_id: None,
            domain_override: None,
            max_routed_tools: default_max_routed_tools(),
            active_toolsets: Vec::new(),
            autonomous_mode: default_autonomous_mode(),
        }
    }
}

// ---------------------------------------------------------------------------
// Scoped memory resolution
// ---------------------------------------------------------------------------

impl AgentConfig {
    /// Resolve the scoped memory directory for this agent's memory backend.
    ///
    /// * When `memory_dir` is set, it is used verbatim — any caller-supplied
    ///   scope override (e.g. a multi-agent room).
    /// * Otherwise (private 1:1 chat) the directory is auto-derived per
    ///   session as `<hermix_home>/sessions/<session_id>/memories`.
    ///
    /// This is the read/write root for both the `memory` tool
    /// (`FileMemoryBackend`) and the prompt/prefetch memory injections, so
    /// every scope physically owns its own `MEMORY.md` / `USER.md` and can
    /// never observe another scope's data.
    pub fn resolved_memory_dir(&self) -> PathBuf {
        if let Some(md) = &self.memory_dir {
            return PathBuf::from(md);
        }
        let home = self
            .hermix_home
            .clone()
            .unwrap_or_else(|| hermix_config::paths::hermix_home().to_string_lossy().to_string());
        PathBuf::from(home)
            .join("sessions")
            .join(fs_safe_session_id(self.session_id.as_deref().unwrap_or_default()))
            .join("memories")
    }

    /// Whether the read paths may fall back to the legacy global
    /// `<hermix_home>/memories` when the scoped directory has no
    /// `MEMORY.md` yet.
    ///
    /// Returns `true` only for the auto-derived private scope
    /// (`memory_dir` is `None`), so existing private memories stay visible
    /// right after the migration (they live in the global directory). Group
    /// room scopes (`memory_dir` set) return `false` — they must never read
    /// global, which is the entire point of the isolation fix.
    ///
    /// Persona-entity sessions (`twin:*` — e.g. the twin:ali assistant)
    /// also return `false`: she is an independent individual whose memory
    /// grows from blank in her own scope. Reading the main assistant's
    /// global memories would leak them into her context every turn —
    /// pollution, not grounding (主人拍板 2026-10-26).
    pub fn memory_reads_global_fallback(&self) -> bool {
        if self
            .session_id
            .as_deref()
            .is_some_and(|sid| sid.starts_with(TWIN_SESSION_PREFIX))
        {
            return false;
        }
        self.memory_dir.is_none()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// (memory injection budget): the four new AgentConfig fields
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

    /// the new fields must round-trip through serde without
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

    /// 0 means "disable the cap" — used by tests and by power
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

    /// backwards-compat — existing configs without the new fields
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

    // pin the turn-transition watchdog default + the
    // "0 = opt-out" contract. Both run_stream() and run() consult
    // this field at the top of every main-loop iteration; if the
    // default drifts the WS recovery latency changes silently.
    // Default raised 30s → 1800s when the armed window was widened to
    // cover tool execution + approval wait (see
    // default_turn_transition_deadline_secs for the bound math).
    #[test]
    fn turn_transition_deadline_default_is_1800_seconds() {
        let c = AgentConfig::default();
        assert_eq!(c.turn_transition_deadline_secs, 1800);
    }

    #[test]
    fn turn_transition_deadline_zero_opt_out_roundtrips() {
        let mut c = AgentConfig::default();
        c.turn_transition_deadline_secs = 0;
        let json = serde_json::to_string(&c).unwrap();
        let back: AgentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.turn_transition_deadline_secs, 0,
            "0 must round-trip as 0 (the watchdog opt-out value)"
        );
    }

    #[test]
    fn old_config_without_turn_transition_field_uses_default() {
        // Mirrors m2_old_config_without_new_fields_uses_defaults but
        // for the new field. If the serde default ever regresses the
        // watchdog would silently disable itself on existing
        // user-saved configs.
        let c = AgentConfig::default();
        let mut v: serde_json::Value = serde_json::to_value(&c).unwrap();
        v.as_object_mut()
            .unwrap()
            .remove("turn_transition_deadline_secs");
        let back: AgentConfig = serde_json::from_value(v).unwrap();
        assert_eq!(back.turn_transition_deadline_secs, 1800);
    }

    /// P0 (memory isolation): `resolved_memory_dir` must yield the room
    /// scope verbatim when `memory_dir` is set, and the per-session
    /// directory (`<home>/sessions/<sid>/memories`) for the auto-derived
    /// private scope. `memory_reads_global_fallback` must be `false` for
    /// room scopes (never read global) and `true` for the private scope
    /// (migration aid).
    #[test]
    fn memory_isolation_resolves_scoped_dirs() {
        // Room scope: memory_dir wins verbatim, no global fallback.
        let room = AgentConfig {
            memory_dir: Some("/x/custom/scope-7/memories".into()),
            session_id: Some("ignored".into()),
            ..AgentConfig::default()
        };
        assert_eq!(
            room.resolved_memory_dir(),
            std::path::PathBuf::from("/x/custom/scope-7/memories")
        );
        assert!(!room.memory_reads_global_fallback());

        // Private scope: derived per session, falls back to global for reads.
        let private = AgentConfig {
            hermix_home: Some("/home/hermix".into()),
            session_id: Some("sess-9".into()),
            ..AgentConfig::default()
        };
        assert_eq!(
            private.resolved_memory_dir(),
            std::path::PathBuf::from("/home/hermix/sessions/sess-9/memories")
        );
        assert!(private.memory_reads_global_fallback());

        // Persona-entity scope (twin:*): own blank memory, never reads
        // global — the main assistant's memories must not leak into her
        // context (2026-10-26 拍板). The dir component is sanitized: a raw
        // `twin:ali` path is illegal on Windows (`:` reserved).
        let twin = AgentConfig {
            hermix_home: Some("/home/hermix".into()),
            session_id: Some("twin:ali".into()),
            ..AgentConfig::default()
        };
        assert_eq!(
            twin.resolved_memory_dir(),
            std::path::PathBuf::from("/home/hermix/sessions/twin_ali/memories")
        );
        assert!(!twin.memory_reads_global_fallback());
    }

    /// Session ids are wire values and may contain `:` / `/` — illegal in
    /// Windows path components. The sanitizer must keep already-safe ids
    /// byte-identical (UUIDs, timestamps) so existing directories stay valid.
    #[test]
    fn fs_safe_session_id_maps_only_unsafe_chars() {
        assert_eq!(
            super::fs_safe_session_id("twin:ali"),
            "twin_ali"
        );
        assert_eq!(
            super::fs_safe_session_id("3f2b1c9e-8a4d-4e2f-9b1a-7c6d5e4f3a2b"),
            "3f2b1c9e-8a4d-4e2f-9b1a-7c6d5e4f3a2b"
        );
        assert_eq!(super::fs_safe_session_id("a/b\\c|d?e*f"), "a_b_c_d_e_f");
    }
}
