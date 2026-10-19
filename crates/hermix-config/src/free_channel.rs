//! 内置免费模型渠道（虚拟 `custom-free-*` provider）。
//!
//! 设计取舍：免费模型不引入新运行时，也不写专用 provider 实现——它们是
//! 普通的 OpenAI 兼容端点，因此以 `custom-free-*` 条目的形态落进
//! `llm_providers`，复用既有的模型开关（`model_selector`）、热推送
//! （`set_config`）与审批链路。
//!
//! 凭据是哨兵值：上游公开的匿名令牌（如 `public`）原样当 api_key 存放，
//! 这样 `provider_has_key` / `auto_correct_model` / 模型清单过滤等既有
//! 判定全部无需特判。`none` 表示该端点不接受 `Authorization` 头，
//! 请求时直接省略（见 [`auth_header`]）。

use crate::config::{FreeChannelConfig, GatewayConfig, LlmProviderConfig};
use crate::paths::env_compat;

/// 表示"该端点不需要 Authorization 头"的哨兵 api_key。
pub const KEY_NO_AUTH: &str = "none";

/// 免费渠道前缀——`is_free_provider` 与启动摘除都按它判定。
pub const FREE_PROVIDER_PREFIX: &str = "custom-free-";

/// 一个内置免费渠道的定义。
pub struct FreeChannel {
    /// provider id（必须带 [`FREE_PROVIDER_PREFIX`]）。
    pub id: &'static str,
    /// UI 显示名。
    pub display_name: &'static str,
    /// 推理 base_url（`{base}/chat/completions`）。
    pub base_url: &'static str,
    /// 哨兵凭据。
    pub api_key: &'static str,
    /// 编译期 seed 清单（模型 id + 保守 context window）。
    pub seed_models: &'static [(&'static str, u64)],
}

/// 内置渠道表。新增渠道 = 加一条；删渠道 = 删一条（已写入
/// `config.yaml` 的条目由 [`strip`] 负责，前提是 `enabled: false`）。
///
/// 只收「普通 OpenAI 客户端可以匿名直连」的端点：
/// - OpenCode Zen（`https://opencode.ai/inference/v1` + `Bearer public`）
///   实测对非它自家客户端一律 `403 FreeTierError`，绕过只能伪造其
///   `x-opencode-*` 指纹头——那是规避检测，不在本功能范围内，故不收录。
/// - Pollinations（`https://text.pollinations.ai/openai`）可匿名直连，但
///   路径不是 OpenAI 约定的 `{base}/chat/completions`，`GenericProvider`
///   的 URL 拼接口径装不下，需专用 provider 实现，暂不收录。
pub const FREE_CHANNELS: &[FreeChannel] = &[
    FreeChannel {
        id: "custom-free-kilo",
        display_name: "免费模型 · Kilo",
        base_url: "https://api.kilo.ai/api/gateway",
        // Kilo 公共网关不校验 Authorization 头。
        api_key: KEY_NO_AUTH,
        // 实测（2026-10-19）：两者都返回 tool_calls，cost=0。
        // `openrouter/free` 是聚合路由（跨免费池），作为默认候选。
        seed_models: &[
            ("openrouter/free", 128_000),
            ("minimax/minimax-m3.1-flash:free", 128_000),
        ],
    },
];

/// 是否为本项目写入的免费渠道 provider。
pub fn is_free_provider(id: &str) -> bool {
    id.starts_with(FREE_PROVIDER_PREFIX)
}

/// 构造 `Authorization` 头的值。
///
/// `None` = 不要发这个头。空 key 与 [`KEY_NO_AUTH`] 哨兵都属于"无鉴权"。
/// 所有出网点（LLM 请求、模型清单拉取、连通性测试）必须走这里，否则
/// Kilo 这类无鉴权端点会被 `Bearer none` 直接打 401。
pub fn auth_header(key: Option<&str>) -> Option<String> {
    let key = key?.trim();
    if key.is_empty() || key.eq_ignore_ascii_case(KEY_NO_AUTH) {
        return None;
    }
    Some(format!("Bearer {key}"))
}

/// 渠道实际使用的 base_url（config 覆盖 > 环境变量 > 内置）。
pub fn resolved_base_url(cfg: &FreeChannelConfig, channel: &FreeChannel) -> String {
    cfg.base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            env_compat("HERMIX_FREE_BASE_URL", "HERMES_FREE_BASE_URL")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| channel.base_url.to_string())
}

/// 渠道实际使用的模型清单地址（config 覆盖 > 环境变量 > `{base}/models`）。
pub fn resolved_models_url(cfg: &FreeChannelConfig, channel: &FreeChannel) -> String {
    cfg.models_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            env_compat("HERMIX_FREE_MODELS_URL", "HERMES_FREE_MODELS_URL")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| format!("{}/models", resolved_base_url(cfg, channel).trim_end_matches('/')))
}

/// 合并模型 CSV：只增不删，保序去重。
fn merge_model_csv(existing: Option<&str>, extra: impl IntoIterator<Item = String>) -> (String, usize) {
    let mut out: Vec<String> = existing
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let mut added = 0usize;
    for id in extra {
        if !out.iter().any(|m| m == &id) {
            out.push(id);
            added += 1;
        }
    }
    (out.join(","), added)
}

/// 幂等 seed：缺失的 provider / 字段补齐，已有内容只增不删。
///
/// 只补 `display_name` / `base_url` / `api_key` 为空的字段——用户改过的
/// 端点不会被每次启动覆盖回去。
/// 返回是否发生了改动（调用方据此决定是否落盘）。
pub fn ensure_seeded(config: &mut GatewayConfig) -> bool {
    let cfg = config.free_channel.clone();
    if !cfg.enabled {
        return false;
    }
    let mut changed = false;

    for channel in FREE_CHANNELS {
        let base_url = resolved_base_url(&cfg, channel);
        let entry = config
            .llm_providers
            .entry(channel.id.to_string())
            .or_insert_with(|| {
                changed = true;
                LlmProviderConfig::default()
            });

        if entry.display_name.is_none() {
            entry.display_name = Some(channel.display_name.to_string());
            changed = true;
        }
        if entry
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_none()
        {
            entry.base_url = Some(base_url);
            changed = true;
        }
        if entry
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_none()
        {
            entry.api_key = Some(channel.api_key.to_string());
            changed = true;
        }

        for (model, ctx) in channel.seed_models {
            entry
                .model_contexts
                .entry((*model).to_string())
                .or_insert(*ctx);
        }

        let (merged, added) = merge_model_csv(
            entry.model.as_deref(),
            channel.seed_models.iter().map(|(m, _)| (*m).to_string()),
        );
        if added > 0 || entry.model.is_none() {
            entry.model = Some(merged);
            changed = true;
        }

        // 保守的 provider 级 context_window：清单/seed 都没给具体值时用它，
        // 避免未知模型落到 128k 兜底后压缩阈值判断失真。
        if entry.context_window.is_none() {
            entry.context_window = Some(128_000);
            changed = true;
        }
    }

    changed
}

/// 兜底默认模型：按 [`FREE_CHANNELS`] 顺序取第一个有模型的渠道。
///
/// 只在"用户一个能用的厂商都没有"时被调用，因此顺序即偏好。
/// 优先取第一个 seed 模型（已实测过），否则退回清单里的第一条。
pub fn default_model(config: &GatewayConfig) -> Option<(String, String)> {
    for channel in FREE_CHANNELS {
        let Some(entry) = config.llm_providers.get(channel.id) else {
            continue;
        };
        let models: Vec<String> = entry
            .model
            .as_deref()
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        if models.is_empty() {
            continue;
        }
        let preferred = channel
            .seed_models
            .first()
            .map(|(m, _)| (*m).to_string())
            .filter(|m| models.iter().any(|x| x == m))
            .unwrap_or_else(|| models[0].clone());
        return Some((channel.id.to_string(), preferred));
    }
    None
}

/// 摘除所有免费渠道条目（全局开关关闭时调用）。
///
/// 同时清掉 `model_selector` 里的对应条目与指向它们的 `config.model`，
/// 否则会留下孤儿：清单过滤掉、默认模型指向不存在的 provider。
pub fn strip(config: &mut GatewayConfig) -> bool {
    let mut changed = false;
    let removed: Vec<String> = config
        .llm_providers
        .keys()
        .filter(|id| is_free_provider(id))
        .cloned()
        .collect();
    for id in &removed {
        config.llm_providers.remove(id);
        changed = true;
    }

    if let Some(sel) = config.model_selector.as_mut() {
        let before = sel.enabled.len();
        sel.enabled.retain(|e| !is_free_provider(&e.provider_id));
        if sel.enabled.len() != before {
            changed = true;
        }
        let default_is_free = sel
            .default_model
            .as_deref()
            .and_then(|d| d.split_once(':'))
            .map(|(p, _)| is_free_provider(p))
            .unwrap_or(false);
        if default_is_free {
            sel.default_model = None;
            changed = true;
        }
    }

    let model_is_free = config
        .model
        .as_deref()
        .and_then(|m| m.split_once(':'))
        .map(|(p, _)| is_free_provider(p))
        .unwrap_or(false);
    if model_is_free {
        config.model = None;
        changed = true;
    }

    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> GatewayConfig {
        let mut cfg = GatewayConfig::default();
        assert!(ensure_seeded(&mut cfg));
        cfg
    }

    #[test]
    fn seeding_is_idempotent_and_keeps_user_edits() {
        let mut cfg = seeded();
        cfg.llm_providers
            .get_mut("custom-free-kilo")
            .expect("seeded")
            .base_url = Some("http://localhost:9999/v1".to_string());

        assert!(!ensure_seeded(&mut cfg), "second seed must be a no-op");
        assert_eq!(
            cfg.llm_providers
                .get("custom-free-kilo")
                .unwrap()
                .base_url
                .as_deref(),
            Some("http://localhost:9999/v1"),
            "user-edited endpoint must not be overwritten"
        );
    }

    #[test]
    fn seeding_only_adds_models() {
        let mut cfg = seeded();
        // 用户自加一条、并删掉了所有 seed 模型。
        cfg.llm_providers
            .get_mut("custom-free-kilo")
            .unwrap()
            .model = Some("my-model".to_string());

        assert!(ensure_seeded(&mut cfg), "missing seed models must be re-added");

        let model = cfg
            .llm_providers
            .get("custom-free-kilo")
            .unwrap()
            .model
            .clone()
            .unwrap();
        assert!(model.starts_with("my-model,"), "user model keeps its place");
        assert!(model.contains("openrouter/free"));
    }

    #[test]
    fn seeding_writes_seed_models_and_context() {
        let cfg = seeded();
        let entry = cfg.llm_providers.get("custom-free-kilo").unwrap();
        assert_eq!(entry.api_key.as_deref(), Some(KEY_NO_AUTH));
        assert_eq!(entry.display_name.as_deref(), Some("免费模型 · Kilo"));
        assert_eq!(entry.context_window, Some(128_000));
        assert!(entry.model.as_deref().unwrap().contains("openrouter/free"));
        assert_eq!(entry.model_contexts.get("openrouter/free"), Some(&128_000));
    }

    #[test]
    fn strip_removes_provider_selector_and_default() {
        let mut cfg = seeded();
        cfg.model = Some("custom-free-kilo:openrouter/free".to_string());
        cfg.model_selector = Some(crate::config::ModelSelectorConfig {
            enabled: vec![
                crate::config::ModelSelectorEntry {
                    provider_id: "custom-free-kilo".to_string(),
                    model_id: "openrouter/free".to_string(),
                },
                crate::config::ModelSelectorEntry {
                    provider_id: "openai".to_string(),
                    model_id: "gpt-4o".to_string(),
                },
            ],
            default_model: Some("custom-free-kilo:openrouter/free".to_string()),
        });

        assert!(strip(&mut cfg));
        assert!(cfg.llm_providers.is_empty());
        assert_eq!(cfg.model, None);
        let sel = cfg.model_selector.unwrap();
        assert_eq!(sel.enabled.len(), 1);
        assert_eq!(sel.enabled[0].provider_id, "openai");
        assert_eq!(sel.default_model, None);
    }

    #[test]
    fn disabled_channel_is_not_seeded() {
        let mut cfg = GatewayConfig::default();
        cfg.free_channel.enabled = false;
        assert!(!ensure_seeded(&mut cfg));
        assert!(cfg.llm_providers.is_empty());
    }

    #[test]
    fn auth_header_skips_anonymous_sentinel() {
        assert_eq!(auth_header(None), None);
        assert_eq!(auth_header(Some("  ")), None);
        assert_eq!(auth_header(Some(KEY_NO_AUTH)), None);
        assert_eq!(auth_header(Some("none")), None);
        assert_eq!(auth_header(Some("public")).as_deref(), Some("Bearer public"));
        assert_eq!(auth_header(Some(" sk-1 ")).as_deref(), Some("Bearer sk-1"));
    }

    #[test]
    fn models_url_defaults_to_base_plus_models() {
        let cfg = FreeChannelConfig::default();
        let channel = &FREE_CHANNELS[0];
        assert!(resolved_models_url(&cfg, channel).ends_with("/models"));
    }
}
