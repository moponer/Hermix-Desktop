//! # Schema 迁移（M2.5 增量迭代）
//!
//! LLM 对话改 schema 后，把旧定义迁移到新定义（`docs/工作台架构设计方案.md` §7.6）：
//!
//! | 变更 | 策略 |
//! |---|---|
//! | 加字段 | `ALTER TABLE ADD COLUMN`（可空；必填由应用层校验兜底，避免 SQLite 对非空新增列的限制） |
//! | 加表 | 建新表（幂等 `CREATE TABLE IF NOT EXISTS`） |
//! | 删表 | **不物理删**——数据保留在 DB，仅从 schema 移除（防数据丢失） |
//! | 删字段 | **不物理删**——列保留，仅从 schema 移除 |
//! | 改枚举 | 校验存量值，报告非法值数量（不删除）；新写入由应用层 `convert_value` 拦 |
//! | 布局/自动化 | 只动 `workspace.json`，无 DB 迁移 |
//!
//! 幂等：`old == new`（serde 值相等）直接返回 unchanged 报告；
//! 已存在的列跳过 ADD COLUMN（PRAGMA table_info 探测）。
//! 失败原子性：DDL 在事务内执行，任一步失败整体回滚，DB 不落半迁移状态。

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::model::{Field, Module, WorkspaceSchema};
use crate::store::sqlite_type;
use crate::store::LocalSqliteStore;
use crate::validate::validate_schema;

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    #[error("schema 校验失败: {0}")]
    Validation(String),
    #[error("SQLite 错误: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("IO 错误: {0}")]
    Io(String),
}

// ---------------------------------------------------------------------------
// 报告
// ---------------------------------------------------------------------------

/// 单列变更记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnChange {
    pub table: String,
    pub column: String,
}

/// 迁移报告（工具/API 返回给 LLM，叙述给用户）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MigrationReport {
    pub added_tables: Vec<String>,
    pub added_columns: Vec<ColumnChange>,
    /// 从 schema 移除但**数据保留**的表/列。
    pub removed_tables: Vec<String>,
    pub removed_columns: Vec<ColumnChange>,
    /// 枚举变更提示（存量非法值数量，不删除）。
    pub enum_notes: Vec<String>,
    /// 幂等：无任何变更。
    pub unchanged: bool,
}

// ---------------------------------------------------------------------------
// 迁移
// ---------------------------------------------------------------------------

/// 把 `db_path` 从旧 schema 迁移到新 schema。
/// 成功后由调用方把新 `workspace.json` 落盘（本函数只管 DB）。
pub fn migrate_schema(
    db_path: &Path,
    old_schema: &WorkspaceSchema,
    new_schema: &WorkspaceSchema,
) -> Result<MigrationReport, MigrateError> {
    validate_schema(new_schema).map_err(|e| MigrateError::Validation(e.to_string()))?;

    // 幂等：serde 值相等 → 无变更
    let old_v = serde_json::to_value(old_schema).unwrap_or_default();
    let new_v = serde_json::to_value(new_schema).unwrap_or_default();
    if old_v == new_v {
        return Ok(MigrationReport { unchanged: true, ..Default::default() });
    }

    // 旧表结构索引：table id → (字段 id → Field)
    let mut old_tables: std::collections::HashMap<String, std::collections::HashMap<String, Field>> =
        std::collections::HashMap::new();
    for m in &old_schema.modules {
        if let Module::Table { id, fields, .. } = m {
            old_tables.insert(id.clone(), fields.iter().map(|f| (f.id.clone(), f.clone())).collect());
        }
    }

    // 建表/补列走 LocalSqliteStore::open（幂等 ensure_schema 建新表），
    // ALTER ADD COLUMN 走事务内裸 DDL。
    let _store = LocalSqliteStore::open(db_path, new_schema.clone())
        .map_err(|e| MigrateError::Io(e.to_string()))?;
    let conn = store_conn(db_path)?;

    let mut report = MigrationReport::default();
    let tx = conn.unchecked_transaction()?;

    for m in &new_schema.modules {
        if let Module::Table { id, fields, .. } = m {
            match old_tables.get(id) {
                None => {
                    // 新表：ensure_schema 已建，登记报告
                    report.added_tables.push(id.clone());
                }
                Some(old_fields) => {
                    // 逐列 diff：新字段不在旧表 → ADD COLUMN（可空）
                    for f in fields {
                        if !old_fields.contains_key(&f.id) {
                            add_column(&tx, id, f)?;
                            report.added_columns.push(ColumnChange {
                                table: id.clone(),
                                column: f.id.clone(),
                            });
                        }
                    }
                }
            }
        }
    }

    // 从 schema 移除的表（数据保留）
    let new_table_ids: std::collections::HashSet<&str> = new_schema
        .modules
        .iter()
        .filter_map(|m| match m {
            Module::Table { id, .. } => Some(id.as_str()),
            Module::Dashboard { .. } => None,
        })
        .collect();
    for tid in old_tables.keys() {
        if !new_table_ids.contains(tid.as_str()) {
            report.removed_tables.push(tid.clone());
        }
    }

    // 移除的列（数据保留）+ 枚举存量校验
    for m in &new_schema.modules {
        if let Module::Table { id, fields, .. } = m {
            if let Some(old_fields) = old_tables.get(id) {
                let new_field_ids: std::collections::HashSet<&str> =
                    fields.iter().map(|f| f.id.as_str()).collect();
                for (fid, _old_f) in old_fields {
                    if !new_field_ids.contains(fid.as_str()) {
                        report.removed_columns.push(ColumnChange {
                            table: id.clone(),
                            column: fid.clone(),
                        });
                    }
                }
                // 枚举选项变化 → 存量非法值计数
                for f in fields {
                    if !f.is_enum() { continue; }
                    if let Some(old_f) = old_fields.get(&f.id) {
                        if old_f.options == f.options { continue; }
                        let opts = f.options.as_ref().cloned().unwrap_or_default();
                        let n = count_invalid_enum(&tx, id, &f.id, &opts)?;
                        report.enum_notes.push(format!(
                            "表[{id}].字段[{}] 枚举选项变更：存量 {n} 行不在新选项内（数据保留，新写入按新枚举校验）",
                            f.id
                        ));
                    }
                }
            }
        }
    }

    tx.commit()?;
    Ok(report)
}

/// 打开带 WAL 的连接（镜像 store 的 open_with_wal）。
fn store_conn(db_path: &Path) -> Result<rusqlite::Connection, MigrateError> {
    let conn = rusqlite::Connection::open(db_path)
        .map_err(|e| MigrateError::Io(format!("打开工作台 DB 失败: {e}")))?;
    if let Err(e) =
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;")
    {
        tracing::warn!("workspace_migrate: WAL/busy_timeout PRAGMA 失败 ({e}); 继续");
    }
    let _ = conn.execute_batch("PRAGMA foreign_keys = ON;");
    Ok(conn)
}

/// 加一列（可空——必填由应用层校验兜底）。
fn add_column(
    tx: &rusqlite::Transaction<'_>,
    table: &str,
    field: &Field,
) -> Result<(), MigrateError> {
    // 幂等：列已存在则跳过
    let exists: bool = tx
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |r| r.get::<_, String>(1))?
        .filter_map(Result::ok)
        .any(|name| name == field.id);
    if exists {
        return Ok(());
    }
    let col_type = sqlite_type(field.field_type);
    let mut sql = format!("ALTER TABLE {table} ADD COLUMN {} {col_type}", field.id);
    // Enum 加 CHECK 约束（新列，仅约束新写入；存量 NULL 不受影响）
    if field.is_enum() {
        let opts: Vec<String> = field
            .options
            .as_ref()
            .unwrap_or(&Vec::new())
            .iter()
            .map(|o| format!("'{}'", o.replace('\'', "''")))
            .collect();
        sql.push_str(&format!(" CHECK({} IN ({}))", field.id, opts.join(", ")));
    }
    tx.execute(&sql, [])?;
    Ok(())
}

/// 统计不在新枚举选项内的存量行数。
fn count_invalid_enum(
    tx: &rusqlite::Transaction<'_>,
    table: &str,
    column: &str,
    options: &[String],
) -> Result<u64, MigrateError> {
    if options.is_empty() {
        // 空选项（理论不会过校验）→ 全部视为非法
        let n: i64 = tx.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE {column} IS NOT NULL"),
            [],
            |r| r.get(0),
        )?;
        return Ok(n.max(0) as u64);
    }
    let placeholders: Vec<String> = (1..=options.len()).map(|i| format!("?{i}")).collect();
    let sql = format!(
        "SELECT COUNT(*) FROM {table} WHERE {column} IS NOT NULL AND {column} NOT IN ({})",
        placeholders.join(", ")
    );
    let params: Vec<&str> = options.iter().map(|o| o.as_str()).collect();
    let n: i64 = tx.query_row(&sql, rusqlite::params_from_iter(params), |r| r.get(0))?;
    Ok(n.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::WorkspaceStore;

    fn schema_v1() -> WorkspaceSchema {
        serde_json::from_value(serde_json::json!({
            "name": "车辆管理",
            "modules": [
                { "type": "table", "id": "vehicles", "name": "车辆",
                  "fields": [
                    { "id": "plate", "name": "车牌", "type": "text", "required": true, "unique": true },
                    { "id": "status", "name": "状态", "type": "enum", "options": ["在用", "维修", "停用"] }
                  ] }
            ]
        }))
        .expect("v1")
    }

    fn schema_v2() -> WorkspaceSchema {
        serde_json::from_value(serde_json::json!({
            "name": "车辆管理",
            "modules": [
                { "type": "table", "id": "vehicles", "name": "车辆",
                  "fields": [
                    { "id": "plate", "name": "车牌", "type": "text", "required": true, "unique": true },
                    { "id": "status", "name": "状态", "type": "enum", "options": ["在用", "维修", "停用", "报废"] },
                    { "id": "mileage", "name": "里程", "type": "number" },
                    { "id": "inspection_date", "name": "年检", "type": "date" }
                  ] },
                { "type": "table", "id": "drivers", "name": "司机",
                  "fields": [ { "id": "name", "name": "姓名", "type": "text", "required": true } ] }
            ]
        }))
        .expect("v2")
    }

    #[test]
    fn add_column_and_table_preserves_data() {
        let dir = tempfile::tempdir().expect("tmp");
        let db = dir.path().join("data.sqlite");

        // 建 v1 + 插入存量数据
        let store = LocalSqliteStore::open(&db, schema_v1()).expect("open v1");
        store
            .insert(
                "vehicles",
                &[serde_json::json!({ "plate": "京A12345", "status": "在用" })],
                "user",
            )
            .expect("insert");

        // 迁移到 v2
        let report = migrate_schema(&db, &schema_v1(), &schema_v2()).expect("migrate");
        assert!(report.added_columns.iter().any(|c| c.column == "mileage"));
        assert!(report.added_columns.iter().any(|c| c.column == "inspection_date"));
        assert!(report.added_tables.contains(&"drivers".to_string()));
        assert!(report.enum_notes.iter().any(|n| n.contains("status")));

        // 存量数据保留
        let reopened = LocalSqliteStore::open(&db, schema_v2()).expect("open v2");
        let rows = reopened.query("vehicles", &[], None, 10, 0).expect("query");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["plate"], serde_json::json!("京A12345"));

        // 新列可写
        reopened
            .update(
                "vehicles",
                &rows[0]["id"].as_str().expect("id"),
                &serde_json::json!({ "mileage": 12000, "inspection_date": "2027-03-15" }),
                "user",
            )
            .expect("update");

        // 新表可写
        reopened
            .insert("drivers", &[serde_json::json!({ "name": "张三" })], "user")
            .expect("insert driver");
    }

    #[test]
    fn remove_table_and_column_keep_data() {
        let dir = tempfile::tempdir().expect("tmp");
        let db = dir.path().join("data.sqlite");

        let store = LocalSqliteStore::open(&db, schema_v2()).expect("open v2");
        store
            .insert(
                "vehicles",
                &[serde_json::json!({ "plate": "京A1", "status": "在用", "mileage": 5 })],
                "user",
            )
            .expect("insert");

        // 迁回 v1（mileage 列移除、drivers 表移除——数据保留）
        let report = migrate_schema(&db, &schema_v2(), &schema_v1()).expect("migrate");
        assert!(report.removed_columns.iter().any(|c| c.column == "mileage"));
        assert!(report.removed_tables.contains(&"drivers".to_string()));

        // 物理数据仍在：用 v2 schema 重新打开仍能查到（列/表未物理删）
        let reopened = LocalSqliteStore::open(&db, schema_v2()).expect("open v2");
        let rows = reopened.query("vehicles", &[], None, 10, 0).expect("query");
        assert_eq!(rows[0]["mileage"], serde_json::json!(5.0));
    }

    #[test]
    fn idempotent_no_change_returns_unchanged() {
        let dir = tempfile::tempdir().expect("tmp");
        let db = dir.path().join("data.sqlite");
        LocalSqliteStore::open(&db, schema_v1()).expect("open");

        let report = migrate_schema(&db, &schema_v1(), &schema_v1()).expect("migrate");
        assert!(report.unchanged);
        assert!(report.added_columns.is_empty());
        assert!(report.added_tables.is_empty());
    }

    #[test]
    fn invalid_new_schema_rejected() {
        let dir = tempfile::tempdir().expect("tmp");
        let db = dir.path().join("data.sqlite");
        LocalSqliteStore::open(&db, schema_v1()).expect("open");

        let bad: WorkspaceSchema = serde_json::from_value(serde_json::json!({
            "name": "bad",
            "modules": [{ "type": "table", "id": "a", "name": "A",
                          "fields": [{ "id": "st", "name": "状态", "type": "enum" }] }]
        }))
        .expect("bad");
        let err = migrate_schema(&db, &schema_v1(), &bad).unwrap_err();
        assert!(matches!(err, MigrateError::Validation(_)));
    }

    #[test]
    fn enum_change_reports_invalid_rows() {
        let dir = tempfile::tempdir().expect("tmp");
        let db = dir.path().join("data.sqlite");
        let store = LocalSqliteStore::open(&db, schema_v1()).expect("open v1");
        store
            .insert(
                "vehicles",
                &[
                    serde_json::json!({ "plate": "京A1", "status": "在用" }),
                    serde_json::json!({ "plate": "京A2", "status": "停用" }),
                ],
                "user",
            )
            .expect("insert");

        // 新 schema 枚举**移除**「停用」→ 存量 1 行非法
        let narrowed: WorkspaceSchema = serde_json::from_value(serde_json::json!({
            "name": "车辆管理",
            "modules": [
                { "type": "table", "id": "vehicles", "name": "车辆",
                  "fields": [
                    { "id": "plate", "name": "车牌", "type": "text", "required": true, "unique": true },
                    { "id": "status", "name": "状态", "type": "enum", "options": ["在用", "维修"] }
                  ] }
            ]
        }))
        .expect("narrowed");

        let report = migrate_schema(&db, &schema_v1(), &narrowed).expect("migrate");
        let note = report
            .enum_notes
            .iter()
            .find(|n| n.contains("status"))
            .expect("enum note");
        assert!(note.contains("1 行"), "{note}");
    }
}
