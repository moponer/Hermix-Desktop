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

    // 定义同步改名（若存在）。序列化/写盘失败只降级不回滚——注册表已改名，
    // 但必须留痕，否则两边名字静默漂移无从排查。
    if let Ok(mut schema) = read_schema(&state, &id) {
        schema.name = name;
        if !req.icon.is_empty() {
            schema.icon = req.icon;
        }
        match serde_json::to_vec_pretty(&schema)
            .map_err(|e| e.to_string())
            .and_then(|raw| std::fs::write(schema_path(&state, &id), raw).map_err(|e| e.to_string()))
        {
            Ok(()) => {}
            Err(e) => tracing::warn!(workspace = %id, error = %e, "改名同步到定义文件失败，注册表与定义名可能漂移"),
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
    // 数据目录删除失败不阻断（注册表已删，目录残留由后续清理兜底），但必须留痕
    let dir = ws_dir(&state, &id);
    if dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            tracing::warn!(workspace = %id, error = %e, "工作台数据目录删除失败，目录残留");
        }
    }
    Ok(Json(WorkspaceOkResponse { ok: true, id }))
}

/// PUT /api/workspaces/{id}/layout — 仅替换布局层。
///
/// 独立于 schema 端点的原因：布局拖拽防抖保存持有的是打开时的 schema
/// 快照，整 schema 回写会把期间 LLM 增删的模块/字段覆盖掉（丢失更新）。
pub async fn update_workspace_layout(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
    Json(req): Json<LayoutUpdateRequest>,
) -> Result<Json<WorkspaceOkResponse>, HttpError> {
    let mut schema = read_schema(&state, &id)?;
    schema.layout = req.layout;
    let raw = serde_json::to_vec_pretty(&schema)
        .map_err(|e| server_err(&format!("序列化工作台定义失败: {e}")))?;
    std::fs::write(schema_path(&state, &id), raw)
        .map_err(|e| server_err(&format!("写入工作台定义失败: {e}")))?;
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
    /// 关键字搜索（P1）：非空时跨全部文本类字段 OR LIKE（store.search_text）。
    #[serde(default)]
    pub keyword: Option<String>,
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
    /// M5：审计来源（user / llm / automation）。缺省按 "user"。
    #[serde(default = "default_updated_by")]
    pub updated_by: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TableCountRequest {
    #[serde(default)]
    pub filters: Vec<Filter>,
    /// 关键字搜索（P1）：非空时按 search 口径计数。
    #[serde(default)]
    pub keyword: Option<String>,
}

/// PUT /{id}/layout 请求体。
#[derive(Debug, Clone, Deserialize)]
pub struct LayoutUpdateRequest {
    pub layout: serde_json::Value,
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
    let rows = match req.keyword.as_deref().map(str::trim) {
        Some(kw) if !kw.is_empty() => store
            .search_text(&table, kw, req.limit, req.offset)
            .map_err(|e| server_err(&e.to_string()))?,
        _ => store
            .query(&table, &req.filters, req.sort.as_deref(), req.limit, req.offset)
            .map_err(|e| server_err(&e.to_string()))?,
    };
    Ok(Json(rows))
}

pub async fn insert_rows(
    State(state): State<HttpServerState>,
    Path((id, table)): Path<TablePath>,
    Json(req): Json<TableInsertRequest>,
) -> Result<Json<Vec<String>>, HttpError> {
    // 写能力句柄：只读工作台在此被拒（403），LLM 工具与 REST 同源校验
    let writable = hermix_workspace::WorkspaceService::new(workspaces_root(&state))
        .open_writable(&id)
        .map_err(HttpError::from)?;
    let ids = writable
        .insert(&table, &req.rows, &req.updated_by)
        .map_err(|e| server_err(&e.to_string()))?;
    Ok(Json(ids))
}

pub async fn update_row(
    State(state): State<HttpServerState>,
    Path((id, table)): Path<TablePath>,
    Json(req): Json<TableUpdateRequest>,
) -> Result<Json<WorkspaceOkResponse>, HttpError> {
    let writable = hermix_workspace::WorkspaceService::new(workspaces_root(&state))
        .open_writable(&id)
        .map_err(HttpError::from)?;
    writable
        .update(&table, &req.id, &req.patch, &req.updated_by)
        .map_err(|e| server_err(&e.to_string()))?;
    Ok(Json(WorkspaceOkResponse { ok: true, id }))
}

pub async fn delete_row(
    State(state): State<HttpServerState>,
    Path((id, table)): Path<TablePath>,
    Json(req): Json<TableDeleteRequest>,
) -> Result<Json<WorkspaceOkResponse>, HttpError> {
    let writable = hermix_workspace::WorkspaceService::new(workspaces_root(&state))
        .open_writable(&id)
        .map_err(HttpError::from)?;
    writable
        .delete(&table, &req.id, &req.updated_by)
        .map_err(|e| server_err(&e.to_string()))?;
    Ok(Json(WorkspaceOkResponse { ok: true, id }))
}

pub async fn count_rows(
    State(state): State<HttpServerState>,
    Path((id, table)): Path<TablePath>,
    Json(req): Json<TableCountRequest>,
) -> Result<Json<u64>, HttpError> {
    let store = open_data_store(&state, &id)?;
    let n = match req.keyword.as_deref().map(str::trim) {
        Some(kw) if !kw.is_empty() => store
            .count_search_text(&table, kw)
            .map_err(|e| server_err(&e.to_string()))?,
        _ => store
            .count(&table, &req.filters)
            .map_err(|e| server_err(&e.to_string()))?,
    };
    Ok(Json(n))
}

/// POST /api/workspaces/{id}/tables/{table}/group_count — 分组计数（M1.6 仪表盘 chart 用）。
/// body: `{ group_by: String, limit?: usize }`，列名过 schema 白名单。
#[derive(Debug, Clone, Deserialize)]
pub struct GroupCountRequest {
    pub group_by: String,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupCountResponse {
    pub groups: Vec<GroupCountEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupCountEntry {
    pub label: String,
    pub count: u64,
}

pub async fn group_count(
    State(state): State<HttpServerState>,
    Path((id, table)): Path<TablePath>,
    Json(req): Json<GroupCountRequest>,
) -> Result<Json<GroupCountResponse>, HttpError> {
    let store = open_data_store(&state, &id)?;
    let limit = req.limit.unwrap_or(20).clamp(1, 100);
    let rows = store
        .count_group_by(&table, &req.group_by, limit)
        .map_err(|e| bad_req(&e.to_string()))?;
    let groups = rows
        .into_iter()
        .map(|(label, count)| GroupCountEntry { label, count })
        .collect();
    Ok(Json(GroupCountResponse { groups }))
}

/// GET /api/workspaces/{id}/tables/{table}/changes — 最近变更记录（P6）。
pub async fn list_table_changes(
    State(state): State<HttpServerState>,
    Path((id, table)): Path<TablePath>,
) -> Result<Json<Vec<hermix_workspace::store::ChangeEntry>>, HttpError> {
    let store = open_data_store(&state, &id)?;
    let changes = store
        .list_changes(&table, 20)
        .map_err(|e| server_err(&e.to_string()))?;
    Ok(Json(changes))
}

/// POST /api/workspaces/{id}/change-log/purge — 清理 change_log（M2）。
/// body: `{ older_than_days: 90, keep_recent_undone: 20 }`
/// 返回被清理的记录数。`older_than_days <= 0` 表示不按时间清理；`keep_recent_undone` 保留最近 N 条已撤销行（不清理）——避免"撤销链"被彻底抹掉。
pub async fn purge_change_log(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
    Json(req): Json<PurgeChangeLogRequest>,
) -> Result<Json<WorkspaceOkResponse>, HttpError> {
    let writable = hermix_workspace::WorkspaceService::new(workspaces_root(&state))
        .open_writable(&id)
        .map_err(HttpError::from)?;
    let n = writable
        .purge_change_log(req.older_than_days, req.keep_recent_undone)
        .map_err(|e| server_err(&e.to_string()))?;
    tracing::info!(workspace = %id, purged = n, "change_log 清理完成");
    Ok(Json(WorkspaceOkResponse { ok: true, id }))
}

#[derive(Debug, Clone, Deserialize)]
pub struct PurgeChangeLogRequest {
    #[serde(default = "default_purge_days")]
    pub older_than_days: i64,
    #[serde(default = "default_keep_undone")]
    pub keep_recent_undone: usize,
}
fn default_purge_days() -> i64 {
    90
}
fn default_keep_undone() -> usize {
    20
}

/// POST /api/workspaces/{id}/tables/{table}/undo — 撤销该表最近一条变更（P6）。
/// 返回撤销的变更记录（无变更时返回 null）。
///
/// H2：撤销也是写（把行从 before 还原到 after），必须走 `open_writable` 闸门——
/// 只读工作台在此 403，与 update/ INSERT/ delete 同源校验。撤销可能恢复被删行，
/// 不受 `allow_delete` 约束（Writable 不挡这层）。
pub async fn undo_last_change(
    State(state): State<HttpServerState>,
    Path((id, table)): Path<TablePath>,
) -> Result<Json<Option<hermix_workspace::store::ChangeEntry>>, HttpError> {
    let writable = hermix_workspace::WorkspaceService::new(workspaces_root(&state))
        .open_writable(&id)
        .map_err(HttpError::from)?;
    let undone = writable
        .undo_last(&table)
        .map_err(|e| server_err(&e.to_string()))?;
    Ok(Json(undone))
}

// ---------------------------------------------------------------------------
// 自动化可视化（P3）
// ---------------------------------------------------------------------------

/// GET /api/workspaces/{id}/automations — 工作台自动化定义列表（schema 内）。
pub async fn list_automations(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<hermix_workspace::Automation>>, HttpError> {
    let schema = read_schema(&state, &id)?;
    Ok(Json(schema.automations))
}

/// 单条自动化的触发记录（自动化 id + 最近 runs）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AutomationRunsEntry {
    pub automation_id: String,
    /// L4：调度器是否启用——前端用这个显式标记"暂无运行记录"的
    /// 原因（没启动调度器 vs 调度器空跑），而不是永远显示空列表让用户
    /// 误以为自动化从来没跑过。
    pub available: bool,
    pub runs: Vec<hermix_automation::AutomationRun>,
}

/// GET /api/workspaces/{id}/automations/runs — 每个自动化的最近触发记录
/// （最多 5 条；automation_service 未启动时 runs 为空且 available=false）。
pub async fn automation_runs(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<AutomationRunsEntry>>, HttpError> {
    let schema = read_schema(&state, &id)?;
    let svc = state.automation_service.clone();
    let available = svc.is_some();
    let mut out = Vec::new();
    for auto in &schema.automations {
        let aid = auto.id().to_string();
        let runs = match &svc {
            Some(s) => s.list_runs(&aid, 5).unwrap_or_default(),
            None => Vec::new(),
        };
        out.push(AutomationRunsEntry {
            automation_id: aid,
            available,
            runs,
        });
    }
    Ok(Json(out))
}

// ---------------------------------------------------------------------------
// 附件上传 / 读取（P5）— base64 JSON 通道（零新依赖，axum 未启用 multipart）
// ---------------------------------------------------------------------------

/// POST /api/workspaces/{id}/files/upload — 上传附件。
/// body: `{ name, data_base64 }`；存 `files/<uuid>.<ext>`，返回相对路径
/// （作为 attachment 字段值落库）。
#[derive(Debug, Clone, Deserialize)]
pub struct FileUploadRequest {
    pub name: String,
    pub data_base64: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileUploadResponse {
    pub ok: bool,
    /// 相对路径（`files/<filename>`），attachment 字段存此值。
    pub path: String,
}

/// M6：允许上传的附件扩展名白名单。设计 §17.3「类型白名单」对齐——
/// 含 `.exe` / `.bat` / `.html` / `.svg` 等可执行/脚本类一律拒绝；防止钓鱼面
/// （`<img src=".svg">` 自身不执行 SVG 脚本，但浏览器对 `<a href=".html">`
/// 直链下载 + 二次跳转里仍构成社工链路）。
const ALLOWED_ATTACHMENT_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", // 图片
    "pdf",                                      // 文档
    "csv", "json",                              // 数据
    "txt", "md", "log",                          // 文本
    "xlsx", "xls",                               // 表格（前端解析依赖）
];

pub async fn upload_file(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
    Json(req): Json<FileUploadRequest>,
) -> Result<Json<FileUploadResponse>, HttpError> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(req.data_base64.trim())
        .map_err(|e| bad_req(&format!("base64 解码失败: {e}")))?;
    if bytes.is_empty() {
        return Err(bad_req("空文件"));
    }
    if bytes.len() > 20 * 1024 * 1024 {
        return Err(bad_req("附件超过 20MB 上限"));
    }
    // 文件名只取安全扩展名（用户文件名不落盘，防注入/穿越）。
    // 扩展名命中 ALLOWED_ATTACHMENT_EXTS 才放行；否则返回 400（拒绝可执行/脚本类）。
    let raw_ext = std::path::Path::new(&req.name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .filter(|e| !e.is_empty() && e.len() <= 10);
    let ext = match raw_ext {
        Some(e) if ALLOWED_ATTACHMENT_EXTS.contains(&e.as_str()) => e,
        _ => return Err(bad_req(&format!(
            "不支持的附件扩展名；允许: {}",
            ALLOWED_ATTACHMENT_EXTS.join(", ")
        ))),
    };
    let filename = format!("{}.{ext}", uuid::Uuid::new_v4().simple());
    let dir = ws_dir(&state, &id).join("files");
    std::fs::create_dir_all(&dir)
        .map_err(|e| server_err(&format!("创建附件目录失败: {e}")))?;
    std::fs::write(dir.join(&filename), &bytes)
        .map_err(|e| server_err(&format!("写入附件失败: {e}")))?;
    Ok(Json(FileUploadResponse {
        ok: true,
        path: format!("files/{filename}"),
    }))
}

/// 按扩展名推断 Content-Type（覆盖常见图片/文档）。与 ALLOWED_ATTACHMENT_EXTS 对齐：
/// 白名单外的扩展根本不会上传，所以这里不需要兜底类型，按精确映射返回。
fn mime_from_ext(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "csv" => "text/csv",
        "json" => "application/json",
        "txt" | "md" | "log" => "text/plain",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "xls" => "application/vnd.ms-excel",
        _ => "application/octet-stream",
    }
}

/// GET /api/workspaces/{id}/files/{filename} — 读取附件。
/// filename 白名单校验（无路径穿越），Content-Type 按扩展名。
pub async fn get_file(
    State(state): State<HttpServerState>,
    Path((id, filename)): Path<(String, String)>,
) -> Result<axum::response::Response, HttpError> {
    // 防路径穿越：只允许纯文件名（无分隔符/..）
    if filename.is_empty()
        || filename.contains('/')
        || filename.contains('\\')
        || filename.contains("..")
    {
        return Err(bad_req("非法文件名"));
    }
    let path = ws_dir(&state, &id).join("files").join(&filename);
    let bytes = std::fs::read(&path).map_err(|_| HttpError {
        status: axum::http::StatusCode::NOT_FOUND,
        message: "附件不存在".to_string(),
    })?;
    let ext = std::path::Path::new(&filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static(mime_from_ext(ext)),
    );
    Ok((
        axum::http::StatusCode::OK,
        headers,
        bytes,
    )
        .into_response())
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
            // 写权限门（M7.3）：只读工作台拒绝合并写入（此前整条 import
            // 路径绕过了数据端点的权限检查，可清空 readonly 工作台）
            let writable = hermix_workspace::WorkspaceService::new(workspaces_root(&state))
                .open_writable(&target_id)
                .map_err(HttpError::from)?;
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
                        // 事务内整表替换，行主键/时间线保留（LWW 语义）
                        let n = writable
                            .replace_rows(id, &merged)
                            .map_err(|e| server_err(&format!("表[{id}]合并失败: {e}")))?;
                        merged_counts.insert(id.clone(), n);
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
        "upsert" => {
            // 2c. upsert（L5）：合并到已有工作台，按表 unique 字段逐行 INSERT/UPDATE
            // — 适合"定期再导同一份名单"的场景，避免整表覆盖丢失中间改动。
            let registry = open_registry(&state)?;
            let entries = registry
                .list()
                .map_err(|e| server_err(&format!("读取工作台注册表失败: {e}")))?;
            let target = entries
                .iter()
                .find(|e| e.name == pkg.workspace.name)
                .ok_or_else(|| {
                    bad_req(&format!(
                        "upsert 模式需要本机存在同名工作台「{}」，未找到；可改用 new 模式",
                        pkg.workspace.name
                    ))
                })?;
            let target_id = target.id.clone();
            let writable = hermix_workspace::WorkspaceService::new(workspaces_root(&state))
                .open_writable(&target_id)
                .map_err(HttpError::from)?;
            // 校验包内表与目标工作台定义兼容（不重建表——只刷数据）
            hermix_workspace::validate_schema(&pkg.workspace)
                .map_err(|e| bad_req(&format!("包内 schema 校验失败: {e}")))?;
            let mut counts = std::collections::HashMap::new();
            for m in &pkg.workspace.modules {
                if let hermix_workspace::Module::Table { id, .. } = m {
                    if let Some(incoming) = pkg.tables.get(id) {
                        let (_ins, _upd) = writable
                            .upsert_by_unique(id, incoming, "import")
                            .map_err(|e| server_err(&format!("表[{id}] upsert 失败: {e}")))?;
                        // upsert 后表行数即"upsert 前 + 真正新增 - 0"，统一为入参行数
                        // （schema 定义表存在则非空）。前端 UI 拿表行数做"已导入 N 行"提示。
                        counts.insert(id.clone(), incoming.len());
                        tracing::info!(
                            table = %id,
                            rows = incoming.len(),
                            "upsert 导入完成"
                        );
                    }
                }
            }
            Ok(Json(WorkspaceImportResponse {
                ok: true,
                id: target_id,
                mode: "upsert".into(),
                tables: counts,
            }))
        }
        other => Err(bad_req(&format!(
            "无效 mode: {other}（支持 new / merge / upsert）"
        ))),
    }
}
