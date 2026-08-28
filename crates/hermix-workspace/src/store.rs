//! # WorkspaceStore — 数据访问层
//!
//! 架构口径（`docs/工作台架构设计方案.md` §8/§11）：
//! - **trait 抽象**：前端/工具族/提醒引擎只依赖 `WorkspaceStore`，
//!   本地实现 `LocalSqliteStore`，未来同步实现（`SyncedStore`）可平替。
//! - **每工作台独立 SQLite 库**：隔离 + 备份/导出天然。
//! - **参数化查询 + 列名白名单**：列名/表名只来自 schema 标识符（过
//!   `validate` 校验），值全部参数化——防 SQL 注入。
//! - **系统列**：`id`(UUID) / `created_at` / `updated_at` / `updated_by`
//!   每表必带（sync-friendly + 审计）。
//! - **插入校验**：必填/类型/枚举/唯一在应用层先校验（友好错误），
//!   DDL 约束（NOT NULL / UNIQUE / CHECK）作兜底。

use std::path::{Path, PathBuf};

use rusqlite::OptionalExtension;
use serde_json::Value;

use crate::model::{FieldType, Module, WorkspaceSchema};
use crate::validate::validate_schema;

// ---------------------------------------------------------------------------
// 过滤条件
// ---------------------------------------------------------------------------

/// 过滤操作符。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilterOp {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
    /// 文本包含（LIKE '%v%'）
    Contains,
}

impl FilterOp {
    fn sql(self) -> &'static str {
        match self {
            FilterOp::Eq => "=",
            FilterOp::Ne => "<>",
            FilterOp::Gt => ">",
            FilterOp::Gte => ">=",
            FilterOp::Lt => "<",
            FilterOp::Lte => "<=",
            FilterOp::Contains => "LIKE",
        }
    }
}

/// 单条件过滤（字段名须在工作台 schema 白名单内）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Filter {
    pub field: String,
    pub op: FilterOp,
    pub value: Value,
}

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("SQLite 错误: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("IO 错误: {0}")]
    Io(String),
    #[error("表[{0}]不存在")]
    TableNotFound(String),
    #[error("行[{0}]不存在")]
    RowNotFound(String),
    #[error("未知列[{0}]（不在 schema 白名单内）")]
    UnknownColumn(String),
    #[error("数据校验失败: {0}")]
    Validation(String),
    #[error("唯一约束冲突: {0}")]
    Unique(String),
}

// ---------------------------------------------------------------------------
// trait
// ---------------------------------------------------------------------------

/// 工作台数据访问抽象。所有实现必须保证：参数化查询 + 列名白名单 +
/// 插入校验（类型/枚举/必填/唯一）。
pub trait WorkspaceStore: Send + Sync {
    /// 查询行。`filters` 全部 AND；`sort` 形如 `"field"` 或 `"-field"`（降序）；
    /// `limit` 强制上限（防上下文膨胀，调用方不可传 0 表示不限）。
    fn query(
        &self,
        table: &str,
        filters: &[Filter],
        sort: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Value>, StoreError>;

    /// 插入一行或多行，返回生成的行 id（UUID）。`updated_by` 记操作来源
    /// （user / llm / automation）。
    fn insert(&self, table: &str, rows: &[Value], updated_by: &str) -> Result<Vec<String>, StoreError>;

    /// 按主键更新（patch 键须在 schema 白名单内）。
    fn update(
        &self,
        table: &str,
        id: &str,
        patch: &Value,
        updated_by: &str,
    ) -> Result<(), StoreError>;

    /// 按主键删除。
    fn delete(&self, table: &str, id: &str) -> Result<(), StoreError>;

    /// 计数（与 `query` 同一套过滤语义）。
    fn count(&self, table: &str, filters: &[Filter]) -> Result<u64, StoreError>;
}

// ---------------------------------------------------------------------------
// LocalSqliteStore
// ---------------------------------------------------------------------------

/// 本地 SQLite 实现：一个工作台一个 DB 文件。
#[derive(Debug)]
pub struct LocalSqliteStore {
    db_path: PathBuf,
    schema: WorkspaceSchema,
}

/// 变更记录（P6 序列化给 REST/前端）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChangeEntry {
    pub seq: i64,
    pub table: String,
    pub row_id: String,
    pub op: String,
    pub before: Option<Value>,
    pub after: Option<Value>,
    pub changed_by: String,
    pub changed_at: String,
}

impl LocalSqliteStore {
    /// 打开（或创建）工作台 DB 并按 schema 建表。
    /// schema 必须通过 `validate_schema`，否则返回 `StoreError::Validation`。
    pub fn open(
        db_path: impl AsRef<Path>,
        schema: WorkspaceSchema,
    ) -> Result<Self, StoreError> {
        validate_schema(&schema).map_err(|e| StoreError::Validation(e.to_string()))?;
        let store = Self {
            db_path: db_path.as_ref().to_path_buf(),
            schema,
        };
        store.ensure_schema()?;
        Ok(store)
    }

    /// 返回当前 schema（供校验/工具注册使用）。
    pub fn schema(&self) -> &WorkspaceSchema {
        &self.schema
    }

    /// 试跑一条自动化条件（M2 建设自测用）——只编译不返回数据。
    /// `condition` 是单表 WHERE 片段（用户确认过的 schema 内字段），
    /// 表名来自模块 id（已过标识符白名单）；LIMIT 0 保证零开销零污染。
    pub fn probe_condition(&self, table: &str, condition: &str) -> Result<(), StoreError> {
        self.table_fields(table)?; // 表存在性
        let sql = format!("SELECT COUNT(*) FROM {table} WHERE ({condition}) LIMIT 0");
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let _ = stmt.query([])?;
        Ok(())
    }

    /// 条件扫描（M4.2 零 LLM 条件提醒引擎）——按 WHERE 片段查询匹配行。
    /// 白名单列 SELECT + 表名/条件片段均已在建设/迁移时校验；LIMIT 上限
    /// 防大结果集拖垮通知通道（条件命中只需首 N 行）。
    pub fn query_by_condition(
        &self,
        table: &str,
        condition: &str,
        limit: usize,
    ) -> Result<Vec<Value>, StoreError> {
        self.table_fields(table)?;
        let cols = self.column_whitelist(table)?;
        let mut sql = format!("SELECT {} FROM {table} WHERE ({condition})", cols.join(", "));
        if limit > 0 {
            sql.push_str(&format!(" LIMIT {limit}"));
        }
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |r| row_to_json(r, &cols))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 关键字搜索（P1 前端筛选）——跨全部文本类字段 OR LIKE，参数化防注入。
    /// 仅搜 `Text`/`Enum`（Relation 存目标行 id、Attachment 存路径，不参与全文）。
    /// 空 keyword 直接返回空（调用方只在非空时调用）。
    pub fn search_text(
        &self,
        table: &str,
        keyword: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Value>, StoreError> {
        if keyword.trim().is_empty() {
            return Ok(Vec::new());
        }
        let fields = self.table_fields(table)?;
        let cols = self.column_whitelist(table)?;
        let searchable: Vec<&str> = fields
            .iter()
            .filter(|f| matches!(f.field_type, FieldType::Text | FieldType::Enum))
            .map(|f| f.id.as_str())
            .collect();
        if searchable.is_empty() {
            return Ok(Vec::new());
        }
        let mut sql = format!("SELECT {} FROM {table} WHERE ", cols.join(", "));
        let mut wheres: Vec<String> = Vec::new();
        for (i, col) in searchable.iter().enumerate() {
            wheres.push(format!("{col} LIKE ?{}", i + 1));
        }
        sql.push_str(&wheres.join(" OR "));
        if limit > 0 {
            sql.push_str(&format!(" LIMIT {limit} OFFSET {offset}"));
        }
        let needle = format!("%{}%", keyword);
        let params: Vec<rusqlite::types::Value> = (0..searchable.len())
            .map(|_| rusqlite::types::Value::Text(needle.clone()))
            .collect();
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
            row_to_json(r, &cols)
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 关键字搜索命中总数（分页用），空 keyword 返回全表数。
    pub fn count_search_text(&self, table: &str, keyword: &str) -> Result<u64, StoreError> {
        let fields = self.table_fields(table)?;
        let searchable: Vec<&str> = fields
            .iter()
            .filter(|f| matches!(f.field_type, FieldType::Text | FieldType::Enum))
            .map(|f| f.id.as_str())
            .collect();
        if searchable.is_empty() {
            return Ok(0);
        }
        if keyword.trim().is_empty() {
            return self.count(table, &[]);
        }
        let mut sql = format!("SELECT COUNT(*) FROM {table} WHERE ");
        let mut wheres: Vec<String> = Vec::new();
        for (i, col) in searchable.iter().enumerate() {
            wheres.push(format!("{col} LIKE ?{}", i + 1));
        }
        sql.push_str(&wheres.join(" OR "));
        let needle = format!("%{}%", keyword);
        let params: Vec<rusqlite::types::Value> = (0..searchable.len())
            .map(|_| rusqlite::types::Value::Text(needle.clone()))
            .collect();
        let conn = self.conn()?;
        conn.query_row(&sql, rusqlite::params_from_iter(params.iter()), |r| r.get(0))
            .map_err(StoreError::Sqlite)
    }

    // ---------------------------------------------------------------------
    // 变更历史（P6）：change_log 记录 + 查询 + 撤销
    // ---------------------------------------------------------------------

    /// 按 id 查单行（审计 before/after 用；不存在 → None）。
    fn query_row_by_id(&self, table: &str, id: &str) -> Result<Option<Value>, StoreError> {
        let rows = self.query(
            table,
            &[Filter {
                field: "id".into(),
                op: FilterOp::Eq,
                value: Value::String(id.to_string()),
            }],
            None,
            1,
            0,
        )?;
        Ok(rows.into_iter().next())
    }

    /// 写一条变更记录（before/after 为 JSON 文本；insert 无 before、delete 无 after）。
    fn log_change(
        &self,
        conn: &rusqlite::Connection,
        table: &str,
        row_id: &str,
        op: &str,
        before: Option<&Value>,
        after: Option<&Value>,
        changed_by: &str,
    ) -> Result<(), StoreError> {
        let to_text = |v: Option<&Value>| -> Result<Option<String>, StoreError> {
            v.map(|v| serde_json::to_string(v))
                .transpose()
                .map_err(|e| StoreError::Io(e.to_string()))
        };
        conn.execute(
            "INSERT INTO change_log (tbl, row_id, op, before, after, changed_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                table,
                row_id,
                op,
                to_text(before)?,
                to_text(after)?,
                changed_by
            ],
        )?;
        Ok(())
    }

    /// 最近变更（按 seq 倒序，limit 上限防大结果集）。
    pub fn list_changes(&self, table: &str, limit: usize) -> Result<Vec<ChangeEntry>, StoreError> {
        self.table_fields(table)?;
        let conn = self.conn()?;
        let n = limit.clamp(1, 50);
        let mut stmt = conn.prepare(
            "SELECT seq, tbl, row_id, op, before, after, changed_by, changed_at
             FROM change_log WHERE tbl = ?1 ORDER BY seq DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![table, n as i64], |r| {
            Ok(ChangeEntry {
                seq: r.get(0)?,
                table: r.get(1)?,
                row_id: r.get(2)?,
                op: r.get(3)?,
                before: r
                    .get::<_, Option<String>>(4)?
                    .and_then(|s| serde_json::from_str(&s).ok()),
                after: r
                    .get::<_, Option<String>>(5)?
                    .and_then(|s| serde_json::from_str(&s).ok()),
                changed_by: r.get(6)?,
                changed_at: r.get(7)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 撤销该表最近一条变更（insert→删行；update→before 覆盖；delete→before 重建）。
    /// 返回撤销的变更记录；无变更返回 None。撤销本身不写新 change_log。
    pub fn undo_last(&self, table: &str) -> Result<Option<ChangeEntry>, StoreError> {
        self.table_fields(table)?;
        let conn = self.conn()?;
        let last: Option<(i64, String, String, Option<String>, Option<String>, String, String)> =
            conn.query_row(
                "SELECT seq, row_id, op, before, after, changed_by, changed_at
                 FROM change_log WHERE tbl = ?1 ORDER BY seq DESC LIMIT 1",
                [table],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((seq, row_id, op, before, after, changed_by, changed_at)) = last else {
            return Ok(None);
        };
        let tx = conn.unchecked_transaction()?;
        match op.as_str() {
            "insert" => {
                // 撤销插入 = 删行
                tx.execute(&format!("DELETE FROM {table} WHERE id = ?1"), [&row_id])?;
            }
            "update" => {
                // 撤销更新 = 用 before 覆盖该行（保留 id/created_at）
                let before_val = before
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    .ok_or_else(|| StoreError::Io("变更记录缺 before".to_string()))?;
                tx.execute(&format!("DELETE FROM {table} WHERE id = ?1"), [&row_id])?;
                let params = self.row_to_params(table, &before_val, "undo")?;
                let cols: Vec<&str> = params.iter().map(|(k, _)| k.as_str()).collect();
                let placeholders: Vec<String> =
                    (1..=params.len()).map(|i| format!("?{i}")).collect();
                let sql = format!(
                    "INSERT INTO {table} ({}) VALUES ({})",
                    cols.join(", "),
                    placeholders.join(", ")
                );
                let values: Vec<rusqlite::types::Value> =
                    params.into_iter().map(|(_, v)| v).collect();
                tx.execute(&sql, rusqlite::params_from_iter(values.iter()))?;
            }
            "delete" => {
                // 撤销删除 = 用 before 重建行
                let before_val = before
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    .ok_or_else(|| StoreError::Io("变更记录缺 before".to_string()))?;
                let params = self.row_to_params(table, &before_val, "undo")?;
                let cols: Vec<&str> = params.iter().map(|(k, _)| k.as_str()).collect();
                let placeholders: Vec<String> =
                    (1..=params.len()).map(|i| format!("?{i}")).collect();
                let sql = format!(
                    "INSERT INTO {table} ({}) VALUES ({})",
                    cols.join(", "),
                    placeholders.join(", ")
                );
                let values: Vec<rusqlite::types::Value> =
                    params.into_iter().map(|(_, v)| v).collect();
                tx.execute(&sql, rusqlite::params_from_iter(values.iter()))?;
            }
            _ => {
                return Err(StoreError::Io(format!("未知变更类型: {op}")));
            }
        }
        tx.execute("DELETE FROM change_log WHERE seq = ?1", [seq])?;
        tx.commit()?;
        Ok(Some(ChangeEntry {
            seq,
            table: table.to_string(),
            row_id,
            op,
            before: before
                .and_then(|s| serde_json::from_str::<Value>(s.as_str()).ok()),
            after: after
                .and_then(|s| serde_json::from_str::<Value>(s.as_str()).ok()),
            changed_by,
            changed_at,
        }))
    }

    /// 条件命中总数（M4.2 条件引擎用，独立 COUNT 不受 limit 钳制）。
    pub fn count_by_condition(&self, table: &str, condition: &str) -> Result<u64, StoreError> {
        self.table_fields(table)?; // 表存在性
        let sql = format!("SELECT COUNT(*) FROM {table} WHERE ({condition})");
        let conn = self.conn()?;
        let n: i64 = conn.query_row(&sql, [], |r| r.get(0))?;
        Ok(n.max(0) as u64)
    }

    /// 整表替换（.hwx LWW 合并）：事务内清空后按合并结果重灌，返回行数。
    ///
    /// 行对象可携带原 `id` / `created_at` / `updated_at` / `updated_by`
    /// ——行主键不重生成，关系外键引用与 LWW 时间线保持稳定；缺省字段
    /// 走正常生成路径（id 生成新 UUID，时间戳交 DDL default）。
    /// 写权限由调用方（`WorkspaceService::open_writable`）把守；
    /// 不受 allow_delete 约束——合并语义是"更新整表"，不是删除数据。
    /// 任一行失败整体回滚（含 DELETE），不留半替换状态。
    pub fn replace_table_rows(&self, table: &str, rows: &[Value]) -> Result<usize, StoreError> {
        self.table_fields(table)?;
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction()?;
        tx.execute(&format!("DELETE FROM {table}"), [])
            .map_err(StoreError::Sqlite)?;
        for row in rows {
            let obj = row.as_object().ok_or_else(|| {
                StoreError::Validation(format!("{table} 行数据必须是 JSON 对象"))
            })?;
            let mut params = self.row_to_params(table, row, "import")?;
            // 合并保真：显式携带的系统列优先于本次写入的默认值
            let override_sys = |key: &str| -> Option<rusqlite::types::Value> {
                obj.get(key)?.as_str().map(|s| rusqlite::types::Value::Text(s.to_string()))
            };
            if let Some(v) = override_sys("updated_by") {
                params.retain(|(k, _)| k != "updated_by");
                params.push(("updated_by".to_string(), v));
            }
            if !params.iter().any(|(k, _)| k == "updated_by") {
                params.push((
                    "updated_by".to_string(),
                    rusqlite::types::Value::Text("import".to_string()),
                ));
            }
            for key in ["created_at", "updated_at"] {
                if let Some(v) = override_sys(key) {
                    params.retain(|(k, _)| k != key);
                    params.push((key.to_string(), v));
                }
            }

            let cols: Vec<&str> = params.iter().map(|(k, _)| k.as_str()).collect();
            let placeholders: Vec<String> =
                (1..=params.len()).map(|i| format!("?{i}")).collect();
            let sql = format!(
                "INSERT INTO {table} ({}) VALUES ({})",
                cols.join(", "),
                placeholders.join(", ")
            );
            let values: Vec<rusqlite::types::Value> =
                params.into_iter().map(|(_, v)| v).collect();
            tx.execute(&sql, rusqlite::params_from_iter(values.iter()))
                .map_err(StoreError::Sqlite)?;
        }
        tx.commit().map_err(StoreError::Sqlite)?;
        Ok(rows.len())
    }


    fn conn(&self) -> Result<rusqlite::Connection, StoreError> {
        // 防御：工作台目录缺失时自动创建（正常由 builder 建，但目录被
        // 手动删除/迁移后可重建，幂等 no-op）。
        if let Some(parent) = self.db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| StoreError::Io(format!("创建工作台 DB 目录失败: {e}")))?;
        }
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| StoreError::Io(format!("打开工作台 DB 失败: {e}")))?;
        if let Err(e) =
            conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;")
        {
            tracing::warn!("workspace_store: WAL/busy_timeout PRAGMA 失败 ({e}); 继续");
        }
        let _ = conn.execute_batch("PRAGMA foreign_keys = ON;");
        Ok(conn)
    }

    /// 按 schema 建表（幂等）。表名 = 模块 id（已过标识符校验）。
    fn ensure_schema(&self) -> Result<(), StoreError> {
        let conn = self.conn()?;
        for m in &self.schema.modules {
            if let Module::Table { id, fields, .. } = m {
                let mut cols: Vec<String> = vec![
                    "id TEXT PRIMARY KEY".to_string(),
                    "created_at TEXT NOT NULL DEFAULT (datetime('now'))".to_string(),
                    "updated_at TEXT NOT NULL DEFAULT (datetime('now'))".to_string(),
                    "updated_by TEXT".to_string(),
                ];
                let mut checks: Vec<String> = Vec::new();
                for f in fields {
                    cols.push(format!("{} {}", f.id, sqlite_type(f.field_type)));
                    if f.required {
                        // 系统列已 NOT NULL；业务必填列加 NOT NULL 兜底
                        let idx = cols.len() - 1;
                        cols[idx] = format!("{} NOT NULL", cols[idx]);
                    }
                    if f.unique {
                        // 列级内联 UNIQUE（实测 SQLite 不允许表级 UNIQUE 约束
                        // 穿插在列定义之间——"plate TEXT NOT NULL, UNIQUE(plate),
                        // status TEXT" 会报 near "status": syntax error）
                        let idx = cols.len() - 1;
                        cols[idx] = format!("{} UNIQUE", cols[idx]);
                    }
                    if f.is_enum() {
                        let opts: Vec<String> = f
                            .options
                            .as_ref()
                            .unwrap_or(&Vec::new())
                            .iter()
                            .map(|o| format!("'{}'", o.replace('\'', "''")))
                            .collect();
                        checks.push(format!(
                            "CHECK({} IN ({}))",
                            f.id,
                            opts.join(", ")
                        ));
                    }
                }
                let mut sql = format!("CREATE TABLE IF NOT EXISTS {} ({}", id, cols.join(", "));
                if !checks.is_empty() {
                    sql.push_str(", ");
                    sql.push_str(&checks.join(", "));
                }
                sql.push_str(");");
                conn.execute_batch(&sql)
                    .map_err(|e| StoreError::Sqlite(e))?;
            }
        }
        // P6 变更历史：change_log 表（每工作台一份，记录 insert/update/delete）
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS change_log (
                seq        INTEGER PRIMARY KEY AUTOINCREMENT,
                tbl        TEXT NOT NULL,
                row_id     TEXT NOT NULL,
                op         TEXT NOT NULL,
                before     TEXT,
                after      TEXT,
                changed_by TEXT NOT NULL,
                changed_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .map_err(StoreError::Sqlite)?;
        Ok(())
    }

    /// 表模块 id → 字段列表。
    fn table_fields(&self, table: &str) -> Result<&[crate::model::Field], StoreError> {
        for m in &self.schema.modules {
            if let Module::Table { id, fields, .. } = m {
                if id == table {
                    return Ok(fields);
                }
            }
        }
        Err(StoreError::TableNotFound(table.to_string()))
    }

    /// 列名白名单：业务字段 + 系统列。
    fn column_whitelist(&self, table: &str) -> Result<Vec<String>, StoreError> {
        let mut cols: Vec<String> = self
            .table_fields(table)?
            .iter()
            .map(|f| f.id.clone())
            .collect();
        cols.extend(
            ["id", "created_at", "updated_at", "updated_by"]
                .iter()
                .map(|s| s.to_string()),
        );
        Ok(cols)
    }

    /// 校验列名在白名单内。
    fn check_column(&self, table: &str, col: &str) -> Result<(), StoreError> {
        let whitelist = self.column_whitelist(table)?;
        if whitelist.iter().any(|c| c == col) {
            Ok(())
        } else {
            Err(StoreError::UnknownColumn(col.to_string()))
        }
    }

    /// 行 → SQLite 参数：按字段类型校验并转换；返回 (列名, 值) 序列。
    fn row_to_params(
        &self,
        table: &str,
        row: &Value,
        updated_by: &str,
    ) -> Result<Vec<(String, rusqlite::types::Value)>, StoreError> {
        let fields = self.table_fields(table)?;
        let obj = row
            .as_object()
            .ok_or_else(|| StoreError::Validation(format!("{table} 行数据必须是 JSON 对象")))?;

        let mut out: Vec<(String, rusqlite::types::Value)> = Vec::new();
        for f in fields {
            let v = obj.get(&f.id);
            match v {
                None => {
                    if f.required {
                        return Err(StoreError::Validation(format!(
                            "字段[{}]必填但缺失",
                            f.id
                        )));
                    }
                    // 非必填缺省 → 交给 DDL DEFAULT / NULL
                }
                Some(val) => {
                    let converted = convert_value(f, val).map_err(|e| {
                        StoreError::Validation(format!("字段[{}]: {e}", f.id))
                    })?;
                    out.push((f.id.clone(), converted));
                }
            }
        }
        // 系统列。id：调用方显式提供则沿用（merge 导入保留行主键、关系引用
        // 与 LWW 时间线不断裂），否则生成新 UUID。
        let id = obj
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(uuid_v4);
        out.push(("id".to_string(), rusqlite::types::Value::Text(id)));
        out.push(("updated_by".to_string(), rusqlite::types::Value::Text(updated_by.to_string())));
        Ok(out)
    }

    /// patch（update 用）→ SQLite 参数；键须在白名单内。
    fn patch_to_params(
        &self,
        table: &str,
        patch: &Value,
    ) -> Result<Vec<(String, rusqlite::types::Value)>, StoreError> {
        let fields = self.table_fields(table)?;
        let obj = patch
            .as_object()
            .ok_or_else(|| StoreError::Validation("patch 必须是 JSON 对象".to_string()))?;
        let mut out: Vec<(String, rusqlite::types::Value)> = Vec::new();
        for (k, v) in obj {
            self.check_column(table, k)?;
            if k == "id" {
                return Err(StoreError::Validation("不允许修改主键 id".to_string()));
            }
            if matches!(k.as_str(), "created_at" | "updated_at" | "updated_by") {
                return Err(StoreError::Validation(format!(
                    "系统列[{k}]不允许手动修改"
                )));
            }
            let field = fields.iter().find(|f| f.id == *k);
            match field {
                Some(f) => {
                    let converted = convert_value(f, v)
                        .map_err(|e| StoreError::Validation(format!("字段[{k}]: {e}")))?;
                    out.push((k.clone(), converted));
                }
                None => out.push((k.clone(), value_to_sqlite(v.clone()))),
            }
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// 类型转换
// ---------------------------------------------------------------------------

/// 字段类型 → SQLite 列类型。
pub(crate) fn sqlite_type(t: FieldType) -> &'static str {
    match t {
        FieldType::Text | FieldType::Enum | FieldType::Relation | FieldType::Attachment => "TEXT",
        FieldType::Number => "REAL",
        FieldType::Date | FieldType::Datetime => "TEXT",
        FieldType::Boolean => "INTEGER",
    }
}

/// 按字段类型校验并转换 JSON 值 → SQLite 值。
fn convert_value(
    f: &crate::model::Field,
    v: &Value,
) -> Result<rusqlite::types::Value, String> {
    match f.field_type {
        FieldType::Text | FieldType::Attachment => match v {
            Value::String(s) => Ok(rusqlite::types::Value::Text(s.clone())),
            Value::Number(n) => Ok(rusqlite::types::Value::Text(n.to_string())),
            _ => Err(format!("期望文本, 实际 {}", type_name(v))),
        },
        FieldType::Number => match v {
            Value::Number(n) => Ok(rusqlite::types::Value::Real(
                n.as_f64().unwrap_or(f64::NAN),
            )),
            _ => Err(format!("期望数字, 实际 {}", type_name(v))),
        },
        FieldType::Date | FieldType::Datetime => match v {
            Value::String(s) if !s.trim().is_empty() => {
                Ok(rusqlite::types::Value::Text(s.clone()))
            }
            _ => Err(format!("期望日期字符串, 实际 {}", type_name(v))),
        },
        FieldType::Boolean => match v {
            Value::Bool(b) => Ok(rusqlite::types::Value::Integer(if *b { 1 } else { 0 })),
            Value::Number(n) if n.as_i64() == Some(0) || n.as_i64() == Some(1) => {
                Ok(rusqlite::types::Value::Integer(n.as_i64().unwrap()))
            }
            _ => Err(format!("期望布尔, 实际 {}", type_name(v))),
        },
        FieldType::Enum => {
            let s = match v {
                Value::String(s) => s.clone(),
                _ => return Err(format!("期望枚举字符串, 实际 {}", type_name(v))),
            };
            let opts = f.options.as_ref().map(Vec::as_slice).unwrap_or(&[]);
            if !opts.iter().any(|o| o == &s) {
                return Err(format!("枚举值[{s}]不在允许范围 {:?}", opts));
            }
            Ok(rusqlite::types::Value::Text(s))
        }
        FieldType::Relation => match v {
            Value::String(s) if !s.trim().is_empty() => {
                Ok(rusqlite::types::Value::Text(s.clone()))
            }
            _ => Err(format!("期望关系目标 id 字符串, 实际 {}", type_name(v))),
        },
    }
}

/// JSON 值 → SQLite 值（白名单列通用转换）。
fn value_to_sqlite(v: Value) -> rusqlite::types::Value {
    match v {
        Value::Null => rusqlite::types::Value::Null,
        Value::Bool(b) => rusqlite::types::Value::Integer(if b { 1 } else { 0 }),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                rusqlite::types::Value::Integer(i)
            } else {
                rusqlite::types::Value::Real(n.as_f64().unwrap_or(f64::NAN))
            }
        }
        Value::String(s) => rusqlite::types::Value::Text(s),
        Value::Array(_) | Value::Object(_) => {
            rusqlite::types::Value::Text(serde_json::to_string(&v).unwrap_or_default())
        }
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ---------------------------------------------------------------------------
// WorkspaceStore impl
// ---------------------------------------------------------------------------

impl WorkspaceStore for LocalSqliteStore {
    fn query(
        &self,
        table: &str,
        filters: &[Filter],
        sort: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Value>, StoreError> {
        self.table_fields(table)?; // 表存在性
        // 显式白名单列 SELECT —— 列白名单下沉到 SQL 层，未知列根本进不了查询
        let cols = self.column_whitelist(table)?;
        let mut sql = format!("SELECT {} FROM {table}", cols.join(", "));
        let mut params: Vec<rusqlite::types::Value> = Vec::new();
        if !filters.is_empty() {
            let mut wheres: Vec<String> = Vec::new();
            for f in filters {
                self.check_column(table, &f.field)?;
                let placeholder = params.len() + 1;
                if f.op == FilterOp::Contains {
                    let needle = match &f.value {
                        Value::String(s) => format!("%{}%", s.replace('%', "\\%")),
                        other => format!("%{}%", other.to_string()),
                    };
                    params.push(rusqlite::types::Value::Text(needle));
                } else {
                    params.push(value_to_sqlite(f.value.clone()));
                }
                wheres.push(format!("{} {} ?{placeholder}", f.field, f.op.sql()));
            }
            sql.push_str(&format!(" WHERE {}", wheres.join(" AND ")));
        }
        if let Some(s) = sort {
            let (col, desc) = match s.strip_prefix('-') {
                Some(rest) => (rest, true),
                None => (s, false),
            };
            self.check_column(table, col)?;
            sql.push_str(&format!(" ORDER BY {col} {}", if desc { "DESC" } else { "ASC" }));
        }
        // limit=0 表示不限制（LLM 查询默认给上限, 调用方显式放开）
        if limit > 0 {
            sql.push_str(&format!(" LIMIT {limit} OFFSET {offset}"));
        }
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
            row_to_json(r, &cols)
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    fn insert(&self, table: &str, rows: &[Value], updated_by: &str) -> Result<Vec<String>, StoreError> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        self.table_fields(table)?;
        let conn = self.conn()?;
        let mut ids = Vec::with_capacity(rows.len());
        for row in rows {
            let params = self.row_to_params(table, row, updated_by)?;
            let id = params
                .iter()
                .find(|(k, _)| k == "id")
                .and_then(|(_, v)| match v {
                    rusqlite::types::Value::Text(s) => Some(s.clone()),
                    _ => None,
                })
                .ok_or_else(|| StoreError::Validation("缺少 id".to_string()))?;

            let cols: Vec<&str> = params.iter().map(|(k, _)| k.as_str()).collect();
            let placeholders: Vec<String> =
                (1..=params.len()).map(|i| format!("?{i}")).collect();
            let sql = format!(
                "INSERT INTO {table} ({}) VALUES ({})",
                cols.join(", "),
                placeholders.join(", ")
            );
            let values: Vec<rusqlite::types::Value> =
                params.into_iter().map(|(_, v)| v).collect();
            let res = conn.execute(&sql, rusqlite::params_from_iter(values.iter()));
            match res {
                Ok(_) => {
                    // P6 audit：记录插入后的完整行（含默认时间戳）
                    if let Ok(Some(after)) = self.query_row_by_id(table, &id) {
                        let _ = self.log_change(&conn, table, &id, "insert", None, Some(&after), updated_by);
                    }
                    ids.push(id)
                }
                Err(rusqlite::Error::SqliteFailure(e, _)) => {
                    if e.code == rusqlite::ErrorCode::ConstraintViolation {
                        let msg = e.to_string();
                        if msg.contains("UNIQUE") {
                            return Err(StoreError::Unique(msg));
                        }
                        return Err(StoreError::Validation(msg));
                    }
                    return Err(StoreError::Sqlite(rusqlite::Error::SqliteFailure(e, None)));
                }
                Err(e) => return Err(StoreError::Sqlite(e)),
            }
        }
        Ok(ids)
    }

    fn update(
        &self,
        table: &str,
        id: &str,
        patch: &Value,
        updated_by: &str,
    ) -> Result<(), StoreError> {
        self.table_fields(table)?;
        let params = self.patch_to_params(table, patch)?;
        if params.is_empty() {
            return Ok(()); // 空 patch 无操作
        }
        let mut set: Vec<String> = params
            .iter()
            .enumerate()
            .map(|(i, (k, _))| format!("{k} = ?{}", i + 1))
            .collect();
        // updated_at 由 SQL 层维护；updated_by 记录本次操作来源（审计）
        set.push(format!("updated_by = ?{}", params.len() + 1));
        let mut all_params = params;
        all_params.push((
            "updated_by".to_string(),
            rusqlite::types::Value::Text(updated_by.to_string()),
        ));
        all_params.push(("id".to_string(), rusqlite::types::Value::Text(id.to_string())));
        let sql = format!(
            "UPDATE {table} SET {}, updated_at = datetime('now') WHERE id = ?{}",
            set.join(", "),
            all_params.len()
        );
        let conn = self.conn()?;
        let values: Vec<rusqlite::types::Value> =
            all_params.into_iter().map(|(_, v)| v).collect();
        // P6 audit：更新前快照（撤销锚点）
        let before = self.query_row_by_id(table, id)?;
        let n = conn.execute(&sql, rusqlite::params_from_iter(values.iter()))?;
        if n == 0 {
            return Err(StoreError::RowNotFound(id.to_string()));
        }
        // P6 audit：更新后快照 + 记录
        if let Ok(Some(after)) = self.query_row_by_id(table, id) {
            let _ = self.log_change(&conn, table, id, "update", before.as_ref(), Some(&after), updated_by);
        }
        Ok(())
    }

    fn delete(&self, table: &str, id: &str) -> Result<(), StoreError> {
        self.table_fields(table)?;
        let conn = self.conn()?;
        // P6 audit：删除前快照（撤销重建锚点）
        let before = self.query_row_by_id(table, id)?;
        let n = conn.execute(
            &format!("DELETE FROM {table} WHERE id = ?1"),
            [id],
        )?;
        if n == 0 {
            return Err(StoreError::RowNotFound(id.to_string()));
        }
        // P6 audit：记录（before 为删前完整行，after 无）
        if let Some(b) = before {
            let _ = self.log_change(&conn, table, id, "delete", Some(&b), None, "user");
        }
        Ok(())
    }

    fn count(&self, table: &str, filters: &[Filter]) -> Result<u64, StoreError> {
        self.table_fields(table)?;
        let mut sql = format!("SELECT COUNT(*) FROM {table}");
        let mut params: Vec<rusqlite::types::Value> = Vec::new();
        if !filters.is_empty() {
            let mut wheres: Vec<String> = Vec::new();
            for f in filters {
                self.check_column(table, &f.field)?;
                let placeholder = params.len() + 1;
                if f.op == FilterOp::Contains {
                    let needle = match &f.value {
                        Value::String(s) => format!("%{}%", s.replace('%', "\\%")),
                        other => format!("%{}%", other.to_string()),
                    };
                    params.push(rusqlite::types::Value::Text(needle));
                } else {
                    params.push(value_to_sqlite(f.value.clone()));
                }
                wheres.push(format!("{} {} ?{placeholder}", f.field, f.op.sql()));
            }
            sql.push_str(&format!(" WHERE {}", wheres.join(" AND ")));
        }
        let conn = self.conn()?;
        let n: i64 = conn.query_row(&sql, rusqlite::params_from_iter(params.iter()), |r| r.get(0))?;
        Ok(n.max(0) as u64)
    }
}

/// SQLite 行 → JSON 对象（列名来自调用方传入的白名单列，按位置索引对齐）。
fn row_to_json(r: &rusqlite::Row<'_>, cols: &[String]) -> rusqlite::Result<Value> {
    let mut obj = serde_json::Map::new();
    for (i, name) in cols.iter().enumerate() {
        let v = match r.get_ref(i)? {
            rusqlite::types::ValueRef::Null => Value::Null,
            rusqlite::types::ValueRef::Integer(i) => Value::from(i),
            rusqlite::types::ValueRef::Real(f) => serde_json::Number::from_f64(f)
                .map(Value::Number)
                .unwrap_or(Value::Null),
            rusqlite::types::ValueRef::Text(t) => {
                Value::String(String::from_utf8_lossy(t).into_owned())
            }
            rusqlite::types::ValueRef::Blob(b) => {
                Value::String(String::from_utf8_lossy(b).into_owned())
            }
        };
        obj.insert(name.clone(), v);
    }
    Ok(Value::Object(obj))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::WorkspaceSchema;

    fn sample_schema() -> WorkspaceSchema {
        serde_json::from_value(serde_json::json!({
            "name": "车辆管理",
            "modules": [
                { "type": "table", "id": "vehicles", "name": "车辆",
                  "fields": [
                    { "id": "plate", "name": "车牌", "type": "text", "required": true, "unique": true },
                    { "id": "status", "name": "状态", "type": "enum", "options": ["在用", "维修", "停用"] },
                    { "id": "mileage", "name": "里程", "type": "number" },
                    { "id": "active", "name": "在用", "type": "boolean" },
                    { "id": "inspection_date", "name": "年检", "type": "date" }
                  ] }
            ]
        }))
        .expect("schema")
    }

    fn open_temp(dir: &tempfile::TempDir) -> LocalSqliteStore {
        LocalSqliteStore::open(dir.path().join("data.sqlite"), sample_schema()).expect("open")
    }

    #[test]
    fn create_and_insert_query_update_delete() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = open_temp(&dir);

        // 插入两行
        let ids = store
            .insert(
                "vehicles",
                &[
                    serde_json::json!({ "plate": "京A12345", "status": "在用", "mileage": 12000, "active": true, "inspection_date": "2027-03-15" }),
                    serde_json::json!({ "plate": "京B67890", "status": "维修", "mileage": 8000, "active": false }),
                ],
                "user",
            )
            .expect("insert");
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]); // UUID 唯一

        // 查询全部
        let rows = store.query("vehicles", &[], None, 0, 0).expect("query");
        assert_eq!(rows.len(), 2);

        // 过滤：status = 在用
        let rows = store
            .query(
                "vehicles",
                &[Filter { field: "status".into(), op: FilterOp::Eq, value: serde_json::json!("在用") }],
                None,
                0,
                0,
            )
            .expect("query");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["plate"], serde_json::json!("京A12345"));

        // Contains
        let rows = store
            .query(
                "vehicles",
                &[Filter { field: "plate".into(), op: FilterOp::Contains, value: serde_json::json!("A12") }],
                None,
                0,
                0,
            )
            .expect("query");
        assert_eq!(rows.len(), 1);

        // 排序 + limit/offset
        let rows = store
            .query("vehicles", &[], Some("-mileage"), 1, 0)
            .expect("query");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["plate"], serde_json::json!("京A12345"));

        // 计数
        let n = store
            .count(
                "vehicles",
                &[Filter { field: "status".into(), op: FilterOp::Eq, value: serde_json::json!("在用") }],
            )
            .expect("count");
        assert_eq!(n, 1);

        // 更新
        store
            .update(
                "vehicles",
                &ids[0],
                &serde_json::json!({ "status": "停用", "mileage": 13000 }),
                "llm",
            )
            .expect("update");
        let row = store
            .query("vehicles", &[Filter { field: "id".into(), op: FilterOp::Eq, value: serde_json::json!(ids[0]) }], None, 1, 0)
            .expect("query")
            .pop()
            .expect("row");
        assert_eq!(row["status"], serde_json::json!("停用"));
        assert_eq!(row["updated_by"], serde_json::json!("llm"));
        // Number 列映射 REAL，读出为 f64（13000.0 == 13000）
        assert_eq!(row["mileage"], serde_json::json!(13000.0));

        // 删除
        store.delete("vehicles", &ids[1]).expect("delete");
        let n = store.count("vehicles", &[]).expect("count");
        assert_eq!(n, 1);
    }

    #[test]
    fn required_field_missing_rejected() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = open_temp(&dir);
        let err = store
            .insert("vehicles", &[serde_json::json!({ "status": "在用" })], "user")
            .unwrap_err()
            .to_string();
        assert!(err.contains("plate"), "{err}");
    }

    #[test]
    fn bad_enum_rejected() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = open_temp(&dir);
        let err = store
            .insert(
                "vehicles",
                &[serde_json::json!({ "plate": "X", "status": "报废" })],
                "user",
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("枚举"), "{err}");
    }

    #[test]
    fn bad_type_rejected() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = open_temp(&dir);
        // mileage 是 number, 传字符串
        let err = store
            .insert(
                "vehicles",
                &[serde_json::json!({ "plate": "X", "mileage": "not-a-number" })],
                "user",
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("期望数字"), "{err}");
    }

    #[test]
    fn unique_conflict_rejected() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = open_temp(&dir);
        store
            .insert("vehicles", &[serde_json::json!({ "plate": "京A1" })], "user")
            .expect("first");
        let err = store
            .insert("vehicles", &[serde_json::json!({ "plate": "京A1" })], "user")
            .unwrap_err()
            .to_string();
        assert!(err.contains("UNIQUE") || err.contains("唯一"), "{err}");
    }

    #[test]
    fn unknown_column_filter_rejected() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = open_temp(&dir);
        let err = store
            .query(
                "vehicles",
                &[Filter { field: "hack; DROP TABLE vehicles;".into(), op: FilterOp::Eq, value: serde_json::json!(1) }],
                None,
                0,
                0,
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("未知列"), "{err}");
    }

    #[test]
    fn unknown_table_rejected() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = open_temp(&dir);
        let err = store.query("ghost", &[], None, 0, 0).unwrap_err().to_string();
        assert!(err.contains("ghost"), "{err}");
    }

    #[test]
    fn patch_unknown_column_rejected() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = open_temp(&dir);
        let id = store
            .insert("vehicles", &[serde_json::json!({ "plate": "京A1" })], "user")
            .expect("insert")
            .pop()
            .expect("id");
        let err = store
            .update("vehicles", &id, &serde_json::json!({ "evil": 1 }), "user")
            .unwrap_err()
            .to_string();
        assert!(err.contains("未知列"), "{err}");
    }

    #[test]
    fn isolation_between_workspaces() {
        // 两个工作台各自独立 DB 文件 → 互不可见
        let dir1 = tempfile::tempdir().expect("tmp");
        let dir2 = tempfile::tempdir().expect("tmp");
        let store1 = open_temp(&dir1);
        let store2 = LocalSqliteStore::open(
            dir2.path().join("data.sqlite"),
            sample_schema(),
        )
        .expect("open2");

        store1
            .insert("vehicles", &[serde_json::json!({ "plate": "京A1" })], "user")
            .expect("insert1");
        let n2 = store2.count("vehicles", &[]).expect("count2");
        assert_eq!(n2, 0, "工作台 2 不应看到工作台 1 的数据");
    }

    #[test]
    fn schema_validation_failure_rejected_at_open() {
        let dir = tempfile::tempdir().expect("tmp");
        // 非法 schema（Enum 无 options）→ open 报 Validation 错误
        let bad: WorkspaceSchema = serde_json::from_value(serde_json::json!({
            "name": "bad",
            "modules": [
                { "type": "table", "id": "a", "name": "A",
                  "fields": [{ "id": "st", "name": "状态", "type": "enum" }] }
            ]
        }))
        .expect("schema");
        let err = LocalSqliteStore::open(dir.path().join("x.sqlite"), bad)
            .unwrap_err()
            .to_string();
        assert!(err.contains("options"), "{err}");
    }

    // ---------------------------------------------------------------------
    // 关键字搜索（P1）
    // ---------------------------------------------------------------------

    fn seed_vehicles(store: &LocalSqliteStore) {
        store
            .insert(
                "vehicles",
                &[
                    serde_json::json!({ "plate": "京A12345", "status": "在用", "mileage": 12000, "active": true, "inspection_date": "2027-03-15" }),
                    serde_json::json!({ "plate": "京B67890", "status": "维修", "mileage": 8000, "active": false }),
                    serde_json::json!({ "plate": "沪C00001", "status": "在用", "mileage": 50000, "active": true }),
                ],
                "user",
            )
            .expect("seed");
    }

    #[test]
    fn search_text_matches_any_text_field() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = open_temp(&dir);
        seed_vehicles(&store);

        // 命中 text 字段（plate 含 京A）
        let rows = store.search_text("vehicles", "京A", 10, 0).expect("search");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["plate"], serde_json::json!("京A12345"));

        // 命中 enum 字段（status=维修）
        let rows = store.search_text("vehicles", "维修", 10, 0).expect("search2");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["plate"], serde_json::json!("京B67890"));

        // 跨字段 OR：搜索词可匹配任意文本类字段
        let rows = store.search_text("vehicles", "0", 10, 0).expect("search3");
        assert!(rows.len() >= 2, "plate 含 0 的行应被命中: {}", rows.len());
    }

    #[test]
    fn search_text_ignores_non_text_fields_and_empty_keyword() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = open_temp(&dir);
        seed_vehicles(&store);

        // number 字段不参与搜索（12000 是 mileage 值，不命中）
        let rows = store.search_text("vehicles", "12000", 10, 0).expect("search");
        assert!(rows.is_empty(), "number 字段不应被搜索");

        // 空 keyword → 空结果
        let rows = store.search_text("vehicles", "", 10, 0).expect("empty");
        assert!(rows.is_empty());
        let rows = store.search_text("vehicles", "   ", 10, 0).expect("blank");
        assert!(rows.is_empty());
    }

    #[test]
    fn count_search_text_matches_search() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = open_temp(&dir);
        seed_vehicles(&store);

        let n = store.count_search_text("vehicles", "在用").expect("count");
        assert_eq!(n, 2, "status=在用 两行");

        // 空 keyword → 全表数
        let n = store.count_search_text("vehicles", "").expect("all");
        assert_eq!(n, 3);
    }

    // ---------------------------------------------------------------------
    // 变更历史 + 撤销（P6）
    // ---------------------------------------------------------------------

    #[test]
    fn change_log_records_ops_in_order() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = open_temp(&dir);
        seed_vehicles(&store);

        // insert 产生一条 insert 记录
        let changes = store.list_changes("vehicles", 20).expect("list");
        assert_eq!(changes.len(), 3, "三行插入各一条");
        assert!(changes.iter().all(|c| c.op == "insert"));

        // update 产生 update 记录（倒序第一条）
        let rows = store.query("vehicles", &[], None, 1, 0).expect("q");
        let rid = rows[0]["id"].as_str().unwrap().to_string();
        store
            .update("vehicles", &rid, &serde_json::json!({ "status": "维修" }), "user")
            .expect("update");
        let changes = store.list_changes("vehicles", 20).expect("list2");
        assert_eq!(changes[0].op, "update");
        assert_eq!(changes[0].row_id, rid);
        assert!(changes[0].before.is_some(), "update 应有 before");
        assert!(changes[0].after.is_some(), "update 应有 after");

        // delete 产生 delete 记录（before 快照）
        store.delete("vehicles", &rid).expect("delete");
        let changes = store.list_changes("vehicles", 20).expect("list3");
        assert_eq!(changes[0].op, "delete");
        assert!(changes[0].before.is_some(), "delete 应有 before 快照");
        assert!(changes[0].after.is_none(), "delete 无 after");
    }

    #[test]
    fn undo_insert_removes_row() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = open_temp(&dir);
        seed_vehicles(&store);

        let ids = store
            .insert(
                "vehicles",
                &[serde_json::json!({ "plate": "京D99999", "status": "在用" })],
                "user",
            )
            .expect("insert");
        let new_id = &ids[0];
        // 撤销最近一条 insert → 该行消失
        let undone = store.undo_last("vehicles").expect("undo").expect("有记录");
        assert_eq!(undone.op, "insert");
        assert_eq!(undone.row_id, *new_id);
        let rows = store
            .query(
                "vehicles",
                &[Filter { field: "id".into(), op: FilterOp::Eq, value: serde_json::json!(new_id) }],
                None,
                1,
                0,
            )
            .expect("q");
        assert!(rows.is_empty(), "撤销后行应消失");
        // 撤销记录本身也清除：seed 3 条 + 新增 1 条 = 4 条 → 3 条
        let cl = store.list_changes("vehicles", 20).expect("cl");
        assert_eq!(cl.len(), 3, "被撤销的记录应从 change_log 清除（seed 3 条仍在）");
    }

    #[test]
    fn undo_update_restores_before() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = open_temp(&dir);
        seed_vehicles(&store);

        let rows = store.query("vehicles", &[], None, 1, 0).expect("q");
        let rid = rows[0]["id"].as_str().unwrap().to_string();
        let old_status = rows[0]["status"].clone();
        store
            .update("vehicles", &rid, &serde_json::json!({ "status": "维修" }), "user")
            .expect("update");

        store.undo_last("vehicles").expect("undo").expect("有记录");
        let after = store
            .query(
                "vehicles",
                &[Filter { field: "id".into(), op: FilterOp::Eq, value: serde_json::json!(rid) }],
                None,
                1,
                0,
            )
            .expect("q");
        assert_eq!(after[0]["status"], old_status, "撤销更新后恢复原值");
    }

    #[test]
    fn undo_delete_restores_row_with_id() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = open_temp(&dir);
        seed_vehicles(&store);

        let rows = store.query("vehicles", &[], None, 1, 0).expect("q");
        let rid = rows[0]["id"].as_str().unwrap().to_string();
        let plate = rows[0]["plate"].clone();
        store.delete("vehicles", &rid).expect("delete");

        store.undo_last("vehicles").expect("undo").expect("有记录");
        let after = store
            .query(
                "vehicles",
                &[Filter { field: "id".into(), op: FilterOp::Eq, value: serde_json::json!(rid) }],
                None,
                1,
                0,
            )
            .expect("q");
        assert_eq!(after.len(), 1, "撤销删除后行重建");
        assert_eq!(after[0]["plate"], plate, "重建保留原 id 与字段");
    }
}
