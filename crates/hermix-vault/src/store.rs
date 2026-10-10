//! SQLite 存取：vault_entries 表 CRUD（secret 列为 AES-GCM 密文 BLOB）。
//!
//! 密文不出本模块之外以原始 BLOB 流动；明文只在 lib.rs 门面层短暂存在。

use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

use crate::VaultError;

/// 元组行（列表用，永不含密码明文——密文也不出列）。
#[derive(Debug, Clone)]
pub struct MetaRow {
    pub id: String,
    pub name: String,
    pub url: String,
    pub username: String,
    pub notes: String,
    pub updated_at: String,
}

/// 全量行（secret 为密文）。
#[derive(Debug, Clone)]
pub struct EncRow {
    pub id: String,
    pub name: String,
    pub url: String,
    pub username: String,
    pub secret: Vec<u8>,
    pub notes: String,
    pub created_at: String,
    pub updated_at: String,
}

/// 打开（必要时创建）vault.db 并确保 schema。
pub fn open_db(path: &Path) -> Result<Connection, VaultError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| VaultError::Io(format!("创建数据目录失败: {e}")))?;
    }
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS vault_entries (
            id         TEXT PRIMARY KEY,
            name       TEXT NOT NULL UNIQUE COLLATE NOCASE,
            url        TEXT NOT NULL DEFAULT '',
            username   TEXT NOT NULL DEFAULT '',
            secret     BLOB NOT NULL,
            notes      TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );",
    )?;
    Ok(conn)
}

/// 列表（按名称排序；不含任何密码数据）。
pub fn list_meta(conn: &Connection) -> Result<Vec<MetaRow>, VaultError> {
    let mut stmt = conn.prepare(
        "SELECT id, name, url, username, notes, updated_at
         FROM vault_entries ORDER BY name COLLATE NOCASE",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(MetaRow {
                id: row.get(0)?,
                name: row.get(1)?,
                url: row.get(2)?,
                username: row.get(3)?,
                notes: row.get(4)?,
                updated_at: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

const FULL_COLS: &str = "id, name, url, username, secret, notes, created_at, updated_at";

fn enc_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EncRow> {
    Ok(EncRow {
        id: row.get(0)?,
        name: row.get(1)?,
        url: row.get(2)?,
        username: row.get(3)?,
        secret: row.get(4)?,
        notes: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

pub fn get_by_id(conn: &Connection, id: &str) -> Result<Option<EncRow>, VaultError> {
    let row = conn
        .query_row(
            &format!("SELECT {FULL_COLS} FROM vault_entries WHERE id = ?1"),
            params![id],
            enc_from_row,
        )
        .optional()?;
    Ok(row)
}

/// agent 的 vault_get 按名称取（用户说「打开 xx 平台」时按名匹配）。
pub fn get_by_name(conn: &Connection, name: &str) -> Result<Option<EncRow>, VaultError> {
    let row = conn
        .query_row(
            &format!("SELECT {FULL_COLS} FROM vault_entries WHERE name = ?1 COLLATE NOCASE"),
            params![name],
            enc_from_row,
        )
        .optional()?;
    Ok(row)
}

/// 插入。name 唯一约束冲突 → `VaultError::Duplicate`（不走通用 Db 错误）。
pub fn insert(conn: &Connection, e: &EncRow) -> Result<(), VaultError> {
    let r = conn.execute(
        "INSERT INTO vault_entries (id, name, url, username, secret, notes, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![e.id, e.name, e.url, e.username, e.secret, e.notes, e.created_at, e.updated_at],
    );
    if let Err(err) = r {
        if matches!(
            err.sqlite_error_code(),
            Some(rusqlite::ErrorCode::ConstraintViolation)
        ) {
            return Err(VaultError::Duplicate(e.name.clone()));
        }
        return Err(err.into());
    }
    Ok(())
}

/// 全列更新（按 id；门面层先读旧行再改写，secret 由调用方决定是否重加密）。
pub fn update_full(conn: &Connection, e: &EncRow) -> Result<bool, VaultError> {
    let n = conn.execute(
        "UPDATE vault_entries SET name=?2, url=?3, username=?4, secret=?5, notes=?6, updated_at=?7
         WHERE id = ?1",
        params![e.id, e.name, e.url, e.username, e.secret, e.notes, e.updated_at],
    )?;
    Ok(n > 0)
}

pub fn delete(conn: &Connection, id: &str) -> Result<bool, VaultError> {
    let n = conn.execute("DELETE FROM vault_entries WHERE id = ?1", params![id])?;
    Ok(n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, name: &str, secret: &[u8]) -> EncRow {
        EncRow {
            id: id.into(),
            name: name.into(),
            url: "https://x".into(),
            username: "u".into(),
            secret: secret.to_vec(),
            notes: String::new(),
            created_at: "t0".into(),
            updated_at: "t0".into(),
        }
    }

    #[test]
    fn crud_and_unique_name() {
        let dir = std::env::temp_dir().join(format!("hermix-vault-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let conn = open_db(&dir.join("vault.db")).unwrap();

        insert(&conn, &row("1", "Alpha", b"enc-a")).unwrap();
        insert(&conn, &row("2", "Beta", b"enc-b")).unwrap();
        // 同名拒绝 → Duplicate
        assert!(matches!(
            insert(&conn, &row("3", "alpha", b"enc-c")),
            Err(VaultError::Duplicate(_))
        ));

        assert_eq!(list_meta(&conn).unwrap().len(), 2);
        assert_eq!(list_meta(&conn).unwrap()[0].name, "Alpha");

        assert_eq!(get_by_id(&conn, "2").unwrap().unwrap().secret, b"enc-b");
        assert_eq!(get_by_name(&conn, "BETA").unwrap().unwrap().id, "2"); // 大小写不敏感
        assert!(get_by_id(&conn, "nope").unwrap().is_none());

        let mut e = get_by_id(&conn, "1").unwrap().unwrap();
        e.url = "https://new".into();
        e.updated_at = "t1".into();
        assert!(update_full(&conn, &e).unwrap());
        assert!(!update_full(&conn, &row("ghost", "g", b"")).unwrap());

        assert!(delete(&conn, "2").unwrap());
        assert!(!delete(&conn, "2").unwrap());
        assert_eq!(list_meta(&conn).unwrap().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
