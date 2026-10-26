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
    /// 模型说明。社区免费网关的每条描述里都带着**逐条免责**
    /// （"Prompts may be logged by the upstream provider … not suitable for
    /// production or sensitive data"），落盘后 UI 才能把它摆给用户看。
    #[serde(default)]
    description: Option<String>,
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

/// 上游说明落盘时的长度上限（字符）。超了截断并加省略号。
const MAX_NOTE_CHARS: usize = 400;

/// 上游把免责/条款写在描述**末尾**（实测：`kilo-auto/free` 的描述是
/// "Rotates through available free models. … **Warning** Prompts may be
/// logged by the upstream provider …"）。UI 只显示一行，条款在末尾就等于
/// 被截掉 —— 所以命中这些标记时，把条款段挪到最前面。
///
/// 标记按上游的**原样大小写**匹配（实测就是这两种写法），不做大小写
/// 折叠：折叠后的字节偏移与原文对不上，切分位置会错。
const NOTE_MARKERS: &[&str] = &[
    "**Warning**",
    "**Terms of Service**",
    "**Terms of service**",
    "**Disclaimer**",
    "Warning:",
];

/// 拉下来的一条免费模型：id + 能力 + 上游说明（免责/条款）。
struct FetchedModel {
    id: String,
    caps: hermix_config::ModelCapabilities,
    /// 上游 `description`（压平空白、条款前置后截断）。
    note: Option<String>,
}

/// 上游 description → 可展示的短文本：压掉换行、条款前置、截断到上限。
fn note_of(raw: &str) -> Option<String> {
    let flat = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return None;
    }
    // 条款段挪到最前：命中最早的标记就以它切分，两段对调。
    let cut = NOTE_MARKERS
        .iter()
        .filter_map(|m| flat.find(m).map(|i| (i, m.len())))
        .min_by_key(|(i, _)| *i);
    let ordered = match cut {
        Some((idx, len)) => {
            let (head, tail) = flat.split_at(idx);
            let (marker, rest) = tail.split_at(len);
            format!("{marker}{rest} {head}")
        }
        None => flat,
    };
    let mut out: String = ordered.chars().take(MAX_NOTE_CHARS).collect();
    if ordered.chars().count() > MAX_NOTE_CHARS {
        out.push('…');
    }
    // 去掉 Markdown 粗体标记：`**Warning**` → `Warning`。上游描述里的 `**`
    // 只用于粗体，直接显示给用户是"原始转义"的样子。
    Some(out.replace("**", ""))
}

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
) -> Result<Vec<FetchedModel>, String> {
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
        .map(|m| FetchedModel {
            id: m.id.clone(),
            caps: capabilities_of(m),
            note: m.description.as_deref().and_then(note_of),
        })
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

    let mut fetched: Vec<(&'static str, Vec<FetchedModel>)> = Vec::new();
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
        let ids: Vec<String> = list.iter().map(|m| m.id.clone()).collect();
        let (merged, added) = merge_models(entry.model.as_deref(), &ids);
        if added > 0 {
            entry.model = Some(merged);
            changed = true;
        }
        for model in list {
            let caps = model.caps;
            // 上下文继续写进老字段 `model_contexts`：压缩阈值/上下文卡片读它。
            if let Some(ctx) = caps.context_window {
                if entry.model_contexts.insert(model.id.clone(), ctx) != Some(ctx) {
                    changed = true;
                }
            }
            // 其余能力（模态 / reasoning / tools / 最大输出）进 `model_meta`，
            // 供 `/api/model/list` 与 `/api/model/info` 展示与兜底。
            if entry.model_meta.get(&model.id) != Some(&caps) {
                entry.model_meta.insert(model.id.clone(), caps);
                changed = true;
            }
            // 上游说明（逐条免责/使用条款）进 `model_notes`，设置页摆给用户看。
            if let Some(note) = model.note {
                if entry.model_notes.get(&model.id) != Some(&note) {
                    entry.model_notes.insert(model.id.clone(), note);
                    changed = true;
                }
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

/// 把确认失效的模型从免费渠道清单里摘除（调用失败闭环的落地）。
///
/// 「只增不删」防的是上游网络抖动误删条目；而 **404 `does not exist`**
/// （上游明确说这个 id 不存在）与 **401**（无鉴权免费路由被拒绝访问 =
/// 路由已死）都是明确信号，不是抖动——不摘的话死模型会永远留在清单里，
/// 用户每次选中都失败（实测：`minimax/minimax-m3.1-flash:free` 被上游
/// 下架后持续报错；401 同理）。只动免费渠道的 provider entry，用户的
/// 自定义与内置清单不受影响。
///
/// 摘除范围是**三处一起清**（2026-10-25 补齐）：provider 的模型目录
/// （`model` CSV / contexts / meta）、`model_selector.enabled`、以及指向
/// 它的默认模型。此前只清目录不清 enabled——`build_model_list` 的启用
/// 清单不按目录过滤，于是死模型从「可添加」里消失了、却永远留在聊天
/// 下拉里：用户看到的就是「明明 401/404 了，列表里一直在」。
/// 返回是否发生改动。
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
    // 该模型归属的免费 provider（enabled / 默认模型的清理范围按它限定）。
    let mut owning: Vec<String> = Vec::new();
    for channel in hermix_config::free_channel::FREE_CHANNELS {
        let Some(entry) = fresh.llm_providers.get_mut(channel.id) else {
            continue;
        };
        let mut owned_here = false;
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
                owned_here = true;
            }
        }
        if entry.model_contexts.remove(model_id).is_some() {
            changed = true;
            owned_here = true;
        }
        if entry.model_meta.remove(model_id).is_some() {
            changed = true;
            owned_here = true;
        }
        if owned_here {
            owning.push(channel.id.to_string());
        }
    }
    if !changed {
        return false;
    }

    // enabled 与默认模型：只清理刚确认属于免费渠道的 provider 条目，
    // 同名模型挂在用户自定义 provider 上的一概不动。
    // （此前提返回后 `changed` 已无读者，后续改动无需再回写它。）
    let is_owned = |pid: &str| owning.iter().any(|o| o == pid);
    if let Some(sel) = fresh.model_selector.as_mut() {
        sel.enabled
            .retain(|e| !(is_owned(&e.provider_id) && e.model_id == model_id));
        if sel
            .default_model
            .as_deref()
            .and_then(|d| d.split_once(':'))
            .map(|(p, m)| is_owned(p) && m == model_id)
            .unwrap_or(false)
        {
            // 默认模型死了 → 指到剩余启用清单的第一条（build_model_list
            // 同一口径），没有剩余就清空。
            sel.default_model = sel
                .enabled
                .first()
                .map(|e| format!("{}:{}", e.provider_id, e.model_id));
        }
    }
    // 顶层 `model`（正在跑的模型）指向死模型时一并重指，否则
    // build_model_list 会把它当孤儿又加回目录。
    let model_dead = fresh
        .model
        .as_deref()
        .and_then(|m| m.split_once(':'))
        .map(|(p, m)| is_owned(p) && m == model_id)
        .unwrap_or(false);
    if model_dead {
        fresh.model = fresh
            .model_selector
            .as_ref()
            .and_then(|s| s.default_model.clone());
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

/// 免费渠道总开关的落地结果（设置页回显用）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct FreeChannelToggle {
    /// 落盘后的开关状态（权威值）。
    pub enabled: bool,
    /// 关掉时从配置里摘掉的免费模型条数（provider 目录里的模型总数）。
    pub removed_models: usize,
    /// 打开时写进启用清单的免费模型条数。
    pub seeded_models: usize,
    /// 摘除/重指之后的默认模型（None = 当前没有可用默认）。
    pub default_model: Option<String>,
}

/// 免费渠道总开关：**立即**生效的开关动作。
///
/// 与启动路径（`state.rs` 里的 `strip` / `ensure_seeded`）用同一套函数，
/// 区别只在**不用等重启**：
/// - 关 → 摘除已拉取的免费条目（provider 目录 + 启用清单 + 默认模型），
///   启动阶段就不该再有免费模型，也不该再拉；
/// - 开 → 重建渠道条目并把「上下文最大的 N 个免费模型」写回启用清单，
///   随后**后台**拉一次社区清单（网络 IO 不阻塞响应）。
///
/// 落盘后立刻热推送（`set_config`），让运行中的 agent 与前端下拉同步。
pub async fn set_enabled(
    home: &std::path::Path,
    service: &Arc<dyn AgentService>,
    enabled: bool,
) -> Result<FreeChannelToggle, String> {
    let mut cfg = hermix_config::load_config(home.to_str())
        .map_err(|e| format!("读取配置失败: {e}"))?;
    let (removed_models, seeded_models) = apply_switch(&mut cfg, enabled);

    let path = home.join("config.yaml");
    hermix_config::save_config_yaml(&path, &cfg).map_err(|e| format!("写配置失败: {e}"))?;
    if let Ok(v) = serde_json::to_value(&cfg) {
        service.set_config(v);
    }

    // 开：后台拉一次社区清单（失败静默，保留 seed 清单）。
    if enabled {
        let home = home.to_path_buf();
        let service = service.clone();
        tokio::spawn(async move {
            refresh_and_persist(&home, &service).await;
        });
    }

    tracing::info!(
        enabled,
        removed_models,
        seeded_models,
        "free channel: global switch toggled"
    );
    Ok(FreeChannelToggle {
        enabled,
        removed_models,
        seeded_models,
        default_model: cfg
            .model_selector
            .as_ref()
            .and_then(|s| s.default_model.clone()),
    })
}

/// 开关的**配置层**改动（纯函数，可测）：返回 (摘除条数, 写回启用清单条数)。
///
/// - 关 → 摘除已拉取的免费条目（provider 目录 + 启用清单 + 默认模型），
///   之后启动阶段不再 seed，也不该再拉；
/// - 开 → 重建渠道条目并把「上下文最大的 N 个免费模型」写回启用清单。
pub fn apply_switch(
    cfg: &mut hermix_config::GatewayConfig,
    enabled: bool,
) -> (usize, usize) {
    cfg.free_channel.enabled = enabled;
    if enabled {
        // 重开：清掉「一次性」标记，让 Top-N 重新进启用清单 ——
        // 否则重开后只剩目录条目、聊天下拉里一条都没有。
        cfg.free_channel.defaults_seeded = false;
        hermix_config::free_channel::ensure_seeded(cfg);
        let seeded = cfg
            .model_selector
            .as_ref()
            .map(|s| {
                s.enabled
                    .iter()
                    .filter(|e| hermix_config::free_channel::is_free_provider(&e.provider_id))
                    .count()
            })
            .unwrap_or(0);
        (0, seeded)
    } else {
        let removed = count_free_models(cfg);
        hermix_config::free_channel::strip(cfg);
        // `strip` 把默认模型清空了：若还有别的已启用模型就指到第一条，
        // 否则用户会被留在「无模型可用」的状态（他只是关了免费源）。
        repoint_default(cfg);
        (removed, 0)
    }
}

/// 当前已登记的免费模型条数（provider 目录里的模型总数）。
fn count_free_models(cfg: &hermix_config::GatewayConfig) -> usize {
    cfg.llm_providers
        .iter()
        .filter(|(id, _)| hermix_config::free_channel::is_free_provider(id))
        .map(|(_, entry)| {
            entry
                .model
                .as_deref()
                .map(|csv| csv.split(',').filter(|m| !m.trim().is_empty()).count())
                .unwrap_or(0)
        })
        .sum()
}

/// 默认模型被摘空时，指到剩余启用清单的第一条。
fn repoint_default(cfg: &mut hermix_config::GatewayConfig) {
    let picked = cfg.model_selector.as_ref().and_then(|sel| {
        if sel
            .default_model
            .as_deref()
            .is_some_and(|d| !d.trim().is_empty())
        {
            return None; // 已有默认，不动
        }
        sel.enabled
            .first()
            .map(|e| format!("{}:{}", e.provider_id, e.model_id))
    });
    let Some(picked) = picked else { return };
    if let Some(sel) = cfg.model_selector.as_mut() {
        sel.default_model = Some(picked.clone());
    }
    if cfg.model.as_deref().is_none_or(|m| m.trim().is_empty()) {
        cfg.model = Some(picked);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一份「已拉取免费模型」的配置：免费 provider + 启用清单 + 默认模型。
    fn pulled_free_config() -> hermix_config::GatewayConfig {
        let mut cfg = hermix_config::GatewayConfig::default();
        hermix_config::free_channel::ensure_seeded(&mut cfg);
        cfg.free_channel.defaults_seeded = true;
        cfg.model = Some("custom-free-kilo:kilo-auto/free".to_string());
        cfg.model_selector = Some(hermix_config::ModelSelectorConfig {
            enabled: vec![hermix_config::ModelSelectorEntry {
                provider_id: "custom-free-kilo".to_string(),
                model_id: "kilo-auto/free".to_string(),
            }],
            default_model: Some("custom-free-kilo:kilo-auto/free".to_string()),
        });
        cfg
    }

    /// 关掉总开关必须**立即**摘除已拉取的免费条目（目录 / 启用清单 / 默认模型）——
    /// 启动路径的 `strip` 只在进程起来时跑一次，等重启等于开关无效。
    #[test]
    fn switch_off_strips_pulled_free_models_immediately() {
        let mut cfg = pulled_free_config();
        let (removed, seeded) = apply_switch(&mut cfg, false);

        assert!(!cfg.free_channel.enabled);
        assert!(removed > 0, "应报告摘除的模型条数");
        assert_eq!(seeded, 0);
        assert!(
            !cfg.llm_providers.contains_key("custom-free-kilo"),
            "免费 provider 必须被摘除"
        );
        let sel = cfg.model_selector.as_ref().expect("selector");
        assert!(
            sel.enabled
                .iter()
                .all(|e| !hermix_config::free_channel::is_free_provider(&e.provider_id)),
            "启用清单里不该剩免费条目"
        );
        assert!(
            cfg.model
                .as_deref()
                .is_none_or(|m| !m.starts_with("custom-free-")),
            "正在跑的模型不该再指向已摘除的免费 provider"
        );
    }

    /// 关掉时若还有别的已启用模型，默认模型要顺延过去 —— 用户只是关了
    /// 免费源，不该被留在「无模型可用」的状态。
    #[test]
    fn switch_off_repoints_default_to_remaining_enabled_model() {
        let mut cfg = pulled_free_config();
        let sel = cfg.model_selector.as_mut().expect("selector");
        sel.enabled.push(hermix_config::ModelSelectorEntry {
            provider_id: "deepseek".to_string(),
            model_id: "deepseek-chat".to_string(),
        });
        cfg.model = Some("custom-free-kilo:kilo-auto/free".to_string());

        apply_switch(&mut cfg, false);

        let sel = cfg.model_selector.as_ref().expect("selector");
        assert_eq!(
            sel.default_model.as_deref(),
            Some("deepseek:deepseek-chat"),
            "默认模型必须顺延到剩余启用项"
        );
        assert_eq!(cfg.model.as_deref(), Some("deepseek:deepseek-chat"));
    }

    /// 打开总开关：渠道条目重建，且免费模型回到启用清单（否则重开后
    /// 聊天下拉里一条都没有，只剩设置页的目录行）。
    #[test]
    fn switch_on_reseeds_free_models_into_enabled_list() {
        let mut cfg = pulled_free_config();
        apply_switch(&mut cfg, false);
        assert!(!cfg.llm_providers.contains_key("custom-free-kilo"));

        let (removed, seeded) = apply_switch(&mut cfg, true);

        assert!(cfg.free_channel.enabled);
        assert_eq!(removed, 0);
        assert!(seeded > 0, "重开应把免费模型写回启用清单");
        assert!(cfg.llm_providers.contains_key("custom-free-kilo"));
        let sel = cfg.model_selector.as_ref().expect("selector");
        assert!(
            sel.enabled
                .iter()
                .any(|e| hermix_config::free_channel::is_free_provider(&e.provider_id)),
            "启用清单里必须重新出现免费条目"
        );
    }

    /// 上游说明落盘前的整形：压掉换行（上游描述是多行 Markdown）、截断到
    /// 上限（有的条目几百字，不截会把 config.yaml 撑大）。
    #[test]
    fn upstream_note_is_flattened_and_truncated() {
        assert_eq!(note_of("  hello \n world  ").as_deref(), Some("hello world"));
        assert_eq!(note_of("   \n  ").as_deref(), None);
        let long = "x".repeat(MAX_NOTE_CHARS + 50);
        let out = note_of(&long).expect("note");
        assert_eq!(out.chars().count(), MAX_NOTE_CHARS + 1, "截断后带省略号");
        assert!(out.ends_with('…'));
    }

    /// 条款必须**排到最前**：上游把免责写在描述末尾，而 UI 只显示一行 ——
    /// 不前置就正好被截断掉，等于没写。
    #[test]
    fn upstream_note_puts_terms_before_description() {
        let raw = "Rotates through available free models. **Warning** Prompts may be logged \
                   by the upstream provider and used to improve their services. Not suitable \
                   for production or sensitive data workloads.";
        let note = note_of(raw).expect("note");
        assert!(note.starts_with("Warning"), "条款必须排到最前：{note}");
        assert!(
            note.contains("Not suitable for production"),
            "条款正文必须完整保留：{note}"
        );
        assert!(!note.contains("**"), "粗体标记要清掉：{note}");
    }

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
            list.iter().any(|m| m.id == "minimax/minimax-m3.1-flash:free"),
            "seed model must be present in the free slice; got {:?}",
            list.iter().map(|m| &m.id).collect::<Vec<_>>()
        );
        assert!(
            list.iter().all(|m| m.caps.context_window.is_some()),
            "上下文必须解析出来"
        );
        assert!(
            !list.iter().any(|m| m.id.contains("claude-opus")),
            "paid models must be filtered out (isFree == false)"
        );
        // 上游说明（免责/条款）必须跟着拉下来 —— 不落盘就等于用户看不到
        // 上游条款（实测每条免费模型都带 **Warning** 段）。
        assert!(
            list.iter().any(|m| m.note.is_some()),
            "免费模型条目应带上游说明（免责/条款）"
        );
    }

    /// 摘除必须三处一起清：目录（model CSV / contexts / meta）、
    /// `model_selector.enabled`、以及指向死模型的默认模型。此前只清目录
    /// ——`build_model_list` 的启用清单不按目录过滤，死模型于是从「可添加」
    /// 消失、却永远留在聊天下拉里（用户实测「明明 401，列表一直在」）。
    #[tokio::test]
    async fn dead_model_removal_clears_catalog_enabled_and_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().to_path_buf();
        let state = crate::HttpServerState::build_with_hermes_home(
            hermix_config::GatewayConfig::default(),
            home.clone(),
        )
        .await
        .expect("build_with_hermes_home");

        // 死模型挂在免费渠道且是默认；付费条目（deepseek）作对照——同轮
        // 摘除不得误伤。
        let mut cfg = hermix_config::load_config(home.to_str()).expect("config");
        let entry = cfg
            .llm_providers
            .get_mut("custom-free-kilo")
            .expect("free channel seeded");
        entry.model = Some("dead-route,alive-route".to_string());
        entry.model_contexts.insert("dead-route".to_string(), 1000);
        cfg.model_selector = Some(hermix_config::ModelSelectorConfig {
            enabled: vec![
                hermix_config::ModelSelectorEntry {
                    provider_id: "custom-free-kilo".to_string(),
                    model_id: "dead-route".to_string(),
                },
                hermix_config::ModelSelectorEntry {
                    provider_id: "custom-free-kilo".to_string(),
                    model_id: "alive-route".to_string(),
                },
                hermix_config::ModelSelectorEntry {
                    provider_id: "deepseek".to_string(),
                    model_id: "chat".to_string(),
                },
            ],
            default_model: Some("custom-free-kilo:dead-route".to_string()),
        });
        cfg.model = Some("custom-free-kilo:dead-route".to_string());
        hermix_config::save_config_yaml(&home.join("config.yaml"), &cfg).expect("save");

        assert!(remove_dead_model(&home, &state.agent_service, "dead-route").await);

        let after = hermix_config::load_config(home.to_str()).expect("reload");
        let entry = after.llm_providers.get("custom-free-kilo").unwrap();
        assert!(
            !entry.model.as_deref().unwrap().contains("dead-route"),
            "目录里必须摘掉"
        );
        let sel = after.model_selector.as_ref().unwrap();
        assert!(
            !sel.enabled.iter().any(|e| e.model_id == "dead-route"),
            "启用清单必须摘掉，got {:?}",
            sel.enabled
        );
        assert!(
            sel.enabled
                .iter()
                .any(|e| e.provider_id == "deepseek" && e.model_id == "chat"),
            "付费条目不受影响"
        );
        assert_eq!(
            sel.default_model.as_deref(),
            Some("custom-free-kilo:alive-route"),
            "默认模型重指到剩余启用首条"
        );
        assert_eq!(
            after.model.as_deref(),
            Some("custom-free-kilo:alive-route"),
            "顶层运行模型同步重指"
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
