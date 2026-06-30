//! Custom LLM provider management.
//!
//! Allows the user to create, list, edit, and delete custom OpenAI-compatible
//! LLM providers.  Stored in `config.yaml` under `llm_providers` using the
//! `custom-{id}` namespace.  The agent_builder already knows to use
//! `CUSTOM_{ID}_API_KEY` + `CUSTOM_{ID}_BASE_URL` env vars.

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::{HttpError, HttpServerState};

use super::env;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Request body for creating / updating a custom provider.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateCustomProviderRequest {
    /// The provider ID: `custom-{id}`. Must match regex `^custom-[a-z0-9-]+$`.
    pub id: String,
    /// Display name (for UI only).
    pub display_name: Option<String>,
    /// OpenAI-compatible API base URL.
    pub base_url: String,
    /// API key.
    pub api_key: String,
    /// Comma-separated model names to use with this provider.
    pub models: Option<String>,
}

/// Response shape for a custom provider listing.
#[derive(Debug, Clone, Serialize)]
pub struct CustomProviderSummary {
    pub id: String,
    pub display_name: Option<String>,
    pub base_url: String,
    /// `true` if an api_key is configured.
    pub has_key: bool,
    /// Parsed model list from the `model` field.
    pub models: Vec<String>,
    /// Provider-wide context window override (tokens), if configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// Per-model context window overrides (tokens) discovered via
    /// `fetch-models` or set by hand. Mirrors `LlmProviderConfig.model_contexts`.
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub model_contexts: HashMap<String, u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OkResponse {
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FetchModelsResponse {
    /// Raw model list from the provider's /models endpoint.
    pub models: Vec<RawModelEntry>,
    /// Error message if the fetch failed.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct RawModelEntry {
    pub id: String,
    /// Optional context window length (tokens) reported by the upstream
    /// `/models` endpoint. Many OpenAI-compatible providers expose this
    /// under one of several field names; we accept all of them via
    /// serde aliases. `None` means the upstream didn't report a context
    /// window for this model — `LlmProviderConfig.model_contexts[id]` will
    /// simply not be populated for it.
    #[serde(
        default,
        alias = "context_length",
        alias = "max_context_length",
        alias = "context_window",
        alias = "context_size",
        alias = "max_input_tokens",
        alias = "max_model_len",
        skip_serializing_if = "Option::is_none"
    )]
    pub context_window: Option<u64>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct ModelsApiResponse {
    data: Vec<RawModelEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TestConnectionResponse {
    pub ok: bool,
    pub status: u16,
    pub error: Option<String>,
    pub models: Option<Vec<RawModelEntry>>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn read_gateway_config(
    state: &HttpServerState,
) -> Result<hermix_config::GatewayConfig, HttpError> {
    hermix_config::load_config(Some(state.hermix_home.to_str().unwrap_or_default())).map_err(|_| {
        HttpError {
            status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            message: "failed to load gateway config".to_string(),
        }
    })
}

// ---------------------------------------------------------------------------
// GET /api/custom-providers
// ---------------------------------------------------------------------------

pub async fn list_custom_providers(
    State(state): State<HttpServerState>,
) -> Result<Json<Vec<CustomProviderSummary>>, HttpError> {
    let config = read_gateway_config(&state)?;

    let results: Vec<CustomProviderSummary> = config
        .llm_providers
        .iter()
        .filter(|(id, _)| id.starts_with("custom-"))
        .map(|(id, cfg)| {
            let models: Vec<String> = cfg
                .model
                .as_ref()
                .map(|m| {
                    m.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            CustomProviderSummary {
                id: id.clone(),
                display_name: None,
                base_url: cfg.base_url.as_deref().unwrap_or("").to_string(),
                has_key: cfg
                    .api_key
                    .as_ref()
                    .map(|k| !k.trim().is_empty())
                    .unwrap_or(false),
                models,
                context_window: cfg.context_window,
                model_contexts: cfg.model_contexts.clone(),
            }
        })
        .collect();

    Ok(Json(results))
}

// ---------------------------------------------------------------------------
// POST /api/custom-providers
// ---------------------------------------------------------------------------

pub async fn create_custom_provider(
    State(state): State<HttpServerState>,
    Json(req): Json<CreateCustomProviderRequest>,
) -> Result<Json<OkResponse>, HttpError> {
    // Validate ID
    if !req.id.starts_with("custom-") || req.id.len() < 8 {
        return Err(HttpError {
            status: axum::http::StatusCode::BAD_REQUEST,
            message: "Provider ID must start with 'custom-' and be at least 8 characters".to_string(),
        });
    }

    // Read config
    let config_path = state.hermix_home.join("config.yaml");
    let mut config = read_gateway_config(&state)?;

    // Insert or update provider
    let entry = config
        .llm_providers
        .entry(req.id.clone())
        .or_insert_with(hermix_config::LlmProviderConfig::default);

    if !req.base_url.is_empty() {
        entry.base_url = Some(req.base_url.trim_end_matches('/').to_string());
    }
    if !req.api_key.is_empty() {
        entry.api_key = Some(req.api_key);
    }
    if let Some(ref models) = req.models {
        if !models.trim().is_empty() {
            entry.model = Some(models.clone());
        }
    }

    // Sync env vars for agent_builder to pick up immediately
    let env_id = req.id.trim_start_matches("custom-").to_uppercase();
    if let Some(ref key) = entry.api_key {
        std::env::set_var(format!("CUSTOM_{}_API_KEY", env_id), key);
    }
    if let Some(ref url) = entry.base_url {
        std::env::set_var(format!("CUSTOM_{}_BASE_URL", env_id), url);
    }

    // Persist to .env so CUSTOM_{ID}_* vars survive restart.
    {
        let mut env_vars = env::read_env_file(&state.hermix_home);
        if let Some(ref key) = entry.api_key {
            env_vars.insert(format!("CUSTOM_{}_API_KEY", env_id), key.clone());
        }
        if let Some(ref url) = entry.base_url {
            env_vars.insert(format!("CUSTOM_{}_BASE_URL", env_id), url.clone());
        }
        env::write_env_file(&state.hermix_home, &env_vars);
    }

    // Persist to config.yaml
    let mut to_save = config.clone();
    to_save.home_dir = None;
    let yaml = serde_yaml::to_string(&to_save).map_err(|e| HttpError {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("failed to serialize config: {}", e),
    })?;
    std::fs::write(&config_path, yaml).map_err(|e| HttpError {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("failed to write config: {}", e),
    })?;

    // Push to live agent service
    let cfg_value = serde_json::to_value(&config).map_err(|e| HttpError {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("failed to serialize config: {}", e),
    })?;
    state.agent_service.set_config(cfg_value);

    // Run auto-correct so the model selector picks up the new provider
    let _ = hermix_config::auto_correct_model(&mut config);

    tracing::info!(provider = %req.id, "custom provider created");

    Ok(Json(OkResponse { ok: true }))
}

// ---------------------------------------------------------------------------
// DELETE /api/custom-providers/{id}
// ---------------------------------------------------------------------------

pub async fn delete_custom_provider(
    State(state): State<HttpServerState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<OkResponse>, HttpError> {
    if !id.starts_with("custom-") {
        return Err(HttpError {
            status: axum::http::StatusCode::BAD_REQUEST,
            message: "Provider ID must start with 'custom-'".to_string(),
        });
    }

    let config_path = state.hermix_home.join("config.yaml");
    let mut config = read_gateway_config(&state)?;

    config.llm_providers.remove(&id);

    // Unset env vars (runtime)
    let env_id = id.trim_start_matches("custom-").to_uppercase();
    std::env::remove_var(format!("CUSTOM_{}_API_KEY", env_id));
    std::env::remove_var(format!("CUSTOM_{}_BASE_URL", env_id));

    // Unset in .env file too (persistent)
    {
        let mut env_vars = env::read_env_file(&state.hermix_home);
        env_vars.remove(&format!("CUSTOM_{}_API_KEY", env_id));
        env_vars.remove(&format!("CUSTOM_{}_BASE_URL", env_id));
        env::write_env_file(&state.hermix_home, &env_vars);
    }

    // Persist
    let mut to_save = config.clone();
    to_save.home_dir = None;
    let yaml = serde_yaml::to_string(&to_save).map_err(|e| HttpError {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("failed to serialize config: {}", e),
    })?;
    std::fs::write(&config_path, yaml).map_err(|e| HttpError {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("failed to write config: {}", e),
    })?;

    let cfg_value = serde_json::to_value(&config).map_err(|e| HttpError {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("failed to serialize config: {}", e),
    })?;
    state.agent_service.set_config(cfg_value);

    tracing::info!(provider = %id, "custom provider deleted");

    Ok(Json(OkResponse { ok: true }))
}

// ---------------------------------------------------------------------------
// POST /api/custom-providers/{id}/fetch-models
// ---------------------------------------------------------------------------

pub async fn fetch_models(
    State(state): State<HttpServerState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<FetchModelsResponse>, HttpError> {
    let config = read_gateway_config(&state)?;

    let provider = config
        .llm_providers
        .get(&id)
        .ok_or_else(|| HttpError {
            status: axum::http::StatusCode::NOT_FOUND,
            message: format!("provider '{}' not found", id),
        })?;

    let base_url = provider.base_url.as_deref().ok_or_else(|| HttpError {
        status: axum::http::StatusCode::BAD_REQUEST,
        message: "provider has no base_url configured".to_string(),
    })?;

    let api_key = provider.api_key.as_deref().ok_or_else(|| HttpError {
        status: axum::http::StatusCode::BAD_REQUEST,
        message: "provider has no api_key configured".to_string(),
    })?;

    let url = format!("{}/models", base_url.trim_end_matches('/'));

    let client = reqwest::Client::new();
    match client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if resp.status().is_success() {
                let text = resp.text().await.unwrap_or_default();
                match serde_json::from_str::<ModelsApiResponse>(&text) {
                    Ok(data) => {
                        // 2026-06-25 主人反馈：自定义 provider "成功获取 5 个模型" 后
                        // 列表仍为空。根因：fetch_models 拿到列表只返回给前端，从不写
                        // 回 config.yaml 的 `model` 字段 → 下次 GET /api/custom-providers
                        // list_custom_providers 读 cfg.model 还是空 → 前端 customModels
                        // 永远是空 map → "自定义模型 (N)" 区块不渲染。
                        // 修法：拿到 models 后，把 id 对应 provider 的 model 字段写为
                        // 逗号分隔字符串，save_config_yaml 持久化。
                        //
                        // 2026-06-28 增量：同步把每个 model 的 context_window 写回
                        // `LlmProviderConfig.model_contexts`，让
                        // `ContextManager::for_model_with_providers` 能解析出真实
                        // 阈值（避免误用 128k fallback 频繁触发压缩）。
                        if !data.data.is_empty() {
                            let model_csv = data.data.iter().map(|m| m.id.as_str()).collect::<Vec<_>>().join(",");
                            let mut ctx_windows: HashMap<String, u64> = HashMap::new();
                            for m in &data.data {
                                if let Some(cw) = m.context_window {
                                    if cw > 0 {
                                        ctx_windows.insert(m.id.clone(), cw);
                                    }
                                }
                            }
                            let mut cfg = config;
                            let persisted_count = if let Some(entry) = cfg.llm_providers.get_mut(&id) {
                                entry.model = Some(model_csv);
                                // Merge — keep existing entries that weren't
                                // refreshed by this fetch call.
                                for (k, v) in ctx_windows {
                                    entry.model_contexts.insert(k, v);
                                }
                                Some(entry.model_contexts.len())
                            } else {
                                None
                            };
                            if persisted_count.is_some() {
                                let path = state.hermix_home.join("config.yaml");
                                match hermix_config::loader::save_config_yaml(&path, &cfg) {
                                    Ok(_) => eprintln!(
                                        "[custom_providers:fetch_models] persisted {} models ({} with context_window) for '{}'",
                                        data.data.len(),
                                        persisted_count.unwrap_or(0),
                                        id
                                    ),
                                    Err(e) => eprintln!(
                                        "[custom_providers:fetch_models] failed to persist models for '{}': {}",
                                        id, e
                                    ),
                                }
                            }
                        }
                        Ok(Json(FetchModelsResponse {
                            models: data.data,
                            error: None,
                        }))
                    }
                    Err(_) => Ok(Json(FetchModelsResponse {
                        models: vec![],
                        error: Some("Failed to parse models response".to_string()),
                    })),
                }
            } else {
                let err_text = resp.text().await.unwrap_or_default();
                Ok(Json(FetchModelsResponse {
                    models: vec![],
                    error: Some(format!("HTTP {}: {}", status, err_text.chars().take(200).collect::<String>())),
                }))
            }
        }
        Err(e) => Ok(Json(FetchModelsResponse {
            models: vec![],
            error: Some(e.to_string()),
        })),
    }
}

// ---------------------------------------------------------------------------
// POST /api/custom-providers/{id}/test-connection
// ---------------------------------------------------------------------------

pub async fn test_connection(
    State(state): State<HttpServerState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<TestConnectionResponse>, HttpError> {
    let config = read_gateway_config(&state)?;

    let provider = config
        .llm_providers
        .get(&id)
        .ok_or_else(|| HttpError {
            status: axum::http::StatusCode::NOT_FOUND,
            message: format!("provider '{}' not found", id),
        })?;

    let base_url = provider.base_url.as_deref().ok_or_else(|| HttpError {
        status: axum::http::StatusCode::BAD_REQUEST,
        message: "provider has no base_url configured".to_string(),
    })?;

    let api_key = provider.api_key.as_deref().ok_or_else(|| HttpError {
        status: axum::http::StatusCode::BAD_REQUEST,
        message: "provider has no api_key configured".to_string(),
    })?;

    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let client = reqwest::Client::new();

    match client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let ok = resp.status().is_success();
            let models = if ok {
                let text = resp.text().await.unwrap_or_default();
                match serde_json::from_str::<ModelsApiResponse>(&text) {
                    Ok(data) => Some(data.data),
                    Err(_) => None,
                }
            } else {
                None
            };

            Ok(Json(TestConnectionResponse {
                ok,
                status,
                error: if ok {
                    None
                } else {
                    Some(format!("HTTP {}", status))
                },
                models,
            }))
        }
        Err(e) => Ok(Json(TestConnectionResponse {
            ok: false,
            status: 0,
            error: Some(e.to_string()),
            models: None,
        })),
    }
}

// ---------------------------------------------------------------------------
// PUT /api/custom-providers/{id}/model-contexts
// ---------------------------------------------------------------------------

/// Request body for updating per-model context windows.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateModelContextsRequest {
    /// Key = model_id, value = context window in tokens (0 to clear).
    pub model_contexts: HashMap<String, u64>,
}

/// Set per-model context window overrides for a custom provider.
/// Persists to `config.yaml` → `llm_providers.{id}.model_contexts`.
pub async fn update_model_contexts(
    State(state): State<HttpServerState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(req): Json<UpdateModelContextsRequest>,
) -> Result<Json<OkResponse>, HttpError> {
    if !id.starts_with("custom-") {
        return Err(HttpError { status: axum::http::StatusCode::BAD_REQUEST, message: "Provider ID must start with 'custom-'".to_string() });
    }
    let config_path = state.hermix_home.join("config.yaml");
    let mut config = read_gateway_config(&state)?;
    let model_contexts: HashMap<String, u64> = req.model_contexts.into_iter().filter(|(_, v)| *v > 0).collect();
    let count = model_contexts.len();
    config.llm_providers.entry(id.clone()).or_insert_with(hermix_config::LlmProviderConfig::default).model_contexts = model_contexts;
    let mut to_save = config.clone();
    to_save.home_dir = None;
    let yaml = serde_yaml::to_string(&to_save).map_err(|e| HttpError { status: axum::http::StatusCode::INTERNAL_SERVER_ERROR, message: format!("failed to serialize config: {}", e) })?;
    std::fs::write(&config_path, yaml).map_err(|e| HttpError { status: axum::http::StatusCode::INTERNAL_SERVER_ERROR, message: format!("failed to write config: {}", e) })?;
    tracing::info!(provider = %id, count, "model_contexts updated");
    Ok(Json(OkResponse { ok: true }))
}
