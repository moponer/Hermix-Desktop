//! # RegistryStore — 工作台注册表
//!
//! `registry.sqlite` 维护工作台列表（id / 名称 / 图标 / 数据目录 / 时间戳）。
//! 数据本身在各自工作台独立 DB（见 `store`）；本表只做索引与顺序。
//!
//! 存储布局（`docs/工作台架构设计方案.md` §8.1）：
//! ```text
//! ~/.hermix/workspaces/
//! ├── <ws_id>/workspace.json + data.sqlite + files/ + audit.sqlite
//! └── registry.sqlite
//! ```
//!
//! 沿用 `hermix-automation::repository` 的 `open_with_wal` 模式（WAL +
//! busy_timeout + foreign_keys）。

use std::path::{Path, PathBuf};

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum RegistryStoreError {
    #[error("SQLite 错误: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("IO 错误: {0}")]
    Io(String),
    #[error("工作台[{0}]不存在")]
    NotFound(String),
}

// ---------------------------------------------------------------------------
// 实体
// ---------------------------------------------------------------------------

/// 注册表条目（registry.sqlite 一行）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegistryEntry {
    pub id: String,
    pub name: String,
    pub icon: String,
    /// 工作台数据目录相对 workspaces 根的名字（= ws_id）
    pub dir: String,
    pub created_at: i64,
    pub updated_at: i64,
}

// ---------------------------------------------------------------------------
// RegistryStore
// ---------------------------------------------------------------------------

/// 工作台注册表（stateless，每方法开新连接）。
pub struct RegistryStore {
    db_path: PathBuf,
}

fn open_with_wal(path: &Path) -> Result<rusqlite::Connection, RegistryStoreError> {
    let conn = rusqlite::Connection::open(path)
        .map_err(|e| RegistryStoreError::Io(format!("打开 registry db 失败: {e}")))?;
    if let Err(e) =
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;")
    {
        tracing::warn!("registry_store: WAL/busy_timeout PRAGMA 失败 ({e}); 继续");
    }
    let _ = conn.execute_batch("PRAGMA foreign_keys = ON;");
    Ok(conn)
}

impl RegistryStore {
    /// 打开（或创建）注册表并确保表结构（幂等）。
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self, RegistryStoreError> {
        let store = Self {
            db_path: db_path.as_ref().to_path_buf(),
        };
        let conn = open_with_wal(&store.db_path)?;
        conn.execute_batch(
            r#"CREATE TABLE IF NOT EXISTS workspaces (
                id         TEXT PRIMARY KEY,
                name       TEXT NOT NULL,
                icon       TEXT NOT NULL DEFAULT '',
                dir        TEXT NOT NULL UNIQUE,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_workspaces_updated ON workspaces(updated_at);"#,
        )?;
        Ok(store)
    }

    fn now() -> i64 {
        chrono::Utc::now().timestamp_millis()
    }

    /// 新增工作台条目（`dir` 即工作台数据目录名，须唯一）。
    pub fn create(&self, id: &str, name: &str, icon: &str, dir: &str) -> Result<RegistryEntry, RegistryStoreError> {
        let now = Self::now();
        let entry = RegistryEntry {
            id: id.to_string(),
            name: name.to_string(),
            icon: icon.to_string(),
            dir: dir.to_string(),
            created_at: now,
            updated_at: now,
        };
        let conn = open_with_wal(&self.db_path)?;
        conn.execute(
            "INSERT INTO workspaces (id, name, icon, dir, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                entry.id,
                entry.name,
                entry.icon,
                entry.dir,
                entry.created_at,
                entry.updated_at
            ],
        )?;
        Ok(entry)
    }

    /// 列出全部工作台（按更新时间倒序 = Tab 顺序）。
    pub fn list(&self) -> Result<Vec<RegistryEntry>, RegistryStoreError> {
        let conn = open_with_wal(&self.db_path)?;
        let mut stmt = conn.prepare(
            "SELECT id, name, icon, dir, created_at, updated_at FROM workspaces
             ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], row_to_entry)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 按 id 查单个工作台。
    pub fn get(&self, id: &str) -> Result<RegistryEntry, RegistryStoreError> {
        let conn = open_with_wal(&self.db_path)?;
        let entry = conn
            .query_row(
                "SELECT id, name, icon, dir, created_at, updated_at FROM workspaces WHERE id = ?1",
                [id],
                row_to_entry,
            )
            .optional()?
            .ok_or_else(|| RegistryStoreError::NotFound(id.to_string()))?;
        Ok(entry)
    }

    /// 重命名工作台（顺带刷新 updated_at）。
    pub fn rename(&self, id: &str, name: &str) -> Result<(), RegistryStoreError> {
        let conn = open_with_wal(&self.db_path)?;
        let n = conn.execute(
            "UPDATE workspaces SET name = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![name, Self::now(), id],
        )?;
        if n == 0 {
            return Err(RegistryStoreError::NotFound(id.to_string()));
        }
        Ok(())
    }

    /// 删除工作台条目（只删注册行，不删数据目录 —— 数据删除走显式清理）。
    pub fn remove(&self, id: &str) -> Result<(), RegistryStoreError> {
        let conn = open_with_wal(&self.db_path)?;
        let n = conn.execute("DELETE FROM workspaces WHERE id = ?1", [id])?;
        if n == 0 {
            return Err(RegistryStoreError::NotFound(id.to_string()));
        }
        Ok(())
    }
}

fn row_to_entry(r: &rusqlite::Row<'_>) -> rusqlite::Result<RegistryEntry> {
    Ok(RegistryEntry {
        id: r.get(0)?,
        name: r.get(1)?,
        icon: r.get(2)?,
        dir: r.get(3)?,
        created_at: r.get(4)?,
        updated_at: r.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_list_get_rename_remove() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = RegistryStore::open(dir.path().join("registry.sqlite")).expect("open");

        // 创建
        let e = store.create("ws-1", "车辆管理", "car", "ws-1").expect("create");
        assert_eq!(e.name, "车辆管理");
        store.create("ws-2", "人员管理", "users", "ws-2").expect("create2");

        // 列表（updated 倒序：后建的 ws-2 在前）
        let list = store.list().expect("list");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "ws-2");

        // get
        let got = store.get("ws-1").expect("get");
        assert_eq!(got.dir, "ws-1");
        assert_eq!(got.icon, "car");

        // rename 刷新 updated_at
        std::thread::sleep(std::time::Duration::from_millis(5));
        store.rename("ws-1", "车队管理").expect("rename");
        let got = store.get("ws-1").expect("get2");
        assert_eq!(got.name, "车队管理");
        assert!(got.updated_at > got.created_at);
        let list = store.list().expect("list2");
        assert_eq!(list[0].id, "ws-1", "重命名后应排最前");

        // remove
        store.remove("ws-2").expect("remove");
        assert_eq!(store.list().expect("list3").len(), 1);
    }

    #[test]
    fn duplicate_dir_rejected() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = RegistryStore::open(dir.path().join("registry.sqlite")).expect("open");
        store.create("a", "A", "", "dir1").expect("ok");
        let err = store.create("b", "B", "", "dir1").unwrap_err();
        assert!(matches!(err, RegistryStoreError::Sqlite(_)), "{err:?}");
    }

    #[test]
    fn not_found_errors() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = RegistryStore::open(dir.path().join("registry.sqlite")).expect("open");
        assert!(matches!(
            store.get("ghost"),
            Err(RegistryStoreError::NotFound(_))
        ));
        assert!(matches!(
            store.rename("ghost", "x"),
            Err(RegistryStoreError::NotFound(_))
        ));
        assert!(matches!(
            store.remove("ghost"),
            Err(RegistryStoreError::NotFound(_))
        ));
    }
}
