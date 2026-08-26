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
