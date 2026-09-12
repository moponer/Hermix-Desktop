//! WorkspaceBackend 实现 — 把 `workspace_build` 工具接到 hermix-workspace。
//!
//! hermix-tools 只定义 trait（参数/返回均为 JSON），真正建设逻辑在这里：
//! 解析 schema → `hermix_workspace::builder::create_from_schema`
//! （校验/建目录/定义/DB/建表/导入/自测）→ 注册进 registry.sqlite
//! （让新工作台出现在 Tab 列表）→ 返回结构化摘要。
//!
//! 数据目录：`~/.hermix/workspaces/`（与 dashboard/workspaces.rs 同根，
//! HTTP API 与 LLM 工具共用同一批工作台）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use hermix_core::ToolError;
use hermix_tools::tools::workspace_data::WorkspaceDataBackend;
use hermix_tools::tools::workspace_manager::WorkspaceBackend;
use hermix_workspace::service::WorkspaceService;
use hermix_workspace::{RegistryStore, WorkspaceSchema, WorkspaceStore};

/// 本地工作台建设后端。
pub struct LocalWorkspaceBackend {
    /// 工作台根目录（`~/.hermix/workspaces/`）。
    root: PathBuf,
    /// 自动化调度服务（M4.4：建设/迁移后同步注册自动化）。
    /// 启动期注入：state.rs 在构造完 automation_service 后调用
    /// `inject_automation_service`（工具注册早于 automation_service
    /// 构造，故用 Mutex 事后注入）。
    automation_service: Arc<std::sync::Mutex<Option<Arc<hermix_automation::AutomationService>>>>,
}

impl LocalWorkspaceBackend {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            automation_service: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// 注入自动化调度服务（state.rs 启动期调用一次；幂等）。
    pub fn inject_automation_service(&self, svc: Arc<hermix_automation::AutomationService>) {
        if let Ok(mut guard) = self.automation_service.lock() {
            *guard = Some(svc);
        }
    }

    fn registry_path(&self) -> PathBuf {
        self.root.join("registry.sqlite")
    }

    /// 数据服务层（解析 id → 目录/定义/store）。
    fn service(&self) -> WorkspaceService {
        WorkspaceService::new(self.root.clone())
    }

    fn err(msg: impl Into<String>) -> ToolError {
        ToolError::ExecutionFailed(msg.into())
    }

    /// 建设/迁移成功后同步注册自动化（M4.4）。
    /// - condition 类：注册占位触发点（prompt 带 `[workspace-condition]`
    ///   标记 + workspace/automation id），触发侧每次读最新 schema 执行
    ///   零 LLM 条件引擎——schema 更新后自动化定义自动最新。
    /// - llm_summary 类：prompt 原文注册，走现有 agent.send_message
    ///   （会话隔离 `automation:{id}` 天然具备）。
    fn register_workspace_automations(
        &self,
        workspace_id: &str,
        schema: &WorkspaceSchema,
    ) {
        let Some(svc) = self
            .automation_service
            .lock()
            .ok()
            .and_then(|g| g.clone())
        else {
            tracing::debug!(workspace_id, "无 automation_service，跳过自动化注册");
            return;
        };
        for plan in hermix_workspace::automation_plan::plan_automations(workspace_id, schema) {
            match &plan {
                hermix_workspace::AutomationPlan::Condition { name, frequency, automation_id, .. } => {
                    let marker = serde_json::json!({
                        "workspace_id": workspace_id,
                        "automation_id": automation_id,
                    });
                    let params = hermix_automation::CreateAutomationParams {
                        name: format!("[工作台] {name}"),
                        prompt: format!(
                            "[workspace-condition]\n{}",
                            serde_json::to_string(&marker).unwrap_or_default()
                        ),
                        schedule_type: hermix_automation::ScheduleType::Recurring,
                        rrule: Some(hermix_workspace::automation_plan::frequency_to_rrule(frequency)),
                        ..Default::default()
                    };
                    match svc.create(params) {
                        Ok(_) => tracing::info!(workspace_id, automation_id, "condition 自动化已注册"),
                        Err(e) => tracing::warn!(workspace_id, automation_id, error = %e, "condition 自动化注册失败"),
                    }
                }
                hermix_workspace::AutomationPlan::LlmSummary { name, schedule, prompt, .. } => {
                    let params = hermix_automation::CreateAutomationParams {
                        name: format!("[工作台] {name}"),
                        prompt: prompt.clone(),
                        schedule_type: hermix_automation::ScheduleType::Recurring,
                        rrule: Some(schedule.clone()),
                        ..Default::default()
                    };
                    match svc.create(params) {
                        Ok(_) => tracing::info!(workspace_id, "llm_summary 自动化已注册"),
                        Err(e) => tracing::warn!(workspace_id, error = %e, "llm_summary 自动化注册失败"),
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// WorkspaceDataBackend（M3.3/M3.4 数据工具族）
// ---------------------------------------------------------------------------

#[async_trait]
impl WorkspaceDataBackend for LocalWorkspaceBackend {
    async fn table_schema(&self, id: &str, table: &str) -> Result<String, ToolError> {
        let svc = self.service();
        let schema = svc
            .read_schema(id)
            .map_err(|e| Self::err(format!("工作台[{id}]: {e}")))?;
        let fields = svc
            .table_fields(&schema, table)
            .map_err(|e| Self::err(e.to_string()))?;
        Ok(serde_json::to_string(&json!({
            "id": id,
            "table": table,
            "fields": fields,
        }))
        .unwrap_or_default())
    }

    async fn query(
        &self,
        id: &str,
        table: &str,
        filters: Value,
        sort: Option<String>,
        limit: usize,
        offset: usize,
    ) -> Result<String, ToolError> {
        let store = self
            .service()
            .open_store(id)
            .map_err(|e| Self::err(e.to_string()))?;
        // 非法过滤条件必须显式报错：静默当无过滤会返回错误结果集
        let filters: Vec<hermix_workspace::Filter> = serde_json::from_value(filters)
            .map_err(|e| ToolError::InvalidParams(format!("filters 解析失败: {e}")))?;
        let rows = store
            .query(table, &filters, sort.as_deref(), limit, offset)
            .map_err(|e| Self::err(format!("表[{table}]查询失败: {e}")))?;
        Ok(serde_json::to_string(&rows).unwrap_or_default())
    }

    async fn count(&self, id: &str, table: &str, filters: Value) -> Result<String, ToolError> {
        let store = self
            .service()
            .open_store(id)
            .map_err(|e| Self::err(e.to_string()))?;
        let filters: Vec<hermix_workspace::Filter> = serde_json::from_value(filters)
            .map_err(|e| ToolError::InvalidParams(format!("filters 解析失败: {e}")))?;
        let n = store
            .count(table, &filters)
            .map_err(|e| Self::err(format!("表[{table}]计数失败: {e}")))?;
        Ok(json!({ "count": n }).to_string())
    }

    async fn insert(&self, id: &str, table: &str, rows: Value) -> Result<String, ToolError> {
        // 写能力句柄：只读工作台在此被拒（LLM 工具与 REST 同源校验）
        let writable = self.service().open_writable(id).map_err(|e| Self::err(e.to_string()))?;
        let rows: Vec<Value> = serde_json::from_value(rows)
            .map_err(|e| ToolError::InvalidParams(format!("rows 必须是 JSON 数组: {e}")))?;
        let ids = writable
            .insert(table, &rows, "agent")
            .map_err(|e| Self::err(format!("表[{table}]插入失败: {e}")))?;
        Ok(json!({ "ok": true, "ids": ids }).to_string())
    }

    async fn update(&self, id: &str, table: &str, row_id: &str, patch: Value) -> Result<String, ToolError> {
        let writable = self.service().open_writable(id).map_err(|e| Self::err(e.to_string()))?;
        writable
            .update(table, row_id, &patch, "agent")
            .map_err(|e| Self::err(format!("表[{table}]更新失败: {e}")))?;
        Ok(json!({ "ok": true, "id": row_id }).to_string())
    }

    async fn delete(&self, id: &str, table: &str, row_id: &str) -> Result<String, ToolError> {
        let writable = self.service().open_writable(id).map_err(|e| Self::err(e.to_string()))?;
        // M5：审计来源固定标 "llm"，与 LLM 工具族其它操作一致。
        writable
            .delete(table, row_id, "llm")
            .map_err(|e| Self::err(format!("表[{table}]删除失败: {e}")))?;
        Ok(json!({ "ok": true, "id": row_id }).to_string())
    }
}

#[async_trait]
impl WorkspaceBackend for LocalWorkspaceBackend {
    async fn build_workspace(
        &self,
        name: &str,
        schema: Value,
        seed_rows: &HashMap<String, Vec<Value>>,
    ) -> Result<String, ToolError> {
        // 1. 解析 schema（LLM 输出的 WorkspaceSchema JSON）
        let ws_schema: WorkspaceSchema = serde_json::from_value(schema.clone()).map_err(|e| {
            ToolError::InvalidParams(format!(
                "schema 解析失败: {e} —— 请检查 modules/fields 结构与模型定义一致"
            ))
        })?;

        // 2. 建设（校验 → 建目录/定义/DB/建表/导入/自测）
        let result = hermix_workspace::builder::create_from_schema(&self.root, ws_schema.clone(), seed_rows)
            .map_err(|e| ToolError::ExecutionFailed(format!("建设工作台失败: {e}")))?;

        // 3. 注册进 registry.sqlite（让新工作台出现在 Tab 列表）
        let registry = RegistryStore::open(self.registry_path())
            .map_err(|e| ToolError::ExecutionFailed(format!("打开工作台注册表失败: {e}")))?;
        registry
            .create(&result.id, name, &ws_schema.icon, &result.id)
            .map_err(|e| ToolError::ExecutionFailed(format!("注册工作台失败: {e}")))?;

        // 4. 同步注册自动化到调度器（M4.4；失败不阻断建设，仅告警）
        self.register_workspace_automations(&result.id, &ws_schema);

        // 5. 返回结构化摘要（LLM 叙述"建设完成，请测试"）
        Ok(serde_json::json!({
            "ok": true,
            "id": result.id,
            "name": name,
            "tables": result.tables,
            "smoke": result.smoke,
        })
        .to_string())
    }

    async fn update_schema(&self, id: &str, schema: Value) -> Result<String, ToolError> {
        // 路径解析走 service 层，与 dashboard 端点共享同一份目录约定
        let svc = self.service();
        let schema_path = svc.schema_path(id);
        let data_path = svc.data_path(id);

        // 1. 读当前定义（不存在 → NotFound）
        let old_raw = std::fs::read(&schema_path).map_err(|e| {
            ToolError::ExecutionFailed(format!("工作台[{id}]定义不存在或不可读: {e}"))
        })?;
        let old_schema: WorkspaceSchema = serde_json::from_slice(&old_raw).map_err(|e| {
            ToolError::ExecutionFailed(format!("工作台[{id}]当前定义解析失败: {e}"))
        })?;

        // 2. 解析新定义
        let new_schema: WorkspaceSchema = serde_json::from_value(schema.clone()).map_err(|e| {
            ToolError::InvalidParams(format!(
                "schema 解析失败: {e} —— 请检查 modules/fields 结构与模型定义一致"
            ))
        })?;

        // 3. 迁移（事务内 DDL，失败整体回滚；幂等返回 unchanged）
        let report = hermix_workspace::migrate::migrate_schema(&data_path, &old_schema, &new_schema)
            .map_err(|e| ToolError::ExecutionFailed(format!("迁移失败: {e}")))?;

        // 4. 迁移成功 → 落盘新定义（仅 DB 已一致才更新定义文件）
        let raw = serde_json::to_vec_pretty(&new_schema)
            .map_err(|e| ToolError::ExecutionFailed(format!("序列化新定义失败: {e}")))?;
        std::fs::write(&schema_path, raw)
            .map_err(|e| ToolError::ExecutionFailed(format!("写新定义失败: {e}")))?;

        // 5. 同步注册/更新自动化到调度器（M4.4；失败不阻断迁移，仅告警）
        self.register_workspace_automations(id, &new_schema);

        Ok(serde_json::json!({
            "ok": true,
            "id": id,
            "report": report,
        })
        .to_string())
    }
}

/// 便捷：构造已接线的 backend（供 state.rs 注册用）。
/// 返回具体类型以便同一实例同时注册 `WorkspaceBackend`（建设/迁移）
/// 与 `WorkspaceDataBackend`（数据读写）两组工具。
pub fn local_backend() -> Arc<LocalWorkspaceBackend> {
    Arc::new(LocalWorkspaceBackend::new(
        hermix_config::paths::hermix_home().join("workspaces"),
    ))
}

// ---------------------------------------------------------------------------
// Tests（用 tempdir 全链路验证：工具参数 → 建设 → 注册表可见）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_schema_value() -> Value {
        serde_json::json!({
            "schema_version": 1,
            "name": "车辆管理",
            "icon": "car",
            "modules": [
                { "type": "table", "id": "vehicles", "name": "车辆",
                  "fields": [
                    { "id": "plate", "name": "车牌", "type": "text", "required": true, "unique": true },
                    { "id": "status", "name": "状态", "type": "enum", "options": ["在用", "维修"] },
                    { "id": "inspection_date", "name": "年检", "type": "date" }
                  ] }
            ],
            "automations": [
                { "kind": "condition", "id": "a1", "name": "临期", "table": "vehicles",
                  "condition": "inspection_date <= date('now','+7 days')", "message": "{plate}",
                  "frequency": "daily" }
            ]
        })
    }

    #[tokio::test]
    async fn build_workspace_full_chain() {
        let dir = tempfile::tempdir().expect("tmp");
        let backend = LocalWorkspaceBackend::new(dir.path().to_path_buf());

        let mut seed = HashMap::new();
        seed.insert(
            "vehicles".to_string(),
            vec![serde_json::json!({ "plate": "京A12345", "status": "在用", "inspection_date": "2027-03-15" })],
        );

        let out = backend
            .build_workspace("车辆管理", sample_schema_value(), &seed)
            .await
            .expect("build");

        let parsed: Value = serde_json::from_str(&out).expect("json");
        assert_eq!(parsed["ok"], true);
        let id = parsed["id"].as_str().expect("id");

        // 目录 + 定义 + DB 都建好
        assert!(dir.path().join(id).join("workspace.json").exists());
        assert!(dir.path().join(id).join("data.sqlite").exists());

        // 自测摘要包含表可查 + 条件可编译
        let smoke = parsed["smoke"].as_array().expect("smoke");
        assert!(smoke.iter().any(|s| s.as_str().unwrap().contains("表[车辆]可查")));
        assert!(smoke.iter().any(|s| s.as_str().unwrap().contains("自动化[a1]条件可编译")));

        // 注册表可见（Tab 列表能查到）
        let registry = RegistryStore::open(dir.path().join("registry.sqlite")).expect("registry");
        let entries = registry.list().expect("list");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "车辆管理");
        assert_eq!(entries[0].id, id);
    }

    #[tokio::test]
    async fn invalid_schema_returns_invalid_params() {
        let dir = tempfile::tempdir().expect("tmp");
        let backend = LocalWorkspaceBackend::new(dir.path().to_path_buf());
        // 非法 schema（Enum 无 options）→ InvalidParams
        let bad = serde_json::json!({
            "name": "bad",
            "modules": [{ "type": "table", "id": "a", "name": "A",
                          "fields": [{ "id": "st", "name": "状态", "type": "enum" }] }]
        });
        let err = backend
            .build_workspace("bad", bad, &HashMap::new())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed(_)));
    }

    #[tokio::test]
    async fn non_schema_json_rejected() {
        let dir = tempfile::tempdir().expect("tmp");
        let backend = LocalWorkspaceBackend::new(dir.path().to_path_buf());
        let err = backend
            .build_workspace("x", serde_json::json!("just-a-string"), &HashMap::new())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParams(_)));
    }
}
