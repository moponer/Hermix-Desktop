//! Workspace manager tool — LLM 对话建设工作台（M2.3）。
//!
//! `workspace_build`：LLM 传完整 `WorkspaceSchema` + 可选种子数据 →
//! 后端建设（建目录/定义/DB/建表/导入/自测）→ 返回工作台 id + 摘要。
//!
//! 流程契约（写入工具描述，LLM 必须遵守）：
//! 1. 先向用户展示 schema 提案（markdown 预览），用户明确确认后才调用本工具；
//! 2. 建设结果含自测摘要，LLM 收到后向用户叙述"建设完成，请测试"；
//! 3. 若返回错误（校验/导入/自测失败），LLM 修正 schema 后重试。
//!
//! 注意：工具层不直接发 acui 卡（acui 只在 agent 层）；用户确认依赖
//! 对话确认 + RiskLevel 审批卡（Caution 及以上触发）。
//!
//! 后端通过 [`WorkspaceBackend`] trait 解耦——hermix-server 提供实现
//! （内部调用 `hermix_workspace::builder::create_from_schema`），
//! 本模块只做参数解析与转发（与 `delegation.rs` 同一模式）。

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema, traits::RiskLevel};

use crate::ToolRegistry;
use std::collections::HashMap;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Backend trait
// ---------------------------------------------------------------------------

/// 工作台建设后端（由 hermix-server 实现）。
#[async_trait]
pub trait WorkspaceBackend: Send + Sync {
    /// 建设一个工作台。
    /// - `name`：工作台名称（展示用）
    /// - `schema`：完整 `WorkspaceSchema` JSON 对象
    /// - `seed_rows`：表 id → 初始行数组（可选）
    /// 返回 JSON 字符串（含 id / tables / smoke 摘要）。
    async fn build_workspace(
        &self,
        name: &str,
        schema: Value,
        seed_rows: &HashMap<String, Vec<Value>>,
    ) -> Result<String, ToolError>;

    /// 增量更新工作台定义（LLM 改 schema → 迁移）。
    /// - `id`：工作台 id
    /// - `schema`：新的完整 `WorkspaceSchema` JSON 对象
    /// 返回 JSON 字符串（含迁移报告：新增表/列、保留的表/列、枚举存量提示）。
    async fn update_schema(&self, id: &str, schema: Value) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

pub struct WorkspaceManagerHandler {
    backend: Arc<dyn WorkspaceBackend>,
}

impl WorkspaceManagerHandler {
    pub fn new(backend: Arc<dyn WorkspaceBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for WorkspaceManagerHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'name' 参数".into()))?;

        let schema = params
            .get("schema")
            .ok_or_else(|| ToolError::InvalidParams("缺少 'schema' 参数".into()))?
            .clone();
        if !schema.is_object() {
            return Err(ToolError::InvalidParams("'schema' 必须是 JSON 对象".into()));
        }

        let seed_rows = params
            .get("seed_rows")
            .and_then(|v| v.as_object())
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_array().map(|a| (k.clone(), a.clone())))
                    .collect::<HashMap<String, Vec<Value>>>()
            })
            .unwrap_or_default();

        self.backend.build_workspace(name, schema, &seed_rows).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "name".into(),
            json!({
                "type": "string",
                "description": "工作台名称（展示名，可中文，如「车辆管理工作台」）"
            }),
        );
        props.insert(
            "schema".into(),
            json!({
                "type": "object",
                "description": "完整 WorkspaceSchema JSON 对象。含 modules（table/dashboard 模块、字段、视图、组件）与 automations（condition/llm_summary 提醒规则）。字段 id 必须用 ASCII 标识符（[A-Za-z_][A-Za-z0-9_]*），name 可中文。Enum 类型必须提供 options；Relation 类型必须提供 target（指向已存在的表模块 id）。视图引用的字段必须存在于所属表。详见模型注释。"
            }),
        );
        props.insert(
            "seed_rows".into(),
            json!({
                "type": "object",
                "description": "可选。表 id → 初始行数组（来自用户导入/粘贴的数据，抽样即可，完整数据后续可通过对话继续录入）。"
            }),
        );

        tool_schema(
            "workspace_build",
            "创建工作台（schema 驱动，M2 生成管线建设端）。调用前必须已向用户展示 schema 提案并获得明确确认（流程: 展示提案 → 用户确认 → 调用本工具）。成功后返回工作台 id 与自测摘要（表行数/条件可编译），必须向用户叙述「建设完成，请测试」。失败时返回具体错误（校验/导入/自测），修正 schema 后重试，不要静默放弃。\n\n\
             # 数据摄入（用户丢入文件时，零上下文膨胀）\n\
             1. 用 delegate_task 派 sub-agent 隔离解析文件（子 agent 用 read_file / markitdown 读，大文件不进主上下文）。\n\
             2. 要求子 agent 返回结构化字段推断结论：列名 + 推断类型（text/number/date/datetime/boolean/enum/relation/attachment）+ 枚举候选 + 抽样 3-5 行 + 数据质量提示（空值率/重复/异常）。\n\
             3. 据此生成 schema：文本列→text，数字列→number，日期列→date，取值集合小的文本列→enum（提供 options），提到另一张表的列→relation（先建目标表）。\n\
             4. 把抽样数据作为 seed_rows 传入（完整数据后续对话继续录入，不要一次性塞全部行）。",
            JsonSchema::object(props, vec!["name".into(), "schema".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register this tool with the registry.
pub fn register(registry: &ToolRegistry, backend: Arc<dyn WorkspaceBackend>) {
    let handler = Arc::new(WorkspaceManagerHandler::new(backend));
    let schema = handler.schema();
    registry.register_with_risk(
        "workspace_build",
        "workspace",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        true,
        "创建工作台（schema 驱动）：LLM 提供完整 WorkspaceSchema + 可选种子数据，后端自动建设（目录/定义/SQLite 建表/导入/自测）。流程契约：先向用户展示 schema 提案并获明确确认 → 调用 → 完成后叙述「建设完成，请测试」。",
        "🗂️",
        None,
        RiskLevel::Caution,
        false,
    );
}

/// Register the `workspace_update_schema` tool（M2.5 增量迭代）。
pub fn register_update(registry: &ToolRegistry, backend: Arc<dyn WorkspaceBackend>) {
    let handler = Arc::new(WorkspaceUpdateSchemaHandler::new(backend));
    let schema = handler.schema();
    registry.register_with_risk(
        "workspace_update_schema",
        "workspace",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        true,
        "增量更新工作台定义（M2.5）：LLM 提供新 WorkspaceSchema（相对当前定义的完整新版本），后端迁移（新增列/表自动建、删除的表/列数据保留、枚举存量校验提示）。流程契约：先向用户展示改动点（新增/删除/修改）并获确认 → 调用 → 完成后叙述迁移结果。",
        "🔄",
        None,
        RiskLevel::Caution,
        false,
    );
}

// ---------------------------------------------------------------------------
// Update handler（workspace_update_schema）
// ---------------------------------------------------------------------------

pub struct WorkspaceUpdateSchemaHandler {
    backend: Arc<dyn WorkspaceBackend>,
}

impl WorkspaceUpdateSchemaHandler {
    pub fn new(backend: Arc<dyn WorkspaceBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for WorkspaceUpdateSchemaHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'id' 参数".into()))?;

        let schema = params
            .get("schema")
            .ok_or_else(|| ToolError::InvalidParams("缺少 'schema' 参数".into()))?
            .clone();
        if !schema.is_object() {
            return Err(ToolError::InvalidParams("'schema' 必须是 JSON 对象".into()));
        }

        self.backend.update_schema(id, schema).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "id".into(),
            json!({
                "type": "string",
                "description": "工作台 id（workspace_build 返回的 id）"
            }),
        );
        props.insert(
            "schema".into(),
            json!({
                "type": "object",
                "description": "新的完整 WorkspaceSchema JSON 对象（相对当前定义的完整新版本，不是 diff）。字段 id 必须用 ASCII 标识符；Enum 类型必须提供 options；Relation 类型必须提供 target。"
            }),
        );

        tool_schema(
            "workspace_update_schema",
            "增量更新工作台定义。调用前必须已向用户展示改动点并获得明确确认。删除的表/列不会物理删除数据（数据保留，仅从定义移除）；枚举选项收窄会报告存量非法行数。",
            JsonSchema::object(props, vec!["id".into(), "schema".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    struct MockBackend;

    #[async_trait]
    impl WorkspaceBackend for MockBackend {
        async fn build_workspace(
            &self,
            name: &str,
            _schema: Value,
            seed_rows: &HashMap<String, Vec<Value>>,
        ) -> Result<String, ToolError> {
            Ok(format!(
                "{{ \"id\": \"ws-1\", \"name\": \"{name}\", \"seeded_tables\": {} }}",
                seed_rows.len()
            ))
        }

        async fn update_schema(&self, id: &str, _schema: Value) -> Result<String, ToolError> {
            Ok(format!("{{ \"ok\": true, \"id\": \"{id}\" }}"))
        }
    }

    #[tokio::test]
    async fn test_schema_shape() {
        let handler = WorkspaceManagerHandler::new(Arc::new(MockBackend));
        let schema = handler.schema();
        assert_eq!(schema.name, "workspace_build");
        assert_eq!(
            schema.parameters.required,
            Some(vec!["name".to_string(), "schema".to_string()])
        );
    }

    #[tokio::test]
    async fn test_execute_forwards_params() {
        let handler = WorkspaceManagerHandler::new(Arc::new(MockBackend));
        let result = handler
            .execute(json!({
                "name": "车辆管理",
                "schema": { "name": "车辆管理", "modules": [] },
                "seed_rows": { "vehicles": [{"plate": "京A1"}] }
            }))
            .await
            .unwrap();
        assert!(result.contains("\"name\": \"车辆管理\""));
        assert!(result.contains("\"seeded_tables\": 1"));
    }

    #[tokio::test]
    async fn test_missing_params_rejected() {
        let handler = WorkspaceManagerHandler::new(Arc::new(MockBackend));
        let err = handler.execute(json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn test_non_object_schema_rejected() {
        let handler = WorkspaceManagerHandler::new(Arc::new(MockBackend));
        let err = handler
            .execute(json!({ "name": "x", "schema": "not-an-object" }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParams(_)));
    }
}
