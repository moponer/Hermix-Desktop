//! hermix-vault — 本地凭据保险库。
//!
//! 决策笔记：docs/notes/implemented/feature/2026-10-09-vault-credentials.md
//! （AES-256-GCM 落盘 + 主密钥 Windows Credential Manager；数据单机，
//! relay/手机对话取凭据走 E2EE 通道——见 hermix-server 侧接入）。
//!
//! 分层：crypto（加解密）/ credman（主密钥托管）/ store（SQLite CRUD）/
//! Vault（门面：装载密钥 → 暴露增删改查，明文只在门面层短暂存在）。
//!
//! 信任边界（不可削减项）：
//! - secret 列永存密文；主密钥永不落 `~/.hermix` 明文文件
//! - 列表接口（`list_meta`）永不含密码数据——agent 的 vault_list 只见名称/URL
//! - 主密钥载体 zeroize（Drop 清零）

pub mod credman;
pub mod crypto;
pub mod store;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use hermix_config::paths::hermix_home;

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("数据库错误: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("IO 错误: {0}")]
    Io(String),
    #[error("加密错误: {0}")]
    Crypto(String),
    #[error("凭据托管错误: {0}")]
    CredMan(String),
    #[error("条目「{0}」已存在")]
    Duplicate(String),
    #[error("条目不存在（{0}）")]
    NotFound(String),
}

/// 条目明文视图（门面层出参；密码字段默认掩码由 UI/工具层控制）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub url: String,
    pub username: String,
    /// 仅 `get`/`get_by_name` 填充；`list` 恒为 None（永不含密码）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    pub notes: String,
    pub updated_at: String,
}

impl Entry {
    fn from_enc(e: &store::EncRow, secret: Option<String>) -> Entry {
        Entry {
            id: e.id.clone(),
            name: e.name.clone(),
            url: e.url.clone(),
            username: e.username.clone(),
            secret,
            notes: e.notes.clone(),
            updated_at: e.updated_at.clone(),
        }
    }
}

/// 门面：持连接 + 主密钥。多线程经 Mutex 串行（凭据操作低频，无吞吐诉求）。
pub struct Vault {
    conn: Mutex<rusqlite::Connection>,
    key: crypto::MasterKey,
}

impl Vault {
    /// 打开（或首启初始化）保险库：`<hermix_home>/vault.db`。
    /// 首启生成主密钥 → CredMan；已有密钥直接装载。**主密钥不在内存长驻**
    /// 的强化（用完即弃）留作后续——当前进程内常驻（Mutex 保护），
    /// 与 Credential Manager 同级安全。
    pub fn open() -> Result<Vault, VaultError> {
        let db_path = default_db_path();
        let conn = store::open_db(&db_path)?;
        let key = load_or_create_key()?;
        Ok(Vault {
            conn: Mutex::new(conn),
            key,
        })
    }

    /// 测试/定制路径版。
    pub fn open_at(db_path: &Path) -> Result<Vault, VaultError> {
        let conn = store::open_db(db_path)?;
        let key = load_or_create_key()?;
        Ok(Vault {
            conn: Mutex::new(conn),
            key,
        })
    }

    /// 列表：**永不含密码**（secret 字段恒 None，结构体层面保证）。
    pub fn list(&self) -> Result<Vec<Entry>, VaultError> {
        let rows = store::list_meta(&self.conn.lock().unwrap())?;
        Ok(rows
            .iter()
            .map(|m| Entry {
                id: m.id.clone(),
                name: m.name.clone(),
                url: m.url.clone(),
                username: m.username.clone(),
                secret: None,
                notes: m.notes.clone(),
                updated_at: m.updated_at.clone(),
            })
            .collect())
    }

    /// 按 id 取（含明文密码——调用方必须已经过权限闸）。
    pub fn get(&self, id: &str) -> Result<Entry, VaultError> {
        let e = store::get_by_id(&self.conn.lock().unwrap(), id)?
            .ok_or_else(|| VaultError::NotFound(id.into()))?;
        let plain = crypto::decrypt(&self.key, &e.secret)?;
        Ok(Entry::from_enc(&e, Some(plain)))
    }

    /// 按名称取（agent 的 vault_get；大小写不敏感）。
    pub fn get_by_name(&self, name: &str) -> Result<Entry, VaultError> {
        let e = store::get_by_name(&self.conn.lock().unwrap(), name)?
            .ok_or_else(|| VaultError::NotFound(name.into()))?;
        let plain = crypto::decrypt(&self.key, &e.secret)?;
        Ok(Entry::from_enc(&e, Some(plain)))
    }

    /// 新增。name 唯一；created/updated 由这里打（调用方不传时间）。
    pub fn add(&self, name: &str, url: &str, username: &str, secret: &str, notes: &str) -> Result<Entry, VaultError> {
        let now = chrono::Utc::now().to_rfc3339();
        let enc = store::EncRow {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.trim().to_string(),
            url: url.trim().to_string(),
            username: username.trim().to_string(),
            secret: crypto::encrypt(&self.key, secret)?,
            notes: notes.to_string(),
            created_at: now.clone(),
            updated_at: now,
        };
        store::insert(&self.conn.lock().unwrap(), &enc)?;
        Ok(Entry::from_enc(&enc, None))
    }

    /// 更新。secret=None 表示不改密码（编辑表单未动密码字段）。
    pub fn update(
        &self,
        id: &str,
        name: &str,
        url: &str,
        username: &str,
        secret: Option<&str>,
        notes: &str,
    ) -> Result<Entry, VaultError> {
        let mut e = store::get_by_id(&self.conn.lock().unwrap(), id)?
            .ok_or_else(|| VaultError::NotFound(id.into()))?;
        e.name = name.trim().to_string();
        e.url = url.trim().to_string();
        e.username = username.trim().to_string();
        e.notes = notes.to_string();
        if let Some(s) = secret {
            e.secret = crypto::encrypt(&self.key, s)?;
        }
        e.updated_at = chrono::Utc::now().to_rfc3339();
        if !store::update_full(&self.conn.lock().unwrap(), &e)? {
            return Err(VaultError::NotFound(id.into()));
        }
        // 改名冲突会以 Db(ConstraintViolation) 冒出（同 insert）。
        Ok(Entry::from_enc(&e, None))
    }

    pub fn delete(&self, id: &str) -> Result<(), VaultError> {
        if !store::delete(&self.conn.lock().unwrap(), id)? {
            return Err(VaultError::NotFound(id.into()));
        }
        Ok(())
    }
}

fn default_db_path() -> PathBuf {
    hermix_home().join("vault.db")
}

/// 装载或首启生成主密钥。CredMan 不可用时**如实报错**（不静默降级为
/// 明文/弱派生——信任边界，决策笔记 Consequences 有记）。
fn load_or_create_key() -> Result<crypto::MasterKey, VaultError> {
    match credman::read_master_key()? {
        Some(bytes) => {
            let arr: [u8; 32] = bytes
                .as_slice()
                .try_into()
                .map_err(|_| VaultError::CredMan("主密钥长度异常（非 32 字节）".into()))?;
            Ok(crypto::MasterKey::from(arr))
        }
        None => {
            let key = crypto::generate_master_key()?;
            credman::write_master_key(key.as_slice())?;
            Ok(key)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_vault(tag: &str) -> Vault {
        let dir = std::env::temp_dir().join(format!("hermix-vault-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Vault::open_at(&dir.join("vault.db")).unwrap()
    }

    #[test]
    fn add_get_update_delete_roundtrip() {
        let v = temp_vault("facade");
        v.add("GitHub", "https://github.com", "me", "gh-p@ss", "主账号").unwrap();
        // 同名拒绝
        assert!(matches!(v.add("github", "", "", "", ""), Err(VaultError::Duplicate(_))));

        let e = v.get_by_name("github").unwrap();
        assert_eq!(e.secret.as_deref(), Some("gh-p@ss"));
        assert_eq!(e.url, "https://github.com");

        // list 永不含密码
        let metas = v.list().unwrap();
        assert_eq!(metas.len(), 1);
        assert!(metas[0].secret.is_none());

        // 不改密码的编辑
        v.update(&e.id, "GitHub", "https://github.com", "me@x", None, "改备注").unwrap();
        assert_eq!(v.get(&e.id).unwrap().secret.as_deref(), Some("gh-p@ss"));
        assert_eq!(v.get(&e.id).unwrap().notes, "改备注");
        // 改密码
        v.update(&e.id, "GitHub", "https://github.com", "me@x", Some("new-p@ss"), "").unwrap();
        assert_eq!(v.get(&e.id).unwrap().secret.as_deref(), Some("new-p@ss"));

        v.delete(&e.id).unwrap();
        assert!(matches!(v.get(&e.id), Err(VaultError::NotFound(_))));
    }

    #[test]
    fn vault_file_stores_no_plaintext() {
        // 落盘审计：db 文件字节里不得出现明文密码——信任边界的直接证据。
        let dir = std::env::temp_dir().join(format!("hermix-vault-plain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = dir.join("vault.db");
        let v = Vault::open_at(&db).unwrap();
        let marker = "PLAINTEXT-MARKER-p@ss";
        v.add("X", "", "", marker, "").unwrap();
        drop(v);
        let bytes = std::fs::read(&db).unwrap();
        assert!(!window_contains(&bytes, marker.as_bytes()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn window_contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }
}
