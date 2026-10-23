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
        // 两者都缺 → 这条对上下文没用，跳过。
        let ctx_opt = match (model_ctx, provider_ctx) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
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

/// 查一个型号的限额。
///
/// 先精确匹配归一化名，再退到后缀匹配（目录 id 常带厂商前缀，
/// 例如配置写 `MiniMax-M3.1-Flash`、目录是 `minimax/minimax-m3.1-flash`）。
/// 多个候选时取最长（最具体的）那条。
pub fn lookup(model: &str) -> Option<ModelLimits> {
    let key = norm(model);
    if key.is_empty() {
        return None;
    }
    let guard = store().read().ok()?;
    if let Some(hit) = guard.get(&key) {
        return Some(*hit);
    }
    guard
        .iter()
        .filter(|(k, _)| k.ends_with(&key) || key.ends_with(k.as_str()))
        .max_by_key(|(k, _)| k.len())
        .map(|(_, v)| *v)
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
    if let Ok(mut guard) = store().write() {
        *guard = parsed;
    }
    n
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

    #[test]
    fn parses_context_and_takes_the_smaller_of_the_two() {
        let m = parse_catalog(&sample());
        assert_eq!(m.get("minimaxminimaxm31flash").unwrap().context, 1_000_000);
        // 模型级 1M 但实际可用 256K → 取小的（估大才会被 API 400 打脸）。
        let nemotron = m
            .iter()
            .find(|(k, _)| k.contains("nemotron"))
            .map(|(_, v)| v)
            .expect("nemotron 条目应在表里");
        assert_eq!(nemotron.context, 262_144);
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
            "MiniMax-M3.1-Flash → context={} max_output={:?}",
            m31.context, m31.max_output
        );
        assert!(
            m31.context >= 1_000_000,
            "M3.1 实际是 {} tokens —— 静态表按 204.8K 算就是这次修的问题",
            m31.context
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
