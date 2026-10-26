//! 密钥托管端口：把 `llm_providers[*].api_key` 从 `config.yaml` 里挪出去。
//!
//! 分层理由：真正存密钥的实现（`hermix-vault`：AES-256-GCM + Credential Manager）
//! 依赖本 crate 的路径解析，让 config 反向依赖 vault 会成环。这里只定义端口，
//! 由 `hermix-server` 在启动时注入实现 —— config 负责「保存前封存、加载后还原」，
//! 不关心密钥存在哪里。
//!
//! 降级语义（2026-10-25 拍板 A）：托管实现不可用时**不阻断保存**，`api_key` 以
//! 明文落盘并 warn。机密性让位于「用户必须能改配置」。
//!
//! 加载侧相反，fail closed：配置里出现引用但取不到明文时把该 provider 的 key
//! 置空并记 error —— 绝不把 `vault:...` 当密钥发给上游。

use std::sync::OnceLock;

use crate::config::GatewayConfig;

/// 密钥托管端口。实现方负责持久化与加密，并保证 `put` 幂等（同 slot 覆盖）。
pub trait SecretStore: Send + Sync {
    fn put(&self, slot: &str, value: &str) -> Result<(), String>;
    fn get(&self, slot: &str) -> Result<Option<String>, String>;
}

static STORE: OnceLock<Box<dyn SecretStore>> = OnceLock::new();

/// 引用的前缀。故意不用 `${...}`：那个语法已被 env 插值占用，
/// `api_key: ${DEEPSEEK_API_KEY}` 与托管引用必须能区分开。
const REF_PREFIX: &str = "vault:";

/// 进程级安装一次（重复安装保留第一次）。
pub fn install_secret_store(store: Box<dyn SecretStore>) {
    let _ = STORE.set(store);
}

fn store() -> Option<&'static dyn SecretStore> {
    STORE.get().map(|b| b.as_ref())
}

/// 这个值值得封存吗？空串、`none` 哨兵、`${ENV}` 引用、已经是 `vault:` 引用的
/// 都不该进托管（前两类不是密钥，后两类说明已经处理过）。
fn is_plain_key(value: &str) -> bool {
    let v = value.trim();
    !v.is_empty()
        && !v.starts_with(REF_PREFIX)
        && !v.starts_with("${")
        && !v.eq_ignore_ascii_case(crate::free_channel::KEY_NO_AUTH)
}

/// 保存前：明文 → 引用。
pub(crate) fn seal(cfg: &mut GatewayConfig) {
    seal_with(cfg, store());
}

/// 加载后：引用 → 明文。
pub(crate) fn unseal(cfg: &mut GatewayConfig) {
    unseal_with(cfg, store());
}

/// 可注入版本（测试用：全局 `OnceLock` 装一次就锁死，测试之间会互相污染）。
pub(crate) fn seal_with(cfg: &mut GatewayConfig, store: Option<&dyn SecretStore>) {
    let Some(store) = store else {
        return;
    };
    for (id, provider) in cfg.llm_providers.iter_mut() {
        let Some(key) = provider.api_key.as_deref() else {
            continue;
        };
        if !is_plain_key(key) {
            continue;
        }
        let slot = format!("llm_providers.{id}");
        match store.put(&slot, key.trim()) {
            Ok(()) => provider.api_key = Some(format!("{REF_PREFIX}{slot}")),
            Err(e) => tracing::warn!(
                provider = %id,
                error = %e,
                "密钥托管不可用，api_key 以明文写入 config.yaml"
            ),
        }
    }
}

/// 可注入版本。`store = None`（未启用托管）时同样把引用清空 —— 带着引用去请求
/// 等于把 `vault:...` 当 API key 发出去，比"未配置"更糟。
pub(crate) fn unseal_with(cfg: &mut GatewayConfig, store: Option<&dyn SecretStore>) {
    for (id, provider) in cfg.llm_providers.iter_mut() {
        let Some(key) = provider.api_key.as_deref() else {
            continue;
        };
        let Some(slot) = key.strip_prefix(REF_PREFIX) else {
            continue;
        };
        match store.map(|s| s.get(slot)) {
            Some(Ok(Some(plain))) => provider.api_key = Some(plain),
            Some(Ok(None)) => {
                tracing::error!(
                    provider = %id,
                    slot,
                    "密钥槽位为空（vault 被清空？），该 provider 视为未配置"
                );
                provider.api_key = None;
            }
            Some(Err(e)) => {
                tracing::error!(
                    provider = %id,
                    slot,
                    error = %e,
                    "密钥托管不可用，无法还原 api_key，该 provider 视为未配置"
                );
                provider.api_key = None;
            }
            None => {
                tracing::error!(
                    provider = %id,
                    slot,
                    "配置引用了托管密钥但本进程未启用密钥托管，该 provider 视为未配置"
                );
                provider.api_key = None;
            }
        }
    }
}

/// JSON 形态的封存。
///
/// `/api/config` 走 `serde_json::Value` 合并（要保留本版本不认识的字段），
/// 不经过 `GatewayConfig`，所以需要一份按字段名操作的实现。
pub fn seal_json_config(cfg: &mut serde_json::Value) {
    seal_json_with(cfg, store());
}

/// JSON 形态的还原。
pub fn unseal_json_config(cfg: &mut serde_json::Value) {
    unseal_json_with(cfg, store());
}

fn providers_mut(cfg: &mut serde_json::Value) -> Option<&mut serde_json::Map<String, serde_json::Value>> {
    cfg.get_mut("llm_providers")?.as_object_mut()
}

pub(crate) fn seal_json_with(cfg: &mut serde_json::Value, store: Option<&dyn SecretStore>) {
    let Some(store) = store else {
        return;
    };
    let Some(providers) = providers_mut(cfg) else {
        return;
    };
    for (id, entry) in providers.iter_mut() {
        let Some(obj) = entry.as_object_mut() else {
            continue;
        };
        let Some(key) = obj.get("api_key").and_then(|v| v.as_str()).map(str::to_string) else {
            continue;
        };
        if !is_plain_key(&key) {
            continue;
        }
        let slot = format!("llm_providers.{id}");
        match store.put(&slot, key.trim()) {
            Ok(()) => {
                obj.insert(
                    "api_key".to_string(),
                    serde_json::Value::String(format!("{REF_PREFIX}{slot}")),
                );
            }
            Err(e) => tracing::warn!(
                provider = %id,
                error = %e,
                "密钥托管不可用，api_key 以明文写入 config.yaml"
            ),
        }
    }
}

pub(crate) fn unseal_json_with(cfg: &mut serde_json::Value, store: Option<&dyn SecretStore>) {
    let Some(providers) = providers_mut(cfg) else {
        return;
    };
    for (id, entry) in providers.iter_mut() {
        let Some(obj) = entry.as_object_mut() else {
            continue;
        };
        let Some(slot) = obj
            .get("api_key")
            .and_then(|v| v.as_str())
            .and_then(|v| v.strip_prefix(REF_PREFIX))
            .map(str::to_string)
        else {
            continue;
        };
        match store.map(|s| s.get(&slot)) {
            Some(Ok(Some(plain))) => {
                obj.insert("api_key".to_string(), serde_json::Value::String(plain));
            }
            _ => {
                tracing::error!(
                    provider = %id,
                    slot = %slot,
                    "无法还原托管密钥（未启用 / 槽位为空 / 托管不可用），该 provider 视为未配置"
                );
                obj.insert("api_key".to_string(), serde_json::Value::Null);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemStore {
        map: Mutex<HashMap<String, String>>,
        fail: bool,
    }

    impl SecretStore for MemStore {
        fn put(&self, slot: &str, value: &str) -> Result<(), String> {
            if self.fail {
                return Err("store unavailable".into());
            }
            self.map.lock().unwrap().insert(slot.into(), value.into());
            Ok(())
        }
        fn get(&self, slot: &str) -> Result<Option<String>, String> {
            if self.fail {
                return Err("store unavailable".into());
            }
            Ok(self.map.lock().unwrap().get(slot).cloned())
        }
    }

    fn config_with_key(value: &str) -> GatewayConfig {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "deepseek".into(),
            crate::config::LlmProviderConfig {
                api_key: Some(value.into()),
                ..Default::default()
            },
        );
        cfg
    }

    fn key_of(cfg: &GatewayConfig) -> Option<&str> {
        cfg.llm_providers
            .get("deepseek")
            .and_then(|p| p.api_key.as_deref())
    }

    #[test]
    fn seal_then_unseal_roundtrip() {
        let store = MemStore::default();
        let mut cfg = config_with_key("sk-real-secret");

        seal_with(&mut cfg, Some(&store));
        assert_eq!(key_of(&cfg), Some("vault:llm_providers.deepseek"));
        assert_eq!(
            store.map.lock().unwrap().get("llm_providers.deepseek").map(String::as_str),
            Some("sk-real-secret")
        );

        unseal_with(&mut cfg, Some(&store));
        assert_eq!(key_of(&cfg), Some("sk-real-secret"));
    }

    #[test]
    fn seal_is_idempotent() {
        let store = MemStore::default();
        let mut cfg = config_with_key("sk-real-secret");
        seal_with(&mut cfg, Some(&store));
        seal_with(&mut cfg, Some(&store));
        assert_eq!(key_of(&cfg), Some("vault:llm_providers.deepseek"));
    }

    #[test]
    fn non_secrets_are_not_sealed() {
        let store = MemStore::default();
        for raw in ["none", "", "${DEEPSEEK_API_KEY}", "vault:llm_providers.deepseek"] {
            let mut cfg = config_with_key(raw);
            seal_with(&mut cfg, Some(&store));
            assert_eq!(key_of(&cfg), Some(raw), "`{raw}` 不该被封存");
        }
        assert!(store.map.lock().unwrap().is_empty());
    }

    #[test]
    fn store_failure_falls_back_to_plaintext() {
        let store = MemStore {
            fail: true,
            ..Default::default()
        };
        let mut cfg = config_with_key("sk-real-secret");
        seal_with(&mut cfg, Some(&store));
        assert_eq!(
            key_of(&cfg),
            Some("sk-real-secret"),
            "托管不可用时必须保持明文（否则保存配置会静默丢密钥）"
        );
    }

    #[test]
    fn missing_slot_fails_closed_instead_of_leaking_reference() {
        let store = MemStore::default();
        let mut cfg = config_with_key("vault:llm_providers.deepseek");
        unseal_with(&mut cfg, Some(&store));
        assert_eq!(key_of(&cfg), None, "取不到明文就该视为未配置");
    }

    #[test]
    fn no_store_also_clears_reference() {
        let mut cfg = config_with_key("vault:llm_providers.deepseek");
        unseal_with(&mut cfg, None);
        assert_eq!(key_of(&cfg), None);
    }

    #[test]
    fn json_roundtrip_keeps_unknown_fields() {
        let store = MemStore::default();
        let mut value = serde_json::json!({
            "model": "deepseek:chat",
            "llm_providers": { "deepseek": { "api_key": "sk-json", "base_url": "https://x" } },
            "future_field": { "keep": true }
        });

        seal_json_with(&mut value, Some(&store));
        assert_eq!(value["llm_providers"]["deepseek"]["api_key"], "vault:llm_providers.deepseek");
        assert_eq!(value["future_field"]["keep"], true, "未知字段必须原样保留");

        unseal_json_with(&mut value, Some(&store));
        assert_eq!(value["llm_providers"]["deepseek"]["api_key"], "sk-json");
    }

    #[test]
    fn json_missing_slot_becomes_null() {
        let store = MemStore::default();
        let mut value = serde_json::json!({
            "llm_providers": { "x": { "api_key": "vault:llm_providers.x" } }
        });
        unseal_json_with(&mut value, Some(&store));
        assert!(value["llm_providers"]["x"]["api_key"].is_null());
    }
}
