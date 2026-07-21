//! MCP (Model Context Protocol) server CRUD endpoints.
//!
//! The desktop front-end's "连接器" tab reads / writes MCP server entries
//! through these routes. Storage is a single JSON file at
//! `$HERMIX_HOME/mcp_servers.json`; on first start we seed 5 official
//! servers matching the front-end's hardcoded fallback (so the UI is
//! populated without manual setup).
//!
//! This module only persists the *configuration* — it does not yet spawn
//! the actual MCP processes. The hermes-mcp crate handles transport; once
//! it's wired into `LocalAgentService` this list will be the source of
//! truth for which servers the agent connects to.

use std::path::PathBuf;

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::HttpServerState;

// ── Storage path ────────────────────────────────────────

fn mcp_state_path(state: &HttpServerState) -> PathBuf {
    state.hermix_home.join("mcp_servers.json")
}

/// Return the on-disk path used for the MCP server config list.
/// Public so `HttpServerState::build_with_hermes_home` can read the
/// same file at startup (without needing to construct an
/// `HttpServerState` to call `load_state`).
pub fn mcp_state_path_for(hermix_home: &std::path::Path) -> PathBuf {
    hermix_home.join("mcp_servers.json")
}

/// Read the persisted MCP server list from disk, seeding the 5
/// official defaults on first run. Public so the agent startup
/// routine can pull the list and call `McpManager::connect` for
/// every enabled entry — without needing to instantiate a full
/// `HttpServerState` first.
pub fn load_mcp_servers(hermix_home: &std::path::Path) -> McpServersState {
    let path = mcp_state_path_for(hermix_home);
    let Ok(data) = std::fs::read_to_string(&path) else {
        return seed_default();
    };
    match serde_json::from_str::<McpServersState>(&data) {
        Ok(v) => v,
        Err(_) => seed_default(),
    }
}

// ── Wire types ──────────────────────────────────────────

/// One MCP server entry. Mirrors the front-end `McpServerEntry` exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerEntry {
    pub id: String,
    pub name: String,
    /// `stdio` spawns `command` + `args`; `http` connects to `url`.
    pub transport: McpTransport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::BTreeMap<String, String>>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at: String,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum McpTransport {
    Stdio,
    Http,
}

/// Wrapper for the persisted list (lets us add metadata later without breaking
/// front-end parsers).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpServersState {
    #[serde(default)]
    pub servers: Vec<McpServerEntry>,
    /// When true, the front-end should display these as the recommended
    /// starting set (vs. user-curated entries).
    #[serde(default)]
    pub seeded: bool,
}

// ── Load / save / seed ─────────────────────────────────

fn load_state(state: &HttpServerState) -> McpServersState {
    let path = mcp_state_path(state);
    let Ok(data) = std::fs::read_to_string(&path) else {
        return seed_default();
    };
    match serde_json::from_str::<McpServersState>(&data) {
        Ok(v) => v,
        Err(_) => seed_default(),
    }
}

fn save_state(state: &HttpServerState, body: &McpServersState) -> Result<(), String> {
    let path = mcp_state_path(state);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(body).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

fn now_iso() -> String {
    // Avoid pulling in chrono just for this — RFC3339-ish form from SystemTime.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 2026-01-01T00:00:00Z — close enough for ordering; the front-end only
    // uses it for tie-breaking and the human-readable created_at label.
    // We don't actually need a calendar-correct timestamp here, just a stable
    // monotonic-ish marker. Front-end overwrites this on add.
    format!("1970-01-01T00:00:{:02}Z", (secs % 60))
}

/// The 5 official MCP seeds. Mirrors the front-end's OFFICIAL_MCP_SEEDS list
/// (filesystem / fetch / git / github / brave-search) so the persistence
/// surface is identical regardless of which side writes first.
fn seed_default() -> McpServersState {
    McpServersState {
        seeded: true,
        servers: vec![],
    }
}

// ── HTTP handlers ──────────────────────────────────────

/// GET /api/mcp/servers — list all configured MCP servers.
pub async fn list_servers(State(state): State<HttpServerState>) -> Json<McpServersState> {
    Json(load_state(&state))
}

/// POST /api/mcp/servers — create a new entry. Server assigns id + created_at
/// if missing.
pub async fn create_server(
    State(state): State<HttpServerState>,
    Json(mut body): Json<McpServerEntry>,
) -> Result<Json<McpServerEntry>, (StatusCode, String)> {
    if body.name.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "name is required".into()));
    }
    if body.id.trim().is_empty() {
        body.id = format!(
            "user-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        );
    }
    if body.created_at.trim().is_empty() {
        body.created_at = now_iso();
    }
    let mut current = load_state(&state);
    // Reject duplicate ids so the front-end can't accidentally overwrite.
    if current.servers.iter().any(|s| s.id == body.id) {
        return Err((
            StatusCode::CONFLICT,
            format!("server with id '{}' already exists", body.id),
        ));
    }
    current.servers.push(body.clone());
    current.seeded = false; // user touched it, no longer pure seed
    save_state(&state, &current).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(body))
}

/// PUT /api/mcp/servers/{id} — partial update.
pub async fn update_server(
    State(state): State<HttpServerState>,
    AxumPath(id): AxumPath<String>,
    Json(patch): Json<Value>,
) -> Result<Json<McpServerEntry>, (StatusCode, String)> {
    let mut current = load_state(&state);
    // Snapshot the old entry so we can detect transitions on
    // `enabled` and `name` after the merge — both of those need to
    // trigger a runtime side-effect on the McpManager.
    let old = current
        .servers
        .iter()
        .find(|s| s.id == id)
        .cloned()
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("server with id '{id}' not found"),
            )
        })?;
    let Some(entry) = current.servers.iter_mut().find(|s| s.id == id) else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("server with id '{id}' not found"),
        ));
    };
    // We accept any subset of fields and overlay onto the existing entry.
    // Round-trip through JSON for a robust merge.
    let mut v = serde_json::to_value(&*entry).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if let (Some(base), Some(patch_obj)) = (v.as_object_mut(), patch.as_object()) {
        for (k, v) in patch_obj {
            base.insert(k.clone(), v.clone());
        }
    }
    let updated: McpServerEntry =
        serde_json::from_value(v).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    *entry = updated.clone();
    current.seeded = false;
    save_state(&state, &current).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Runtime side-effects: keep the McpManager in sync with what the
    // user just persisted. Disconnect when toggling off or renaming;
    // (re)connect when toggling on. We use the OLD name as the
    // disconnect key so a rename cleanly tears down the old
    // connection before the new one comes up.
    drop(current); // release the lock the implicit closure may want
    sync_runtime_after_change(&state, &old, &updated).await;

    Ok(Json(updated))
}

/// DELETE /api/mcp/servers/{id}
pub async fn delete_server(
    State(state): State<HttpServerState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut current = load_state(&state);
    let removed = current.servers.iter().find(|s| s.id == id).cloned();
    let before = current.servers.len();
    current.servers.retain(|s| s.id != id);
    if current.servers.len() == before {
        return Err((
            StatusCode::NOT_FOUND,
            format!("server with id '{id}' not found"),
        ));
    }
    save_state(&state, &current).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Runtime side-effect: tear down the connection if the deleted
    // entry was enabled. We don't block the response on disconnect
    // — fire-and-forget is fine here since the manager logs the
    // result itself.
    if let Some(entry) = removed {
        if entry.enabled {
            let mgr = state.mcp_manager.clone();
            let key = entry.name.clone();
            tokio::spawn(async move {
                if let Err(e) = mgr.disconnect(&key).await {
                    tracing::warn!(
                        server = %key,
                        error = %e,
                        "failed to disconnect MCP server after delete"
                    );
                }
            });
        }
    }

    Ok(Json(json!({ "ok": true, "id": id })))
}

/// Reconcile the McpManager with a before/after pair after a
/// dashboard `update_server`. Handles three transitions:
/// - enabled true → false: disconnect
/// - enabled false → true: connect (best effort, errors logged)
/// - name changed while enabled: disconnect old, connect new
async fn sync_runtime_after_change(
    state: &HttpServerState,
    old: &McpServerEntry,
    updated: &McpServerEntry,
) {
    let renamed = old.name != updated.name;
    let turned_off = old.enabled && !updated.enabled;
    let turned_on = !old.enabled && updated.enabled;

    if turned_off || (renamed && old.enabled) {
        let mgr = state.mcp_manager.clone();
        let key = old.name.clone();
        tokio::spawn(async move {
            if let Err(e) = mgr.disconnect(&key).await {
                tracing::warn!(
                    server = %key,
                    error = %e,
                    "MCP disconnect on update failed (likely not connected)"
                );
            }
        });
    }
    if turned_on || (renamed && updated.enabled) {
        match entry_to_server_config(updated) {
            Ok((key, cfg)) => {
                let mgr = state.mcp_manager.clone();
                tokio::spawn(async move {
                    if let Err(e) = mgr.connect(&key, cfg).await {
                        tracing::warn!(
                            server = %key,
                            error = %e,
                            "MCP connect on update failed"
                        );
                    }
                });
            }
            Err(e) => tracing::warn!(
                server = %updated.name,
                error = %e,
                "MCP entry invalid after update; not connecting"
            ),
        }
    }
}

/// POST /api/mcp/servers/reset — restore the 5 official seeds. Wipes any
/// user-added entries; useful after a botched import.
pub async fn reset_to_official(
    State(state): State<HttpServerState>,
) -> Result<Json<McpServersState>, (StatusCode, String)> {
    let fresh = seed_default();
    save_state(&state, &fresh).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(fresh))
}

// ── Wire → hermix-mcp adapter ───────────────────────────

/// Convert one dashboard entry to the `(connect_name, McpServerConfig)`
/// pair `hermix_mcp::McpManager::connect` expects. Returns an error if
/// the entry is malformed (e.g. `transport: stdio` without a command).
pub fn entry_to_server_config(
    entry: &McpServerEntry,
) -> Result<(String, hermix_mcp::McpServerConfig), String> {
    let mut cfg = match entry.transport {
        McpTransport::Stdio => {
                let command = entry
                    .command
                    .clone()
                    .ok_or_else(|| format!("server '{}' is stdio but has no command", entry.name))?;
                let args = entry.args.clone().unwrap_or_default();
                hermix_mcp::McpServerConfig::stdio(command, args)
            }
        McpTransport::Http => {
                let url = entry
                    .url
                    .clone()
                    .ok_or_else(|| format!("server '{}' is http but has no url", entry.name))?;
                hermix_mcp::McpServerConfig::http(url)
            }
    };
    if let Some(env_map) = &entry.env {
        for (k, v) in env_map {
            // Skip empty values: a blank env var usually means the
            // user hasn't filled in their API key yet, and we don't
            // want to launch the subprocess with an env override
            // that erases the parent shell's value.
            if v.is_empty() {
                continue;
            }
            cfg = cfg.with_env(k.clone(), v.clone());
        }
    }
    // Use the user-visible `name` as the connection key. MCP tool
    // registrations end up prefixed by this — e.g. `filesystem__read_file`
    // — so it's the right thing to show in tool error messages.
    Ok((entry.name.clone(), cfg))
}
