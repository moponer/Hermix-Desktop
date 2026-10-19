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
/// Read the on-disk `config.yaml` as a JSON object.
///
/// A missing / unparseable file degrades to `{}` — same spirit as
/// `load_config` ("a broken file is a fresh config"), so a damaged file
/// must not turn one save into a hard 500.
fn read_config_json(path: &std::path::Path) -> serde_json::Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_yaml::from_str::<serde_yaml::Value>(&s).ok())
        .and_then(|v| serde_json::to_value(v).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}))
}

/// Merge the fields a request actually carried into the existing config.
///
/// This exists because the handler used to serialise `body.config`
/// **verbatim** over `config.yaml`. The desktop's `AdvancedSettingsTab`
/// only ever sends `{compaction, advanced}` — so saving an unrelated
/// setting silently deleted `model`, `llm_providers`, `model_selector`
/// and `custom_providers` outright. Everything downstream that reads
/// those (notably `GET /api/model/list`) then reported "no models
/// configured" while chat kept working off the surviving `model` field.
///
/// Semantics are JSON Merge Patch: a key present in the patch wins, a
/// `null` removes it, and absent keys are left alone.
fn merge_config_patch(existing: &mut serde_json::Value, patch: &serde_json::Value) {
    let (Some(dst), Some(src)) = (existing.as_object_mut(), patch.as_object()) else {
        return;
    };
    for (key, value) in src {
        if value.is_null() {
            dst.remove(key);
        } else {
            dst.insert(key.clone(), value.clone());
        }
    }
}

/// PUT /api/config — update config from JSON.
pub async fn update_config(
    State(state): State<HttpServerState>,
    axum::extract::Extension(origin): axum::extract::Extension<hermix_core::Origin>,
    axum::extract::Extension(device_scopes): axum::extract::Extension<
        Option<std::collections::HashSet<hermix_core::origin::Capability>>,
    >,
    Json(body): Json<ConfigUpdate>,
) -> Result<Json<OkResponse>, HttpError> {
    // §5.5 第 5 层：端点级能力位关口（ConfigMutation）。PairedDevice 来源
    // 未配该能力位 → 403（矩阵已 Denied，端点层再拦是纵深）；Local 来源放行。
    crate::routes::check_capability_from_extensions(
        hermix_core::origin::Capability::ConfigMutation,
        &origin,
        &device_scopes,
    )
    .map_err(|(status, message)| HttpError { status, message })?;
    let config_path = state.hermix_home.join("config.yaml");
    // Merge into the on-disk config rather than overwriting it — see
    // `merge_config_patch` for the data loss this replaces.
    let mut merged = read_config_json(&config_path);
    merge_config_patch(&mut merged, &body.config);

    let yaml = serde_yaml::to_string(&merged).map_err(|e| HttpError {
        status: axum::http::StatusCode::BAD_REQUEST,
        message: format!("invalid config: {}", e),
    })?;
    // 原子写（tmp+rename）：整份覆盖期间并发读者（load_config）会读到空/
    // 半截 YAML，把 providers 等整段配置丢掉。
    hermix_config::loader::write_yaml_atomic(&config_path, &yaml).map_err(|e| HttpError {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("failed to write config: {}", e),
    })?;
    // Push to the live agent service so the next chat uses the
    // updated config immediately. This must be the *merged* config:
    // pushing `body.config` would leave the same truncated shape in
    // memory even with disk intact, and `current_config_value()` is what
    // `GET /api/config` and `GET /api/model/list` read first.
    state.agent_service.set_config(merged.clone());

    // 安全中心：配置变更后刷新自动备份策略（best-effort）。
    if let Some(sec) = merged.get("security") {
        if let Ok(sec) = serde_json::from_value::<hermix_config::SecurityConfig>(sec.clone()) {
            hermix_tools::backup::set_policy_from_config(&sec, &state.hermix_home);
        }
    }

    Ok(Json(OkResponse { ok: true }))
}

/// GET /api/backup/status — 返回自动备份目录的统计（总数/占用/最近备份）。
pub async fn get_backup_status(
    State(state): State<HttpServerState>,
) -> Json<hermix_tools::backup::BackupStatus> {
    let _ = &state;
    Json(hermix_tools::backup::status())
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
            // Pull reasoning / vision / tools capability bits from the static
            // catalog so the desktop model-info card can show "supports thinking"
            // instead of hardcoding `true`. `get_model_info` returns None for
            // unknown models — in that case default tools=true (every chat model
            // in this app is tool-calling), reasoning/vision=false.
            let caps = hermix_intelligence::model_metadata::get_model_info(&m).unwrap_or_else(|| {
                hermix_intelligence::model_metadata::ModelInfo {
                    name: model_name.to_string(),
                    provider: provider.to_string(),
                    context_window: ctx_length as u64,
                    max_output_tokens: None,
                    supports_vision: false,
                    supports_tools: true,
                    supports_streaming: true,
                    supports_reasoning: false,
                    input_cost_per_million: None,
                    output_cost_per_million: None,
                }
            });
            Json(ModelInfoResponse {
                model: model_name.to_string(),
                provider: provider.to_string(),
                auto_context_length: ctx_length,
                config_context_length: 0,
                effective_context_length: ctx_length,
                capabilities: serde_json::json!({
                    "supports_tools": caps.supports_tools,
                    "supports_vision": caps.supports_vision,
                    "supports_reasoning": caps.supports_reasoning,
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

/// Request for `POST /api/models/context-windows`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ContextWindowsRequest {
    /// Full `"provider:model"` ids to resolve.
    pub models: Vec<String>,
}

/// One resolved context window — the value plus the tier that produced it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ResolvedContextWindow {
    pub value: u64,
    /// `"probed"` | `"provider"` | `"catalog"` | `"fallback"`.
    pub source: String,
}

/// Response for `POST /api/models/context-windows`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ContextWindowsResponse {
    /// Keyed by the requested model id.
    pub windows: std::collections::HashMap<String, ResolvedContextWindow>,
}

/// POST /api/models/context-windows — authoritative context-window
/// resolution for a batch of models.
///
/// The settings UI used to hand-mirror the backend's static catalog
/// (`DEFAULT_CONTEXT_LENGTHS`) just so it could label where a number came
/// from; the two copies drifted apart. The backend owns the catalog, so it
/// now answers the question directly.
pub async fn resolve_context_windows(
    State(state): State<HttpServerState>,
    Json(body): Json<ContextWindowsRequest>,
) -> Json<ContextWindowsResponse> {
    let providers = &state.config.llm_providers;
    let windows = body
        .models
        .iter()
        .map(|m| {
            let (value, source) =
                hermix_intelligence::model_metadata::get_model_context_length_with_source(
                    m,
                    Some(providers),
                );
            (
                m.clone(),
                ResolvedContextWindow {
                    value,
                    source: source.as_str().to_string(),
                },
            )
        })
        .collect();
    Json(ContextWindowsResponse { windows })
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
    Json(build_model_list(&cfg))
}

/// 清单构建（`get_model_list` 的纯逻辑，独立出来可测）。
fn build_model_list(cfg: &hermix_config::GatewayConfig) -> ModelListResponse {
    // 仅保留后端确实已配置的条目，避免下拉展示 provider 改名/删除后残留
    // 的孤儿模型（orphan）。判定必须复用 hermix_config::provider_has_key
    // （config api_key 或环境变量任一命中）——此前只查 config 字段的半份
    // 副本会把纯 env 配钥的内置 provider 全部误杀出下拉。
    let is_configured = |pid: &str| hermix_config::provider_has_key(cfg, pid);

    if let Some(sel) = &cfg.model_selector {
        if !sel.enabled.is_empty() || sel.default_model.is_some() {
            let models: Vec<hermix_config::ModelSelectorEntry> = sel
                .enabled
                .iter()
                .filter(|e| is_configured(&e.provider_id))
                .cloned()
                .collect();
            // 全部条目都是孤儿（provider 改名/删钥）时不在这里返回：
            // 落到下面的兜底，别把"实际在跑"的模型藏成"未配置"。
            if !models.is_empty() {
                let default_model = sel
                    .default_model
                    .as_ref()
                    .filter(|d| {
                        d.split_once(':')
                            .map(|(p, _)| models.iter().any(|m| m.provider_id == p))
                            .unwrap_or(false)
                    })
                    .cloned();
                return ModelListResponse {
                    models,
                    default_model,
                };
            }
        }
    }

    // Fallback: builtin provider defaults.
    let mut models = Vec::new();
    for (pid, pc) in &cfg.llm_providers {
        if let Some(m) = &pc.model {
            if !is_configured(pid) {
                continue;
            }
            // `model` 是逗号分隔的清单（自定义 provider / 内置免费渠道都写多值）。
            // 整串当成一个模型 id 会造出 `a,b` 这种假条目——切换到它必失败。
            for part in m.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                models.push(hermix_config::ModelSelectorEntry {
                    provider_id: pid.clone(),
                    model_id: part.to_string(),
                });
            }
        }
    }
    let default_model = match &cfg.model {
        Some(d) if d.split_once(':').map(|(p, _)| is_configured(p)).unwrap_or(false) => {
            Some(d.clone())
        }
        _ => models
            .first()
            .map(|m| format!("{}:{}", m.provider_id, m.model_id)),
    };
    // 顶层 `model` 兜底（2026-10-04 实测）：`model_selector` 从未写过、
    // provider 条目又没填 `model` 字段时，对话一直按顶层 `model` 跑 ——
    // 把它合成进清单，否则开关器显示"未配置"而聊天正常工作。
    if models.is_empty() {
        if let Some(d) = &default_model {
            if let Some((pid, mid)) = d.split_once(':') {
                models.push(hermix_config::ModelSelectorEntry {
                    provider_id: pid.to_string(),
                    model_id: mid.to_string(),
                });
            }
        }
    }
    ModelListResponse {
        models,
        default_model,
    }
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

/// GET /api/providers/meta — 内置 provider 元数据（id + 配钥环境变量名 +
/// 权威默认模型）。唯一真理源 `hermix_config::BUILTIN_PROVIDER_ENV_VARS` 与
/// `hermix_config::default_model_for_provider`；前端据此做 builtin/自定义分组、
/// 环境变量提示，以及**首次配置时自动选中哪个模型**。
///
/// 下发 `default_model` 是为了让桌面端不必自己猜：厂商 `/v1/models` 返回的
/// 是全量目录（含 embedding / 图像模型），按字典序取第一个会选中
/// `babbage-002` 这类非对话模型；这里给的是人工维护的可对话默认。
pub async fn get_provider_meta() -> Json<serde_json::Value> {
    let items: Vec<serde_json::Value> = hermix_config::BUILTIN_PROVIDER_ENV_VARS
        .iter()
        .map(|(id, env_var)| {
            // `default_model_for_provider` 对未收录的 provider 返回占位符
            // "default"，这不是一个真实模型名，下发成 null 让前端走自己的候选池。
            let dm = hermix_config::default_model_for_provider(id);
            serde_json::json!({
                "id": id,
                "env_var": env_var,
                "default_model": if dm == "default" { serde_json::Value::Null } else { serde_json::json!(dm) },
            })
        })
        .collect();
    Json(serde_json::Value::Array(items))
}

#[cfg(test)]
mod provider_meta_tests {
    use super::get_provider_meta;

    /// `/api/providers/meta` 必须下发权威默认模型，且 `default_model_for_provider`
    /// 的占位符 `"default"` 不能当成真实模型名漏给前端 —— 否则桌面端首次自动
    /// 选型会选中一个不存在的模型，聊天直接报错。
    #[tokio::test]
    async fn exposes_default_model_and_hides_placeholder() {
        let items = get_provider_meta().await.0;
        let items = items.as_array().expect("meta must be a JSON array");
        assert!(!items.is_empty());

        let find = |id: &str| items.iter().find(|v| v["id"] == id);
        let openai = find("openai").expect("openai is a builtin provider");
        assert_eq!(openai["default_model"], "gpt-4o");
        assert_eq!(openai["env_var"], "OPENAI_API_KEY");

        // baichuan 在 BUILTIN_PROVIDER_ENV_VARS 里但不在 default_model_for_provider
        // 的清单里 → 必须是 null，不能是占位符。
        let baichuan = find("baichuan").expect("baichuan is a builtin provider");
        assert!(baichuan["default_model"].is_null());

        assert!(
            items.iter().all(|v| v["default_model"] != "default"),
            "placeholder leaked into api response"
        );
    }
}

#[cfg(test)]
mod model_list_tests {
    use super::build_model_list;
    use hermix_config::{GatewayConfig, LlmProviderConfig};

    /// 2026-10-04 实测案例：`model_selector` 从未写过（None）、provider
    /// 条目没填 `model` 字段，只有顶层 `model` + 配好的 api_key —— 对话按
    /// 顶层 `model` 跑，清单却返回空，输入框芯片显示"未配置，请先配置模型"。
    #[test]
    fn legacy_top_level_model_becomes_selector_entry() {
        let mut cfg = GatewayConfig::default();
        cfg.model = Some("minimax:MiniMax-M3".to_string());
        cfg.llm_providers.insert(
            "minimax".to_string(),
            LlmProviderConfig {
                api_key: Some("sk-test".to_string()),
                ..Default::default()
            },
        );

        let resp = build_model_list(&cfg);
        assert_eq!(resp.models.len(), 1, "顶层 model 应合成出唯一条目");
        assert_eq!(resp.models[0].provider_id, "minimax");
        assert_eq!(resp.models[0].model_id, "MiniMax-M3");
        assert_eq!(resp.default_model.as_deref(), Some("minimax:MiniMax-M3"));
    }

    /// 反向：顶层 model 指向未配钥 provider（用非内置 id，避免测试机
    /// 环境变量干扰）时不得合成，清单如实为空。
    #[test]
    fn legacy_model_without_key_stays_empty() {
        let mut cfg = GatewayConfig::default();
        cfg.model = Some("ghost:Model-X".to_string());
        let resp = build_model_list(&cfg);
        assert!(resp.models.is_empty());
        assert!(resp.default_model.is_none());
    }

    /// `model` 是逗号分隔清单（内置免费渠道默认就写多条）。整串当成一个
    /// id 会让切换器出现 `a,b` 这种假条目，选中必失败。
    #[test]
    fn csv_model_field_splits_into_entries() {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "custom-free-kilo".to_string(),
            LlmProviderConfig {
                api_key: Some("none".to_string()),
                model: Some("openrouter/free, minimax/minimax-m3.1-flash:free".to_string()),
                ..Default::default()
            },
        );
        cfg.model = Some("custom-free-kilo:openrouter/free".to_string());

        let resp = build_model_list(&cfg);
        let ids: Vec<&str> = resp.models.iter().map(|m| m.model_id.as_str()).collect();
        assert_eq!(ids, vec!["openrouter/free", "minimax/minimax-m3.1-flash:free"]);
        assert_eq!(
            resp.default_model.as_deref(),
            Some("custom-free-kilo:openrouter/free")
        );
    }
}

#[cfg(test)]
mod config_patch_tests {
    use super::{merge_config_patch, read_config_json};

    /// Regression: saving an unrelated settings section must not delete the
    /// model configuration. Reproduces the 2026-10-03 incident where
    /// `AdvancedSettingsTab` PUT `{compaction, advanced}` and the handler
    /// wrote the body verbatim, wiping `model` / `llm_providers` /
    /// `model_selector` / `custom_providers` — the model settings page then
    /// reported "还没有配置任何模型".
    #[test]
    fn partial_patch_preserves_model_config() {
        let mut existing = serde_json::json!({
            "model": "minimax:MiniMax-M3",
            "llm_providers": { "minimax": { "api_key": "sk-live" } },
            "model_selector": {
                "enabled": [{ "provider_id": "minimax", "model_id": "MiniMax-M3" }],
                "default_model": "minimax:MiniMax-M3"
            },
            "custom_providers": [{ "id": "custom-agnes" }]
        });

        merge_config_patch(
            &mut existing,
            &serde_json::json!({
                "compaction": { "min_summary_tokens": 2000 },
                "advanced": { "sub_agent_max_turns_cap": 12 }
            }),
        );

        assert_eq!(existing["compaction"]["min_summary_tokens"], 2000);
        assert_eq!(existing["advanced"]["sub_agent_max_turns_cap"], 12);
        // The whole point: untouched top-level sections survive.
        assert_eq!(existing["model"], "minimax:MiniMax-M3");
        assert_eq!(existing["llm_providers"]["minimax"]["api_key"], "sk-live");
        assert_eq!(existing["model_selector"]["default_model"], "minimax:MiniMax-M3");
        assert_eq!(existing["model_selector"]["enabled"][0]["model_id"], "MiniMax-M3");
        assert_eq!(existing["custom_providers"][0]["id"], "custom-agnes");
    }

    /// A key present in the patch overwrites; an explicit `null` deletes.
    /// This is JSON Merge Patch semantics — "absent means untouched" is the
    /// property that makes partial saves safe.
    #[test]
    fn patch_overwrites_and_null_deletes() {
        let mut existing = serde_json::json!({
            "max_turns": 30,
            "model": "deepseek:deepseek-flash",
            "permission_profile": { "paths": ["/tmp"] }
        });

        merge_config_patch(
            &mut existing,
            &serde_json::json!({ "max_turns": 15, "model": null }),
        );

        assert_eq!(existing["max_turns"], 15);
        assert!(
            existing.get("model").is_none(),
            "null in patch must delete the key, got {}",
            existing["model"]
        );
        // An empty object is a real value, not a delete signal.
        assert!(existing["permission_profile"].is_object());
    }

    /// A missing or corrupt config.yaml degrades to `{}` instead of failing
    /// the request, matching `load_config`'s "broken file == fresh config".
    #[test]
    fn read_config_json_tolerates_missing_and_corrupt_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("nope.yaml");
        assert_eq!(read_config_json(&missing), serde_json::json!({}));

        let corrupt = dir.path().join("corrupt.yaml");
        std::fs::write(&corrupt, "{{{ not yaml at all").expect("write");
        assert_eq!(read_config_json(&corrupt), serde_json::json!({}));

        let good = dir.path().join("config.yaml");
        std::fs::write(&good, "model: minimax:MiniMax-M3\nmax_turns: 30\n").expect("write");
        let loaded = read_config_json(&good);
        assert_eq!(loaded["model"], "minimax:MiniMax-M3");
        assert_eq!(loaded["max_turns"], 30);
    }
}
