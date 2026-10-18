//! Memory-inspection API for the desktop "记忆管理" settings page.
//!
//! Read-mostly view over the same stores the agent loop writes:
//! `memory.db` (frames / facts / reflections via the
//! `hermix-focus` embedded backend), `daily_memory.db` and the
//! `MEMORY.md` / `USER.md` profile files (global + per-session
//! scopes). The graph assembles nodes from all of them so the user
//! can finally *see* what the agent remembers; item detail /
//! soft-delete / pin close the management loop.
//!
//! Node ids are self-describing so the detail endpoints stay
//! stateless: `fact:<uuid>`, `frame:<uuid>`, `refl:<rowid>`,
//! `daily:<date>`, `md:<uuid>@<entry-index>`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use axum::extract::{Query, State};
use axum::Json;
use hermix_focus::embedded::ui::UiMemoryRow;
use hermix_focus::EmbeddedMemoryBackend;
use serde::Serialize;

use crate::{HttpError, HttpServerState};

/// Node ids for md entries embed a per-process uuid → file-path map
/// so the item endpoint never accepts raw paths from the client
/// (trust boundary: the API only ever reads files it listed itself).
static MD_FILE_REGISTRY: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();

fn md_registry() -> &'static Mutex<HashMap<String, PathBuf>> {
    MD_FILE_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

// ── backend handle ─────────────────────────────────────────────────

/// One shared read connection, opened lazily and retried until the
/// agent loop creates the db (fresh installs have no memory.db yet).
fn memory_backend(state: &HttpServerState) -> Option<Arc<EmbeddedMemoryBackend>> {
    static BACKEND: OnceLock<Mutex<Option<Arc<EmbeddedMemoryBackend>>>> = OnceLock::new();
    let cell = BACKEND.get_or_init(|| Mutex::new(None));
    let mut guard = cell.lock().ok()?;
    if let Some(b) = guard.as_ref() {
        return Some(Arc::clone(b));
    }
    let db_path = hermix_config::paths::data_dir_in(&state.hermix_home).join("memory.db");
    match EmbeddedMemoryBackend::open(&db_path) {
        Ok(b) => {
            *guard = Some(Arc::new(b));
            guard.as_ref().map(Arc::clone)
        }
        Err(e) => {
            tracing::debug!(target: "hermix.server", error = %e, "memory_inspect: memory.db not openable yet");
            None
        }
    }
}

// ── wire types ─────────────────────────────────────────────────────

#[derive(Serialize, Clone, Debug)]
pub struct GraphNode {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub preview: String,
    pub salience: f32,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub pinned: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct GraphEdge {
    pub source: String,
    pub target: String,
    /// `parent` (delegation hierarchy) | `daily` (same-day grouping).
    pub relation: String,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct MemoryOverview {
    pub frames: i64,
    pub facts: i64,
    pub reflections: i64,
    pub daily_days: i64,
    pub md_entries: i64,
    pub db_bytes: i64,
    /// (local date, visible rows created that day), newest first.
    pub per_day: Vec<(String, i64)>,
}

#[derive(Serialize)]
pub struct GraphResponse {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub stats: MemoryOverview,
    /// True when the source stores had more rows than `max_nodes`
    /// allowed — the UI shows a "结果已截断" hint.
    pub truncated: bool,
}

#[derive(Serialize)]
pub struct MemoryItemResponse {
    pub id: String,
    pub kind: String,
    pub content: String,
    pub created_at: String,
    pub salience: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
}

// ── helpers ────────────────────────────────────────────────────────

fn truncate_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let head: String = s.chars().take(n).collect();
        format!("{head}…")
    }
}

/// Pull a human-readable line out of a stored payload: JSON objects
/// are probed for the usual text fields, plain strings pass through.
fn payload_text(payload: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) {
        if let Some(s) = v.as_str() {
            return s.to_string();
        }
        if let Some(obj) = v.as_object() {
            for field in ["task", "summary", "text", "content", "conclusion", "key", "description"] {
                if let Some(s) = obj.get(field).and_then(|x| x.as_str()) {
                    if !s.trim().is_empty() {
                        return s.to_string();
                    }
                }
            }
            return serde_json::to_string_pretty(&v).unwrap_or_else(|_| payload.to_string());
        }
    }
    payload.to_string()
}

fn row_to_graph_node(id_prefix: &str, row: &UiMemoryRow) -> GraphNode {
    let text = payload_text(&row.text);
    let is_fact = row.kind == "fact";
    GraphNode {
        id: format!("{id_prefix}{}", row.id),
        kind: if row.kind.starts_with("reflection:") {
            "reflection".to_string()
        } else {
            row.kind.clone()
        },
        label: truncate_chars(if is_fact {
            row.key.as_deref().unwrap_or("fact")
        } else {
            &text
        }, 48),
        preview: truncate_chars(&text, 200),
        salience: row.salience,
        created_at: row.created_at.clone(),
        session_id: row.session_id.clone(),
        pinned: row.pin_count > 0,
    }
}

/// Collect the profile-md entries shown as graph nodes: the global
/// `memories/` files plus the per-session scopes (newest 20 by
/// mtime). Returns (file path, entry index, entry text).
fn md_entries(state: &HttpServerState) -> Vec<(PathBuf, usize, String)> {
    let mut files: Vec<PathBuf> = Vec::new();
    let memories_dir = state.hermix_home.join("memories");
    for name in ["MEMORY.md", "USER.md"] {
        let p = memories_dir.join(name);
        if p.is_file() {
            files.push(p);
        }
    }
    let sessions_dir = state.hermix_home.join("sessions");
    if sessions_dir.is_dir() {
        let mut scoped: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&sessions_dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let p = e.path().join("memories").join("MEMORY.md");
                let meta = std::fs::metadata(&p).ok()?;
                Some((meta.modified().ok()?, p))
            })
            .collect();
        scoped.sort_by(|a, b| b.0.cmp(&a.0));
        files.extend(scoped.into_iter().take(20).map(|(_, p)| p));
    }
    let mut out = Vec::new();
    for file in files {
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (i, entry) in hermix_tools::backends::memory::FileMemoryBackend::parse_entries(&content)
            .into_iter()
            .enumerate()
        {
            if !entry.trim().is_empty() {
                out.push((file.clone(), i, entry));
            }
        }
    }
    out
}

fn md_node_id(file: &std::path::Path, entry_index: usize) -> Option<String> {
    let key = uuid::Uuid::new_v4().to_string();
    {
        let mut reg = md_registry().lock().ok()?;
        // Every graph load registers fresh ids; drop stale ones once
        // the map grows past a bound instead of leaking forever.
        if reg.len() > 10_000 {
            reg.clear();
        }
        reg.insert(key.clone(), file.to_path_buf());
    }
    Some(format!("md:{key}@{entry_index}"))
}

fn overview(state: &HttpServerState, backend: Option<&Arc<EmbeddedMemoryBackend>>) -> MemoryOverview {
    let mut ov = MemoryOverview::default();
    if let Some(b) = backend {
        if let Ok(s) = b.ui_stats() {
            ov.frames = s.frames;
            ov.facts = s.facts;
            ov.reflections = s.reflections;
            ov.db_bytes = s.db_bytes;
            ov.per_day = s.per_day;
        }
    }
    if let Ok(store) = hermix_tools::tools::daily_memory::DailyMemoryStore::open() {
        if let Ok(days) = store.list_days(1000) {
            ov.daily_days = days.len() as i64;
        }
    }
    ov.md_entries = md_entries(state).len() as i64;
    ov
}

// ── handlers ───────────────────────────────────────────────────────

/// GET /api/memory/graph?max_nodes=400
pub async fn get_memory_graph(
    State(state): State<HttpServerState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<GraphResponse>, HttpError> {
    let max_nodes: usize = params
        .get("max_nodes")
        .and_then(|v| v.parse().ok())
        .unwrap_or(400)
        .clamp(20, 2000);
    let backend = memory_backend(&state);
    let mut nodes: Vec<GraphNode> = Vec::new();
    let mut edges: Vec<GraphEdge> = Vec::new();
    let mut truncated = false;

    let mut node_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let push_node = |n: GraphNode, nodes: &mut Vec<GraphNode>, ids: &mut std::collections::HashSet<String>, truncated: &mut bool| {
        if nodes.len() >= max_nodes {
            *truncated = true;
            return;
        }
        ids.insert(n.id.clone());
        nodes.push(n);
    };

    if let Some(b) = backend.as_ref() {
        for row in b.ui_list_facts(max_nodes as u32).unwrap_or_default() {
            push_node(row_to_graph_node("fact:", &row), &mut nodes, &mut node_ids, &mut truncated);
        }
        let frame_rows = b.ui_list_frames(max_nodes as u32).unwrap_or_default();
        for row in &frame_rows {
            push_node(row_to_graph_node("frame:", row), &mut nodes, &mut node_ids, &mut truncated);
        }
        // Delegation hierarchy edges (only when both ends survived
        // the node cap).
        for row in &frame_rows {
            if let Some(parent) = &row.parent_id {
                let (s, t) = (format!("frame:{}", parent), format!("frame:{}", row.id));
                if node_ids.contains(&s) && node_ids.contains(&t) {
                    edges.push(GraphEdge { source: s, target: t, relation: "parent".into() });
                }
            }
        }
        for row in b.ui_list_reflections(max_nodes as u32).unwrap_or_default() {
            push_node(row_to_graph_node("refl:", &row), &mut nodes, &mut node_ids, &mut truncated);
        }
    }

    // Daily-memory day nodes + same-day edges into frames/reflections.
    if let Ok(store) = hermix_tools::tools::daily_memory::DailyMemoryStore::open() {
        if let Ok(days) = store.list_days(60) {
            for day in days {
                let id = format!("daily:{}", day.date);
                let first_line = day
                    .conclusions
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("(无结论)")
                    .trim()
                    .to_string();
                push_node(
                    GraphNode {
                        id: id.clone(),
                        kind: "daily".into(),
                        label: day.date.clone(),
                        preview: truncate_chars(&first_line, 200),
                        salience: 0.5,
                        created_at: day.date.clone(),
                        session_id: None,
                        pinned: false,
                    },
                    &mut nodes,
                    &mut node_ids,
                    &mut truncated,
                );
                let date = day.date.as_str();
                let mut linked = 0;
                for n in &nodes {
                    if linked >= 8 {
                        break;
                    }
                    if (n.kind == "reflection" || (n.kind != "daily" && n.kind != "md" && n.kind != "fact"))
                        && n.created_at.starts_with(date)
                    {
                        edges.push(GraphEdge {
                            source: id.clone(),
                            target: n.id.clone(),
                            relation: "daily".into(),
                        });
                        linked += 1;
                    }
                }
            }
        }
    }

    // Profile-md entries (global + recent session scopes).
    for (file, index, entry) in md_entries(&state) {
        let Some(id) = md_node_id(&file, index) else { continue };
        push_node(
            GraphNode {
                label: truncate_chars(&entry, 48),
                preview: truncate_chars(&entry, 200),
                salience: 0.7,
                id,
                kind: "md".into(),
                created_at: file
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .map(chrono_to_rfc)
                    .unwrap_or_default(),
                session_id: None,
                pinned: false,
            },
            &mut nodes,
            &mut node_ids,
            &mut truncated,
        );
    }

    Ok(Json(GraphResponse {
        nodes,
        edges,
        stats: overview(&state, backend.as_ref()),
        truncated,
    }))
}

fn chrono_to_rfc(_t: std::time::SystemTime) -> String {
    // SystemTime → RFC3339 via chrono (unix ts round-trip).
    chrono::DateTime::<chrono::Utc>::from(_t).to_rfc3339()
}

/// GET /api/memory/item?kind=&id= — full content for the detail view.
pub async fn get_memory_item(
    State(state): State<HttpServerState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<MemoryItemResponse>, HttpError> {
    let kind = params.get("kind").cloned().unwrap_or_default();
    let id = params.get("id").cloned().unwrap_or_default();
    if kind.is_empty() || id.is_empty() {
        return Err(http_err(400, "缺少 kind 或 id 参数"));
    }
    match kind.as_str() {
        "fact" | "turn" | "memory_tool" | "session" | "delegation"
        | "compression_backfill" | "focus" | "reflection" => {
            resolve_db_item(&state, &kind, &id)
        }
        "daily" => {
            let date = id.strip_prefix("daily:").unwrap_or(&id);
            let store = hermix_tools::tools::daily_memory::DailyMemoryStore::open()
                .map_err(|e| http_err(500, &format!("打开 daily_memory 失败: {e}")))?;
            let entry = store
                .get(date)
                .map_err(|e| http_err(404, &format!("当日记忆不存在: {e}")))?;
            Ok(MemoryItemResponse {
                id,
                kind: "daily".into(),
                content: serde_json::to_string_pretty(&serde_json::json!({
                    "date": entry.date,
                    "goals": entry.goals,
                    "progress": entry.progress,
                    "conclusions": entry.conclusions,
                }))
                .unwrap_or_default(),
                created_at: entry.date,
                salience: 0.0,
                session_id: None,
                file: None,
            })
        }
        "md" => {
            // id = md:<uuid>@<index>
            let (key, idx) = id
                .strip_prefix("md:")
                .and_then(|rest| rest.split_once('@'))
                .ok_or_else(|| http_err(400, "md 节点 id 格式错误"))?;
            let index: usize = idx.parse().map_err(|_| http_err(400, "md 索引无效"))?;
            let path = md_registry()
                .lock()
                .ok()
                .and_then(|r| r.get(key).cloned())
                .ok_or_else(|| http_err(404, "md 条目已失效，请刷新图谱"))?;
            ensure_under_home(&state, &path)?;
            let content = std::fs::read_to_string(&path)
                .map_err(|e| http_err(500, &format!("读取失败: {e}")))?;
            let entries = hermix_tools::backends::memory::FileMemoryBackend::parse_entries(&content);
            let entry = entries.get(index).ok_or_else(|| http_err(404, "条目序号越界，请刷新图谱"))?;
            Ok(MemoryItemResponse {
                id,
                kind: "md".into(),
                content: entry.clone(),
                created_at: path
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .map(chrono_to_rfc)
                    .unwrap_or_default(),
                salience: 0.0,
                session_id: None,
                file: Some(path.display().to_string()),
            })
        }
        other => Err(http_err(400, &format!("未知 kind: {other}"))),
    }
    .map(Json)
}

fn resolve_db_item(
    state: &HttpServerState,
    kind: &str,
    id: &str,
) -> Result<MemoryItemResponse, HttpError> {
    let backend = memory_backend(state).ok_or_else(|| http_err(503, "memory.db 尚未创建"))?;
    if let Some(rest) = id.strip_prefix("fact:") {
        let facts = backend.ui_list_facts(5000).unwrap_or_default();
        let row = facts.iter().find(|r| r.id == rest).ok_or_else(|| http_err(404, "记忆不存在"))?;
        return Ok(MemoryItemResponse {
            id: id.to_string(),
            kind: "fact".into(),
            content: serde_json::to_string_pretty(&serde_json::json!({
                "key": row.key,
                "payload": serde_json::from_str::<serde_json::Value>(&row.text).unwrap_or(serde_json::Value::String(row.text.clone())),
            }))
            .unwrap_or_else(|_| row.text.clone()),
            created_at: row.created_at.clone(),
            salience: row.salience,
            session_id: row.session_id.clone(),
            file: None,
        });
    }
    if let Some(rest) = id.strip_prefix("frame:") {
        let frames = backend.ui_list_frames(5000).unwrap_or_default();
        let row = frames.iter().find(|r| r.id == rest).ok_or_else(|| http_err(404, "记忆不存在"))?;
        return Ok(MemoryItemResponse {
            id: id.to_string(),
            kind: row.kind.clone(),
            content: serde_json::to_string_pretty(
                &serde_json::from_str::<serde_json::Value>(&row.text)
                    .unwrap_or(serde_json::Value::String(row.text.clone())),
            )
            .unwrap_or_else(|_| row.text.clone()),
            created_at: row.created_at.clone(),
            salience: row.salience,
            session_id: row.session_id.clone(),
            file: None,
        });
    }
    if let Some(rest) = id.strip_prefix("refl:") {
        let rows = backend.ui_list_reflections(5000).unwrap_or_default();
        let row = rows.iter().find(|r| r.id == rest).ok_or_else(|| http_err(404, "记忆不存在"))?;
        return Ok(MemoryItemResponse {
            id: id.to_string(),
            kind: row.kind.clone(),
            content: row.text.clone(),
            created_at: row.created_at.clone(),
            salience: row.salience,
            session_id: row.session_id.clone(),
            file: None,
        });
    }
    Err(http_err(400, &format!("未知 id 前缀: {kind}")))
}

/// DELETE /api/memory/item?kind=&id= — soft-delete (reversible at
/// the SQL level), matching the agent's own cleanup semantics.
pub async fn delete_memory_item(
    State(state): State<HttpServerState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let kind = params.get("kind").cloned().unwrap_or_default();
    let id = params.get("id").cloned().unwrap_or_default();
    let backend = memory_backend(&state).ok_or_else(|| http_err(503, "memory.db 尚未创建"))?;
    let (pfx, rest) = id.split_once(':').ok_or_else(|| http_err(400, "id 格式错误"))?;
    let deleted = match (kind.as_str(), pfx) {
        ("fact", "fact") => backend
            .ui_soft_hide_fact(rest)
            .map_err(|e| http_err(500, &format!("删除失败: {e}")))?,
        ("reflection", "refl") => {
            let rowid: i64 = rest.parse().map_err(|_| http_err(400, "reflection id 无效"))?;
            backend
                .ui_soft_hide_reflection(rowid)
                .map_err(|e| http_err(500, &format!("删除失败: {e}")))?
        }
        (k, "frame") if k != "fact" && k != "reflection" && k != "daily" && k != "md" => {
            let uuid = uuid::Uuid::parse_str(rest).map_err(|_| http_err(400, "frame id 无效"))?;
            backend
                .soft_hide_frame(uuid)
                .map_err(|e| http_err(500, &format!("删除失败: {e}")))?
        }
        ("daily" | "md", _) => {
            return Err(http_err(400, "该类记忆请在对应文本编辑器中修改"));
        }
        _ => return Err(http_err(400, "kind 与 id 不匹配")),
    };
    Ok(Json(serde_json::json!({ "deleted": deleted })))
}

/// GET /api/memory/search?q= — FTS over facts + LIKE over frames /
/// reflections, plus a substring pass over global md entries.
pub async fn search_memory(
    State(state): State<HttpServerState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let q = params.get("q").cloned().unwrap_or_default();
    let limit: u32 = params
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(30)
        .clamp(1, 200);
    let mut results: Vec<GraphNode> = Vec::new();
    if !q.trim().is_empty() {
        if let Some(b) = memory_backend(&state) {
            for row in b.ui_search(q.trim(), limit).unwrap_or_default() {
                let prefix = if row.kind == "fact" {
                    "fact:"
                } else if row.kind.starts_with("reflection") {
                    "refl:"
                } else {
                    "frame:"
                };
                results.push(row_to_graph_node(prefix, &row));
            }
        }
        let ql = q.to_lowercase();
        for (file, index, entry) in md_entries(&state) {
            if results.len() >= limit as usize {
                break;
            }
            if entry.to_lowercase().contains(&ql) {
                let id = md_node_id(&file, index).unwrap_or_default();
                results.push(GraphNode {
                    label: truncate_chars(&entry, 48),
                    preview: truncate_chars(&entry, 200),
                    salience: 0.7,
                    id,
                    kind: "md".into(),
                    created_at: String::new(),
                    session_id: None,
                    pinned: false,
                });
            }
        }
    }
    Ok(Json(serde_json::json!({ "results": results })))
}

/// GET /api/memory/daily?date= 或 ?limit=60
pub async fn get_daily_memory(
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let store = hermix_tools::tools::daily_memory::DailyMemoryStore::open()
        .map_err(|e| http_err(500, &format!("打开 daily_memory 失败: {e}")))?;
    if let Some(date) = params.get("date") {
        let entry = store
            .get(date)
            .map_err(|e| http_err(404, &format!("当日记忆不存在: {e}")))?;
        return Ok(Json(serde_json::to_value(&entry).unwrap_or_default()));
    }
    let limit: u32 = params
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(60)
        .clamp(1, 365);
    let days = store
        .list_days(limit)
        .map_err(|e| http_err(500, &format!("读取 daily_memory 失败: {e}")))?;
    Ok(Json(serde_json::to_value(&days).unwrap_or_default()))
}

/// GET /api/memory/overview — counts for the page header row.
pub async fn get_memory_overview(
    State(state): State<HttpServerState>,
) -> Result<Json<MemoryOverview>, HttpError> {
    Ok(Json(overview(&state, memory_backend(&state).as_ref())))
}

/// POST /api/memory/pin — {id: "frame:<uuid>", pinned: bool}.
pub async fn pin_memory(
    State(state): State<HttpServerState>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or_default();
    let pinned = body.get("pinned").and_then(|v| v.as_bool()).unwrap_or(true);
    let backend = memory_backend(&state).ok_or_else(|| http_err(503, "memory.db 尚未创建"))?;
    let rest = id
        .strip_prefix("frame:")
        .ok_or_else(|| http_err(400, "只有帧类记忆支持置顶"))?;
    let uuid = uuid::Uuid::parse_str(rest).map_err(|_| http_err(400, "frame id 无效"))?;
    let changed = backend
        .ui_set_frame_pin(&uuid.to_string(), pinned)
        .map_err(|e| http_err(500, &format!("置顶失败: {e}")))?;
    Ok(Json(serde_json::json!({ "pinned": pinned, "changed": changed })))
}

// ── plumbing ───────────────────────────────────────────────────────

fn http_err(status: u16, message: &str) -> HttpError {
    HttpError {
        status: axum::http::StatusCode::from_u16(status)
            .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR),
        message: message.to_string(),
    }
}

fn ensure_under_home(state: &HttpServerState, path: &std::path::Path) -> Result<(), HttpError> {
    let home = state
        .hermix_home
        .canonicalize()
        .unwrap_or_else(|_| state.hermix_home.clone());
    let target = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if target.starts_with(&home) {
        Ok(())
    } else {
        Err(http_err(403, "路径越界"))
    }
}

// 本模块的路由注册在 `super::mod` 里逐条 `.route()` 完成（同一批
// 路径）。此处不再提供 `router()`：2026-10-18 删除的那份与 mod.rs
// 是重复定义，且从未被挂载——两份并存时改一处漏一处，正是它没被
// 任何人调用的原因。
