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

    /// 按主键删除。`updated_by` 记操作来源（user / llm / automation）。
    fn delete(&self, table: &str, id: &str, updated_by: &str) -> Result<(), StoreError>;

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

    /// LIKE 通配符转义（H4 修复）：% / _ / \ 在用户输入里都是 LIKE 通配符，
    /// 必须用 `\` 前缀转义，同时 SQL 必须声明 `ESCAPE '\'`。搜索路径的占位符
    /// `format!("%{escaped}%")` 把通配字符放回外壳里——既保留"前后都模糊"语义，
    /// 又确保输入里出现"%"不会被当成新通配模式匹配全表。
    fn escape_like(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '\\' | '%' | '_' => {
                    out.push('\\');
                    out.push(c);
                }
                _ => out.push(c),
            }
        }
        out
    }

    /// FTS5 MATCH 模式构造（M1 修复）：把用户输入包成"整短语"做前缀匹配。
    /// - 双引号把整段文本变成 phrase（按 token 顺序精确匹配，而非"任一词命中"）；
    /// - 末尾 `*` 让最后一个词变前缀（搜 "bei" 命中 "北京"/"背景"）；
    /// - 内部的 `"` 转义为 `""`（FTS5 phrase 内的引号转义规则）。
    /// 这样用户搜 "10% off" 不会因为 `%` 被 FTS 解析器当成语法；搜"京A" 命中"京A12345"。
    /// 极端输入（仅空白/控制字符）返回空串，调用方会回退到 LIKE。
    fn format_fts_pattern(s: &str) -> String {
        let s = s.trim();
        if s.is_empty() {
            return String::new();
        }
        let escaped = s.replace('"', "\"\"");
        format!("\"{}\"*", escaped)
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

    /// L5 upsert：按表 schema 中第一个 `unique=true` 字段做"存在则 UPDATE、不存在则 INSERT"。
    /// 用于工作台增量导入（设计 §17.1）：用户每月再导同一份人员/车辆名单，
    /// 按主键自动合并行；不重不漏，关系外键引用保留（LWW 表 id 不变）。
    /// 找不到 unique 字段时降级为"全表替换"（等价于 merge 模式），不会静默丢失重复。
    /// 返回 (inserted, updated) 行数。
    pub fn upsert_by_unique(
        &self,
        table: &str,
        rows: &[Value],
        updated_by: &str,
    ) -> Result<(usize, usize), StoreError> {
        use std::collections::HashSet;
        self.table_fields(table)?;
        let fields = self.table_fields(table)?.to_vec();
        // 找 unique 字段：取第一个 unique=true
        let unique_field = fields.iter().find(|f| f.unique).map(|f| f.id.clone());
        let unique_field = match unique_field {
            Some(u) => u,
            None => {
                // 无 unique 字段：降级为整表替换（保持向后兼容）
                let n = self.replace_table_rows(table, rows)?;
                return Ok((n, 0));
            }
        };
        // 列名白名单校验
        self.check_column(table, &unique_field)?;
        let cols = self.column_whitelist(table)?;
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction()?;
        let mut inserted = 0usize;
        let mut updated = 0usize;
        for incoming in rows {
            // 取 unique 字段值（不接 row_to_params，避免必填/类型校验挡道）
            let key_val = incoming
                .as_object()
                .and_then(|o| o.get(&unique_field))
                .and_then(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Number(n) => Some(n.to_string()),
                    Value::Bool(b) => Some(b.to_string()),
                    Value::Null => None,
                    _ => Some(v.to_string()),
                });
            let Some(key_val) = key_val else {
                // unique 字段为 null → 跳过（不允许空主键 upsert）
                continue;
            };
            // 查现有 id
            let sql = format!(
                "SELECT id FROM {table} WHERE {unique_field} = ?1 LIMIT 1"
            );
            let existing_id: Option<String> = tx
                .query_row(&sql, [&key_val], |r| r.get::<_, String>(0))
                .optional()
                .map_err(StoreError::Sqlite)?;
            if let Some(id) = existing_id {
                // UPDATE：用 incoming 全部字段（不含 id）覆盖
                let patch = Value::Object(
                    incoming
                        .as_object()
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|(k, _)| k != "id")
                        .collect(),
                );
                let params = self.patch_to_params(table, &patch)?;
                if !params.is_empty() {
                    let mut set: Vec<String> = params
                        .iter()
                        .enumerate()
                        .map(|(i, (k, _))| format!("{k} = ?{}", i + 1))
                        .collect();
                    set.push(format!("updated_by = ?{}", params.len() + 1));
                    let mut all_params = params;
                    all_params.push((
                        "updated_by".to_string(),
                        rusqlite::types::Value::Text(updated_by.to_string()),
                    ));
                    all_params.push((
                        "id".to_string(),
                        rusqlite::types::Value::Text(id.clone()),
                    ));
                    let update_sql = format!(
                        "UPDATE {table} SET {}, updated_at = datetime('now') WHERE id = ?{}",
                        set.join(", "),
                        all_params.len()
                    );
                    let values: Vec<rusqlite::types::Value> =
                        all_params.into_iter().map(|(_, v)| v).collect();
                    tx.execute(&update_sql, rusqlite::params_from_iter(values.iter()))?;
                    let after = self.query_row_by_id(table, &id)?;
                    if let Some(ref after_val) = after {
                        let _ = self.log_change(
                            &tx,
                            table,
                            &id,
                            "update",
                            None,
                            Some(after_val),
                            updated_by,
                        );
                    }
                }
                updated += 1;
            } else {
                // INSERT
                let params = self.row_to_params(table, incoming, updated_by)?;
                // 入库后查行 id 写 change_log（"insert"）
                let cols2: Vec<&str> =
                    params.iter().map(|(k, _)| k.as_str()).collect();
                let placeholders: Vec<String> =
                    (1..=params.len()).map(|i| format!("?{i}")).collect();
                let ins_sql = format!(
                    "INSERT INTO {table} ({}) VALUES ({})",
                    cols2.join(", "),
                    placeholders.join(", ")
                );
                let values: Vec<rusqlite::types::Value> =
                    params.clone().into_iter().map(|(_, v)| v).collect();
                tx.execute(&ins_sql, rusqlite::params_from_iter(values.iter()))?;
                let new_id = params
                    .iter()
                    .find(|(k, _)| k == "id")
                    .and_then(|(_, v)| match v {
                        rusqlite::types::Value::Text(s) => Some(s.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                if !new_id.is_empty() {
                    if let Ok(Some(after)) = self.query_row_by_id(table, &new_id) {
                        let _ = self.log_change(
                            &tx,
                            table,
                            &new_id,
                            "insert",
                            None,
                            Some(&after),
                            updated_by,
                        );
                    }
                }
                inserted += 1;
            }
        }
        tx.commit()?;
        let _ = cols; // suppress unused warning if compiler complains
        let _ = HashSet::<()>::new(); // keep use import alive across feature combos
        Ok((inserted, updated))
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

    /// 关键字搜索（P1 前端筛选）——优先走 FTS5 虚表（设计 §17.7），无虚表时回退
    /// LIKE 跨文本字段 OR。用户输入的 `%` / `_` / `\` 会被转义，构造 FTS5 MATCH
    /// 时用双引号做整短语 + `*` 后缀（防 FTS 解析器把标点当语法）。空 keyword
    /// 直接返回全表（与 count 路径对称）。
    pub fn search_text(
        &self,
        table: &str,
        keyword: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Value>, StoreError> {
        if keyword.trim().is_empty() {
            return self.query(table, &[], None, limit, offset);
        }
        let fields = self.table_fields(table)?;
        let cols = self.column_whitelist(table)?;
        let conn = self.conn()?;
        // FTS5 优先：表存在 `<table>_fts` 时用 MATCH 查 row_id，再回主表取全行。
        let fts_table = format!("{table}_fts");
        let has_fts: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [&fts_table],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false);
        if has_fts {
            let pattern = Self::format_fts_pattern(keyword);
            // 必须按 cols（column_whitelist）顺序 SELECT —— CREATE TABLE 的
            // 系统列（id/created_at/updated_at/updated_by）在前，业务字段在后；
            // row_to_json 按 cols 下标读，列序必须一致否则行内容错位。
            let select_cols = cols
                .iter()
                .map(|c| format!("{table}.{c}"))
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "SELECT {select_cols} FROM {table} INNER JOIN {fts} AS fts ON {table}.id = fts.row_id
                 WHERE {fts} MATCH ?1 ORDER BY rank LIMIT {limit} OFFSET {offset}",
                fts = fts_table,
                limit = if limit == 0 { 1000000 } else { limit },
            );
            let mut stmt = match conn.prepare(&sql) {
                Ok(s) => s,
                Err(_) => {
                    // FTS5 解析失败（用户输入含特殊序列）→ 降级 LIKE
                    return self.search_text_like_fallback(&conn, table, &fields, &cols, keyword, limit, offset);
                }
            };
            let rows = match stmt.query_map(rusqlite::params![pattern], |r| row_to_json(r, &cols)) {
                Ok(r) => r,
                Err(_) => {
                    return self.search_text_like_fallback(&conn, table, &fields, &cols, keyword, limit, offset);
                }
            };
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            return Ok(out);
        }
        // 回退 LIKE（无 FTS 虚表的表 / 旧工作台）
        self.search_text_like_fallback(&conn, table, &fields, &cols, keyword, limit, offset)
    }

    /// FTS5 不可用时的 LIKE 回退（与原 P1 行为等价，但用 ESCAPE 转义）。
    fn search_text_like_fallback(
        &self,
        conn: &rusqlite::Connection,
        table: &str,
        fields: &[crate::model::Field],
        cols: &[String],
        keyword: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Value>, StoreError> {
        let searchable: Vec<&str> = fields
            .iter()
            .filter(|f| matches!(f.field_type, FieldType::Text | FieldType::Enum))
            .map(|f| f.id.as_str())
            .collect();
        if searchable.is_empty() {
            return Ok(Vec::new());
        }
        let escaped = Self::escape_like(keyword);
        let needle = format!("%{escaped}%");
        let mut sql = format!("SELECT {} FROM {table} WHERE ", cols.join(", "));
        let wheres: Vec<String> = searchable
            .iter()
            .enumerate()
            .map(|(i, col)| format!("{col} LIKE ?{} ESCAPE '\\'", i + 1))
            .collect();
        sql.push_str(&wheres.join(" OR "));
        if limit > 0 {
            sql.push_str(&format!(" LIMIT {limit} OFFSET {offset}"));
        }
        let params: Vec<rusqlite::types::Value> = (0..searchable.len())
            .map(|_| rusqlite::types::Value::Text(needle.clone()))
            .collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
            row_to_json(r, cols)
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 关键字搜索命中总数（分页用），空 keyword 返回全表数。
    pub fn count_search_text(&self, table: &str, keyword: &str) -> Result<u64, StoreError> {
        if keyword.trim().is_empty() {
            return self.count(table, &[]);
        }
        let fields = self.table_fields(table)?;
        let fts_table = format!("{table}_fts");
        let conn = self.conn()?;
        let has_fts: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [&fts_table],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false);
        if has_fts {
            let pattern = Self::format_fts_pattern(keyword);
            let sql = format!("SELECT COUNT(*) FROM {fts} WHERE {fts} MATCH ?1", fts = fts_table);
            let mut stmt = match conn.prepare(&sql) {
                Ok(s) => s,
                Err(_) => return self.count_search_text_like_fallback(table, &fields, keyword),
            };
            if let Ok(n) = stmt.query_row(rusqlite::params![pattern], |r| r.get::<_, i64>(0)) {
                return Ok(n.max(0) as u64);
            }
            return self.count_search_text_like_fallback(table, &fields, keyword);
        }
        self.count_search_text_like_fallback(table, &fields, keyword)
    }

        fn count_search_text_like_fallback(
        &self,
        table: &str,
        fields: &[crate::model::Field],
        keyword: &str,
    ) -> Result<u64, StoreError> {
        let searchable: Vec<&str> = fields
            .iter()
            .filter(|f| matches!(f.field_type, FieldType::Text | FieldType::Enum))
            .map(|f| f.id.as_str())
            .collect();
        if searchable.is_empty() {
            return Ok(0);
        }
        let conn = self.conn()?;
        let escaped = Self::escape_like(keyword);
        let needle = format!("%{escaped}%");
        let mut sql = format!("SELECT COUNT(*) FROM {table} WHERE ");
        let wheres: Vec<String> = searchable
            .iter()
            .enumerate()
            .map(|(i, col)| format!("{col} LIKE ?{} ESCAPE '\\'", i + 1))
            .collect();
        sql.push_str(&wheres.join(" OR "));
        let params: Vec<rusqlite::types::Value> = (0..searchable.len())
            .map(|_| rusqlite::types::Value::Text(needle.clone()))
            .collect();
        conn.query_row(&sql, rusqlite::params_from_iter(params.iter()), |r| r.get(0))
            .map(|n: i64| n.max(0) as u64)
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

    /// 最近变更（按 seq 倒序，limit 上限防大结果集；已撤销的隐藏）。
    pub fn list_changes(&self, table: &str, limit: usize) -> Result<Vec<ChangeEntry>, StoreError> {
        self.table_fields(table)?;
        let conn = self.conn()?;
        let n = limit.clamp(1, 50);
        let mut stmt = conn.prepare(
            "SELECT seq, tbl, row_id, op, before, after, changed_by, changed_at
             FROM change_log WHERE tbl = ?1 AND undone_at IS NULL
             ORDER BY seq DESC LIMIT ?2",
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

    /// M2：手动清理 change_log。删除早于 `older_than_days` 天的记录 + 已撤销
    /// （`undone_at IS NOT NULL`）的"远旧"记录——前者按时间老化，后者保留历史的
    /// 同时不让"已撤销"无限堆积（`keep_recent_undone` 留最后 N 条）。
    /// 返回被删除的记录数（供端点回报）。事务内执行，避免部分删除。
    pub fn purge_change_log(
        &self,
        older_than_days: i64,
        keep_recent_undone: usize,
    ) -> Result<usize, StoreError> {
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction()?;
        // 子查询取"未撤销最近 N 条 seq"白名单——超出白名单的已撤销行可清理。
        // 负号表示 `older_than_days` 之前的（SQLite datetime 修改器语法）。
        let n = tx.execute(
            "DELETE FROM change_log
             WHERE (changed_at < datetime('now', '-' || ?1 || ' days'))
                OR (undone_at IS NOT NULL
                    AND seq NOT IN (
                        SELECT seq FROM change_log
                        WHERE undone_at IS NULL
                        ORDER BY seq DESC LIMIT ?2
                    ))",
            rusqlite::params![older_than_days, keep_recent_undone as i64],
        )?;
        tx.commit()?;
        Ok(n)
    }

    /// 把撤销快照（before JSON）转成 INSERT 用的参数列，**保留原 created_at / updated_at**。
/// `replace_table_rows` 已经有了类似逻辑（"合并保真"），撤销路径复用同一个心智模型：
/// 业务字段走严格转换，系统时间戳从快照里挖出来作为显式覆盖，避免 DDL DEFAULT 抹掉历史。
/// `updated_by` 取传入值（H5 用 `changed_by` 保留操作来源追溯）。
    fn restore_row_params(
        &self,
        table: &str,
        before_val: &Value,
        updated_by: &str,
    ) -> Result<Vec<(String, rusqlite::types::Value)>, StoreError> {
        let mut params = self.row_to_params(table, before_val, updated_by)?;
        let obj = before_val
            .as_object()
            .ok_or_else(|| StoreError::Validation("undo 快照不是 JSON 对象".to_string()))?;
        let override_sys = |key: &str| -> Option<rusqlite::types::Value> {
            obj.get(key)
                .and_then(|v| v.as_str())
                .map(|s| rusqlite::types::Value::Text(s.to_string()))
        };
        if let Some(v) = override_sys("created_at") {
            params.retain(|(k, _)| k != "created_at");
            params.push(("created_at".to_string(), v));
        }
        if let Some(v) = override_sys("updated_at") {
            params.retain(|(k, _)| k != "updated_at");
            params.push(("updated_at".to_string(), v));
        }
        Ok(params)
    }

    /// 撤销该表最近一条**未撤销**的变更（H1 保留 created_at/updated_at；H5 撤销不删行）。
    /// - `insert` → DELETE row
    /// - `update` / `delete` → DELETE row + INSERT 用 before 重建（保留原时间戳）
    /// 撤销本身只置 `undone_at` 时间戳，不删 change_log 行——让"撤销最近 N 步"在历史里
    /// 依然可追溯，前端 `list_changes` 自动过滤已撤销的。
    /// 返回变更记录的快照（调用方无需再读 change_log）。
    pub fn undo_last(&self, table: &str) -> Result<Option<ChangeEntry>, StoreError> {
        self.table_fields(table)?;
        let conn = self.conn()?;
        let last: Option<(i64, String, String, Option<String>, Option<String>, String, String)> =
            conn.query_row(
                "SELECT seq, row_id, op, before, after, changed_by, changed_at
                 FROM change_log WHERE tbl = ?1 AND undone_at IS NULL
                 ORDER BY seq DESC LIMIT 1",
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
                tx.execute(&format!("DELETE FROM {table} WHERE id = ?1"), [&row_id])?;
            }
            "update" | "delete" => {
                let before_val = before
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    .ok_or_else(|| StoreError::Io("变更记录缺 before".to_string()))?;
                tx.execute(&format!("DELETE FROM {table} WHERE id = ?1"), [&row_id])?;
                let params = self.restore_row_params(table, &before_val, &changed_by)?;
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
        // H5：撤销标记而不是删除；保留全部审计链供后续 redo / 历史展示。
        tx.execute(
            "UPDATE change_log SET undone_at = datetime('now') WHERE seq = ?1",
            [seq],
        )?;
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

    /// 分组计数（仪表盘 chart 组件用）。
    ///
    /// 按指定字段分组，统计每个分组出现的行数；返回 (label, count) 序列，
    /// 按 count 倒序、label 升序兜底，最多 `limit` 条。`group_by` 走
    /// `check_column` 列名白名单校验，杜绝 SQL 注入。NULL / 空字符串被
    /// 归并为"（空）"以保证前端图表 X 轴无空槽。
    pub fn count_group_by(
        &self,
        table: &str,
        group_by: &str,
        limit: usize,
    ) -> Result<Vec<(String, u64)>, StoreError> {
        self.check_column(table, group_by)?;
        let cap = limit.clamp(1, 1000);
        let sql = format!(
            "SELECT COALESCE(NULLIF(CAST({group_by} AS TEXT), ''), '（空）') AS k, COUNT(*) AS c
             FROM {table}
             GROUP BY k
             ORDER BY c DESC, k ASC
             LIMIT {cap}",
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map([], |r| {
                let k: String = r.get(0)?;
                let c: i64 = r.get(1)?;
                Ok((k, c.max(0) as u64))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
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
        // P6 变更历史：change_log 表（每工作台一份，记录 insert/update/delete）。
        // `undone_at`（H5）：撤销不再物理删除变更记录，只置撤销时间戳——
        // 让"撤销最近 N 步"在历史里依然可追溯，避免连点两次撤销把审计
        // 也清掉。前端 list_changes 只看未撤销的；server 端只算未撤销的撤销链。
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS change_log (
                seq        INTEGER PRIMARY KEY AUTOINCREMENT,
                tbl        TEXT NOT NULL,
                row_id     TEXT NOT NULL,
                op         TEXT NOT NULL,
                before     TEXT,
                after      TEXT,
                changed_by TEXT NOT NULL,
                changed_at TEXT NOT NULL DEFAULT (datetime('now')),
                undone_at  TEXT
            );",
        )
        .map_err(StoreError::Sqlite)?;
        // 幂等迁移：旧库加 undone_at 列（SQLite 不支持 IF NOT EXISTS on ADD COLUMN）
        let has_undone: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('change_log') WHERE name = 'undone_at'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if has_undone == 0 {
            let _ = conn.execute_batch("ALTER TABLE change_log ADD COLUMN undone_at TEXT");
        }
        // M1 FTS5 全文索引：每张含 Text 字段的表配一个 `<table>_fts` 虚表 + 三触发器。
        // FTS5 走 SQLite 内置，无新依赖。词法器 unicode61 + 大小写不敏感，
        // 对中英文都按 unicode 字符切分（设计 §17.7）。
        for m in &self.schema.modules {
            if let Module::Table { id, fields, .. } = m {
                if let Err(e) = self.ensure_fts_schema(&conn, id, fields) {
                    // FTS5 schema 错误不应阻塞主表——降级为只 LIKE 搜索。
                    tracing::warn!(table = %id, error = %e, "FTS5 初始化失败，search 将回退 LIKE");
                }
            }
        }
        Ok(())
    }

    /// 创建 / 重建单表的 FTS5 虚表 + 同步触发器 + 存量回填。
    /// 表名 / 列名均来自 schema（已过标识符白名单）——直接拼进 DDL 是安全的。
    fn ensure_fts_schema(
        &self,
        conn: &rusqlite::Connection,
        table: &str,
        fields: &[crate::model::Field],
    ) -> Result<(), StoreError> {
        // 索引范围：Text + Enum（枚举值是字符串，可按词匹配）。
        // Date/Datetime/Number/Boolean/Relation/Attachment 不参与（语义与全文无关）。
        let text_cols: Vec<&str> = fields
            .iter()
            .filter(|f| matches!(f.field_type, FieldType::Text | FieldType::Enum))
            .map(|f| f.id.as_str())
            .collect();
        if text_cols.is_empty() {
            let _ = conn.execute_batch(&format!("DROP TABLE IF EXISTS {table}_fts"));
            return Ok(());
        }
        let fts = format!("{table}_fts");
        // 重建策略：旧虚表 schema 可能与新 schema 不一致（字段增删），DROP + CREATE。
        // 重建后用 INSERT INTO fts SELECT … FROM main 回填。
        conn.execute_batch(&format!("DROP TABLE IF EXISTS {fts}"))?;
        // FTS5 DDL：row_id UNINDEXED（不索引，匹配时只作 hit 列表）
        let col_ddl = text_cols
            .iter()
            .map(|c| format!("{c}"))
            .collect::<Vec<_>>()
            .join(", ");
        let create_sql = format!(
            "CREATE VIRTUAL TABLE {fts} USING fts5(row_id UNINDEXED, {col_ddl}, tokenize='unicode61')"
        );
        conn.execute_batch(&create_sql)?;
        // 触发器：主表写入/更新/删除时同步 FTS。
        // 触发器 DDL 内嵌 SELECT NEW.c1 … 必须逐列展开——所以按 text_cols 动态拼。
        let insert_cols = text_cols.join(", ");
        let insert_vals = text_cols.iter().map(|c| format!("NEW.{c}")).collect::<Vec<_>>().join(", ");
        let update_sets = text_cols.iter().map(|c| format!("{c} = NEW.{c}")).collect::<Vec<_>>().join(", ");
        let ddl_triggers = format!(
            "CREATE TRIGGER IF NOT EXISTS {table}_ai AFTER INSERT ON {table} BEGIN
                INSERT INTO {fts}(row_id, {insert_cols}) VALUES (NEW.id, {insert_vals});
            END;
            CREATE TRIGGER IF NOT EXISTS {table}_ad AFTER DELETE ON {table} BEGIN
                DELETE FROM {fts} WHERE row_id = OLD.id;
            END;
            CREATE TRIGGER IF NOT EXISTS {table}_au AFTER UPDATE ON {table} BEGIN
                UPDATE {fts} SET {update_sets} WHERE row_id = NEW.id;
            END;"
        );
        conn.execute_batch(&ddl_triggers)?;
        // 存量回填：FTS5 不能直接 content-link（会与 triggers 冲突），所以
        // 显式 SELECT INTO 把现有行灌进 FTS 表。Schema 漂移（字段被删）会让
        // 这里 SELECT 报错——降级为 warn，搜索回退 LIKE。
        let backfill_cols = text_cols.join(", ");
        let backfill = format!(
            "INSERT INTO {fts}(row_id, {backfill_cols}) SELECT id, {backfill_cols} FROM {table}"
        );
        conn.execute_batch(&backfill)?;
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

    /// 收集一行中所有 `attachment` 字段引用的磁盘文件路径（`files/<uuid>.<ext>`），
    /// 用于删除/撤销时清理孤儿附件（H3）。非 attachment 字段忽略。
    fn attachment_paths_in_row(_table: &str, row: &Value) -> Vec<String> {
        let Some(obj) = row.as_object() else {
            return Vec::new();
        };
        // FieldType::Attachment 的字段名 → 列名 → 值引用
        // 这里不查 schema，直接对所有以 "files/" 开头的 string 字段兜底：
        // schema 驱动的 attachment 字段值就是这种形式，跨字段多走几次不影响。
        obj.values()
            .filter_map(|v| v.as_str())
            .filter(|s| s.starts_with("files/") && !s.contains(".."))
            .map(|s| s.to_string())
            .filter(|s| {
                // 仅保留纯文件名（防止攻击者注入路径）
                let fname = s.trim_start_matches("files/");
                !fname.is_empty()
                    && !fname.contains('/')
                    && !fname.contains('\\')
            })
            .collect()
    }

    /// 工作台根目录（`<hermix_home>/workspaces/<ws_id>/`）。db_path 是
    /// `<ws_root>/data.sqlite`，parent() 一次就是 ws_root。GC 附件文件用。
    fn workspace_root(&self) -> std::path::PathBuf {
        self.db_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.db_path.clone())
    }

    /// 删一个附件文件（`files/<name>` → `<ws_root>/files/<name>`）。best-effort：
    /// 失败仅 warn，不抛错（磁盘 IO 失败不该影响主流程）。
    fn remove_attachment_file(ws_root: &std::path::Path, path: &str) {
        let fname = path.trim_start_matches("files/");
        let full = ws_root.join("files").join(fname);
        match std::fs::remove_file(&full) {
            Ok(()) => tracing::info!(path = %path, "已清理孤儿附件"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => { /* 已经不在；不警告 */ }
            Err(e) => tracing::warn!(path = %path, error = %e, "附件清理失败"),
        }
    }

    /// 更新前后 attachment 字段差集清理（H3）：删 before 引用但 after 不再
    /// 引用的文件；after 新增的文件属于"用户上传"，服务端应在 upload 时已落盘，
    /// 落在清理范围之外。
    fn gc_attachment_diff(&self, table: &str, before: &Value, after: &Value) {
        let before_set: std::collections::HashSet<String> =
            Self::attachment_paths_in_row(table, before).into_iter().collect();
        let after_set: std::collections::HashSet<String> =
            Self::attachment_paths_in_row(table, after).into_iter().collect();
        for removed in before_set.difference(&after_set) {
            Self::remove_attachment_file(&self.workspace_root(), removed);
        }
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
    // null 视为"未设置"：非必填字段返回 NULL（DDL DEFAULT 兜底），
    // 必填字段报错（保证业务约束不被静默突破）。
    if matches!(v, Value::Null) {
        return if f.required {
            Err(format!("字段[{}]必填但为 null", f.id))
        } else {
            Ok(rusqlite::types::Value::Null)
        };
    }
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
        let after = self.query_row_by_id(table, id)?;
        if let Some(ref after_val) = after {
            let _ = self.log_change(
                &conn,
                table,
                id,
                "update",
                before.as_ref(),
                Some(after_val),
                updated_by,
            );
        }
        // H3：更新后做"被替换的附件"差集清理——before 引用但 after 不再引用的
        // 文件就是孤儿，立即删除；after 新引用而 before 没有的属于用户上传
        // （理论上服务端已落盘），不在清理范围。
        // 失败仅 warn——磁盘小问题不该回滚已成功的 UPDATE。
        if let (Some(ref b), Some(ref a)) = (before.as_ref(), after.as_ref()) {
            self.gc_attachment_diff(table, b, a);
        }
        Ok(())
    }

    fn delete(&self, table: &str, id: &str, updated_by: &str) -> Result<(), StoreError> {
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
        // H3：删除行后清理该行引用的所有附件文件——这些文件再无主。
        // 必须在 log_change 之后做，万一 GC 失败审计已经记录了原始 before 引用；
        // 但顺序其实无所谓（GC 是 best-effort）。
        if let Some(ref b) = before {
            for path in Self::attachment_paths_in_row(table, b) {
                Self::remove_attachment_file(&self.workspace_root(), &path);
            }
        }
        // P6 audit：记录（before 为删前完整行，after 无）
        if let Some(b) = before {
            let _ = self.log_change(
                &conn,
                table,
                id,
                "delete",
                Some(&b),
                None,
                updated_by,
            );
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
        store.delete("vehicles", &ids[1], "user").expect("delete");
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
        // FTS5 按 token 匹配，与 LIKE 子串语义不同——
        // 搜词必须与某 token 的前缀一致才命中。"京B" 是 "京B67890" 的前缀，
        // 用 "京B" 搜 1 行；用 "B" 不会命中 "京B67890"（前缀不是 B）。
        let rows = store.search_text("vehicles", "京B", 10, 0).expect("search3");
        assert!(rows.len() >= 1, "含「京B」前缀的 plate 应被命中: {}", rows.len());
    }

    #[test]
    fn search_text_ignores_non_text_fields_and_empty_keyword() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = open_temp(&dir);
        seed_vehicles(&store);

        // number 字段不参与搜索（12000 是 mileage 值，不命中）
        let rows = store.search_text("vehicles", "12000", 10, 0).expect("search");
        assert!(rows.is_empty(), "number 字段不应被搜索");

        // 空 keyword → 全表（M3 修复后一致行为：search_text 与 count_search_text 对称）
        let rows = store.search_text("vehicles", "", 10, 0).expect("empty");
        assert_eq!(rows.len(), 3, "空 keyword 应返回全表");
        let rows = store.search_text("vehicles", "   ", 10, 0).expect("blank");
        assert_eq!(rows.len(), 3, "纯空白 keyword 视为空，应返回全表");
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

        // L3 补充（H4 转义）：keyword 含 % / _ / \ 不会被当成通配符
        let n = store.count_search_text("vehicles", "10%off").expect("escape-pct");
        assert_eq!(n, 0, "% 不应作通配匹配（无任何行含字面「10%off」）");
        let n = store.count_search_text("vehicles", "%").expect("only-pct");
        assert_eq!(n, 0, "单独 % 不匹配任何行（H4 escape）");
        // 真实子串仍然命中（前缀匹配 FTS5 unicode61；"京A" 是 "京A12345" 的前缀）
        let n = store.count_search_text("vehicles", "京A").expect("prefix");
        assert_eq!(n, 1, "前缀「京A」应命中 plate=京A12345");
    }

    #[test]
    fn change_log_undo_preserves_timestamps_through_chain() {
        // L3 补充（H1 + H5）：insert → update → undo chain 必须逐步恢复
        // 且撤销后原 created_at/updated_at 保留（H1 修复），
        // change_log 行不删（H5 修复，第二次 undo 仍能命中前一次 insert）。
        let dir = tempfile::tempdir().expect("tmp");
        let store = open_temp(&dir);
        store
            .insert("vehicles", &[serde_json::json!({"plate": "京A1", "status": "在用"})], "user")
            .expect("insert");
        // 拿原 created_at
        let rows = store.query("vehicles", &[], None, 1, 0).expect("q");
        let original_created = rows[0]["created_at"].as_str().unwrap().to_string();
        // 标记后 update
        std::thread::sleep(std::time::Duration::from_millis(1100));
        store
            .update("vehicles", &rows[0]["id"].as_str().unwrap(), &serde_json::json!({"status": "维修"}), "user")
            .expect("update");
        // 撤销 update → status 回 "在用"，原 created_at 保留
        store.undo_last("vehicles").expect("undo-1");
        let rows = store.query("vehicles", &[], None, 1, 0).expect("q");
        assert_eq!(rows[0]["status"], "在用");
        assert_eq!(
            rows[0]["created_at"].as_str().unwrap(),
            original_created,
            "H1 修复：撤销 update 后 created_at 必须保留为初值"
        );
        // 撤销 insert → 行应被删
        let undone = store.undo_last("vehicles").expect("undo-2");
        assert!(undone.is_some(), "H5 修复：第二次撤销仍能命中（H1 undo 留下的 change_log 行）");
        let n = store.count("vehicles", &[]).expect("count");
        assert_eq!(n, 0, "撤销 insert 后表应为空");
    }

    #[test]
    fn delete_cleans_orphan_attachment_files() {
        // L3 补充（H3）：delete 行后该行引用的 `files/<uuid>.<ext>` 应被清理。
        // 在临时 DB 目录预创建 `files/<uuid>.png`，写入 attachment 字段值，
        // delete 后断言文件不在了。
        // 用带 attachment 字段的 schema（默认 sample_schema 没附件字段）
        let dir = tempfile::tempdir().expect("tmp");
        let schema: crate::WorkspaceSchema = serde_json::from_value(serde_json::json!({
            "name": "attach-ws",
            "modules": [{
                "type":"table","id":"media","name":"媒体",
                "fields":[
                    {"id":"title","name":"标题","type":"text","required":true},
                    {"id":"photo","name":"照片","type":"attachment"}
                ]
            }]
        })).expect("schema");
        let store = LocalSqliteStore::open(
            dir.path().join("data.sqlite"),
            schema,
        )
        .expect("open");
        // 准备 attachments 目录与文件
        let files_dir = dir.path().join("files");
        std::fs::create_dir_all(&files_dir).expect("mkdir");
        let orphan = "files/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee.png";
        let orphan_full = files_dir.join("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee.png");
        std::fs::write(&orphan_full, b"PNGDATA").expect("write");
        assert!(orphan_full.exists(), "前置：附件存在");
        // 无关文件保留
        let _other = "files/11111111-2222-3333-4444-555555555555.png";
        std::fs::write(files_dir.join("11111111-2222-3333-4444-555555555555.png"), b"X")
            .expect("write other");
        let ids = store
            .insert("media", &[serde_json::json!({"title": "A", "photo": orphan})], "user")
            .expect("insert");
        // 删行
        store.delete("media", &ids[0], "user").expect("delete");
        assert!(!orphan_full.exists(), "H3 修复：被删行引用的附件文件应被清理");
        // 无关文件保留
        let other_full = files_dir.join("11111111-2222-3333-4444-555555555555.png");
        assert!(other_full.exists(), "未被引用的文件应保留");
    }

    #[test]
    fn undo_rejected_on_readonly_workspace() {
        // L3 补充（H2）：撤销也是写——只读工作台必须拒绝。
        // 通过 WorkspaceService::open_writable 把守，只读权限下根本没机会拿到写句柄。
        let dir = tempfile::tempdir().expect("tmp");
        let mut schema = sample_schema();
        schema.name = "readonly-ws".to_string();
        schema.permissions = serde_json::json!({"mode": "readonly"});
        let store = LocalSqliteStore::open(
            dir.path().join("readonly-ws").join("data.sqlite"),
            schema.clone(),
        )
        .expect("open");
        store
            .insert("vehicles", &[serde_json::json!({"plate": "A", "status": "在用"})], "user")
            .expect("insert");
        // 通过 WorkspaceService 验证：open_writable 直接拒
        let svc = crate::WorkspaceService::new(dir.path().to_path_buf());
        let res = svc.open_writable(&schema.name);
        assert!(res.is_err(), "H2 修复：只读工作台 open_writable 必拒");
        // 直接调 store.undo_last 仍能跑（H2 在 REST 闸门而非 store 层），
        // 这与设计一致：撤销是 WorkspaceService 把守的写权限检查。
        // 用 store 内部测试可单独验证 change_log 撤销逻辑。
        let undone = store.undo_last("vehicles").expect("undo-direct");
        assert!(undone.is_some(), "store 层 undo 不受 service 权限门控制");
    }

    #[test]
    fn upsert_by_unique_inserts_then_updates() {
        // L5 测试：unique 字段存在时 upsert 行为正确
        let dir = tempfile::tempdir().expect("tmp");
        let store = open_temp(&dir);
        store
            .insert(
                "vehicles",
                &[
                    serde_json::json!({"plate": "A", "status": "在用"}),
                    serde_json::json!({"plate": "B", "status": "维修"}),
                ],
                "user",
            )
            .expect("seed");
        let (ins, upd) = store
            .upsert_by_unique(
                "vehicles",
                &[
                    serde_json::json!({"plate": "A", "status": "停用"}), // 更新
                    serde_json::json!({"plate": "C", "status": "在用"}), // 新增
                ],
                "import",
            )
            .expect("upsert");
        assert_eq!((ins, upd), (1, 1), "1 行更新 + 1 行新增");
        // 验证 A 的 status 被更新（按 plate=唯一键定位）
        let row_a = store
            .query(
                "vehicles",
                &[crate::Filter {
                    field: "plate".into(),
                    op: crate::FilterOp::Eq,
                    value: serde_json::json!("A"),
                }],
                None,
                1,
                0,
            )
            .expect("q");
        assert_eq!(row_a[0]["status"], "停用");
        // 验证总行数 = 3（A 已更新 + B 保留 + C 新增）
        let n = store.count("vehicles", &[]).expect("count");
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
        store.delete("vehicles", &rid, "user").expect("delete");
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
        store.delete("vehicles", &rid, "user").expect("delete");

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
