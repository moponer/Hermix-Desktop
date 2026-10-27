//! 审批行查询：从持久化的 messages.extra 反查 tool/资源参数（自 ws.rs 拆分，P1-13）。
use super::*;

/// Returns the `tool` name and the raw `arguments` object from the
/// persisted approval row so callers can re-derive the trust
/// signature for any tool type. Used by the WS + REST `Always` /
/// `Session` side-effect paths.
///
/// Like every approval lookup here, this runs **after**
/// `approval_registry.resolve()` has removed the pending entry —
/// the SQLite `messages.extra` row is the only surviving source.
///
/// Returns `(tool, arguments, expires_at_ms)`. The deadline is
/// carried alongside so a re-projected card (one rebuilt on a fresh
/// connection after the original socket closed) still shows a
/// countdown instead of hanging forever in the UI.
pub(crate) async fn lookup_pending_approval(
    state: &HttpServerState,
    session_id: &str,
    request_id: &str,
) -> Result<Option<(String, serde_json::Value, Option<i64>)>, String> {
    let db_path = state.session_persistence.db_path().to_path_buf();
    let session_id = session_id.to_string();
    let request_id = request_id.to_string();
    tokio::task::spawn_blocking(move || {
        let needle = format!("%\"request_id\":\"{}\"%", request_id);
        let conn = rusqlite::Connection::open(&db_path)
            .map_err(|e| format!("open sessions db: {e}"))?;
        let mut stmt = conn
            .prepare(
                "SELECT extra FROM messages
                 WHERE session_id = ?1 AND role = 'approval' AND extra LIKE ?2
                 LIMIT 1",
            )
            .map_err(|e| format!("prepare: {e}"))?;
        let extra: Option<String> = match stmt.query_row(
            rusqlite::params![session_id, needle],
            |row| row.get::<_, Option<String>>(0),
        ) {
            Ok(s) => s,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(format!("query: {e}")),
        };
        let Some(extra) = extra else {
            return Ok(None);
        };
        let v: serde_json::Value = serde_json::from_str(&extra)
            .map_err(|e| format!("parse extra: {e}"))?;
        let tool = v
            .get("tool")
            .and_then(|t| t.as_str())
            .unwrap_or("unknown")
            .to_string();
        // Persisted arguments may be a JSON object or stringified
        // JSON; tolerate both by parsing if it looks like a JSON
        // value (starts with `{` or `[`).
        let args_value: serde_json::Value = match v.get("arguments") {
            Some(serde_json::Value::Object(_)) | Some(serde_json::Value::Array(_)) => {
                v.get("arguments").cloned().unwrap_or(serde_json::Value::Null)
            }
            Some(serde_json::Value::String(s)) if s.trim_start().starts_with('{') => {
                serde_json::from_str(s).unwrap_or(serde_json::Value::Null)
            }
            _ => serde_json::Value::Null,
        };
        // Deadline is optional: approvals created before the field
        // existed (or ones with no timeout) simply carry `None`, and
        // the front end then renders no countdown.
        let expires_at = v.get("expires_at").and_then(|t| t.as_i64());
        Ok(Some((tool, args_value, expires_at)))
    })
    .await
    .map_err(|e| format!("lookup_pending_approval join error: {e}"))?
}

/// P6 : lookup variant for **resource-scope** approvals
/// (P6 direction 1, `risk: "resource"`). Returns `(path, command)`
/// extracted from the same persisted approval row. Either field may
/// be `None` depending on what the original tool call used. Caller
/// (typically `record_approval_side_effect_for_resource`) decides
/// what to persist based on which fields are populated.
/// IMPORTANT : like [`lookup_pending_command`], this is
/// called after `approval_registry.resolve()`, so the previous
/// `is_pending` guard would always short-circuit to `(None, None)`
/// and silently discard every Session/Always grant. The persisted
/// `messages.extra` row is the source of truth for the resource
/// arguments — registry state is irrelevant.
pub(crate) async fn lookup_pending_resource(
    state: &HttpServerState,
    session_id: &str,
    request_id: &str,
) -> Result<(Option<String>, Option<String>), String> {
    //Same offloading rationale as `lookup_pending_command` .
    let db_path = state.session_persistence.db_path().to_path_buf();
    let session_id = session_id.to_string();
    let request_id = request_id.to_string();
    tokio::task::spawn_blocking(move || {
    let needle = format!("%\"request_id\":\"{}\"%", request_id);
    let conn = rusqlite::Connection::open(&db_path)
        .map_err(|e| format!("open sessions db: {e}"))?;
    let mut stmt = conn
        .prepare(
            "SELECT extra FROM messages
             WHERE session_id = ?1 AND role = 'approval' AND extra LIKE ?2
             LIMIT 1",
        )
        .map_err(|e| format!("prepare: {e}"))?;
    let extra: Option<String> = match stmt.query_row(
        rusqlite::params![session_id, needle],
        |row| row.get::<_, Option<String>>(0),
    ) {
        Ok(s) => s,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok((None, None)),
        Err(e) => return Err(format!("query: {e}")),
    };
    let Some(extra) = extra else {
        return Ok((None, None));
    };
    let v: serde_json::Value = serde_json::from_str(&extra)
        .map_err(|e| format!("parse extra: {e}"))?;
    let arguments = v.get("arguments");
    let path = arguments
        .and_then(|a| a.get("path"))
        .and_then(|p| p.as_str())
        .map(|s| s.to_string());
    let command = arguments
        .and_then(|a| a.get("command"))
        .or_else(|| arguments.and_then(|a| a.get("cmd")))
        .and_then(|c| c.as_str())
        .map(|s| s.to_string());
    Ok((path, command))
    })
    .await
    .map_err(|e| format!("lookup_pending_resource join error: {e}"))?
}
