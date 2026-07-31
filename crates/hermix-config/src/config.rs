//! Gateway configuration: the top-level config struct and its sub-types.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use hermix_core::{BudgetConfig, PermissionProfile};

use crate::platform::PlatformConfig;
use crate::session::SessionConfig;
use crate::streaming::StreamingConfig;

// ---------------------------------------------------------------------------
// GatewayConfig
// ---------------------------------------------------------------------------

/// Top-level configuration for the hermes gateway.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GatewayConfig {
    /// Default LLM model identifier (e.g. "gpt-4o", "claude-3-opus").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Personality / persona name to load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality: Option<String>,

    /// Maximum agent conversation turns before forced stop.
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,

    /// Custom system prompt override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,

    /// List of enabled tool names. Defaults to all core tools.
    #[serde(default = "default_tools")]
    pub tools: Vec<String>,

    /// Budget limits for tool output.
    #[serde(default)]
    pub budget: BudgetConfig,

    /// Per-platform configuration (keyed by platform name, e.g. "discord").
    #[serde(default)]
    pub platforms: HashMap<String, PlatformConfig>,

    /// Session management settings.
    #[serde(default)]
    pub session: SessionConfig,

    /// Streaming / progressive-output settings.
    #[serde(default)]
    pub streaming: StreamingConfig,

    /// Terminal / command-execution backend settings.
    #[serde(default)]
    pub terminal: TerminalConfig,

    /// Web-tool backend selection (`backend`, `use_gateway`, ...).
    #[serde(default)]
    pub web: ToolCapabilityConfig,

    /// Image-generation backend selection (`provider`, `use_gateway`, ...).
    #[serde(default)]
    pub image_gen: ToolCapabilityConfig,

    /// Text-to-speech backend selection (`provider`, `use_gateway`, ...).
    #[serde(default)]
    pub tts: ToolCapabilityConfig,

    /// Browser automation backend selection (`cloud_provider`, `use_gateway`, ...).
    #[serde(default)]
    pub browser: ToolCapabilityConfig,

    /// Named LLM provider configurations.
    #[serde(default)]
    pub llm_providers: HashMap<String, LlmProviderConfig>,

    /// Optional per-turn smart model routing (cheap-vs-strong).
    #[serde(default)]
    pub smart_model_routing: SmartModelRoutingConfig,

    /// Compaction / context-pressure tuning (backward-compatible: all fields
    /// optional; old `config.yaml` files without this block use defaults).
    #[serde(default)]
    pub compaction: CompactionConfig,

    /// sub-agent / delegation
    /// tuning surfaced by the desktop Settings → Advanced tab.
    /// All fields optional; old config.yaml without this block
    /// uses the agent's hardcoded defaults.
    #[serde(default)]
    pub advanced: AdvancedSettingsConfig,

    /// Optional HTTP/SOCKS proxy settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyConfig>,

    /// Approval / safety-gate settings.
    #[serde(default)]
    pub approval: ApprovalConfig,

    /// Skills enable/disable configuration.
    #[serde(default)]
    pub skills: SkillsSettings,

    /// Tools enable/disable and per-tool configuration.
    #[serde(default)]
    pub tools_config: ToolsSettings,

    /// MCP server connection configuration.
    #[serde(default)]
    pub mcp_servers: Vec<McpServerEntry>,

    /// Profile system: selected profile and named profile files.
    #[serde(default)]
    pub profile: ProfileConfig,

    /// Agent loop nudges + background review (parity with Python `memory` / `skills` cadence).
    #[serde(default)]
    pub agent: AgentLoopBehaviorConfig,

    /// When `true` (default), enables autonomous execution mode: the agent will
    /// not stop mid-task to ask the user for information. It exhausts all
    /// available tools before giving up. Default is `true` because the product
    /// philosophy is "try everything before stopping". Opt out explicitly with
    /// `autonomous_mode: false` in `config.yaml` or `HERMIX_AUTONOMOUS_MODE=0`.
    #[serde(default = "default_autonomous_mode")]
    pub autonomous_mode: bool,

    /// Override for the hermes home directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_dir: Option<String>,

    /// Durable copy of the desktop model-selector state (the full list of
    /// user-enabled models + the chosen default). The desktop UI keeps an
    /// immediate copy in `localStorage`, but `localStorage` is volatile
    /// (cleared on some Tauri storage evictions / dev HMR), which previously
    /// caused the chat model switcher to "forget" configured models and fall
    /// back to showing only the backend default. Persisting here makes the
    /// backend the authoritative source of truth so the switcher is stable
    /// across restarts. `None` (default) means "no explicit selector state" —
    /// callers fall back to `model` + `llm_providers[].model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_selector: Option<ModelSelectorConfig>,

    /// P6 : declarative permission profile. The agent
    /// gates resource-bearing tool calls (file/terminal) against this
    /// scope — anything outside triggers an approval prompt instead
    /// of silently hitting OS `access denied`. The profile is
    /// persisted to `config.yaml` so the user's "Always Allow"
    /// grants survive server restart. See
    /// `hermix_core::permission_profile::PermissionProfile`.
    #[serde(default)]
    pub permission_profile: PermissionProfile,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            model: None,
            personality: None,
            max_turns: default_max_turns(),
            system_prompt: None,
            tools: default_tools(),
            budget: BudgetConfig::default(),
            platforms: HashMap::new(),
            session: SessionConfig::default(),
            streaming: StreamingConfig::default(),
            terminal: TerminalConfig::default(),
            web: ToolCapabilityConfig::default(),
            image_gen: ToolCapabilityConfig::default(),
            tts: ToolCapabilityConfig::default(),
            browser: ToolCapabilityConfig::default(),
            llm_providers: HashMap::new(),
            smart_model_routing: SmartModelRoutingConfig::default(),
            compaction: CompactionConfig::default(),
            advanced: AdvancedSettingsConfig::default(),
            proxy: None,
            approval: ApprovalConfig::default(),
            skills: SkillsSettings::default(),
            tools_config: ToolsSettings::default(),
            mcp_servers: Vec::new(),
            profile: ProfileConfig::default(),
            agent: AgentLoopBehaviorConfig::default(),
            autonomous_mode: true,
            home_dir: None,
            model_selector: None,
            permission_profile: PermissionProfile::default(),
        }
    }
}

impl GatewayConfig {
    /// Validate the configuration and return any warnings/errors.
    /// Called at startup and by the config update endpoint.
    pub fn validate(&self) -> Vec<String> {
        let mut warnings = Vec::new();

        // Validate max_turns
        if self.max_turns == 0 {
            warnings.push("max_turns is 0 — agent will be unable to complete any task".to_string());
        } else if self.max_turns > 1000 {
            warnings.push(format!(
                "max_turns is {} (very high) — consider lowering to prevent unbounded loops",
                self.max_turns
            ));
        }

        // Validate temperature
        for (name, provider) in &self.llm_providers {
            if let Some(temp) = provider.temperature {
                if temp.is_nan() || temp.is_infinite() {
                    warnings.push(format!(
                        "provider '{}' has invalid temperature: {:?}",
                        name, temp
                    ));
                } else if temp < 0.0 || temp > 2.0 {
                    warnings.push(format!(
                        "provider '{}' temperature {} is outside recommended range [0.0, 2.0]",
                        name, temp
                    ));
                }
            }
        }

        // Validate compaction config
        if let Some(threshold) = self.compaction.threshold_percent {
            if threshold <= 0.0 || threshold >= 1.0 {
                warnings.push(format!(
                    "compaction.threshold_percent {} is outside valid range (0.0, 1.0)",
                    threshold
                ));
            }
        }
        if let Some(pressure) = self.compaction.pressure_threshold_percent {
            if pressure <= 0.0 || pressure >= 1.0 {
                warnings.push(format!(
                    "compaction.pressure_threshold_percent {} is outside valid range (0.0, 1.0)",
                    pressure
                ));
            }
        }
        // Cross-field validation: summary_target_ratio should be < threshold_percent
        if let (Some(target), Some(threshold)) = (
            self.compaction.summary_target_ratio,
            self.compaction.threshold_percent,
        ) {
            if target >= threshold {
                warnings.push(format!(
                    "compaction.summary_target_ratio ({}) >= threshold_percent ({}) — \
                     summary would be larger than the trigger threshold",
                    target, threshold
                ));
            }
        }

        // Validate MCP server entries
        for (i, server) in self.mcp_servers.iter().enumerate() {
            if server.name.trim().is_empty() {
                warnings.push(format!(
                    "mcp_servers[{}] has empty name — connection will likely fail",
                    i
                ));
            }
            if server.command.is_none() && server.url.is_none() {
                warnings.push(format!(
                    "mcp_servers[{}] '{}' has neither command nor url",
                    i, server.name
                ));
            }
        }

        warnings
    }
}

fn default_autonomous_mode() -> bool {
    true
}

// ---------------------------------------------------------------------------
// AgentLoopBehaviorConfig (Python-shaped nudge + background review)
// ---------------------------------------------------------------------------

/// Mirrors Python defaults: `memory.nudge_interval` / `skills.creation_nudge_interval`,
/// and implicit background memory/skill review when those intervals fire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentLoopBehaviorConfig {
    #[serde(default = "default_agent_memory_nudge_interval")]
    pub memory_nudge_interval: u32,
    #[serde(default = "default_agent_skill_nudge_interval")]
    pub skill_creation_nudge_interval: u32,
    /// When true (default), spawn the extra LLM pass for memory/skill review — Python has no master off-switch.
    #[serde(default = "default_agent_background_review_enabled")]
    pub background_review_enabled: bool,
    /// master off-switch for the auto-skill learning loop.
    /// When `true` (default), the background skill review may
    /// invoke `skill_manage(action='auto_extract_from_conversation')`
    /// to crystallize a learned workflow into a reusable skill.
    /// Set `agent.auto_skill_extraction: false` in
    /// `~/.hermix/config.yaml` to disable the extraction half of
    /// the loop without turning off memory review entirely.
    #[serde(default = "default_agent_auto_skill_extraction")]
    pub auto_skill_extraction: bool,
}

fn default_agent_memory_nudge_interval() -> u32 {
    10
}

fn default_agent_skill_nudge_interval() -> u32 {
    10
}

fn default_agent_background_review_enabled() -> bool {
    true
}

fn default_agent_auto_skill_extraction() -> bool {
    true
}

impl Default for AgentLoopBehaviorConfig {
    fn default() -> Self {
        Self {
            memory_nudge_interval: default_agent_memory_nudge_interval(),
            skill_creation_nudge_interval: default_agent_skill_nudge_interval(),
            background_review_enabled: default_agent_background_review_enabled(),
            auto_skill_extraction: default_agent_auto_skill_extraction(),
        }
    }
}

// ---------------------------------------------------------------------------
// CompactionConfig (Day 3+ tuning; backward-compatible — every field is
// optional, old config.yaml without this block still loads cleanly).
// ---------------------------------------------------------------------------

/// Compaction / context-pressure tuning knobs.
/// **Backward compatibility**: every field uses `#[serde(default)]` so
/// existing `~/.hermix/config.yaml` files load unchanged. New fields can
/// be added in any order without breaking deployments.
/// Mirrors `hermix_agent::compression::CompressorConfig` (the runtime
/// single-source-of-truth), but is intentionally an independent struct so
/// the hermix-config crate does not depend on hermix-agent. The agent
/// loop bridges them via `CompressorConfig::from(compaction: CompactionConfig)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactionConfig {
    /// Pressure-warning ratio: emit `Context pressure N%` warnings when
    /// prompt tokens cross this fraction of the model's context window.
    /// Distinct from the LLM-compressor trigger (`threshold_percent`); this
    /// drives the user-visible UI ladder only. Default: `0.80`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressure_threshold_percent: Option<f64>,
    /// Tokens reserved for the LLM's own response (subtracted from the
    /// effective window before `pressure_threshold_percent` is applied).
    /// Default: `4096`. Power users with 8B local models can shrink it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_reserve_tokens: Option<usize>,
    /// Compressor trigger ratio (LLM summary fires when prompt tokens
    /// exceed `context_length * threshold_percent`). Default: `0.50`.
    /// Distinct from `pressure_threshold_percent` (UI warn) — these two
    /// ratios run on independent ladders by design.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold_percent: Option<f64>,
    /// Number of head messages always preserved verbatim. Default: `3`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protect_first_n: Option<usize>,
    /// Hard minimum tail messages to protect. Default: `20`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protect_last_n: Option<usize>,
    /// Summary target as fraction of the trigger threshold. Default: `0.20`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_target_ratio: Option<f64>,
    /// Master off-switch: when `false`, all auto-compression is suppressed
    /// (the user can still `/compact` manually). Default: `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Circuit breaker: number of consecutive LLM-compression failures
    /// before the breaker trips open. Default: `3` (Anthropic's
    /// "stop wasting 250K API/day" number from their incident report).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub circuit_breaker_threshold: Option<u32>,
    /// Circuit breaker cooldown before transitioning Open → HalfOpen.
    /// Default: `600` seconds (10 minutes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub circuit_breaker_cooldown_secs: Option<u64>,
    /// O5.1: Override the model used for LLM summarisation during context
    /// compression. When set, the compressor routes through this model
    /// instead of the default auxiliary chain. Useful for users who want
    /// a cheap/fast model for summarisation (e.g. `gpt-4o-mini`) while
    /// keeping a more capable model for chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_model: Option<String>,
    /// minimum token budget the
    /// LLM-compressor must produce. Mirrors
    /// `hermix_agent::compression::CompressorConfig::min_summary_tokens`
    /// so the desktop Settings → Advanced tab can tune it via the
    /// `PUT /api/config` endpoint without restarting the agent
    /// process (the live config is forwarded via
    /// `agent_service.set_config`). Default in the agent crate is
    /// `2000`; the backend currently treats `None` as "use the
    /// hardcoded default" so existing deployments are unaffected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_summary_tokens: Option<u64>,
    /// maximum token budget the
    /// LLM-compressor is allowed to emit. Mirrors
    /// `hermix_agent::compression::CompressorConfig::summary_tokens_ceiling`.
    /// Backend default is `12_000`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_tokens_ceiling: Option<u64>,
}

/// Sub-agent orchestration / multi-step delegation tuning knobs.
/// Sits at `advanced.sub_agent_*` / `advanced.delegation_*` in
/// config.yaml. Defaults match `hermix_agent::agent_config::AgentConfig`
/// so existing deployments without this block still behave identically.
/// exposed by the desktop
/// Settings → Advanced tab. The values are forwarded to the live
/// agent service via `agent_service.set_config`; the agent
/// re-reads them on every turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AdvancedSettingsConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_agent_timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_agent_min_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_agent_max_turns_cap: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation_max_iterations: Option<u32>,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            pressure_threshold_percent: None,
            output_reserve_tokens: None,
            threshold_percent: None,
            protect_first_n: None,
            protect_last_n: None,
            summary_target_ratio: None,
            enabled: None,
            circuit_breaker_threshold: None,
            circuit_breaker_cooldown_secs: None,
            summary_model: None,
            min_summary_tokens: None,
            summary_tokens_ceiling: None,
        }
    }
}

fn default_max_turns() -> u32 {
    30
}

fn default_tools() -> Vec<String> {
    // Whitelist of core tool names enabled by default when the user
    // hasn't customised `tools:` in their config.yaml. These names
    // MUST match the keys `hermix_tools::register_builtins` actually
    // registers — earlier versions shipped a Python-era list
    // (`bash` / `read` / `write` / `edit` / `glob` / `grep` /
    // `web_fetch`) that did not correspond to any registered Rust
    // tool, so a fresh install with this default whitelist ended up
    // with ZERO core tools visible to the LLM after the
    // `enabled_toolsets.json` reconcile pass.
    // The names below are the canonical tool IDs defined in
    // `register_builtins.rs`. Empty vector means "no whitelist" —
    // every registered tool is eligible. We deliberately ship a
    // minimal default rather than the full toolset; the user can
    // extend it from the Settings UI or by editing config.yaml.
    vec![
        "terminal".into(),
        "read_file".into(),
        "write_file".into(),
        "patch".into(),
        "search_files".into(),
        "web_search".into(),
        "web_fetch".into(),
    ]
}

// ---------------------------------------------------------------------------
// LlmProviderConfig
// ---------------------------------------------------------------------------

/// Configuration for a named LLM provider endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct LlmProviderConfig {
    /// API key (or env-var reference like "${MY_API_KEY}").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,

    /// Base URL for the provider API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    /// Optional external-process command used by runtime-provider resolvers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Optional external-process argv tail.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,

    /// Default model to use for this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Maximum tokens in the completion response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    /// Sampling temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    /// Extra JSON body fields forwarded to the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_body: Option<serde_json::Value>,

    /// Requests-per-minute rate limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<u32>,

    /// Pool of credential identifiers for rotation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credential_pool: Vec<String>,

    /// OAuth2 token endpoint used for refresh flows (e.g. openai-codex, qwen-oauth).
    /// When unset, falls back to provider-specific `HERMES_<PROVIDER>_OAUTH_TOKEN_URL`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_token_url: Option<String>,

    /// OAuth2 client_id used for refresh flows.
    /// When unset, falls back to provider-specific `HERMES_<PROVIDER>_OAUTH_CLIENT_ID`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_client_id: Option<String>,

    /// Preferred upstream provider order (OpenRouter-specific).
    /// e.g. ["DeepInfra", "Together"]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_order: Vec<String>,

    /// Optional context window override (tokens) used when a model served
    /// by this provider isn't covered by the static catalog and we have no
    /// per-model value (see `model_contexts`). When set, this value wins
    /// over `hermix_intelligence::model_metadata::DEFAULT_FALLBACK_CONTEXT`
    /// (128_000). The agent loop passes provider configs through to
    /// `ContextManager::for_model_with_config`, which resolves
    /// `custom-{id}:{model}` against this map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,

    /// Per-model context window overrides (tokens). Keyed by the bare model
    /// name (no provider prefix). Resolution order when looking up
    /// `custom-{id}:{model_name}`:
    /// 1. `model_contexts[model_name]` (most specific)
    /// 2. `context_window` (provider-wide default)
    /// 3. static catalog → `DEFAULT_FALLBACK_CONTEXT`
    /// Populated automatically by `POST /api/custom-providers/{id}/fetch-models`
    /// when the upstream `/models` endpoint exposes a context length, and
    /// can also be set by hand in `config.yaml`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub model_contexts: HashMap<String, u64>,

    /// Estimated prompt-cache hit rate (0.0–1.0) for this provider, used
    /// by the analytics dashboard when the provider does NOT report
    /// `cache_read_tokens` in its usage payload (e.g. MiniMax). The
    /// dashboard shows this as an *estimate* (with a "(估算)" label)
    /// rather than a hard 0%, so the cache-hit card isn't silently
    /// wrong for providers that cache but don't expose the metric.
    /// `None` (default) means "no estimate available — show real 0%".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_hit_rate_estimate: Option<f64>,
}

// ---------------------------------------------------------------------------
// ModelSelectorConfig — durable copy of the desktop model switcher state
// ---------------------------------------------------------------------------

/// A single user-enabled model in the chat model switcher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSelectorEntry {
    /// Provider id, e.g. `"openai"`, `"siliconflow"`, `"custom-agnes"`.
    pub provider_id: String,
    /// Model id within the provider, e.g. `"gpt-4o"`, `"Qwen/Qwen2.5-7B"`.
    pub model_id: String,
}

/// Durable, backend-persisted mirror of the desktop model-selector UI state.
/// See `GatewayConfig::model_selector` for why this exists: `localStorage`
/// is volatile, so the chat switcher can "forget" configured models after a
/// restart. This lets `GET /api/model/list` serve the full enabled list as
/// the authoritative source of truth.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ModelSelectorConfig {
    /// All models the user has enabled in the switcher.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enabled: Vec<ModelSelectorEntry>,
    /// The chosen default `"provider:model"` (mirrors `GatewayConfig::model`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
}

// ---------------------------------------------------------------------------
// SmartModelRoutingConfig
// ---------------------------------------------------------------------------

/// Route short/simple turns to a cheaper model while preserving the primary model
/// for complex prompts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SmartModelRoutingConfig {
    /// Master switch.
    #[serde(default)]
    pub enabled: bool,
    /// Max chars for a message to be considered "simple".
    #[serde(default = "default_max_simple_chars")]
    pub max_simple_chars: usize,
    /// Max words for a message to be considered "simple".
    #[serde(default = "default_max_simple_words")]
    pub max_simple_words: usize,
    /// Optional cheap route target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cheap_model: Option<CheapModelRouteConfig>,
}

impl Default for SmartModelRoutingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_simple_chars: default_max_simple_chars(),
            max_simple_words: default_max_simple_words(),
            cheap_model: None,
        }
    }
}

/// Cheap route target details.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CheapModelRouteConfig {
    /// Optional provider; when set and `model` lacks provider prefix, runtime
    /// can compose `<provider>:<model>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model slug (required for routing to activate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Optional endpoint override (reserved for parity with Python config shape).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Optional env var name for api key (reserved for parity).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
}

fn default_max_simple_chars() -> usize {
    160
}

fn default_max_simple_words() -> usize {
    28
}

// ---------------------------------------------------------------------------
// TerminalConfig / TerminalBackendType
// ---------------------------------------------------------------------------

/// Which backend to use for terminal/command execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum TerminalBackendType {
    #[default]
    Local,
    Docker,
    Ssh,
    Daytona,
    Modal,
    Singularity,
}

/// Configuration for terminal/command-execution backends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerminalConfig {
    /// Which backend type to use.
    #[serde(default)]
    pub backend: TerminalBackendType,

    /// Timeout in seconds for a single command.
    #[serde(default = "default_terminal_timeout")]
    pub timeout: u64,

    /// Maximum output size in bytes.
    #[serde(default = "default_max_output_size")]
    pub max_output_size: usize,

    /// Direct-vs-managed preference when `backend = modal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modal_mode: Option<String>,

    /// Working directory override for command execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,

    /// target container name/id when `backend = docker`. The
    /// container must already be running — we deliberately don't
    /// auto-spawn one. Defaults to `hermix-sandbox`; override via
    /// `~/.hermix/config.yaml` to point at a user-managed dev
    /// container without recompiling.
    #[serde(default = "default_docker_container")]
    pub docker_container: String,
}

fn default_docker_container() -> String {
    "hermix-sandbox".to_string()
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            backend: TerminalBackendType::default(),
            timeout: default_terminal_timeout(),
            max_output_size: default_max_output_size(),
            modal_mode: None,
            workdir: None,
            docker_container: default_docker_container(),
        }
    }
}

fn default_terminal_timeout() -> u64 {
    120
}

fn default_max_output_size() -> usize {
    1_048_576 // 1 MiB
}

// ---------------------------------------------------------------------------
// ApprovalConfig
// ---------------------------------------------------------------------------

/// Approval / safety-gate settings for dangerous operations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ApprovalConfig {
    /// Whether the approval gate is enabled.
    #[serde(default)]
    pub enabled: bool,

    /// List of command patterns that are considered dangerous.
    #[serde(default)]
    pub dangerous_commands: Vec<String>,

    /// Whether to require explicit approval for all tool calls.
    #[serde(default)]
    pub require_approval: bool,

    /// Commands matching whitelist bypass confirmation.
    #[serde(default)]
    pub whitelist_commands: Vec<String>,
}

/// Shared shape for tool-capability routing config copied from Python's
/// `config.yaml` sections such as `web`, `tts`, `image_gen`, and `browser`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ToolCapabilityConfig {
    /// Generic backend selector (`web.backend`, etc).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    /// Generic provider selector (`tts.provider`, `image_gen.provider`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Browser-specific cloud provider selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_provider: Option<String>,
    /// When true, prefer the Nous Tool Gateway over direct credentials.
    #[serde(default)]
    pub use_gateway: bool,
    /// Browser-specific: CDP port Chrome/Edge listens on.
    /// Defaults to `9222` (Chrome's `--remote-debugging-port` default).
    /// Set to a different port (e.g. `9223`) when running multiple browsers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cdp_port: Option<u16>,
    /// Browser-specific: launch Chrome/Edge with `--headless=new`.
    /// `None` falls back to `HERMIX_BROWSER_HEADLESS` env var, then
    /// defaults to `true` (matches vercel-labs/agent-browser behaviour
    /// and avoids surprise GUI windows on headless servers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headless: Option<bool>,
}

/// Skills configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillsSettings {
    #[serde(default)]
    pub enabled: Vec<String>,
    #[serde(default)]
    pub disabled: Vec<String>,
    /// when true (default), tasks that complete ≥
    /// `skill_creation_nudge_interval` iters prompt the LLM to
    /// auto-extract a reusable skill via
    /// `skill_manage(action='auto_extract_from_conversation')`.
    /// Set to false in `~/.hermix/config.yaml` under `skills:` to
    /// opt out without disabling `background_review_enabled`.
    #[serde(default = "default_skills_auto_extraction")]
    pub auto_extraction: bool,
}

impl Default for SkillsSettings {
    fn default() -> Self {
        Self {
            enabled: Vec::new(),
            disabled: Vec::new(),
            auto_extraction: default_skills_auto_extraction(),
        }
    }
}

fn default_skills_auto_extraction() -> bool {
    true
}

/// Tools configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ToolsSettings {
    #[serde(default)]
    pub enabled: Vec<String>,
    #[serde(default)]
    pub disabled: Vec<String>,
    #[serde(default)]
    pub per_tool: HashMap<String, serde_json::Value>,
}

/// MCP server entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct McpServerEntry {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Active profile info.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ProfileConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    #[serde(default)]
    pub available: Vec<String>,
}

// ---------------------------------------------------------------------------
// ProxyConfig
// ---------------------------------------------------------------------------

/// HTTP/SOCKS proxy settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ProxyConfig {
    /// HTTP proxy URL (e.g. "http://proxy:8080").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_proxy: Option<String>,

    /// SOCKS5 proxy URL (e.g. "socks5://proxy:1080").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socks_proxy: Option<String>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_config_default() {
        let cfg = GatewayConfig::default();
        assert_eq!(cfg.max_turns, 30);
        assert!(!cfg.tools.is_empty());
        assert!(cfg.model.is_none());
        assert!(cfg.proxy.is_none());
        // autonomous_mode defaults to true: product philosophy is "try
        // everything before stopping". Opt out explicitly.
        assert!(
            cfg.autonomous_mode,
            "autonomous_mode must default to true"
        );
    }

    #[test]
    fn autonomous_mode_yaml_missing_uses_true_default() {
        // Real-world config.yaml files don't have `autonomous_mode` set.
        // When deserialised, serde's `default` must return `true`.
        let yaml = r#"
max_turns: 30
model: foo
"#;
        let cfg: GatewayConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(
            cfg.autonomous_mode,
            "missing autonomous_mode must default to true"
        );
    }

    #[test]
    fn autonomous_mode_yaml_false_overrides_default() {
        let yaml = r#"
autonomous_mode: false
max_turns: 30
"#;
        let cfg: GatewayConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(
            !cfg.autonomous_mode,
            "explicit false in yaml must override default true"
        );
    }

    #[test]
    fn gateway_config_serde_roundtrip() {
        let cfg = GatewayConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: GatewayConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.max_turns, cfg.max_turns);
        assert_eq!(back.tools, cfg.tools);
    }

    #[test]
    fn terminal_backend_type_serde() {
        let t = TerminalBackendType::Docker;
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(json, "\"docker\"");
        let back: TerminalBackendType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, TerminalBackendType::Docker);
    }

    #[test]
    fn approval_config_default() {
        let a = ApprovalConfig::default();
        assert!(!a.enabled);
        assert!(!a.require_approval);
        assert!(a.dangerous_commands.is_empty());
    }

    #[test]
    fn proxy_config_serde() {
        let p = ProxyConfig {
            http_proxy: Some("http://proxy:8080".into()),
            socks_proxy: None,
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: ProxyConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.http_proxy, Some("http://proxy:8080".to_string()));
        assert_eq!(back.socks_proxy, None);
    }
}
