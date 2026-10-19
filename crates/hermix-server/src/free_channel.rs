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
}

/// 一次刷新最多并入多少个模型：清单是给用户挑的，不是全量目录镜像。
const MAX_REFRESH_MODELS: usize = 24;

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
) -> Result<Vec<(String, Option<u64>)>, String> {
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
        .into_iter()
        .filter(|m| m.is_free != Some(false))
        .take(MAX_REFRESH_MODELS)
        .map(|m| (m.id, m.context_window.filter(|v| *v > 0)))
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

    let mut fetched: Vec<(&'static str, Vec<(String, Option<u64>)>)> = Vec::new();
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
        for (model, ctx) in list {
            if let Some(ctx) = ctx {
                // 只在探测值变化时算改动：模型集合没变但 context 更准时
                // 也要落盘，否则永远停留在旧值。
                if entry.model_contexts.insert(model, ctx) != Some(ctx) {
                    changed = true;
                }
            }
        }
        added_total += added;
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
            list.iter().any(|(id, _)| id == "openrouter/free"),
            "seed model must be present upstream"
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
