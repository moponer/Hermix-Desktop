//! Workspace templates API — 内置预置台账模板（M6.3/M6.4）。
//!
//! - `GET  /api/workspace-templates`：模板列表（元信息 + 表清单预览，
//!   不含 schema 全文——列表页只需要看"有什么"）
//! - `POST /api/workspace-templates/{id}/create`：按模板建工作台
//!   （复用 M2 `create_from_schema`，模板 schema 已在分发前校验）

use axum::extract::{Path, State};
use axum::Json;

use crate::{HttpError, HttpServerState};

use hermix_workspace::templates::{get_template, list_template_metas};

/// GET /api/workspace-templates — 模板列表（元信息 + 表清单）。
pub async fn list_templates(
) -> Result<Json<Vec<hermix_workspace::TemplateMeta>>, HttpError> {
    Ok(Json(list_template_metas()))
}

/// POST /api/workspace-templates/{id}/create — 按模板建工作台。
/// 返回新工作台 id（与 create_workspace 同构，前端可直接切 Tab）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TemplateCreateResponse {
    pub ok: bool,
    pub id: String,
    pub name: String,
    /// 自测摘要（表行数/自动化条件可编译）。
    pub smoke: Vec<String>,
}

pub async fn create_from_template(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<Json<TemplateCreateResponse>, HttpError> {
    let tpl = get_template(&id).ok_or_else(|| HttpError {
        status: axum::http::StatusCode::NOT_FOUND,
        message: format!("模板[{id}]不存在（可选：personnel / vehicles / safety）"),
    })?;

    let root = hermix_config::paths::hermix_home().join("workspaces");
    let result = hermix_workspace::builder::create_from_schema(&root, tpl.schema, &Default::default())
        .map_err(|e| HttpError {
            status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("按模板建设工作台失败: {e}"),
        })?;

    // 注册进 registry（Tab 列表可见）
    let registry_path = root.join("registry.sqlite");
    let registry = hermix_workspace::RegistryStore::open(&registry_path).map_err(|e| HttpError {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("打开工作台注册表失败: {e}"),
    })?;
    registry
        .create(&result.id, tpl.name, tpl.icon, &result.id)
        .map_err(|e| HttpError {
            status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("注册工作台失败: {e}"),
        })?;

    Ok(Json(TemplateCreateResponse {
        ok: true,
        id: result.id,
        name: tpl.name.to_string(),
        smoke: result.smoke,
    }))
}
