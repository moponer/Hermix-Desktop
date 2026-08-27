//! Workspace management API — schema-driven workbenches (registry + definition).
//!
//! 存储布局（`docs/工作台架构设计方案.md` §8.1）：
//! ```text
//! ~/.hermix/workspaces/
//! ├── <ws_id>/workspace.json + data.sqlite + files/ + audit.sqlite
//! └── registry.sqlite
//! ```
//!
//! M1.3 提供注册表级 CRUD + schema 读取（WorkspaceTabs / WorkspaceRenderer 前置）；
//! 数据 CRUD 端点在 M1.6（打通 LocalSqliteStore）。

use std::path::PathBuf;

use axum::extract::{Path, State};
use axum::Json;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};

use crate::{HttpError, HttpServerState};

use hermix_workspace::{
    Filter, LocalSqliteStore, RegistryEntry, RegistryStore, WorkspaceSchema, WorkspaceStore,
};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// 创建/重命名工作台请求体。
#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceMutateRequest {
    pub name: String,
    #[serde(default)]
    pub icon: String,
}

/// 工作台详情（注册表条目 + 定义）。
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceDetail {
    #[serde(flatten)]
    pub entry: RegistryEntry,
    pub schema: WorkspaceSchema,
}

/// 通用 ok 响应。
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceOkResponse {
    pub ok: bool,
    pub id: String,
}

// ---------------------------------------------------------------------------
// 存储路径 helper
// ---------------------------------------------------------------------------

fn workspaces_root(state: &HttpServerState) -> PathBuf {
    state.hermix_home.join("workspaces")
}

fn registry_path(state: &HttpServerState) -> PathBuf {
    workspaces_root(state).join("registry.sqlite")
}

fn ws_dir(state: &HttpServerState, id: &str) -> PathBuf {
    workspaces_root(state).join(id)
}

fn schema_path(state: &HttpServerState, id: &str) -> PathBuf {
    ws_dir(state, id).join("workspace.json")
}

fn open_registry(state: &HttpServerState) -> Result<RegistryStore, HttpError> {
    RegistryStore::open(registry_path(state))
        .map_err(|e| server_err(&format!("打开工作台注册表失败: {e}")))
}

fn read_schema(state: &HttpServerState, id: &str) -> Result<WorkspaceSchema, HttpError> {
    let raw = std::fs::read(schema_path(state, id))
        .map_err(|e| HttpError {
            status: axum::http::StatusCode::NOT_FOUND,
            message: format!("工作台[{id}]定义不存在: {e}"),
        })?;
    serde_json::from_slice(&raw).map_err(|e| server_err(&format!("工作台[{id}]定义解析失败: {e}")))
}

fn server_err(msg: &str) -> HttpError {
    HttpError {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        message: msg.to_string(),
    }
}

fn bad_req(msg: &str) -> HttpError {
    HttpError {
        status: axum::http::StatusCode::BAD_REQUEST,
        message: msg.to_string(),
    }
}

// ---------------------------------------------------------------------------
// GET /api/workspaces — 列表
// ---------------------------------------------------------------------------

pub async fn list_workspaces(
    State(state): State<HttpServerState>,
) -> Result<Json<Vec<RegistryEntry>>, HttpError> {
    let registry = open_registry(&state)?;
    let entries = registry.list().map_err(|e| server_err(&e.to_string()))?;
    Ok(Json(entries))
}

// ---------------------------------------------------------------------------
// POST /api/workspaces — 创建（注册表条目 + 目录 + 默认定义）
// ---------------------------------------------------------------------------

pub async fn create_workspace(
    State(state): State<HttpServerState>,
    Json(req): Json<WorkspaceMutateRequest>,
) -> Result<Json<WorkspaceOkResponse>, HttpError> {
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err(bad_req("工作台名称不能为空"));
    }
    let id = uuid::Uuid::new_v4().to_string();

    // 1. 建目录 + 默认定义
    let dir = ws_dir(&state, &id);
    std::fs::create_dir_all(&dir).map_err(|e| server_err(&format!("创建工作台目录失败: {e}")))?;
    let schema = WorkspaceSchema {
        schema_version: 1,
        name: name.clone(),
        icon: req.icon.clone(),
        modules: Vec::new(),
        layout: serde_json::Value::Object(Default::default()),
        automations: Vec::new(),
        permissions: serde_json::Value::Object(Default::default()),
    };
    let raw = serde_json::to_vec_pretty(&schema)
        .map_err(|e| server_err(&format!("序列化工作台定义失败: {e}")))?;
    std::fs::write(schema_path(&state, &id), raw)
        .map_err(|e| server_err(&format!("写入工作台定义失败: {e}")))?;

    // 2. 注册表条目
    let registry = open_registry(&state)?;
    registry
        .create(&id, &name, &req.icon, &id)
        .map_err(|e| server_err(&e.to_string()))?;

    Ok(Json(WorkspaceOkResponse { ok: true, id }))
}

// ---------------------------------------------------------------------------
// GET /api/workspaces/{id} — 详情（注册表 + schema）
// ---------------------------------------------------------------------------

pub async fn get_workspace(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<Json<WorkspaceDetail>, HttpError> {
    let registry = open_registry(&state)?;
    let entry = registry
        .get(&id)
        .map_err(|e| HttpError {
            status: axum::http::StatusCode::NOT_FOUND,
            message: e.to_string(),
        })?;
    let schema = read_schema(&state, &id)?;
    Ok(Json(WorkspaceDetail { entry, schema }))
}

// ---------------------------------------------------------------------------
// PUT /api/workspaces/{id} — 重命名（注册表 + 定义同步）
// ---------------------------------------------------------------------------

pub async fn rename_workspace(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
    Json(req): Json<WorkspaceMutateRequest>,
) -> Result<Json<WorkspaceOkResponse>, HttpError> {
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err(bad_req("工作台名称不能为空"));
    }
    let registry = open_registry(&state)?;
    registry.rename(&id, &name).map_err(|e| HttpError {
        status: axum::http::StatusCode::NOT_FOUND,
        message: e.to_string(),
    })?;

    // 定义同步改名（若存在）
    if let Ok(mut schema) = read_schema(&state, &id) {
        schema.name = name;
        if !req.icon.is_empty() {
            schema.icon = req.icon;
        }
        if let Ok(raw) = serde_json::to_vec_pretty(&schema) {
            let _ = std::fs::write(schema_path(&state, &id), raw);
        }
    }

    Ok(Json(WorkspaceOkResponse { ok: true, id }))
}

// ---------------------------------------------------------------------------
// DELETE /api/workspaces/{id} — 删除（注册表条目 + 数据目录）
// ---------------------------------------------------------------------------

pub async fn delete_workspace(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<Json<WorkspaceOkResponse>, HttpError> {
    let registry = open_registry(&state)?;
    registry.remove(&id).map_err(|e| HttpError {
        status: axum::http::StatusCode::NOT_FOUND,
        message: e.to_string(),
    })?;
    // 数据目录删除失败不阻断（注册表已删，目录残留由后续清理兜底）
    let dir = ws_dir(&state, &id);
    if dir.exists() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    Ok(Json(WorkspaceOkResponse { ok: true, id }))
}

// ---------------------------------------------------------------------------
// PUT /api/workspaces/{id}/schema — 更新定义（布局/模块变更持久化）
// ---------------------------------------------------------------------------

pub async fn update_workspace_schema(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
    Json(schema): Json<WorkspaceSchema>,
) -> Result<Json<WorkspaceOkResponse>, HttpError> {
    // 校验后再落盘（M0 validate 兜底，防坏 schema 写坏定义文件）
    hermix_workspace::validate_schema(&schema).map_err(|e| bad_req(&e.to_string()))?;
    let raw = serde_json::to_vec_pretty(&schema)
        .map_err(|e| server_err(&format!("序列化工作台定义失败: {e}")))?;
    std::fs::write(schema_path(&state, &id), raw)
        .map_err(|e| server_err(&format!("写入工作台定义失败: {e}")))?;
    Ok(Json(WorkspaceOkResponse { ok: true, id }))
}

// ---------------------------------------------------------------------------
// 数据端点 — 打通 LocalSqliteStore（M1.6）
// ---------------------------------------------------------------------------

/// 打开工作台数据 store（读定义 → 打开/建表）。
fn open_data_store(state: &HttpServerState, id: &str) -> Result<LocalSqliteStore, HttpError> {
    let schema = read_schema(state, id)?;
    let data_path = ws_dir(state, id).join("data.sqlite");
    LocalSqliteStore::open(&data_path, schema).map_err(|e| server_err(&e.to_string()))
}

/// 表路径参数：`/api/workspaces/{id}/tables/{table}/...`
type TablePath = (String, String);

#[derive(Debug, Clone, Deserialize)]
pub struct TableQueryRequest {
    #[serde(default)]
    pub filters: Vec<Filter>,
    pub sort: Option<String>,
    /// 0 = 不限（谨慎使用；前端默认给上限防大表拖垮）
    #[serde(default)]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TableInsertRequest {
    pub rows: Vec<serde_json::Value>,
    #[serde(default = "default_updated_by")]
    pub updated_by: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TableUpdateRequest {
    pub id: String,
    pub patch: serde_json::Value,
    #[serde(default = "default_updated_by")]
    pub updated_by: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TableDeleteRequest {
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TableCountRequest {
    #[serde(default)]
    pub filters: Vec<Filter>,
}

fn default_updated_by() -> String {
    "user".to_string()
}

pub async fn query_table(
    State(state): State<HttpServerState>,
    Path((id, table)): Path<TablePath>,
    Json(req): Json<TableQueryRequest>,
) -> Result<Json<Vec<serde_json::Value>>, HttpError> {
    let store = open_data_store(&state, &id)?;
    let rows = store
        .query(&table, &req.filters, req.sort.as_deref(), req.limit, req.offset)
        .map_err(|e| server_err(&e.to_string()))?;
    Ok(Json(rows))
}

pub async fn insert_rows(
    State(state): State<HttpServerState>,
    Path((id, table)): Path<TablePath>,
    Json(req): Json<TableInsertRequest>,
) -> Result<Json<Vec<String>>, HttpError> {
    let store = open_data_store(&state, &id)?;
    let ids = store
        .insert(&table, &req.rows, &req.updated_by)
        .map_err(|e| server_err(&e.to_string()))?;
    Ok(Json(ids))
}

pub async fn update_row(
    State(state): State<HttpServerState>,
    Path((id, table)): Path<TablePath>,
    Json(req): Json<TableUpdateRequest>,
) -> Result<Json<WorkspaceOkResponse>, HttpError> {
    let store = open_data_store(&state, &id)?;
    store
        .update(&table, &req.id, &req.patch, &req.updated_by)
        .map_err(|e| server_err(&e.to_string()))?;
    Ok(Json(WorkspaceOkResponse { ok: true, id }))
}

pub async fn delete_row(
    State(state): State<HttpServerState>,
    Path((id, table)): Path<TablePath>,
    Json(req): Json<TableDeleteRequest>,
) -> Result<Json<WorkspaceOkResponse>, HttpError> {
    let store = open_data_store(&state, &id)?;
    store
        .delete(&table, &req.id)
        .map_err(|e| server_err(&e.to_string()))?;
    Ok(Json(WorkspaceOkResponse { ok: true, id }))
}

pub async fn count_rows(
    State(state): State<HttpServerState>,
    Path((id, table)): Path<TablePath>,
    Json(req): Json<TableCountRequest>,
) -> Result<Json<u64>, HttpError> {
    let store = open_data_store(&state, &id)?;
    let n = store
        .count(&table, &req.filters)
        .map_err(|e| server_err(&e.to_string()))?;
    Ok(Json(n))
}

// ---------------------------------------------------------------------------
// 导出 / 导入（M5 协作分发）
// ---------------------------------------------------------------------------

/// GET /api/workspaces/{id}/export → .hwx 包下载（定义 + 全量数据）。
/// 文件名 `{ws_id}.hwx`（避免中文名编码问题；显示名在包内）。
pub async fn export_workspace(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<axum::response::Response, HttpError> {
    let schema = read_schema(&state, &id)?;
    let store = open_data_store(&state, &id)?;
    let pkg = hermix_workspace::export_package(&store, &schema)
        .map_err(|e| server_err(&format!("导出工作台失败: {e}")))?;
    let bytes = hermix_workspace::to_bytes(&pkg)
        .map_err(|e| server_err(&format!("序列化包失败: {e}")))?;

    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    headers.insert(
        axum::http::header::CONTENT_DISPOSITION,
        axum::http::HeaderValue::from_str(&format!("attachment; filename=\"{id}.hwx\""))
            .map_err(|e| server_err(&format!("响应头构造失败: {e}")))?,
    );
    Ok((
        axum::http::StatusCode::OK,
        headers,
        bytes,
    )
        .into_response())
}

/// POST /api/workspaces/import — 导入 .hwx 包（JSON body）。
/// 冲突策略（`mode`）：
/// - `new`（默认）：新建工作台，包内 id 重生成，避免覆盖本机同名
/// - `merge`：合并到已有工作台（按 id 匹配，LWW 每行 updated_at 比对）
#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceImportRequest {
    /// .hwx 包内容（`parse_package` 的输入）。
    pub package: serde_json::Value,
    #[serde(default)]
    pub mode: String,
}

/// 导入结果。
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceImportResponse {
    pub ok: bool,
    /// 新建时为新工作台 id；merge 时为目标工作台 id。
    pub id: String,
    pub mode: String,
    /// 各表导入行数（merge 模式为合并后总行数）。
    pub tables: std::collections::HashMap<String, usize>,
}

pub async fn import_workspace(
    State(state): State<HttpServerState>,
    Json(req): Json<WorkspaceImportRequest>,
) -> Result<Json<WorkspaceImportResponse>, HttpError> {
    // 1. 解析 + 校验包（防脏包：格式标记/版本/schema/表 id/字段白名单/必填/枚举）
    let raw = serde_json::to_vec(&req.package)
        .map_err(|e| bad_req(&format!("包序列化失败: {e}")))?;
    let pkg = hermix_workspace::parse_package(&raw)
        .map_err(|e| bad_req(&format!("包无效: {e}")))?;

    let mode = req.mode.as_str();
    match mode {
        "merge" => {
            // 2a. merge：按包内工作台名找本机已有工作台
            let registry = open_registry(&state)?;
            let entries = registry
                .list()
                .map_err(|e| server_err(&format!("读取工作台注册表失败: {e}")))?;
            let target = entries
                .iter()
                .find(|e| e.name == pkg.workspace.name)
                .ok_or_else(|| {
                    bad_req(&format!(
                        "merge 模式需要本机存在同名工作台「{}」，未找到；可改用 new 模式",
                        pkg.workspace.name
                    ))
                })?;

            let target_id = target.id.clone();
            let target_schema = read_schema(&state, &target_id)?;
            let store = open_data_store(&state, &target_id)?;

            // LWW 合并：每表 本地行 + 包行（按 id 对齐，updated_at 较新者胜）
            let mut merged_counts = std::collections::HashMap::new();
            for m in &pkg.workspace.modules {
                if let hermix_workspace::Module::Table { id, .. } = m {
                    if let Some(incoming) = pkg.tables.get(id) {
                        let local = store
                            .query(id, &[], None, 0, 0)
                            .map_err(|e| server_err(&format!("表[{id}]读取失败: {e}")))?;
                        let merged = hermix_workspace::merge_rows(&local, incoming);
                        // 全量重写该表（合并结果）
                        rewrite_table(&store, id, &merged)?;
                        merged_counts.insert(id.clone(), merged.len());
                    }
                }
            }

            Ok(Json(WorkspaceImportResponse {
                ok: true,
                id: target_id,
                mode: "merge".into(),
                tables: merged_counts,
            }))
        }
        "new" | "" => {
            // 2b. new：新建工作台（定义 + 数据；id 由 create_from_schema 生成）
            let ws_name = pkg.workspace.name.clone();
            let ws_icon = pkg.workspace.icon.clone();
            let result =
                hermix_workspace::builder::create_from_schema(&workspaces_root(&state), pkg.workspace, &pkg.tables)
                    .map_err(|e| server_err(&format!("建设工作台失败: {e}")))?;
            // 注册进 registry
            let registry = open_registry(&state)?;
            registry
                .create(&result.id, &ws_name, &ws_icon, &result.id)
                .map_err(|e| server_err(&format!("注册工作台失败: {e}")))?;

            let mut counts = std::collections::HashMap::new();
            for t in &result.tables {
                counts.insert(t.id.clone(), t.rows);
            }
            Ok(Json(WorkspaceImportResponse {
                ok: true,
                id: result.id,
                mode: "new".into(),
                tables: counts,
            }))
        }
        other => Err(bad_req(&format!(
            "无效 mode: {other}（支持 new / merge）"
        ))),
    }
}

/// 全量重写一张表（merge 模式用）：清空后批量插入合并结果。
/// 表内数据由 LWW 合并产生，清空重写是安全幂等的（行 id 保留）。
fn rewrite_table(
    store: &LocalSqliteStore,
    table: &str,
    rows: &[serde_json::Value],
) -> Result<(), HttpError> {
    let ids: Vec<String> = store
        .query(table, &[], None, 0, 0)
        .map_err(|e| server_err(&format!("表[{table}]读取失败: {e}")))?
        .iter()
        .filter_map(|r| r.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .collect();
    for rid in &ids {
        store
            .delete(table, rid)
            .map_err(|e| server_err(&format!("表[{table}]清空失败: {e}")))?;
    }
    if !rows.is_empty() {
        // 保留包内原始 id/updated_at/updated_by（merge 语义：行主键 UUID 不重生成）
        let rows_with_sys = rows
            .iter()
            .map(|r| {
                let mut v = r.clone();
                let o = v.as_object_mut().expect("行是对象");
                if !o.contains_key("updated_by") {
                    o.insert("updated_by".into(), serde_json::json!("import"));
                }
                v
            })
            .collect::<Vec<_>>();
        store
            .insert(table, &rows_with_sys, "import")
            .map_err(|e| server_err(&format!("表[{table}]导入失败: {e}")))?;
    }
    Ok(())
}
