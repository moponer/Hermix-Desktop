//! internal 成员会话运行器（总管方案 P1，D0/D4）。
//!
//! 成员 = **执行单元**：独立持久会话、小时级长任务、可停可查、最终交付
//! 报告 + 验收。与 `delegate_task` 子代理（数据通道：一次性、≤12 轮、
//! 8K 压缩结论）是两个概念，见
//! docs/notes/proposed/feature/2026-10-24-ali-chief-orchestrator.md D0。
//!
//! 差异点（相对 sub_agent_orchestrator 的执行机制，机制复用、语义不同）：
//! - 无 12 轮 / 30 分钟 / 8K 上限：轮次与超时由工单决定（默认 100 轮 /
//!   4 小时），报告上限 16K（仍是编排者上下文，必须有界）；
//! - 会话独立（`member::<id>`），depth 重置为 0 —— 成员的子代理拿完整
//!   预算做它自己的上下文保洁（子代理被硬禁再委派，成员相反）；
//! - `dispatch_member` 派单即返回（成员天然后台），`get_member_result`
//!   查进度/结果，`stop_member` 中止 —— 三工具由本文件注册，与
//!   delegation.rs 的数据通道工具并存互不沾染；
//! - 记录落盘 `~/.hermix/members/<id>.json`（原子写），重启后 get 可读。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use hermix_core::traits::RiskLevel;
use hermix_core::{
    tool_schema, JsonSchema, LlmProvider, Message, MessageRole, StreamChunk,
    StreamDelta, ToolError, ToolHandler, ToolSchema,
};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::agent_helpers::AgentCallbacks;
use crate::agent_loop::{AgentConfig, AgentLoop, ToolRegistry};
use crate::interrupt::InterruptController;
use crate::member::roles::{self, MemberResources, MemberRole, MemberWorker};
use crate::sub_agent_orchestrator::{tool_step_label, SubAgentOrchestrator};

/// 成员默认轮次预算（对比子代理硬顶 12 轮）。
pub const MEMBER_DEFAULT_MAX_TURNS: u32 = 100;
/// 成员默认墙钟上限：4 小时（对比子代理 30 分钟）。
pub const MEMBER_DEFAULT_TIMEOUT_SECS: u64 = 4 * 3600;
/// 成员最终报告上限。比子代理 8K 宽（长任务交付说明更厚），但必须有界
/// —— 编排者（阿黎）的上下文同样是钱。截断按字符边界（CJK 安全，
/// 见 2026-10-24 字符边界 panic 的教训）。
pub const MEMBER_REPORT_MAX_CHARS: usize = 16_000;
/// 前端「成员」面板滚动日志长度（对齐子代理面板的 6 条）。
pub const MEMBER_STEP_LOG_CAP: usize = 6;

/// 成员会话内被剥除的工具：不能再派成员（总管单层）、不能跨会话边界
/// （clarify 的用户在主会话那边听不到成员、cronjob/send_message/memory
/// 会污染宿主状态）。`delegate_task` **保留** —— 上下文保洁是成员自己的
/// 责任，这与子代理（被硬禁再委派）方向相反。
const MEMBER_BLOCKED_TOOLS: &[&str] = &[
    "dispatch_member",
    "get_member_result",
    "stop_member",
    // 治理动作只归总管（复查 P1-4）：成员自验收/改角色 = 绕开「验收人
    // ≠ 执行人」，评分/角色库也不是成员该碰的面。
    "review_member",
    "agent_role_upsert",
    "agent_role_list",
    "member_stats",
    "cronjob",
    "send_message",
    "memory",
];

/// 成员的 clarify 转发后端（P5）：成员问用户的问题盖**父会话**的 id 进
/// 全局 dispatcher——卡片渲染在阿黎的对话里，用户答完成员继续跑。
/// 注册账号时的「请把验证码给我」就靠它。
///
/// 问题加**归属前缀**（复查 P1-5）：没有标注的话，成员的问题在总管对话
/// 里与阿黎本人的提问一模一样——被提示注入的成员（registrar 会读任意
/// 网页）可以让用户交出验证码/凭据而无从分辨。前缀让用户始终知道是谁在问。
struct MemberClarifyBackend {
    inner: hermix_tools::ChannelClarifyBackend,
    parent_session_id: String,
    role: String,
    member_id: String,
}

#[async_trait]
impl hermix_tools::ClarifyBackend for MemberClarifyBackend {
    async fn ask(
        &self,
        question: &str,
        choices: Option<&[String]>,
        _member_session: Option<String>,
    ) -> Result<String, ToolError> {
        let tagged = format!("[成员 {} · {} 提问] {}", self.role, self.member_id, question);
        self.inner
            .ask(&tagged, choices, Some(self.parent_session_id.clone()))
            .await
    }
}

/// 一张成员派工单。
#[derive(Debug, Clone)]
pub struct MemberDispatch {
    /// 角色名（`~/.hermix/agents/*.md` 的 `name`）。
    pub role: String,
    pub task: String,
    /// 验收判据（reviewer 验收与成员自检共用）。
    pub acceptance_criteria: Vec<String>,
    pub max_turns: Option<u32>,
    pub timeout_secs: Option<u64>,
    /// 尝试序号（1 起；自动重派 +1）。D3：同一工单最多重派 2 次
    /// （attempts ≤ 3），仍 reject 升级问用户。
    pub attempts: u32,
    /// 重派链：本次是哪张工单的重试。
    pub retry_of: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemberStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
    Timeout,
}

/// 成员记录（`~/.hermix/members/<id>.json`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberRecord {
    pub member_id: String,
    pub role: String,
    pub task: String,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    pub status: MemberStatus,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub total_turns: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_report: Option<String>,
    /// 终态时的步骤日志快照（运行中看 running map 的 live 版）。
    #[serde(default)]
    pub recent_steps: Vec<String>,
    /// 独立评审结论（review_member 写入；验收人 ≠ 执行人，D3）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<MemberReview>,
    /// 尝试序号（1 起；自动重派递增）。
    #[serde(default = "default_attempts")]
    pub attempts: u32,
    /// 重派链上游。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_of: Option<String>,
}

fn default_attempts() -> u32 {
    1
}

/// `review_member` 的验收结论。verdict ∈ pass | warn | reject。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberReview {
    pub verdict: String,
    /// reviewer 的报告正文（证据所在，截断到 8K）。
    pub evidence: String,
    pub reviewer_member_id: String,
    pub reviewed_at: DateTime<Utc>,
}

#[derive(Default)]
struct RoleStatsAcc {
    total: u32,
    completed: u32,
    failed: u32,
    cancelled: u32,
    timeout: u32,
    review_pass: u32,
    review_warn: u32,
    review_reject: u32,
    turns_sum: f64,
    seconds_sum: f64,
}

/// 运行中的成员句柄（stop 用）。`abort` 杀外层包装任务；`inner_abort`
/// 由 run_member_to_terminal 起内层 AgentLoop 后回填——stop 必须两个都
/// 杀：外层 abort 只在 await 点生效，内层的长工具会继续写盘到下一个
/// LLM 边界（复查 P1-2）。
struct RunningMember {
    abort: tokio::task::AbortHandle,
    inner_abort: Mutex<Option<tokio::task::AbortHandle>>,
    interrupt: InterruptController,
    steps: Arc<Mutex<Vec<String>>>,
}

/// 运行器配置（生产用 `from_parent`，测试直接构造）。
pub struct MemberRunnerConfig {
    pub base_config: AgentConfig,
    pub tool_registry: Arc<ToolRegistry>,
    pub llm_provider: Arc<dyn LlmProvider>,
    pub hermix_home: PathBuf,
    pub chunk_emitter: Option<Arc<dyn Fn(StreamChunk) + Send + Sync>>,
    pub roles_dir: PathBuf,
}

pub struct MemberRunner {
    cfg: MemberRunnerConfig,
    running: Mutex<HashMap<String, RunningMember>>,
}

/// 进程级运行器槽：**只装一次**（首个会话构建期）。running map 在实例里，
/// 每 turn 重装会让 stop/get 对此前派出的成员失效（复查 P0-1）；后续会话
/// 只刷新 emitter（P1-1：成员事件要跟当前 WS 通道走）。
static RUNNER: RwLock<Option<Arc<MemberRunner>>> = RwLock::new(None);

/// 进程级 emitter 槽：每个会话构建时刷新（成员事件流向当前活跃 WS 通道）。
static EMITTER: RwLock<Option<Arc<dyn Fn(StreamChunk) + Send + Sync>>> = RwLock::new(None);

pub fn install_member_runner(runner: Arc<MemberRunner>, live_emitter: bool) {
    // live_emitter=true（流式会话）才刷新：新会话的 on_chunk 链是新 WS 通道。
    // 同步路径（REST/automation）传 no-op emitter——覆盖会把槽变成黑洞，
    // 此前流式会话的成员事件全部丢失（复查 P1-1）。
    if live_emitter {
        let mut em = EMITTER.write().expect("member emitter slot poisoned");
        *em = runner.cfg.chunk_emitter.clone();
    }
    let mut slot = RUNNER.write().expect("member runner slot poisoned");
    if slot.is_none() {
        reconcile_stale_running(&runner);
        *slot = Some(runner);
    }
}

/// 进程重启后盘上残留的 `running` 记录永不收敛（僵尸"进行中"卡片，
/// 复查 P1-6）：首装时扫一遍，全部翻成 Failed 并注明原因。
fn reconcile_stale_running(runner: &MemberRunner) {
    let dir = runner.members_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let mut fixed = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(&path) else { continue };
        let Ok(mut record) = serde_json::from_str::<MemberRecord>(&body) else { continue };
        if record.status != MemberStatus::Running {
            continue;
        }
        record.status = MemberStatus::Failed;
        record.ended_at = Some(Utc::now());
        record.final_report = Some("进程重启中断：工单未跑到终态，请重派。".to_string());
        if let Ok(out) = serde_json::to_string_pretty(&record) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, &out).is_ok()
                && std::fs::rename(&tmp, &path).is_ok()
            {
                fixed += 1;
            }
        }
    }
    if fixed > 0 {
        tracing::info!(fixed, "reconciled stale running member records after restart");
    }
}

/// 当前 emitter（emit_member 用）：优先槽里的（最新会话），回落 runner 自带。
pub fn current_member_emitter(
    runner: &MemberRunner,
) -> Option<Arc<dyn Fn(StreamChunk) + Send + Sync>> {
    EMITTER
        .read()
        .expect("member emitter slot poisoned")
        .clone()
        .or_else(|| runner.cfg.chunk_emitter.clone())
}

pub fn current_member_runner() -> Option<Arc<MemberRunner>> {
    RUNNER.read().expect("member runner slot poisoned").clone()
}

/// 停派门（总管方案 D5：派工单前必查）。true = 资源闸门越限（C 盘水位
/// 或 scratch 配额），拒发新工单。由 server 侧资源巡检安装
/// （`with_resource_patrol`），运行器只读。
static DISPATCH_GATE: RwLock<Option<Arc<dyn Fn() -> bool + Send + Sync>>> =
    RwLock::new(None);

pub fn install_dispatch_gate(gate: Arc<dyn Fn() -> bool + Send + Sync>) {
    let mut slot = DISPATCH_GATE.write().expect("dispatch gate slot poisoned");
    *slot = Some(gate);
}

fn dispatch_blocked_by_gate() -> bool {
    DISPATCH_GATE
        .read()
        .expect("dispatch gate slot poisoned")
        .as_ref()
        .is_some_and(|gate| gate())
}

impl MemberRunner {
    pub fn new(cfg: MemberRunnerConfig) -> Self {
        Self { cfg, running: Mutex::new(HashMap::new()) }
    }

    /// 从宿主 AgentLoop 提取依赖（与 SubAgentOrchestrator::from_parent
    /// 同源：成员与子代理共享宿主的 provider / registry 基线）。
    pub fn from_parent(
        parent: &AgentLoop,
        hermix_home: PathBuf,
        chunk_emitter: Option<Arc<dyn Fn(StreamChunk) + Send + Sync>>,
    ) -> Self {
        Self::new(MemberRunnerConfig {
            base_config: parent.config.clone(),
            tool_registry: parent.tool_registry.clone(),
            llm_provider: parent.llm_provider.clone(),
            hermix_home: hermix_home.clone(),
            chunk_emitter,
            roles_dir: hermix_home.join("agents"),
        })
    }

    pub fn registry(&self) -> &Arc<ToolRegistry> {
        &self.cfg.tool_registry
    }

    fn members_dir(&self) -> PathBuf {
        self.cfg.hermix_home.join("members")
    }

    /// 派单：解析角色 → 记录落盘 → 后台起成员会话 → 立即返回工单号。
    pub async fn dispatch(
        self: &Arc<Self>,
        req: MemberDispatch,
    ) -> Result<String, String> {
        if req.task.trim().is_empty() {
            return Err("task 不能为空".to_string());
        }
        // 停派门（D5）：资源闸门越限时拒发新工单。已运行的成员不杀——
        // 它们自带预算与超时，越限只挡增量。
        if dispatch_blocked_by_gate() {
            return Err(
                "资源闸门越限（C 盘水位 / scratch 配额），已停派新成员工单。\
                 请清理空间或等待 ResourceAlert 恢复通知后重派。"
                    .to_string(),
            );
        }
        // 内置角色缺则补（best-effort，用户改过的不会被覆盖）。
        let _ = roles::ensure_builtin_roles(&self.cfg.roles_dir);
        let library = roles::load_role_library(&self.cfg.roles_dir);
        let Some(role) = library.find(&req.role) else {
            let names: Vec<String> = library.roles.iter().map(|r| r.name.clone()).collect();
            let mut hint = format!("角色 `{}` 不存在。可用角色：{}", req.role, names.join(", "));
            if names.is_empty() {
                hint = format!("角色 `{}` 不存在，且 {} 下没有任何角色文件", req.role, self.cfg.roles_dir.display());
            }
            if !library.errors.is_empty() {
                hint.push_str(&format!(
                    "（另有 {} 个坏文件被跳过：{:?}）",
                    library.errors.len(),
                    library.errors.iter().map(|(p, _)| p.display().to_string()).collect::<Vec<_>>()
                ));
            }
            return Err(hint);
        };
        // 只落 internal。external 的三家本机客户端（ZCode/MiniMax Code/
        // CodeBuddy CN）经 2026-10-24 探测均无可用无头入口，P3 重定向为
        // provider 通道（MiniMax 额度走 minimax-cn provider + 角色 model
        // 覆盖）；browser 是 P5。见方案笔记 P3 修正。
        if !matches!(role.worker, MemberWorker::Internal) {
            let hint = match &role.worker {
                MemberWorker::External(d) => format!(
                    "外部客户端 `{d}` 无可用无头入口（2026-10-24 探测结论）。                     想用该家的模型额度：把本角色的 worker 改 internal 并设 model 覆盖                     （如 minimax 档），或等该客户端恢复无头接口。"
                ),
                MemberWorker::Browser => {
                    "browser worker 属于 P5（账号运营）落地项，尚未接线。".to_string()
                }
                MemberWorker::Internal => unreachable!(),
            };
            return Err(format!("角色 `{}` 的 worker 形态是 {:?}，{}",
                role.name, role.worker, hint));
        }

        let member_id = format!("member-{}", uuid::Uuid::new_v4());
        let record = MemberRecord {
            member_id: member_id.clone(),
            role: role.name.clone(),
            task: req.task.clone(),
            acceptance_criteria: req.acceptance_criteria.clone(),
            status: MemberStatus::Running,
            started_at: Utc::now(),
            ended_at: None,
            total_turns: 0,
            final_report: None,
            recent_steps: Vec::new(),
            review: None,
            attempts: req.attempts.max(1),
            retry_of: req.retry_of.clone(),
        };
        self.persist_record(&record).await;
        self.emit_member(&member_id, "dispatched", &req.task, 0, None, &[]);

        let steps: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let interrupt = InterruptController::new();
        let runner = self.clone();
        let runner_cleanup = self.clone();
        let member_id_cleanup = member_id.clone();
        let id_for_task = member_id.clone();
        let role_for_task = role.clone();
        let req_for_task = req.clone();
        let steps_for_task = steps.clone();
        let interrupt_for_task = interrupt.clone();
        // spawn + insert 同锁（对齐 spawn_background：防「先完成后插入」
        // 留下陈旧 AbortHandle，stop 会把已完成工单翻成 cancelled）。
        {
            let mut map = self
                .running
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let handle = tokio::spawn(async move {
                runner
                    .run_member_to_terminal(
                        id_for_task,
                        role_for_task,
                        req_for_task,
                        steps_for_task,
                        interrupt_for_task,
                    )
                    .await;
                runner_cleanup
                    .running
                    .lock()
                    .map(|mut m| m.remove(&member_id_cleanup))
                    .ok();
            });
            map.insert(
                member_id.clone(),
                RunningMember {
                    abort: handle.abort_handle(),
                    inner_abort: Mutex::new(None),
                    interrupt,
                    steps,
                },
            );
        }

        Ok(json!({
            "member_id": member_id,
            "status": "dispatched",
            "role": role.name,
            "default_max_turns": req.max_turns.unwrap_or(MEMBER_DEFAULT_MAX_TURNS),
            "note": "成员工单已派出（独立会话后台执行）。用 get_member_result 查进度与结果，stop_member 中止。",
        })
        .to_string())
    }

    /// 查询：live 步骤优先（运行中），否则读终态记录（重启后也可读）。
    pub async fn get(&self, member_id: &str) -> Value {
        let live_steps = self
            .running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(member_id)
            .map(|r| r.steps.lock().map(|v| v.clone()).unwrap_or_default());
        let is_running = live_steps.is_some();

        let path = self.members_dir().join(format!("{}.json", member_id));
        if !path.exists() {
            return json!({ "member_id": member_id, "status": "not_found" });
        }
        let Ok(body) = tokio::fs::read_to_string(&path).await else {
            return json!({ "member_id": member_id, "status": "record_unreadable" });
        };
        let Ok(record) = serde_json::from_str::<MemberRecord>(&body) else {
            return json!({ "member_id": member_id, "status": "record_corrupt" });
        };
        let steps = live_steps.unwrap_or(record.recent_steps.clone());
        let status_json = if is_running {
            json!("running")
        } else {
            serde_json::to_value(record.status).unwrap_or_else(|_| json!("unknown"))
        };
        json!({
            "member_id": record.member_id,
            "role": record.role,
            "status": status_json,
            "task": record.task,
            "acceptance_criteria": record.acceptance_criteria,
            "started_at": record.started_at,
            "ended_at": record.ended_at,
            "total_turns": record.total_turns,
            "attempts": record.attempts,
            "retry_of": record.retry_of,
            "recent_steps": steps,
            "review": record.review.clone().map(|r| json!({
                "verdict": r.verdict,
                "reviewer_member_id": r.reviewer_member_id,
                "reviewed_at": r.reviewed_at,
                "evidence": r.evidence,
            })).unwrap_or(Value::Null),
            "final_report": if is_running { Value::Null } else { record.final_report.clone().map(Value::String).unwrap_or(Value::Null) },
        })
    }

    /// 中止成员。返回 false = 该 id 当前没有在运行（已完成或不存在），
    /// 调用方应答「未在运行」而不是报错（对齐 stop_subagent 语义）。
    pub async fn stop(&self, member_id: &str) -> bool {
        let Some(handle) = self
            .running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(member_id)
        else {
            return false;
        };
        handle.interrupt.interrupt(Some("stopped by orchestrator".to_string()));
        handle.abort.abort();
        // 内层 AgentLoop 任务独立于外层包装——不杀它，长工具继续跑。
        if let Some(inner) = handle
            .inner_abort
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            inner.abort();
        }

        // 终态记录改写为 cancelled，让 get 查得到而不是永远 running。
        // 竞态：成员恰在 remove 与 abort 之间自然完成时，这里的 Cancelled
        // 可能覆盖 Completed —— 可接受（报告仍在，仅状态翻错），P1 不为此
        // 加锁序。
        let path = self.members_dir().join(format!("{}.json", member_id));
        if let Ok(body) = tokio::fs::read_to_string(&path).await {
            if let Ok(mut record) = serde_json::from_str::<MemberRecord>(&body) {
                record.status = MemberStatus::Cancelled;
                record.ended_at = Some(Utc::now());
                self.persist_record(&record).await;
            }
        }
        self.emit_member(member_id, "stopped", "", 0, Some("stopped by orchestrator"), &[]);
        true
    }

    /// 读盘上的成员记录（get 的结构化数据源）。
    async fn read_record(&self, member_id: &str) -> Option<MemberRecord> {
        let path = self.members_dir().join(format!("{}.json", member_id));
        let body = tokio::fs::read_to_string(&path).await.ok()?;
        serde_json::from_str(&body).ok()
    }

    /// 角色维度评分（P6）：从成员记录聚合。评分只算证据：完成率、验收
    /// 判定分布、平均轮次与墙钟。角色修订（agent_role_upsert）前必看。
    pub async fn role_stats(&self) -> Vec<Value> {
        let mut acc: std::collections::BTreeMap<String, RoleStatsAcc> =
            std::collections::BTreeMap::new();
        let Ok(mut entries) = tokio::fs::read_dir(self.members_dir()).await else {
            return Vec::new();
        };
        while let Ok(Some(e)) = entries.next_entry().await {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let Ok(body) = tokio::fs::read_to_string(&p).await else { continue };
            let Ok(r) = serde_json::from_str::<MemberRecord>(&body) else { continue };
            let a = acc.entry(r.role.clone()).or_default();
            a.total += 1;
            match r.status {
                MemberStatus::Completed => a.completed += 1,
                MemberStatus::Failed => a.failed += 1,
                MemberStatus::Cancelled => a.cancelled += 1,
                MemberStatus::Timeout => a.timeout += 1,
                MemberStatus::Running => {}
            }
            if let Some(rv) = &r.review {
                match rv.verdict.as_str() {
                    "pass" => a.review_pass += 1,
                    "warn" => a.review_warn += 1,
                    _ => a.review_reject += 1,
                }
            }
            a.turns_sum += r.total_turns as f64;
            if let Some(ended) = r.ended_at {
                if let Ok(d) = (ended - r.started_at).to_std() {
                    a.seconds_sum += d.as_secs() as f64;
                }
            }
        }
        acc.into_iter()
            .map(|(role, a)| {
                let n = a.total.max(1) as f64;
                json!({
                    "role": role,
                    "total": a.total,
                    "completed": a.completed,
                    "failed": a.failed,
                    "cancelled": a.cancelled,
                    "timeout": a.timeout,
                    "completion_rate": (a.completed as f64 / n * 100.0).round() / 100.0,
                    "review": { "pass": a.review_pass, "warn": a.review_warn, "reject": a.review_reject },
                    "avg_turns": (a.turns_sum / n).round() / 100.0,
                    "avg_seconds": (a.seconds_sum / n).round() / 100.0,
                })
            })
            .collect()
    }

    /// 工单列表（任务台用）：全部记录按开始时间倒序，**剥掉最终报告
    /// （16K）与步骤日志**——列表要轻，详情走 `get`。上限 200 条。
    pub async fn list(&self) -> Vec<Value> {
        let mut items: Vec<(String, Value)> = Vec::new();
        let Ok(mut entries) = tokio::fs::read_dir(self.members_dir()).await else {
            return Vec::new();
        };
        while let Ok(Some(e)) = entries.next_entry().await {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let Ok(body) = tokio::fs::read_to_string(&p).await else { continue };
            let Ok(r) = serde_json::from_str::<MemberRecord>(&body) else { continue };
            let started = hermix_core::time::utc_to_beijing(&r.started_at).to_rfc3339();
            items.push((
                started.clone(),
                json!({
                    "member_id": r.member_id,
                    "role": r.role,
                    "task": r.task.chars().take(80).collect::<String>(),
                    "status": r.status,
                    "started_at": r.started_at,
                    "ended_at": r.ended_at,
                    "total_turns": r.total_turns,
                    "review_verdict": r.review.as_ref().map(|v| v.verdict.clone()),
                    "attempts": r.attempts,
                    "retry_of": r.retry_of,
                }),
            ));
        }
        // RFC3339 同精度下字典序即时间序，倒序 = 新工单在前。
        items.sort_by(|a, b| b.0.cmp(&a.0));
        items.into_iter().map(|(_, v)| v).take(200).collect()
    }

    /// 验收（D3：验收人 ≠ 执行人）。派只读 reviewer 对照工单判据核验
    /// 交付物，解析三态判定并把证据写回成员记录。返回给编排者的 JSON。
    pub async fn review_member(
        self: &Arc<Self>,
        member_id: &str,
    ) -> Result<String, String> {
        let Some(record) = self.read_record(member_id).await else {
            return Err(format!("成员 `{member_id}` 不存在"));
        };
        let report = record.final_report.clone().unwrap_or_default();
        if matches!(record.status, MemberStatus::Running) {
            return Err(format!("成员 `{member_id}` 还在运行，先等终态（get_member_result）再验收"));
        }

        // 评审工单：原任务 + 判据 + 执行者的自述报告。reviewer 的角色
        // prompt 规定结论第一行必须是「判定: pass|warn|reject」。
        let mut ticket = format!(
            "# 待验收工单（member_id: {member_id}）\n## 原任务\n{}\n",
            record.task
        );
        if !record.acceptance_criteria.is_empty() {
            ticket.push_str("\n## 验收判据（逐项核验）\n");
            for (i, c) in record.acceptance_criteria.iter().enumerate() {
                ticket.push_str(&format!("{}. {}\n", i + 1, c));
            }
        } else {
            ticket.push_str("\n## 验收判据\n（工单未给判据——按任务描述的合理完成标准核验，并在备注里说明判据缺失）\n");
        }
        ticket.push_str(&format!(
            "\n## 执行者自述报告\n{}\n\n请以只读方式核验交付物（路径/测试/输出可复核），输出三态判定。",
            if report.is_empty() { "（执行者无最终报告——直接 reject）" } else { report.as_str() }
        ));

        let dispatch_out = self
            .dispatch(MemberDispatch {
                role: "reviewer".to_string(),
                task: ticket,
                acceptance_criteria: record.acceptance_criteria.clone(),
                max_turns: Some(30),
                timeout_secs: Some(1800),
                attempts: 1,
                retry_of: None,
            })
            .await?;
        let parsed: Value =
            serde_json::from_str(&dispatch_out).map_err(|e| format!("reviewer 派单回包异常: {e}"))?;
        let reviewer_id = parsed["member_id"]
            .as_str()
            .ok_or("reviewer 派单回包缺 member_id")?
            .to_string();

        // 等评审终态（真实时钟 deadline；评审通常 1-5 分钟）。
        let deadline = std::time::Instant::now() + Duration::from_secs(600);
        let mut reviewer_snapshot = self.get(&reviewer_id).await;
        loop {
            if reviewer_snapshot["status"] != "running" && reviewer_snapshot["status"] != "dispatched" {
                break;
            }
            if std::time::Instant::now() >= deadline {
                return Ok(json!({
                    "member_id": member_id,
                    "verdict": "review_timeout",
                    "reviewer_member_id": reviewer_id,
                    "note": "评审 10 分钟未到终态。稍后用 get_member_result 查 reviewer 结果并重试验收。",
                })
                .to_string());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            reviewer_snapshot = self.get(&reviewer_id).await;
        }

        let reviewer_report = reviewer_snapshot["final_report"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let (verdict, parse_note) = match parse_review_verdict(&reviewer_report) {
            Some(v) => (v.to_string(), None),
            None => (
                "reject".to_string(),
                Some("评审输出缺少可解析的「判定:」行，按未通过处理".to_string()),
            ),
        };
        let evidence: String = reviewer_report.chars().take(8000).collect();

        // 写回成员记录。
        let mut target = self
            .read_record(member_id)
            .await
            .ok_or("验收写回时记录丢失")?;
        target.review = Some(MemberReview {
            verdict: verdict.clone(),
            evidence: evidence.clone(),
            reviewer_member_id: reviewer_id.clone(),
            reviewed_at: Utc::now(),
        });
        self.persist_record(&target).await;
        self.emit_member(member_id, &format!("review_{verdict}"), &target.task, 0, None, &[]);

        // D3：reject 自动重派（同一工单 attempts ≤ 3，即首派 + 重派 2 次）。
        // 重派工单附上 reviewer 证据；第 3 次仍 reject 停止，升级问用户。
        let mut redispatched: Option<String> = None;
        let next_step;
        if verdict == "reject" {
            if target.attempts < 3 {
                let retry_ticket = format!(
                    "{}

# 上一次验收未通过（第 {} 次尝试）—— 针对性修复
评审证据：
{}",
                    target.task,
                    target.attempts,
                    evidence.chars().take(4000).collect::<String>()
                );
                let retry_req = MemberDispatch {
                    role: target.role.clone(),
                    task: retry_ticket,
                    acceptance_criteria: target.acceptance_criteria.clone(),
                    max_turns: None,
                    timeout_secs: None,
                    attempts: target.attempts + 1,
                    retry_of: Some(member_id.to_string()),
                };
                match self.dispatch(retry_req).await {
                    Ok(out) => {
                        redispatched = serde_json::from_str::<Value>(&out)
                            .ok()
                            .and_then(|v| v["member_id"].as_str().map(String::from));
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "auto-retry dispatch failed");
                    }
                }
                next_step = match &redispatched {
                    Some(_) => "未通过：已自动重派（证据附进新工单）。跟踪新工单的 member_id。",
                    None => "未通过：自动重派失败（资源闸门或角色问题）。请人工决策。",
                };
            } else {
                next_step = "已到重派上限（3 次尝试）仍未通过：停止重派，把证据摆给用户定夺。";
            }
        } else {
            next_step = match verdict.as_str() {
                "pass" => "验收通过：消化报告后向用户交付结论。",
                _ => "有保留通过：交付时带上 reviewer 的备注。",
            };
        }
        Ok(json!({
            "member_id": member_id,
            "verdict": verdict,
            "parse_note": parse_note,
            "reviewer_member_id": reviewer_id,
            "evidence": evidence,
            "attempts": target.attempts,
            "redispatched_member_id": redispatched,
            "next_step": next_step,
        })
        .to_string())
    }

    /// 成员会话主体：构建配置/注册表/初始消息 → 跑 loop → 终态落盘。
    async fn run_member_to_terminal(
        &self,
        member_id: String,
        role: MemberRole,
        req: MemberDispatch,
        steps: Arc<Mutex<Vec<String>>>,
        interrupt: InterruptController,
    ) {
        let timeout = Duration::from_secs(req.timeout_secs.unwrap_or(MEMBER_DEFAULT_TIMEOUT_SECS));

        let mut cfg = self.cfg.base_config.clone();
        cfg.session_id = Some(format!("member::{}", member_id));
        cfg.max_turns = req.max_turns.unwrap_or(MEMBER_DEFAULT_MAX_TURNS);
        cfg.stream = false;
        cfg.quiet_mode = true;
        cfg.skip_memory = true;
        cfg.is_sub_agent = true; // 容器语义（静默/内敛）；depth 独立重置为 0
        cfg.background_review_enabled = false;
        cfg.background_review_metrics_enabled = false;
        cfg.memory_nudge_interval = 0;
        cfg.skill_creation_nudge_interval = 0;
        if let Some(model) = &role.model {
            cfg.model = model.clone();
        }
        // 角色人格叠加在全局系统提示之后（personality overlay），
        // 全局纪律保留、成员人格注入。
        cfg.personality = Some(role.system_prompt.clone());

        // 成员工具面：剥成员禁项；read_only 角色按 Explore 同款白名单收窄；
        // 逗号名单则逐项收窄。
        let registry = self
            .cfg
            .tool_registry
            .without_tools(MEMBER_BLOCKED_TOOLS.iter().copied());
        let mut registry = match role.toolset.as_deref() {
            Some("read_only") => {
                let allow: std::collections::HashSet<&str> =
                    crate::sub_agent_orchestrator::EXPLORE_ROLE_TOOLS.iter().copied().collect();
                registry.retain_only(|name| allow.contains(name))
            }
            Some("full") | None => registry,
            Some(list) => {
                let allow: std::collections::HashSet<String> =
                    list.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                registry.retain_only(move |name| allow.contains(&name.to_string()))
            }
        };
        // clarify 换装为父会话转发版（P5 验证码环）：read_only 收窄会剥掉
        // 它，这里重新注册；原 handler 盖的是成员自己的 session id（没有
        // UI 拥有者，卡片永远无人应答），转发版盖**父会话** id——卡片出在
        // 总管的对话里，用户答完成员继续跑。桥接注册表是同步闭包，用
        // block_in_place + block_on 适配异步 ask（与 memory 桥接同款）。
        let parent_session = self
            .cfg
            .base_config
            .session_id
            .clone()
            .unwrap_or_else(|| "default".to_string());
        if let Some(entry) = self.cfg.tool_registry.get("clarify") {
            let backend = Arc::new(MemberClarifyBackend {
                inner: hermix_tools::ChannelClarifyBackend::new(
                    hermix_tools::ClarifyDispatcher::global().clone(),
                ),
                parent_session_id: parent_session,
                role: role.name.clone(),
                member_id: member_id.clone(),
            });
            registry.register_with_risk(
                "clarify".to_string(),
                entry.schema.clone(),
                Arc::new(move |params: Value| -> Result<String, ToolError> {
                    let question = params
                        .get("question")
                        .and_then(Value::as_str)
                        .ok_or_else(|| ToolError::InvalidParams("Missing 'question'".into()))?
                        .to_string();
                    let choices: Option<Vec<String>> = params
                        .get("choices")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect());
                    let b = backend.clone();
                    use hermix_tools::ClarifyBackend as _;
                    tokio::task::block_in_place(move || {
                        tokio::runtime::Handle::current().block_on(async move {
                            b.ask(&question, choices.as_deref(), None).await
                        })
                    })
                }),
                entry.risk_level,
            );
        }

        let initial = member_initial_messages(&role, &req);

        let emitter = self.cfg.chunk_emitter.clone();
        let task_label = req.task.chars().take(80).collect::<String>();
        let steps_cb = steps.clone();
        let steps_cb2 = steps.clone();
        let id_cb = member_id.clone();
        let callbacks = AgentCallbacks {
            on_tool_start: Some(Box::new(move |_id, tool, args| {
                let step = tool_step_label(tool, args);
                if let Ok(mut v) = steps_cb.lock() {
                    v.push(step);
                    if v.len() > MEMBER_STEP_LOG_CAP {
                        let excess = v.len() - MEMBER_STEP_LOG_CAP;
                        v.drain(..excess);
                    }
                }
                if let Some(emit) = &emitter {
                    let snapshot = steps_cb.lock().map(|v| v.clone()).unwrap_or_default();
                    let payload = json!({
                        "event": "member_update",
                        "member_id": id_cb,
                        "phase": "running",
                        "task": task_label,
                        "recent_steps": snapshot,
                    });
                    emit(StreamChunk {
                        delta: Some(StreamDelta {
                            content: None,
                            tool_calls: None,
                            extra: Some(payload),
                        }),
                        finish_reason: None,
                        usage: None,
                    });
                }
            })),
            on_thinking: Some(Box::new(move |_text| {
                if let Ok(mut v) = steps_cb2.lock() {
                    let last = v.last().map(|s| s.as_str());
                    if last != Some("正在思考…") {
                        v.push("正在思考…".to_string());
                        if v.len() > MEMBER_STEP_LOG_CAP {
                            let excess = v.len() - MEMBER_STEP_LOG_CAP;
                            v.drain(..excess);
                        }
                    }
                }
            })),
            ..Default::default()
        };

        let child_interrupt = interrupt.clone();
        // spawn 块不能捕获 `&self`（非 'static）—— 需要的字段先克隆。
        let provider = self.cfg.llm_provider.clone();
        let hermix_home = self.cfg.hermix_home.clone();
        let join = tokio::spawn(async move {
            let member_agent = AgentLoop::with_interrupt(
                cfg,
                Arc::new(registry),
                provider,
                child_interrupt,
            )
            .with_delegate_depth(0)
            .with_callbacks(callbacks);
            // 成员自己的 delegate_task：附上以成员为父的编排器（成员的
            // 子代理 depth 从 0 起算，预算全新）。
            let orch = SubAgentOrchestrator::from_parent(&member_agent, hermix_home, None);
            let member_agent = member_agent.with_sub_agent_orchestrator(Arc::new(orch));
            member_agent.run(initial, None).await
        });

        // 回填内层 abort 句柄（P1-2）：stop 双杀的另一半。
        if let Some(entry) = self
            .running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&member_id)
        {
            *entry.inner_abort.lock().unwrap_or_else(|p| p.into_inner()) =
                Some(join.abort_handle());
        }

        let mut join = Box::pin(join);
        // run() 返回三层：timeout → join → run 的外层 → 内层 AgentResult。
        let result = match tokio::time::timeout(timeout, join.as_mut()).await {
            Ok(Ok(Ok(agent_result))) if agent_result.interrupted => Err("cancelled".to_string()),
            Ok(Ok(Ok(agent_result))) => Ok(agent_result),
            Ok(Ok(Err(e))) => Err(format!("member loop failed: {e}")),
            Ok(Err(join_err)) => Err(format!("member task lost: {join_err}")),
            Err(_) => {
                interrupt.interrupt(Some("timeout".to_string()));
                // timeout 以 &mut 消费 future，join handle 仍可 abort。
                // 协作中断只在 LLM 边界生效，工具执行中要靠 abort 兜底
                //（对齐 orchestrator 的教训：不 abort 会留下写后半截的孤儿）。
                join.abort();
                Err("timeout".to_string())
            }
        };

        // 终态落盘。
        let path = self.members_dir().join(format!("{}.json", member_id));
        let steps_snapshot = steps.lock().map(|v| v.clone()).unwrap_or_default();
        let mut record = tokio::fs::read_to_string(&path)
            .await
            .ok()
            .and_then(|body| serde_json::from_str::<MemberRecord>(&body).ok())
            .unwrap_or_else(|| MemberRecord {
                member_id: member_id.clone(),
                role: role.name.clone(),
                task: req.task.clone(),
                acceptance_criteria: req.acceptance_criteria.clone(),
                status: MemberStatus::Failed,
                started_at: Utc::now(),
                ended_at: None,
                total_turns: 0,
                final_report: None,
                recent_steps: Vec::new(),
                review: None,
                attempts: 1,
                retry_of: None,
            });
        let phase;
        match result {
            Ok(agent_result) => {
                record.status = MemberStatus::Completed;
                record.total_turns = agent_result.total_turns;
                record.final_report = Some(extract_member_report(&agent_result.messages));
                phase = "done";
            }
            Err(reason) => {
                record.status = match reason.as_str() {
                    "timeout" => MemberStatus::Timeout,
                    "cancelled" => MemberStatus::Cancelled,
                    _ => MemberStatus::Failed,
                };
                record.final_report = None;
                phase = match record.status {
                    MemberStatus::Timeout => "timeout",
                    MemberStatus::Cancelled => "cancelled",
                    _ => "failed",
                };
            }
        }
        record.ended_at = Some(Utc::now());
        record.recent_steps = steps_snapshot.clone();
        self.persist_record(&record).await;
        self.emit_member(
            &member_id,
            phase,
            &req.task,
            record.total_turns,
            None,
            &steps_snapshot,
        );
    }

    async fn persist_record(&self, record: &MemberRecord) {
        let dir = self.members_dir();
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            tracing::debug!(error = %e, "member record dir create failed");
            return;
        }
        let path = dir.join(format!("{}.json", record.member_id));
        let body = match serde_json::to_string_pretty(record) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(error = %e, "member record serialize failed");
                return;
            }
        };
        // 原子写（tmp + rename）：get 的轮询不能读到半个 JSON。
        let tmp = path.with_extension("json.tmp");
        if let Err(e) = tokio::fs::write(&tmp, body).await {
            tracing::debug!(error = %e, path = %tmp.display(), "member record write failed");
            return;
        }
        if let Err(e) = tokio::fs::rename(&tmp, &path).await {
            tracing::debug!(error = %e, path = %path.display(), "member record rename failed");
            let _ = tokio::fs::remove_file(&tmp).await;
        }
    }

    fn emit_member(
        &self,
        member_id: &str,
        phase: &str,
        task: &str,
        turns: u32,
        err: Option<&str>,
        steps: &[String],
    ) {
        let Some(emit) = current_member_emitter(self) else { return };
        let payload = json!({
            "event": "member_update",
            "member_id": member_id,
            "phase": phase,
            "task": task.chars().take(80).collect::<String>(),
            "turns": turns,
            "error": err,
            "recent_steps": steps,
        });
        emit(StreamChunk {
            delta: Some(StreamDelta { content: None, tool_calls: None, extra: Some(payload) }),
            finish_reason: None,
            usage: None,
        });
    }
}

/// 成员初始消息：系统契约 + 工单（任务 + 判据）。
fn member_initial_messages(role: &MemberRole, req: &MemberDispatch) -> Vec<Message> {
    let mut guidance = String::from(
        "[Member operating contract]\n\
         你是团队成员（执行单元）：独立承包一张工单，从理解需求到交付验收全程自驱。\n\
         \n\
         # 边界\n\
         1. 你可以用 delegate_task 派子代理做上下文保洁（大文件/长输出/多文件检索），\n\
            这是你自己的责任 —— 你的子代理预算充足。\n\
         2. 你不能调用 dispatch_member / cronjob / send_message / memory ——\n\
            它们已从你的工具注册表移除，调用只会得到错误。\n\
         3. clarify **可以**用：你的问题会转发到总管的对话由用户回答（验证码/关键决策），能自己查的别问。\n\
         3. 你的最终报告上限约 16,000 字符：交付说明要结构化、给证据（文件/行号/测试），\n\
            不贴原始数据。\n\
         \n\
         # 完成定义\n\
         工单里的验收判据就是你的完成定义。完工前逐条自检并把结果写进最终报告。\n",
    );
    guidance.push_str(&format!("\n# 你的角色\n{}\n{}\n", role.name, role.description));

    let mut ticket = format!("# 工单\n{}\n", req.task);
    if !req.acceptance_criteria.is_empty() {
        ticket.push_str("\n# 验收判据（完成定义，逐条自检）\n");
        for (i, c) in req.acceptance_criteria.iter().enumerate() {
            ticket.push_str(&format!("{}. {}\n", i + 1, c));
        }
    }

    vec![
        Message { role: MessageRole::System, content: Some(guidance), ..Default::default() },
        Message { role: MessageRole::User, content: Some(ticket), ..Default::default() },
    ]
}

/// 提取成员最终报告：最后一条 assistant 可见文本，CJK 安全截断到
/// [`MEMBER_REPORT_MAX_CHARS`]。
fn extract_member_report(messages: &[Message]) -> String {
    let last = messages
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::Assistant)
        .and_then(|m| m.content.as_deref())
        .map(|s| s.trim())
        .unwrap_or("");
    if last.chars().count() <= MEMBER_REPORT_MAX_CHARS {
        last.to_string()
    } else {
        let mut cut: String = last.chars().take(MEMBER_REPORT_MAX_CHARS).collect();
        cut.push_str("\n…[报告超长已截断]");
        cut
    }
}

// ---------------------------------------------------------------------------
// 工具注册：dispatch_member / get_member_result / stop_member /
// review_member / agent_role_list / agent_role_upsert。
// 与 delegation.rs 并存互不沾染 —— delegate_task 的 schema 一字不动。
// ---------------------------------------------------------------------------

/// 从 reviewer 报告解析三态判定。契约（reviewer 角色文件规定）：
/// 结论第一行是 `判定: pass|warn|reject`（中英冒号皆可）。读不出 → None，
/// 调用方按 reject 处理（验收人输出不合格 = 未通过，宁严勿松）。
fn parse_review_verdict(report: &str) -> Option<&'static str> {
    for line in report.lines() {
        let l = line.trim();
        let Some(rest) = l.strip_prefix("判定") else { continue };
        let rest = rest
            .trim_start_matches([':', '：', ' ', '\t'])
            .trim()
            .to_ascii_lowercase();
        return match () {
            _ if rest.starts_with("pass") => Some("pass"),
            _ if rest.starts_with("warn") => Some("warn"),
            _ if rest.starts_with("reject") => Some("reject"),
            _ => None,
        };
    }
    None
}

struct ReviewMemberHandler;

#[async_trait]
impl ToolHandler for ReviewMemberHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let Some(runner) = current_member_runner() else {
            return Err(ToolError::ExecutionFailed(
                "review_member: 成员运行器未安装。".to_string(),
            ));
        };
        let id = params
            .get("member_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::ExecutionFailed("review_member requires 'member_id'.".into()))?;
        runner
            .review_member(id)
            .await
            .map_err(ToolError::ExecutionFailed)
    }

    fn schema(&self) -> ToolSchema {
        tool_schema(
            "review_member",
            "验收成员工单",
            JsonSchema::object(
                [(
                    "member_id".to_string(),
                    json!({
                        "type": "string",
                        "description": "待验收的 member_id（须已到终态）。"
                    }),
                )]
                .into_iter()
                .collect(),
                vec!["member_id".into()],
            ),
        )
    }
}

struct MemberStatsHandler;

#[async_trait]
impl ToolHandler for MemberStatsHandler {
    async fn execute(&self, _params: Value) -> Result<String, ToolError> {
        let Some(runner) = current_member_runner() else {
            return Err(ToolError::ExecutionFailed(
                "member_stats: 成员运行器未安装。".to_string(),
            ));
        };
        Ok(json!({ "stats": runner.role_stats().await }).to_string())
    }

    fn schema(&self) -> ToolSchema {
        tool_schema(
            "member_stats",
            "看角色评分",
            JsonSchema::object(IndexMap::new(), vec![]),
        )
    }
}

struct AgentRoleListHandler;

#[async_trait]
impl ToolHandler for AgentRoleListHandler {
    async fn execute(&self, _params: Value) -> Result<String, ToolError> {
        let Some(runner) = current_member_runner() else {
            return Err(ToolError::ExecutionFailed(
                "agent_role_list: 成员运行器未安装。".to_string(),
            ));
        };
        let _ = roles::ensure_builtin_roles(&runner.cfg.roles_dir);
        let library = roles::load_role_library(&runner.cfg.roles_dir);
        let list: Vec<Value> = library
            .roles
            .iter()
            .map(|r| {
                json!({
                    "name": r.name,
                    "description": r.description,
                    "worker": match &r.worker {
                        MemberWorker::Internal => "internal".to_string(),
                        MemberWorker::External(d) => format!("external:{d}"),
                        MemberWorker::Browser => "browser".to_string(),
                    },
                    "toolset": r.toolset,
                    "model": r.model,
                    "version": r.version,
                    "file": r.source_path.display().to_string(),
                })
            })
            .collect();
        Ok(json!({
            "roles": list,
            "errors": library.errors.iter().map(|(p, e)| json!({
                "file": p.display().to_string(), "error": e
            })).collect::<Vec<_>>(),
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        tool_schema(
            "agent_role_list",
            "列角色库",
            JsonSchema::object(IndexMap::new(), vec![]),
        )
    }
}

struct AgentRoleUpsertHandler;

#[async_trait]
impl ToolHandler for AgentRoleUpsertHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let Some(runner) = current_member_runner() else {
            return Err(ToolError::ExecutionFailed(
                "agent_role_upsert: 成员运行器未安装。".to_string(),
            ));
        };
        let get_str = |k: &str| {
            params
                .get(k)
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };
        let name = get_str("name")
            .ok_or_else(|| ToolError::ExecutionFailed("agent_role_upsert requires 'name'.".into()))?;
        // 文件名安全：只放行小写字母/数字/连字符/下划线（防路径穿越）。
        if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
            return Err(ToolError::ExecutionFailed(
                "name 只允许小写字母/数字/`-`/`_`（它同时是文件名）".to_string(),
            ));
        }
        let description = get_str("description").ok_or_else(|| {
            ToolError::ExecutionFailed("agent_role_upsert requires 'description'.".into())
        })?;
        let system_prompt = params
            .get("system_prompt")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ToolError::ExecutionFailed("agent_role_upsert requires non-empty 'system_prompt'.".into())
            })?;
        let toolset = get_str("toolset");
        let model = get_str("model");
        let worker = get_str("worker").unwrap_or_else(|| "internal".to_string());
        if !matches!(worker.as_str(), "internal" | "browser")
            && !worker.starts_with("external:")
        {
            return Err(ToolError::ExecutionFailed(
                "worker 只认 internal | browser | external:<driver>".to_string(),
            ));
        }

        // 版本递增 + resources 保留（改角色不动资源档）。
        let roles_dir = &runner.cfg.roles_dir;
        let path = roles_dir.join(format!("{name}.md"));
        let (version, resources) = match std::fs::read_to_string(&path) {
            Ok(existing) => {
                let parsed = roles::parse_role_content(&path, &existing)
                    .map_err(|e| ToolError::ExecutionFailed(format!("现有文件已坏，先人工修复: {e}")))?;
                (parsed.version + 1, parsed.resources)
            }
            Err(_) => (1, MemberResources::upsert_default()),
        };

        // description 用双引号包裹：LLM 生成的话术常含裸冒号（YAML 解析
        // 直接失败）甚至换行（注入任意 frontmatter 字段，复查 P1-3）。
        // 引号内只需转义反斜杠与双引号本身。
        let esc = |v: &str| {
            format!(
                "\"{}\"",
                v.replace('\\', "\\\\").replace('"', "\\\"")
            )
        };
        let mut fm = format!("---\nname: {name}\ndescription: {}\n", esc(&description));
        if let Some(t) = &toolset {
            fm.push_str(&format!("toolset: {t}\n"));
        }
        if let Some(m) = &model {
            fm.push_str(&format!("model: {}\n", esc(m)));
        }
        fm.push_str(&format!("worker: {worker}\n"));
        fm.push_str(&format!(
            "resources: {{mem_gb: {}, cpu_weight: {}, scratch_cap_gb: {}}}\n",
            resources.mem_gb.unwrap_or(4),
            resources.cpu_weight.unwrap_or(5),
            resources.scratch_cap_gb.unwrap_or(5),
        ));
        fm.push_str(&format!("version: {version}\n"));
        fm.push_str("---\n\n");
        fm.push_str(&system_prompt);
        fm.push('\n');

        // **先校验后落盘**（复查 P1-3）：坏 frontmatter 一旦 rename 进库，
        // load_role_library 会整库跳过该文件——角色凭空消失。
        roles::parse_role_content(&path, &fm)
            .map_err(|e| ToolError::ExecutionFailed(format!("生成的内容校验失败，未落盘: {e}")))?;

        std::fs::create_dir_all(roles_dir)
            .map_err(|e| ToolError::ExecutionFailed(format!("roles 目录创建失败: {e}")))?;
        let tmp = path.with_extension("md.tmp");
        std::fs::write(&tmp, &fm)
            .map_err(|e| ToolError::ExecutionFailed(format!("写入失败: {e}")))?;
        std::fs::rename(&tmp, &path)
            .map_err(|e| ToolError::ExecutionFailed(format!("落盘失败: {e}")))?;

        Ok(json!({
            "name": name,
            "version": version,
            "file": path.display().to_string(),
            "note": "角色已保存（热加载，下一次派单即生效）。",
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("name".to_string(), json!({
            "type": "string",
            "description": "角色名（小写字母/数字/-/_，同时是文件名）。已存在则更新并递增 version。"
        }));
        props.insert("description".to_string(), json!({
            "type": "string",
            "description": "一句话角色说明（何时派给它）。"
        }));
        props.insert("system_prompt".to_string(), json!({
            "type": "string",
            "description": "角色 system prompt 正文（职责、纪律、输出格式契约）。"
        }));
        props.insert("toolset".to_string(), json!({
            "type": "string",
            "description": "read_only | full | 逗号分隔工具名单（可选，默认继承全部）。"
        }));
        props.insert("model".to_string(), json!({
            "type": "string",
            "description": "模型覆盖（可选，缺省跟随宿主）。"
        }));
        props.insert("worker".to_string(), json!({
            "type": "string",
            "description": "internal | browser | external:<driver>（可选，默认 internal）。"
        }));
        tool_schema(
            "agent_role_upsert",
            "建改角色",
            JsonSchema::object(
                props,
                vec!["name".into(), "description".into(), "system_prompt".into()],
            ),
        )
    }
}

struct DispatchMemberHandler;

#[async_trait]
impl ToolHandler for DispatchMemberHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let Some(runner) = current_member_runner() else {
            return Err(ToolError::ExecutionFailed(
                "dispatch_member: 成员运行器未安装（当前宿主未接线）。".to_string(),
            ));
        };
        let role = params
            .get("role")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::ExecutionFailed("dispatch_member requires 'role'.".into()))?;
        let task = params
            .get("task")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::ExecutionFailed("dispatch_member requires 'task'.".into()))?;
        let criteria: Vec<String> = params
            .get("acceptance_criteria")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect()
            })
            .unwrap_or_default();
        let req = MemberDispatch {
            role,
            task,
            acceptance_criteria: criteria,
            max_turns: params.get("max_turns").and_then(|v| v.as_u64()).map(|v| v as u32),
            timeout_secs: params.get("timeout_secs").and_then(|v| v.as_u64()),
            attempts: 1,
            retry_of: None,
        };
        runner
            .dispatch(req)
            .await
            .map_err(ToolError::ExecutionFailed)
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("role".to_string(), json!({
            "type": "string",
            "description": "角色名（~/.hermix/agents/*.md 的 name；先 agent_role 未落地时可从 dispatch 报错里拿可用清单）"
        }));
        props.insert("task".to_string(), json!({
            "type": "string",
            "description": "工单正文：目标、范围、边界。这是成员的完成定义之源。"
        }));
        props.insert("acceptance_criteria".to_string(), json!({
            "type": "array", "items": { "type": "string" },
            "description": "验收判据列表（可选但强烈建议）。成员自检与 reviewer 验收共用。"
        }));
        props.insert("max_turns".to_string(), json!({
            "type": "integer", "minimum": 1,
            "description": "轮次预算（默认 100）。"
        }));
        props.insert("timeout_secs".to_string(), json!({
            "type": "integer", "minimum": 60,
            "description": "墙钟上限秒数（默认 14400 = 4 小时）。"
        }));
        tool_schema("dispatch_member", "派工单", JsonSchema::object(props, vec!["role".into(), "task".into()]))
    }
}

struct GetMemberResultHandler;

#[async_trait]
impl ToolHandler for GetMemberResultHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let Some(runner) = current_member_runner() else {
            return Err(ToolError::ExecutionFailed(
                "get_member_result: 成员运行器未安装。".to_string(),
            ));
        };
        let id = params
            .get("member_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::ExecutionFailed("get_member_result requires 'member_id'.".into()))?;
        Ok(runner.get(id).await.to_string())
    }

    fn schema(&self) -> ToolSchema {
        tool_schema(
            "get_member_result",
            "查成员工单",
            JsonSchema::object(
                [(
                    "member_id".to_string(),
                    json!({
                        "type": "string",
                        "description": "dispatch_member 返回的 member_id。"
                    }),
                )]
                .into_iter()
                .collect(),
                vec!["member_id".into()],
            ),
        )
    }
}

struct StopMemberHandler;

#[async_trait]
impl ToolHandler for StopMemberHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let Some(runner) = current_member_runner() else {
            return Err(ToolError::ExecutionFailed(
                "stop_member: 成员运行器未安装。".to_string(),
            ));
        };
        let id = params
            .get("member_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::ExecutionFailed("stop_member requires 'member_id'.".into()))?;
        let stopped = runner.stop(id).await;
        Ok(json!({
            "member_id": id,
            "status": if stopped { "cancelled" } else { "not_running" },
            "note": if stopped { "成员工单已中止，记录翻为 cancelled。" } else { "该 id 当前没有在运行（可能已完成或不存在）。" },
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        tool_schema(
            "stop_member",
            "止成员工单",
            JsonSchema::object(
                [(
                    "member_id".to_string(),
                    json!({
                        "type": "string",
                        "description": "dispatch_member 返回的 member_id。"
                    }),
                )]
                .into_iter()
                .collect(),
                vec!["member_id".into()],
            ),
        )
    }
}

/// 注册三个成员工具。**注册目标是 hermix-tools 的进程级注册表**——
/// agent_loop 侧的 `ToolRegistry` 只是桥接视图（handler 形状不同），
/// 真正的注册/发现走这份，服务器启动时注册一次，所有会话共享。
/// registry 拒绝重复注册，调用方保证进程一次（state 构建期调用即可）。
pub fn register_member_tools(registry: &hermix_tools::ToolRegistry) {
    let dispatch = Arc::new(DispatchMemberHandler);
    registry.register_with_risk(
        "dispatch_member",
        "member",
        dispatch.schema(),
        dispatch,
        Arc::new(|| true),
        vec![],
        true,
        "派一张长任务工单给团队成员（执行单元：独立会话、可停可查、对交付物负责）。\
         角色来自 ~/.hermix/agents/*.md（内置 reviewer/coder）。派单立即返回 member_id；\
         get_member_result 查进度与最终报告，stop_member 中止。\n\n\
         ## 与 delegate_task 的分界（勿混用）\n\
         delegate_task = 数据通道：读/分析一次、回传 ≤8K 结论，几秒到几分钟。\
         dispatch_member = 执行单元：承包整张工单（默认 100 轮 / 4 小时），\
         交付完整工作报告与验收。\n\n\
         ## 用法\n\
         编码/评审/长调查等需要多轮自驱的任务 → dispatch_member（务必给 acceptance_criteria）；\
         只要一份摘要/结论 → delegate_task。",
        "🧑‍🔧",
        None,
        RiskLevel::Caution,
        false,
    );

    let get = Arc::new(GetMemberResultHandler);
    registry.register_with_risk(
        "get_member_result",
        "member",
        get.schema(),
        get,
        Arc::new(|| true),
        vec![],
        true,
        "查成员工单状态：running（含最近步骤日志）或终态（含完整最终报告与轮次）。\
         记录持久化在 ~/.hermix/members/，重启后仍可查。",
        "📋",
        None,
        RiskLevel::Safe,
        false,
    );

    let stop = Arc::new(StopMemberHandler);
    registry.register_with_risk(
        "stop_member",
        "member",
        stop.schema(),
        stop,
        Arc::new(|| true),
        vec![],
        true,
        "中止一个成员工单（interrupt + abort 整个成员会话）。记录翻为 cancelled；\
         对已完成的 id 返回 not_running 而非报错。",
        "🛑",
        None,
        RiskLevel::Safe,
        false,
    );

    let review = Arc::new(ReviewMemberHandler);
    registry.register_with_risk(
        "review_member",
        "member",
        review.schema(),
        review,
        Arc::new(|| true),
        vec![],
        true,
        "验收一张成员工单（D3：验收人 ≠ 执行人）。内部派只读 reviewer 对照工单判据\
         核验交付物，产出 pass / warn / reject 三态判定与证据，写回成员记录。\
         reject 时把证据附进新工单重派（同一工单最多 2 次），仍不过升级问用户。",
        "🔍",
        None,
        RiskLevel::Caution,
        false,
    );

    let stats = Arc::new(MemberStatsHandler);
    registry.register_with_risk(
        "member_stats",
        "member",
        stats.schema(),
        stats,
        Arc::new(|| true),
        vec![],
        true,
        "角色评分（P6）：完成率、验收判定分布、平均轮次与墙钟，来自全部成员记录。
         ## 纪律
         修订角色（agent_role_upsert）前必须先看本表：只有评分证据显示某角色系统性         偏弱（reject 率高 / 轮次虚耗）才改 prompt 或 model，改完用新工单验证。",
        "📊",
        None,
        RiskLevel::Safe,
        false,
    );

    let role_list = Arc::new(AgentRoleListHandler);
    registry.register_with_risk(
        "agent_role_list",
        "member",
        role_list.schema(),
        role_list,
        Arc::new(|| true),
        vec![],
        true,
        "列出成员角色库（~/.hermix/agents/*.md）：名字、说明、worker 形态、工具面、\
         版本与坏文件清单。派单前先看有哪些工种。",
        "🗂️",
        None,
        RiskLevel::Safe,
        false,
    );

    let role_upsert = Arc::new(AgentRoleUpsertHandler);
    registry.register_with_risk(
        "agent_role_upsert",
        "member",
        role_upsert.schema(),
        role_upsert,
        Arc::new(|| true),
        vec![],
        true,
        "创建或更新成员角色（写 ~/.hermix/agents/<name>.md，已存在则 version+1、\
         保留 resources 档；热加载，下次派单即生效）。用户明确要求新增工种、或\
         按验收反馈修订角色 prompt 时使用——改角色前先给出修改理由。",
        "✏️",
        None,
        RiskLevel::Caution,
        false,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use hermix_core::{AgentError, LlmResponse};

    /// 一问即答的假 provider：首轮返回固定终稿文本（无工具调用）。
    struct FinalTextProvider(&'static str);

    #[async_trait]
    impl LlmProvider for FinalTextProvider {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&Value>,
        ) -> Result<LlmResponse, AgentError> {
            Ok(LlmResponse {
                message: Message {
                    role: MessageRole::Assistant,
                    content: Some(self.0.to_string()),
                    ..Default::default()
                },
                usage: None,
                model: "fake".to_string(),
                finish_reason: Some("stop".to_string()),
            })
        }
        fn chat_completion_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&Value>,
        ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
            futures::stream::empty().boxed()
        }
    }

    fn runner_with(tmp: &tempfile::TempDir, provider: Arc<dyn LlmProvider>) -> Arc<MemberRunner> {
        Arc::new(MemberRunner::new(MemberRunnerConfig {
            base_config: AgentConfig::default(),
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: provider,
            hermix_home: tmp.path().to_path_buf(),
            chunk_emitter: None,
            roles_dir: tmp.path().join("agents"),
        }))
    }

    /// 停派门是进程级全局态：派单类测试互斥执行，门测试持锁完成
    /// 安装与复位，避免并行把别的测试的派单拦下来。
    static DISPATCH_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 按工单内容分流的假 provider：验收工单 → 三态判定文本；
    /// 普通工单 → 执行报告。
    struct ScriptedMemberProvider;

    #[async_trait]
    impl LlmProvider for ScriptedMemberProvider {
        async fn chat_completion(
            &self,
            messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&Value>,
        ) -> Result<LlmResponse, AgentError> {
            let is_review = messages.iter().any(|m| {
                m.content.as_deref().is_some_and(|c| c.contains("待验收工单"))
            });
            let text = if is_review {
                "判定: reject
判据:
  - [fail] 测试通过 — 未找到任何测试输出证据
备注: 交付物未包含测试。"
            } else {
                "交付完成：功能 X 已实现，自测全绿。"
            };
            Ok(LlmResponse {
                message: Message {
                    role: MessageRole::Assistant,
                    content: Some(text.to_string()),
                    ..Default::default()
                },
                usage: None,
                model: "fake".to_string(),
                finish_reason: Some("stop".to_string()),
            })
        }
        fn chat_completion_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&Value>,
        ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
            futures::stream::empty().boxed()
        }
    }

    #[tokio::test]
    async fn member_clarify_forwards_to_parent_session() {
        use hermix_tools::ChannelClarifyBackend;

        let dispatcher = hermix_tools::ClarifyDispatcher::new();
        let backend = MemberClarifyBackend {
            inner: ChannelClarifyBackend::new(dispatcher.clone()),
            parent_session_id: "parent-sess".to_string(),
            role: "coder".to_string(),
            member_id: "member-test".to_string(),
        };
        let ask = tokio::spawn(async move {
            use hermix_tools::ClarifyBackend as _;
            backend
                .ask("请把验证码告诉我", None, Some("member::x".into()))
                .await
                .expect("ask")
        });
        // 泵视角：pending 应盖**父会话** id（成员自己的 id 被替换掉）。
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let snaps = dispatcher.next_snapshot().await;
        assert_eq!(snaps.len(), 1, "应有一条 pending");
        assert_eq!(snaps[0].session_id, "parent-sess", "session 应为父会话");
        assert!(
            snaps[0].question.contains("[成员 coder · member-test 提问]"),
            "应带归属前缀：{}",
            snaps[0].question
        );
        let rid = snaps[0].request_id.clone();
        dispatcher.respond_by_id(&rid, "886744").await.expect("respond");
        let answer = ask.await.expect("join");
        assert!(answer.contains("886744"), "answer={answer}");
    }

    #[test]
    fn review_verdict_parsing_covers_contract_forms() {
        assert_eq!(parse_review_verdict("判定: pass
判据: …"), Some("pass"));
        assert_eq!(parse_review_verdict("判定：warn"), Some("warn"));
        assert_eq!(
            parse_review_verdict("前置噪音
  判定: reject —— 证据不足"),
            Some("reject")
        );
        assert_eq!(parse_review_verdict("判定: 也许吧"), None);
        assert_eq!(parse_review_verdict("没有任何判定行"), None);
    }

    #[tokio::test]
    async fn review_member_records_verdict_and_evidence() {
        let _guard = DISPATCH_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let runner = runner_with(&tmp, Arc::new(ScriptedMemberProvider));
        let out = runner
            .dispatch(MemberDispatch {
                role: "coder".into(),
                task: "实现功能 X".into(),
                acceptance_criteria: vec!["测试通过".into()],
                max_turns: Some(4),
                timeout_secs: Some(60),
                attempts: 1,
                retry_of: None,
            })
            .await
            .expect("dispatch");
        let id = serde_json::from_str::<Value>(&out).expect("json")["member_id"]
            .as_str()
            .expect("id")
            .to_string();
        wait_terminal(&runner, &id).await;

        let review_out = runner.review_member(&id).await.expect("review");
        let review: Value = serde_json::from_str(&review_out).expect("review json");
        assert_eq!(review["verdict"], "reject", "review={review_out}");
        assert!(review["evidence"].as_str().unwrap_or("").contains("判据"));

        // 判定写回了成员记录，get 可见。
        let after = runner.get(&id).await;
        assert_eq!(after["review"]["verdict"], "reject");
        assert!(after["review"]["reviewer_member_id"].as_str().unwrap_or("").starts_with("member-"));
    }

    #[tokio::test]
    async fn review_reject_auto_retries_twice_then_escalates() {
        let _guard = DISPATCH_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        // reviewer 恒 reject（ScriptedMemberProvider 的评审分支固定 reject）。
        let runner = runner_with(&tmp, Arc::new(ScriptedMemberProvider));
        let out = runner
            .dispatch(MemberDispatch {
                role: "coder".into(),
                task: "实现功能 Y".into(),
                acceptance_criteria: vec!["测试通过".into()],
                max_turns: Some(2),
                timeout_secs: Some(30),
                attempts: 1,
                retry_of: None,
            })
            .await
            .expect("dispatch");
        let current = serde_json::from_str::<Value>(&out).expect("json")["member_id"]
            .as_str().expect("id").to_string();
        wait_terminal(&runner, &current).await;

        // 第一次 reject → attempts=1 < 3 → 自动重派。
        let r1: Value = serde_json::from_str(&runner.review_member(&current).await.expect("r1")).expect("json");
        assert_eq!(r1["verdict"], "reject");
        let retry1 = r1["redispatched_member_id"].as_str().expect("应自动重派").to_string();
        wait_terminal(&runner, &retry1).await;
        let t1 = runner.get(&retry1).await;
        assert_eq!(t1["attempts"], 2, "重派单 attempts 应为 2：{t1}");
        assert_eq!(t1["retry_of"], current);

        // 第二次（重派单 attempts=2）→ 再派一次。
        let r2: Value = serde_json::from_str(&runner.review_member(&retry1).await.expect("r2")).expect("json");
        let retry2 = r2["redispatched_member_id"].as_str().expect("第二次重派").to_string();
        wait_terminal(&runner, &retry2).await;
        assert_eq!(runner.get(&retry2).await["attempts"], 3);

        // 第三次（attempts=3）→ 到顶：不再重派，升级提示。
        let r3: Value = serde_json::from_str(&runner.review_member(&retry2).await.expect("r3")).expect("json");
        assert_eq!(r3["verdict"], "reject");
        assert!(r3["redispatched_member_id"].is_null(), "第三次不应重派：{r3}");
        assert!(r3["next_step"].as_str().unwrap_or("").contains("上限"), "应提示升级：{r3}");
    }

    #[tokio::test]
    async fn review_member_rejects_unknown_id() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let runner = runner_with(&tmp, Arc::new(ScriptedMemberProvider));
        let err = runner.review_member("member-nope").await.expect_err("unknown");
        assert!(err.contains("不存在"), "err={err}");
    }

    #[tokio::test]
    async fn role_upsert_bumps_version_and_lists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let runner = runner_with(&tmp, Arc::new(ScriptedMemberProvider));
        install_member_runner(runner.clone(), false);

        let upsert = AgentRoleUpsertHandler;
        let v1 = upsert
            .execute(json!({
                "name": "investigator",
                "description": "长调查专员",
                "system_prompt": "你是调查员。"
            }))
            .await
            .expect("upsert v1");
        let parsed: Value = serde_json::from_str(&v1).expect("json");
        assert_eq!(parsed["version"], 1, "v1={v1}");

        let v2 = upsert
            .execute(json!({
                "name": "investigator",
                "description": "长调查专员（改）",
                "system_prompt": "你是调查员，输出带证据索引。"
            }))
            .await
            .expect("upsert v2");
        let parsed2: Value = serde_json::from_str(&v2).expect("json");
        assert_eq!(parsed2["version"], 2, "重复 upsert 应递增版本");

        let list = AgentRoleListHandler.execute(json!({})).await.expect("list");
        let list: Value = serde_json::from_str(&list).expect("json");
        let inv = list["roles"]
            .as_array()
            .expect("roles")
            .iter()
            .find(|r| r["name"] == "investigator")
            .expect("investigator 应在列表里");
        assert_eq!(inv["version"], 2);
        assert_eq!(inv["worker"], "internal");
    }

    #[tokio::test]
    async fn role_upsert_rejects_unsafe_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let runner = runner_with(&tmp, Arc::new(ScriptedMemberProvider));
        install_member_runner(runner, false);
        let err = AgentRoleUpsertHandler
            .execute(json!({
                "name": "../evil",
                "description": "x",
                "system_prompt": "y"
            }))
            .await
            .expect_err("unsafe name should fail");
        assert!(err.to_string().contains("只允许"), "err={err}");
    }

    async fn wait_terminal(runner: &MemberRunner, id: &str) -> Value {
        for _ in 0..200 {
            let v = runner.get(id).await;
            if v["status"] != "running" && v["status"] != "not_found" {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("成员 10 秒内未到终态");
    }

    #[tokio::test]
    async fn dispatch_unknown_role_lists_available() {
        let _guard = DISPATCH_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let runner = runner_with(&tmp, Arc::new(FinalTextProvider("x")));
        let err = runner
            .dispatch(MemberDispatch {
                role: "ghost".into(),
                task: "t".into(),
                acceptance_criteria: vec![],
                max_turns: None,
                timeout_secs: None,
                attempts: 1,
                retry_of: None,
            })
            .await
            .expect_err("unknown role should fail");
        assert!(err.contains("不存在"), "err={err}");
        assert!(err.contains("reviewer") && err.contains("coder"), "应列出内置角色：{err}");
    }

    #[tokio::test]
    async fn dispatch_gate_blocks_new_tickets_when_resource_breach() {
        let _guard = DISPATCH_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let runner = runner_with(&tmp, Arc::new(FinalTextProvider("x")));
        // 门只在 true 时拦截；测试完必须摘掉，避免污染其他测试。
        install_dispatch_gate(Arc::new(|| true));
        let err = runner
            .dispatch(MemberDispatch {
                role: "coder".into(),
                task: "t".into(),
                acceptance_criteria: vec![],
                max_turns: None,
                timeout_secs: None,
                attempts: 1,
                retry_of: None,
            })
            .await
            .expect_err("gate should block");
        assert!(err.contains("停派"), "err={err}");
        install_dispatch_gate(Arc::new(|| false));
        let ok = runner
            .dispatch(MemberDispatch {
                role: "coder".into(),
                task: "t".into(),
                acceptance_criteria: vec![],
                max_turns: Some(2),
                timeout_secs: Some(30),
                attempts: 1,
                retry_of: None,
            })
            .await
            .expect("gate open should dispatch");
        assert!(ok.contains("member-"));
    }

    #[tokio::test]
    async fn dispatch_external_worker_is_p3_rejected() {
        let _guard = DISPATCH_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let roles_dir = tmp.path().join("agents");
        std::fs::create_dir_all(&roles_dir).expect("mkdir");
        std::fs::write(
            roles_dir.join("ext.md"),
            "---\nname: ext\ndescription: d\nworker: external:codex\n---\nbody",
        )
        .expect("write");
        let runner = runner_with(&tmp, Arc::new(FinalTextProvider("x")));
        let err = runner
            .dispatch(MemberDispatch {
                role: "ext".into(),
                task: "t".into(),
                acceptance_criteria: vec![],
                max_turns: None,
                timeout_secs: None,
                attempts: 1,
                retry_of: None,
            })
            .await
            .expect_err("external worker not yet");
        assert!(err.contains("无可用无头入口"), "err={err}");
    }

    #[tokio::test]
    async fn dispatch_runs_to_completed_with_report() {
        let _guard = DISPATCH_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let runner = runner_with(&tmp, Arc::new(FinalTextProvider("交付完成：功能 X 已实现，测试全绿。")));
        let out = runner
            .dispatch(MemberDispatch {
                role: "coder".into(),
                task: "实现功能 X".into(),
                acceptance_criteria: vec!["测试通过".into()],
                max_turns: Some(4),
                timeout_secs: Some(60),
                attempts: 1,
                retry_of: None,
            })
            .await
            .expect("dispatch");
        let parsed: Value = serde_json::from_str(&out).expect("json");
        let id = parsed["member_id"].as_str().expect("member_id").to_string();
        assert!(id.starts_with("member-"));
        assert_eq!(parsed["status"], "dispatched");

        let terminal = wait_terminal(&runner, &id).await;
        assert_eq!(terminal["status"], "completed", "terminal={terminal}");
        let report = terminal["final_report"].as_str().expect("report");
        assert!(report.contains("交付完成"), "report={report}");
        assert_eq!(terminal["total_turns"], 1);
    }

    #[tokio::test]
    async fn report_truncation_is_char_safe() {
        let long: String = "汉".repeat(MEMBER_REPORT_MAX_CHARS + 500);
        let cut = extract_member_report(&[Message {
            role: MessageRole::Assistant,
            content: Some(long),
            ..Default::default()
        }]);
        assert!(cut.contains("已截断"));
        assert!(cut.chars().count() <= MEMBER_REPORT_MAX_CHARS + 20);
    }

    #[tokio::test]
    async fn list_orders_newest_first_and_strips_report() {
        let _guard = DISPATCH_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let runner = runner_with(&tmp, Arc::new(FinalTextProvider("报告正文。")));
        for task in ["第一单", "第二单"] {
            let out = runner
                .dispatch(MemberDispatch {
                    role: "coder".into(),
                    task: task.into(),
                    acceptance_criteria: vec![],
                    max_turns: Some(2),
                    timeout_secs: Some(30),
                    attempts: 1,
                    retry_of: None,
                })
                .await
                .expect("dispatch");
            let id = serde_json::from_str::<Value>(&out).expect("json")["member_id"]
                .as_str().expect("id").to_string();
            wait_terminal(&runner, &id).await;
        }
        let list = runner.list().await;
        assert_eq!(list.len(), 2);
        // 新工单在前；列表不携带报告正文与步骤日志。
        assert_eq!(list[0]["task"], "第二单");
        assert_eq!(list[1]["task"], "第一单");
        assert!(list[0].get("final_report").is_none(), "列表不应带报告：{:?}", list[0]);
        assert!(list[0].get("recent_steps").is_none());
    }

    #[tokio::test]
    async fn stop_unknown_returns_not_running_shape() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let runner = runner_with(&tmp, Arc::new(FinalTextProvider("x")));
        assert!(!runner.stop("member-nope").await);
        let v = runner.get("member-nope").await;
        assert_eq!(v["status"], "not_found");
    }
}
