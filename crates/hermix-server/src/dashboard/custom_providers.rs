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
    // Read config
    let config_path = state.hermix_home.join("config.yaml");
    let mut config = read_gateway_config(&state)?;

    // 命名规范只约束**新建**：config 里已存在的 id（历史遗留的不规范 id，
    // 如手工写进 config.yaml 或旧版本签发的）视为更新，放行——否则这些
    // 条目永远无法编辑，前端保存必 400。
    if !config.llm_providers.contains_key(&req.id)
        && (!req.id.starts_with("custom-") || req.id.len() < 8)
    {
        return Err(HttpError {
            status: axum::http::StatusCode::BAD_REQUEST,
            message: "Provider ID must start with 'custom-' and be at least 8 characters".to_string(),
        });
    }

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

    // 模型选择联动：若 config.model 指向无钥 provider，自动改指可用者
    // （必须在持久化/推送之前执行，否则修正结果被丢弃成死变异）
    let correction = hermix_config::auto_correct_model(&mut config);

    // Persist to config.yaml（原子写，并发读者不会读到半截文件）
    hermix_config::loader::save_config_yaml(&config_path, &config).map_err(|e| HttpError {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("failed to write config: {}", e),
    })?;

    // Push to live agent service
    let cfg_value = serde_json::to_value(&config).map_err(|e| HttpError {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("failed to serialize config: {}", e),
    })?;
    state.agent_service.set_config(cfg_value);

    match &correction {
        Some(r) => tracing::info!(
            provider = %req.id,
            before = ?r.before,
            after = %r.after,
            reason = %r.reason,
            "custom provider created; default model auto-corrected"
        ),
        None => tracing::info!(provider = %req.id, "custom provider created"),
    }

    Ok(Json(OkResponse { ok: true }))
}

// ---------------------------------------------------------------------------
// DELETE /api/custom-providers/{id}
// ---------------------------------------------------------------------------

pub async fn delete_custom_provider(
    State(state): State<HttpServerState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<OkResponse>, HttpError> {
    let config_path = state.hermix_home.join("config.yaml");
    let mut config = read_gateway_config(&state)?;

    // 命名规范只拦「不在册」的 id；config 里已有的（历史遗留不规范 id）
    // 必须可删，否则遗留条目删不掉。
    if !id.starts_with("custom-") && !config.llm_providers.contains_key(&id) {
        return Err(HttpError {
            status: axum::http::StatusCode::BAD_REQUEST,
            message: "Provider ID must start with 'custom-'".to_string(),
        });
    }

    config.llm_providers.remove(&id);

    // 级联清理：删除 provider 后同步移除 model_selector 中引用该 provider_id
    // 的孤儿条目，否则前端下拉仍可选中已删 provider，发消息才在 build_provider 崩。
    if let Some(sel) = config.model_selector.as_mut() {
        sel.enabled.retain(|e| e.provider_id != id);
        if let Some(d) = &sel.default_model {
            if d.split_once(':').map(|(p, _)| p) == Some(id.as_str()) {
                sel.default_model = None;
            }
        }
    }

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

    // Persist（原子写，并发读者不会读到半截文件）
    hermix_config::loader::save_config_yaml(&config_path, &config).map_err(|e| HttpError {
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
                        //：自定义 provider "成功获取 5 个模型" 后
                        //列表仍为空。fetch_models 拿到列表只返回给前端，从不写
                        // 回 config.yaml 的 `model` 字段 → 下次 GET /api/custom-providers
                        // list_custom_providers 读 cfg.model 还是空 → 前端 customModels
                        // 永远是空 map → "自定义模型 (N)" 区块不渲染。
                        //拿到 models 后，把 id 对应 provider 的 model 字段写为
                        // 逗号分隔字符串，save_config_yaml 持久化。
                        // 增量：同步把每个 model 的 context_window 写回
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
// POST /api/providers/test — 用"还没落盘"的配置试连通
// ---------------------------------------------------------------------------

/// 测试连接的入参：只带连接所需字段，不要求先建 provider。
///
/// 为什么另开一个端点：`test-connection` 走 `/api/custom-providers/{id}`，
/// 必须先落盘才能测；而添加模型弹窗里用户还没点保存，此时"填完就测"是
/// 最自然的动作。让它先写盘再测会留下取消后的垃圾 provider。
#[derive(Debug, Clone, Deserialize)]
pub struct InlineTestRequest {
    /// OpenAI 兼容端点基址，例如 `https://api.deepseek.com/v1`。
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    /// 内置 provider id（如 `deepseek`）。**给了它就改由服务端从环境变量
    /// 取密钥**，`api_key` 忽略。
    ///
    /// 为什么需要这条：内置供应商的凭据只存在于后端进程的环境变量里，
    /// 前端永远读不到。此前模型页的「测试连接」对内置项只能提示"请去编辑
    /// 里用新 Key 测"——等于没测。base_url 不是机密（前端自带映射表），
    /// 密钥则绝不过网：这条分支让"测内置模型"真正可用，同时不把 key 暴露
    /// 给前端。映射唯一真理源是 `hermix_config::env_var_name_for_provider`。
    #[serde(default)]
    pub provider_id: Option<String>,
    /// 可选：只验证这个模型是否在 /models 列表里。
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InlineTestResponse {
    pub ok: bool,
    /// 错误分类：`apikey` / `network` / `quota` / `provider` / `bad_request`。
    /// 前端按这个给中文提示，不再让用户自己对着一串 HTTP 状态码猜。
    pub kind: Option<String>,
    pub status: u16,
    /// 端到端耗时（毫秒），测通时用来显示"延迟 xxx ms"。
    pub latency_ms: u64,
    pub error: Option<String>,
    pub models: Option<Vec<String>>,
    /// 传了 model 时：该模型是否在 /models 列表里。
    pub model_found: Option<bool>,
}

fn classify(status: u16) -> &'static str {
    match status {
        401 | 403 => "apikey",
        402 | 429 => "quota",
        400 | 404 | 422 => "bad_request",
        _ => "provider",
    }
}

pub async fn test_inline(
    Json(req): Json<InlineTestRequest>,
) -> Result<Json<InlineTestResponse>, HttpError> {
    let base = req.base_url.trim_end_matches('/').to_string();

    // 密钥解析：给了 provider_id 就走环境变量（内置供应商路径），
    // 否则用请求里带的（自定义 provider 路径，添加弹窗里用户刚敲的）。
    let api_key: String = match req.provider_id.as_deref() {
        Some(pid) if !pid.trim().is_empty() => {
            match hermix_config::provider_api_key_from_env(pid) {
                Some(k) if !k.trim().is_empty() => k,
                _ => {
                    let var = hermix_config::env_var_name_for_provider(pid);
                    return Ok(Json(InlineTestResponse {
                        ok: false,
                        kind: Some("apikey".into()),
                        status: 0,
                        latency_ms: 0,
                        error: Some(format!("未配置 {var}，无法测试该内置模型")),
                        models: None,
                        model_found: None,
                    }));
                }
            }
        }
        _ => req.api_key.clone(),
    };

    if base.is_empty() || api_key.trim().is_empty() {
        return Ok(Json(InlineTestResponse {
            ok: false,
            kind: Some("bad_request".into()),
            status: 0,
            latency_ms: 0,
            error: Some("接口地址与 API Key 都不能为空".into()),
            models: None,
            model_found: None,
        }));
    }

    let url = format!("{}/models", base);
    let started = std::time::Instant::now();
    // 10s 上限：连通性测试不是聊天请求，超时了就该立刻告诉用户，
    // 别让弹窗上的按钮转半分钟。
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    match client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let ok = resp.status().is_success();
            let latency_ms = started.elapsed().as_millis() as u64;
            let (models, model_found) = if ok {
                let text = resp.text().await.unwrap_or_default();
                match serde_json::from_str::<ModelsApiResponse>(&text) {
                    Ok(data) => {
                        let ids: Vec<String> = data.data.iter().map(|m| m.id.clone()).collect();
                        let found = req
                            .model
                            .as_deref()
                            .map(|want| ids.iter().any(|m| m == want));
                        (Some(ids), found)
                    }
                    Err(_) => (None, None),
                }
            } else {
                (None, None)
            };
            Ok(Json(InlineTestResponse {
                ok,
                kind: if ok { None } else { Some(classify(status).into()) },
                status,
                latency_ms,
                error: if ok { None } else { Some(format!("HTTP {}", status)) },
                models,
                model_found,
            }))
        }
        // 连不上 / DNS / TLS 失败都属于"网络"，与鉴权失败严格区分 ——
        // 用户的处置动作完全不同（换 Key vs 查网络）。
        Err(e) => Ok(Json(InlineTestResponse {
            ok: false,
            kind: Some("network".into()),
            status: 0,
            latency_ms: started.elapsed().as_millis() as u64,
            error: Some(e.to_string()),
            models: None,
            model_found: None,
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
    let config_path = state.hermix_home.join("config.yaml");
    let mut config = read_gateway_config(&state)?;
    // 与 create/delete 一致：命名规范只拦「不在册」的 id，历史遗留条目可改。
    if !id.starts_with("custom-") && !config.llm_providers.contains_key(&id) {
        return Err(HttpError { status: axum::http::StatusCode::BAD_REQUEST, message: "Provider ID must start with 'custom-'".to_string() });
    }
    let model_contexts: HashMap<String, u64> = req.model_contexts.into_iter().filter(|(_, v)| *v > 0).collect();
    let count = model_contexts.len();
    config.llm_providers.entry(id.clone()).or_insert_with(hermix_config::LlmProviderConfig::default).model_contexts = model_contexts;
    hermix_config::loader::save_config_yaml(&config_path, &config).map_err(|e| HttpError { status: axum::http::StatusCode::INTERNAL_SERVER_ERROR, message: format!("failed to write config: {}", e) })?;
    tracing::info!(provider = %id, count, "model_contexts updated");
    Ok(Json(OkResponse { ok: true }))
}
