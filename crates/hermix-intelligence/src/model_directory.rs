//! 模型目录：从 OpenRouter 的公开模型接口取**权威上下文长度**。
//!
//! ## 为什么需要它
//!
//! 上下文长度的解析链原来是「用户覆盖 → 静态家族表 → 128K 兜底」，
//! 中间那层是**按家族前缀猜的**：`("minimax", 204_800)` 在 M3.1（实际
//! 1M）上仍然生效，于是模型按 1M 干活、项目按 205K 触发压缩
//! （2026-10-23 实测）。`models.dev` 的客户端（`models_dev/`）早就存在，
//! 但**全仓没有生产调用点**——只在测试里用过，等于数据源有、线没接。
//!
//! `GET https://openrouter.ai/api/v1/models` 是**免 API key** 的公开目录，
//! 每个条目带 `context_length`（模型级上限）与
//! `top_provider.context_length`（实际可用窗口，可能更小）。实测
//! `minimax/minimax-m3.1-flash` 两者都是 1,000,000。
//!
//! ## 设计取舍
//!
//! - **同步可查**：解析链（`get_model_context_length_with_source`）是同步
//!   函数、每一轮都会调，不能等网络。所以查询只读内存快照；快照来自磁盘
//!   缓存（启动时 `load_from_disk`）与后台 `refresh`。
//! - **失败即忽略**：目录拿不到就退回静态表——它只是"更准的一层"，不是
//!   依赖。任何 IO / 解析错误都只记 debug。
//! - **名字归一化匹配**：配置里的型号（`MiniMax-M3.1-Flash`）与目录 id
//!   （`minimax/minimax-m3.1-flash`）格式不同，比较前统一小写并只留
//!   字母数字，再用后缀匹配（目录 id 通常带厂商前缀）。
//! - **取两者较小值**：模型级上限与实际可用窗口不一致时（实测
//!   `nvidia/nemotron-3.5-super-vl` 是 1M vs 256K），按小的算——估大
//!   会让压缩推迟到 API 报 400，估小只是早压缩，代价不对称。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, SystemTime};

use serde_json::Value;
use tracing::debug;

/// 公开目录，无需认证。
pub const OPENROUTER_MODELS_URL: &str = "https://openrouter.ai/api/v1/models";

/// 缓存文件名（与 `models_dev_cache.json` 并列，互不干扰）。
const CACHE_FILE: &str = "openrouter_models_cache.json";

/// 缓存有效期。模型上下文的变动以周为单位，一天一刷足够。
const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// 目录里的一条：上下文与输出上限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelLimits {
    pub context: u64,
    pub max_output: Option<u64>,
}

fn store() -> &'static RwLock<HashMap<String, ModelLimits>> {
    static STORE: OnceLock<RwLock<HashMap<String, ModelLimits>>> = OnceLock::new();
    STORE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// provider 级真实限额（`/models/{id}/endpoints` 的一条）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointLimits {
    /// 上游 provider 名（`Minimax` / `Anthropic` / `Bedrock` …）。
    pub provider: String,
    /// 该 provider 实际提供的上下文窗口。
    pub context: u64,
    /// 该 provider 实际允许的最大输出。
    pub max_output: Option<u64>,
}

/// `endpoints` 快照：归一化模型名 → 各 provider 的限额。
///
/// 为什么需要它（而不是只用 `/models` 的聚合值）：`/models` 的
/// `top_provider.context_length` 是"某个 provider 的窗口"，而同一模型
/// 可能被多家托管、窗口各不相同。我们直连的是特定一家，聚合值可能给
/// 出别家的数字——估大就会被上游 400。
fn endpoint_store() -> &'static RwLock<HashMap<String, Vec<EndpointLimits>>> {
    static STORE: OnceLock<RwLock<HashMap<String, Vec<EndpointLimits>>>> = OnceLock::new();
    STORE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// 归一化模型名 → OpenRouter 的完整 id（拉 endpoints 要用原样 id）。
fn id_index() -> &'static RwLock<HashMap<String, String>> {
    static IDX: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();
    IDX.get_or_init(|| RwLock::new(HashMap::new()))
}

/// 归一化：小写 + 只保留 ASCII 字母数字。
///
/// `MiniMax-M3.1-Flash` → `minimaxm31flash`
/// `minimax/minimax-m3.1-flash` → `minimaxminimaxm31flash`
fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// 缓存路径：`<HERMIX_HOME>/openrouter_models_cache.json`。
pub fn cache_path() -> PathBuf {
    hermix_config::paths::hermix_home().join(CACHE_FILE)
}

/// 解析目录响应（`{"data": [ {id, context_length, top_provider:{...}} ]}`）。
///
/// 纯函数，便于测试。返回归一化 id → 限额。
pub fn parse_catalog(v: &Value) -> HashMap<String, ModelLimits> {
    let mut out = HashMap::new();
    let Some(items) = v.get("data").and_then(|d| d.as_array()) else {
        return out;
    };
    for item in items {
        let Some(id) = item.get("id").and_then(|x| x.as_str()) else {
            continue;
        };
        let model_ctx = item.get("context_length").and_then(|x| x.as_u64());
        let top = item.get("top_provider");
        let provider_ctx = top
            .and_then(|t| t.get("context_length"))
            .and_then(|x| x.as_u64())
            .filter(|c| *c > 0);
        // **模型级优先**，不是取 min（2026-10-23 修正）。
        //
        // `context_length` 是"该模型支持的最大窗口"；`top_provider` 只是
        // "某一家 provider 的实际窗口"。取 min 会把小 provider 当成全局：
        // 实测 `minimax/minimax-m3` 模型级 1,048,576、某家 524,288 ——
        // 取 min 会把 M3 显示成 512K（再被静态表兜成 205K），而用户直连
        // 官方拿到的就是 1M。
        //
        // provider 精确值由 endpoints 层负责：它带 provider 名字，能按我们
        // 实际用的那家匹配（见 `lookup_limits`）。
        let ctx_opt = model_ctx.or(provider_ctx);
        let Some(ctx) = ctx_opt else {
            continue;
        };
        if ctx == 0 {
            continue;
        }
        let max_output = top
            .and_then(|t| t.get("max_completion_tokens"))
            .and_then(|x| x.as_u64())
            .filter(|c| *c > 0);
        out.insert(
            norm(id),
            ModelLimits {
                context: ctx,
                max_output,
            },
        );
    }
    out
}

/// 解析 per-model endpoints 响应
/// （`{"data": {"endpoints": [{provider_name, context_length, max_completion_tokens}]}}`）。
///
/// 纯函数，便于测试。
pub fn parse_endpoints(v: &Value) -> Vec<EndpointLimits> {
    let Some(eps) = v.pointer("/data/endpoints").and_then(|x| x.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ep in eps {
        let Some(context) = ep.get("context_length").and_then(|x| x.as_u64()) else {
            continue;
        };
        if context == 0 {
            continue;
        }
        let provider = ep
            .get("provider_name")
            .and_then(|x| x.as_str())
            .unwrap_or("unknown")
            .to_string();
        let max_output = ep
            .get("max_completion_tokens")
            .and_then(|x| x.as_u64())
            .filter(|c| *c > 0);
        out.push(EndpointLimits {
            provider,
            context,
            max_output,
        });
    }
    out
}

/// 在 endpoints 快照里找这个型号（精确 → 后缀匹配）。
fn resolve_endpoints<'a>(
    guard: &'a HashMap<String, Vec<EndpointLimits>>,
    key: &str,
) -> Option<&'a Vec<EndpointLimits>> {
    if let Some(v) = guard.get(key) {
        return Some(v);
    }
    guard
        .iter()
        .filter(|(k, _)| k.ends_with(key) || key.ends_with(k.as_str()))
        .max_by_key(|(k, _)| k.len())
        .map(|(_, v)| v)
}

/// 查型号限额：**provider 级真实值优先**，其次目录聚合值。
///
/// `provider_hint` 是配置里的 provider id（`minimax` / `anthropic`…）。
/// 命中该 provider 的 endpoint 就用它；没注明或没命中时，取 endpoints 里
/// **上下文最大**的那条（与我们"直连官方"的常见形态一致，且不会因为
/// 取到小窗口而过早压缩）。
pub fn lookup_limits(model: &str, provider_hint: Option<&str>) -> Option<ModelLimits> {
    let key = norm(model);
    if key.is_empty() {
        return None;
    }

    // 1. provider 级（endpoints 快照）。
    if let Ok(guard) = endpoint_store().read() {
        if let Some(list) = resolve_endpoints(&guard, &key) {
            let picked = provider_hint
                .map(norm)
                .and_then(|hint| {
                    list.iter()
                        .find(|e| {
                            let p = norm(&e.provider);
                            !p.is_empty() && (p.contains(&hint) || hint.contains(&p))
                        })
                        .cloned()
                })
                .or_else(|| list.iter().max_by_key(|e| e.context).cloned());
            if let Some(e) = picked {
                return Some(ModelLimits {
                    context: e.context,
                    max_output: e.max_output,
                });
            }
        }
    }

    // 2. 目录聚合值。
    lookup_aggregate(&key)
}

/// 只查聚合值（`/models` 的 `context_length` / `top_provider`）。
fn lookup_aggregate(key: &str) -> Option<ModelLimits> {
    let guard = store().read().ok()?;
    if let Some(hit) = guard.get(key) {
        return Some(*hit);
    }
    guard
        .iter()
        .filter(|(k, _)| k.ends_with(key) || key.ends_with(k.as_str()))
        .max_by_key(|(k, _)| k.len())
        .map(|(_, v)| *v)
}

/// 兼容旧调用点：不带 provider hint 的查询。
pub fn lookup(model: &str) -> Option<ModelLimits> {
    lookup_limits(model, None)
}

/// 把请求的输出上限钳到模型**真实**上限。
///
/// 上下文窗口是 `prompt + max_tokens` 的总和：配置里写了超过模型上限的
/// `max_tokens`，请求会被上游直接 400，而错误通常只说 "invalid
/// max_tokens"，看不出是配置问题。目录没收录时原样返回（不猜）。
pub fn clamp_output_tokens(model: &str, requested: Option<u32>) -> Option<u32> {
    let req = requested?;
    let cap = lookup(model)
        .and_then(|l| l.max_output)
        .and_then(|v| u32::try_from(v).ok())
        .filter(|c| *c > 0);
    match cap {
        Some(c) => Some(req.min(c)),
        None => Some(req),
    }
}

/// 拉取并缓存单个模型的 provider 级 endpoints。
///
/// 需要 OpenRouter 的完整 id（`minimax/minimax-m3.1-flash`）——从目录快照的
/// id 反查；查不到就直接用传进来的值试。
pub async fn refresh_endpoints(http: &reqwest::Client, openrouter_id: &str) -> usize {
    let url = format!("{OPENROUTER_MODELS_URL}/{openrouter_id}/endpoints");
    let resp = match http.get(&url).send().await {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            debug!(status = %r.status(), id = openrouter_id, "endpoints: non-success");
            return 0;
        }
        Err(e) => {
            debug!(id = openrouter_id, "endpoints: fetch failed: {e}");
            return 0;
        }
    };
    let v = match resp.json::<Value>().await {
        Ok(v) => v,
        Err(e) => {
            debug!(id = openrouter_id, "endpoints: decode failed: {e}");
            return 0;
        }
    };
    let list = parse_endpoints(&v);
    if list.is_empty() {
        debug!(id = openrouter_id, "endpoints: no usable provider entries");
        return 0;
    }
    let n = list.len();
    let key = norm(openrouter_id);
    if let Ok(mut guard) = endpoint_store().write() {
        guard.insert(key.clone(), list);
    }
    persist_endpoints();
    n
}

/// 目录里这个型号的 OpenRouter 完整 id（拉 endpoints 用）。
pub fn openrouter_id_for(model: &str) -> Option<String> {
    let key = norm(model);
    let guard = id_index().read().ok()?;
    if let Some(id) = guard.get(&key) {
        return Some(id.clone());
    }
    guard
        .iter()
        .filter(|(k, _)| k.ends_with(&key) || key.ends_with(k.as_str()))
        .max_by_key(|(k, _)| k.len())
        .map(|(_, id)| id.clone())
}

/// endpoints 磁盘缓存路径。
pub fn endpoints_cache_path() -> PathBuf {
    hermix_config::paths::hermix_home().join("openrouter_endpoints_cache.json")
}

/// 把 endpoints 快照落盘（失败只记 debug——它只是缓存）。
fn persist_endpoints() {
    let Ok(guard) = endpoint_store().read() else {
        return;
    };
    let map: serde_json::Map<String, Value> = guard
        .iter()
        .map(|(k, v)| {
            let arr: Vec<Value> = v
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "provider": e.provider,
                        "context": e.context,
                        "max_output": e.max_output,
                    })
                })
                .collect();
            (k.clone(), Value::Array(arr))
        })
        .collect();
    let payload = Value::Object(map);
    if let Ok(text) = serde_json::to_string(&payload) {
        if let Err(e) = std::fs::write(endpoints_cache_path(), text) {
            debug!("endpoints cache write failed: {e}");
        }
    }
}

/// 启动时装载 endpoints 缓存（与目录缓存并列）。
pub fn load_endpoints_from_disk() -> usize {
    let path = endpoints_cache_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return 0;
    };
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) else {
        return 0;
    };
    let mut guard = match endpoint_store().write() {
        Ok(g) => g,
        Err(_) => return 0,
    };
    guard.clear();
    for (key, val) in map {
        let Some(arr) = val.as_array() else { continue };
        let list: Vec<EndpointLimits> = arr
            .iter()
            .filter_map(|e| {
                let context = e.get("context")?.as_u64()?;
                if context == 0 {
                    return None;
                }
                Some(EndpointLimits {
                    provider: e
                        .get("provider")
                        .and_then(|x| x.as_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    context,
                    max_output: e.get("max_output").and_then(|x| x.as_u64()),
                })
            })
            .collect();
        if !list.is_empty() {
            guard.insert(key, list);
        }
    }
    guard.len()
}

/// 同步装载磁盘缓存（启动时调）。过期与否都装——旧的也比静态表准。
pub fn load_from_disk() -> usize {
    let path = cache_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return 0;
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        debug!(?path, "openrouter catalog cache is not valid JSON; ignoring");
        return 0;
    };
    let parsed = parse_catalog(&v);
    let n = parsed.len();
    fill_id_index(&v);
    if let Ok(mut guard) = store().write() {
        *guard = parsed;
    }
    n
}

/// 把目录里的 `归一化名 → 原始 id` 记下来（拉 endpoints 要用原样 id）。
fn fill_id_index(v: &Value) {
    let Some(items) = v.get("data").and_then(|d| d.as_array()) else {
        return;
    };
    let Ok(mut idx) = id_index().write() else {
        return;
    };
    for item in items {
        if let Some(id) = item.get("id").and_then(|x| x.as_str()) {
            idx.insert(norm(id), id.to_string());
        }
    }
}

/// 缓存是否已过期（文件不存在也算过期）。
pub fn cache_is_stale() -> bool {
    let path = cache_path();
    let Ok(meta) = std::fs::metadata(&path) else {
        return true;
    };
    let Ok(modified) = meta.modified() else {
        return true;
    };
    SystemTime::now()
        .duration_since(modified)
        .map(|age| age > CACHE_TTL)
        .unwrap_or(true)
}

/// 从网络刷新目录并写盘。失败保持现有快照不变。
pub async fn refresh(http: &reqwest::Client) -> usize {
    let resp = match http.get(OPENROUTER_MODELS_URL).send().await {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            debug!(status = %r.status(), "openrouter catalog: non-success status");
            return 0;
        }
        Err(e) => {
            debug!("openrouter catalog: fetch failed: {e}");
            return 0;
        }
    };
    let v = match resp.json::<Value>().await {
        Ok(v) => v,
        Err(e) => {
            debug!("openrouter catalog: decode failed: {e}");
            return 0;
        }
    };
    let parsed = parse_catalog(&v);
    if parsed.is_empty() {
        debug!("openrouter catalog: empty payload; keeping previous snapshot");
        return 0;
    }
    let n = parsed.len();
    fill_id_index(&v);
    if let Ok(mut guard) = store().write() {
        *guard = parsed;
    }
    // 写盘失败不影响本次运行（只是下次启动要重新拉）。
    if let Ok(text) = serde_json::to_string(&v) {
        if let Err(e) = std::fs::write(cache_path(), text) {
            debug!("openrouter catalog: cache write failed: {e}");
        }
    }
    n
}

/// 快照里的条目数（诊断用）。
pub fn len() -> usize {
    store().read().map(|g| g.len()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        serde_json::json!({
            "data": [
                {
                    "id": "minimax/minimax-m3.1-flash",
                    "context_length": 1_000_000,
                    "top_provider": { "context_length": 1_000_000, "max_completion_tokens": 524_288 }
                },
                {
                    "id": "nvidia/nemotron-3.5-super-vl",
                    "context_length": 1_048_576,
                    "top_provider": { "context_length": 262_144 }
                },
                {
                    // 只有模型级值
                    "id": "openai/gpt-5",
                    "context_length": 128_000
                },
                {
                    // 完全没有上下文信息 → 跳过
                    "id": "some/router-only",
                    "pricing": { "prompt": "-1" }
                }
            ]
        })
    }

    /// 模型级优先：`top_provider` 只是"某一家 provider"，不能当全局上限。
    ///
    /// 2026-10-23 修正前这里是 `min(模型级, top_provider)`，实测把
    /// `minimax/minimax-m3`（模型级 1,048,576 / 某家 524,288）压成 512K，
    /// 而那家不是用户直连的官方。
    #[test]
    fn parses_context_and_prefers_the_model_level_value() {
        let m = parse_catalog(&sample());
        assert_eq!(m.get("minimaxminimaxm31flash").unwrap().context, 1_000_000);
        // nemotron：模型级 1,048,576 / 某家 262,144 → 用模型级。
        let nemotron = m
            .iter()
            .find(|(k, _)| k.contains("nemotron"))
            .map(|(_, v)| v)
            .expect("nemotron 条目应在表里");
        assert_eq!(
            nemotron.context, 1_048_576,
            "provider 级窗口不能压掉模型级上限（provider 精确值由 endpoints 层负责）"
        );
        assert_eq!(m.get("openaigpt5").unwrap().context, 128_000);
        assert!(
            !m.keys().any(|k| k.contains("router")),
            "没有上下文信息的条目不入表"
        );
    }

    #[test]
    fn normalisation_bridges_config_names_and_catalog_ids() {
        // 配置里写的是 MiniMax-M3.1-Flash，目录 id 带厂商前缀。
        assert_eq!(norm("MiniMax-M3.1-Flash"), "minimaxm31flash");
        assert_eq!(norm("minimax/minimax-m3.1-flash"), "minimaxminimaxm31flash");
        // 后缀匹配：目录 id 以配置名结尾。
        let catalog = norm("minimax/minimax-m3.1-flash");
        assert!(catalog.ends_with(&norm("MiniMax-M3.1-Flash")));
    }

    #[test]
    fn parse_endpoints_reads_each_provider_separately() {
        let v = serde_json::json!({
            "data": {
                "id": "minimax/minimax-m3.1-flash",
                "endpoints": [
                    { "provider_name": "Minimax", "context_length": 1_000_000, "max_completion_tokens": 524_288 },
                    { "provider_name": "Together", "context_length": 262_144, "max_completion_tokens": 16_384 },
                    // 上下文为 0 的条目没有信息量 → 丢弃
                    { "provider_name": "Broken", "context_length": 0 }
                ]
            }
        });
        let list = parse_endpoints(&v);
        assert_eq!(list.len(), 2, "0 上下文的条目应被丢弃: {list:?}");
        assert_eq!(list[0].provider, "Minimax");
        assert_eq!(list[0].context, 1_000_000);
        assert_eq!(list[0].max_output, Some(524_288));
        assert_eq!(list[1].max_output, Some(16_384));
    }

    /// provider hint 命中 → 用它；没命中/没给 → 取窗口最大的那条。
    ///
    /// 这条语义很重要：我们直连官方 API，聚合值可能来自别家（窗口更小），
    /// 按别家的值算会把压缩提前；但也不能反过来把窗口估大（会 400）——
    /// 所以只有"明确知道 provider"时才取小的那家，否则取官方最大。
    #[test]
    fn lookup_limits_prefers_the_matching_provider_then_the_largest() {
        {
            let mut g = endpoint_store().write().unwrap();
            g.insert(
                "minimaxminimaxm31flash".into(),
                vec![
                    EndpointLimits {
                        provider: "Minimax".into(),
                        context: 1_000_000,
                        max_output: Some(524_288),
                    },
                    EndpointLimits {
                        provider: "Together".into(),
                        context: 262_144,
                        max_output: Some(16_384),
                    },
                ],
            );
        }

        let together = lookup_limits("MiniMax-M3.1-Flash", Some("together")).unwrap();
        assert_eq!(together.context, 262_144, "指定 provider 时用它的真实窗口");

        let any = lookup_limits("MiniMax-M3.1-Flash", None).unwrap();
        assert_eq!(any.context, 1_000_000, "未指定时取最大窗口");
        assert_eq!(any.max_output, Some(524_288));
    }

    /// 联网实测：真实目录里 M3.1 必须是 1M（这正是这次修复的动机）。
    #[tokio::test]
    #[ignore = "network; run with --ignored --nocapture"]
    async fn live_catalog_carries_the_real_minimax_context() {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("http client");
        let n = refresh(&http).await;
        println!("catalog entries: {n}");
        if n == 0 {
            eprintln!("SKIP: network unavailable / empty payload");
            return;
        }
        let m31 = lookup("MiniMax-M3.1-Flash").expect("M3.1 应在目录里");
        println!(
            "聚合值：MiniMax-M3.1-Flash → context={} max_output={:?}",
            m31.context, m31.max_output
        );
        assert!(
            m31.context >= 1_000_000,
            "M3.1 实际是 {} tokens —— 静态表按 204.8K 算就是这次修的问题",
            m31.context
        );

        // M3（非 3.1）同样是 1M —— 2026-10-23 用户质疑后实测确认，
        // 旧实现取 min(模型级 1,048,576, 某家 524,288) 会把它压成 512K，
        // 再被静态表兜成 205K。
        let m3 = lookup("MiniMax-M3").expect("M3 应在目录里");
        println!("MiniMax-M3 → context={} max_output={:?}", m3.context, m3.max_output);
        assert!(
            m3.context >= 1_000_000,
            "M3 的模型级窗口是 1,048,576，实际读到 {}",
            m3.context
        );

        // provider 级（endpoints）：真实可用窗口 + 输出上限。
        let id = openrouter_id_for("MiniMax-M3.1-Flash").expect("目录里应有完整 id");
        let providers = refresh_endpoints(&http, &id).await;
        println!("endpoints: {id} → {providers} 个 provider");
        let lim = lookup_limits("MiniMax-M3.1-Flash", None).expect("endpoints 应可查");
        println!(
            "provider 级：context={} max_output={:?}",
            lim.context, lim.max_output
        );
        // endpoints 可能有多家 provider，取最大窗口；只要 ≥1M 就说明
        // provider 级数据可用（具体值随上游变动，不写死）。
        assert!(
            lim.context >= 1_000_000,
            "provider 级窗口应 ≥1M，实际 {}",
            lim.context
        );
        assert_eq!(
            lim.max_output,
            Some(524_288),
            "输出上限 512K 是压缩阈值的关键输入"
        );
    }

    #[test]
    fn lookup_prefers_the_most_specific_suffix_match() {
        // 手工装一份快照，避免依赖全局状态。
        {
            let mut g = store().write().unwrap();
            g.clear();
            g.insert(
                "minimaxminimaxm31flash".into(),
                ModelLimits {
                    context: 1_000_000,
                    max_output: None,
                },
            );
            g.insert(
                "minimaxminimaxm2".into(),
                ModelLimits {
                    context: 204_800,
                    max_output: None,
                },
            );
        }
        assert_eq!(lookup("MiniMax-M3.1-Flash").unwrap().context, 1_000_000);
        assert_eq!(lookup("MiniMax-M2").unwrap().context, 204_800);
        assert_eq!(lookup("unknown-model-xyz"), None);
    }
}
