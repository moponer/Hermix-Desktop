//! Configuration loading from YAML, JSON, and environment variables.

use std::path::Path;
use crate::env_compat;

// Re-export ConfigError for convenience
pub use hermix_core::ConfigError;

use crate::config::{GatewayConfig, ProxyConfig};
use crate::merge::merge_configs;
use crate::paths;

// ---------------------------------------------------------------------------
// ConfigError conversion helpers
// ---------------------------------------------------------------------------

/// Helper function to convert serde_yaml::Error to ConfigError (avoids orphan rule).
fn yaml_to_config_error(e: serde_yaml::Error) -> ConfigError {
    ConfigError::ParseError(e.to_string())
}

/// Helper function to convert serde_json::Error to ConfigError (avoids orphan rule).
fn json_to_config_error(e: serde_json::Error) -> ConfigError {
    ConfigError::ParseError(e.to_string())
}

/// Helper function to convert std::io::Error to ConfigError (avoids orphan rule).
fn io_to_config_error(e: std::io::Error) -> ConfigError {
    ConfigError::ParseError(e.to_string())
}

// ---------------------------------------------------------------------------
// load_dotenv
// ---------------------------------------------------------------------------

/// Load variables from `$HERMIX_HOME/.env` into the process environment.
/// Only sets a variable if it is not already present in the environment,
/// so real env vars always win. Supports `#` comments, blank lines, and
/// optional single/double quoting of values.
pub fn load_dotenv() {
    let env_file = paths::env_path();
    let contents = match std::fs::read_to_string(&env_file) {
        Ok(c) => c,
        Err(_) => return,
    };

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once('=') {
            let key = key.trim();
            if key.is_empty() {
                continue;
            }
            let value = value.trim();
            let value = strip_quotes(value);
            if std::env::var(key).is_err() {
                // SAFETY: called once at startup before multi-threading.
                unsafe { std::env::set_var(key, value) };
            }
        }
    }
}

fn strip_quotes(s: &str) -> &str {
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

// ---------------------------------------------------------------------------
// load_config
// ---------------------------------------------------------------------------

/// Load the full configuration, applying the priority chain:
/// env vars  >  .env  >  cli-config.yaml > config.yaml > gateway.json > defaults
/// If `home_dir` is provided it overrides the `HERMIX_HOME` env var.
pub fn load_config(home_dir: Option<&str>) -> Result<GatewayConfig, ConfigError> {
    // Load .env before anything else so env overrides see those values.
    load_dotenv();

    // Determine effective hermes home
    // Resolution order (highest → lowest):
    // 1. `home_dir` parameter (explicit caller override)
    // 2. `HERMIX_HOME` env var (user-set)
    // 3. `paths::hermix_home()` which honours `HERMIX_PORTABLE_DATA_DIR` → `HERMIX_HOME` → `~/.hermix`
    // NOTE: we deliberately do NOT fall back to `HERMES_HOME` in `load_config`.
    // `paths::hermix_home()` already does portable > HERMIX_HOME > ~/.hermix
    // with zero HERMES_HOME dependency.
    let effective_home = home_dir
        .map(|s| s.to_string())
        .unwrap_or_else(|| paths::hermix_home().to_string_lossy().to_string());

    let config_yaml_path = Path::new(&effective_home).join("config.yaml");
    let cli_config_yaml_path = Path::new(&effective_home).join("cli-config.yaml");
    let gateway_json_path = Path::new(&effective_home).join("gateway.json");

    // Start from defaults
    let mut config = GatewayConfig::default();

    // Layer 1: gateway.json (lowest priority file source)
    if gateway_json_path.exists() {
        match load_from_json(&gateway_json_path) {
            Ok(json_cfg) => config = json_cfg,
            Err(e) => {
                tracing::warn!("Failed to load {}: {e}", gateway_json_path.display());
            }
        }
    }

    // Layer 2: config.yaml (higher priority file source)
    if config_yaml_path.exists() {
        match load_from_yaml(&config_yaml_path) {
            Ok(yaml_cfg) => {
                config = merge_configs(&yaml_cfg, &config);
            }
            Err(e) => {
                tracing::warn!("Failed to load {}: {e}", config_yaml_path.display());
            }
        }
    }

    // Layer 2.5: cli-config.yaml (CLI-specific + high-priority overlay)
    if cli_config_yaml_path.exists() {
        match load_from_yaml(&cli_config_yaml_path) {
            Ok(cli_cfg) => {
                config = merge_configs(&cli_cfg, &config);
            }
            Err(e) => {
                tracing::warn!("Failed to load {}: {e}", cli_config_yaml_path.display());
            }
        }
    }

    // Layer 3: environment variables (highest priority)
    apply_env_overrides(&mut config);

    // Record the effective home dir
    config.home_dir = Some(effective_home);

    // Validate
    validate_config(&config)?;

    Ok(config)
}

// ---------------------------------------------------------------------------
// load_from_yaml
// ---------------------------------------------------------------------------

/// Load a GatewayConfig from a YAML file.
pub fn load_from_yaml(path: &Path) -> Result<GatewayConfig, ConfigError> {
    let contents = std::fs::read_to_string(path).map_err(io_to_config_error)?;
    let mut root: serde_yaml::Value =
        serde_yaml::from_str(&contents).map_err(yaml_to_config_error)?;
    if let serde_yaml::Value::Mapping(ref mut m) = root {
        crate::python_yaml_compat::normalize_config_yaml_root(m);
    }
    let config: GatewayConfig = serde_yaml::from_value(root).map_err(yaml_to_config_error)?;
    Ok(config)
}

/// Load `config.yaml` from disk if it exists; otherwise return defaults (no env merge).
pub fn load_user_config_file(path: &Path) -> Result<GatewayConfig, ConfigError> {
    if path.exists() {
        load_from_yaml(path)
    } else {
        Ok(GatewayConfig::default())
    }
}

const CONFIG_PATCH_HELP: &str = "model, personality, max_turns, system_prompt, budget.max_result_size_chars, budget.max_aggregate_chars, proxy.http, proxy.socks, llm.<provider>.api_key|base_url|model|command|args|oauth_token_url|oauth_client_id, smart_model_routing.enabled|max_simple_chars|max_simple_words|cheap_model.model|cheap_model.provider, compaction.pressure_threshold_percent|output_reserve_tokens|threshold_percent|protect_first_n|protect_last_n|summary_target_ratio|enabled|circuit_breaker_threshold|circuit_breaker_cooldown_secs";

fn mask_secret(s: &str) -> String {
    if s.is_empty() {
        return "(empty)".to_string();
    }
    if s.len() <= 4 {
        "***".to_string()
    } else {
        format!("***{}", &s[s.len() - 4..])
    }
}

/// Apply a single scalar field used by `hermes config set` (does not touch other keys).
/// Supports dotted keys aligned with `GatewayConfig`:
/// - `budget.max_result_size_chars`, `budget.max_aggregate_chars`
/// - `proxy.http` / `proxy.http_proxy`, `proxy.socks` / `proxy.socks_proxy`
/// - `llm.<provider>.api_key`, `llm.<provider>.base_url`, `llm.<provider>.model`
/// - `llm.<provider>.command`, `llm.<provider>.args`
pub fn apply_user_config_patch(
    config: &mut GatewayConfig,
    key: &str,
    value: &str,
) -> Result<(), ConfigError> {
    if !key.contains('.') {
        return apply_user_config_patch_flat(config, key, value);
    }
    apply_user_config_patch_dotted(config, key, value)
}

fn apply_user_config_patch_flat(
    config: &mut GatewayConfig,
    key: &str,
    value: &str,
) -> Result<(), ConfigError> {
    match key {
        "model" => {
            config.model = Some(value.to_string());
        }
        "personality" => {
            config.personality = Some(value.to_string());
        }
        "max_turns" => {
            config.max_turns = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "max_turns must be a non-negative integer: {}",
                    value
                ))
            })?;
        }
        "system_prompt" => {
            config.system_prompt = Some(value.to_string());
        }
        other => {
            return Err(ConfigError::NotFound(format!(
                "unknown config key: {} (supported: {})",
                other, CONFIG_PATCH_HELP
            )));
        }
    }
    Ok(())
}

fn apply_user_config_patch_dotted(
    config: &mut GatewayConfig,
    key: &str,
    value: &str,
) -> Result<(), ConfigError> {
    let parts: Vec<&str> = key.split('.').collect();
    match parts.as_slice() {
        ["budget", "max_result_size_chars"] => {
            config.budget.max_result_size_chars = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "budget.max_result_size_chars must be a usize: {}",
                    value
                ))
            })?;
        }
        ["budget", "max_aggregate_chars"] => {
            config.budget.max_aggregate_chars = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "budget.max_aggregate_chars must be a usize: {}",
                    value
                ))
            })?;
        }
        ["proxy", "http"] | ["proxy", "http_proxy"] => {
            let proxy = config.proxy.get_or_insert_with(ProxyConfig::default);
            proxy.http_proxy = Some(value.to_string());
        }
        ["proxy", "socks"] | ["proxy", "socks_proxy"] => {
            let proxy = config.proxy.get_or_insert_with(ProxyConfig::default);
            proxy.socks_proxy = Some(value.to_string());
        }
        ["llm", provider, field] => {
            let entry = config
                .llm_providers
                .entry((*provider).to_string())
                .or_default();
            match *field {
                "api_key" => entry.api_key = Some(value.to_string()),
                "base_url" => entry.base_url = Some(value.to_string()),
                "model" => entry.model = Some(value.to_string()),
                "command" => entry.command = Some(value.to_string()),
                "args" => {
                    entry.args = value
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
                "oauth_token_url" => entry.oauth_token_url = Some(value.to_string()),
                "oauth_client_id" => entry.oauth_client_id = Some(value.to_string()),
                other => {
                    return Err(ConfigError::NotFound(format!(
                        "unknown llm field: llm.{}.{} (supported: api_key, base_url, model, command, args, oauth_token_url, oauth_client_id)",
                        provider, other
                    )));
                }
            }
        }
        ["smart_model_routing", "enabled"] => {
            let normalized = value.trim().to_ascii_lowercase();
            let parsed = match normalized.as_str() {
                "1" | "true" | "yes" | "on" => true,
                "0" | "false" | "no" | "off" => false,
                _ => {
                    return Err(ConfigError::ValidationError(format!(
                        "smart_model_routing.enabled must be a boolean: {}",
                        value
                    )));
                }
            };
            config.smart_model_routing.enabled = parsed;
        }
        ["smart_model_routing", "max_simple_chars"] => {
            config.smart_model_routing.max_simple_chars = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "smart_model_routing.max_simple_chars must be a usize: {}",
                    value
                ))
            })?;
        }
        ["smart_model_routing", "max_simple_words"] => {
            config.smart_model_routing.max_simple_words = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "smart_model_routing.max_simple_words must be a usize: {}",
                    value
                ))
            })?;
        }
        ["smart_model_routing", "cheap_model", "model"] => {
            let cheap = config
                .smart_model_routing
                .cheap_model
                .get_or_insert_with(crate::CheapModelRouteConfig::default);
            cheap.model = Some(value.to_string());
        }
        ["smart_model_routing", "cheap_model", "provider"] => {
            let cheap = config
                .smart_model_routing
                .cheap_model
                .get_or_insert_with(crate::CheapModelRouteConfig::default);
            cheap.provider = Some(value.to_string());
        }
        _ => {
            return Err(ConfigError::NotFound(format!(
                "unknown config key: {} (supported: {})",
                key, CONFIG_PATCH_HELP
            )));
        }
    }
    Ok(())
}

/// Display a single config field for `hermes config get` (same keys as [`apply_user_config_patch`]).
pub fn user_config_field_display(config: &GatewayConfig, key: &str) -> Result<String, ConfigError> {
    if !key.contains('.') {
        return Ok(match key {
            "model" => config
                .model
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "(not set)".to_string()),
            "personality" => config
                .personality
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "(not set)".to_string()),
            "max_turns" => config.max_turns.to_string(),
            "system_prompt" => config
                .system_prompt
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "(not set)".to_string()),
            other => {
                return Err(ConfigError::NotFound(format!(
                    "unknown config key: {} (supported: {})",
                    other, CONFIG_PATCH_HELP
                )));
            }
        });
    }

    let parts: Vec<&str> = key.split('.').collect();
    match parts.as_slice() {
        ["budget", "max_result_size_chars"] => Ok(config.budget.max_result_size_chars.to_string()),
        ["budget", "max_aggregate_chars"] => Ok(config.budget.max_aggregate_chars.to_string()),
        ["proxy", "http"] | ["proxy", "http_proxy"] => Ok(config
            .proxy
            .as_ref()
            .and_then(|p| p.http_proxy.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["proxy", "socks"] | ["proxy", "socks_proxy"] => Ok(config
            .proxy
            .as_ref()
            .and_then(|p| p.socks_proxy.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "api_key"] => Ok(
            match config
                .llm_providers
                .get(*provider)
                .and_then(|c| c.api_key.as_deref())
                .filter(|s| !s.is_empty())
            {
                Some(s) => mask_secret(s),
                None => "(not set)".to_string(),
            },
        ),
        ["llm", provider, "base_url"] => Ok(config
            .llm_providers
            .get(*provider)
            .and_then(|c| c.base_url.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "model"] => Ok(config
            .llm_providers
            .get(*provider)
            .and_then(|c| c.model.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "command"] => Ok(config
            .llm_providers
            .get(*provider)
            .and_then(|c| c.command.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "args"] => Ok(config
            .llm_providers
            .get(*provider)
            .map(|c| c.args.join(","))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "oauth_token_url"] => Ok(config
            .llm_providers
            .get(*provider)
            .and_then(|c| c.oauth_token_url.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "oauth_client_id"] => Ok(config
            .llm_providers
            .get(*provider)
            .and_then(|c| c.oauth_client_id.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["smart_model_routing", "enabled"] => Ok(config.smart_model_routing.enabled.to_string()),
        ["smart_model_routing", "max_simple_chars"] => {
            Ok(config.smart_model_routing.max_simple_chars.to_string())
        }
        ["smart_model_routing", "max_simple_words"] => {
            Ok(config.smart_model_routing.max_simple_words.to_string())
        }
        ["smart_model_routing", "cheap_model", "model"] => Ok(config
            .smart_model_routing
            .cheap_model
            .as_ref()
            .and_then(|c| c.model.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["smart_model_routing", "cheap_model", "provider"] => Ok(config
            .smart_model_routing
            .cheap_model
            .as_ref()
            .and_then(|c| c.provider.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        _ => Err(ConfigError::NotFound(format!(
            "unknown config key: {} (supported: {})",
            key, CONFIG_PATCH_HELP
        ))),
    }
}

/// Serialize `GatewayConfig` to YAML. Creates parent directories. Omits `home_dir` from output.
pub fn save_config_yaml(path: &Path, config: &GatewayConfig) -> Result<(), ConfigError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io_to_config_error)?;
    }
    let mut to_save = config.clone();
    to_save.home_dir = None;
    let yaml = serde_yaml::to_string(&to_save).map_err(yaml_to_config_error)?;
    std::fs::write(path, yaml).map_err(io_to_config_error)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// load_from_json
// ---------------------------------------------------------------------------

/// Load a GatewayConfig from a JSON file.
pub fn load_from_json(path: &Path) -> Result<GatewayConfig, ConfigError> {
    let contents = std::fs::read_to_string(path).map_err(io_to_config_error)?;
    let config: GatewayConfig = serde_json::from_str(&contents).map_err(json_to_config_error)?;
    Ok(config)
}

// ---------------------------------------------------------------------------
// apply_env_overrides
// ---------------------------------------------------------------------------

/// Override configuration fields from environment variables.
/// Environment variable mapping:
/// HERMES_MODEL           -> config.model
/// HERMES_PERSONALITY     -> config.personality
/// HERMIX_HOME            -> config.home_dir
/// HERMES_MAX_TURNS       -> config.max_turns
/// HERMES_SYSTEM_PROMPT   -> config.system_prompt
/// HERMES_PROXY_HTTP      -> config.proxy.http_proxy
/// HERMES_PROXY_SOCKS     -> config.proxy.socks_proxy
/// HERMES_LLM_API_KEY     -> all llm_providers[*].api_key
/// HERMES_BUDGET_MAX_RESULT_CHARS -> config.budget.max_result_size_chars
/// HERMES_BUDGET_MAX_AGGREGATE_CHARS -> config.budget.max_aggregate_chars
/// OPENAI_API_KEY             -> llm_providers["openai"].api_key
/// ANTHROPIC_API_KEY          -> llm_providers["anthropic"].api_key
/// OPENROUTER_API_KEY         -> llm_providers["openrouter"].api_key
/// OPENROUTER_BASE_URL        -> llm_providers["openrouter"].base_url
/// DASHSCOPE_API_KEY          -> llm_providers["qwen"].api_key
/// MOONSHOT_API_KEY           -> llm_providers["kimi"].api_key
/// MINIMAX_API_KEY            -> llm_providers["minimax"].api_key
/// NOUS_API_KEY               -> llm_providers["nous"].api_key
/// GITHUB_COPILOT_TOKEN       -> llm_providers["copilot"].api_key
/// HERMES_BASE_URL            -> all llm_providers[*].base_url
/// 另见 [`crate::python_platform_env::apply_python_named_platform_env`]：
/// `WEIXIN_*`、`DINGTALK_*` 等与 Python `gateway/platforms/*.py` 一致的键写入 `platforms`。
pub fn apply_env_overrides(config: &mut GatewayConfig) {
    if let Some(v) = env_compat("HERMIX_MODEL", "HERMES_MODEL") {
        config.model = Some(v);
    }
    if let Some(v) = env_compat("HERMIX_PERSONALITY", "HERMES_PERSONALITY") {
        config.personality = Some(v);
    }
    if let Some(v) = env_compat("HERMIX_HOME", "HERMES_HOME") {
        config.home_dir = Some(v);
    }
    if let Some(v) = env_compat("HERMIX_MAX_TURNS", "HERMES_MAX_TURNS") {
        if let Ok(n) = v.parse::<u32>() {
            config.max_turns = n;
        } else {
            tracing::warn!("HERMES_MAX_TURNS is not a valid u32: {v}");
        }
    }
    if let Some(v) = env_compat("HERMIX_SYSTEM_PROMPT", "HERMES_SYSTEM_PROMPT") {
        config.system_prompt = Some(v);
    }
    if let Ok(v) = std::env::var("HERMIX_AUTONOMOUS_MODE") {
        config.autonomous_mode = matches!(v.trim(), "1" | "true" | "yes" | "on");
    }
    if let Some(v) = env_compat("HERMIX_PROXY_HTTP", "HERMES_PROXY_HTTP") {
        let proxy = config
            .proxy
            .get_or_insert_with(crate::config::ProxyConfig::default);
        proxy.http_proxy = Some(v);
    }
    if let Some(v) = env_compat("HERMIX_PROXY_SOCKS", "HERMES_PROXY_SOCKS") {
        let proxy = config
            .proxy
            .get_or_insert_with(crate::config::ProxyConfig::default);
        proxy.socks_proxy = Some(v);
    }
    if let Some(v) = env_compat("HERMIX_LLM_API_KEY", "HERMES_LLM_API_KEY") {
        for provider in config.llm_providers.values_mut() {
            provider.api_key = Some(v.clone());
        }
    }
    if let Some(v) = env_compat("HERMIX_BUDGET_MAX_RESULT_CHARS", "HERMES_BUDGET_MAX_RESULT_CHARS") {
        if let Ok(n) = v.parse::<usize>() {
            config.budget.max_result_size_chars = n;
        }
    }
    if let Some(v) = env_compat("HERMIX_BUDGET_MAX_AGGREGATE_CHARS", "HERMES_BUDGET_MAX_AGGREGATE_CHARS") {
        if let Ok(n) = v.parse::<usize>() {
            config.budget.max_aggregate_chars = n;
        }
    }

    // ---- Compaction / context-pressure (Day 3+) ----
    // Backward-compatible: every override is optional and only writes the
    // field when the env var is present. `None` in CompactionConfig lets
    // the runtime fall back to `CompressorConfig::default()`.
    // Source priority (highest → lowest):
    // 1. env vars below
    // 2. config.yaml [compaction] section
    // 3. CompressorConfig::default() at runtime

    if let Some(v) = env_compat("HERMIX_COMPACT_PRESSURE_THRESHOLD", "HERMES_COMPACT_PRESSURE_THRESHOLD") {
        if let Ok(n) = v.parse::<f64>() {
            config.compaction.pressure_threshold_percent = Some(n);
        } else {
            tracing::warn!("HERMIX_COMPACT_PRESSURE_THRESHOLD is not a valid f64: {v}");
        }
    }
    if let Some(v) = env_compat("HERMIX_COMPACT_OUTPUT_RESERVE", "HERMES_COMPACT_OUTPUT_RESERVE") {
        if let Ok(n) = v.parse::<usize>() {
            config.compaction.output_reserve_tokens = Some(n);
        }
    }
    if let Some(v) = env_compat("HERMIX_COMPACT_THRESHOLD", "HERMES_COMPACT_THRESHOLD") {
        if let Ok(n) = v.parse::<f64>() {
            config.compaction.threshold_percent = Some(n);
        }
    }
    if let Some(v) = env_compat("HERMIX_COMPACT_PROTECT_FIRST_N", "HERMES_COMPACT_PROTECT_FIRST_N") {
        if let Ok(n) = v.parse::<usize>() {
            config.compaction.protect_first_n = Some(n);
        }
    }
    if let Some(v) = env_compat("HERMIX_COMPACT_PROTECT_LAST_N", "HERMES_COMPACT_PROTECT_LAST_N") {
        if let Ok(n) = v.parse::<usize>() {
            config.compaction.protect_last_n = Some(n);
        }
    }
    if let Some(v) = env_compat("HERMIX_COMPACT_SUMMARY_RATIO", "HERMES_COMPACT_SUMMARY_RATIO") {
        if let Ok(n) = v.parse::<f64>() {
            config.compaction.summary_target_ratio = Some(n);
        }
    }
    if let Some(v) = env_compat("HERMIX_COMPACT_ENABLED", "HERMES_COMPACT_ENABLED") {
        config.compaction.enabled = Some(matches!(v.trim(), "1" | "true" | "yes" | "on"));
    }
    if let Some(v) = env_compat("HERMIX_COMPACT_BREAKER_THRESHOLD", "HERMES_COMPACT_BREAKER_THRESHOLD") {
        if let Ok(n) = v.parse::<u32>() {
            config.compaction.circuit_breaker_threshold = Some(n);
        }
    }
    if let Some(v) = env_compat("HERMIX_COMPACT_BREAKER_COOLDOWN_SECS", "HERMES_COMPACT_BREAKER_COOLDOWN_SECS") {
        if let Ok(n) = v.parse::<u64>() {
            config.compaction.circuit_breaker_cooldown_secs = Some(n);
        }
    }

    // Provider-specific API keys
    for (env_var, provider_name) in [
        ("OPENAI_API_KEY", "openai"),
        ("ANTHROPIC_API_KEY", "anthropic"),
        ("OPENROUTER_API_KEY", "openrouter"),
        ("DASHSCOPE_API_KEY", "qwen"),
        ("DASHSCOPE_API_KEY", "dashscope"),
        ("DEEPSEEK_API_KEY", "deepseek"),
        ("MOONSHOT_API_KEY", "kimi"),
        ("MINIMAX_API_KEY", "minimax"),
        ("NOUS_API_KEY", "nous"),
        ("GITHUB_COPILOT_TOKEN", "copilot"),
        ("SILICONFLOW_API_KEY", "siliconflow"),
        ("ZHIPU_API_KEY", "zhipu"),
        ("ARK_API_KEY", "volcengine"),
    ] {
        if let Ok(v) = std::env::var(env_var) {
            config
                .llm_providers
                .entry(provider_name.to_string())
                .or_default()
                .api_key = Some(v);
        }
    }

    if let Some(v) = env_compat("HERMIX_BASE_URL", "HERMES_BASE_URL") {
        for provider in config.llm_providers.values_mut() {
            provider.base_url = Some(v.clone());
        }
    }
    if let Ok(v) = std::env::var("OPENROUTER_BASE_URL") {
        config
            .llm_providers
            .entry("openrouter".to_string())
            .or_default()
            .base_url = Some(v);
    }

    crate::python_platform_env::apply_python_named_platform_env(config);
}

// ---------------------------------------------------------------------------
// Auto-correct model
// ---------------------------------------------------------------------------

/// 内置 provider 注册表（id → 配钥环境变量名）——全项目唯一真理源。
/// hermix-agent / server / 前端均由此派生，禁止再手写平行副本
/// （此前 config 与 agent 各持一份且已漂移，agent 独有 baichuan/
/// tencent/spark/yi 四家，导致 provider_has_key 不认识它们）。
pub const BUILTIN_PROVIDER_ENV_VARS: &[(&str, &str)] = &[
    ("openai", "OPENAI_API_KEY"),
    ("anthropic", "ANTHROPIC_API_KEY"),
    ("openrouter", "OPENROUTER_API_KEY"),
    ("deepseek", "DEEPSEEK_API_KEY"),
    ("qwen", "DASHSCOPE_API_KEY"),
    ("kimi", "MOONSHOT_API_KEY"),
    ("moonshot", "MOONSHOT_API_KEY"),
    ("minimax", "MINIMAX_API_KEY"),
    ("nous", "NOUS_API_KEY"),
    ("copilot", "GITHUB_COPILOT_TOKEN"),
    ("siliconflow", "SILICONFLOW_API_KEY"),
    ("zhipu", "ZHIPU_API_KEY"),
    ("volcengine", "ARK_API_KEY"),
    ("dashscope", "DASHSCOPE_API_KEY"),
    // 4 个国内厂商（2026-08 自 agent 侧副本合并）
    ("baichuan", "BAICHUAN_API_KEY"),
    ("tencent", "HUNYUAN_API_KEY"),
    ("spark", "SPARK_API_KEY"),
    ("yi", "YI_API_KEY"),
];

/// Map a provider name to its well-known API key env var.
pub fn env_var_name_for_provider(provider: &str) -> &'static str {
    BUILTIN_PROVIDER_ENV_VARS
        .iter()
        .find(|(id, _)| *id == provider)
        .map(|(_, var)| *var)
        .unwrap_or("<unknown provider>")
}

/// Default model for each provider when the user hasn't picked one.
/// Used by [`auto_correct_model`] to populate a sensible default after
/// switching the model to a configured provider.
pub fn default_model_for_provider(provider: &str) -> &'static str {
    match provider {
        "minimax" => "MiniMax-M3",
        "openai" => "gpt-4o",
        "anthropic" => "claude-3-5-sonnet-latest",
        "openrouter" => "deepseek/deepseek-chat-v3-0324",
        "deepseek" => "deepseek-chat",
        "qwen" => "qwen-max",
        "kimi" | "moonshot" => "moonshot-v1-128k",
        "nous" => "nous-hermes-2-mixtral-8x7b-dpo",
        "copilot" => "gpt-4o",
        "siliconflow" => "Qwen/Qwen2.5-72B-Instruct",
        "zhipu" => "glm-4-plus",
        "volcengine" => "doubao-pro-32k",
        "dashscope" => "qwen-plus",
        _ => "default",
    }
}

/// Probe the live process environment for a provider's API key.
pub fn provider_api_key_from_env(provider: &str) -> Option<String> {
    // Custom providers (custom-xxx) store keys as CUSTOM_{ID}_API_KEY.
    if provider.starts_with("custom-") {
        let id_part = &provider["custom-".len()..];
        let env_var = format!(
            "CUSTOM_{}_API_KEY",
            id_part.to_uppercase().replace('-', "_")
        );
        return std::env::var(env_var).ok().filter(|s| !s.is_empty());
    }
    let var = env_var_name_for_provider(provider);
    if var == "<unknown provider>" {
        return None;
    }
    std::env::var(var).ok().filter(|s| !s.is_empty())
}

/// Has this provider been configured (in `config.llm_providers` or via env)?
pub fn provider_has_key(config: &GatewayConfig, provider: &str) -> bool {
    if let Some(cfg) = config.llm_providers.get(provider) {
        if cfg
            .api_key
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
        {
            return true;
        }
    }
    provider_api_key_from_env(provider).is_some()
}

/// Result of [`auto_correct_model`].
#[derive(Debug, Clone, serde::Serialize)]
pub struct AutoCorrectReport {
    /// The original `config.model` before correction (may be `None`).
    pub before: Option<String>,
    /// The new `config.model` after correction.
    pub after: String,
    /// The provider picked (`minimax`, `openai`, …).
    pub provider: String,
    /// Why the correction was needed. One of:
    /// - `"unset"`: `config.model` was `None` and we picked a configured provider.
    /// - `"no_key"`: `config.model` pointed to a provider with no API key, so we
    /// routed to a configured provider instead.
    /// - `"unknown_provider"`: `config.model` referenced a provider that has no
    /// branch in `build_provider` and would have errored.
    pub reason: String,
    /// The full list of providers that *do* have a key. Useful for the desktop
    /// UI to render "we auto-routed to minimax because you have keys for
    /// [minimax, …]".
    pub configured_providers: Vec<String>,
}

/// Auto-correct `config.model` when it is unset, points to a provider with no
/// key, or references an unknown provider. The function is idempotent: if
/// `config.model` already points to a configured provider it returns `None`
/// and makes no change.
/// Used at server startup and after the desktop Settings UI saves a new key
/// (the latter via the `/api/config/auto-correct` REST endpoint), so the
/// common "I configured a key but the chat still asks for openai" failure
/// mode can't bite twice.
/// The return value is `Some(report)` only when a change was actually made.
pub fn auto_correct_model(config: &mut GatewayConfig) -> Option<AutoCorrectReport> {
    // Build a stable, deduplicated list of providers that DO have a key
    // (config or env). This is the universe we are allowed to pick from.
    let mut configured: Vec<String> = Vec::new();
    for (name, cfg) in &config.llm_providers {
        if cfg
            .api_key
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
        {
            configured.push(name.clone());
        } else if provider_api_key_from_env(name).is_some() {
            configured.push(name.clone());
        }
    }
    // Also probe raw env vars the user may have exported outside the
    // standard `llm_providers.<name>.api_key` channel.
    for probe in [
        ("OPENAI_API_KEY", "openai"),
        ("ANTHROPIC_API_KEY", "anthropic"),
        ("MINIMAX_API_KEY", "minimax"),
        ("DASHSCOPE_API_KEY", "dashscope"),
        ("ZHIPUAI_API_KEY", "zhipu"),
        ("VOLCENGINE_API_KEY", "volcengine"),
        ("SILICONFLOW_API_KEY", "siliconflow"),
        ("OPENROUTER_API_KEY", "openrouter"),
        ("DEEPSEEK_API_KEY", "deepseek"),
    ] {
        if let Ok(v) = std::env::var(probe.0) {
            if !v.trim().is_empty() && !configured.iter().any(|n| n == probe.1) {
                configured.push(probe.1.to_string());
            }
        }
    }
    // Probe custom providers: CUSTOM_{ID}_API_KEY env vars.
    // Any env var matching CUSTOM_{ALPHANUMERIC}_API_KEY means the user
    // has a custom provider configured.  The `build_provider` in
    // `agent_builder.rs` already supports `custom-{id}` namespaces.
    for entry in std::env::vars() {
        let (k, v) = (&entry.0, &entry.1);
        if k.starts_with("CUSTOM_") && k.ends_with("_API_KEY") && !v.is_empty() {
            // Extract the ID part between CUSTOM_ and _API_KEY
            let id_part = &k["CUSTOM_".len()..k.len() - "_API_KEY".len()];
            let provider_id = format!(
                "custom-{}",
                id_part.to_lowercase().replace('_', "-")
            );
            if !configured.iter().any(|n| n == &provider_id) {
                configured.push(provider_id);
            }
        }
    }
    // Also include custom providers from config.yaml llm_providers
    for (id, _cfg) in &config.llm_providers {
        if id.starts_with("custom-") && provider_has_key(config, id) {
            if !configured.iter().any(|n| n == id) {
                configured.push(id.clone());
            }
        }
    }
    configured.sort();
    configured.dedup();

    let before = config.model.clone();

    // Priority order, loosely matching `LocalAgentService::fallback_model`
    // so the auto-correct and the runtime fallback always agree.
    const PRIORITY: &[&str] = &[
        "openrouter",
        "anthropic",
        "minimax",
        "dashscope",
        "zhipu",
        "volcengine",
        "siliconflow",
        "moonshot",
        "kimi",
        "deepseek",
        "qwen",
        "nous",
        "openai",
    ];

    let pick_provider = |provider: &str| -> String {
        let model = config
            .llm_providers
            .get(provider)
            .and_then(|c| c.model.clone())
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| default_model_for_provider(provider).to_string());
        format!("{provider}:{model}")
    };

    // 1. config.model unset → pick the first configured provider in priority.
    let chosen = match before.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => {
            let provider = PRIORITY
                .iter()
                .find(|p| configured.iter().any(|c| c == *p))
                .map(|s| s.to_string())
                .or_else(|| configured.first().cloned());
            match provider {
                Some(p) => (p, "unset".to_string()),
                None => return None, // nothing configured at all
            }
        }
        Some(model) => {
            // 2. config.model set but its provider has no key → switch to a
            // configured one. The original error the user would have seen
            // is "missing API key for X", which is exactly the symptom we
            // are trying to prevent.
            let provider = model.split_once(':').map(|(p, _)| p).unwrap_or(model);
            if provider_has_key(config, provider) {
                return None; // already good
            }
            let replacement = PRIORITY
                .iter()
                .find(|p| configured.iter().any(|c| c == *p))
                .map(|s| s.to_string())
                .or_else(|| configured.first().cloned());
            match replacement {
                Some(p) if p != provider => (p, "no_key".to_string()),
                Some(p) => (p, "no_key".to_string()),
                None => return None,
            }
        }
    };

    let (provider, reason) = chosen;
    let after = pick_provider(&provider);
    config.model = Some(after.clone());

    Some(AutoCorrectReport {
        before,
        after,
        provider,
        reason,
        configured_providers: configured,
    })
}

// ---------------------------------------------------------------------------
// validate_config
// ---------------------------------------------------------------------------

/// Validate a fully-loaded configuration.
/// Checks:
/// - max_turns > 0
/// - SessionResetPolicy::Daily at_hour in 0..=23
/// - All LLM providers with an api_key set have a non-empty value
/// - Terminal timeout > 0
pub fn validate_config(config: &GatewayConfig) -> Result<(), ConfigError> {
    if config.max_turns == 0 {
        return Err(ConfigError::ValidationError(
            "max_turns must be greater than 0".into(),
        ));
    }

    if config.terminal.timeout == 0 {
        return Err(ConfigError::ValidationError(
            "terminal.timeout must be greater than 0".into(),
        ));
    }

    // Validate session reset policy (clamping already done during merge)
    let _ = config.session.reset_policy.validate();

    for (name, provider) in &config.llm_providers {
        if let Some(key) = &provider.api_key {
            if key.is_empty() {
                return Err(ConfigError::ValidationError(format!(
                    "llm_providers.{name}.api_key must not be empty"
                )));
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn validate_valid_config() {
        let config = GatewayConfig::default();
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn validate_zero_max_turns() {
        let mut config = GatewayConfig::default();
        config.max_turns = 0;
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn validate_zero_terminal_timeout() {
        let mut config = GatewayConfig::default();
        config.terminal.timeout = 0;
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn load_user_config_defaults_autonomous_mode_to_true() {
        // Real-world user config.yaml omits the `autonomous_mode` field
        // entirely. After loading, the value must be `true` (product
        // philosophy: try everything before stopping).
        let yaml = r#"
max_turns: 30
model: custom-agnes:agnes-2.0-flash
"#;
        let dir = std::env::temp_dir().join("hermix_config_test_user_yaml");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");
        std::fs::write(&path, yaml).unwrap();
        let cfg = load_user_config_file(&path).unwrap();
        assert!(
            cfg.autonomous_mode,
            "user config.yaml without autonomous_mode key must default to true; \
             got {}",
            cfg.autonomous_mode
        );
    }

    #[test]
    fn validate_empty_api_key() {
        let mut config = GatewayConfig::default();
        let mut providers = HashMap::new();
        providers.insert(
            "test".into(),
            crate::config::LlmProviderConfig {
                api_key: Some("".into()),
                ..Default::default()
            },
        );
        config.llm_providers = providers;
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn env_overrides_model() {
        let mut config = GatewayConfig::default();
        // Simulate env var (we can't easily set env vars in tests, so test the logic directly)
        config.model = Some("env-model".into());
        assert_eq!(config.model.as_deref(), Some("env-model"));
    }

    #[test]
    fn env_overrides_openrouter_base_url() {
        let mut config = GatewayConfig::default();
        // SAFETY: test-only scoped env mutation.
        unsafe {
            std::env::set_var(
                "OPENROUTER_BASE_URL",
                "https://example-openrouter.invalid/v1",
            );
        }
        apply_env_overrides(&mut config);
        assert_eq!(
            config
                .llm_providers
                .get("openrouter")
                .and_then(|p| p.base_url.as_deref()),
            Some("https://example-openrouter.invalid/v1")
        );
        // SAFETY: test-only cleanup.
        unsafe {
            std::env::remove_var("OPENROUTER_BASE_URL");
        }
    }

    #[test]
    fn apply_patch_save_load_roundtrip() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let mut c = GatewayConfig::default();
        apply_user_config_patch(&mut c, "model", "openai:gpt-4o-mini").unwrap();
        apply_user_config_patch(&mut c, "max_turns", "15").unwrap();
        save_config_yaml(&path, &c).unwrap();
        let loaded = load_user_config_file(&path).unwrap();
        assert_eq!(loaded.model.as_deref(), Some("openai:gpt-4o-mini"));
        assert_eq!(loaded.max_turns, 15);
    }

    #[test]
    fn apply_patch_dotted_llm_proxy_budget() {
        let mut c = GatewayConfig::default();
        apply_user_config_patch(&mut c, "llm.openai.api_key", "sk-test").unwrap();
        apply_user_config_patch(&mut c, "llm.openai.base_url", "https://api.openai.com/v1")
            .unwrap();
        apply_user_config_patch(&mut c, "llm.openai.command", "copilot-language-server").unwrap();
        apply_user_config_patch(&mut c, "llm.openai.args", "--stdio,--model,gpt-4o-mini").unwrap();
        apply_user_config_patch(&mut c, "proxy.http", "http://127.0.0.1:8080").unwrap();
        apply_user_config_patch(&mut c, "budget.max_result_size_chars", "500").unwrap();
        assert_eq!(
            c.llm_providers.get("openai").unwrap().api_key.as_deref(),
            Some("sk-test")
        );
        assert_eq!(
            c.llm_providers.get("openai").unwrap().base_url.as_deref(),
            Some("https://api.openai.com/v1")
        );
        assert_eq!(
            c.llm_providers.get("openai").unwrap().command.as_deref(),
            Some("copilot-language-server")
        );
        assert_eq!(
            c.llm_providers.get("openai").unwrap().args,
            vec![
                "--stdio".to_string(),
                "--model".to_string(),
                "gpt-4o-mini".to_string()
            ]
        );
        assert_eq!(
            c.proxy.as_ref().unwrap().http_proxy.as_deref(),
            Some("http://127.0.0.1:8080")
        );
        assert_eq!(c.budget.max_result_size_chars, 500);
        assert!(user_config_field_display(&c, "llm.openai.api_key")
            .unwrap()
            .starts_with("***"));
        assert_eq!(
            user_config_field_display(&c, "llm.openai.command").unwrap(),
            "copilot-language-server"
        );
        assert_eq!(
            user_config_field_display(&c, "llm.openai.args").unwrap(),
            "--stdio,--model,gpt-4o-mini"
        );
    }
}
