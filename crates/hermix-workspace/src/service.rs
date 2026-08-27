//! # WorkspaceService — 数据服务层（M3.2）
//!
//! LLM 数据工具族（M3.3/M3.4）的统一入口：把「工作台 id → 可操作的数据
//! 存储」解析成一段路径，供工具复用，避免每个工具各自复制目录拼接、
//! 定义读取、白名单提取逻辑。
//!
//! ```text
//! id → <root>/<id>/workspace.json（定义） + data.sqlite（数据）
//!     → LocalSqliteStore::open（校验 schema + 幂等建表 + 白名单查询）
//! ```
//!
//! 真实逻辑（字段校验/类型转换/参数化 SQL）全部在 `LocalSqliteStore`
//! （`convert_value` / `row_to_params` / `patch_to_params`），本层只做
//! 解析与白名单暴露——REST 端点（dashboard/workspaces.rs）与 LLM 工具
//! 共用同一套 store 语义，天然一致。

use std::path::{Path, PathBuf};

use crate::model::{Field, Module, WorkspaceSchema};
use crate::store::{LocalSqliteStore, WorkspaceStore};

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("工作台[{0}]不存在")]
    NotFound(String),
    #[error("IO 错误: {0}")]
    Io(String),
    #[error("store 错误: {0}")]
    Store(String),
    #[error("表[{0}]不存在")]
    TableNotFound(String),
    #[error("权限拒绝: {0}")]
    PermissionDenied(String),
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// 工作台数据服务：`<root>/<id>/` 目录下定义 + 数据的统一访问入口。
pub struct WorkspaceService {
    root: PathBuf,
}

impl WorkspaceService {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 工作台目录（不检查存在性）。
    pub fn dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    pub fn schema_path(&self, id: &str) -> PathBuf {
        self.dir(id).join("workspace.json")
    }

    pub fn data_path(&self, id: &str) -> PathBuf {
        self.dir(id).join("data.sqlite")
    }

    /// 读工作台定义（不存在 → `NotFound`）。
    pub fn read_schema(&self, id: &str) -> Result<WorkspaceSchema, ServiceError> {
        let raw = std::fs::read(self.schema_path(id))
            .map_err(|_| ServiceError::NotFound(id.to_string()))?;
        serde_json::from_slice(&raw)
            .map_err(|e| ServiceError::Io(format!("工作台[{id}]定义解析失败: {e}")))
    }

    /// 解析工作台并打开数据存储（定义缺失 → `NotFound`）。
    pub fn open_store(&self, id: &str) -> Result<LocalSqliteStore, ServiceError> {
        let schema = self.read_schema(id)?;
        LocalSqliteStore::open(self.data_path(id), schema)
            .map_err(|e| ServiceError::Store(e.to_string()))
    }

    /// 打开工作台数据存储并确认写权限（M7.3）。
    ///
    /// 只读工作台 → `PermissionDenied`；返回的 [`Writable`] 是写能力的
    /// 类型化载体——REST 数据端点、LLM 工具后端与 import 合并都必须
    /// 经由它写数据。单次读盘同时完成解析与权限判定（不二次读 schema）。
    pub fn open_writable(&self, id: &str) -> Result<Writable, ServiceError> {
        let schema = self.read_schema(id)?;
        let perms = schema.permissions_parsed();
        if !perms.can_write() {
            return Err(ServiceError::PermissionDenied(format!(
                "工作台[{id}]为只读（permissions.mode=readonly），拒绝写操作"
            )));
        }
        let store = LocalSqliteStore::open(self.data_path(id), schema)
            .map_err(|e| ServiceError::Store(e.to_string()))?;
        Ok(Writable {
            store,
            allow_delete: perms.allow_delete,
        })
    }

    /// 表字段白名单（供工具 schema 注入 / 校验用）。
    /// 表不存在 → `TableNotFound`。
    pub fn table_fields(&self, schema: &WorkspaceSchema, table: &str) -> Result<Vec<Field>, ServiceError> {
        for m in &schema.modules {
            if let Module::Table { id, fields, .. } = m {
                if id == table {
                    return Ok(fields.clone());
                }
            }
        }
        Err(ServiceError::TableNotFound(table.to_string()))
    }

    /// 全部表（id, name）清单（供 LLM 发现可操作的表）。
    pub fn list_tables(&self, schema: &WorkspaceSchema) -> Vec<(String, String)> {
        schema
            .modules
            .iter()
            .filter_map(|m| match m {
                Module::Table { id, name, .. } => Some((id.clone(), name.clone())),
                Module::Dashboard { .. } => None,
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Writable — 已通过写权限门的存储句柄（M7.3 能力模式）
// ---------------------------------------------------------------------------

/// 写能力的类型化载体：写操作只存在于 `Writable` 上，漏做权限检查在
/// 结构上不可能；只读工作台永远拿不到本类型（`open_writable` 拒绝）。
#[derive(Debug)]
pub struct Writable {
    store: LocalSqliteStore,
    allow_delete: bool,
}

impl Writable {
    pub fn insert(
        &self,
        table: &str,
        rows: &[serde_json::Value],
        updated_by: &str,
    ) -> Result<Vec<String>, ServiceError> {
        self.store
            .insert(table, rows, updated_by)
            .map_err(store_err)
    }

    pub fn update(
        &self,
        table: &str,
        id: &str,
        patch: &serde_json::Value,
        updated_by: &str,
    ) -> Result<(), ServiceError> {
        self.store
            .update(table, id, patch, updated_by)
            .map_err(store_err)
    }

    /// 删行。`allow_delete=false` 的工作台在此拒绝。
    pub fn delete(&self, table: &str, id: &str) -> Result<(), ServiceError> {
        if !self.allow_delete {
            return Err(ServiceError::PermissionDenied(
                "该工作台禁止删除行（permissions.allow_delete=false）".to_string(),
            ));
        }
        self.store.delete(table, id).map_err(store_err)
    }

    /// 整表替换（.hwx LWW 合并专用）：见 [`LocalSqliteStore::replace_table_rows`]。
    /// 受写权限门保护、不受 allow_delete 约束——合并语义是"更新整表"。
    pub fn replace_rows(&self, table: &str, rows: &[serde_json::Value]) -> Result<usize, ServiceError> {
        self.store
            .replace_table_rows(table, rows)
            .map_err(store_err)
    }
}

fn store_err(e: crate::store::StoreError) -> ServiceError {
    ServiceError::Store(e.to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::create_from_schema;
    use crate::store::{Filter, FilterOp, WorkspaceStore};
    use std::collections::HashMap;

    fn build_sample(root: &Path) -> String {
        let schema: WorkspaceSchema = serde_json::from_value(serde_json::json!({
            "name": "车辆管理",
            "modules": [
                { "type": "table", "id": "vehicles", "name": "车辆",
                  "fields": [
                    { "id": "plate", "name": "车牌", "type": "text", "required": true, "unique": true },
                    { "id": "status", "name": "状态", "type": "enum", "options": ["在用", "维修"] }
                  ] },
                { "type": "table", "id": "drivers", "name": "司机",
                  "fields": [ { "id": "name", "name": "姓名", "type": "text", "required": true } ] }
            ]
        }))
        .expect("schema");
        let mut seed = HashMap::new();
        seed.insert(
            "vehicles".to_string(),
            vec![serde_json::json!({ "plate": "京A12345", "status": "在用" })],
        );
        create_from_schema(root, schema, &seed).expect("build").id
    }

    #[test]
    fn resolve_missing_workspace_is_not_found() {
        let dir = tempfile::tempdir().expect("tmp");
        let svc = WorkspaceService::new(dir.path());
        assert!(matches!(
            svc.open_store("nope"),
            Err(ServiceError::NotFound(_))
        ));
        assert!(matches!(
            svc.read_schema("nope"),
            Err(ServiceError::NotFound(_))
        ));
    }

    #[test]
    fn open_store_and_query_roundtrip() {
        let dir = tempfile::tempdir().expect("tmp");
        let id = build_sample(dir.path());
        let svc = WorkspaceService::new(dir.path());

        let store = svc.open_store(&id).expect("open");
        let rows = store.query("vehicles", &[], None, 10, 0).expect("query");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["plate"], serde_json::json!("京A12345"));

        // 数据写入同样走 store（服务层不重复实现）
        store
            .insert("drivers", &[serde_json::json!({ "name": "张三" })], "user")
            .expect("insert");
        assert_eq!(store.count("drivers", &[]).expect("count"), 1);
    }

    #[test]
    fn table_fields_whitelist_and_list() {
        let dir = tempfile::tempdir().expect("tmp");
        let id = build_sample(dir.path());
        let svc = WorkspaceService::new(dir.path());
        let schema = svc.read_schema(&id).expect("schema");

        let fields = svc.table_fields(&schema, "vehicles").expect("fields");
        assert_eq!(fields.len(), 2);
        assert!(fields.iter().any(|f| f.id == "plate" && f.required));
        assert!(fields.iter().any(|f| f.id == "status" && f.is_enum()));

        assert!(matches!(
            svc.table_fields(&schema, "ghost"),
            Err(ServiceError::TableNotFound(_))
        ));

        let tables = svc.list_tables(&schema);
        assert_eq!(tables.len(), 2);
        assert!(tables.contains(&("vehicles".to_string(), "车辆".to_string())));
    }

    /// 手写 LLM 数据工具调用链（M3.5 验收）：
    /// 查表结构 → 查询 → 插入 → 更新 → 删除 → 计数，
    /// 与 workspace_* 6 工具的语义一致（table_schema/query/insert/
    /// update/delete/count 走同一 store 层）。
    #[test]
    fn llm_data_tool_chain_full_roundtrip() {
        let dir = tempfile::tempdir().expect("tmp");
        let id = build_sample(dir.path());
        let svc = WorkspaceService::new(dir.path());

        // 1. 查表结构（workspace_table_schema）：拿到字段白名单
        let schema = svc.read_schema(&id).expect("schema");
        let fields = svc.table_fields(&schema, "vehicles").expect("fields");
        let field_ids: Vec<&str> = fields.iter().map(|f| f.id.as_str()).collect();
        assert!(field_ids.contains(&"plate") && field_ids.contains(&"status"));
        assert!(fields.iter().any(|f| f.id == "status" && f.is_enum()));

        // 2. 查询（workspace_query）：初始 1 行
        let store = svc.open_store(&id).expect("open");
        let rows = store.query("vehicles", &[], None, 10, 0).expect("query");
        assert_eq!(rows.len(), 1);

        // 3. 插入（workspace_insert）：LLM 从对话提取的数据写入
        let ids = store
            .insert(
                "vehicles",
                &[serde_json::json!({ "plate": "京B67890", "status": "维修" })],
                "agent",
            )
            .expect("insert");
        assert_eq!(ids.len(), 1);
        assert_eq!(store.count("vehicles", &[]).expect("count"), 2);

        // 4. 更新（workspace_update）：按 id 改字段
        let target = store
            .query("vehicles", &[], None, 10, 0)
            .expect("query")
            .pop()
            .expect("row");
        let row_id = target["id"].as_str().expect("id");
        store
            .update("vehicles", row_id, &serde_json::json!({ "status": "在用" }), "agent")
            .expect("update");

        // 5. 过滤查询（带条件，模拟"查在用车辆"）
        let running = store
            .query(
                "vehicles",
                &[Filter {
                    field: "status".into(),
                    op: FilterOp::Eq,
                    value: serde_json::json!("在用"),
                }],
                None,
                10,
                0,
            )
            .expect("query");
        assert_eq!(running.len(), 2, "两行都应为在用");

        // 6. 删除（workspace_delete）+ 计数（workspace_count）
        store.delete("vehicles", row_id).expect("delete");
        let n = store.count("vehicles", &[]).expect("count");
        assert_eq!(n, 1);

        // 非法枚举值写入被拒（严格校验）
        let err = store
            .insert(
                "vehicles",
                &[serde_json::json!({ "plate": "京C1", "status": "报废" })],
                "agent",
            )
            .unwrap_err();
        assert!(err.to_string().contains("枚举") || err.to_string().contains("options"));
    }

    // ---------------------------------------------------------------------
    // 权限校验（M7.3）
    // ---------------------------------------------------------------------

    /// 构造带指定 permissions 的工作台。
    fn build_with_permissions(root: &Path, permissions: serde_json::Value) -> String {
        let schema: WorkspaceSchema = serde_json::from_value(serde_json::json!({
            "name": "权限测试",
            "permissions": permissions,
            "modules": [
                { "type": "table", "id": "vehicles", "name": "车辆",
                  "fields": [ { "id": "plate", "name": "车牌", "type": "text", "required": true, "unique": true } ] }
            ]
        }))
        .expect("schema");
        create_from_schema(root, schema, &HashMap::new()).expect("build").id
    }

    #[test]
    fn open_writable_allows_default_workspace() {
        let dir = tempfile::tempdir().expect("tmp");
        let id = build_sample(dir.path());
        let svc = WorkspaceService::new(dir.path());
        let w = svc.open_writable(&id).expect("默认工作台应可写");
        assert!(w.delete("vehicles", "ghost-row").is_err(), "行不存在仍应报错而非权限拒绝");
    }

    #[test]
    fn open_writable_rejects_readonly_workspace() {
        let dir = tempfile::tempdir().expect("tmp");
        let id = build_with_permissions(
            dir.path(),
            serde_json::json!({ "mode": "readonly", "allow_delete": false }),
        );
        let svc = WorkspaceService::new(dir.path());
        let err = svc.open_writable(&id).unwrap_err();
        assert!(matches!(err, ServiceError::PermissionDenied(_)), "{err}");
    }

    #[test]
    fn writable_delete_rejected_when_allow_delete_false() {
        let dir = tempfile::tempdir().expect("tmp");
        let id = build_with_permissions(
            dir.path(),
            serde_json::json!({ "mode": "readwrite", "allow_delete": false }),
        );
        let svc = WorkspaceService::new(dir.path());
        let w = svc.open_writable(&id).expect("可写");
        // 可写但禁删
        let err = w.delete("vehicles", "whatever").unwrap_err();
        assert!(matches!(err, ServiceError::PermissionDenied(_)), "{err}");
        // 整表替换（合并路径）不受 allow_delete 约束——语义是更新不是删除
        w.replace_rows("vehicles", &[serde_json::json!({ "plate": "京B00001" })])
            .expect("merge 替换应放行");
    }

    #[test]
    fn replace_rows_preserves_row_identity_and_timeline() {
        // P0-3 回归：导入携带原 id/updated_at 的行必须原样保留，
        // 否则 relation 外键悬空、LWW 时间线被重置。
        let dir = tempfile::tempdir().expect("tmp");
        let id = build_sample(dir.path());
        let svc = WorkspaceService::new(dir.path());

        // 先建一行拿到真实系统列
        let w = svc.open_writable(&id).expect("open");
        let rid = w
            .insert("drivers", &[serde_json::json!({ "name": "张三" })], "user")
            .expect("insert")
            .pop()
            .expect("id");
        let store = svc.open_store(&id).expect("store");
        let original = store.query("drivers", &[], None, 10, 0).expect("query").pop().expect("row");

        // 模拟 merge：携带原 id + updated_at 重灌
        let merged = serde_json::json!({
            "id": rid,
            "name": "张三改",
            "created_at": original["created_at"],
            "updated_at": original["updated_at"],
        });
        w.replace_rows("drivers", &[merged]).expect("replace");

        let rows = svc.open_store(&id).expect("re-open").query("drivers", &[], None, 10, 0).expect("query");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], rid.as_str(), "行主键不得重生成");
        assert_eq!(rows[0]["updated_at"], original["updated_at"], "时间线不得重置");

        // 不带 id 的新行走正常生成路径
        w.replace_rows("drivers", &[serde_json::json!({ "name": "王五" })]).expect("replace2");
        let rows = svc.open_store(&id).expect("re-open").query("drivers", &[], None, 10, 0).expect("query");
        assert_eq!(rows.len(), 1);
        assert_ne!(rows[0]["id"], rid.as_str());
    }
}
