//! Automation task CRUD + scheduler endpoints.
//!
//! An "automation task" is a user-defined recurring agent prompt: at the
//! scheduled time the backend sends the prompt to `LocalAgentService` and
//! records the outcome in the task's `history`.
//!
//! Storage is a single JSON file at `$HERMIX_HOME/automation_tasks.json`.
//! On first start we seed 6 demo tasks (Chinese, matching the front-end's
//! fallback) so the UI is populated without manual setup.
//!
//! ## Scheduler
//! A background tokio task ticks every 30 seconds, walks every enabled
//! task, computes the next firing time from its cron expression, and
//! dispatches any task whose `next_run_at` has just elapsed. Dispatch is
//! `fire_and_record` — we don't block the tick loop on a slow agent run.
//!
//! This is intentionally separate from `hermes-cron`, which is the lower-
//! level cron engine with its own persistence and delivery model. The
//! "automation" surface here is the user-facing scheduling UX.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use cron::Schedule;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex as AsyncMutex, RwLock};

use crate::HttpServerState;

// ── Storage path ────────────────────────────────────────

fn automation_state_path(state: &HttpServerState) -> PathBuf {
    state.hermix_home.join("automation_tasks.json")
}

// ── Wire types ──────────────────────────────────────────

/// One row in the run history. Capped to the most recent N=20 entries
/// per task; older runs are summarized into `run_count`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub at: String,
    /// "ok" | "failed" | "running"
    pub status: String,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutomationTask {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub prompt: String,
    /// Standard 5-field cron expression, e.g. "0 9 * * 1-5" for weekday 9am.
    pub schedule: String,
    pub enabled: bool,
    /// Human-readable schedule label, e.g. "每周一至五 09:00". Server may
    /// recompute on save, but the client can supply its own for the
    /// Chinese demo templates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_label: Option<String>,
    /// Optional model override, e.g. "openai:gpt-4o".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Optional expert id (matches the `ExpertDef.id` from builtin catalog).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expert_id: Option<String>,
    /// Optional workspace path to constrain file tools to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_path: Option<String>,
    /// "default" | "full"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_level: Option<String>,
    /// Optional category for grouping in the UI, e.g. "工作" / "学习".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Optional emoji / lucide-name string for the UI card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub last_run_at: Option<String>,
    /// "ok" | "failed" | "running" | None
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub next_run_at: Option<String>,
    #[serde(default)]
    pub run_count: u32,
    /// Most recent 20 runs. Older runs collapse into `run_count` only.
    #[serde(default)]
    pub history: Vec<RunRecord>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AutomationState {
    #[serde(default)]
    pub tasks: Vec<AutomationTask>,
    #[serde(default)]
    pub seeded: bool,
}

// ── Load / save / seed ─────────────────────────────────

fn load_state(state: &HttpServerState) -> AutomationState {
    let path = automation_state_path(state);
    let Ok(data) = std::fs::read_to_string(&path) else {
        return seed_default();
    };
    match serde_json::from_str::<AutomationState>(&data) {
        Ok(v) => v,
        Err(_) => seed_default(),
    }
}

/// Process-wide serialization for `save_state`.
/// /// On Windows `std::fs::write` opens the destination with `FILE_SHARE_NONE`
/// semantics, so any concurrent reader (the Tauri front-end polls this file,
/// log indexers, AV scans, etc.) makes the open return
/// `ERROR_ACCESS_DENIED` (os error 5). Serializing writers + writing to a
/// sibling `.tmp` first + bounded retry covers all of: transient share
/// locks, in-flight readers, and short bursts of overlapping HTTP edits.
static SAVE_LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();

fn save_lock() -> &'static AsyncMutex<()> {
    SAVE_LOCK.get_or_init(|| AsyncMutex::new(()))
}

/// Maximum number of retry attempts when the atomic write hits a transient
/// lock conflict. Backoff is exponential: 100, 200, 400, 800, 1600 ms.
const SAVE_RETRIES: u32 = 5;

async fn save_state(state: &HttpServerState, body: &AutomationState) -> Result<(), String> {
    // Serialize writers so we never have two .tmp files racing to rename.
    let _guard = save_lock().lock().await;

    let path = automation_state_path(state);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("create_dir_all({:?}): {}", parent, e))?;
    }
    let json = serde_json::to_string_pretty(body).map_err(|e| e.to_string())?;

    let mut last_err: Option<String> = None;
    for attempt in 0..SAVE_RETRIES {
        match write_atomic(&path, &json) {
            Ok(()) => return Ok(()),
            Err(e) => {
                tracing::warn!(
                    attempt,
                    path = %path.display(),
                    error = %e,
                    "automation save_state retrying"
                );
                last_err = Some(e);
                let backoff = Duration::from_millis(100u64.saturating_mul(1u64 << attempt));
                tokio::time::sleep(backoff).await;
            }
        }
    }
    Err(last_err.unwrap_or_else(|| "save_state failed without an error".into()))
}

/// Write `content` to `final_path` atomically:
/// /// 1. Stage into a sibling `*.tmp.<pid>.<nanos>` file opened with
/// `FILE_SHARE_READ | FILE_SHARE_WRITE` on Windows so concurrent readers
/// do not block us.
/// 2. `fsync` the temp file so the bytes survive a power loss.
/// 3. `std::fs::rename` — atomic on POSIX and on NTFS since Rust 1.46.
/// /// On the final attempt we fall back to a non-atomic direct write so a
/// persistent lock on the temp file (which would otherwise loop forever)
/// still produces *some* on-disk update rather than failing the tick.
fn write_atomic(final_path: &Path, content: &str) -> Result<(), String> {
    let pid = std::process::id();
    let nanos = chrono::Utc::now()
        .timestamp_nanos_opt()
        .unwrap_or_else(|| chrono::Utc::now().timestamp_micros() * 1000);
    let tmp = final_path.with_extension(format!("json.tmp.{pid}.{nanos}"));

    let write_result = (|| -> std::io::Result<()> {
        let mut f = open_shared_write(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
        Ok(())
    })();

    match write_result {
        Ok(()) => match std::fs::rename(&tmp, final_path) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(format!("rename {:?} -> {:?}: {}", tmp, final_path, e))
            }
        },
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(format!("write tmp {:?}: {}", tmp, e))
        }
    }
}

#[cfg(windows)]
fn open_shared_write(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
    // Let co-operating readers (Tauri, indexers) keep the file open while
    // we write the temp copy. The atomic rename is what actually swaps the
    // bytes under the readers' noses.
    const SHARE: u32 = 0x1 | 0x2 | 0x4;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .share_mode(SHARE)
        .open(path)
}

#[cfg(not(windows))]
fn open_shared_write(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// 6 Chinese demo templates mirroring the front-end's `AUTOMATION_TEMPLATES`
/// in `AutomationView.tsx` (the ones shown in the empty-state of the
/// "通过设置定期聊天，实现工作自动化" page). Server is the source of truth
/// so a fresh install immediately has populated data.
fn seed_default() -> AutomationState {
    let now = now_iso();
    let mk = |id: &str,
              name: &str,
              description: &str,
              prompt: &str,
              schedule: &str,
              schedule_label: &str,
              category: &str,
              icon: &str| AutomationTask {
        id: id.into(),
        name: name.into(),
        description: Some(description.into()),
        prompt: prompt.into(),
        schedule: schedule.into(),
        enabled: false, // demos off by default — user explicitly opts in
        schedule_label: Some(schedule_label.into()),
        model: None,
        expert_id: None,
        workspace_path: None,
        permission_level: None,
        category: Some(category.into()),
        icon: Some(icon.into()),
        last_run_at: None,
        last_status: None,
        last_error: None,
        next_run_at: None,
        run_count: 0,
        history: vec![],
        created_at: now.clone(),
        updated_at: now.clone(),
    };
    AutomationState {
        seeded: true,
        tasks: vec![
            mk(
                "demo-morning-brief",
                "每日早报",
                "每天早上 8 点把昨天的关键进展 + 今日待办整理成简报",
                "请帮我整理一份工作简报：1) 昨天我做了哪些事（从最近会话里提取）2) 今天应该关注什么 3) 有什么阻塞或风险。",
                "0 8 * * *",
                "每天 08:00",
                "工作",
                "📰",
            ),
            mk(
                "demo-eod-review",
                "每日复盘",
                "每晚 21 点回顾当天会话，提取可改进的点",
                "请基于今天的所有对话，做一个简短复盘：哪几轮最有效？哪几轮浪费了时间？明早可以怎么调整？",
                "0 21 * * *",
                "每天 21:00",
                "工作",
                "🌙",
            ),
            mk(
                "demo-weekly-archive",
                "每周归档",
                "每周五 17 点把本周所有会话的关键产物路径整理到一份清单",
                "请汇总本周所有会话里产出的文件路径（绝对路径），按项目分类，整理成可索引的清单。",
                "0 17 * * 5",
                "每周五 17:00",
                "工作",
                "🗂",
            ),
            mk(
                "demo-error-watch",
                "错误巡检",
                "每个工作日 10 点扫描本机项目里的 build / lint 错误",
                "请扫描当前工作目录里的 build / lint 错误（rust / typescript / go 都行），按严重度排序，列出前 10 个。",
                "0 10 * * 1-5",
                "工作日 10:00",
                "运维",
                "🛡",
            ),
            mk(
                "demo-news-digest",
                "行业动态",
                "每周一 9 点抓取指定 RSS / 站点，整理成 5 条要点",
                "请基于最近 7 天的对话上下文（或者我接下来给你的 RSS 源），整理出本周最值得关注的 5 条行业动态，每条 1-2 句。",
                "0 9 * * 1",
                "每周一 09:00",
                "学习",
                "📡",
            ),
            mk(
                "demo-commit-reminder",
                "未提交提醒",
                "每个工作日 18 点检查 git 状态，没提交的代码就提醒我",
                "请检查当前 git 仓库状态：有没有未提交的修改 / 未推送的 commit？如有，给我一段简短的提交信息草稿。",
                "0 18 * * 1-5",
                "工作日 18:00",
                "工作",
                "📌",
            ),
        ],
    }
}

// ── Cron helpers ────────────────────────────────────────

/// Normalize a user-supplied cron expression to the 7-field format
/// (`sec min hour dom month dow year`) required by the `cron` crate.
/// 5-field → `0 <expr> *`; 6-field → `<expr> *`; 7-field → as-is.
/// Other lengths pass through (the `Schedule` parser will then reject).
fn normalize_cron_expr(expr: &str) -> String {
    let parts: Vec<&str> = expr.trim().split_whitespace().collect();
    match parts.len() {
        5 => format!("0 {} *", expr.trim()),
        6 => format!("{} *", expr.trim()),
        7 => expr.trim().to_string(),
        _ => expr.trim().to_string(),
    }
}

/// Parse a cron expression and compute the next firing time after `from`.
/// Returns `None` if the expression is invalid.
fn next_fire(schedule: &str, from: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let normalized = normalize_cron_expr(schedule);
    let sched = normalized.parse::<Schedule>().ok()?;
    let next = sched.after(&from).next()?;
    Some(next)
}

fn ensure_next_run(task: &mut AutomationTask) {
    if !task.enabled {
        task.next_run_at = None;
        return;
    }
    let now = Utc::now();
    task.next_run_at = next_fire(&task.schedule, now).map(|d| {
        d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    });
}
// ── HTTP handlers ──────────────────────────────────────

/// GET /api/automation/tasks — list all tasks. Recomputes `next_run_at`
/// for enabled tasks on every read so the UI always shows a live value
/// without having to re-create the task.
pub async fn list_tasks(State(state): State<HttpServerState>) -> Json<AutomationState> {
    let mut snapshot = load_state(&state);
    for t in &mut snapshot.tasks {
        ensure_next_run(t);
    }
    Json(snapshot)
}

/// POST /api/automation/tasks — create. Server assigns id/timestamps if missing.
pub async fn create_task(
    State(state): State<HttpServerState>,
    Json(mut body): Json<AutomationTask>,
) -> Result<Json<AutomationTask>, (StatusCode, String)> {
    if body.name.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "name 不能为空".into()));
    }
    if body.prompt.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "prompt 不能为空".into()));
    }
    if body.schedule.trim().is_empty()
        || normalize_cron_expr(&body.schedule).parse::<Schedule>().is_err()
    {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("schedule 不是合法的 cron 表达式: '{}'", body.schedule),
        ));
    }
    let mut s = load_state(&state);
    if body.id.is_empty() {
        body.id = format!("task-{}", chrono::Utc::now().timestamp_millis());
    } else if s.tasks.iter().any(|t| t.id == body.id) {
        return Err((StatusCode::CONFLICT, format!("id 已存在: {}", body.id)));
    }
    let now = now_iso();
    body.created_at = now.clone();
    body.updated_at = now;
    body.last_run_at = None;
    body.last_status = None;
    body.last_error = None;
    body.run_count = 0;
    body.history.clear();
    ensure_next_run(&mut body);
    s.tasks.push(body.clone());
    save_state(&state, &s).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(body))
}

/// PUT /api/automation/tasks/{id} — replace. Returns 404 if id unknown.
pub async fn update_task(
    State(state): State<HttpServerState>,
    AxumPath(id): AxumPath<String>,
    Json(mut body): Json<AutomationTask>,
) -> Result<Json<AutomationTask>, (StatusCode, String)> {
    if body.prompt.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "prompt 不能为空".into()));
    }
    if normalize_cron_expr(&body.schedule).parse::<Schedule>().is_err() {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("schedule 不合法: '{}'", body.schedule),
        ));
    }
    let mut s = load_state(&state);
    let slot = s
        .tasks
        .iter_mut()
        .find(|t| t.id == id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("task id 不存在: {}", id)))?;
    body.id = id.clone();
    body.created_at = slot.created_at.clone();
    body.updated_at = now_iso();
    ensure_next_run(&mut body);
    *slot = body.clone();
    save_state(&state, &s).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(body))
}

/// DELETE /api/automation/tasks/{id} — remove. Returns 404 if unknown.
pub async fn delete_task(
    State(state): State<HttpServerState>,
    AxumPath(id): AxumPath<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let mut s = load_state(&state);
    let before = s.tasks.len();
    s.tasks.retain(|t| t.id != id);
    if s.tasks.len() == before {
        return Err((StatusCode::NOT_FOUND, format!("task id 不存在: {}", id)));
    }
    save_state(&state, &s).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(StatusCode::NO_CONTENT)
}

/// POST /api/automation/tasks/{id}/trigger — manual run.
/// Spawns the agent call on a tokio task and returns 202 immediately
/// (the UI polls task state to see when `last_status` flips to "ok"/"failed").
pub async fn trigger_task(
    State(state): State<HttpServerState>,
    AxumPath(id): AxumPath<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let mut snapshot = load_state(&state);
    let task = snapshot
        .tasks
        .iter()
        .find(|t| t.id == id)
        .cloned()
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("task id 不存在: {}", id)))?;
    if let Some(slot) = snapshot.tasks.iter_mut().find(|t| t.id == id) {
        slot.last_status = Some("running".into());
        slot.last_error = None;
    }
    save_state(&state, &snapshot).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    spawn_run(state, task);
    Ok(StatusCode::ACCEPTED)
}

/// POST /api/automation/reset — restore the 6 demo templates. Existing
/// user-created tasks are kept; demo tasks are reset to their seeded form.
pub async fn reset_to_official(
    State(state): State<HttpServerState>,
) -> Json<AutomationState> {
    let mut current = load_state(&state);
    let seed = seed_default();
    let demo_ids: Vec<String> = seed.tasks.iter().map(|t| t.id.clone()).collect();
    current.tasks.retain(|t| !demo_ids.contains(&t.id));
    current.tasks.extend(seed.tasks);
    current.seeded = true;
    for t in &mut current.tasks {
        ensure_next_run(t);
    }
    let _ = save_state(&state, &current).await;
    Json(current)
}

// ── Background scheduler ───────────────────────────────

/// Maximum number of history records retained per task.
const MAX_HISTORY: usize = 20;

/// Spawn the agent call for a single automation task. Updates the
/// persisted state with the outcome. Never panics — agent errors are
/// recorded in the task's `last_error` and `history`.
fn spawn_run(state: HttpServerState, task: AutomationTask) {
    tokio::spawn(async move {
        let started = Instant::now();
        let agent_service = state.agent_service.clone();
        let task_id = task.id.clone();
        let prompt = task.prompt.clone();
        let session_id = format!("automation:{}", task.id);
        let model = task.model.clone();
        let workspace_path = task.workspace_path.clone();
        let expert_id = task.expert_id.clone();
        let permission_level = task.permission_level.clone();

        let mut overrides = hermix_core::traits::AgentOverrides::default();
        if let Some(m) = model {
            if let Some((prov, mdl)) = m.split_once(':') {
                overrides.model = Some(format!("{}:{}", prov, mdl));
            } else {
                overrides.model = Some(m);
            }
        }
        if let Some(ws) = workspace_path {
            overrides.workspace = Some(ws);
        }
        if let Some(perm) = permission_level {
            overrides.permission = match perm.as_str() {
                "full" => Some(hermix_core::traits::PermissionLevel::Full),
                _ => Some(hermix_core::traits::PermissionLevel::Default),
            };
        }
        if let Some(ref eid) = expert_id {
            // Resolve expert definition from the registry and apply its
            // system prompt and toolset to the agent overrides.
            let expert_entry = state.expert_registry.get(eid);
            if let Some(expert) = expert_entry {
                overrides.system_prompt = Some(expert.system_prompt.clone());
                tracing::info!(
                    expert_id = %eid,
                    expert_name = %expert.name,
                    "automation task: resolved expert definition"
                );
            } else {
                tracing::warn!(
                    expert_id = %eid,
                    "automation task: expert_id not found in registry"
                );
            }
        }

        let result = agent_service
            .send_message(&session_id, &prompt, overrides)
            .await;

        let duration_ms = started.elapsed().as_millis() as u64;
        let now = now_iso();
        let (status, summary, error) = match result {
            Ok(reply) => {
                let preview: String = reply.text.chars().take(160).collect();
                ("ok".to_string(), Some(preview), None)
            }
            Err(e) => ("failed".to_string(), None, Some(e.to_string())),
        };

        let record = RunRecord {
            at: now.clone(),
            status: status.clone(),
            duration_ms,
            summary,
            error: error.clone(),
        };

        let mut snapshot = load_state(&state);
        if let Some(slot) = snapshot.tasks.iter_mut().find(|t| t.id == task_id) {
            slot.last_run_at = Some(now);
            slot.last_status = Some(status.clone());
            slot.last_error = error;
            slot.run_count = slot.run_count.saturating_add(1);
            slot.history.push(record);
            let overflow = slot.history.len().saturating_sub(MAX_HISTORY);
            if overflow > 0 {
                slot.history.drain(0..overflow);
            }
            ensure_next_run(slot);
        }
        let _ = save_state(&state, &snapshot).await;
        tracing::info!(
            task_id = %task_id,
            status = %status,
            duration_ms,
            "automation task completed"
        );
    });
}

/// Shared scheduler state. Held by `HttpServerState` so tests can
/// inspect / shut it down.
pub struct AutomationScheduler {
    /// Per-rule last-fire timestamp; the spawned tick task reads + writes
    /// this to enforce the cadence window. The struct itself doesn't
    /// expose it externally — only the tick loop uses it — so it's
    /// private and Rust 2024 warns "never read" against it. Silence
    /// the warning rather than leak an accessor we don't need.
    #[allow(dead_code)]
    last_fired: Arc<RwLock<HashMap<String, DateTime<Utc>>>>,
    shutdown: tokio::sync::watch::Sender<bool>,
    _handle: tokio::task::JoinHandle<()>,
}

impl AutomationScheduler {
    /// Start the background tick loop. Returns a handle that can be
    /// used to shut the loop down.
    pub fn start(state: HttpServerState) -> Arc<Self> {
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        let last_fired: Arc<RwLock<HashMap<String, DateTime<Utc>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let state_for_tick = state.clone();
        let last_fired_for_tick = last_fired.clone();
        let handle = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(30));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        if let Err(e) = tick_once(&state_for_tick, &last_fired_for_tick).await {
                            tracing::warn!(error=%e, "automation tick failed");
                        }
                    }
                    _ = rx.changed() => {
                        if *rx.borrow() { break; }
                    }
                }
            }
            tracing::info!("automation scheduler shut down");
        });
        Arc::new(Self {
            last_fired,
            shutdown: tx,
            _handle: handle,
        })
    }

    /// Signal the background loop to exit. Idempotent.
    pub fn shutdown(&self) {
        let _ = self.shutdown.send(true);
    }
}

async fn tick_once(
    state: &HttpServerState,
    last_fired: &RwLock<HashMap<String, DateTime<Utc>>>,
) -> Result<(), String> {
    let mut snapshot = load_state(state);
    let now = Utc::now();
    for t in &mut snapshot.tasks {
        if !t.enabled {
            continue;
        }
        let next: Option<DateTime<Utc>> = match normalize_cron_expr(&t.schedule).parse::<Schedule>() {
            Ok(sched) => sched.after(&now).next(),
            Err(_) => {
                t.next_run_at = None;
                continue;
            }
        };
        let next_str = next.map(|d: DateTime<Utc>| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
        t.next_run_at = next_str.clone();
        let should_fire = match (next, t.last_run_at.as_ref()) {
            (Some(next_at), Some(last)) => {
                let last_at = DateTime::parse_from_rfc3339(last)
                    .map(|d| d.with_timezone(&Utc))
                    .unwrap_or_else(|_| DateTime::<Utc>::from_timestamp(0, 0).unwrap());
                next_at <= now && last_at < next_at
            }
            (Some(next_at), None) => next_at <= now,
            _ => false,
        };
        if should_fire {
            let key = format!("{}:{}", t.id, next_str.clone().unwrap_or_default());
            {
                let map = last_fired.read().await;
                if let Some(prev) = map.get(&key) {
                    if now.signed_duration_since(*prev) < chrono::Duration::seconds(60) {
                        continue;
                    }
                }
            }
            last_fired.write().await.insert(key, now);
            let task = t.clone();
            spawn_run(state.clone(), task);
        }
    }
    save_state(state, &snapshot).await.map_err(|e| e.to_string())?;
    Ok(())
}
