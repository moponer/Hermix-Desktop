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

    // 能力/上下文必须读**运行中的**配置：免费渠道的 `model_meta` 是启动后
    // 后台刷新写进 config.yaml + set_config 的，`state.config` 还是启动快照。
    let live_cfg: hermix_config::GatewayConfig = state
        .agent_service
        .current_config_value()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_else(|| state.config.as_ref().clone());

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
                Some(&live_cfg.llm_providers),
            ) as u32;
            // Pull reasoning / vision / tools capability bits from the static
            // catalog so the desktop model-info card can show "supports thinking"
            // instead of hardcoding `true`. `get_model_info` returns None for
            // unknown models — in that case default tools=true (every chat model
            // in this app is tool-calling), reasoning/vision=false.
            // 静态目录没有的模型（免费池几乎都这样）→ 用刷新时探测到的
            // `model_meta`。没有它的话，支持 reasoning 的模型也拿不到
            // 思考开关（此前 unknown 一律 reporting 成 false）。
            let meta = live_cfg
                .llm_providers
                .get(provider)
                .and_then(|p| p.model_meta.get(model_name));
            let caps = hermix_intelligence::model_metadata::get_model_info(&m).unwrap_or_else(|| {
                hermix_intelligence::model_metadata::ModelInfo {
                    name: model_name.to_string(),
                    provider: provider.to_string(),
                    context_window: ctx_length as u64,
                    max_output_tokens: meta.and_then(|x| x.max_output_tokens),
                    supports_vision: meta.and_then(|x| x.supports_vision).unwrap_or(false),
                    // 没探测结果时保守按"支持工具"（本应用所有对话模型都要用工具）。
                    supports_tools: meta.and_then(|x| x.supports_tools).unwrap_or(true),
                    supports_streaming: true,
                    supports_reasoning: meta
                        .and_then(|x| x.supports_reasoning)
                        .unwrap_or(false),
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
                    "max_output_tokens": caps.max_output_tokens,
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

/// `GET /api/model/list` 的单条模型（启用/可添加通用）。
///
/// 除标识外还带上上游声明的能力（上下文、最大输出、模态、reasoning），
/// 让模型选择卡片不必再逐条向 `/api/model/info` 追问，也不至于只能显示
/// 一个光秃秃的模型 id。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelInfoEntry {
    pub provider_id: String,
    pub model_id: String,
    /// 供应商展示名（自定义 provider 用 `display_name`，否则 id）。
    pub provider_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_tools: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_reasoning: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_vision: Option<bool>,
    /// 内置免费渠道的模型（UI 显示绿色「免费」标记）。
    pub is_free: bool,
    /// 上游声明会拿提示词训练（免责文案据此升级）。
    pub trains_on_prompts: bool,
}

/// Response for `GET /api/model/list`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelListResponse {
    /// Every model the user has enabled in the chat switcher.
    pub models: Vec<ModelInfoEntry>,
    /// The chosen default `"provider:model"`, if any.
    pub default_model: Option<String>,
    /// 已配置但**未启用**的模型（设置页的「可添加」清单）。
    ///
    /// 与 `models` 分开是硬要求：此前两者共用一个数组，设置页点一次
    /// 「设为默认」就把整份自定义/免费清单写进 `enabled`，付费厂商的
    /// 模型被挤出可见范围（用户实测事故）。这里只读，不进 enabled。
    #[serde(default)]
    pub catalog: Vec<ModelInfoEntry>,
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
    Json(build_model_list(&current_gateway_config(&state)))
}

/// 桌面的**当前**网关配置（内存里的 live config 优先，落盘配置兜底）。
///
/// `GET /api/model/list` 与 WS 推送共用这一份——两处各读一次会出现
/// 「HTTP 看到的清单和推给手机的不一样」这种最难查的偏差。
fn current_gateway_config(state: &HttpServerState) -> hermix_config::GatewayConfig {
    state
        .agent_service
        .current_config_value()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_else(|| state.config.as_ref().clone())
}

/// 当前模型清单（供 **WS 推给手机**用）。
///
/// 手机端没有独立的模型配置面：清单的唯一真相源在桌面。手机此前只能走
/// `GET {relay}/api/model/list`——而中继是**只路由的字节管道**，没有通往
/// 节点的 HTTP 通道，那条请求必然 404，于是模型选择器永远是空的。这里把
/// 同一份逻辑暴露出来，让桌面在手机连上时推一份快照过去。
pub fn model_list_for(state: &HttpServerState) -> ModelListResponse {
    build_model_list(&current_gateway_config(state))
}

/// 手机在**电脑离线**时直连云模型用的 provider 凭据。
///
/// 只给**已启用模型实际用到的** provider（其余的不推）——推得越少，手机上
/// 持有的密钥面越小。
///
/// key 的来源，按优先级：
/// 1. `config.yaml` 里 `llm_providers[pid].api_key` 的**字面量**
///    （`${ENV}` 形式是引用不是密钥本身，不算）；
/// 2. 环境变量里的 key（`provider_api_key_from_env`）；
/// 3. 内置免费渠道的哨兵凭据。
///
/// 免费渠道的 `KEY_NO_AUTH` 哨兵会被解析成 `None`（无鉴权端点）——手机端
/// 的约定是「没有 key 就不发 Authorization 头」，与 `auth_header` 一致。
pub fn model_credentials_for(
    state: &HttpServerState,
    providers_in_use: &[String],
) -> Vec<hermix_transport::ProviderCredential> {
    let cfg = current_gateway_config(state);
    let mut out = Vec::new();
    for pid in providers_in_use {
        let pc = cfg.llm_providers.get(pid);
        let free = hermix_config::free_channel::FREE_CHANNELS
            .iter()
            .find(|c| c.id == pid);
        let base_url = pc
            .and_then(|p| p.base_url.clone())
            .or_else(|| free.map(|c| c.base_url.to_string()));
        let key = pc
            .and_then(|p| p.api_key.as_deref())
            .map(str::trim)
            .filter(|k| !k.is_empty() && !k.starts_with("${"))
            .map(str::to_string)
            .or_else(|| hermix_config::provider_api_key_from_env(pid))
            .or_else(|| free.map(|c| c.api_key.to_string()))
            .filter(|k| !k.eq_ignore_ascii_case(hermix_config::free_channel::KEY_NO_AUTH));
        out.push(hermix_transport::ProviderCredential {
            provider_id: pid.clone(),
            base_url,
            api_key: key,
            // 目前只实现了 OpenAI 兼容风格（`{base}/chat/completions`）。
            api_style: "openai".to_string(),
        });
    }
    out
}

/// 手机端「模型配置推送」开关（主人拍板 2026-10-22：**默认关闭**）。
///
/// ## 为什么要有这个开关
///
/// 推给手机的不只是模型名单，还有 **provider 凭据**（base_url + API Key）——
/// 没有凭据，手机在电脑关机时就没法直连云厂商继续对话。
/// 但凭据出电脑等于**多了一把长期有效的钥匙**：它不像其它配对能力那样随电脑
/// 一起失效，手机丢了/被攻破就等于泄漏了一把能烧钱的 key。
///
/// 所以这件事**默认不做**，要由主人在电脑端显式开一次。
///
/// ## 关闭 = 撤销，不是「不再推」
///
/// 关掉后推给手机的是一份**空清单**（`models`/`providers` 都空）——手机端据此
/// **删掉本地存的那份**（含 key）并提示「电脑端未启用模型配置推送」。
/// 只停推而不清手机那份，等于给人**假安全感**：用户以为收���了，手机其实还在
/// 拿着旧 key 照用不误。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct MobileModelPushConfig {
    /// 是否允许把模型配置（含凭据）推给手机。**默认 false。**
    #[serde(default)]
    pub enabled: bool,
}

impl MobileModelPushConfig {
    fn path(state: &HttpServerState) -> std::path::PathBuf {
        state.hermix_home.join("mobile_model_push.json")
    }

    /// 读开关。文件缺失 / 解析失败 → **关闭**（fail-closed）。
    pub fn load(state: &HttpServerState) -> Self {
        let Ok(raw) = std::fs::read_to_string(Self::path(state)) else {
            return Self::default();
        };
        match serde_json::from_str(&raw) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!("mobile_model_push.json 解析失败（按关闭处理）: {e}");
                Self::default()
            }
        }
    }

    fn save(&self, state: &HttpServerState) -> Result<(), String> {
        let path = Self::path(state);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())
    }

    /// 当前是否允许推送（含凭据）。**默认 false。**
    pub fn is_enabled(state: &HttpServerState) -> bool {
        Self::load(state).enabled
    }
}

/// GET /api/mobile/model-push — 桌面端读开关状态。
pub async fn get_mobile_model_push(
    State(state): State<HttpServerState>,
) -> Json<MobileModelPushConfig> {
    Json(MobileModelPushConfig::load(&state))
}

/// PUT /api/mobile/model-push — 桌面端保存开关。保存即生效：下一次手机连上
/// 就按新状态推送（已在连的手机重连后更新）。
pub async fn put_mobile_model_push(
    State(state): State<HttpServerState>,
    Json(body): Json<MobileModelPushConfig>,
) -> Result<Json<serde_json::Value>, HttpError> {
    body.save(&state).map_err(|e| HttpError {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("mobile_model_push.json 写入失败: {e}"),
    })?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// 只改**默认模型**，不动 `enabled` 清单。
///
/// 手机端切模型时只带一个 `provider:model`。若让它整份回写 enabled，会把
/// 桌面「已启用」列表覆盖成手机那份——付费厂商的模型会被挤出可见范围
/// （`/api/model/list` 的注释记着这次用户实测事故）。这里保持 enabled
/// 原样，只改 default_model，并同步旧的单模型字段 `cfg.model`。
pub fn set_default_model(state: &HttpServerState, model: &str) -> Result<(), String> {
    let m = model.trim();
    if m.is_empty() {
        return Err("模型名为空".to_string());
    }
    if !m.contains(':') {
        return Err(format!("模型名应为 provider:model，收到 {m:?}"));
    }
    let mut cfg = current_gateway_config(state);
    cfg.model = Some(m.to_string());
    if let Some(sel) = cfg.model_selector.as_mut() {
        sel.default_model = Some(m.to_string());
    }
    let config_path = state.hermix_home.join("config.yaml");
    hermix_config::save_config_yaml(&config_path, &cfg)
        .map_err(|e| format!("persist model selection: {e}"))?;
    let cfg_value = serde_json::to_value(&cfg).map_err(|e| format!("serialize config: {e}"))?;
    state.agent_service.set_config(cfg_value);
    Ok(())
}

/// 解析单条模型的展示信息与能力。
///
/// 优先级：上游探测（`model_meta`）> 人维护的静态目录 > provider 级兜底。
/// 静态目录排在后面是因为它只覆盖主流模型；免费池模型只可能来自探测。
fn resolve_model_info(
    cfg: &hermix_config::GatewayConfig,
    pid: &str,
    model: &str,
) -> ModelInfoEntry {
    let provider = cfg.llm_providers.get(pid);
    let meta = provider.and_then(|p| p.model_meta.get(model));
    let full = format!("{pid}:{model}");
    let known = hermix_intelligence::model_metadata::get_model_info(&full);

    let context_window = meta
        .and_then(|m| m.context_window)
        .or_else(|| provider.and_then(|p| p.model_contexts.get(model).copied()))
        .or_else(|| known.as_ref().map(|k| k.context_window))
        .filter(|v| *v > 0)
        .or_else(|| provider.and_then(|p| p.context_window))
        .unwrap_or_else(|| {
            hermix_intelligence::model_metadata::get_model_context_length_with(
                &full,
                Some(&cfg.llm_providers),
            )
        });

    ModelInfoEntry {
        provider_id: pid.to_string(),
        model_id: model.to_string(),
        provider_name: provider
            .and_then(|p| p.display_name.clone())
            .unwrap_or_else(|| pid.to_string()),
        context_window: Some(context_window),
        max_output_tokens: meta
            .and_then(|m| m.max_output_tokens)
            .or_else(|| known.as_ref().and_then(|k| k.max_output_tokens)),
        supports_tools: meta
            .and_then(|m| m.supports_tools)
            .or_else(|| known.as_ref().map(|k| k.supports_tools)),
        supports_reasoning: meta
            .and_then(|m| m.supports_reasoning)
            .or_else(|| known.as_ref().map(|k| k.supports_reasoning)),
        supports_vision: meta
            .and_then(|m| m.supports_vision)
            .or_else(|| known.as_ref().map(|k| k.supports_vision)),
        is_free: hermix_config::free_channel::is_free_provider(pid),
        trains_on_prompts: meta.map(|m| m.trains_on_prompts).unwrap_or(false),
    }
}

/// 清单构建（`get_model_list` 的纯逻辑，独立出来可测）。
fn build_model_list(cfg: &hermix_config::GatewayConfig) -> ModelListResponse {
    // 仅保留后端确实已配置的条目，避免下拉展示 provider 改名/删除后残留
    // 的孤儿模型（orphan）。判定必须复用 hermix_config::provider_has_key
    // （config api_key 或环境变量任一命中）——此前只查 config 字段的半份
    // 副本会把纯 env 配钥的内置 provider 全部误杀出下拉。
    let is_configured = |pid: &str| hermix_config::provider_has_key(cfg, pid);

    // 「可添加」清单原料：每个已配置 provider 的模型。
    // `model` 为空时必须回落到该厂商的权威默认模型 —— 只配了 key 的
    // 厂商（minimax 这种）此前在列表里彻底消失，用户以为配置丢了。
    let mut catalog_ids: Vec<(String, String)> = Vec::new();
    for (pid, pc) in &cfg.llm_providers {
        if !is_configured(pid) {
            continue;
        }
        let mut models: Vec<String> = pc
            .model
            .as_deref()
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        if models.is_empty() {
            let dm = hermix_config::default_model_for_provider(pid);
            if dm != "default" {
                models.push(dm.to_string());
            }
        }
        for model_id in models {
            catalog_ids.push((pid.clone(), model_id));
        }
    }
    // 顶层 `model` 是"实际在跑的模型"，必须在清单里可见 —— 否则切换器
    // 显示一个清单里不存在的名字，用户以为模型被换了。
    if let Some((pid, mid)) = cfg.model.as_deref().and_then(|d| d.split_once(':')) {
        if is_configured(pid)
            && !catalog_ids
                .iter()
                .any(|(p, m)| p == pid && m == mid)
        {
            catalog_ids.push((pid.to_string(), mid.to_string()));
        }
    }

    // `models` = 已启用（selector 权威；从未写过 selector 时退化为全量配置）。
    let enabled_ids: Vec<(String, String)> = cfg
        .model_selector
        .as_ref()
        .filter(|sel| !sel.enabled.is_empty() || sel.default_model.is_some())
        .map(|sel| {
            sel.enabled
                .iter()
                .filter(|e| is_configured(&e.provider_id))
                .map(|e| (e.provider_id.clone(), e.model_id.clone()))
                .collect::<Vec<_>>()
        })
        .filter(|models| !models.is_empty())
        .unwrap_or_else(|| catalog_ids.clone());

    // 默认模型：selector 的默认 > 顶层 model（须在启用清单里）> 首项。
    let in_enabled = |d: &str| {
        d.split_once(':')
            .map(|(p, m)| enabled_ids.iter().any(|(ep, em)| ep == p && em == m))
            .unwrap_or(false)
    };
    let default_model = cfg
        .model_selector
        .as_ref()
        .and_then(|s| s.default_model.clone())
        .filter(|d| in_enabled(d))
        .or_else(|| cfg.model.clone().filter(|d| in_enabled(d)))
        .or_else(|| {
            enabled_ids
                .first()
                .map(|(p, m)| format!("{p}:{m}"))
        });

    ModelListResponse {
        models: enabled_ids
            .iter()
            .map(|(p, m)| resolve_model_info(cfg, p, m))
            .collect(),
        default_model,
        catalog: catalog_ids
            .into_iter()
            .filter(|(p, m)| !enabled_ids.iter().any(|(ep, em)| ep == p && em == m))
            .map(|(p, m)| resolve_model_info(cfg, &p, &m))
            .collect(),
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

    /// 只配了 key、没写 `model` 的厂商必须出现在 `catalog` 里。
    /// 用户实测事故：内置免费渠道接管后，minimax（config 只有 api_key）
    /// 从模型页彻底消失，被当成"配置丢了"。
    #[test]
    fn catalog_lists_keyed_provider_without_model_field() {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "custom-free-kilo".to_string(),
            LlmProviderConfig {
                api_key: Some("none".to_string()),
                model: Some("openrouter/free".to_string()),
                ..Default::default()
            },
        );
        cfg.llm_providers.insert(
            "minimax".to_string(),
            LlmProviderConfig {
                api_key: Some("sk-test".to_string()),
                ..Default::default()
            },
        );
        cfg.model = Some("custom-free-kilo:openrouter/free".to_string());
        cfg.model_selector = Some(hermix_config::ModelSelectorConfig {
            enabled: vec![hermix_config::ModelSelectorEntry {
                provider_id: "custom-free-kilo".to_string(),
                model_id: "openrouter/free".to_string(),
            }],
            default_model: Some("custom-free-kilo:openrouter/free".to_string()),
        });

        let resp = build_model_list(&cfg);
        assert_eq!(resp.models.len(), 1, "只有 selector 里那一条是启用的");
        assert!(
            resp.catalog
                .iter()
                .any(|e| e.provider_id == "minimax" && e.model_id == "MiniMax-M3"),
            "未启用的付费厂商默认模型必须出现在可添加清单里，got {:?}",
            resp.catalog
        );
        assert!(
            !resp
                .catalog
                .iter()
                .any(|e| e.provider_id == "custom-free-kilo"),
            "已启用的模型不该重复出现在可添加清单"
        );
    }

    /// 上游探测到的能力必须原样带给前端：模型卡片据此显示上下文、
    /// 「思考 / 视觉」标签，`supports_reasoning` 还决定思考开关是否露出。
    #[test]
    fn model_list_exposes_upstream_capabilities() {
        let model = "minimax/minimax-m3.1-flash:free";
        let mut meta = std::collections::HashMap::new();
        meta.insert(
            model.to_string(),
            hermix_config::ModelCapabilities {
                context_window: Some(1_000_000),
                max_output_tokens: Some(524_288),
                supports_tools: Some(true),
                supports_reasoning: Some(true),
                supports_vision: Some(true),
                trains_on_prompts: true,
            },
        );
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "custom-free-kilo".to_string(),
            LlmProviderConfig {
                api_key: Some("none".to_string()),
                model: Some(model.to_string()),
                model_meta: meta,
                ..Default::default()
            },
        );
        cfg.model = Some(format!("custom-free-kilo:{model}"));

        let resp = build_model_list(&cfg);
        let e = resp.models.first().expect("one enabled model");
        assert_eq!(e.context_window, Some(1_000_000));
        assert_eq!(e.max_output_tokens, Some(524_288));
        assert_eq!(e.supports_reasoning, Some(true));
        assert_eq!(e.supports_vision, Some(true));
        assert_eq!(e.supports_tools, Some(true));
        assert!(e.is_free, "免费渠道要带标记");
        assert!(e.trains_on_prompts, "上游声明训练用途要透传");
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
