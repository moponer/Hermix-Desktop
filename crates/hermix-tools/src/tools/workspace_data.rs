//! Workspace data tools — LLM 读写工作台数据（M3.3/M3.4）。
//!
//! 读侧 3 工具：
//! - `workspace_table_schema`：查表字段白名单（类型/枚举/必填/唯一）——LLM
//!   操作任何表前必须先查结构，保证参数精确（schema 动态注入的查询端）
//! - `workspace_query`：带过滤/排序/分页查询
//! - `workspace_count`：条件计数
//!
//! 写侧 3 工具（M3.4）：
//! - `workspace_insert` / `workspace_update` / `workspace_delete`
//!
//! 后端通过 [`WorkspaceDataBackend`] trait 解耦——hermix-server 提供实现
//! （内部用 `hermix_workspace::service::WorkspaceService` 解析工作台目录 +
//! `LocalSqliteStore` 参数化 CRUD），本模块只做参数解析与转发。
//!
//! 安全模型：表名/字段名过 schema 白名单（`table_fields`），值经
//! `convert_value` 类型转换 + 枚举校验，SQL 全程参数化——LLM 不直接写 SQL。

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema, traits::RiskLevel};

use crate::ToolRegistry;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Backend trait
// ---------------------------------------------------------------------------

/// 工作台数据后端（由 hermix-server 实现）。
#[async_trait]
pub trait WorkspaceDataBackend: Send + Sync {
    /// 表字段白名单（含类型/枚举/必填/唯一）。表或工作台不存在 → 错误。
    async fn table_schema(&self, id: &str, table: &str) -> Result<String, ToolError>;

    /// 查询行。`filters` 为 `[{field, op, value}]`，`sort` 为 `"field"` 或
    /// `"-field"`（升/降序），`limit` 默认 100（上限 500 防大表拖垮上下文）。
    async fn query(
        &self,
        id: &str,
        table: &str,
        filters: Value,
        sort: Option<String>,
        limit: usize,
        offset: usize,
    ) -> Result<String, ToolError>;

    /// 条件计数。
    async fn count(&self, id: &str, table: &str, filters: Value) -> Result<String, ToolError>;

    /// 插入行（返回新行 id 列表）。
    async fn insert(&self, id: &str, table: &str, rows: Value) -> Result<String, ToolError>;

    /// 更新一行（patch 只含要改的字段）。
    async fn update(&self, id: &str, table: &str, row_id: &str, patch: Value) -> Result<String, ToolError>;

    /// 删除一行（危险操作——工具描述要求 LLM 先征得用户确认）。
    async fn delete(&self, id: &str, table: &str, row_id: &str) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// 公共参数解析
// ---------------------------------------------------------------------------

fn parse_id(params: &Value) -> Result<String, ToolError> {
    params
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| ToolError::InvalidParams("缺少 'id' 参数（工作台 id）".into()))
}

fn parse_table(params: &Value) -> Result<String, ToolError> {
    params
        .get("table")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| ToolError::InvalidParams("缺少 'table' 参数（表模块 id，用 workspace_table_schema 查）".into()))
}

fn parse_filters(params: &Value) -> Value {
    params
        .get("filters")
        .cloned()
        .unwrap_or_else(|| json!([]))
}

fn parse_limit(params: &Value, default: usize, max: usize) -> Result<usize, ToolError> {
    let n = params
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(default as u64) as usize;
    Ok(n.min(max))
}

// ---------------------------------------------------------------------------
// 读侧 handlers
// ---------------------------------------------------------------------------

pub struct TableSchemaHandler {
    backend: Arc<dyn WorkspaceDataBackend>,
}

impl TableSchemaHandler {
    pub fn new(backend: Arc<dyn WorkspaceDataBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for TableSchemaHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let id = parse_id(&params)?;
        let table = parse_table(&params)?;
        self.backend.table_schema(&id, &table).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("id".into(), json!({"type": "string", "description": "工作台 id"}));
        props.insert("table".into(), json!({"type": "string", "description": "表模块 id（不记得时用 workspace_list_tables 查）"}));
        tool_schema(
            "workspace_table_schema",
            "查询工作台某张表的字段结构（白名单：字段 id/name/type/options/required/unique）。操作任何表的数据前必须先调用本工具拿到字段白名单，再构造参数——字段名/枚举值必须与白名单一致，否则会被校验拒绝。",
            JsonSchema::object(props, vec!["id".into(), "table".into()]),
        )
    }
}

pub struct QueryHandler {
    backend: Arc<dyn WorkspaceDataBackend>,
}

impl QueryHandler {
    pub fn new(backend: Arc<dyn WorkspaceDataBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for QueryHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let id = parse_id(&params)?;
        let table = parse_table(&params)?;
        let filters = parse_filters(&params);
        let sort = params.get("sort").and_then(|v| v.as_str()).map(|s| s.to_string());
        let limit = parse_limit(&params, 100, 500)?;
        let offset = params.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        self.backend.query(&id, &table, filters, sort, limit, offset).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("id".into(), json!({"type": "string", "description": "工作台 id"}));
        props.insert("table".into(), json!({"type": "string", "description": "表模块 id"}));
        props.insert("filters".into(), json!({
            "type": "array",
            "description": "可选。过滤条件数组，每项 {field: 字段id, op: eq|ne|gt|gte|lt|lte|contains, value: 值}。字段名必须来自 workspace_table_schema 白名单。",
            "items": { "type": "object" }
        }));
        props.insert("sort".into(), json!({"type": "string", "description": "可选。排序字段，\"field\" 升序 / \"-field\" 降序"}));
        props.insert("limit".into(), json!({"type": "integer", "description": "可选。最大行数（默认 100，上限 500）"}));
        props.insert("offset".into(), json!({"type": "integer", "description": "可选。分页偏移"}));
        tool_schema(
            "workspace_query",
            "查询工作台表中的数据行。先调 workspace_table_schema 确认字段名，再构造 filters。返回 JSON 数组（每行含 id/created_at/updated_at）。",
            JsonSchema::object(props, vec!["id".into(), "table".into()]),
        )
    }
}

pub struct CountHandler {
    backend: Arc<dyn WorkspaceDataBackend>,
}

impl CountHandler {
    pub fn new(backend: Arc<dyn WorkspaceDataBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for CountHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let id = parse_id(&params)?;
        let table = parse_table(&params)?;
        let filters = parse_filters(&params);
        self.backend.count(&id, &table, filters).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("id".into(), json!({"type": "string", "description": "工作台 id"}));
        props.insert("table".into(), json!({"type": "string", "description": "表模块 id"}));
        props.insert("filters".into(), json!({
            "type": "array",
            "description": "可选。过滤条件数组（同 workspace_query）",
            "items": { "type": "object" }
        }));
        tool_schema(
            "workspace_count",
            "按条件统计工作台表中的行数（例：还剩多少辆车未年检）。",
            JsonSchema::object(props, vec!["id".into(), "table".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// 写侧 handlers（M3.4）
// ---------------------------------------------------------------------------

pub struct InsertHandler {
    backend: Arc<dyn WorkspaceDataBackend>,
}

impl InsertHandler {
    pub fn new(backend: Arc<dyn WorkspaceDataBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for InsertHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let id = parse_id(&params)?;
        let table = parse_table(&params)?;
        let rows = params
            .get("rows")
            .and_then(|v| v.as_array())
            .cloned()
            .ok_or_else(|| ToolError::InvalidParams("缺少 'rows' 参数（行对象数组）".into()))?;
        if rows.is_empty() {
            return Err(ToolError::InvalidParams("'rows' 不能为空".into()));
        }
        self.backend.insert(&id, &table, Value::Array(rows)).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("id".into(), json!({"type": "string", "description": "工作台 id"}));
        props.insert("table".into(), json!({"type": "string", "description": "表模块 id"}));
        props.insert("rows".into(), json!({
            "type": "array",
            "description": "要插入的行对象数组。键必须是 workspace_table_schema 返回的字段 id；必填字段不能缺；枚举值必须在 options 内；日期用 YYYY-MM-DD 或 YYYY-MM-DD HH:MM:SS。",
            "items": { "type": "object" }
        }));
        tool_schema(
            "workspace_insert",
            "向工作台表插入一行或多行数据（LLM 把对话/文件中提取的数据写入工作台）。返回新行 id 列表。",
            JsonSchema::object(props, vec!["id".into(), "table".into(), "rows".into()]),
        )
    }
}

pub struct UpdateHandler {
    backend: Arc<dyn WorkspaceDataBackend>,
}

impl UpdateHandler {
    pub fn new(backend: Arc<dyn WorkspaceDataBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for UpdateHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let id = parse_id(&params)?;
        let table = parse_table(&params)?;
        let row_id = params
            .get("row_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'row_id' 参数（要更新的行 id）".into()))?;
        let patch = params
            .get("patch")
            .and_then(|v| v.as_object())
            .cloned()
            .map(Value::Object)
            .ok_or_else(|| ToolError::InvalidParams("缺少 'patch' 参数（只含要修改字段的对象）".into()))?;
        self.backend.update(&id, &table, row_id, patch).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("id".into(), json!({"type": "string", "description": "工作台 id"}));
        props.insert("table".into(), json!({"type": "string", "description": "表模块 id"}));
        props.insert("row_id".into(), json!({"type": "string", "description": "目标行 id（从 workspace_query 结果取）"}));
        props.insert("patch".into(), json!({
            "type": "object",
            "description": "只包含要修改字段的对象。键必须来自字段白名单；枚举值必须在 options 内。"
        }));
        tool_schema(
            "workspace_update",
            "更新工作台表中的一行（按 id 定位，patch 只含要改的字段）。",
            JsonSchema::object(props, vec!["id".into(), "table".into(), "row_id".into(), "patch".into()]),
        )
    }
}

pub struct DeleteHandler {
    backend: Arc<dyn WorkspaceDataBackend>,
}

impl DeleteHandler {
    pub fn new(backend: Arc<dyn WorkspaceDataBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for DeleteHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let id = parse_id(&params)?;
        let table = parse_table(&params)?;
        let row_id = params
            .get("row_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'row_id' 参数（要删除的行 id）".into()))?;
        self.backend.delete(&id, &table, row_id).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("id".into(), json!({"type": "string", "description": "工作台 id"}));
        props.insert("table".into(), json!({"type": "string", "description": "表模块 id"}));
        props.insert("row_id".into(), json!({"type": "string", "description": "要删除的行 id"}));
        tool_schema(
            "workspace_delete",
            "删除工作台表中的一行。**危险操作**：删除不可恢复，调用前必须向用户明确展示将删除的行并取得确认，用户确认后才执行。",
            JsonSchema::object(props, vec!["id".into(), "table".into(), "row_id".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register all six workspace data tools.
pub fn register(registry: &ToolRegistry, backend: Arc<dyn WorkspaceDataBackend>) {
    let b1 = backend.clone();
    registry.register_with_risk(
        "workspace_table_schema",
        "workspace",
        TableSchemaHandler::new(b1.clone()).schema(),
        Arc::new(TableSchemaHandler::new(b1)),
        Arc::new(|| true),
        vec![],
        true,
        "查询工作台表结构（字段白名单）",
        "📋",
        None,
        RiskLevel::Safe,
        false,
    );

    let b2 = backend.clone();
    registry.register_with_risk(
        "workspace_query",
        "workspace",
        QueryHandler::new(b2.clone()).schema(),
        Arc::new(QueryHandler::new(b2)),
        Arc::new(|| true),
        vec![],
        true,
        "查询工作台表数据",
        "🔍",
        None,
        RiskLevel::Safe,
        false,
    );

    let b3 = backend.clone();
    registry.register_with_risk(
        "workspace_count",
        "workspace",
        CountHandler::new(b3.clone()).schema(),
        Arc::new(CountHandler::new(b3)),
        Arc::new(|| true),
        vec![],
        true,
        "统计工作台表行数",
        "🔢",
        None,
        RiskLevel::Safe,
        false,
    );

    let b4 = backend.clone();
    registry.register_with_risk(
        "workspace_insert",
        "workspace",
        InsertHandler::new(b4.clone()).schema(),
        Arc::new(InsertHandler::new(b4)),
        Arc::new(|| true),
        vec![],
        true,
        "向工作台表插入数据",
        "➕",
        None,
        RiskLevel::Caution,
        false,
    );

    let b5 = backend.clone();
    registry.register_with_risk(
        "workspace_update",
        "workspace",
        UpdateHandler::new(b5.clone()).schema(),
        Arc::new(UpdateHandler::new(b5)),
        Arc::new(|| true),
        vec![],
        true,
        "更新工作台表行",
        "✏️",
        None,
        RiskLevel::Caution,
        false,
    );

    let b6 = backend.clone();
    registry.register_with_risk(
        "workspace_delete",
        "workspace",
        DeleteHandler::new(b6.clone()).schema(),
        Arc::new(DeleteHandler::new(b6)),
        Arc::new(|| true),
        vec![],
        true,
        "删除工作台表行（危险，需用户确认）",
        "🗑️",
        None,
        RiskLevel::Dangerous,
        false,
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    struct MockBackend;

    #[async_trait]
    impl WorkspaceDataBackend for MockBackend {
        async fn table_schema(&self, id: &str, table: &str) -> Result<String, ToolError> {
            Ok(format!("{{\"id\": \"{id}\", \"table\": \"{table}\"}}"))
        }
        async fn query(
            &self,
            id: &str,
            table: &str,
            _filters: Value,
            _sort: Option<String>,
            limit: usize,
            _offset: usize,
        ) -> Result<String, ToolError> {
            Ok(format!("{{\"id\": \"{id}\", \"table\": \"{table}\", \"limit\": {limit}}}"))
        }
        async fn count(&self, id: &str, table: &str, _filters: Value) -> Result<String, ToolError> {
            Ok(format!("{{\"id\": \"{id}\", \"table\": \"{table}\", \"count\": 3}}"))
        }
        async fn insert(&self, id: &str, table: &str, rows: Value) -> Result<String, ToolError> {
            let n = rows.as_array().map(|a| a.len()).unwrap_or(0);
            Ok(format!("{{\"id\": \"{id}\", \"table\": \"{table}\", \"inserted\": {n}}}"))
        }
        async fn update(&self, id: &str, table: &str, row_id: &str, _patch: Value) -> Result<String, ToolError> {
            Ok(format!("{{\"id\": \"{id}\", \"table\": \"{table}\", \"updated\": \"{row_id}\"}}"))
        }
        async fn delete(&self, id: &str, table: &str, row_id: &str) -> Result<String, ToolError> {
            Ok(format!("{{\"id\": \"{id}\", \"table\": \"{table}\", \"deleted\": \"{row_id}\"}}"))
        }
    }

    fn backend() -> Arc<dyn WorkspaceDataBackend> {
        Arc::new(MockBackend)
    }

    #[tokio::test]
    async fn table_schema_tool_roundtrip() {
        let out = TableSchemaHandler::new(backend())
            .execute(json!({"id": "ws-1", "table": "vehicles"}))
            .await
            .unwrap();
        assert!(out.contains("\"table\": \"vehicles\""));
    }

    #[tokio::test]
    async fn query_defaults_limit_to_100() {
        let out = QueryHandler::new(backend())
            .execute(json!({"id": "ws-1", "table": "vehicles"}))
            .await
            .unwrap();
        assert!(out.contains("\"limit\": 100"));
    }

    #[tokio::test]
    async fn query_caps_limit_at_500() {
        let out = QueryHandler::new(backend())
            .execute(json!({"id": "ws-1", "table": "vehicles", "limit": 9999}))
            .await
            .unwrap();
        assert!(out.contains("\"limit\": 500"));
    }

    #[tokio::test]
    async fn insert_requires_nonempty_rows() {
        let err = InsertHandler::new(backend())
            .execute(json!({"id": "ws-1", "table": "vehicles", "rows": []}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn update_requires_patch_object() {
        let err = UpdateHandler::new(backend())
            .execute(json!({"id": "ws-1", "table": "vehicles", "row_id": "r1", "patch": "nope"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn delete_forwards_row_id() {
        let out = DeleteHandler::new(backend())
            .execute(json!({"id": "ws-1", "table": "vehicles", "row_id": "r42"}))
            .await
            .unwrap();
        assert!(out.contains("\"deleted\": \"r42\""));
    }

    #[tokio::test]
    async fn missing_id_rejected() {
        let err = CountHandler::new(backend())
            .execute(json!({"table": "vehicles"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParams(_)));
    }
}
