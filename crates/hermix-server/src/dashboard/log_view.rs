//! Log file viewing API endpoints.
//!
//! `GET  /api/logs/files`      — list log files (optional ?category=server|agent|tools|crash)
//! `GET  /api/logs/read`       — read log content (?category=&filename=&offset=0&limit=200)
//! `GET  /api/logs/disk_usage` — total disk usage (?category=...)

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;

use crate::errors::HttpError;
use crate::HttpServerState;

// ---------------------------------------------------------------------------
// Query params
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct LogFilesParams {
    pub category: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LogReadParams {
    pub category: String,
    pub filename: String,
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct DiskUsageParams {
    pub category: Option<String>,
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
pub struct LogFilesResponse {
    pub files: Vec<hermix_telemetry::LogFileInfo>,
}

#[derive(Debug, serde::Serialize)]
pub struct LogReadResponse {
    pub lines: Vec<String>,
    pub total_lines: usize,
}

#[derive(Debug, serde::Serialize)]
pub struct DiskUsageResponse {
    pub bytes: u64,
    pub category: Option<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_err(msg: String) -> HttpError {
    HttpError { status: StatusCode::INTERNAL_SERVER_ERROR, message: msg }
}

fn not_found(msg: String) -> HttpError {
    HttpError { status: StatusCode::NOT_FOUND, message: msg }
}

fn cat_from_str(s: &str) -> Option<hermix_telemetry::LogCategory> {
    match s {
        "server" => Some(hermix_telemetry::LogCategory::Server),
        "agent" => Some(hermix_telemetry::LogCategory::Agent),
        "tools" => Some(hermix_telemetry::LogCategory::Tools),
        "crash" => Some(hermix_telemetry::LogCategory::Crash),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /api/logs/files
pub async fn list_log_files(
    State(_state): State<HttpServerState>,
    Query(params): Query<LogFilesParams>,
) -> Result<Json<LogFilesResponse>, HttpError> {
    let log_dir = hermix_telemetry::get_log_dir()
        .ok_or_else(|| not_found("log directory not initialized".into()))?;

    let cat = params.category.as_ref().and_then(|c| cat_from_str(c));

    let files = hermix_telemetry::list_log_files(log_dir, cat)
        .map_err(|e| make_err(format!("failed to list log files: {e}")))?;

    Ok(Json(LogFilesResponse { files }))
}

/// GET /api/logs/read
pub async fn read_log(
    State(_state): State<HttpServerState>,
    Query(params): Query<LogReadParams>,
) -> Result<Json<LogReadResponse>, HttpError> {
    let log_dir = hermix_telemetry::get_log_dir()
        .ok_or_else(|| not_found("log directory not initialized".into()))?;

    let limit = params.limit.unwrap_or(200).min(1000);
    let offset = params.offset.unwrap_or(0);

    let lines = hermix_telemetry::read_log_range(log_dir, &params.filename, offset, offset + limit)
        .map_err(|e| make_err(format!("failed to read log: {e}")))?;

    // Total lines for pagination metadata.
    let total_lines = hermix_telemetry::read_log_range(log_dir, &params.filename, 0, usize::MAX)
        .map(|l| l.len())
        .unwrap_or(0);

    Ok(Json(LogReadResponse { lines, total_lines }))
}

/// GET /api/logs/disk_usage
pub async fn disk_usage(
    State(_state): State<HttpServerState>,
    Query(params): Query<DiskUsageParams>,
) -> Result<Json<DiskUsageResponse>, HttpError> {
    let log_dir = hermix_telemetry::get_log_dir()
        .ok_or_else(|| not_found("log directory not initialized".into()))?;

    let cat = params.category.as_ref().and_then(|c| cat_from_str(c));

    let bytes = hermix_telemetry::log_disk_usage(log_dir, cat)
        .map_err(|e| make_err(format!("failed to compute disk usage: {e}")))?;

    Ok(Json(DiskUsageResponse {
        bytes,
        category: params.category,
    }))
}
