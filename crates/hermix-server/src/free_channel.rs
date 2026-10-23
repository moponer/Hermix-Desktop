//! 内置免费渠道的模型清单刷新（best-effort，不阻塞启动）。
//!
//! 上游 `/models` 是唯一可信来源：免费模型池变动频繁，静态清单会静默腐烂。
//! 刷新**只增不删**——上游一次抖动不该把用户已启用的模型从清单里抹掉；
//! 真正下线由模型实际调用失败暴露，而不是这里替用户删条目。
//!
//! 不引入 cron：启动后跑一次，设置页的手动刷新复用
//! `POST /api/custom-providers/{id}/fetch-models`。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hermix_config::FreeChannelConfig;
use hermix_core::traits::AgentService;

use crate::HttpServerState;

/// `/models` 响应的最小形状（OpenAI 兼容）。
#[derive(Debug, serde::Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct ModelEntry {
    id: String,
    #[serde(
        default,
        alias = "context_length",
        alias = "max_context_length",
        alias = "context_window",
        alias = "context_size",
        alias = "max_input_tokens",
        alias = "max_model_len"
    )]
    context_window: Option<u64>,
    /// 部分网关（Kilo）在同一条 `data` 里混着付费模型，用这个字段区分。
    /// 缺失 = 上游没标，按免费处理（其它网关的全部模型都是可用集合）。
    #[serde(default, rename = "isFree")]
    is_free: Option<bool>,
    /// OpenRouter 风格的"支持的请求参数"清单（`tools` / `reasoning` …）。
    #[serde(default)]
    supported_parameters: Vec<String>,
    /// 输入模态（`text` / `image` / `video` / `pdf`）。
    #[serde(default)]
    architecture: Option<Architecture>,
    #[serde(default)]
    top_provider: Option<TopProvider>,
    /// 上游声明会拿提示词训练。免费池几乎全部为真，UI 免责据此升级。
    #[serde(default, rename = "mayTrainOnYourPrompts")]
    trains_on_prompts: bool,
}

#[derive(Debug, Default, serde::Deserialize)]
struct Architecture {
    #[serde(default)]
    input_modalities: Vec<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct TopProvider {
    #[serde(default)]
    max_completion_tokens: Option<u64>,
}

/// 把上游条目翻译成持久化的能力描述。上游没报的字段一律留 `None`，
/// 不能把"没报"当成"不支持"——UI 会据此显示错误的能力标签。
fn capabilities_of(m: &ModelEntry) -> hermix_config::ModelCapabilities {
    let modalities: &[String] = m
        .architecture
        .as_ref()
        .map(|a| a.input_modalities.as_slice())
        .unwrap_or_default();
    let params_known = !m.supported_parameters.is_empty();
    let has = |k: &str| m.supported_parameters.iter().any(|p| p == k);
    hermix_config::ModelCapabilities {
        context_window: m.context_window.filter(|v| *v > 0),
        max_output_tokens: m
            .top_provider
            .as_ref()
            .and_then(|t| t.max_completion_tokens)
            .filter(|v| *v > 0),
        supports_tools: params_known.then(|| has("tools")),
        supports_reasoning: params_known.then(|| has("reasoning") || has("include_reasoning")),
        supports_vision: (!modalities.is_empty())
            .then(|| modalities.iter().any(|x| x == "image")),
        trains_on_prompts: m.trains_on_prompts,
    }
}

/// 一次刷新最多并入多少个模型：清单是给用户挑的，不是全量目录镜像。
/// 首版写 24 条把设置页刷成了目录墙，收到 8 条（seed 2 + 补 6）。
const MAX_REFRESH_MODELS: usize = 8;

/// 合并模型清单：只增不删，保序去重。返回合并后的 CSV 与新增数量。
pub fn merge_models(existing: Option<&str>, fetched: &[String]) -> (String, usize) {
    let mut out: Vec<String> = existing
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let mut added = 0usize;
    for id in fetched {
        if !out.iter().any(|m| m == id) {
            out.push(id.clone());
            added += 1;
        }
    }
    (out.join(","), added)
}

/// 拉一个渠道的模型清单。超时/网络失败都归为 `Err(String)`，由调用方静默降级。
async fn fetch_models(
    client: &reqwest::Client,
    models_url: &str,
    api_key: &str,
) -> Result<Vec<(String, hermix_config::ModelCapabilities)>, String> {
    let mut request = client.get(models_url);
    if let Some(value) = hermix_config::free_channel::auth_header(Some(api_key)) {
        request = request.header("Authorization", value);
    }
    let resp = request.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let text = resp.text().await.map_err(|e| e.to_string())?;
    let parsed: ModelsResponse = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(parsed
        .data
        .iter()
        .filter(|m| m.is_free != Some(false))
        .take(MAX_REFRESH_MODELS)
        .map(|m| (m.id.clone(), capabilities_of(m)))
        .collect())
}

/// 拉取全部渠道的清单并落盘（只增不删），随后热推送运行中的 agent。
///
/// 落盘前重新读一次 `config.yaml`：刷新是后台上跑的长任务，期间设置页
/// 可能已经改过配置，用启动时的快照覆盖会把用户的改动洗掉。
pub async fn refresh_and_persist(home: &std::path::Path, service: &Arc<dyn AgentService>) -> usize {
    let cfg = match hermix_config::load_config(home.to_str()) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "free channel: load config failed");
            return 0;
        }
    };
    if !cfg.free_channel.enabled {
        return 0;
    }

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let mut fetched: Vec<(&'static str, Vec<(String, hermix_config::ModelCapabilities)>)> =
        Vec::new();
    for channel in hermix_config::free_channel::FREE_CHANNELS {
        let models_url = hermix_config::free_channel::resolved_models_url(&cfg.free_channel, channel);
        match fetch_models(&client, &models_url, channel.api_key).await {
            Ok(list) => fetched.push((channel.id, list)),
            Err(e) => tracing::info!(
                channel = channel.id,
                error = %e,
                "free channel: models refresh skipped (keeping previous list)"
            ),
        }
    }
    if fetched.is_empty() {
        return 0;
    }

    let mut fresh = match hermix_config::load_config(home.to_str()) {
        Ok(c) => c,
        Err(_) => return 0,
    };
    let mut added_total = 0usize;
    let mut changed = false;
    for (id, list) in fetched {
        let Some(entry) = fresh.llm_providers.get_mut(id) else {
            continue;
        };
        let ids: Vec<String> = list.iter().map(|(m, _)| m.clone()).collect();
        let (merged, added) = merge_models(entry.model.as_deref(), &ids);
        if added > 0 {
            entry.model = Some(merged);
            changed = true;
        }
        for (model, caps) in list {
            // 上下文继续写进老字段 `model_contexts`：压缩阈值/上下文卡片读它。
            if let Some(ctx) = caps.context_window {
                if entry.model_contexts.insert(model.clone(), ctx) != Some(ctx) {
                    changed = true;
                }
            }
            // 其余能力（模态 / reasoning / tools / 最大输出）进 `model_meta`，
            // 供 `/api/model/list` 与 `/api/model/info` 展示与兜底。
            if entry.model_meta.get(&model) != Some(&caps) {
                entry.model_meta.insert(model, caps);
                changed = true;
            }
        }
        added_total += added;
    }

    // 开箱默认：把上下文最大的 N 个免费模型写进启用清单（一次性）。
    // 此时已拿到上游真实上下文，能选出真正的前 N 个并锁定，避免覆盖
    // 用户后续在设置页的手动启/停。
    if hermix_config::free_channel::seed_default_free_models(&mut fresh, true) {
        changed = true;
    }

    if !changed {
        return 0;
    }
    let path = home.join("config.yaml");
    if let Err(e) = hermix_config::save_config_yaml(&path, &fresh) {
        tracing::warn!(error = %e, "free channel: persist refreshed models failed");
        return 0;
    }
    if let Ok(v) = serde_json::to_value(&fresh) {
        service.set_config(v);
    }
    tracing::info!(added = added_total, "free channel: model list refreshed");
    added_total
}

/// 把确认 404 下架的模型从免费渠道清单里摘除（调用失败闭环的落地）。
///
/// 「只增不删」防的是上游网络抖动误删条目；而 404 `does not exist` 是
/// 上游明确的"这个 id 不存在"信号，不是抖动——不摘的话死模型会永远
/// 留在清单里，用户每次选中都 404（实测：`minimax/minimax-m3.1-flash:free`
/// 被上游下架后持续报错）。只动免费渠道的 provider entry，用户的自定义
/// 与内置清单不受影响。返回是否发生改动。
pub async fn remove_dead_model(
    home: &std::path::Path,
    service: &Arc<dyn AgentService>,
    model_id: &str,
) -> bool {
    let model_id = model_id.trim();
    if model_id.is_empty() {
        return false;
    }
    let mut fresh = match hermix_config::load_config(home.to_str()) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "free channel: dead-model removal load config failed");
            return false;
        }
    };
    if !fresh.free_channel.enabled {
        return false;
    }
    let mut changed = false;
    for channel in hermix_config::free_channel::FREE_CHANNELS {
        let Some(entry) = fresh.llm_providers.get_mut(channel.id) else {
            continue;
        };
        if let Some(csv) = entry.model.clone() {
            let present = csv.split(',').map(str::trim).any(|m| m == model_id);
            if present {
                let kept: Vec<&str> = csv
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty() && *s != model_id)
                    .collect();
                entry.model = Some(kept.join(","));
                changed = true;
            }
        }
        if entry.model_contexts.remove(model_id).is_some() {
            changed = true;
        }
        if entry.model_meta.remove(model_id).is_some() {
            changed = true;
        }
    }
    if !changed {
        return false;
    }
    let path = home.join("config.yaml");
    if let Err(e) = hermix_config::save_config_yaml(&path, &fresh) {
        tracing::warn!(error = %e, model = %model_id, "free channel: dead-model removal persist failed");
        return false;
    }
    if let Ok(v) = serde_json::to_value(&fresh) {
        service.set_config(v);
    }
    tracing::info!(model = %model_id, "free channel: dead model removed from list");
    true
}

/// 启动后延迟一小段再刷新：让首屏与首次推理先跑，别和启动竞争网络。
pub fn spawn_startup_refresh(state: &HttpServerState) {
    let cfg: FreeChannelConfig = state.config.free_channel.clone();
    if !cfg.enabled || !cfg.refresh_on_start {
        return;
    }
    let home: PathBuf = state.hermix_home.clone();
    let service = state.agent_service.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(20)).await;
        refresh_and_persist(&home, &service).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实联网验证清单刷新：无鉴权头、只收免费模型、条数受上限约束。
    /// 手工跑：`cargo test -p hermix-server --lib live_refresh -- --ignored`
    #[tokio::test]
    #[ignore = "network: hits api.kilo.ai (free tier)"]
    async fn live_refresh_reads_free_models() {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .expect("client");
        let list = fetch_models(
            &client,
            "https://api.kilo.ai/api/gateway/models",
            hermix_config::free_channel::KEY_NO_AUTH,
        )
        .await
        .expect("models fetch");

        assert!(list.len() <= MAX_REFRESH_MODELS);
        assert!(
            list.iter()
                .any(|(id, _)| id == "minimax/minimax-m3.1-flash:free"),
            "seed model must be present in the free slice; got {:?}",
            list.iter().map(|(id, _)| id).collect::<Vec<_>>()
        );
        assert!(
            list.iter().all(|(_, caps)| caps.context_window.is_some()),
            "上下文必须解析出来"
        );
        assert!(
            !list.iter().any(|(id, _)| id.contains("claude-opus")),
            "paid models must be filtered out (isFree == false)"
        );
    }

    /// 启动装配必须把免费渠道写进配置并落盘——这是"装上就能选免费模型"的
    /// 唯一入口；缺了它，前端模型页只会看到空白。
    #[tokio::test]
    async fn startup_seeds_free_channels_and_persists_them() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().to_path_buf();
        let state = crate::HttpServerState::build_with_hermes_home(
            hermix_config::GatewayConfig::default(),
            home.clone(),
        )
        .await
        .expect("build_with_hermes_home");

        let entry = state
            .config
            .llm_providers
            .get("custom-free-kilo")
            .expect("free channel seeded");
        assert_eq!(
            entry.api_key.as_deref(),
            Some(hermix_config::free_channel::KEY_NO_AUTH)
        );
        assert_eq!(entry.display_name.as_deref(), Some("免费模型 · Kilo"));
        assert!(entry
            .model
            .as_deref()
            .unwrap_or_default()
            .contains("openrouter/free"));

        let persisted = hermix_config::load_config(home.to_str()).expect("load persisted config");
        assert!(
            persisted.llm_providers.contains_key("custom-free-kilo"),
            "seeded provider must survive restart"
        );

        // 免费渠道若被选成默认模型，第二次启动的 auto-correct 不能改写它。
        if let Some(model) = persisted.model.as_deref() {
            if model.starts_with(hermix_config::free_channel::FREE_PROVIDER_PREFIX) {
                let mut again = persisted.clone();
                assert!(hermix_config::auto_correct_model(&mut again).is_none());
                assert_eq!(again.model.as_deref(), Some(model));
            }
        }
    }

    /// Kilo/OpenRouter 形状的 `/models` 条目 → 持久化能力。
    /// 上游没报的字段必须是 `None`（"没报"≠"不支持"）。
    #[test]
    fn capabilities_parse_openrouter_style_metadata() {
        let m: ModelEntry = serde_json::from_value(serde_json::json!({
            "id": "minimax/minimax-m3.1-flash:free",
            "context_length": 1_000_000,
            "isFree": true,
            "supported_parameters": ["max_tokens", "tools", "reasoning", "include_reasoning"],
            "architecture": { "input_modalities": ["text", "image", "video"] },
            "top_provider": { "max_completion_tokens": 524_288 },
            "mayTrainOnYourPrompts": true
        }))
        .expect("parse");

        let caps = capabilities_of(&m);
        assert_eq!(caps.context_window, Some(1_000_000));
        assert_eq!(caps.max_output_tokens, Some(524_288));
        assert_eq!(caps.supports_tools, Some(true));
        assert_eq!(caps.supports_reasoning, Some(true));
        assert_eq!(caps.supports_vision, Some(true));
        assert!(caps.trains_on_prompts);

        // 只报文本模态 → 视觉 false，但 reasoning/tools 仍未知（None）。
        let text_only: ModelEntry =
            serde_json::from_value(serde_json::json!({
                "id": "liquid/lfm-2.5-2.6b:free",
                "architecture": { "input_modalities": ["text"] }
            }))
            .expect("parse");
        let caps = capabilities_of(&text_only);
        assert_eq!(caps.supports_vision, Some(false));
        assert_eq!(caps.supports_reasoning, None);
        assert_eq!(caps.supports_tools, None);
        assert_eq!(caps.context_window, None);
    }

    #[test]
    fn merge_only_adds_and_keeps_order() {
        let (csv, added) = merge_models(Some("a,b"), &["b".into(), "c".into()]);
        assert_eq!(csv, "a,b,c");
        assert_eq!(added, 1);
    }

    #[test]
    fn merge_into_empty_and_blank_entries() {
        let (csv, added) = merge_models(None, &["x".into()]);
        assert_eq!(csv, "x");
        assert_eq!(added, 1);

        let (csv, added) = merge_models(Some(" , a ,"), &["a".into()]);
        assert_eq!(csv, "a");
        assert_eq!(added, 0);
    }

    #[test]
    fn merge_never_deletes_user_removed_upstream_entries() {
        // 上游这次只回 b，用户清单里的 a 必须留着。
        let (csv, _) = merge_models(Some("a"), &["b".into()]);
        assert_eq!(csv, "a,b");
    }
}
