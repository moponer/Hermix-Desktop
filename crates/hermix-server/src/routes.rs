//! HTTP REST handlers registered by `router`.


use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};

use serde::{Deserialize, Serialize};


use crate::errors::HttpError;

use super::*;

#[derive(Serialize)]
pub(crate) struct ExpertListResponse {
    experts: Vec<hermix_skills::ExpertEntry>,
}

pub(crate) async fn list_experts(State(state): State<HttpServerState>) -> Json<ExpertListResponse> {
    Json(ExpertListResponse {
        experts: state.expert_registry.list(),
    })
}

// ---------------------------------------------------------------------------
// TP1: dynamic tool discovery
// ---------------------------------------------------------------------------
// Returns every tool currently registered in the `ToolRegistry` so the
// desktop client can decide what to expose to the LLM without any
// hard-coded tool names on the front-end. Adding a tool to the registry
// (e.g. `tool_registry.register(...)` at startup) makes it appear here
// automatically.

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolDescriptor {
    name: String,
    toolset: String,
    description: String,
    emoji: String,
    is_async: bool,
    is_enabled: bool,
    env_deps: Vec<String>,
    max_result_size_chars: Option<usize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolListResponse {
    tools: Vec<ToolDescriptor>,
}

pub(crate) async fn list_available_tools(
    State(state): State<HttpServerState>,
) -> Json<ToolListResponse> {
    let tools = state
        .tool_registry
        .list_tools()
        .into_iter()
        .map(|t| ToolDescriptor {
            name: t.name,
            toolset: t.toolset,
            description: t.description,
            emoji: t.emoji,
            is_async: t.is_async,
            is_enabled: t.is_enabled,
            env_deps: t.env_deps,
            max_result_size_chars: t.max_result_size_chars,
        })
        .collect();
    Json(ToolListResponse { tools })
}

#[derive(Serialize)]
pub(crate) struct ExpertResponse {
    expert: hermix_skills::ExpertEntry,
}

pub(crate) async fn get_expert(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<Json<ExpertResponse>, (StatusCode, String)> {
    match state.expert_registry.get(&id) {
        Some(expert) => Ok(Json(ExpertResponse { expert })),
        None => Err((
            StatusCode::NOT_FOUND,
            format!("expert '{id}' is not registered"),
        )),
    }
}

#[derive(Deserialize)]
pub(crate) struct RegisterExpertRequest {
    experts: Vec<hermix_skills::ExpertEntry>,
}

#[derive(Serialize)]
pub(crate) struct RegisterExpertResponse {
    inserted: usize,
    total: usize,
}

pub(crate) async fn register_expert(
    State(state): State<HttpServerState>,
    Json(req): Json<RegisterExpertRequest>,
) -> Json<RegisterExpertResponse> {
    let inserted = state.expert_registry.register_many(req.experts);
    let total = state.expert_registry.len();
    Json(RegisterExpertResponse { inserted, total })
}

pub(crate) async fn metrics_prometheus() -> impl IntoResponse {
(
[(
header::CONTENT_TYPE,
"text/plain; version=0.0.4; charset=utf-8",
)],
hermix_telemetry::prometheus_text(),
)
}

/// ( / ) JSON snapshot of local metrics
/// for dashboard display. Returns counters, latency percentiles,
/// and buffer status.
pub(crate) async fn metrics_snapshot(
    State(state): State<HttpServerState>,
) -> Json<crate::metrics::MetricsSnapshot> {
    Json(state.metrics.snapshot())
}



pub(crate) async fn protocol_schema() -> impl IntoResponse {
    Json(serde_json::json!({
        "version": 1,
        "ws_envelope": hermix_transport::ws_schema_json()
    }))
}

pub(crate) async fn client_telemetry_event(
    Json(event): Json<ClientTelemetryEvent>,
) -> Result<Json<serde_json::Value>, HttpError> {
    match event.level.as_str() {
        "error" => tracing::error!(
            runtime = %event.runtime,
            message = %event.message,
            app_version = ?event.app_version,
            trace_id = ?event.trace_id,
            tags = ?event.tags,
            "client telemetry event"
        ),
        "warn" => tracing::warn!(
            runtime = %event.runtime,
            message = %event.message,
            app_version = ?event.app_version,
            trace_id = ?event.trace_id,
            tags = ?event.tags,
            "client telemetry event"
        ),
        _ => tracing::info!(
            runtime = %event.runtime,
            message = %event.message,
            app_version = ?event.app_version,
            trace_id = ?event.trace_id,
            tags = ?event.tags,
            "client telemetry event"
        ),
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub(crate) async fn list_sessions(State(state): State<HttpServerState>) -> Json<ListSessionsResponse> {
    let db_path = hermix_config::paths::data_dir_in(&state.hermix_home).join("sessions.db");
    let conn = match open_sessions_db_for_read(&db_path) {
        Ok(conn) => conn,
        Err(_) => {
            return Json(ListSessionsResponse {
                sessions: vec![],
                total: 0,
            });
        }
    };
    let mut stmt = match conn.prepare(
        "SELECT id, title, created_at, updated_at, message_count, model, workspace, expert_id \
         FROM sessions WHERE id NOT LIKE 'groupchat-%' ORDER BY updated_at DESC",
    ) {
        Ok(stmt) => stmt,
        Err(_) => {
            return Json(ListSessionsResponse {
                sessions: vec![],
                total: 0,
            });
        }
    };
    let sessions: Vec<SessionSummary> = stmt
        .query_map([], |row| {
            Ok(SessionSummary {
                id: row.get::<_, String>(0)?,
                title: row
                    .get::<_, Option<String>>(1)?
                    .unwrap_or_else(|| "New chat".to_string()),
                created_at: row.get::<_, String>(2)?,
                updated_at: row.get::<_, String>(3)?,
                message_count: row.get::<_, i64>(4).unwrap_or(0) as usize,
                model: row.get::<_, Option<String>>(5).ok().flatten(),
                workspace: row.get::<_, Option<String>>(6).ok().flatten(),
                expert_id: row.get::<_, Option<String>>(7).ok().flatten(),
            })
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default();
    Json(ListSessionsResponse {
        total: sessions.len(),
        sessions,
    })
}

pub(crate) async fn create_session(
    State(state): State<HttpServerState>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<Json<SessionSummary>, HttpError> {
    let session_id = uuid::Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    let title = if req.title.trim().is_empty() {
        "New chat".to_string()
    } else {
        req.title
    };
    state
        .session_persistence
        .persist_session(
            &session_id,
            &[] as &[Message],
            state.config.model.as_deref(),
            Some(HTTP_PLATFORM),
            Some(&title),
            None,
            None,
            // Bind workspace at creation time so a session reload
            // (after desktop restart) restores the workspace category
            // the user picked in the welcome UI. Without this the
            // welcome-state draft workspace only ever lived in
            // `hermix.session.<id>.workspace` localStorage and
            // vanished on restart — Sidebar bucketed the row under
            //"未分组" (bug ). `req.project` is the legacy
            // field name carried over from the createSession wire
            // payload — the desktop client sends the welcome
            // draftWorkspace here on first message.
            req.project.as_deref(),
            None,
        )
        .map_err(HttpError::from)?;
    Ok(Json(SessionSummary {
        id: session_id,
        title,
        created_at: now.clone(),
        updated_at: now,
        message_count: 0,
        model: state.config.model.as_deref().map(|s| s.to_string()),
        workspace: req.project.clone(),
        expert_id: None,
    }))
}

pub(crate) async fn rename_session(
    Path(session_id): Path<String>,
    State(state): State<HttpServerState>,
    Json(req): Json<RenameSessionRequest>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let messages = state
        .session_persistence
        .load_session(&session_id)
        .map_err(HttpError::from)?;
    state
        .session_persistence
        .persist_session(
            &session_id,
                    &messages,
                    // model: None — preserve the session's existing model
                    // binding (a per-session override or the global default).
                    // Passing `config.model` here would clobber a per-session
                    // override on every rename, a regression once the
                    // ON CONFLICT branch in `save_session_atomic` updates
                    // `model` (COALESCE keeps the stored value when we pass
                    // None).
                    None,
            Some(HTTP_PLATFORM),
            Some(req.title.trim()),
            None,
            None,
            // Rename must not clobber the workspace/expert binding the
            // user picked earlier — pass None so save_session_atomic
            // COALESCE leaves the existing columns alone.
            None,
            None,
        )
        .map_err(HttpError::from)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// PATCH /v1/sessions/{id}
/// Update session metadata (workspace, expert_id, model).
/// Fields missing from the request body are left unchanged.
pub(crate) async fn patch_session(
    Path(session_id): Path<String>,
    State(state): State<HttpServerState>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let db_path = hermix_config::paths::data_dir_in(&state.hermix_home).join("sessions.db");
    let conn = open_sessions_db_for_read(&db_path).map_err(|e| HttpError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: e.to_string(),
    })?;
    let sid = session_id;
    // Build SET clauses dynamically based on what's in the request.
    // A key present with `null` means "clear this field".
    if let Some(v) = req.get("workspace") {
        if v.is_null() {
            conn.execute("UPDATE sessions SET workspace = NULL WHERE id = ?1", [sid.clone()])
                .map_err(|e| HttpError { status: StatusCode::INTERNAL_SERVER_ERROR, message: e.to_string() })?;
        } else if let Some(s) = v.as_str() {
            conn.execute("UPDATE sessions SET workspace = ?1 WHERE id = ?2", rusqlite::params![s, sid.clone()])
                .map_err(|e| HttpError { status: StatusCode::INTERNAL_SERVER_ERROR, message: e.to_string() })?;
        }
    }
    if let Some(v) = req.get("expert_id") {
        if v.is_null() {
            conn.execute("UPDATE sessions SET expert_id = NULL WHERE id = ?1", [sid.clone()])
                .map_err(|e| HttpError { status: StatusCode::INTERNAL_SERVER_ERROR, message: e.to_string() })?;
        } else if let Some(s) = v.as_str() {
            conn.execute("UPDATE sessions SET expert_id = ?1 WHERE id = ?2", rusqlite::params![s, sid.clone()])
                .map_err(|e| HttpError { status: StatusCode::INTERNAL_SERVER_ERROR, message: e.to_string() })?;
        }
    }
    if let Some(v) = req.get("model") {
        if v.is_null() {
            conn.execute("UPDATE sessions SET model = NULL WHERE id = ?1", [sid.clone()])
                .map_err(|e| HttpError { status: StatusCode::INTERNAL_SERVER_ERROR, message: e.to_string() })?;
        } else if let Some(s) = v.as_str() {
            conn.execute("UPDATE sessions SET model = ?1 WHERE id = ?2", rusqlite::params![s, sid.clone()])
                .map_err(|e| HttpError { status: StatusCode::INTERNAL_SERVER_ERROR, message: e.to_string() })?;
        }
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Todo (task list) HTTP endpoints
// ---------------------------------------------------------------------------
// The `todo` tool is the canonical mutation path (LLM-driven). These
// endpoints exist so the front-end can (a) hydrate the TodoPanel on
// initial mount via `GET /v1/todos`, and (b) let the user manually
// flip a task to `done` via `PATCH /v1/todos/{id}`. Both endpoints
// dispatch through the same `TodoHandler` so the broadcast path
// (`todo_event_tx`) fires on every change, keeping the WS stream and
// the HTTP loop consistent.
// Why not call FileTodoBackend directly? Two reasons:
// * The broadcast fires only when TodoHandler.execute() runs.
// Bypassing the handler would skip the WS forwarder and leave
// the desktop panel stale.
// * It keeps the schema (parameter names, status enums) in one
// place — the LLM tool definition.
// Trade-off: each `GET` re-enters `ToolHandler::execute`, which
// parses the LLM-style action envelope. For N items this is O(N)
// JSON parse, which is fine (todo lists are <100 items).

pub(crate) async fn list_todos(
    State(state): State<HttpServerState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, HttpError> {
    // Commit D — accept optional `?session_id=...` so the TodoPanel
    // doesn't fetch every session's tasks when a single tab is open.
    // Without `session_id` we fall back to the legacy "list all"
    // behaviour so the dashboard's own panels keep working.
    let session_id = params.get("session_id").cloned();
    let raw = match session_id {
        Some(sid) => state
            .tool_registry
            .dispatch(
                "todo",
                serde_json::json!({ "action": "list", "session_id": sid }),
            ),
        None => state
            .tool_registry
            .dispatch("todo", serde_json::json!({ "action": "list" })),
    };
    // dispatch() returns the JSON envelope as a String. The todo
    // tool's list backend returns `{"todos":[...],"total":N}` so
    // forward it as-is; if it's not valid JSON we surface the raw
    // string under a `raw` key for debugging.
    let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|e| HttpError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("todo backend returned non-JSON: {e}: {raw}"),
    })?;
    Ok(Json(parsed))
}

pub(crate) async fn patch_todo(
    Path(id): Path<String>,
    State(state): State<HttpServerState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, HttpError> {
    // The TodoPanel only ever flips a task to `completed` /
    // `pending`; everything else (title / priority / dependencies)
    // is LLM-driven and goes through the tool directly. Accept
    // any of the canonical fields and forward verbatim so we don't
    // surprise the user when their clicks somehow round-trip a
    // back-end update.
    let mut params_obj = serde_json::json!({
        "action": "update",
        "id": id,
    });
    if let Some(s) = req.get("status").and_then(|v| v.as_str()) {
        params_obj["status"] = serde_json::Value::String(s.to_string());
    }
    if let Some(t) = req.get("title").and_then(|v| v.as_str()) {
        params_obj["title"] = serde_json::Value::String(t.to_string());
    }
    if let Some(p) = req.get("priority").and_then(|v| v.as_str()) {
        params_obj["priority"] = serde_json::Value::String(p.to_string());
    }

    // Commit D — `?session_id=...` so the TodoPanel's checkbox toggle
    // can be attributed to the right chat. Without it the row would
    // be persisted with `session_id = None` (orphan) and the WS
    // snapshot would not include it on the next reconnect.
    // We don't restore the previous session after dispatch: the
    // TodoItem has already been written with the correct session_id
    // (Commit A) and a concurrent agent-loop turn will re-stamp its
    // own session before the next mutation. The registry's stamper
    // is a "last writer wins" cell by design.
    let session_id = params.get("session_id").cloned();
    let _ = state
        .tool_registry
        .set_active_session_for_tool("todo", session_id);

    let raw = state.tool_registry.dispatch("todo", params_obj);

    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|_| {
        serde_json::json!({ "ok": true, "raw": raw })
    });
    Ok(Json(parsed))
}

pub(crate) async fn delete_todo(
    Path(id): Path<String>,
    State(state): State<HttpServerState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, HttpError> {
    // Dispatches through `TodoHandler::execute` so the broadcast path
    // (`todo_event_tx`) fires and the WS forwarder drops the row from
    // every connected TodoPanel — keeping the HTTP loop and the WS
    // stream consistent (same rationale as `patch_todo`).
    // Commit D — same `?session_id=...` rationale as `patch_todo`:
    // thread the session through to the persisted TodoItem so the
    // delete doesn't leave an orphan row in `todos.json`.
    let session_id = params.get("session_id").cloned();
    let _ = state
        .tool_registry
        .set_active_session_for_tool("todo", session_id);

    let raw = state.tool_registry.dispatch(
        "todo",
        serde_json::json!({ "action": "delete", "id": id }),
    );

    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|_| {
        serde_json::json!({ "ok": true, "raw": raw })
    });
    Ok(Json(parsed))
}

/// Open `sessions.db` for **read-only-ish** server-side access.
/// Every server route that touches `sessions.db` MUST go through this
/// helper rather than calling `Connection::open` directly. Without it,
/// `journal_mode = WAL` only applies to the writer side
/// (`session_persistence::open_with_wal`), and the server opener
/// inherits the legacy `journal_mode = delete` default. That mismatch
/// surfaces `SQLITE_CORRUPT` / "database disk image is malformed" the
/// moment the writer was killed mid-transaction and left a stale
/// `-shm` page table — which happens on every Tauri sidecar SIGKILL
/// shutdown.
/// Best-effort on PRAGMA failures: if WAL mode is rejected (read-only
/// file, exclusive lock held, etc.) we log a warning and continue —
/// Detect the canonical SQLite "database disk image is malformed"
/// diagnostic. extension : the original WAL fix
/// closed the `journal_mode` mismatch, but FTS5 virtual tables can
/// also surface this error when their shadow pages are corrupted.
/// A rebuild of `messages_fts` + `messages_fts_trigram` resolves it,
/// so we treat the error as recoverable rather than fatal.
pub(crate) fn is_malformed_error(err: &rusqlite::Error) -> bool {
    let msg = err.to_string();
    msg.contains("database disk image is malformed")
        || msg.contains("database disk image")
        || msg.contains("malformed")
}

/// Rebuild both FTS5 indexes attached to `messages`. Idempotent —
/// safe to call from a retry path. If the rebuild itself fails the
/// caller should give up and return 500 to the user rather than
/// silently swallow.
/// Both indexes are present from migration 2 onward (the schema
/// migration creates `messages_fts` for CJK bigram search and
/// `messages_fts_trigram` for ASCII substring). On older DBs where
/// trigram is missing, we treat the rebuild as best-effort and
/// continue — failing to rebuild an absent table is not a blocker.
pub(crate) fn rebuild_messages_fts(conn: &rusqlite::Connection) -> Result<(), String> {
    conn.execute_batch("INSERT INTO messages_fts(messages_fts) VALUES('rebuild');")
        .map_err(|e| format!("messages_fts rebuild failed: {e}"))?;
    // trigram may not exist on pre-migration-2 DBs — try and warn,
    // don't fail the whole delete for a missing optional index.
    if let Err(e) = conn.execute_batch(
        "INSERT INTO messages_fts_trigram(messages_fts_trigram) VALUES('rebuild');",
    ) {
        tracing::warn!(
            "rebuild_messages_fts: trigram rebuild skipped ({e});              the trigram index will be lazy-rebuilt on next INSERT"
        );
    }
    Ok(())
}

/// `busy_timeout` alone still resolves the silent-drop / lock-contention
/// case.
pub(crate) fn open_sessions_db_for_read(path: &std::path::Path) -> Result<rusqlite::Connection, String> {
    let conn = rusqlite::Connection::open(path).map_err(|e| e.to_string())?;
    // 500ms (was 5000): a DELETE in flight holds the WAL writer lock for
    // 1-3s on a multi-MB sessions.db; with the old 5s window, the very
    // next SELECT (sidebar click, session switch) hit busy_timeout and
    // stalled the front-end for seconds. 500ms lets the lock resolve
    // fast in normal cases and bounds worst-case latency to half a
    // second — the SELECT then retries on its own.
    conn.busy_timeout(std::time::Duration::from_millis(500))
        .map_err(|e| e.to_string())?;
    if let Err(e) = conn.execute_batch("PRAGMA journal_mode = WAL;") {
        tracing::warn!(
            "open_sessions_db_for_read: WAL PRAGMA failed ({e}); \
             continuing with default journaling"
        );
    }
    // Passive checkpoint so we don't read the WAL header from a page
    // the writer hasn't flushed yet. PASSIVE never blocks the writer.
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
    Ok(conn)
}

pub(crate) async fn delete_session(
    Path(session_id): Path<String>,
    State(state): State<HttpServerState>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let db_path = hermix_config::paths::data_dir_in(&state.hermix_home).join("sessions.db");
    let conn = open_sessions_db_for_read(&db_path).map_err(|e| HttpError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: e.to_string(),
    })?;

    //extension : try the cascade-delete; if it
    // fails with "database disk image is malformed" — typically a
    // corrupted `messages_fts` shadow page — rebuild both FTS5
    // indexes and retry once. If the rebuild itself fails or the
    // retry still errors, fall through to the original 500 surface
    // so the user isn't silently told the delete succeeded.
    // No labelled block needed: the retry path returns directly
    // via `return Ok(Json(...))`, and the happy path falls through
    // to the closing `}` of the for-loop naturally. Both branches
    // exit the function on completion, so there's nothing to break
    // out of.
    let tx = conn.unchecked_transaction().map_err(|e| HttpError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: e.to_string(),
    })?;

    // after the DB rows are gone, also remove the on-disk
    // session artifacts so they don't linger as orphaned data. There
    // are TWO separate trees:
    // 1. `<home>/sessions/<sid>/`        — archive-index.jsonl
    // (written via `hermix_home`, NOT data_dir)
    // 2. `<home>/data/sessions/<sid>.md` — session log
    // (written via `data_dir`, flat file)
    // The sidebar list reads from the DB only, so the session won't
    // resurrect — but leaving these behind is an orphaned-data leak.
    // Best-effort: log + ignore on failure; never fail the delete
    // because of leftover files (the DB row is already gone).
    //( / ): typed as a non-capturing `fn`
    // pointer so it can be `Copy`-captured into `spawn_blocking`
    // closures at both call sites without being consumed.
    let cleanup_session_dir: fn(&std::path::Path, &str) = |home, session_id| {
        let data_dir = hermix_config::paths::data_dir_in(home);

        // 1. Archive tree: <hermix_home>/sessions/<sid>/ (holds
        // archive-index.jsonl + per-turn markdown sidecars).
        let archive_dir = home.join("sessions").join(&session_id);
        if archive_dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&archive_dir) {
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "delete_session: archive dir removal failed (DB row already deleted)"
                );
            }
        }

        // 2. Session log file: <data>/sessions/<sid>.md
        let sessions_dir = data_dir.join("sessions");
        let log_file = sessions_dir.join(format!("{session_id}.md"));
        if log_file.exists() {
            if let Err(e) = std::fs::remove_file(&log_file) {
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "delete_session: session log file removal failed (DB row already deleted)"
                );
            }
        }
        // 2b. Legacy per-turn sidecars: any `<stamp>-<sid>.md` left in
        // sessions_dir (normally migrated to sessions_dir/orphans/
        // on the next save, but a deleted session never gets one).
        if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.ends_with(&format!("-{session_id}.md"))
                    && name != format!("{session_id}.md")
                {
                    if let Err(e) = std::fs::remove_file(entry.path()) {
                        tracing::warn!(
                            session_id = %session_id,
                            error = %e,
                            "delete_session: legacy session sidecar removal failed"
                        );
                    }
                }
            }
        }

        // 3. RL trajectory: <data>/trajectories/<sid>.json (written by
        // SessionPersistence::save_trajectory on every completed turn).
        let traj_file = data_dir.join("trajectories").join(format!("{session_id}.json"));
        if traj_file.exists() {
            if let Err(e) = std::fs::remove_file(&traj_file) {
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "delete_session: trajectory file removal failed (DB row already deleted)"
                );
            }
        }
    };

    // purge the deleted session's recall memory from
    // memory.db so a removed conversation leaves zero vector/embedding
    // residue. memory.db is WAL + owned by the same process as
    // sessions.db, so a second `Connection::open` (mirroring
    // `open_sessions_db_for_read`) is the established, safe pattern.
    // Every session-scoped store is covered:
    // - frames / facts            (tagged `session:{id}`, JSON `tags`)
    // - recall_audit / session_snapshots (explicit `session_id` col)
    // - frames_fts / facts_fts    (FTS shadow tables; must stay in
    // sync with frames/facts or future searches corrupt)
    // Best-effort: any failure is logged + ignored; the session delete
    // itself must never fail because the memory purge hiccupped.
    //: same `fn`-pointer pattern as `cleanup_session_dir`.
    let purge_session_memory: fn(&std::path::Path, &str) = |hermix_home, session_id| {
        let mem_path =
            hermix_config::paths::data_dir_in(hermix_home)
                .join("memory.db");
        let Ok(conn) = rusqlite::Connection::open(&mem_path) else {
            tracing::warn!(
                session_id = %session_id,
                "delete_session: memory.db missing, skipping recall-memory purge"
            );
            return;
        };
        // 500ms matches open_sessions_db_for_read so a concurrent
        // memory.db write doesn't stall the purge past half a second.
        let _ = conn.busy_timeout(std::time::Duration::from_millis(500));
        let _ = conn.execute_batch("PRAGMA journal_mode = WAL;");
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");

        let like = format!("%\"session:{}\"%", session_id);
        let mut total: usize = 0;

        // FTS shadow tables first — their id subquery still reads the
        // live frames/facts rows.
        let _ = conn.execute(
            "DELETE FROM frames_fts WHERE id IN (SELECT id FROM frames WHERE tags LIKE ?1)",
            rusqlite::params![like],
        );
        let _ = conn.execute(
            "DELETE FROM facts_fts WHERE id IN (SELECT id FROM facts WHERE tags LIKE ?1)",
            rusqlite::params![like],
        );
        if let Ok(n) = conn.execute("DELETE FROM frames WHERE tags LIKE ?1", rusqlite::params![like]) {
            total += n as usize;
        }
        if let Ok(n) = conn.execute("DELETE FROM facts WHERE tags LIKE ?1", rusqlite::params![like]) {
            total += n as usize;
        }
        // Explicit session_id columns (older DBs without these tables
        // simply no-op the DELETE — swallowed by `if let Ok`).
        if let Ok(n) = conn.execute(
            "DELETE FROM recall_audit WHERE session_id = ?1",
            rusqlite::params![session_id],
        ) {
            total += n as usize;
        }
        if let Ok(n) = conn.execute(
            "DELETE FROM session_snapshots WHERE session_id = ?1",
            rusqlite::params![session_id],
        ) {
            total += n as usize;
        }
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");

        if total > 0 {
            tracing::info!(
                session_id = %session_id,
                purged_rows = total,
                "delete_session: purged session recall memory from memory.db"
            );
        }
    };

    // Cascade-delete all child tables that reference sessions(id)
    let tables = [
        "messages",
        "compression_archives",
        "compression_summary_history",
        "token_usage",
    ];
    {
        for table in &tables {
            if let Err(e) = tx.execute(
                &format!("DELETE FROM {table} WHERE session_id = ?1"),
                rusqlite::params![session_id],
            ) {
                if !is_malformed_error(&e) {
                    return Err(HttpError {
                        status: StatusCode::INTERNAL_SERVER_ERROR,
                        message: e.to_string(),
                    });
                }
                // Roll back the partial cascade, rebuild FTS, retry once.
                let _ = tx.rollback();
                tracing::warn!(
                    table = %table,
                    error = %e,
                    "delete_session: cascade DELETE hit 'malformed'; rebuilding messages_fts and retrying"
                );
                if let Err(rb_err) = rebuild_messages_fts(&conn) {
                    return Err(HttpError {
                        status: StatusCode::INTERNAL_SERVER_ERROR,
                        message: format!(
                            "delete_session: cascade failed ({e}); FTS rebuild also failed ({rb_err})"
                        ),
                    });
                }
                let tx2 = conn.unchecked_transaction().map_err(|e| HttpError {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    message: e.to_string(),
                })?;
                for retry_table in &tables {
                    tx2.execute(
                        &format!("DELETE FROM {retry_table} WHERE session_id = ?1"),
                        rusqlite::params![session_id],
                    )
                    .map_err(|e2| HttpError {
                        status: StatusCode::INTERNAL_SERVER_ERROR,
                        message: format!(
                            "delete_session: retry after FTS rebuild still failed on `{retry_table}`: {e2}"
                        ),
                    })?;
                }
                let deleted = tx2
                    .execute(
                        "DELETE FROM sessions WHERE id = ?1",
                        rusqlite::params![session_id],
                    )
                    .map_err(|e| HttpError {
                        status: StatusCode::INTERNAL_SERVER_ERROR,
                        message: e.to_string(),
                    })?;
                if deleted == 0 {
                    let _ = tx2.rollback();
                    return Err(HttpError {
                        status: StatusCode::NOT_FOUND,
                        message: format!("session '{session_id}' not found"),
                    });
                }
                tx2.commit().map_err(|e| HttpError {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    message: e.to_string(),
                })?;
                // No WAL TRUNCATE here: TRUNCATE blocks the writer's
                // WAL header (1-3s on a multi-MB DB) and would starve
                // any concurrent SELECT behind busy_timeout. The next
                // open_sessions_db_for_read already runs PASSIVE before
                // its first read — that's enough for the post-delete
                // path.
                // same success-path logging/cleanup as the
                // happy path below (see cleanup_session_dir + tracing::info).
                //: side effects run fire-and-forget on the
                // blocking pool so the HTTP response returns
                // immediately and the async worker stays free.
                let home = state.hermix_home.clone();
                let sid = session_id.clone();
                tokio::task::spawn_blocking(move || {
                    cleanup_session_dir(&home, &sid);
                    purge_session_memory(&home, &sid);
                });
                tracing::info!(
                    session_id = %session_id,
                    "delete_session: deleted session + cascade child rows (after FTS rebuild)"
                );
                return Ok(Json(serde_json::json!({
                    "ok": true,
                    "fts_rebuilt": true,
                })));
            }
        }
        // Happy path — fall through to the original
        // DELETE-from-sessions / 404 / commit / wal-truncate block.
        // Replace `tx` semantics: re-bind for the rest of the fn.
        // We use a labelled break above + fall-through here, so the
        // original block can still execute against the same `tx`.
        // (Rust doesn't allow reassigning `tx`, so we use the
        // labelled block to short-circuit the retry path and let
        // execution continue with `tx` already in scope below.)
    }
    let deleted = tx
        .execute(
            "DELETE FROM sessions WHERE id = ?1",
            rusqlite::params![session_id],
        )
        .map_err(|e| HttpError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: e.to_string(),
        })?;
    if deleted == 0 {
        // Caller asked to delete a session that no longer exists.
        // Either it was already removed (idempotent retry) or the
        // path is wrong. Roll back the child-table DELETEs we just
        // ran (they might be no-ops if no rows matched, but the
        // rollback is the safe default) and report 404 so the
        // front-end can stop showing the spinner.
        let _ = tx.rollback();
        return Err(HttpError {
            status: StatusCode::NOT_FOUND,
            message: format!("session '{session_id}' not found"),
        });
    }

    tx.commit().map_err(|e| HttpError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: e.to_string(),
    })?;
    // No WAL TRUNCATE here either (same reasoning as the retry path
    // above): the open_sessions_db_for_read used by every SELECT
    // caller runs PASSIVE before its first read, which is enough to
    // keep the on-disk `sessions.db` consistent for the next opener.
    // A synchronous TRUNCATE here would block the axum worker on the
    // writer's WAL header (1-3s on a multi-MB DB) and starve the very
    // next click the user makes.

    // surface the deletion in backend logs. Previously the
    // happy path was completely silent, so operators/owners had no way
    // to confirm a delete actually happened.
    //: side effects run fire-and-forget on the blocking pool
    // — the DB row is already gone; fs cleanup + memory purge are
    // best-effort and must not block the HTTP response or the async
    // runtime worker (same class of fix as Bug #3).
    let home = state.hermix_home.clone();
    let sid = session_id.clone();
    tokio::task::spawn_blocking(move || {
        cleanup_session_dir(&home, &sid);
        purge_session_memory(&home, &sid);
    });
    tracing::info!(
        session_id = %session_id,
        "delete_session: deleted session + cascade child rows (messages/compression_archives/compression_summary_history/token_usage)"
    );

    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `GET /v1/sessions/{id}/archive-refs` — return every [`ArchiveRef`]
/// the agent loop has written for this session.
/// backing storage is a JSONL file at
/// `<hermix_home>/sessions/<sid>/archive-index.jsonl`. We don't go
/// through `AgentService` / `AgentLoop::last_archive_refs()` because
/// that field only holds the **most recent** archive run — for the
/// chat banner we want the full history of archived nodes so the
/// user can re-open any of them.
/// Returns `200 { refs: [] }` when:
/// - the session has never triggered a compression round (common),
/// - the index file is missing / empty,
/// - the session id is malformed (we still don't 404 because the
/// state of "session exists but never archived" is a valid one).
/// All errors during file IO are swallowed (logged) and surfaced as
/// an empty list — the chat UI must never crash just because the
/// archive sidecar had a bad write.
pub(crate) async fn get_session_archive_refs(
    Path(session_id): Path<String>,
) -> Json<ArchiveRefsResponse> {
    let hermix_home = hermix_config::paths::hermix_home();
    let refs = hermix_agent::read_session_index(&hermix_home, &session_id);
    Json(ArchiveRefsResponse { refs })
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ArchiveRefsResponse {
    refs: Vec<ArchiveRef>,
}

pub(crate) async fn get_session_messages(
    Path(session_id): Path<String>,
    State(state): State<HttpServerState>,
) -> Result<Json<SessionMessagesResponse>, HttpError> {
    // use the *timestamped* loader so each entry
    // carries the persisted `messages.created_at` and the desktop
    // client can render the real relative time on session reload
    // (e.g. "3 天前" instead of "刚刚"). The legacy
    // `load_session_with_approvals` was paired with `timestamp: now`
    // below, which masked every message's true age.
    let entries_with_ts = state
        .session_persistence
        .load_session_with_approvals_and_timestamps(&session_id)
        .map_err(HttpError::from)?;
    let entries: Vec<hermix_agent::SessionEntry> =
        entries_with_ts.iter().map(|(e, _)| e.clone()).collect();
    // Pull the timestamps out in lockstep with the entries below.
    let timestamps: Vec<Option<String>> =
        entries_with_ts.into_iter().map(|(_, ts)| ts).collect();
    let now = Utc::now().to_rfc3339();

    // Build tool_call_id → arguments map from all assistant messages so
    // we can reconstruct tool arguments for role: "tool" rows on reload.
    // The DB stores tool_call_id on tool messages and tool_calls[{id,
    // function.arguments}] on assistant messages — linking them at
    // serve time avoids adding an `arguments` column to the DB and a
    // new field to the Rust `Message` struct.
    let mut args_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for entry in &entries {
        if let hermix_agent::SessionEntry::Message(m) = entry {
            if let Some(ref tcs) = m.tool_calls {
                for tc in tcs {
                    args_map.insert(tc.id.clone(), tc.function.arguments.clone());
                }
            }
        }
    }

    // (Plan C): load archived messages from compression
    // child sessions so the user can still see what was said before
    // compression. Children are ordered by creation time (oldest
    // first), so the archived turns appear in chronological order
    // before the main session's active messages.
    let child_ids = state
        .session_persistence
        .find_children(&session_id)
        .unwrap_or_default();
    let mut archived_entries_with_ts: Vec<(String, Vec<(hermix_agent::SessionEntry, Option<String>)>)> = Vec::new();
    for child_id in &child_ids {
        let child_ets = state
            .session_persistence
            .load_session_with_approvals_and_timestamps(child_id)
            .unwrap_or_default();
        // Also build args_map from archived entries so tool argument
        // reconstruction works for archived tool messages.
        for (entry, _) in &child_ets {
            if let hermix_agent::SessionEntry::Message(m) = entry {
                if let Some(ref tcs) = m.tool_calls {
                    for tc in tcs {
                        args_map.insert(tc.id.clone(), tc.function.arguments.clone());
                    }
                }
            }
        }
        archived_entries_with_ts.push((child_id.clone(), child_ets));
    }

    let mut idx = 0usize;
    let mut all_messages: Vec<ProtocolMessage> = Vec::new();
    // FIX: buffer compression-child messages here instead of
    // extending `all_messages` immediately. They are prepended *after*
    // the main session messages are built so we can de-duplicate child
    // copies against the parent's (possibly reactivated) active rows.
    let mut child_all: Vec<ProtocolMessage> = Vec::new();

    // Archived messages from compression child sessions (oldest first).
    // These carry `compressed: Some(true)` so the frontend can render
    // a subtle "已压缩" marker on them.
    for (child_id, child_ets) in archived_entries_with_ts {
        let child_msgs: Vec<ProtocolMessage> = child_ets
            .into_iter()
            .filter(|(e, _ts)| !matches!(e, hermix_agent::SessionEntry::Message(m) if matches!(m.role, MessageRole::System)))
            .filter_map(|(entry, ts)| {
                match entry {
                    hermix_agent::SessionEntry::Message(m) => {
                        let id = format!("{}-{}", child_id, idx);
                        idx += 1;
                        let tool_calls = m.tool_calls.clone();
                        let tool_name = m.name.clone();
                        let execution_timeline = m.execution_timeline.clone();
                        let tool_arguments = if matches!(m.role, MessageRole::Tool) {
                            m.tool_call_id.as_ref().and_then(|id| args_map.get(id)).cloned()
                        } else {
                            None
                        };
                        let timestamp = ts.clone().unwrap_or_else(|| now.clone());
                        Some(ProtocolMessage {
                            id,
                            role: match m.role {
                                MessageRole::User => hermix_transport::Role::User,
                                MessageRole::Assistant => hermix_transport::Role::Assistant,
                                MessageRole::System => hermix_transport::Role::System,
                                MessageRole::Tool => hermix_transport::Role::Tool,
                            },
                            content: m.content.unwrap_or_default(),
                            timestamp,
                            model: None,
                            tool_calls,
                            tool_name,
                            tool_arguments,
                            extra: None,
                            reasoning_content: m.reasoning_content.clone(),
                            execution_timeline,
                            unified_steps: m.unified_steps.clone(),
                            created_at: ts,
                            compressed: Some(true),
                        })
                    }
                    hermix_agent::SessionEntry::Approval(record)
                    | hermix_agent::SessionEntry::ApprovalOrphan(record) => {
                        let id = format!("{}-approval-{}", child_id, idx);
                        idx += 1;
                        Some(ProtocolMessage {
                            id,
                            role: hermix_transport::Role::Approval,
                            content: String::new(),
                            timestamp: ts.clone().unwrap_or_else(|| now.clone()),
                            model: None,
                            tool_calls: None,
                            tool_name: None,
                            tool_arguments: None,
                            extra: Some(serde_json::to_value(&record).unwrap_or(serde_json::Value::Null)),
                            reasoning_content: None,
                            execution_timeline: None,
                            unified_steps: None,
                            created_at: ts,
                            compressed: Some(true),
                        })
                    }
                }
            })
            .collect();
        child_all.extend(child_msgs);
    }

    // Main session messages (not compressed).
    let main_msgs: Vec<ProtocolMessage> = entries
        .into_iter()
        .zip(timestamps.into_iter())
        // Defense-in-depth: the SQL filter in
        // `load_session_with_approvals` already excludes role='system'
        // rows. This match-arm is here so even if a future caller
        // accidentally loads a different entry set (e.g. a test
        // fixture) the system prompt never reaches the wire.
        .filter(|(e, _ts)| !matches!(e, hermix_agent::SessionEntry::Message(m) if matches!(m.role, MessageRole::System)))
        .map(|(entry, ts)| match entry {
            hermix_agent::SessionEntry::Message(m) => {
                let id = format!("{}-{}", session_id, idx);
                idx += 1;
                // Forward `tool_calls` (assistant rows) and `name`
                // (`role: "tool"` rows) so the desktop client can
                // rebuild the same inline tool cards the user saw
                // mid-stream on session reload. Without this,
                // `ProtocolMessage` only carries
                // `id/role/content/timestamp` and the front-end is
                // forced to fall back to a generic "tool" label.
                let tool_calls = m.tool_calls.clone();
                let tool_name = m.name.clone();
                // forward the persisted execution
                // timeline so session reloads can rebuild the same
                // inline tool cards the user saw mid-stream
                // (correct status, arguments, stdout). Without this,
                // every tool call after a session reload rendered
                // as a stale "运行中" pill because no event marked
                // them as completed.
                let execution_timeline = m.execution_timeline.clone();
                // reconstruct tool arguments from
                // the linked assistant message's tool_calls. Without
                // this, reloaded tool messages have no `args` and
                // the desktop client falls back to "调用工具" (generic).
                let tool_arguments = if matches!(m.role, MessageRole::Tool) {
                    m.tool_call_id
                        .as_ref()
                        .and_then(|id| args_map.get(id))
                        .cloned()
                } else {
                    None
                };
                // use the persisted `created_at`
                // (carried in via the parallel query) as the wire
                // timestamp so the desktop client shows the real
                // relative time on reload. Fall back to `now` only
                // if the timestamp query somehow missed this row
                // (shouldn't happen — same filter on both sides).
                let timestamp = ts.clone().unwrap_or_else(|| now.clone());
                ProtocolMessage {
                    id,
                    role: match m.role {
                        MessageRole::User => hermix_transport::Role::User,
                        MessageRole::Assistant => hermix_transport::Role::Assistant,
                        MessageRole::System => hermix_transport::Role::System,
                        MessageRole::Tool => hermix_transport::Role::Tool,
                    },
                    // (Bug B): persist already runs through
                    // `ToolCallSanitizer` in `stream_collect` and the
                    // ThinkBlockParser strips `<think>...</think>` from
                    // visible content at write time, so what we read
                    // back here is already clean. The earlier
                    // "defence-in-depth" feed+flush on the read path
                    // was a *streaming state machine* applied to a
                    // whole-string input — when the persisted content
                    // happened to contain a lone `<think>` (no
                    // `</think>`), the sanitizer would enter
                    // `InsideThink`, hold the rest of the string, and
                    // `flush()` would discard it (held text is not
                    // re-emitted on flush). Net effect: the user
                    // reloaded the session and the assistant bubble
                    // was blank. Sanitization on the read path is
                    // fundamentally the wrong shape — content is a
                    // complete blob, not a stream — so we just trust
                    // what was persisted.
                    content: m.content.unwrap_or_default(),
                    timestamp,
                    model: None,
                    tool_calls,
                    tool_name,
                    tool_arguments,
                    extra: None,
                    reasoning_content: m.reasoning_content.clone(),
                    execution_timeline,
                    unified_steps: m.unified_steps.clone(),
                    created_at: ts,
                    compressed: None,
                }
            }
            hermix_agent::SessionEntry::Approval(record)
            | hermix_agent::SessionEntry::ApprovalOrphan(record) => {
                let id = format!("{}-approval-{}", session_id, idx);
                idx += 1;
                // Embed the full ApprovalRecord as JSON for the UI to
                // consume directly. The frontend `ApprovalMessage` type
                // mirrors this shape (requestId / tool / args /
                // riskLevel / decision / decidedAt / linkedAssistantId).
                ProtocolMessage {
                    id,
                    role: hermix_transport::Role::Approval,
                    content: String::new(),
                    timestamp: ts.clone().unwrap_or_else(|| now.clone()),
                    model: None,
                    // Approval rows don't carry tool-calls; the
                    // tool name and risk for the request live in
                    // `extra` (mirrors `ApprovalRecord`). The two
                    // new fields are `None` here.
                    tool_calls: None,
                    tool_name: None,
                    tool_arguments: None,
                    extra: Some(serde_json::to_value(&record).unwrap_or(serde_json::Value::Null)),
                    reasoning_content: None,
                    // Approval rows are not associated with any
                    // assistant turn's tool calls, so no timeline.
                    execution_timeline: None,
                    // Approvals carry no reasoning/tools timeline.
                    unified_steps: None,
                    created_at: ts,
                    compressed: None,
                }
            }
        })
        .collect();
    // FIX (restore+compression duplicate): a compression
    // child session stores a *copy* of the folded-away turns (written
    // with active=1 into the child). At compression time the parent's
    // own copy of those turns is deactivated, so normally only the
    // child copy is shown. But after `/restore` (restore_last_undone)
    // reactivates the parent's turns, BOTH the parent's reactivated
    // rows AND the child's copy would be returned — every archived
    // turn rendered twice in the UI. Drop a child message when an
    // identical (role+content) message already exists among the
    // parent's active messages. Because the child copy is a faithful
    // byte-for-byte copy of the original turn content, this signature
    // match is exact; legitimate distinct turns are never collapsed.
    let role_sig = |r: &hermix_transport::Role| -> &'static str {
        match r {
            hermix_transport::Role::User => "user",
            hermix_transport::Role::Assistant => "assistant",
            hermix_transport::Role::System => "system",
            hermix_transport::Role::Tool => "tool",
            hermix_transport::Role::Approval => "approval",
        }
    };
    let main_sigs: std::collections::HashSet<String> = main_msgs
        .iter()
        .map(|m| format!("{}|{}", role_sig(&m.role), m.content))
        .collect();
    let child_filtered: Vec<ProtocolMessage> = child_all
        .into_iter()
        .filter(|m| !main_sigs.contains(&format!("{}|{}", role_sig(&m.role), m.content)))
        .collect();
    // Preserve chronological order: archived (child) first, then the
    // parent's active messages.
    all_messages.extend(child_filtered);
    all_messages.extend(main_msgs);

    let context_usage = state.session_persistence.get_context_usage(&session_id);
    Ok(Json(SessionMessagesResponse {
        session_id,
        messages: all_messages,
        context_usage,
    }))
}

/// Request body for [`save_session_unified_steps`].
/// `unified_steps` is the frontend-built chronological
/// reasoning/tools/text timeline (array of `{kind, ...}` step objects).
#[derive(serde::Deserialize)]
pub(crate) struct UnifiedStepsBody {
    unified_steps: serde_json::Value,
}

/// Persist the desktop client's frontend-built `unified_steps` timeline
/// for the just-completed assistant turn. The agent loop can't produce
/// this (it's assembled live in the client from the WS stream), so the
/// client ships it here after the stream finishes. See
/// `SessionPersistence::update_latest_assistant_unified_steps` for the
/// DB write and the re-flush carry-forward that keeps it alive.
pub(crate) async fn save_session_unified_steps(
    Path(session_id): Path<String>,
    State(state): State<HttpServerState>,
    Json(body): Json<UnifiedStepsBody>,
) -> Result<StatusCode, HttpError> {
    let json = serde_json::to_string(&body.unified_steps).unwrap_or_default();
    state
        .session_persistence
        .update_latest_assistant_unified_steps(&session_id, &json)
        .map_err(HttpError::from)?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn send_message(
    Path(session_id): Path<String>,
    State(state): State<HttpServerState>,
    Json(req): Json<SendMessageRequest>,
) -> Result<Json<SendMessageResponse>, HttpError> {
    hermix_telemetry::record_http_request();
    let trace_id = hermix_transport::new_trace_id();

    // Build AgentOverrides from request
    let overrides = hermix_core::traits::AgentOverrides {
        model: req.model.clone(),
        provider: req.provider.clone(),
        personality: req.personality.clone(),
        system_prompt: req.system_prompt.clone(),
        tools: req.tools.clone(),
        workspace: req.workspace.clone(),
        permission: parse_permission_level(req.permission.as_deref()),
        images: convert_wire_images(&req.images),
        image: None,
        image_mime_type: None,
        skills: req.skills.clone(),
        memory_dir: None,
        approval_requester: None,
        on_event: None,
        pre_approval_checker: None,
        on_prefetch_intent: None,
        // type-B: non-streaming HTTP path doesn't own a WS
        // socket, so there is no soft-shutdown signal to wake any
        // approval `rx.await`. Leave None — it falls back to plain
        // `rx.await` (which here has no race because this path
        // doesn't abort a task).
        shutdown: None,
    };

    // Call agent service
    let reply = state
        .agent_service
        .send_message(&session_id, &req.text, overrides)
        .await
        .map_err(|e| HttpError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: e.to_string(),
        })?;

    Ok(Json(SendMessageResponse {
        session_id,
        reply: reply.text,
        message_count: reply.message_count,
        trace_id,
    }))
}

//T2-A : moved from `dashboard/sessions.rs`. The dashboard
// router was deleted and these endpoints now share the same `/v1/sessions/*`
// namespace as the rest of the session API.

// ── /undo (soft-delete) ─────────────────────────────────────────────

/// POST /v1/sessions/{session_id}/undo?count=N
/// Soft-delete the most recent `count` user-to-assistant turns.
/// Default count is 1 if the query string is absent.
pub(crate) async fn undo_session(
    State(state): State<HttpServerState>,
    Path(session_id): Path<String>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let count: usize = params
        .get("count")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .max(1);
    let n = state
        .session_persistence
        .mark_recent_turns_inactive(&session_id, count)
        .map_err(HttpError::from)?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "session_id": session_id,
        "deactivated": n,
        "turns_requested": count,
    })))
}

/// POST /v1/sessions/{session_id}/restore
/// Restore the most recently deactivated block of messages. Returns
/// the number of messages that were reactivated.
pub(crate) async fn restore_session(
    State(state): State<HttpServerState>,
    Path(session_id): Path<String>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let n = state
        .session_persistence
        .restore_last_undone(&session_id)
        .map_err(HttpError::from)?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "session_id": session_id,
        "restored": n,
    })))
}

/// POST /v1/sessions/{session_id}/approval
/// REST fallback for approval responses when WebSocket delivery fails.
/// The frontend sends the approval via HTTP when `ws.send()` returns true
/// but the server doesn't receive the message (Tauri WebView2 WebSocket
/// can lose buffered messages on connection reset).
/// Request body: `{ "request_id": "...", "decision": "approved" | "denied" }`
pub(crate) async fn approve_via_rest(
    State(state): State<HttpServerState>,
    Path(session_id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let request_id = body
        .get("request_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| HttpError {
            status: StatusCode::BAD_REQUEST,
            message: "missing request_id".into(),
        })?;

    let decision = body
        .get("decision")
        .and_then(|v| v.as_str())
        .unwrap_or("approved");

    let choice = hermix_transport::ApprovalChoice::from_decision_str(decision);
    let choice = match choice {
        Some(c) => c,
        None => hermix_transport::ApprovalChoice::Deny,
    };

    let approved = choice.is_approved();
    let outcome = if approved {
        hermix_core::ApprovalOutcome::Approved
    } else {
        hermix_core::ApprovalOutcome::DeniedByUser
    };

    // 2026-08-29 fix (approval stuck after batch approve): the batch
    // REST fallback sends the coalescer's `batch_id` in the
    // `request_id` field (the frontend reuses the same key for both
    // shapes — see App.tsx handleApproveBatch). A bare `resolve` on a
    // batch_id ALWAYS misses (the id lives in the batches map, not the
    // per-request map), so when the live WS dropped the
    // `approval_responses_batch` frame the fallback was a no-op and —
    // with DEFAULT_APPROVAL_TIMEOUT = ZERO (no auto-deny) — the agent
    // blocked on AwaitApproval forever. Try the batch fan-out first;
    // fall back to the single-request resolve for legacy single-item
    // payloads.
    let batch_pairs = state
        .approval_registry
        .resolve_batch(&session_id, request_id, outcome.clone());
    let batch_matched = batch_pairs.iter().filter(|(_, ok)| *ok).count();
    let matched = if batch_matched > 0 {
        // Record side effects per resolved member (same policy as the
        // WS streaming batch path: only members that actually resolved).
        for (rid, ok) in &batch_pairs {
            if *ok {
                crate::util::record_approval_resolution(
                    &state, &session_id, rid, choice,
                )
                .await;
            }
        }
        true
    } else {
        let single = state
            .approval_registry
            .resolve(&session_id, request_id, outcome);
        single
    };

    tracing::info!(
        session_id = %session_id,
        request_id = %request_id,
        decision = %decision,
        matched,
        batch_resolved = batch_matched,
        "REST approval: {}",
        if matched { "hit" } else { "miss — no pending entry" }
    );

    // Persist the decision to the session timeline
    let decision_str = match choice {
        hermix_transport::ApprovalChoice::Once => "once",
        hermix_transport::ApprovalChoice::Session => "session",
        hermix_transport::ApprovalChoice::Always => "always",
        hermix_transport::ApprovalChoice::Deny => "deny",
    };
    let _ = state
        .session_persistence
        .resolve_approval_message(&session_id, request_id, decision_str);

    //Record side effects for session/always choices. P6 :
    // resource-scope approvals (P6 direction 1, `risk: "resource"`)
    // carry path/command-prefix grants — handled by the dedicated
    // resource-side-effect path that persists to GatewayConfig. The
    // existing command-only path remains as a fallback for the
    // Dangerous-tier terminal approvals that don't carry a path.
    // Only record on the **first** successful resolve. Duplicate
    // responses (matched=false from the resolve call above) would
    // otherwise re-grant the same path / re-add the same allowlist
    // regex. `grant_path` / `grant_command` are idempotent, but
    // `UserAllowlist::add_literal` and `session_bypass` push are
    // not — so the matched gate is the cheapest place to dedupe.
    if matched
        && matches!(
            choice,
            hermix_transport::ApprovalChoice::Session
                | hermix_transport::ApprovalChoice::Always
        )
    {
        // Try the resource path first — for non-resource approvals
        // (legacy terminal danger), `lookup_pending_resource` returns
        // `(None, None)` and the function returns early without
        // touching the profile. The regular command-side-effect then
        // runs as the fallback.
        record_resource_approval_side_effect(&state, &session_id, request_id, choice).await;
        if let Ok(Some((tool, params))) =
            ws::lookup_pending_approval(&state, &session_id, request_id).await
        {
            record_approval_side_effect(&state, &session_id, &tool, &params, choice);
        }
    }

    Ok(Json(serde_json::json!({
        "ok": true,
        "session_id": session_id,
        "request_id": request_id,
        "matched": matched,
        "decision": decision_str,
    })))
}

/// `GET /v1/sessions/{session_id}/lineage` — return the parent→ancestor
/// chain (upward) and immediate children (downward) for session lineage
/// browsing in the frontend tree view.
/// Response: `{ ancestors: [SessionNode], children: [SessionNode], current: SessionNode }`
pub(crate) async fn get_session_lineage(
    Path(session_id): Path<String>,
    State(state): State<HttpServerState>,
) -> Result<Json<serde_json::Value>, HttpError> {
    #[derive(Serialize)]
    struct SessionNode {
        id: String,
        title: String,
        has_compression: bool,
    }

    let sp = &state.session_persistence;

    // Walk up the parent chain.
    let mut ancestors: Vec<SessionNode> = Vec::new();
    let mut cursor = sp.get_parent(&session_id)?;
    while let Some(ref pid) = cursor {
        if pid.is_empty() {
            break;
        }
        if let Some((title, _)) = sp.get_session_title(pid)? {
            ancestors.push(SessionNode {
                id: pid.clone(),
                title: title.clone(),
                has_compression: sp.load_compressed_summary(pid).is_some(),
            });
        } else {
            ancestors.push(SessionNode {
                id: pid.clone(),
                title: String::new(),
                has_compression: false,
            });
        }
        cursor = sp.get_parent(pid)?;
    }

    // Immediate children.
    let child_ids = sp.find_children(&session_id)?;
    let mut children: Vec<SessionNode> = Vec::new();
    for cid in &child_ids {
        let (title, _) = sp.get_session_title(cid)
            .unwrap_or_else(|_| None)
            .unwrap_or((String::new(), None));
        children.push(SessionNode {
            id: cid.clone(),
            title,
            has_compression: sp.load_compressed_summary(cid).is_some(),
        });
    }

    // Current node.
    let (cur_title, _) = sp.get_session_title(&session_id)
        .unwrap_or_else(|_| None)
        .unwrap_or((String::new(), None));
    let current = SessionNode {
        id: session_id.clone(),
        title: cur_title,
        has_compression: sp.load_compressed_summary(&session_id).is_some(),
    };

    Ok(Json(serde_json::json!({
        "ancestors": ancestors,
        "children": children,
        "current": current,
    })))
}

/// GET /v1/sessions/search?q=...
pub(crate) async fn search_sessions(
    State(state): State<HttpServerState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<SessionSearchResponse> {
    let q = params.get("q").cloned().unwrap_or_default();
    let limit: u32 = params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let db_path = hermix_config::paths::data_dir_in(&state.hermix_home).join("sessions.db");
    let results = match open_sessions_db_for_read(&db_path) {
        Ok(conn) => query_search_with_conn(&conn, &q, limit).unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    Json(SessionSearchResponse { results })
}

/// FTS5 search across the messages table. Returns snippets ordered by
/// rank. Best-effort — on any read failure returns an empty vec so the
/// dashboard search box never 500s.
/// FTS5 search using an already-opened connection (so callers that
/// already hold a handle — e.g. `search_sessions` after opening via
/// `open_sessions_db_for_read` — don't pay for a second open).
pub(crate) fn query_search_with_conn(
    conn: &rusqlite::Connection,
    query: &str,
    limit: u32,
) -> Result<Vec<SessionSearchResult>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT m.session_id, snippet(messages_fts, 0, '>>>', '<<<', '...', 48) as snip,
                    m.role, s.platform, s.model, s.created_at
             FROM messages_fts AS mf
             JOIN messages AS m ON m.id = mf.rowid
             JOIN sessions AS s ON s.id = m.session_id
             WHERE messages_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )
        .map_err(|e| e.to_string())?;

    let results = stmt
        .query_map(rusqlite::params![query, limit], |row| {
            let session_id: String = row.get(0)?;
            let snippet: String = row.get(1)?;
            let role: Option<String> = row.get(2)?;
            let source: Option<String> = row.get(3)?;
            let model: Option<String> = row.get(4)?;
            let created_at: Option<String> = row.get(5)?;
            let session_started = created_at.and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(&s)
                    .map(|d| d.timestamp() as f64)
                    .ok()
            });

            Ok(SessionSearchResult {
                session_id,
                snippet,
                role,
                source,
                model,
                session_started,
            })
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

pub(crate) async fn exec_command(
    State(state): State<HttpServerState>,
    Json(req): Json<CommandRequest>,
) -> Result<Json<CommandResponse>, HttpError> {
    hermix_telemetry::record_http_request();
    let trimmed = req.command.trim();
    if trimmed.is_empty() {
        return Ok(Json(CommandResponse {
            accepted: false,
            output: "empty command".to_string(),
        }));
    }

    let cmd = if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{}", trimmed)
    };

    let session_id = req
        .session_id
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "default".to_string());

    // For now, we'll just send the command as a regular message
    // In a real implementation, we'd need to handle slash commands specially
    let overrides = hermix_core::traits::AgentOverrides::default();
    let reply = state
        .agent_service
        .send_message(&session_id, &cmd, overrides)
        .await
        .map_err(|e| HttpError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: e.to_string(),
        })?;

    Ok(Json(CommandResponse {
        accepted: true,
        output: reply.text,
    }))
}

/// POST /v1/llm/warmup — UI 切换模型时触发对应 provider 预热。
/// 立即返回，实际 1-token 探测在 agent service 内 spawn 执行。
pub(crate) async fn warm_up_provider_handler(
    State(state): State<HttpServerState>,
    Json(req): Json<WarmUpProviderRequest>,
) -> StatusCode {
    if !req.model.is_empty() && req.model != "unknown" {
        state.agent_service.warm_up_provider(&req.model);
    }
    StatusCode::OK
}

#[derive(Deserialize)]
pub(crate) struct WarmUpProviderRequest {
    model: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Regression guard: open_sessions_db_for_read must bound
    /// busy_timeout at 500ms (not 5000ms) so a concurrent DELETE
    /// can't stall the next SELECT on the WAL writer lock. The old
    /// 5s window turned "switch sessions" into "click 3-5 times
    /// before it opens".
    #[test]
    fn open_sessions_db_for_read_bounds_busy_timeout_at_500ms() {
        let mut tmp = tempfile_helpers::temp_db_path("sessions_busy_to");
        tmp.set_extension("db");
        let _ = std::fs::File::create(&tmp).unwrap().write_all(b"").unwrap();

        let conn = open_sessions_db_for_read(&tmp).expect("open ok");
        let busy_ms: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .expect("PRAGMA readable");
        assert!(
            busy_ms <= 500,
            "busy_timeout must be ≤ 500ms, got {busy_ms}"
        );
        let _ = std::fs::remove_file(&tmp);
    }

    mod tempfile_helpers {
        use std::path::PathBuf;

        pub(super) fn temp_db_path(label: &str) -> PathBuf {
            let mut p = std::env::temp_dir();
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            p.push(format!("hermix_routes_{label}_{stamp}.tmp"));
            p
        }
    }
}

