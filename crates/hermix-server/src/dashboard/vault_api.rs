//! Vault (凭据保险库) API 端点 — dashboard 面整体仅本机可达。
//!
//! 决策笔记：docs/notes/implemented/feature/2026-10-09-vault-credentials.md
//! 密码只在两个响应里出明文：`reveal`（UI 点击显示）、编辑回填（PUT 不返回
//! 明文）。列表响应**永不含密码**（hermix-vault::Entry 的结构保证，secret
//! 字段 skip_serializing_if）。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;

use crate::{HttpError, HttpServerState};

use hermix_vault::{Vault, VaultError};

/// 进程级单例（低频操作，Mutex 串行足够）。
fn vault() -> Result<&'static Vault, HttpError> {
    static VAULT: std::sync::OnceLock<Result<Vault, String>> = std::sync::OnceLock::new();
    let init = VAULT.get_or_init(|| Vault::open().map_err(|e| format!("vault 打开失败: {e}")));
    match init {
        Ok(v) => Ok(v),
        Err(e) => Err(HttpError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("vault 不可用: {e}"),
        }),
    }
}

/// VaultError → HTTP 状态映射：重名 409 / 不存在 404 / 其余 500。
fn vault_err(e: VaultError) -> HttpError {
    let status = match &e {
        VaultError::Duplicate(_) => StatusCode::CONFLICT,
        VaultError::NotFound(_) => StatusCode::NOT_FOUND,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    HttpError {
        status,
        message: e.to_string(),
    }
}

/// GET /api/vault/entries — 列表（永不含密码）。
pub async fn list_entries() -> Result<Json<serde_json::Value>, HttpError> {
    let v = vault()?;
    let entries = v.list().map_err(vault_err)?;
    Ok(Json(serde_json::json!({ "entries": entries })))
}

#[derive(Deserialize)]
pub struct UpsertBody {
    pub name: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub notes: String,
}

/// POST /api/vault/entries — 新增。
pub async fn create_entry(
    State(_s): State<HttpServerState>,
    Json(body): Json<UpsertBody>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let v = vault()?;
    let entry = v
        .add(
            &body.name,
            &body.url,
            &body.username,
            body.secret.as_deref().unwrap_or(""),
            &body.notes,
        )
        .map_err(vault_err)?;
    Ok(Json(serde_json::json!({ "entry": entry })))
}

/// PUT /api/vault/entries/{id} — 更新（secret 缺省 = 不改密码）。
pub async fn update_entry(
    State(_s): State<HttpServerState>,
    Path(id): Path<String>,
    Json(body): Json<UpsertBody>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let v = vault()?;
    let entry = v
        .update(
            &id,
            &body.name,
            &body.url,
            &body.username,
            body.secret.as_deref(),
            &body.notes,
        )
        .map_err(vault_err)?;
    Ok(Json(serde_json::json!({ "entry": entry })))
}

/// DELETE /api/vault/entries/{id}。
pub async fn delete_entry(
    State(_s): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let v = vault()?;
    v.delete(&id).map_err(vault_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// POST /api/vault/entries/{id}/reveal — UI 点击显示密码。
pub async fn reveal_entry(
    State(_s): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let v = vault()?;
    let entry = v.get(&id).map_err(vault_err)?;
    Ok(Json(serde_json::json!({ "secret": entry.secret })))
}
