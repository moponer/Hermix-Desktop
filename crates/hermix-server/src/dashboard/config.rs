//! Configuration management endpoints.

use axum::extract::State;
use axum::Json;

use super::types::*;
use crate::{HttpError, HttpServerState};

/// Response shape for `POST /api/config/auto-correct` — mirrors
/// `hermix_config::AutoCorrectReport` so the desktop UI can render
/// "auto-routed from <before> to <after> because <reason>".
#[derive(Debug, Clone, serde::Serialize)]
pub struct AutoCorrectResponse {
    /// The fresh `GatewayConfig` after auto-correction. The
    /// `state.config` snapshot is NOT updated — only the agent
    /// service's live config is, so callers that need the latest
    /// values should re-fetch `GET /api/config`.
    pub config: serde_json::Value,
    /// The structured correction report. `null` when no correction
    /// was needed (config was already valid).
    pub report: Option<hermix_config::AutoCorrectReport>,
    /// Human-readable summary suitable for a toast notification.
    pub message: String,
}

/// `POST /api/config/auto-correct` — point `config.model` at a
/// provider the user actually has a key for.
/// Idempotent: if `config.model` already references a configured
/// provider the call returns `{report: null, message: "already
/// valid"}` without touching anything. Otherwise it picks a
/// provider in the priority order
/// openrouter → anthropic → minimax → dashscope → zhipu → volcengine
/// → siliconflow → moonshot → kimi → deepseek → qwen → nous → openai,
/// writes the corrected config to `config.yaml`, AND updates the
/// live `AgentService` config so the next chat picks it up without
/// a desktop-app restart.
pub async fn auto_correct_config(
    State(state): State<HttpServerState>,
) -> Result<Json<AutoCorrectResponse>, HttpError> {
    // 1. Pull a *fresh* config snapshot from the agent service (it
    // may differ from the startup-time `state.config` if the
    // user already saved a key through Settings).
    let mut cfg: hermix_config::GatewayConfig = state
        .agent_service
        .current_config_value()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_else(|| state.config.as_ref().clone());

    // 2. Run the auto-correct.
    let report = hermix_config::auto_correct_model(&mut cfg);

    // 3. If a correction was made, persist + push to the agent
    // service. We always serialize the (possibly unchanged) cfg
    // back to the client so the UI can display the effective
    // model.
    if let Some(ref r) = report {
        // Persist to disk so subsequent server starts don't re-do
        // the same correction.
        let config_path = state.hermix_home.join("config.yaml");
        if let Err(e) = hermix_config::save_config_yaml(&config_path, &cfg) {
            return Err(HttpError {
                status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                message: format!("failed to persist auto-corrected config: {e}"),
            });
        }
        // Push to the live agent service so the next chat uses the
        // new model immediately.
        let cfg_value = serde_json::to_value(&cfg).map_err(|e| HttpError {
            status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("failed to serialize corrected config: {e}"),
        })?;
        state.agent_service.set_config(cfg_value);

        tracing::info!(
            before = ?r.before,
            after = %r.after,
            provider = %r.provider,
            reason = %r.reason,
            configured = ?r.configured_providers,
            "/api/config/auto-correct: applied correction"
        );
    }

    let message = match &report {
        Some(r) => format!(
            "已自动将模型从 '{}' 切换到 '{}' (provider: {}, 原因: {})。\
             已配置的 provider: [{}]。",
            r.before.as_deref().unwrap_or("(未设置)"),
            r.after,
            r.provider,
            r.reason,
            r.configured_providers.join(", ")
        ),
        None => "模型配置已正确，无需修正。".to_string(),
    };

    let cfg_value = serde_json::to_value(&cfg).map_err(|e| HttpError {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("failed to serialize config: {e}"),
    })?;

    Ok(Json(AutoCorrectResponse {
        config: cfg_value,
        report,
        message,
    }))
}

/// GET /api/config — return current config as JSON.
pub async fn get_config(State(state): State<HttpServerState>) -> Json<serde_json::Value> {
    let config = hermix_config::load_config(Some(state.hermix_home.to_str().unwrap_or_default()))
        .map(|c| serde_json::to_value(c).unwrap_or_default())
        .unwrap_or_default();
    Json(config)
}
/// PUT /api/config — update config from JSON.
pub async fn update_config(
    State(state): State<HttpServerState>,
    Json(body): Json<ConfigUpdate>,
) -> Result<Json<OkResponse>, HttpError> {
    let config_path = state.hermix_home.join("config.yaml");
    let yaml = serde_yaml::to_string(&body.config).map_err(|e| HttpError {
        status: axum::http::StatusCode::BAD_REQUEST,
        message: format!("invalid config: {}", e),
    })?;
    std::fs::write(&config_path, yaml).map_err(|e| HttpError {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("failed to write config: {}", e),
    })?;
    // Push to the live agent service so the next chat uses the
    // updated config immediately.
    let cfg_value = serde_json::to_value(&body.config).map_err(|e| HttpError {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("failed to serialize config: {e}"),
    })?;
    state.agent_service.set_config(cfg_value);
    Ok(Json(OkResponse { ok: true }))
}
/// GET /api/model/info — return current model information.
/// Reads from the *live* agent service config (which may differ from
/// `state.config` after `/api/config/auto-correct` has been called),
/// so the desktop model selector and chat default always reflect
/// the model the next chat will actually use. When no model is set
/// anywhere we report `provider = "auto"` and a clear "no model
/// configured" message instead of silently falling back to
/// `openai:gpt-4o` (the previous behavior that hid provider
/// misconfiguration behind a generic "missing API key" error).
pub async fn get_model_info(State(state): State<HttpServerState>) -> Json<ModelInfoResponse> {
    // Prefer the live agent-service config (auto-correct updates
    // land here but not on state.config). Fall back to state.config
    // for older agent-service impls that don't expose
    // `current_config_value`.
    let model = state
        .agent_service
        .current_config_value()
        .and_then(|v| v.get("model").and_then(|m| m.as_str()).map(|s| s.to_string()))
        .or_else(|| state.config.model.clone());

    match model.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(m) => {
            let (provider, model_name) = m.split_once(':').unwrap_or(("openai", m));
            // Pull the context window from the static catalog + any
            // per-provider override. Previously hardcoded 128000 which
            // lied about e.g. gemini-2.5-flash (actual 1_048_576) and
            // made the UI show "100% used" the moment a session
            // crossed 128k tokens. The owner asked for the real value
            // here — `get_model_context_length_with` honours the
            // user-set provider override first, then falls through to
            // the catalog (gemini-2.5-flash = 1_048_576, claude-4-6
            // family = 1_000_000, gpt-5 = 128_000, etc.).
            let ctx_length: u32 = hermix_intelligence::model_metadata::get_model_context_length_with(
                &m,
                Some(&state.config.llm_providers),
            ) as u32;
            Json(ModelInfoResponse {
                model: model_name.to_string(),
                provider: provider.to_string(),
                auto_context_length: ctx_length,
                config_context_length: 0,
                effective_context_length: ctx_length,
                capabilities: serde_json::json!({
                    "supports_tools": true,
                    "supports_vision": true,
                    "context_window": ctx_length,
                }),
            })
        }
        None => Json(ModelInfoResponse {
            model: "(no model configured)".to_string(),
            provider: "auto".to_string(),
            auto_context_length: 0,
            config_context_length: 0,
            effective_context_length: 0,
            capabilities: serde_json::json!({
                "supports_tools": false,
                "supports_vision": false,
                "context_window": 0,
                "hint": "Open Settings → API Keys and add a key, then click 'Fetch Models'."
            }),
        }),
    }
}

/// Response for `GET /api/model/list`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelListResponse {
    /// Every model the user has enabled in the chat switcher.
    pub models: Vec<hermix_config::ModelSelectorEntry>,
    /// The chosen default `"provider:model"`, if any.
    pub default_model: Option<String>,
}

/// Request body for `PUT /api/model/list`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ModelListRequest {
    /// Full enabled-model list (replaces the persisted set).
    pub models: Vec<hermix_config::ModelSelectorEntry>,
    /// Chosen default `"provider:model"`.
    #[serde(default)]
    pub default_model: Option<String>,
}

/// GET /api/model/list — return the authoritative enabled-model list.
/// The desktop model switcher keeps an immediate copy in `localStorage`,
/// but `localStorage` is volatile (some Tauri storage evictions / dev HMR
/// wipe it), which used to make the switcher "forget" configured models and
/// fall back to showing only the backend default. This endpoint serves the
/// durable, backend-persisted copy (`GatewayConfig::model_selector`) as the
/// source of truth. When that is absent we fall back to building the list
/// from each `llm_providers[*].model` so the switcher is never empty.
pub async fn get_model_list(State(state): State<HttpServerState>) -> Json<ModelListResponse> {
    let cfg: hermix_config::GatewayConfig = state
        .agent_service
        .current_config_value()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_else(|| state.config.as_ref().clone());

    if let Some(sel) = &cfg.model_selector {
        if !sel.enabled.is_empty() || sel.default_model.is_some() {
            return Json(ModelListResponse {
                models: sel.enabled.clone(),
                default_model: sel.default_model.clone(),
            });
        }
    }

    // Fallback: builtin provider defaults.
    let mut models = Vec::new();
    for (pid, pc) in &cfg.llm_providers {
        if let Some(m) = &pc.model {
            if !m.trim().is_empty() {
                models.push(hermix_config::ModelSelectorEntry {
                    provider_id: pid.clone(),
                    model_id: m.clone(),
                });
            }
        }
    }
    Json(ModelListResponse {
        models,
        default_model: cfg.model.clone(),
    })
}

/// PUT /api/model/list — persist the full enabled-model list.
/// Writes `GatewayConfig::model_selector` to `config.yaml` and pushes the
/// updated config to the live agent service, so the model switcher is stable
/// across app restarts even if `localStorage` is wiped.
pub async fn update_model_list(
    State(state): State<HttpServerState>,
    Json(body): Json<ModelListRequest>,
) -> Result<Json<OkResponse>, HttpError> {
    let mut cfg: hermix_config::GatewayConfig = state
        .agent_service
        .current_config_value()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_else(|| state.config.as_ref().clone());

    // Keep `model` (the legacy single default) in sync with the selector
    // default so other readers of `config.model` don't drift.
    if let Some(def) = &body.default_model {
        if !def.trim().is_empty() {
            cfg.model = Some(def.clone());
        }
    }

    cfg.model_selector = Some(hermix_config::ModelSelectorConfig {
        enabled: body.models,
        default_model: body.default_model,
    });

    let config_path = state.hermix_home.join("config.yaml");
    if let Err(e) = hermix_config::save_config_yaml(&config_path, &cfg) {
        return Err(HttpError {
            status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("failed to persist model list: {e}"),
        });
    }
    let cfg_value =
        serde_json::to_value(&cfg).map_err(|e| HttpError {
            status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("failed to serialize config: {e}"),
        })?;
    state.agent_service.set_config(cfg_value);

    Ok(Json(OkResponse { ok: true }))
}
