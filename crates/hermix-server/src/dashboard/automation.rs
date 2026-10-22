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

use async_trait::async_trait;
use hermix_automation::{decide_delivery_target, DeliveryGuardInput, DeliveryTarget};
use hermix_core::traits::ExternalDelivery;
use hermix_tools::tools::cronjob::CronjobBackend;

// ── Storage path ────────────────────────────────────────

/// 自动化每轮送给模型的**历史预算**（估算 token，不是 `chars/4`）。
///
/// 定时任务每轮都往同一个会话追加消息，而上下文压缩要等到逼近模型窗口
/// （本项目用的 MiniMax 免费档窗口是 1M token）才触发 —— 于是前面每一轮都在
/// 为越来越长的历史付费。实测一条跑 41 轮的刷课任务：历史累积到 3,718 条
/// 消息 / 6.1M 字符，单次请求长到 ~1M token，而系统提示只占其中 1%。
///
/// 这个预算把「送进模型的历史」限制在最近约 4 轮：够 agent 接着上一轮的
/// 状态往下做（它本来每轮就会重读课程目录），又不会随轮次线性膨胀。
///
/// 用**估算 token**而不是消息条数：工具调用参数（JSON）才是大头，按条数
/// 裁不准。
pub const AUTOMATION_HISTORY_TOKEN_BUDGET: usize = 60_000;

/// 环境变量名：覆盖上面的默认预算。
///
/// 预算直接决定**每次请求要付多少钱**：实测成本 ≈ 窗口大小 × 未命中率 ×
/// 调用次数（窗口 60K token、未命中 10%、每轮 67 次调用 ≈ 每轮 40 万
/// 计费 token）。调参需要反复对比不同预算，而每次改常量都要重编 sidecar
/// —— 这条变量把「重编」从实验回路里拿掉。
pub const AUTOMATION_HISTORY_TOKEN_BUDGET_ENV: &str = "HERMIX_AUTOMATION_HISTORY_TOKEN_BUDGET";

/// 当前生效的历史预算：环境变量优先，否则用默认值。
///
/// 下限 1,000 token —— 比这更小的窗口连一轮对话都放不下，与其静默
/// 产生一个必然失败的请求，不如回落到默认值。
pub fn automation_history_token_budget() -> usize {
    std::env::var(AUTOMATION_HISTORY_TOKEN_BUDGET_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v >= 1_000)
        .unwrap_or(AUTOMATION_HISTORY_TOKEN_BUDGET)
}

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
    /// 任务级技能名（前端编辑器勾选）。执行时经 `AgentOverrides::skills`
    /// 以 ephemeral 技能注入——内容强制进提示词，不受全局技能启停影响。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    /// 有限次任务：累计运行 run_count 次数跑满后自动停用。None = 无限循环
    /// （对齐 ZCode 的 recurring / maxRuns 语义）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_runs: Option<u32>,
    /// Optional category for grouping in the UI, e.g. "工作" / "学习".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Optional emoji / lucide-name string for the UI card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    /// "ok" | "failed" | "running" | None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<String>,
    #[serde(default)]
    pub run_count: u32,
    /// Most recent 20 runs. Older runs collapse into `run_count` only.
    #[serde(default)]
    pub history: Vec<RunRecord>,
    /// 正在跑的那一轮的**开始时刻**（RFC3339）。
    ///
    /// 用来做**单飞**：上一次跑还没结束（跑得比调度间隔还长）时，下一个槽
    /// 不能接着再起一个 —— 两个 agent 同时驱动同一个目标会互相踩
    /// （2026-10-21 超星刷课就踩过：一个 12 分钟的活配 15 分钟的间隔，
    /// 中途的 tick 会再触发一次）。
    ///
    /// 设了超过 [`RUNNING_STALE_MINUTES`] 还没清，就当那次进程死了、允许重跑。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running_since: Option<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    /// Conversation this job was created in. Set when the `cronjob`
    /// tool ran inside a chat (the registry stamps `_session_id` into
    /// tool params); `None` for jobs created from this UI. The
    /// scheduler routes results back to the anchored conversation
    /// instead of a private `automation:{id}` session. Stored in the
    /// automation row; the routing branch lands with the schedule
    /// model upgrade.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_session_id: Option<String>,
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
    let mut parsed: AutomationState = match serde_json::from_str::<AutomationState>(&data) {
        Ok(v) => v,
        Err(_) => seed_default(),
    };
    refresh_builtin_demos(&mut parsed);
    parsed
}

/// 升级映射：`(旧提示词, 新示例 id)`。
///
/// 示例是内置的（不是用户数据），升级后应当跟着换 —— 旧示例（读会话历史 /
/// 扫工作目录 / 看 git 状态）即使被用户启用，也会去翻用户自己的数据。
/// 提示词命中这里 = 用户没动过它 → 可以安全刷新成新定义；
/// 不命中 = 用户自己改过 → 原样保留，不覆盖。
///
/// 按**旧提示词**而不是 id 匹配：这次换版连 id 一起换了（`demo-morning-brief`
/// → `demo-daily-weather`），按 id 匹配会一条都刷不到。
const RETIRED_DEMO_MAP: &[(&str, &str)] = &[
    (
        "请帮我整理一份工作简报：1) 昨天我做了哪些事（从最近会话里提取）2) 今天应该关注什么 3) 有什么阻塞或风险。",
        "demo-daily-weather",
    ),
    (
        "请基于今天的所有对话，做一个简短复盘：哪几轮最有效？哪几轮浪费了时间？明早可以怎么调整？",
        "demo-daily-news",
    ),
    (
        "请汇总本周所有会话里产出的文件路径（绝对路径），按项目分类，整理成可索引的清单。",
        "demo-daily-term",
    ),
    (
        "请扫描当前工作目录里的 build / lint 错误（rust / typescript / go 都行），按严重度排序，列出前 10 个。",
        "demo-weekly-oss",
    ),
    (
        "请基于最近 7 天的对话上下文（或者我接下来给你的 RSS 源），整理出本周最值得关注的 5 条行业动态，每条 1-2 句。",
        "demo-weekly-courses",
    ),
    (
        "请检查当前 git 仓库状态：有没有未提交的修改 / 未推送的 commit？如有，给我一段简短的提交信息草稿。",
        "demo-daily-fact",
    ),
];

/// 把内置示例任务刷新到当前定义。
///
/// 存量 JSON 里的示例是上一版留下的（读会话历史 / 扫工作目录 / 看 git 状态）
/// —— 那些即使被启用也会去翻用户的数据。示例属于**内置内容**，不是用户数据，
/// 所以升级时应当跟着换；只有用户**自己改过**提示词的示例才保留原样。
///
/// 用户的开关状态保留：用户把某个示例打开过，刷新不该把它关掉。
fn refresh_builtin_demos(state: &mut AutomationState) {
    let builtin = seed_default();
    for task in state.tasks.iter_mut() {
        let Some((_, new_id)) = RETIRED_DEMO_MAP
            .iter()
            .find(|(old_prompt, _)| *old_prompt == task.prompt)
        else {
            // 用户改过（或本来就不是内置示例）→ 不动。
            continue;
        };
        let Some(def) = builtin.tasks.iter().find(|d| d.id == *new_id) else {
            continue;
        };
        let enabled = task.enabled;
        *task = def.clone();
        task.enabled = enabled;
    }
}

/// Process-wide serialization for `save_state`.
/// On Windows `std::fs::write` opens the destination with `FILE_SHARE_NONE`
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
/// 1. Stage into a sibling `*.tmp.<pid>.<nanos>` file opened with
/// `FILE_SHARE_READ | FILE_SHARE_WRITE` on Windows so concurrent readers
/// do not block us.
/// 2. `fsync` the temp file so the bytes survive a power loss.
/// 3. `std::fs::rename` — atomic on POSIX and on NTFS since Rust 1.46.
/// On the final attempt we fall back to a non-atomic direct write so a
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
        skills: None,
        max_runs: None,
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
        running_since: None,
        created_at: now.clone(),
        updated_at: now.clone(),
        // 内置示例不属于任何对话，无锚点。
        anchor_session_id: None,
    };
    AutomationState {
        seeded: true,
        tasks: vec![
            mk(
                "demo-daily-weather",
                "每日天气",
                "每天早上 8 点查一次公开天气信息，出一行简报",
                "请查一下今天（{date}）的公开天气信息：城市、天气状况、最高/最低气温、是否有降水或大风预警。用两三句话说清楚，附上信息来源链接。只查询公开信息，不要读取或修改任何本地文件、会话历史或配置。",
                "0 8 * * *",
                "每天 08:00",
                "生活",
                "🌤️",
            ),
            mk(
                "demo-daily-news",
                "每日要闻",
                "每天中午 12 点汇总 5 条公开科技要闻",
                "请汇总今天（{date}）公开可查的科技领域要闻 5 条，每条 1-2 句，附来源链接；重复的要合并。只查询公开信息，不要读取或修改任何本地文件、会话历史或配置。",
                "0 12 * * *",
                "每天 12:00",
                "学习",
                "📰",
            ),
            mk(
                "demo-daily-term",
                "每日一词",
                "每天晚上 21 点解释一个公开的计算机/管理领域名词",
                "请挑一个计算机或管理领域的常用名词，用 3-4 句讲清楚它是什么、解决什么问题、举一个生活化的例子。只查询公开信息，不要读取或修改任何本地文件、会话历史或配置。",
                "0 21 * * *",
                "每天 21:00",
                "学习",
                "📖",
            ),
            mk(
                "demo-weekly-oss",
                "每周开源速览",
                "每周一 9 点汇总本周公开的热门开源项目",
                "请汇总最近一周公开可查的热门开源项目 5 个：项目名、做什么的、star 数或热度、仓库链接。只查询公开信息，不要读取或修改任何本地文件、会话历史或配置。",
                "0 9 * * 1",
                "每周一 09:00",
                "学习",
                "🧩",
            ),
            mk(
                "demo-weekly-courses",
                "每周公开课",
                "每周五 17 点推荐 3 门公开的免费公开课",
                "请推荐 3 门公开可免费旁听的优质公开课（大学课程或技术课程均可），说明讲什么、适合谁、在哪能上，附链接。只查询公开信息，不要读取或修改任何本地文件、会话历史或配置。",
                "0 17 * * 5",
                "每周五 17:00",
                "学习",
                "🎓",
            ),
            mk(
                "demo-daily-fact",
                "每日冷知识",
                "每个工作日 10 点讲一个公开的常识/科普点",
                "请讲一个有意思的常识或科普点：一个反直觉的事实、一个小知识、或者一件历史趣闻，讲清楚为什么反直觉。只查询公开信息，不要读取或修改任何本地文件、会话历史或配置。",
                "0 10 * * 1-5",
                "工作日 10:00",
                "生活",
                "💡",
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

/// 一轮跑多久之后，`running_since` 就当成陈旧（进程死了）而允许重跑。
///
/// 取值要**明显大于一次正常运行的时长**（超星刷课一节 12 分钟），否则会把
/// 还在跑的任务误判成死了、并发再起一个。
const RUNNING_STALE_MINUTES: i64 = 120;

/// 「从没跑过」时，锚点回退到「多久之前」（分钟）。
///
/// 必须大于调度 tick 的间隔（30s）并留出余量：tick 在槽位时刻之后到达才算
/// 「已发生」。回退量太窄会漏掉刚过去的槽（表现为任务偶尔不跑），
/// 太宽则可能把上一槽重复算成新槽（每槽一次的去重会挡住它，所以不致命）。
const DUE_LOOKBACK_MINUTES: i64 = 2;

/// 解析 RFC3339 字符串为 UTC 时间；解析不了返回 `None`（而不是 epoch——
/// epoch 会让「比上次新」的比较永远成立，等于去重失效）。
fn parse_rfc3339_utc(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// 「这一轮是不是还在跑」——单飞护栏的纯判定。
///
/// 上一轮没结束就不该再起一轮：跑得比调度间隔长时（超星刷课一节 12 分钟配
/// 15 分钟的槽），中途的 tick 会再触发一次，两个 agent 同时驱动同一个目标。
/// 超过 [`RUNNING_STALE_MINUTES`] 还没清，就当那次进程死了、允许重跑。
fn is_run_in_flight(task: &AutomationTask, now: DateTime<Utc>) -> bool {
    match task.running_since.as_deref().and_then(parse_rfc3339_utc) {
        Some(since) => {
            now.signed_duration_since(since) < chrono::Duration::minutes(RUNNING_STALE_MINUTES)
        }
        None => false,
    }
}

/// 「这一槽该不该跑」的纯判定。
///
/// 返回 `Some(slot)` = 该跑了，`slot` 是本次要认领的时刻（**从上次跑完之后
/// 起的第一个发生**）；`None` = 不该跑（还没到点 / 这一槽已经跑过）。
///
/// **为什么不能拿「下一个未来时刻」判**：cron 的 `after(&now)` 永远返回晚于
/// `now` 的下一次发生，拿它跟 `now` 比恒为假 —— 旧实现就是这么写的，结果这个
/// 调度器**从不触发任何任务**（2026-10-21 实测：任务启用后 `next_run_at` 正常
/// 推进、`run_count` 恒 0、会话里零回投）。
///
/// 正确的锚点是**上次跑完之后**：`last_run_at` 之后的第一个发生时刻，就是
/// 还没跑的那一槽。它 ≤ now 就该跑（顺带把进程停机期间错过的槽补掉一次
/// ——补跑后 `last_run_at` 变成现在，下一槽自然重新对齐，不会连补三次）。
/// 没有 `last_run_at`（从没跑过）时，锚点取「刚才」而不是无限远的过去：
/// 否则一个刚启用的 09:00 任务会立刻补跑一个它启用前就该跑的槽。
///
/// LOOKBACK 是「从没跑过」时的锚点回退量，必须大于调度 tick 的间隔（30s），
/// 否则刚过去的槽会被漏掉。
fn due_slot(schedule: &str, last_run_at: Option<&str>, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let sched = normalize_cron_expr(schedule).parse::<Schedule>().ok()?;
    let since = last_run_at
        .and_then(parse_rfc3339_utc)
        .unwrap_or_else(|| now - chrono::Duration::minutes(DUE_LOOKBACK_MINUTES));
    let slot = sched.after(&since).next()?;
    if slot > now {
        return None;
    }
    Some(slot)
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

/// GET /api/automation/active-count — how many automation runs are in flight.
///
/// The desktop keeps the machine awake while "Hermix is working". A chat
/// session can see its own streaming state, but a background automation has
/// no session the UI is watching, so activity has to be read from the run
/// table. Kept as its own endpoint (rather than a field on `list_tasks`)
/// because the poller must stay cheap: the keep-awake tick runs on a timer
/// and only needs one integer.
pub async fn active_count(State(state): State<HttpServerState>) -> Json<ActiveRunCount> {
    let active = state
        .automation_service
        .as_ref()
        .and_then(|svc| svc.count_active_runs().ok())
        .unwrap_or(0);
    Json(ActiveRunCount { active })
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Default)]
pub struct ActiveRunCount {
    /// Runs currently executing (`status = 'running'` and unfinished).
    pub active: u64,
}

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

/// PUT /api/automation/tasks/{id} — partial merge patch. Every field
/// optional: absent = keep the stored value. The desktop client sends
/// single-key patches (e.g. the enable/disable toggle sends only
/// `{enabled}`), so a whole-struct `Json<AutomationTask>` extractor
/// 422'd on the missing required fields. `null` for an `Option` field
/// clears it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateTaskRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub schedule: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub schedule_label: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub expert_id: Option<String>,
    #[serde(default)]
    pub workspace_path: Option<String>,
    #[serde(default)]
    pub permission_level: Option<String>,
    #[serde(default)]
    pub skills: Option<Vec<String>>,
    #[serde(default)]
    pub max_runs: Option<u32>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    /// Server-managed fields are accepted but ignored on patch —
    /// run history / next-run are recomputed by the scheduler.
    #[serde(default)]
    pub last_run_at: Option<String>,
    #[serde(default)]
    pub last_status: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub next_run_at: Option<String>,
}

/// Full-row → patch request (used by the in-process `cronjob` tool,
/// which works with whole `AutomationTask` values).
fn to_patch(t: &AutomationTask) -> UpdateTaskRequest {
    UpdateTaskRequest {
        name: Some(t.name.clone()),
        description: t.description.clone(),
        prompt: Some(t.prompt.clone()),
        schedule: Some(t.schedule.clone()),
        enabled: Some(t.enabled),
        schedule_label: t.schedule_label.clone(),
        model: t.model.clone(),
        expert_id: t.expert_id.clone(),
        workspace_path: t.workspace_path.clone(),
        permission_level: t.permission_level.clone(),
        skills: t.skills.clone(),
        max_runs: t.max_runs,
        category: t.category.clone(),
        icon: t.icon.clone(),
        last_run_at: t.last_run_at.clone(),
        last_status: t.last_status.clone(),
        last_error: t.last_error.clone(),
        next_run_at: t.next_run_at.clone(),
    }
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
    // 创建上限（对齐 ZCode 的 AUTOMATION_CREATE_LIMIT = 20，所有生命周期
    // 状态都计入）：定时任务是长驻数据，无上限会让任务表和调度 tick
    // 无限膨胀。
    const AUTOMATION_CREATE_LIMIT: usize = 20;
    if s.tasks.len() >= AUTOMATION_CREATE_LIMIT {
        return Err((
            StatusCode::CONFLICT,
            format!(
                "定时任务数量已达上限（{AUTOMATION_CREATE_LIMIT} 条，含内置示例）。请先删除不需要的任务再创建。"
            ),
        ));
    }
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

/// PUT /api/automation/tasks/{id} — merge patch (see UpdateTaskRequest).
/// Returns 404 if id unknown. Validates the *merged* result, not the patch.
pub async fn update_task(
    State(state): State<HttpServerState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<UpdateTaskRequest>,
) -> Result<Json<AutomationTask>, (StatusCode, String)> {
    let mut s = load_state(&state);
    let slot = s
        .tasks
        .iter_mut()
        .find(|t| t.id == id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("task id 不存在: {}", id)))?;

    // Merge onto the stored row; validate after merging so a
    // single-key patch (e.g. `{enabled}`) can't bypass the
    // prompt/schedule invariants by shipping empty strings.
    let mut merged = slot.clone();
    if let Some(v) = body.name {
        merged.name = v;
    }
    if body.description.is_some() {
        merged.description = body.description;
    }
    if let Some(v) = body.prompt {
        merged.prompt = v;
    }
    if let Some(v) = body.schedule {
        merged.schedule = v;
    }
    if let Some(v) = body.enabled {
        merged.enabled = v;
    }
    if body.schedule_label.is_some() {
        merged.schedule_label = body.schedule_label;
    }
    if body.model.is_some() {
        merged.model = body.model;
    }
    if body.expert_id.is_some() {
        merged.expert_id = body.expert_id;
    }
    if body.workspace_path.is_some() {
        merged.workspace_path = body.workspace_path;
    }
    if body.permission_level.is_some() {
        merged.permission_level = body.permission_level;
    }
    if body.skills.is_some() {
        merged.skills = body.skills;
    }
    if body.max_runs.is_some() {
        merged.max_runs = body.max_runs;
    }
    if body.category.is_some() {
        merged.category = body.category;
    }
    if body.icon.is_some() {
        merged.icon = body.icon;
    }

    if merged.prompt.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "prompt 不能为空".into()));
    }
    if normalize_cron_expr(&merged.schedule).parse::<Schedule>().is_err() {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("schedule 不合法: '{}'", merged.schedule),
        ));
    }
    merged.id = id.clone();
    merged.updated_at = now_iso();
    ensure_next_run(&mut merged);
    *slot = merged.clone();
    save_state(&state, &s).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(merged))
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
        // 手动触发同样要占住「单飞」位：调度 tick 不该在这轮跑的时候又起一轮。
        slot.running_since = Some(Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
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

/// 连续失败次数（护栏的第二个输入）：从最近一次成功往后数，连续失败的 run 数。
///
/// 与注入次数分开算，是因为失败模式不同：一个每天报错的任务，注入次数可能
/// 还没到上限，但已经在往用户主对话里灌错误信息。
fn consecutive_failures(history: &[RunRecord]) -> u64 {
    let mut n = 0u64;
    for r in history.iter().rev() {
        if r.status == "ok" {
            break;
        }
        n += 1;
    }
    n
}

/// 会话是否还存在。查不动 ≠ 不存在：查不动时按「不存在」处理，回退到独立会话
/// 是唯一安全的方向 —— 投进一个已被删除的对话会把它复活成一条无标题的孤儿行。
fn session_exists(state: &HttpServerState, session_id: &str) -> bool {
    match state.session_persistence.get_session_title(session_id) {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(e) => {
            tracing::warn!(
                session_id = %session_id,
                error = %e,
                "automation: session existence check failed; treating as gone"
            );
            false
        }
    }
}

/// Spawn the agent call for a single automation task. Updates the
/// persisted state with the outcome. Never panics — agent errors are
/// recorded in the task's `last_error` and `history`.
fn spawn_run(state: HttpServerState, task: AutomationTask) {
    tokio::spawn(async move {
        let started = Instant::now();
        let agent_service = state.agent_service.clone();
        let task_id = task.id.clone();
        let prompt = task.prompt.clone();
        let model = task.model.clone();
        let workspace_path = task.workspace_path.clone();
        let expert_id = task.expert_id.clone();
        let permission_level = task.permission_level.clone();

        // ── 投递目标：有锚点就续跑原会话，否则落到独立会话 ──
        //
        // 旧实现写死 `automation:{id}`：**锚点被存了但没人读**，于是「对话里建
        // 个定时任务、结果回到这个对话」这条最常见的用法拿不到。
        // 现在把锚点交给与 SQLite 引擎同一套护栏
        // （`hermix_automation::decide_delivery_target`）判定，判定规则不重写：
        // 无锚点 / 锚点已删 / 连续失败达上限 / 注入次数达上限 → 独立会话。
        let anchor = task
            .anchor_session_id
            .clone()
            .filter(|s| !s.trim().is_empty());
        let guard = DeliveryGuardInput {
            anchor_session_id: anchor.clone(),
            prior_injections: task.run_count as u64,
            consecutive_failures: consecutive_failures(&task.history),
            anchor_exists: match anchor.as_deref() {
                Some(a) => session_exists(&state, a),
                None => false,
            },
            // 回退会话**按任务固定**（不是按运行分桶）：注入次数达到上限后，
            // 结果要继续有地方去，而用户的主对话必须保持干净 —— 任务自己的
            // 会话正是那个地方。
            //
            // 为什么不能按 run 分桶（`automation:{id}:{run_id}`）：那会让每一轮
            // 回退都是**全新会话**，agent 每轮都要从零重新摸清「刷到哪了」。
            // 实测一个 630 节的刷课任务会连续跑几百轮 —— 每轮重摸状态就是
            // 几百轮 × 1~2 分钟的纯浪费。
            fallback_session_id: format!("automation:{}", task.id),
            max_injections: None,
            max_consecutive_failures: None,
        };
        let target = decide_delivery_target(&guard);
        let session_id = target.session_id().to_string();
        let fallback_reason = match &target {
            DeliveryTarget::Anchor(_) => None,
            DeliveryTarget::Standalone { reason, .. } => Some(reason.clone()),
        };
        if let Some(reason) = &fallback_reason {
            tracing::info!(
                automation_id = %task_id,
                reason = %reason,
                "automation result falling back to a standalone session"
            );
        }

        let mut overrides = hermix_core::traits::AgentOverrides::default();
        // **有界历史窗口**：这一轮只把最近若干 token 的历史送给模型。
        // 磁盘上的会话照旧完整（UI 时间线 / 归档 / 检索不受影响），但不再每轮
        // 重发整段历史 —— 实测一条跑 41 轮的刷课任务，历史累积到 3,718 条消息 /
        // 6.1M 字符，单次请求长到 ~1M token（模型窗口就是 1M），系统提示只占 1%。
        overrides.history_token_budget = Some(automation_history_token_budget());
        // 标成「外部触发」：这一轮的提示词与回复都以 `MessageRole::Automation`
        // 落库 —— 前端渲染成自动化卡片，而**不进 LLM 上下文**，模型不会在下一轮
        // 看到「用户说过这句话」的假象。
        overrides.external_delivery = Some(ExternalDelivery {
            source_label: task.name.clone(),
            run_id: None,
            fallback_reason,
        });
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
        if let Some(skills) = task.skills.clone() {
            if !skills.is_empty() {
                // 任务级技能（编辑器勾选）：走 ephemeral skills 通道，
                // 这些技能的内容强制注入提示词，不等同于全局启停。
                overrides.skills = Some(skills);
            }
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
            // 这一轮结束了 → 清掉「在跑」标记，单飞护栏放行下一槽。
            slot.running_since = None;
            // 有限次任务：跑满 max_runs 自动停用（调度 tick 跳过停用任务，
            // 停用即终止；重新启用会继续消耗剩余额度）。必须在
            // ensure_next_run 之前判——停用后不再计算下次触发时间。
            if let Some(max) = slot.max_runs {
                if slot.run_count >= max {
                    slot.enabled = false;
                }
            }
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
        let state_for_startup = state.clone();
        let handle = tokio::spawn(async move {
            // **进程刚起来 = 没有任何在途运行**：上一进程留下的 `running_since`
            // 标记一定是死的（它是在那个进程里被置上的，而那个进程已经没了）。
            // 不清掉的话，单飞护栏会把这些任务当成「还在跑」，
            // **整整 RUNNING_STALE_MINUTES（2 小时）都不触发**。
            // 与 SQLite 引擎的 `reap_abandoned_runs` 是同一件事。
            if let Err(e) = clear_stale_running_markers(&state_for_startup).await {
                tracing::warn!(
                    error = %e,
                    "automation scheduler: failed to clear stale running markers",
                );
            }
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

/// 清掉上一进程留下的 `running_since` 标记。
///
/// 进程刚启动时不可能有在途运行 —— 那些标记都是上一个进程置上的，而那个进程
/// 已经死了。不清的话单飞护栏会认为任务「还在跑」，要等
/// `RUNNING_STALE_MINUTES`（2 小时）才放行。
async fn clear_stale_running_markers(state: &HttpServerState) -> Result<(), String> {
    let mut snapshot = load_state(state);
    let mut cleared = 0usize;
    for task in snapshot.tasks.iter_mut() {
        if task.running_since.take().is_some() {
            cleared += 1;
        }
    }
    if cleared > 0 {
        save_state(state, &snapshot).await?;
        tracing::info!(
            cleared,
            "automation scheduler: cleared stale running markers from the previous process"
        );
    }
    Ok(())
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
        // **单飞**：上一轮还在跑就别再起一个。跑得比调度间隔长时（超星刷课
        // 一节 12 分钟 vs 15 分钟的槽），中途的 tick 会再触发一次 → 两个
        // agent 同时驱动同一个目标。`running_since` 太久没清就当那次进程死了。
        if is_run_in_flight(t, now) {
            continue;
        }
        let sched = match normalize_cron_expr(&t.schedule).parse::<Schedule>() {
            Ok(s) => s,
            Err(_) => {
                t.next_run_at = None;
                continue;
            }
        };
        // 「下一次什么时候跑」——纯展示用。
        let next = sched.after(&now).next();
        t.next_run_at = next.map(|d: DateTime<Utc>| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));

        // 「这一次该不该跑」——判的是**最近一次已过去的发生时刻**。
        //
        // 旧实现取 `sched.after(&now).next()`（恒晚于 now）再判 `next_at <= now`：
        // 恒假，于是这个调度器**从不触发任何任务**（2026-10-21 实测：任务启用后
        // next_run_at 正常推进，run_count 恒 0，会话里零回投）。
        //
        // 现在改成：在 [now - LOOKBACK, now] 窗口里找最近一次发生（LOOKBACK 覆盖
        // 一个 30s tick 的最大间隔 + 一次跳表），它比 `last_run_at` 新就说明这一槽
        // 还没跑过。顺带得到「进程停机期间错过的槽会在下次 tick 补跑」。
        if let Some(slot_at) = due_slot(&t.schedule, t.last_run_at.as_deref(), now) {
            let key = format!("{}:{}", t.id, slot_at.to_rfc3339());
            {
                let map = last_fired.read().await;
                if let Some(prev) = map.get(&key) {
                    if now.signed_duration_since(*prev) < chrono::Duration::seconds(60) {
                        continue;
                    }
                }
            }
            last_fired.write().await.insert(key, now);
            // 标记「这一轮在跑」：单飞护栏靠它挡住中途 tick 的重复触发。
            // **必须写在 snapshot 的条目上**（tick 结束时整个 snapshot 才落盘）——
            // 只写进克隆体的话，下一个 tick 从盘上读回来还是「没在跑」，护栏失效。
            t.running_since = Some(now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
            let task = t.clone();
            spawn_run(state.clone(), task);
        }
    }
    save_state(state, &snapshot).await.map_err(|e| e.to_string())?;
    Ok(())
}

// ── cronjob 工具的真实后端 ───────────────────────────────
//
// `SignalCronjobBackend` 只返回一段没人消费的请求 JSON——对话里"创建
// 定时任务"此前是空头支票。这里用本模块的存储 + 调度面实现
// CronjobBackend，装配时（with_automation_scheduler）覆盖注册，对话里
// 创建的任务真正落到自动化任务表并进入 30s 调度。toolset 参数暂不
// 映射：AutomationTask 没有工具集维度。

use hermix_core::ToolError;

pub struct DashboardCronjobBackend {
    state: HttpServerState,
}

impl DashboardCronjobBackend {
    pub fn new(state: HttpServerState) -> Self {
        Self { state }
    }

    fn find_task(&self, id: &str) -> Result<AutomationTask, ToolError> {
        // 走同步的 `load_state` 而非 `list_tasks`：后者是 async handler，
        // 在同步 fn 里调用拿到的是 Future，`.0` 取不到字段（E0609）。
        load_state(&self.state)
            .tasks
            .into_iter()
            .find(|t| t.id == id)
            .ok_or_else(|| ToolError::NotFound(format!("没有 id 为 '{id}' 的定时任务")))
    }
}

#[async_trait]
impl CronjobBackend for DashboardCronjobBackend {
    async fn create(
        &self,
        name: &str,
        schedule: &str,
        task: &str,
        _toolset: Option<&str>,
        anchor_session_id: Option<&str>,
    ) -> Result<String, ToolError> {
        let saved = create_task(
            State(self.state.clone()),
            Json(AutomationTask {
                id: String::new(),
                name: name.to_string(),
                description: None,
                prompt: task.to_string(),
                schedule: schedule.to_string(),
                enabled: true,
                schedule_label: None,
                model: None,
                expert_id: None,
                workspace_path: None,
                permission_level: None,
                skills: None,
                max_runs: None,
                category: None,
                icon: Some("⏰".to_string()),
                last_run_at: None,
                last_status: None,
                last_error: None,
                next_run_at: None,
                run_count: 0,
                history: Vec::new(),
        running_since: None,
                created_at: String::new(),
                updated_at: String::new(),
                anchor_session_id: anchor_session_id.map(str::to_string),
            }),
        )
        .await
        .map_err(|(_, e)| ToolError::ExecutionFailed(e))?;
        // 只有真的带锚点时才承诺回投。无锚点（UI 侧创建、非对话来源）
        // 走 `automation:{id}:{run_id}` 回退会话，结果落在独立会话里，
        // 说"会回到本对话"就是让模型转述一个不存在的行为。
        match anchor_session_id.filter(|s| !s.is_empty()) {
            Some(_) => Ok(format!(
                "已创建定时任务「{}」（schedule: {}），到点会自动运行，运行结果会回到本对话。",
                saved.name, saved.schedule
            )),
            None => Ok(format!(
                "已创建定时任务「{}」（schedule: {}），到点会自动运行。",
                saved.name, saved.schedule
            )),
        }
    }

    async fn list(&self) -> Result<String, ToolError> {
        // 同 find_task：list_tasks 是 async handler，直接取 `.0` 拿到的是 Future。
        serde_json::to_string(&load_state(&self.state).tasks)
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))
    }

    async fn update(
        &self,
        id: &str,
        schedule: Option<&str>,
        task: Option<&str>,
        enabled: Option<bool>,
    ) -> Result<String, ToolError> {
        let mut t = self.find_task(id)?;
        if let Some(s) = schedule {
            t.schedule = s.to_string();
        }
        if let Some(p) = task {
            t.prompt = p.to_string();
        }
        if let Some(e) = enabled {
            t.enabled = e;
        }
        let _ = update_task(State(self.state.clone()), AxumPath(id.to_string()), Json(to_patch(&t)))
            .await
            .map_err(|(_, e)| ToolError::ExecutionFailed(e))?;
        Ok(format!("已更新定时任务 {id}"))
    }

    async fn pause(&self, id: &str) -> Result<String, ToolError> {
        let mut t = self.find_task(id)?;
        t.enabled = false;
        let _ = update_task(State(self.state.clone()), AxumPath(id.to_string()), Json(to_patch(&t)))
            .await
            .map_err(|(_, e)| ToolError::ExecutionFailed(e))?;
        Ok(format!("定时任务 {id} 已暂停"))
    }

    async fn resume(&self, id: &str) -> Result<String, ToolError> {
        let mut t = self.find_task(id)?;
        t.enabled = true;
        let _ = update_task(State(self.state.clone()), AxumPath(id.to_string()), Json(to_patch(&t)))
            .await
            .map_err(|(_, e)| ToolError::ExecutionFailed(e))?;
        Ok(format!("定时任务 {id} 已恢复"))
    }

    async fn remove(&self, id: &str) -> Result<String, ToolError> {
        delete_task(State(self.state.clone()), AxumPath(id.to_string()))
            .await
            .map_err(|(_, e)| ToolError::ExecutionFailed(e))?;
        Ok(format!("已删除定时任务 {id}"))
    }

    async fn run(&self, id: &str) -> Result<String, ToolError> {
        trigger_task(State(self.state.clone()), AxumPath(id.to_string()))
            .await
            .map_err(|(_, e)| ToolError::ExecutionFailed(e))?;
        Ok(format!("已触发定时任务 {id} 立即运行"))
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(h: u32, m: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 21, h, m, s).unwrap()
    }

    fn rfc(t: DateTime<Utc>) -> String {
        t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    /// **回归钉子**：每分钟一次的任务，上次跑完在 13:29:30 → 下一槽是 13:30，
    /// 到了就该触发。
    /// 旧实现用「下一个未来时刻」判定，恒为假 → 调度器从不触发。
    #[test]
    fn due_slot_fires_for_a_schedule_whose_minute_just_passed() {
        let now = at(13, 30, 5);
        let last = rfc(at(13, 29, 30));
        let slot = due_slot("* * * * *", Some(&last), now).expect("13:30 那一槽该触发");
        assert_eq!(slot, at(13, 30, 0));
    }

    /// 同一槽跑过（`last_run_at` 落在下一次发生之后）→ 不该重复跑。
    #[test]
    fn due_slot_does_not_refire_a_slot_already_run() {
        let now = at(13, 30, 5);
        // 上次跑完在 13:30:02 → 下一个还没跑的槽是 13:31，还没到。
        let last = rfc(at(13, 30, 2));
        assert!(
            due_slot("* * * * *", Some(&last), now).is_none(),
            "这一槽已经跑过，不该再跑"
        );
    }

    /// 还没到的槽不跑：09:00 的任务在 08:00 判定为「不该跑」。
    #[test]
    fn due_slot_is_none_before_the_slot_arrives() {
        let now = at(8, 0, 0);
        assert!(due_slot("0 9 * * *", None, now).is_none());
    }

    /// 停机期间错过的槽要**补跑一次**：`last_run_at` 停在 3 小时前，每小时一次
    /// 的任务在 13:30 判定时认领「第一个没跑过的槽」（11:00）——
    /// 补跑后 `last_run_at` 变成现在，下一槽自然对齐，不会连补三次。
    #[test]
    fn due_slot_catches_up_a_slot_missed_while_stopped() {
        let now = at(13, 30, 5);
        let last = rfc(at(10, 0, 0));
        let slot = due_slot("0 * * * *", Some(&last), now).expect("错过的槽要补跑");
        assert_eq!(slot, at(11, 0, 0), "补跑认领的是第一个没跑过的槽");
        assert!(slot <= now, "补跑的槽必须已经过去");
    }

    /// 停用状态由 `tick_once` 跳过，不进这个判定。
    #[test]
    fn due_slot_ignores_unparseable_schedule() {
        let now = at(13, 30, 5);
        assert!(due_slot("不是 cron", None, now).is_none());
    }

    /// 连续失败只数**末尾**那一段：上一次成功之后的失败才计入。
    #[test]
    fn consecutive_failures_counts_only_the_trailing_streak() {
        let rec = |status: &str| RunRecord {
            at: "2026-10-21T00:00:00Z".into(),
            status: status.into(),
            duration_ms: 1,
            summary: None,
            error: None,
        };
        assert_eq!(consecutive_failures(&[]), 0);
        assert_eq!(consecutive_failures(&[rec("ok")]), 0);
        assert_eq!(consecutive_failures(&[rec("ok"), rec("failed")]), 1);
        assert_eq!(
            consecutive_failures(&[rec("failed"), rec("failed"), rec("ok"), rec("failed")]),
            1
        );
        assert_eq!(consecutive_failures(&[rec("failed"), rec("failed")]), 2);
    }

    /// **单飞**：上一轮还在跑时，不能再起一轮。
    ///
    /// 跑得比调度间隔长是常态（超星刷课一节 12 分钟 vs 15 分钟的槽），
    /// 没有这道闸，中途的 tick 会再触发一次 → 两个 agent 同时驱动同一个目标。
    #[test]
    fn a_task_already_running_is_not_fired_again() {
        let now = at(13, 30, 0);
        let mut task = seed_default().tasks.remove(0);
        assert!(
            !is_run_in_flight(&task, now),
            "没有 running_since 时不该认为在跑"
        );
        task.running_since = Some(rfc(at(13, 18, 0)));
        assert!(
            is_run_in_flight(&task, now),
            "12 分钟前起的那轮还在跑 → 挡住这一槽"
        );
    }

    /// 跑太久（进程死了）就当它不在跑，允许重跑。
    #[test]
    fn a_stale_running_marker_stops_blocking() {
        let now = at(13, 30, 0);
        let mut task = seed_default().tasks.remove(0);
        task.running_since = Some(rfc(at(9, 0, 0)));
        assert!(
            !is_run_in_flight(&task, now),
            "陈旧的 running_since 不能永久挡住任务"
        );
    }

    /// 进程重启后，上一进程留下的 `running_since` 标记必须被清掉。
    ///
    /// 不清的话，单飞护栏会把任务当成「还在跑」——要等
    /// `RUNNING_STALE_MINUTES`（2 小时）才放行。实测：杀掉 sidecar 再拉起，
    /// 那个任务整整两小时没跑一轮。
    #[tokio::test]
    async fn startup_clears_stale_running_markers() {
        let state = crate::test_state::test_state_with_temp_home().await;
        let mut snapshot = load_state(&state);
        assert!(!snapshot.tasks.is_empty(), "示例任务应当已被播种");
        snapshot.tasks[0].running_since = Some(rfc(Utc::now()));
        save_state(&state, &snapshot).await.unwrap();

        clear_stale_running_markers(&state).await.unwrap();

        let after = load_state(&state);
        assert!(
            after.tasks.iter().all(|t| t.running_since.is_none()),
            "上一进程留下的在途标记必须被清掉，否则任务会被当成还在跑"
        );
    }

    /// 存量 JSON 里的**旧示例**（读会话历史 / 扫工作目录 / 看 git 状态）会被刷新成
    /// 纯读公开信息的版本；但**用户自己改过**提示词的示例必须原样保留。
    #[test]
    fn builtin_demos_are_refreshed_but_user_edits_are_kept() {
        let mut state = seed_default();
        // 模拟「上一版留下的旧示例」：提示词命中升级映射。
        state.tasks[0].prompt = RETIRED_DEMO_MAP[0].0.to_string();
        // 模拟「用户改过的示例」：提示词不在升级映射里。
        let mut edited = state.tasks[1].clone();
        edited.prompt = "我自己写的提示词".into();
        state.tasks.push(edited);

        refresh_builtin_demos(&mut state);

        assert!(
            state.tasks[0].prompt.contains("不要读取或修改任何本地文件"),
            "旧示例应被刷新成不碰用户数据的版本，实际：{}",
            state.tasks[0].prompt
        );
        assert_eq!(
            state.tasks[0].id, "demo-daily-weather",
            "旧示例应换到新定义（含新 id）"
        );
        assert_eq!(
            state.tasks.last().unwrap().prompt,
            "我自己写的提示词",
            "用户改过的示例不能被覆盖"
        );
    }

    /// **内置示例必须是「不动用户数据」的纯信息任务。**    ///
    /// 旧示例读会话历史、扫工作目录、看 git 状态 —— 一旦用户启用示例，它们就会
    /// 去翻用户自己的数据。示例只该演示「定时任务能跑」，不该演示「能翻你的
    /// 私房」。每条示例的提示词都必须带上这句约束。
    #[test]
    fn demo_tasks_are_harmless_and_disabled_by_default() {
        let state = seed_default();
        assert_eq!(state.tasks.len(), 6, "示例任务应当是 6 条");
        for t in &state.tasks {
            assert!(
                !t.enabled,
                "示例任务「{}」必须默认停用（用户明确启用才会跑）",
                t.name
            );
            assert!(
                t.prompt.contains("不要读取或修改任何本地文件、会话历史或配置"),
                "示例任务「{}」的提示词必须写明不碰用户数据",
                t.name
            );
        }
    }
}
