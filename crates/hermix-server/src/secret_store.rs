//! 把 `hermix-config` 的密钥托管端口接到 vault 上。
//!
//! 分层：`hermix-config` 只定义 [`hermix_config::SecretStore`] 端口（避免
//! `config → vault → config` 依赖环），实现放在 server —— 它本来就同时依赖两者。
//!
//! 打开是**惰性**的：`install` 只注册实现，第一次真正存取密钥时才打开 vault
//! （SQLite + Credential Manager）。打不开就记住失败并让 `put`/`get` 返回 Err，
//! 于是保存配置降级为明文 + warn（2026-10-25 拍板 A：不能因为密钥库坏了就让
//! 用户改不了设置），加载配置把该 provider 视为未配置（fail closed）。

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use hermix_config::SecretStore;
use hermix_vault::{Vault, VaultError};

const NOTE: &str = "hermix config provider key（由 config.yaml 托管，勿手动删除）";

/// 幂等安装（重复调用保留第一次注册的实现）。
pub fn install_default(vault_db: PathBuf) {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    if INSTALLED.set(()).is_err() {
        return;
    }
    hermix_config::install_secret_store(Box::new(VaultSecretStore {
        state: Mutex::new(State::Unopened(vault_db)),
    }));
    tracing::debug!("secret store: vault 端口已注册（惰性打开）");
}

enum State {
    Unopened(PathBuf),
    Ready(Vault),
    Failed(String),
}

struct VaultSecretStore {
    state: Mutex<State>,
}

impl VaultSecretStore {
    fn with_vault<T>(&self, f: impl FnOnce(&Vault) -> Result<T, String>) -> Result<T, String> {
        let mut guard = self
            .state
            .lock()
            .map_err(|_| "vault 状态锁中毒".to_string())?;
        // 先取出待打开路径再赋值（直接 match &*guard 会与 *guard = ... 冲突）。
        let pending = match &*guard {
            State::Unopened(path) => Some(path.clone()),
            _ => None,
        };
        if let Some(path) = pending {
            *guard = match Vault::open_at(&path) {
                Ok(v) => {
                    tracing::info!(db = %path.display(), "secret store: vault 已打开");
                    State::Ready(v)
                }
                Err(e) => {
                    let msg = format!("{e}");
                    tracing::warn!(
                        db = %path.display(),
                        error = %msg,
                        "secret store: vault 打不开，密钥将以明文写入 config.yaml"
                    );
                    State::Failed(msg)
                }
            };
        }
        match &*guard {
            State::Ready(vault) => f(vault),
            State::Failed(e) => Err(e.clone()),
            State::Unopened(_) => unreachable!("上面已把 Unopened 处理掉"),
        }
    }

}

impl SecretStore for VaultSecretStore {
    fn put(&self, slot: &str, value: &str) -> Result<(), String> {
        self.with_vault(|vault| {
            let existing = vault
                .list()
                .map_err(|e| e.to_string())?
                .into_iter()
                .find(|e| e.name == slot);
            match existing {
                Some(entry) => vault
                    .update(&entry.id, slot, "", "", Some(value), NOTE)
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
                None => vault
                    .add(slot, "", "", value, NOTE)
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
            }
        })
    }

    fn get(&self, slot: &str) -> Result<Option<String>, String> {
        self.with_vault(|vault| match vault.get_by_name(slot) {
            Ok(entry) => Ok(entry.secret),
            Err(VaultError::NotFound(_)) => Ok(None),
            Err(e) => Err(e.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 打不开的 vault 必须返回 Err（而不是 panic / 静默 Ok），上层据此降级明文。
    #[test]
    fn unopened_failure_is_reported_not_silent() {
        // 把一个**已存在的目录**当 db 文件路径：SQLite 报 CANTOPEN，
        // 且不会在磁盘上留下任何东西（用不存在的盘符会因为"目录能建出来"
        // 而在真机上意外成功并写出文件）。
        let not_a_db = std::env::temp_dir();
        let store = VaultSecretStore {
            state: Mutex::new(State::Unopened(not_a_db)),
        };
        assert!(store.put("slot", "secret").is_err());
        assert!(store.get("slot").is_err());
    }
}
