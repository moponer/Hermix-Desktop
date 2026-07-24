//! Custom apps API — REST endpoints for managing user-facing desktop applications.
//!
//! Routes:
//!   GET    /api/custom-apps              — list all apps
//!   POST   /api/custom-apps              — create a new app
//!   GET    /api/custom-apps/{id}         — get app details
//!   PUT    /api/custom-apps/{id}         — update app code
//!   DELETE /api/custom-apps/{id}         — delete an app
//!   POST   /api/custom-apps/{id}/package — package app as standalone EXE
//!   POST   /api/custom-apps/{id}/run     — launch a packaged app
//!   GET    /api/custom-apps/{id}/icon    — get app custom icon (PNG)

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use crate::HttpServerState;
use hermix_plugin::api::{AppResponse, AppSummary, CreateAppRequest, UpdateAppRequest};

/// Get the PluginManager from state, or return a 503 error.
fn get_manager(state: &HttpServerState) -> Result<&Arc<hermix_plugin::PluginManager>, (StatusCode, Json<Value>)> {
    state.custom_app_manager.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "自定义应用系统未初始化",
                "code": "CUSTOM_APPS_NOT_INITIALIZED"
            })),
        )
    })
}

/// GET /api/custom-apps — list all custom apps.
pub async fn list_apps(
    State(state): State<HttpServerState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let manager = get_manager(&state)?;

    manager
        .scan_plugins()
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("扫描应用失败: {e}") })),
            )
        })?;

    let entries: Vec<AppSummary> = manager
        .list_plugins()
        .iter()
        .filter_map(|m| manager.get_plugin(&m.id))
        .map(|e| AppSummary::from(&e))
        .collect();

    Ok(Json(json!({
        "success": true,
        "apps": entries,
        "count": entries.len(),
    })))
}

/// POST /api/custom-apps — create a new custom app.
pub async fn create_app(
    State(state): State<HttpServerState>,
    Json(req): Json<CreateAppRequest>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let manager = get_manager(&state)?;

    req.manifest
        .validate()
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("manifest 无效: {e}") })),
            )
        })?;

    let icon_data = None; // icon_data is passed via LLM tool, not REST API
    let entry = manager
        .install_plugin(&req.manifest, &req.backend_code, req.ui_code.as_deref(), icon_data)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("创建应用失败: {e}") })),
            )
        })?;

    let response: AppResponse = (&entry).into();
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "success": true,
            "message": format!("应用 '{}' 创建成功", entry.manifest.name),
            "app": response,
        })),
    ))
}

/// GET /api/custom-apps/{id} — get app details.
pub async fn get_app(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let manager = get_manager(&state)?;

    let entry = manager.get_plugin(&id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("应用不存在: {id}") })),
        )
    })?;

    let response: AppResponse = (&entry).into();
    Ok(Json(json!({
        "success": true,
        "app": response,
    })))
}

/// PUT /api/custom-apps/{id} — update app code.
pub async fn update_app(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateAppRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let manager = get_manager(&state)?;

    let entry = manager
        .update_plugin(&id, req.backend_code.as_deref(), req.ui_code.as_deref())
        .map_err(|e| {
            let status = match &e {
                hermix_plugin::PluginError::NotFound(_) => StatusCode::NOT_FOUND,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, Json(json!({ "error": format!("更新应用失败: {e}") })))
        })?;

    let response: AppResponse = (&entry).into();
    Ok(Json(json!({
        "success": true,
        "message": format!("应用 '{}' 更新成功", entry.manifest.name),
        "app": response,
    })))
}

/// DELETE /api/custom-apps/{id} — delete an app.
pub async fn delete_app(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let manager = get_manager(&state)?;

    manager
        .uninstall_plugin(&id)
        .map_err(|e| {
            let status = match &e {
                hermix_plugin::PluginError::NotFound(_) => StatusCode::NOT_FOUND,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, Json(json!({ "error": format!("删除应用失败: {e}") })))
        })?;

    Ok(Json(json!({
        "success": true,
        "message": format!("应用 '{id}' 已删除"),
    })))
}

/// POST /api/custom-apps/{id}/package — package app as standalone EXE.
pub async fn package_app(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let manager = get_manager(&state)?;

    let exe_path = manager
        .package_plugin(&id)
        .await
        .map_err(|e| {
            let status = match &e {
                hermix_plugin::PluginError::NotFound(_) => StatusCode::NOT_FOUND,
                hermix_plugin::PluginError::EsbuildNotFound(_) => {
                    StatusCode::SERVICE_UNAVAILABLE
                }
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, Json(json!({ "error": format!("打包应用失败: {e}") })))
        })?;

    Ok(Json(json!({
        "success": true,
        "message": format!("应用 '{id}' 打包成功"),
        "exe_path": exe_path.to_string_lossy(),
        "output_dir": exe_path.parent().map(|p| p.to_string_lossy().to_string()),
    })))
}

/// POST /api/custom-apps/{id}/run — launch a packaged app.
pub async fn run_app(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let manager = get_manager(&state)?;

    let entry = manager.get_plugin(&id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("应用不存在: {id}") })),
        )
    })?;

    let wm = hermix_plugin::PluginWindowManager::new(manager.shell_path().to_path_buf());
    wm.open_plugin(&entry)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("启动应用失败: {e}") })),
            )
        })?;

    Ok(Json(json!({
        "success": true,
        "message": format!("应用 '{id}' 已启动"),
    })))
}

/// GET /api/custom-apps/{id}/icon — serve the app's custom icon as PNG.
/// /// Returns 404 if the app has no custom icon.
pub async fn get_app_icon(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<(StatusCode, axum::http::HeaderMap, Vec<u8>), (StatusCode, Json<Value>)> {
    let manager = get_manager(&state)?;

    let entry = manager.get_plugin(&id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("应用不存在: {id}") })),
        )
    })?;

    let icon_path = entry.icon_path.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "此应用没有自定义图标" })),
        )
    })?;

    let bytes = std::fs::read(&icon_path).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("读取图标失败: {e}") })),
        )
    })?;

    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        "image/png".parse().unwrap(),
    );
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        "public, max-age=3600".parse().unwrap(),
    );

    Ok((StatusCode::OK, headers, bytes))
}

#[cfg(test)]
mod wiring_tests {
    use super::*;

    /// Wiring sanity check (2026-07-21): after `HttpServerState::build`,
    /// `custom_app_manager` must be `Some(...)`, otherwise the
    /// `get_manager` helper here would return 503 for every
    /// `/api/custom-apps` request. We exercise the same code path that
    /// production takes — `build_with_hermes_home` — but skip the agent
    /// service wiring (which needs an LLM provider) by going through
    /// `build_with_agent_service` with a no-op agent instead.
    #[tokio::test]
    async fn build_with_hermes_home_initializes_custom_app_manager() {
        // Use a tempdir hermix_home so we don't touch the user's real
        // config / database files.
        let tmp = tempfile::tempdir().expect("tempdir");
        let hermix_home = tmp.path().to_path_buf();

        // Minimal gateway config — model=None is fine because
        // auto_correct_model() falls back when no API keys are present
        // (it'll try to read env vars, but won't crash if they aren't
        // set; the agent service init may still fail downstream if
        // it's needed, but build_with_hermes_home only constructs
        // the custom_app_manager before any provider call).
        let config = hermix_config::GatewayConfig::default();

        let state = crate::HttpServerState::build_with_hermes_home(config, hermix_home)
            .await
            .expect("build_with_hermes_home");

        assert!(
            state.custom_app_manager.is_some(),
            "build_with_hermes_home left custom_app_manager = None — \
             /api/custom-apps will return 503 (CUSTOM_APPS_NOT_INITIALIZED)"
        );
    }
}
