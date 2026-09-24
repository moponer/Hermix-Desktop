//! Environment variable (API key) management endpoints.

use axum::extract::State;
use axum::Json;
use std::collections::HashMap;

use super::types::*;
use crate::{HttpError, HttpServerState};

/// Known environment variables with metadata.
struct EnvVarMeta {
    description: &'static str,
    url: Option<&'static str>,
    category: &'static str,
    is_password: bool,
    tools: &'static [&'static str],
    advanced: bool,
}

fn known_env_vars() -> Vec<(&'static str, EnvVarMeta)> {
    vec![
        (
            "OPENAI_API_KEY",
            EnvVarMeta {
                description: "OpenAI API key for GPT models",
                url: Some("https://platform.openai.com/api-keys"),
                category: "provider",
                is_password: true,
                tools: &[],
                advanced: false,
            },
        ),
        (
            "ANTHROPIC_API_KEY",
            EnvVarMeta {
                description: "Anthropic API key for Claude models",
                url: Some("https://console.anthropic.com/settings/keys"),
                category: "provider",
                is_password: true,
                tools: &[],
                advanced: false,
            },
        ),
        (
            "OPENROUTER_API_KEY",
            EnvVarMeta {
                description: "OpenRouter API key for multi-model routing",
                url: Some("https://openrouter.ai/keys"),
                category: "provider",
                is_password: true,
                tools: &[],
                advanced: false,
            },
        ),
        (
            "DEEPSEEK_API_KEY",
            EnvVarMeta {
                description: "DeepSeek API key",
                url: Some("https://platform.deepseek.com/api_keys"),
                category: "provider",
                is_password: true,
                tools: &[],
                advanced: false,
            },
        ),
        (
            "DASHSCOPE_API_KEY",
            EnvVarMeta {
                description: "DashScope (Qwen) API key",
                url: Some("https://dashscope.console.aliyun.com/apiKey"),
                category: "provider",
                is_password: true,
                tools: &[],
                advanced: false,
            },
        ),
        (
            "KIMI_API_KEY",
            EnvVarMeta {
                description: "Kimi / Moonshot API key",
                url: Some("https://platform.moonshot.cn/console/api-keys"),
                category: "provider",
                is_password: true,
                tools: &[],
                advanced: false,
            },
        ),
        (
            "MINIMAX_API_KEY",
            EnvVarMeta {
                description: "MiniMax API key",
                url: Some(
                    "https://platform.minimaxi.com/user-center/basic-information/interface-key",
                ),
                category: "provider",
                is_password: true,
                tools: &[],
                advanced: false,
            },
        ),
        (
            "EXA_API_KEY",
            EnvVarMeta {
                description: "Exa search API key",
                url: Some("https://exa.ai"),
                category: "tool",
                is_password: true,
                tools: &["web_search"],
                advanced: false,
            },
        ),
        (
            "FIRECRAWL_API_KEY",
            EnvVarMeta {
                description: "Firecrawl web scraping API key",
                url: Some("https://firecrawl.dev"),
                category: "tool",
                is_password: true,
                tools: &["web_search", "web_extract"],
                advanced: false,
            },
        ),
        (
            "ELEVENLABS_API_KEY",
            EnvVarMeta {
                description: "ElevenLabs text-to-speech API key",
                url: Some("https://elevenlabs.io"),
                category: "tool",
                is_password: true,
                tools: &["tts"],
                advanced: true,
            },
        ),
    ]
}

fn redact(value: &str) -> String {
    if value.len() <= 8 {
        "****".to_string()
    } else {
        format!(
            "{}...{}",
            &value[..value.floor_char_boundary(4)],
            &value[value.floor_char_boundary(value.len().saturating_sub(4))..]
        )
    }
}

pub(crate) fn read_env_file(hermix_home: &std::path::Path) -> HashMap<String, String> {
    let env_path = hermix_home.join(".env");
    let mut map = HashMap::new();
    if let Ok(content) = std::fs::read_to_string(&env_path) {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('#') || !line.contains('=') {
                continue;
            }
            if let Some((key, val)) = line.split_once('=') {
                let val = val.trim().trim_matches('"').trim_matches('\'');
                if !val.is_empty() {
                    map.insert(key.trim().to_string(), val.to_string());
                }
            }
        }
    }
    map
}

pub(crate) fn write_env_file(hermix_home: &std::path::Path, vars: &HashMap<String, String>) {
    let env_path = hermix_home.join(".env");
    let mut lines = Vec::new();
    for (key, val) in vars {
        lines.push(format!("{}=\"{}\"", key, val));
    }
    lines.sort();
    let _ = std::fs::write(env_path, lines.join("\n") + "\n");
}

/// GET /api/env
pub async fn get_env_vars(
    State(state): State<HttpServerState>,
) -> Json<HashMap<String, EnvVarInfo>> {
    let file_vars = read_env_file(&state.hermix_home);
    let mut result = HashMap::new();

    for (key, meta) in known_env_vars() {
        let env_val = std::env::var(key)
            .ok()
            .or_else(|| file_vars.get(key).cloned());
        let is_set = env_val.is_some();
        let redacted_value = env_val.as_deref().map(redact);

        result.insert(
            key.to_string(),
            EnvVarInfo {
                is_set,
                redacted_value,
                description: meta.description.to_string(),
                url: meta.url.map(|s| s.to_string()),
                category: meta.category.to_string(),
                is_password: meta.is_password,
                tools: meta.tools.iter().map(|s| s.to_string()).collect(),
                advanced: meta.advanced,
            },
        );
    }

    // ── Custom provider keys ─────────────────────────────
    // Scan for CUSTOM_*_API_KEY entries in both the live process
    // environment and the .env file.  Built-in providers are handled
    // above via `known_env_vars()`; custom providers are dynamic.
    for (key, val) in &file_vars {
        if !key.starts_with("CUSTOM_") || !key.ends_with("_API_KEY") {
            continue;
        }
        if result.contains_key(key) {
            continue;
        }
        let env_val = std::env::var(key)
            .ok()
            .or_else(|| Some(val.clone()));
        let is_set = env_val.as_ref().map(|v| !v.is_empty()).unwrap_or(false);
        let redacted_value = env_val.as_deref().filter(|v| !v.is_empty()).map(redact);

        let label = key
            .strip_prefix("CUSTOM_")
            .and_then(|s| s.strip_suffix("_API_KEY"))
            .unwrap_or("unknown");
        let description = format!("自定义 Provider「{}」的 API Key", label);

        result.insert(
            key.clone(),
            EnvVarInfo {
                is_set,
                redacted_value,
                description,
                url: None,
                category: "provider".to_string(),
                is_password: true,
                tools: vec![],
                advanced: false,
            },
        );
    }

    // Also check config.yaml for custom providers whose api_key
    // lives in llm_providers.<id>.api_key rather than .env.
    // Use state.hermix_home so we read the same config.yaml that
    // the rest of the app uses (important when running as Tauri sidecar
    // with HERMIX_PORTABLE_DATA_DIR set).
    let hermix_home_str = state.hermix_home.to_string_lossy().to_string();
    if let Ok(cfg) = hermix_config::loader::load_config(Some(hermix_home_str.as_str())) {
        for (id, prov) in &cfg.llm_providers {
            if !id.starts_with("custom-") {
                continue;
            }
            let has_key = prov.api_key.as_ref().map(|k| !k.trim().is_empty()).unwrap_or(false);
            if has_key {
                let ev_name = format!("CUSTOM_{}_API_KEY", id.replace("-", "_").to_uppercase());
                if !result.contains_key(&ev_name) {
                    let ak = prov.api_key.as_ref().unwrap();
                    result.insert(ev_name.clone(), EnvVarInfo {
                        is_set: true,
                        redacted_value: Some(redact(ak)),
                        description: format!("自定义 Provider「{}」的 API Key", id),
                        url: None,
                        category: "provider".to_string(),
                        is_password: true,
                        tools: vec![],
                        advanced: false,
                    });
                }
            }
        }
    }

    Json(result)
}

/// PUT /api/env
pub async fn set_env_var(
    State(state): State<HttpServerState>,
    axum::extract::Extension(origin): axum::extract::Extension<hermix_core::Origin>,
    axum::extract::Extension(device_scopes): axum::extract::Extension<
        Option<std::collections::HashSet<hermix_core::origin::Capability>>,
    >,
    Json(body): Json<EnvVarUpdate>,
) -> Result<Json<OkResponse>, HttpError> {
    // §5.5 第 5 层：端点级能力位关口（ConfigMutation）。PairedDevice 来源
    // 未配该能力位 → 403（矩阵已 Denied，端点层再拦是纵深）；Local 来源放行。
    crate::routes::check_capability_from_extensions(
        hermix_core::origin::Capability::ConfigMutation,
        &origin,
        &device_scopes,
    )
    .map_err(|(status, message)| HttpError { status, message })?;
    let mut vars = read_env_file(&state.hermix_home);
    vars.insert(body.key.clone(), body.value.clone());
    write_env_file(&state.hermix_home, &vars);
    // Also set in current process
    std::env::set_var(&body.key, &body.value);
    Ok(Json(OkResponse { ok: true }))
}

/// DELETE /api/env
pub async fn delete_env_var(
    State(state): State<HttpServerState>,
    axum::extract::Extension(origin): axum::extract::Extension<hermix_core::Origin>,
    axum::extract::Extension(device_scopes): axum::extract::Extension<
        Option<std::collections::HashSet<hermix_core::origin::Capability>>,
    >,
    Json(body): Json<EnvVarDelete>,
) -> Result<Json<OkResponse>, HttpError> {
    // §5.5 第 5 层：端点级能力位关口（ConfigMutation）。PairedDevice 来源
    // 未配该能力位 → 403；Local 来源放行。
    crate::routes::check_capability_from_extensions(
        hermix_core::origin::Capability::ConfigMutation,
        &origin,
        &device_scopes,
    )
    .map_err(|(status, message)| HttpError { status, message })?;
    let mut vars = read_env_file(&state.hermix_home);
    vars.remove(&body.key);
    write_env_file(&state.hermix_home, &vars);
    std::env::remove_var(&body.key);
    Ok(Json(OkResponse { ok: true }))
}

/// POST /api/env/reveal
pub async fn reveal_env_var(
    State(state): State<HttpServerState>,
    Json(body): Json<EnvVarReveal>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let file_vars = read_env_file(&state.hermix_home);
    let value = std::env::var(&body.key)
        .ok()
        .or_else(|| file_vars.get(&body.key).cloned())
        .unwrap_or_default();

    Ok(Json(serde_json::json!({
        "key": body.key,
        "value": value,
    })))
}
