//! Shared HTTP server state: HttpServerState and its methods.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;


use hermix_config::GatewayConfig;
use hermix_core::AgentError;
use hermix_tools::ToolRegistry;

use super::*;

/// 自动化锚定投递的会话存在性探针。
///
/// 存在的理由是一个具体的失败模式：`LocalAgentService::send_message`
/// 对 sessions 表做 UPSERT，所以把自动化结果投进一个已被用户删除的
/// 对话**不会报错**——它会把那个 id 复活成一条 `title`/`workspace`
/// 全为 NULL 的孤儿行，出现在侧栏里，且用户无法解释它从哪来。
///
/// 判定为「不存在」时走回退路径（独立 `automation:*` 会话）并带上
/// 可见原因，而不是静默复活。
struct SessionExistenceProbe {
    persistence: std::sync::Arc<hermix_agent::session_persistence::SessionPersistence>,
}

impl automation_trigger::SessionProbe for SessionExistenceProbe {
    fn session_exists(&self, session_id: &str) -> bool {
        match self.persistence.get_session_title(session_id) {
            Ok(Some(_)) => true,
            Ok(None) => false,
            Err(e) => {
                // 查不动 ≠ 不存在。此时回退是唯一安全的方向：真去
                // dispatch 的最坏结果就是上面那个孤儿会话。
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "automation: session existence check failed; treating as gone"
                );
                false
            }
        }
    }
}

/// EffectLog → EffectSink 适配器（P3 Effect Sandwich 装配）。
/// 单笔持久化失败吞掉降级为不记录——三明治是崩溃恢复依据，
/// 不能阻断工具执行。
///
/// `registry` 用 Weak 持 ToolRegistry 查 risk_level——Safe 视为幂等
/// （只读 / 无副作用），恢复时按幂等语义重放；非幂等走 needs_review。
/// Weak 避免 EffectLogSink(Arc) → ToolRegistry 的引用环。
struct EffectLogSink {
    log: hermix_task::EffectLog,
    registry: std::sync::Weak<ToolRegistry>,
}

/// §3.4 L2：TaskRecord 启动对账。启动时残留 `running` = 上次进程死在
/// sub-agent 执行中途；Checkpoint.from_step 未细化无从续跑——唯一诚实
/// 的处置是落成 `Terminal(Faulted)`，让恢复视图不留僵尸。只读扫描 +
/// best-effort 写入：对账失败不阻断启动。
fn reconcile_task_records(store: &hermix_task::TaskRecordStore) {
    let running = match store.scan_unfinished() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("startup TaskRecord recovery scan failed: {e}");
            return;
        }
    };
    if running.is_empty() {
        return;
    }
    tracing::warn!(
        count = running.len(),
        "startup recovery: {} sub-agent TaskRecord(s) stuck in running \
         from a previous run; marking Faulted (no checkpoint to resume)",
        running.len()
    );
    for rec in &running {
        let faulted = hermix_task::TaskRecord::terminal(
            rec.id.clone(),
            rec.conversation_id.clone(),
            hermix_task::TaskOutcome::Faulted {
                error: "进程重启，sub-agent 执行中断（启动对账标记）".into(),
            },
        );
        if let Err(e) = store.save(&faulted) {
            tracing::warn!(
                error = %e, id = %rec.id,
                "startup recovery: failed to mark TaskRecord Faulted"
            );
        }
    }
}

impl hermix_tools::EffectSink for EffectLogSink {
    fn record_intent(&self, tool: &str, args_json: &str) -> String {
        // 查注册表拿 risk_level；查不到（工具未注册 / Arc 已 drop）
        // 按非幂等保守侧。Safe ⇒ 幂等（只读类工具）。
        let idempotent = self.registry.upgrade().is_some_and(|reg| {
            reg.risk_level(tool) == Some(hermix_core::traits::RiskLevel::Safe)
        });
        let intent = hermix_task::ToolIntent {
            id: hermix_task::effect::new_intent_id(),
            task_id: String::new(),
            tool: tool.to_string(),
            args_json: args_json.to_string(),
            idempotent,
            created_at: chrono::Utc::now().timestamp(),
        };
        let id = intent.id.clone();
        let _ = self.log.record_intent(&intent);
        id
    }

    fn record_outcome(&self, intent_id: &str, status: &str, summary: &str) {
        let _ = self.log.resolve_intent(intent_id, status, summary);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarifyBroadcast {
    pub session_id: String,
    pub request_id: String,
    pub question: String,
    pub choices: Vec<String>,
}

/// 一个 IM / 设备会话的对外可见状态。
///
/// 字段刻意只到「够手机端画任务列表」为止：`first_line` 会被原样送到手机上，
/// 所以它只取正文首行（`session.rs::first_line` 截 40 字），**不携带完整
/// 会话内容**。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChannelSessionView {
    /// `{platform}:{user}`，与 `ChannelRuntime::key` 同形（显示用）。
    pub key: String,
    /// 手机端调用 `interrupt_session` 所需的会话 ID（格式 `im:device:{device_id}`）。
    pub sid: String,
    /// `running` | `queued`。
    pub state: &'static str,
    /// 正文首行（截断）。
    pub first_line: String,
    /// 排队位次（1-based）；`running` 为 None。
    pub position: Option<usize>,
}

#[derive(Clone)]
pub struct HttpServerState {
    pub config: Arc<GatewayConfig>,
    pub tool_registry: Arc<ToolRegistry>,
    pub hermix_home: std::path::PathBuf,
    pub session_persistence: Arc<hermix_agent::session_persistence::SessionPersistence>,
    pub skill_store: Option<Arc<dyn hermix_skills::SkillStore>>,
    /// 进程级装配：持有 provider 注册 guard，保证 fs/shell/sandbox/transport
    /// 服务与进程同寿命。`Boot::seams()` 把同一组 provider 实例传给
    /// `register_builtin_tools`——避免业务侧各自 `Arc::new(LocalFs)` 重复构造。
    /// `Arc` 包裹是因为 `Boot` 持有不可 Clone 的 `ServiceGuard`，
    /// 而 `HttpServerState` 自身需要 Clone（多个 WS handler 共享）。
    pub boot: Arc<hermix_context::Boot>,
    pub runtime_gateway_running: Option<Arc<AtomicBool>>,
    pub(crate) agent_service: Arc<dyn hermix_core::traits::AgentService>,
    pub approval_registry: ApprovalRegistry,
    /// Pending `clarify` tool calls wait here. The desktop UI subscribes
    /// to the dispatcher via [`Self::clarify_subscribe`] and pushes the
    /// user's answer back over WebSocket.
    pub clarify_dispatcher: hermix_tools::ClarifyDispatcher,
    /// Broadcast of "new pending clarify snapshot available" — every
    /// WebSocket that wants to render question cards subscribes a clone.
    pub clarify_broadcast: tokio::sync::broadcast::Sender<ClarifyBroadcast>,
    /// In-memory catalog of expert role packages. The desktop client pushes
    /// the bundled built-ins into this registry at startup; every expert
    /// surfaced in the UI must come from here.
    pub expert_registry: hermix_skills::ExpertRegistry,
    /// Background scheduler that fires automation tasks at their cron
    /// schedule. Started automatically by `build*`; can be shut down via
    /// `AutomationScheduler::shutdown()`.
    pub automation_scheduler: Option<Arc<crate::dashboard::automation::AutomationScheduler>>,
    /// 资源水位巡检（总管方案 P0.5）：C 盘水位 + scratch 配额 + 停派门。
    /// `None` = 未启动巡检（如 stdio 模式下的最小装配）。
    pub resource_patrol: Option<Arc<crate::resource_patrol::ResourcePatrol>>,
    /// Persistent user "always allow" command list, loaded from
    /// `$HERMIX_HOME/approval_allowlist.json` at startup. Mutated
    /// when the user picks `Always` on a `RequestApproval` card.
    /// Wrapped in `std::sync::Mutex` for sync access from the
    /// pre-tool-dispatch hot path (the closure is called on every
    /// tool dispatch, not async). `None` means "no allowlist
    /// configured" — the pre-approval checker still runs against
    /// the per-session bypass set, but skips the disk-backed
    /// `UserAllowlist` check.
    pub user_allowlist: Arc<std::sync::Mutex<Option<hermix_tools::UserAllowlist>>>,
    /// Per-session in-memory bypass set: commands the user picked
    /// `Session` on. Cleared on server restart (matches upstream
    /// Hermes semantics). Keys are `session_id`, values are a
    /// list of compiled regex patterns (mirroring `UserAllowlist`).
    pub session_bypass:
        Arc<std::sync::Mutex<HashMap<String, Vec<Regex>>>>,
    /// per-`(session_id, content_hash)` send-dedup.
    /// Prevents retry storms (WS auto-retry + queue re-drain) from
    /// triggering N parallel `send_message_stream` calls for the
    /// same user message — the visible symptom was 9 calls in 2
    /// minutes for a single "你好" send. Window: 10s; older entries
    /// are GC'd on every insert. In-memory only, cleared on restart
    /// (good enough — restart also clears the WS clients that
    /// produced the storm).
    pub recent_sends: Arc<Mutex<HashMap<String, std::time::Instant>>>,
    /// set of session IDs that currently have an active
    /// `send_message_stream` running. Prevents concurrent streams for
    /// the same session — the frontend retry mechanism (MAX_RETRIES=3
    /// in useStreamChat.ts) can race with a disconnected WS to start
    /// a second agent for the same user message. Without this guard,
    /// two agent loops run in parallel, producing duplicate tool calls
    /// and doubling the response text — the user sees "LLM 重复回复".
    pub active_sessions: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// per-active-stream `JoinHandle`, keyed by the same
    /// `dedup_key(session_id, text)` used for `active_sessions`. Enables
    /// the "user re-sends the original question after the LLM self-
    /// interrupts / hangs" recovery path: when a fresh send arrives for
    /// a (session, text) that STILL has a live task, we `abort()` the
    /// stale task, free the lock, and let the new send take over —
    /// instead of silently dropping the retry as a "duplicate".
    pub active_handles: Arc<std::sync::Mutex<std::collections::HashMap<String, tokio::task::JoinHandle<()>>>>,
    /// Per-(session,text) pending grace timers from a soft-disconnect. When
    /// the client reconnects and takes over the same key, we abort the
    /// pending timer so it doesn't later `remove` the NEW handle the
    /// reconnect just inserted (which would orphan the reconnect's agent
    /// and let a 2nd retry spawn a duplicate task for the same prompt).
    pub pending_grace: Arc<std::sync::Mutex<std::collections::HashMap<String, tokio::task::JoinHandle<()>>>>,
    /// Per-(session,text) final assistant content for a COMPLETED stream.
    /// Lets a reconnect that lost its `Done` frame short-circuit to a
    /// synthetic Done (with the answer) instead of re-running the whole
    /// task — which would double-execute non-idempotent tools. Cleared
    /// when a fresh agent starts for that key.
    pub completed_keys: Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
    /// L3 seq 源（P4，设计 §3.5）：进程级单调递增计数器。
    /// 每条 Envelope 下行帧带 seq，客户端重连用最后收到的 seq 锚
    /// 重新订阅；seq 间隙允许（客户端只做大小比较）。`AtomicU64`
    /// 无锁——广播热路径每次 fetch_add(1)。
    ///
    /// P5 attachment 模型落地后此字段被 SessionScopus seq 域替代
    /// （每 session 独立计数，支持快照压缩后重连补发）。本阶段
    /// 进程级 16n 已满足「同视图」验收（设计 §4：任何时刻接入
    /// 都能复原同一会话状态）。
    pub seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// P5 多端 attachment 锚（设计 §3.5）：进程级单调计数器，
    /// 每条 WS 连接建立时 `fetch_add(1)` 分配 attachment_id。
    /// 与 `seq` 同一模式（`AtomicU64` 无锁，per-connection 取号
    /// 在热路径外，开销可忽略）。可选——老前端忽略 `attachment_id`
    /// 字段不报错；新前端读取此值区分"同一 attachment 重连"与
    /// "新 attachment 挂载"（zombie 分支 attachment-aware 改造用）。
    pub attachment_seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// H3 — agent "consciousness stream" tracker. The
    /// desktop UI subscribes to this over WebSocket and renders the
    /// samples as a full-screen ECG visualization. Cheap to clone
    /// (single `Arc` over an inner mutex-protected ring buffer).
    pub consciousness_tracker: hermix_agent::ConsciousnessTracker,
    /// TickLoop lifecycle manager. Default ON at startup; can be
    /// toggled via `POST /api/v1/tickloop`.
    pub tickloop_manager: hermix_runtime::TickLoopManager,
    /// MCP (Model Context Protocol) manager — connects to the external
    /// MCP servers the user configured in the dashboard's "连接器" tab,
    /// registers their tools into the shared `tool_registry`, and
    /// routes `tools/call` JSON-RPC back into the right server.
    /// Wrapped in `Arc` so the per-tool handlers (which live in the
    /// `ToolRegistry`) can hold a `Weak` back-reference without
    /// forming a cycle. Earlier versions declared `McpManager` but
    /// never constructed it — the manager is wired here, once, at
    /// server startup, immediately after `tool_registry` is built so
    /// the very first LLM turn sees MCP tools.
    pub mcp_manager: Arc<hermix_mcp::McpManager>,
    /// Per-process broadcast sender for todo mutations. The `todo` tool
    /// holds the same sender; the WS handler subscribes per-session
    /// (filtering by `session_id` carried in `TodoChange.session_id`
    /// — currently `None` and filtered on the WS side via the
    /// conversation's session id, see `handle_ws_stream`).
    pub todo_event_tx: Option<tokio::sync::broadcast::Sender<hermix_tools::tools::todo::TodoChange>>,
    /// Orchestration event bus. Cross-crate channel
    /// for Task lifecycle, automation triggers, and result delivery
    /// events. Tool backends and the agent loop hold a clone to emit;
    /// the WS handler subscribes to forward events to connected clients.
    pub orchestration_bus: OrchestrationEventBus,
    /// ( / ) Lightweight metric collector.
    /// In-memory accumulation with batch flush to SQLite. Used by
    /// the `/v1/metrics` dashboard endpoint.
    pub metrics: crate::metrics::MetricsCollector,
    /// Task lifecycle service. Backed by
    /// [`hermix_task::TaskRepository`] pointing at the same
    /// `sessions.db` (铁律 D/WAL). `None` when the DB path can't be
    /// resolved (e.g. in test fixtures).
    pub task_service: Option<std::sync::Arc<hermix_task::TaskService>>,
    /// TaskRecord 持久状态机（§3.4 L2）。与 `EffectLog` 同库
    /// `effects.db`，崩溃恢复协议靠它做任务级判定。
    /// `None` 时端点降级 503（测试 / CLI 模式）。
    pub task_record_store: Option<std::sync::Arc<hermix_task::TaskRecordStore>>,
    /// IM 通道的会话视图快照，手机端 `GET /v1/tasks` 的唯一数据源。
    ///
    /// **state 只存不算**：`running` / `queues` 的真相在 `ChannelRuntime` 里，
    /// 由它全量重建后写进来（`crate::mobile::publish_channel_sessions`）。
    /// 反过来让 HTTP 层去问通道，等于让 state 依赖 channels——那条依赖是反的。
    pub channel_sessions: std::sync::Arc<std::sync::Mutex<Vec<ChannelSessionView>>>,
    /// Automation lifecycle service. Backed by
    /// [`hermix_automation::AutomationRepository`] pointing at the same
    /// `sessions.db` (铁律 D/WAL). `None` when the DB path can't be
    /// resolved (e.g. in test fixtures).
    pub automation_service: Option<std::sync::Arc<hermix_automation::AutomationService>>,
    /// Handle to the automation engine scheduler
    /// (from `hermix-automation` crate). Started automatically by
    /// `build_with_hermes_home`; triggers go through the unified
    /// `agent_service.send_message()` dispatch path (铁律 DD).
    pub automation_engine_scheduler: Option<automation_trigger::AutomationSchedulerHandle>,
    /// Custom app manager — manages user-facing desktop applications.
    /// `None` when not initialized (e.g. in test fixtures or CLI mode).
    pub custom_app_manager: Option<Arc<hermix_plugin::PluginManager>>,
    /// §5.5 第 3/4 层：已配对设备注册表（`<hermix_home>/devices.db`）。
    /// `None` = 打不开库（测试 / 只读环境），此时远程一律 Unpaired。
    pub device_registry: Option<Arc<crate::pairing::DeviceRegistry>>,
    /// 请求签名 nonce 缓存，**与 HTTP 面的 `HttpSecurity::nonce_cache` 是两个
    /// 实例**。
    ///
    /// 刻意不共享：共享要改 `HttpSecurity::new` 的构造签名并牵动中间件装配，
    /// 而分开是安全的——签名覆盖 `method\npath`，跨面重放必然因路径不匹配而
    /// HMAC 失配（`/v1/ws-stream/{sid}` 没有任何 HTTP 路由与之同路径），能真正
    /// 重放的只有同一个面，而那由该面自己的缓存兜住。TS 绕过要靠改这里。
    pub nonce_cache: crate::request_signature::NonceCache,
    /// 一次性配对会话管理器（进程内，TTL 120s）。
    pub pairing_sessions: Arc<crate::pairing::PairingSessions>,
    /// relay 连接设置（`<hermix_home>/relay.json`）。`tokio::sync::Mutex`
    /// 而不是 `std::sync`：`PUT /api/relay/config` 是 async handler，
    /// 在异步上下文里持 std 锁是经典的自死锁来源。外面再套一层 `Arc` 是因为
    /// `HttpServerState` 自身要 `Clone`（WS handler 都持有它的克隆）。
    pub relay_settings: Arc<tokio::sync::Mutex<crate::relay_settings::RelaySettings>>,
    /// relay 出站连接状态，供 `/api/relay/config` 回给界面。内部已是 `Arc`，
    /// `RelayStatus` 本身可以直接克隆共享。
    pub relay_status: crate::relay_client::RelayStatus,
    /// relay 最近一次连接失败的原因。`RelayStatus` 只记状态不记原因，
    /// 而「连不上」的原因差别很大（DNS / 401 / 协议版本），界面要能说清。
    pub relay_last_error: Arc<tokio::sync::Mutex<Option<String>>>,
    /// 出站连接任务是否已在跑。启动拉起一次；`reset_identity` 重置后要再拉起，
    /// 没有这道闸，连点两次重置就会有两个循环互相把对方的连接挤下线。
    pub relay_connector_running: Arc<std::sync::atomic::AtomicBool>,
}

impl HttpServerState {
    /// Subscribe to the broadcast of new pending `clarify` requests. Each
    /// WebSocket call this once at connect-time; the returned receiver
    /// yields one [`ClarifyBroadcast`] per freshly enqueued request.
    pub fn clarify_subscribe(&self) -> tokio::sync::broadcast::Receiver<ClarifyBroadcast> {
        self.clarify_broadcast.subscribe()
    }

    /// Register the `task_create` / `task_update` /
    /// `task_list` tools into the tool registry, backed by the
    /// `TaskService`. No-op if `task_service` is `None`.
    pub fn register_task_tracker_tools(&self) {
        if let Some(svc) = &self.task_service {
            let backend = Arc::new(task_tracker_adapter::TaskTrackerBackendImpl::new(
                svc.clone(),
            ));
            hermix_tools::tools::task_tracker::register(&self.tool_registry, backend);
            tracing::info!("Registered task_tracker tools (task_create, task_update, task_list, task_get, present_files)");
        }
    }

    /// Access the agent service that backs chat endpoints. Always present
    /// — the constructor takes care of building a `LocalAgentService`
    /// (or honouring a test-supplied [`AgentService`] mock) before
    /// exposing the state to the router.
    /// Polish/automation rewrite endpoints live in
    /// [`crate::dashboard::polish`] and call this to dispatch a single
    /// stateless LLM turn. They build an ad-hoc session id, send one
    /// user turn, then call `reset_session` so the parent conversation
    /// history stays untouched.
    pub fn agent_service(&self) -> Arc<dyn hermix_core::traits::AgentService> {
        self.agent_service.clone()
    }

    /// Build dashboard state using the default [`hermix_config::hermix_home`] resolution.
    pub async fn build(config: GatewayConfig) -> Result<Self, AgentError> {
        Self::build_with_hermes_home(config, hermix_config::hermix_home()).await
    }

    /// Build dashboard state with an explicit Hermes data directory (sessions DB, cron dir,
    /// `dashboard_plugins.json`, etc.). Useful for tests and alternate install layouts.
    pub async fn build_with_hermes_home(
        mut config: GatewayConfig,
        hermix_home: std::path::PathBuf,
    ) -> Result<Self, AgentError> {
        // ── 内置免费渠道：摘除 → auto-correct → seed → 兜底默认模型。
        //
        // 顺序是有意的，不能交换：
        //  · 摘除必须早于 auto-correct，否则 config.model 会留下指向已被
        //    删除 provider 的孤儿；
        //  · seed 必须在 auto-correct 之后，否则刚写进去的 `custom-free-*`
        //    会参与"挑默认厂商"的竞争（auto_correct_model 本身也已把它们
        //    排除在候选之外）；
        //  · 免费渠道只在"一个能用的厂商都没有"时充当默认模型，绝不抢
        //    用户已配好的厂商。
        let mut config_dirty = false;

        if !config.free_channel.enabled && hermix_config::free_channel::strip(&mut config) {
            config_dirty = true;
            tracing::info!("startup: free channel disabled — removed custom-free-* providers");
        }

        // ── Auto-correct config.model BEFORE building the agent
        // service. Without this, a fresh install with only
        // `MINIMAX_API_KEY` in `.env` (no `config.yaml`) starts with
        // `config.model = None`, the runtime falls back to
        // `fallback_model()`, and on the OLD binary that fallback
        // silently routed to `openai:gpt-4o` — leaving the user
        // staring at a "missing API key for openai" error even
        // though they had configured MiniMax. We now pick a
        // provider that actually has a key and persist the
        // correction so subsequent restarts load it.
        if let Some(report) = hermix_config::auto_correct_model(&mut config) {
            tracing::info!(
                before = ?report.before,
                after = %report.after,
                provider = %report.provider,
                reason = %report.reason,
                configured = ?report.configured_providers,
                "startup: auto-corrected config.model"
            );
            config_dirty = true;
        }

        if hermix_config::free_channel::ensure_seeded(&mut config) {
            config_dirty = true;
            tracing::info!(
                providers = ?hermix_config::free_channel::FREE_CHANNELS
                    .iter()
                    .map(|c| c.id)
                    .collect::<Vec<_>>(),
                "startup: seeded built-in free model channels"
            );
        }

        // 全新安装 / 无任何自配厂商：让免费渠道兜底成为默认模型，
        // 保证"装上就能对话"。有自配厂商时不动 config.model。
        let has_paid_provider = config.llm_providers.keys().any(|id| {
            !hermix_config::free_channel::is_free_provider(id)
                && hermix_config::provider_has_key(&config, id)
        });
        if config.model.is_none() && !has_paid_provider {
            if let Some((provider_id, model_id)) =
                hermix_config::free_channel::default_model(&config)
            {
                let full = format!("{provider_id}:{model_id}");
                config.model = Some(full.clone());
                if config.model_selector.is_none() {
                    config.model_selector = Some(hermix_config::ModelSelectorConfig {
                        enabled: vec![hermix_config::ModelSelectorEntry {
                            provider_id: provider_id.clone(),
                            model_id,
                        }],
                        default_model: Some(full),
                    });
                }
                config_dirty = true;
                tracing::info!(
                    model = %config.model.as_deref().unwrap_or_default(),
                    "startup: free channel is the fallback default model"
                );
            }
        }

        if config_dirty {
            let config_path = hermix_home.join("config.yaml");
            if let Err(e) = hermix_config::save_config_yaml(&config_path, &config) {
                tracing::warn!(
                    error = %e,
                    path = %config_path.display(),
                    "startup: failed to persist corrected config (continuing with in-memory fix)"
                );
            }
        }

        let tool_registry = Arc::new(ToolRegistry::new());
        // Effect Sandwich 持久层（P3）：EffectLog 适配成 EffectSink，
        // 工具执行前后落 intent/outcome。EffectLog 每次操作自开连接，
        // 构造本身不会失败；单笔持久化失败在 sink 内部吞掉（降级为
        // 不记录）——三明治是崩溃恢复依据，不能阻断工具执行。
        {
            let effects_path = hermix_config::paths::data_dir_in(&hermix_home).join("effects.db");
            let effect_log = hermix_task::EffectLog::new(effects_path);
            // 崩溃恢复扫描（§3.4）：上次进程可能死在工具执行中途。
            // 幂等 → 重放语义（由下次调用自然覆盖）；非幂等 → needs_review，
            // 只记日志（通知条目/用户裁决 UI 属后续批次），不自动重放。
            match effect_log.scan_unfinished() {
                Ok(actions) if !actions.is_empty() => {
                    tracing::warn!(
                        count = actions.len(),
                        "startup recovery: {} unfinished tool effects from previous run \
                         (idempotent=REPLAY eligible, non-idempotent=NEEDS_REVIEW)",
                        actions.len()
                    );
                    for a in &actions {
                        match a {
                            hermix_task::RecoveryAction::Replay(i) => tracing::warn!(
                                intent = %i.id, tool = %i.tool,
                                "recovery: idempotent effect eligible for replay"
                            ),
                            hermix_task::RecoveryAction::NeedsReview(i) => tracing::warn!(
                                intent = %i.id, tool = %i.tool,
                                "recovery: non-idempotent effect needs user review"
                            ),
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("startup recovery scan failed: {e}"),
            }
            tool_registry.set_effect_sink(Arc::new(EffectLogSink {
                log: effect_log,
                registry: Arc::downgrade(&tool_registry),
            }));
        }
        // §3.4 L2：TaskRecord 启动对账。正常终态由 sub-agent 生命周期的
        // 三个挂点落盘（task_record_sink），启动时残留 running = 上次进程
        // 死在执行中途。Checkpoint.from_step 未细化，无从续跑——唯一诚实
        // 的处置是落成 Terminal(Faulted)，让恢复视图不留僵尸。
        // store 构造后供 task_record_store 字段复用（单实例，不再重复开库）。
        let task_record_store = {
            let store = hermix_task::TaskRecordStore::new(
                hermix_config::paths::data_dir_in(&hermix_home).join("effects.db"),
            );
            reconcile_task_records(&store);
            store
        };
        // Register builtin tools (file, shell, browser, etc.)
        let terminal_backend: Arc<dyn hermix_core::TerminalBackend> =
            Arc::new(hermix_environments::LocalBackend::default());
        let skill_store = Arc::new(hermix_skills::FileSkillStore::new(
            hermix_skills::FileSkillStore::default_dir(),
        ));
        let skill_provider: Arc<dyn hermix_core::SkillProvider> =
            Arc::new(hermix_skills::SkillManager::new(skill_store));
        // Wire `clarify` to a blocking dispatcher so the agent truly waits
        // for the desktop UI to render a question card and reply, instead of
        // returning a JSON envelope that the model then re-parses as
        // ordinary tool output.
        // 全局单例（P5）：成员会话的 clarify 转发要进同一 dispatcher。
        let clarify_dispatcher = hermix_tools::ClarifyDispatcher::global().clone();
        let (clarify_broadcast, _) = tokio::sync::broadcast::channel::<ClarifyBroadcast>(64);
        // 进程级装配（C4）。`boot` 持有 service 注册 guard——必须活到
        // `HttpServerState` 析构；`boot_seams` 把同一组 provider 实例
        // 传给 `register_builtin_tools`，让 file/terminal 工具真接 Fs/Shell
        // seam 而不是退回老路径。
        let boot = hermix_context::Boot::local(hermix_context::BootProfile::new());
        let boot_seams = boot.seams();
        // Background pump: subscribes to dispatcher snapshots and
        // broadcasts each new pending `clarify` to every connected
        // WebSocket. The desktop UI filters by session id before showing
        // a question card.
        //
        // Guard against re-broadcasting already-forwarded requests:
        // `next_snapshot()` returns ALL pending clarifies whenever a new
        // one arrives, so without a seen-set the same question would be
        // sent to the UI multiple times (causing a cascade of duplicate
        // cards).
        let pump_dispatcher = clarify_dispatcher.clone();
        let pump_broadcast = clarify_broadcast.clone();
        tokio::spawn(async move {
            let mut seen: HashSet<String> = HashSet::new();
            loop {
                let snapshots = pump_dispatcher.next_snapshot().await;
                for snap in snapshots {
                    if !seen.insert(snap.request_id.clone()) {
                        continue;
                    }
                    let env = ClarifyBroadcast {
                        session_id: snap.session_id.clone(),
                        request_id: snap.request_id.clone(),
                        question: snap.question.clone(),
                        choices: snap.choices.clone(),
                    };
                    let _ = pump_broadcast.send(env);
                }
            }
        });
        // Todo event bus (see build() for full rationale).
        let (todo_event_tx, _) = tokio::sync::broadcast::channel::<
            hermix_tools::tools::todo::TodoChange,
        >(hermix_tools::tools::todo::TODO_EVENT_CHANNEL_CAPACITY);

        // (custom_apps wiring fix): construct a shared
        // PluginManager for the custom-app system and clone it into
        // both `state.custom_app_manager` (HTTP API at /api/custom-apps)
        // and the LLM tool `custom_app_manager` (registered by
        // `register_builtin_tools` below). Previously both consumers
        // were wired to `None`, leaving `/api/custom-apps` stuck at 503
        // and the LLM tool missing from the registry. Paths mirror
        // `commands_custom_app.rs`.
        let custom_app_manager = std::sync::Arc::new(hermix_plugin::PluginManager::new(
            hermix_config::paths::hermix_home().join("custom-apps"),
        ));
        // C 修复: 启动首扫填充内存基线 (list 路径改为只读内存, 不再每次请求扫盘)
        let _ = custom_app_manager.scan_plugins();

        hermix_tools::register_builtin_tools(
            &tool_registry,
            terminal_backend,
            skill_provider,
            Some(Arc::new(
                hermix_tools::ChannelClarifyBackend::new(clarify_dispatcher.clone()),
            )),
            None,
            Some(todo_event_tx.clone()),
            Some(custom_app_manager.clone()),
            Some(boot_seams.clone()),
        );
        // 总管方案 P1：成员工具（dispatch_member / get_member_result /
        // stop_member）注册进进程级注册表。执行侧（MemberRunner）由各会话
        // 构建时安装（local_agent_service），这里只管工具面。
        hermix_agent::member::runner::register_member_tools(&tool_registry);
        // with whatever the user last persisted in
        // `enabled_toolsets.json`. Without this, every restart
        // would silently reset every toggle the user made in
        // the Settings UI to "on" — the LLM would start seeing
        // the toolsets the user thought they had disabled.
        crate::dashboard::skills::apply_persisted_toolset_state(
            &tool_registry,
            &hermix_home,
        );
        // 持久化 enabled_toolsets.json 是「上次保存设置时的穷举快照」——
        // 版本升级带来的**新 toolset** 不在快照里，上面的 apply 会把它当
        // 「用户已禁用」关掉（dispatch_member 冒烟实测踩中）。新工具集
        // 的正确默认是放行；用户真想关，去设置里关，下次持久化会记录。
        // 每次随版本发新 toolset，在这里补一行。
        tool_registry.set_toolset_enabled("member", true);


        //★ P1-② : load native plugins (.dll) from
        // $HERMIX_HOME/plugins/tools/ and /plugins/memory/.
        // Each directory must contain a `plugin.yaml` with a
        // `binary` field. Plugins export `hermix_plugin_init()`
        // via the C ABI. Each ToolInstance is wrapped as
        // `NativeToolAdapter`, registered into `tool_registry`.
        {
            let dll_paths =
                hermix_agent::native_plugin::discover_native_plugins(&hermix_home);
            for dll in &dll_paths {
                // SAFETY: `dll` path is discovered by `discover_native_plugins`
                // which filters for .dll files. `load` validates exported symbols.
                match unsafe {
                    hermix_agent::native_plugin::LoadedNativePlugin::load(dll)
                } {
                    Ok(plugin) => {
                        let mut registered = 0usize;
                        for idx in 0..plugin.tool_count {
                            // SAFETY: `idx` is bounded by `plugin.tool_count`.
                            match unsafe { plugin.create_tool_handler(idx) } {
                                Ok(handler) => {
                                    let schema = handler.schema();
                                    let name = schema.name.clone();
                                    let risk = RiskLevel::Caution;
                                    tool_registry.register_with_risk(
                                        &name,
                                        "native-plugin",
                                        schema,
                                        handler,
                                        Arc::new(|| true),
                                        vec![],
                                        true,
                                        &format!(
                                            "Native plugin tool: {}",
                                            plugin.name
                                        ),
                                        "🔌",
                                        None,
                                        risk,
                                        false,
                                    );
                                    registered += 1;
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        target: "hermix.plugins",
                                        name = %plugin.name,
                                        index = idx,
                                        error = %e,
                                        "Native plugin tool creation failed"
                                    );
                                }
                            }
                        }
                        tracing::info!(
                            target: "hermix.plugins",
                            name = %plugin.name,
                            tools = plugin.tool_count,
                            registered,
                            "Native plugin loaded"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            target: "hermix.plugins",
                            dll = %dll.display(),
                            error = %e,
                            "Failed to load native plugin"
                        );
                    }
                }
            }
            if !dll_paths.is_empty() {
                tracing::info!(
                    target: "hermix.plugins",
                    count = dll_paths.len(),
                    "Scanned native plugins"
                );
            }
        }

        //MCP integration ( — wiring the previously-dead
        // McpManager). We construct the manager against the SAME
        // tool_registry that the agent service already sees, then
        // iterate the user-configured servers in
        // `$HERMIX_HOME/mcp_servers.json` and `connect()` each enabled
        // one. Disabling happens later in the request lifecycle: the
        // dashboard's `update_server` route calls
        // `McpManager::disconnect` when `enabled` flips to false.
        // Each `connect()` registers discovered tools (with real
        // schemas) into the tool registry, so the LLM sees them in the
        // next `get_definitions()` call. We deliberately ignore
        // per-server connect failures: one broken server must not
        // prevent the rest from coming up.
        let mcp_manager = hermix_mcp::ArcMcpManager::new(tool_registry.clone());
        {
            let mcp_list = crate::dashboard::mcp::load_mcp_servers(&hermix_home);
            let mcp_mgr = mcp_manager.clone();
            // Spawn MCP connection as a background task so it does NOT
            // block the HTTP listener from binding. `npx -y <pkg>` can
            // take 10-30s on first run (package download + Node startup),
            // which would cause the sidecar watchdog to think the server
            // is dead and restart it in a loop. The MCP tools appear in
            // the registry as soon as each server finishes connecting.
            tokio::spawn(async move {
                for entry in mcp_list.servers.iter().filter(|e| e.enabled) {
                    match crate::dashboard::mcp::entry_to_server_config(entry) {
                        Ok((cfg_name, cfg)) => {
                            if let Err(e) = mcp_mgr.connect(&cfg_name, cfg).await {
                                tracing::warn!(
                                    server = %cfg_name,
                                    error = %e,
                                    "MCP server connect failed at startup; skipping"
                                );
                            } else {
                                tracing::info!(server = %cfg_name, "MCP server connected");
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                server = %entry.name,
                                error = %e,
                                "MCP server entry invalid; skipping"
                            );
                        }
                    }
                }
            });
        }

        let session_persistence = Arc::new(
            hermix_agent::session_persistence::SessionPersistence::new(&hermix_home),
        );
        // Don't swallow ensure_db errors silently — a failed migration
        // means later queries will crash with "no such column: …" and
        // the user has no way to know what went wrong. Log loudly.
        if let Err(e) = session_persistence.ensure_db() {
            tracing::error!(error = %e, "session db ensure_db failed at startup");
        }

        // Sweep pending approvals from any previous session that never
        // got a user decision. Without this, reloading a session that
        // was active right before a server crash would re-render the
        // approval card with its buttons still clickable — but the
        // in-memory approval registry is gone, so clicking would be a
        // silent no-op (the LLM never sees the decision, agent loop is
        // already dead). Marking them `superseded` lets the front-end
        // render the "已超时 · 操作已自动取消" chip instead, preserving
        // visibility without dangling buttons. Errors are logged but
        // don't fail startup — a stuck sweep is much better than a
        // server that refuses to boot.
        match session_persistence.supersede_pending_approvals() {
            Ok(0) => {}
            Ok(n) => {
                tracing::info!(
                    superseded = n,
                    "startup: marked pending approvals as superseded (previous session crashed / killed before user decided)"
                );
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "startup: failed to supersede pending approvals; stale cards may render as clickable"
                );
            }
        }

        //a single consciousness tracker shared by the
        // agent service (writer) and the dashboard endpoints
        // (readers via REST + WebSocket broadcast).
        let consciousness_tracker = hermix_agent::ConsciousnessTracker::new();

        // §3.4 L2：TaskRecord 热路径生产者接线（提前算路径，此处
        // hermix_home 尚未被 move 进字段；648 行处的同名绑定仍供
        // task_record_store 字段使用，两处指向同一 effects.db）。
        let agent_effects_db_path =
            hermix_config::paths::data_dir_in(&hermix_home).join("effects.db");

        // Create LocalAgentService, wiring in the shared
        // consciousness tracker so the agent loop emits ECG
        // vitals for the desktop UI.
        let agent_service_inner = hermix_agent::LocalAgentService::new(
            Arc::new(config.clone()),
            tool_registry.clone(),
            session_persistence.clone(),
        )
        .with_task_record_sink(std::sync::Arc::new(
            crate::task_record_sink::TaskRecordSinkImpl::new(std::sync::Arc::new(
                hermix_task::TaskRecordStore::new(agent_effects_db_path),
            )),
        ))
        .with_consciousness_tracker(Arc::new(consciousness_tracker.clone()));
        // Kick off a background cold-start warmup. The user's
        // first real chat would otherwise eat the 30–60s the
        // provider needs to load its model and warm its HTTP
        // keep-alive pool. The warmup is fire-and-forget and
        // cannot fail in a way that blocks startup.
        agent_service_inner.warm_up_default_provider();
        let agent_service: Arc<dyn hermix_core::traits::AgentService> =
            Arc::new(agent_service_inner);

        // Initialize skill store
        let skill_store: Arc<dyn hermix_skills::SkillStore> = Arc::new(
            hermix_skills::FileSkillStore::new(hermix_skills::FileSkillStore::default_dir()),
        );

        // Seed builtin skills into the default skills dir so the front-end
        // "扩展 → 技能" tab has a populated list on first launch. Idempotent.
        crate::dashboard::skills::seed_builtin_skills_default();

        // Load the persistent user allowlist (if present) and seed
        // a default-empty in-memory session bypass map. Both are
        // wired into the pre-tool-dispatch checker below.
        let user_allowlist_path = hermix_config::paths::approval_allowlist_path();
        let user_allowlist = UserAllowlist::load(&user_allowlist_path);
        let user_allowlist = if user_allowlist.is_empty() {
            None
        } else {
            Some(user_allowlist)
        };

        let (llm_provider_name, llm_probe_url) = extract_llm_probe_url(&config).unzip();

        let metrics = crate::metrics::MetricsCollector::new(&hermix_home);

        // §5.5 第 3/4 层：设备注册表路径（hermix_home 在字面量里被 move，
        // 提前解析）。首个字面量未用到时会有 unused 警告——两处构造路径
        // 共用同一绑定消除重复。
        let devices_db_path = hermix_home.join("devices.db");
        // 审批超时：单一真相源在 hermix_config 的进程级全局，
        // registry / 前端倒计时 / agent 兜底三处都从这里读。
        let approval_timeout_secs = config.approval.timeout_secs;
        hermix_config::set_approval_timeout_secs(approval_timeout_secs);
        let state = Self {
            config: Arc::new(config),
            tool_registry,
            hermix_home,
            session_persistence: session_persistence.clone(),
            skill_store: Some(skill_store),
            boot: Arc::new(boot),
            runtime_gateway_running: None,
            agent_service,
            approval_registry: ApprovalRegistry::with_timeout(std::time::Duration::from_secs(
                approval_timeout_secs,
            )),
            clarify_dispatcher,
            clarify_broadcast,
            expert_registry: hermix_skills::ExpertRegistry::new(),
            automation_scheduler: None,
            resource_patrol: None,
            user_allowlist: Arc::new(std::sync::Mutex::new(user_allowlist)),
            session_bypass: Arc::new(std::sync::Mutex::new(HashMap::new())),
            consciousness_tracker,            tickloop_manager: hermix_runtime::TickLoopManager::new(
                hermix_runtime::TickLoopConfig {
                    llm_provider_url: llm_probe_url.flatten(),
                    llm_provider_name,
                    ..hermix_runtime::TickLoopConfig::default()
                },
            ),
            mcp_manager,
            // dedup map for retry-storm protection. See
            // the `recent_sends` field doc on the struct. Cleared
            // on server restart, which is fine because the WS
            // clients that produced the storm are also gone.
            recent_sends: Arc::new(std::sync::Mutex::new(HashMap::new())),
            active_sessions: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            active_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_grace: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            completed_keys: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            seq: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            attachment_seq: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            // we know the runtime config. Initialised to None here so
            // the early struct literal doesn't need to await anything.
            todo_event_tx: Some(todo_event_tx.clone()),
            orchestration_bus: OrchestrationEventBus::new(),
            metrics,
            task_service: {
                let repo = std::sync::Arc::new(hermix_task::TaskRepository::new(
                    session_persistence.db_path().to_path_buf(),
                ));
                // 不要吞：迁移失败 = 此后每条 task_* 查询都在运行期报
                // "no such column"，用户只会看到任务工具神秘失效。
                if let Err(e) = repo.ensure_schema() {
                    tracing::error!(error = %e, "tasks 表 ensure_schema 失败（任务工具将不可用）");
                }
                Some(std::sync::Arc::new(hermix_task::TaskService::new(repo)))
            },
            // §3.4 L2：TaskRecord 持久状态机与 EffectLog 同库（effects.db）。
            // 单实例复用启动对账时构造的 store（见上方 TaskRecord 对账块）；
            // 单笔持久化失败在端点侧降级 500。
            task_record_store: Some(std::sync::Arc::new(task_record_store)),
            channel_sessions: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            automation_service: {
                let repo = std::sync::Arc::new(hermix_automation::AutomationRepository::new(
                    session_persistence.db_path().to_path_buf(),
                ));
                let _ = repo.ensure_schema();
                Some(std::sync::Arc::new(hermix_automation::AutomationService::new(repo)))
            },
            automation_engine_scheduler: None,
            custom_app_manager: Some(custom_app_manager),
            // §5.5 第 3/4 层（build_with_hermes_home 路径）：注册表打不开 =
            // 无配对能力（远程一律 Unpaired），降级不阻断主服务启动。
            device_registry: {
                match crate::pairing::DeviceRegistry::open(&devices_db_path) {
                    Ok(r) => Some(Arc::new(r)),
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            path = %devices_db_path.display(),
                            "devices.db 打开失败：设备配对功能不可用"
                        );
                        None
                    }
                }
            },
            pairing_sessions: Arc::new(crate::pairing::PairingSessions::new()),
            nonce_cache: crate::request_signature::NonceCache::new(),
            relay_settings: Arc::new(tokio::sync::Mutex::new(crate::relay_settings::load())),
            relay_status: crate::relay_client::RelayStatus::new(),
            relay_last_error: Arc::new(tokio::sync::Mutex::new(None)),
            relay_connector_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        state.register_task_tracker_tools();

        // 内置免费渠道：启动后延迟刷新一次模型清单（best-effort，
        // 失败沿用上次成功快照，不阻塞首屏）。
        crate::free_channel::spawn_startup_refresh(&state);

        // : Start the automation scheduler with the unified
        // dispatch path. The trigger callback calls `agent_service.send_message()`,
        // whose tool calls go through `ToolRegistry::dispatch_with_context()` —
        // the same path as HTTP chat requests (铁律 DD).
        if let (Some(auto_svc), Some(task_svc)) = (
            &state.automation_service,
            &state.task_service,
        ) {
            // 收割上一次进程留下的在途 run。必须发生在 scheduler 启动
            // 之前：那几行既占着单飞槽（挡住本该触发的那次），又让
            // `count_active_runs` 恒为非零——保活信号会把机器永久按在
            // 唤醒状态，而 UI 上看不出任何原因。
            if let Err(e) = auto_svc.reap_abandoned_runs() {
                tracing::warn!(
                    error = %e,
                    "automation: failed to reap abandoned runs at startup"
                );
            }
            // 锚定投递的存在性探针。必须真接上：`send_message` 会 UPSERT
            // sessions 行，把结果投进一个已被删除的对话不会"失败"，只会把
            // 那个 id 复活成一条无标题孤儿会话，出现在侧栏里且无从解释。
            let session_probe: Arc<dyn automation_trigger::SessionProbe> =
                Arc::new(SessionExistenceProbe {
                    persistence: session_persistence.clone(),
                });
            let handle = automation_trigger::start_automation_scheduler(
                auto_svc.clone(),
                state.agent_service(),
                state.orchestration_bus.clone(),
                Some(task_svc.clone()),
                Some(session_probe),
            );
            // SAFETY: we're mutating a field of the returned state before
            // handing it to the caller. This is safe because no other code
            // has a reference to `state` yet.
            let mut state = state;
            state.automation_engine_scheduler = Some(handle);
            return Ok(state);
        }

        Ok(state)
    }

    pub async fn build_with_agent_service(
        config: GatewayConfig,
        agent_service: Arc<dyn hermix_core::traits::AgentService>,
    ) -> Result<Self, AgentError> {
        let tool_registry = Arc::new(ToolRegistry::new());
        // Effect Sandwich 持久层（P3）：EffectLog 适配成 EffectSink，
        // 工具执行前后落 intent/outcome。EffectLog 每次操作自开连接，
        // 构造本身不会失败；单笔持久化失败在 sink 内部吞掉（降级为
        // 不记录）——三明治是崩溃恢复依据，不能阻断工具执行。
        {
            let effects_path =
                hermix_config::paths::hermix_home().join("data").join("effects.db");
            let effect_log = hermix_task::EffectLog::new(effects_path);
            tool_registry.set_effect_sink(Arc::new(EffectLogSink {
                log: effect_log,
                registry: Arc::downgrade(&tool_registry),
            }));
        }
        // §3.4 L2：TaskRecord 启动对账。正常终态由 sub-agent 生命周期的
        // 三个挂点落盘（task_record_sink），启动时残留 running = 上次进程
        // 死在执行中途。Checkpoint.from_step 未细化，无从续跑——唯一诚实
        // 的处置是落成 Terminal(Faulted)，让恢复视图不留僵尸。
        // store 构造后供 task_record_store 字段复用（单实例，不再重复开库）。
        let task_record_store = {
            let store = hermix_task::TaskRecordStore::new(
                hermix_config::paths::hermix_home().join("data").join("effects.db"),
            );
            reconcile_task_records(&store);
            store
        };
        // Register builtin tools (file, shell, browser, etc.)
        let terminal_backend: Arc<dyn hermix_core::TerminalBackend> =
            Arc::new(hermix_environments::LocalBackend::default());
        let skill_store = Arc::new(hermix_skills::FileSkillStore::new(
            hermix_skills::FileSkillStore::default_dir(),
        ));
        let skill_provider: Arc<dyn hermix_core::SkillProvider> =
            Arc::new(hermix_skills::SkillManager::new(skill_store));
        // 全局单例（P5）：成员会话的 clarify 转发要进同一 dispatcher。
        let clarify_dispatcher = hermix_tools::ClarifyDispatcher::global().clone();
        let (clarify_broadcast, _) = tokio::sync::broadcast::channel::<ClarifyBroadcast>(64);
        let pump_dispatcher = clarify_dispatcher.clone();
        let pump_broadcast = clarify_broadcast.clone();
        tokio::spawn(async move {
            let mut seen: HashSet<String> = HashSet::new();
            loop {
                let snapshots = pump_dispatcher.next_snapshot().await;
                for snap in snapshots {
                    if !seen.insert(snap.request_id.clone()) {
                        continue;
                    }
                    let env = ClarifyBroadcast {
                        session_id: snap.session_id.clone(),
                        request_id: snap.request_id.clone(),
                        question: snap.question.clone(),
                        choices: snap.choices.clone(),
                    };
                    let _ = pump_broadcast.send(env);
                }
            }
        });
        // Todo event bus (see build() for full rationale).
        let (todo_event_tx, _) = tokio::sync::broadcast::channel::<
            hermix_tools::tools::todo::TodoChange,
        >(hermix_tools::tools::todo::TODO_EVENT_CHANNEL_CAPACITY);

        // 进程级装配（C4）。同 build_with_hermes_home——保证两个 build
        // 路径产出的 HttpServerState 都能把接缝实例注入到 file/terminal
        // 工具。详见同函数上方的注释。
        let boot = hermix_context::Boot::local(hermix_context::BootProfile::new());
        let boot_seams = boot.seams();

        // (custom_apps wiring fix): construct a shared
        // PluginManager for the custom-app system and clone it into
        // both `state.custom_app_manager` (HTTP API at /api/custom-apps)
        // and the LLM tool `custom_app_manager` (registered by
        // `register_builtin_tools` below). Previously both consumers
        // were wired to `None`, leaving `/api/custom-apps` stuck at 503
        // and the LLM tool missing from the registry. Paths mirror
        // `commands_custom_app.rs`.
        let custom_app_manager = std::sync::Arc::new(hermix_plugin::PluginManager::new(
            hermix_config::paths::hermix_home().join("custom-apps"),
        ));
        // C 修复: 启动首扫填充内存基线 (list 路径改为只读内存, 不再每次请求扫盘)
        let _ = custom_app_manager.scan_plugins();

        hermix_tools::register_builtin_tools(
            &tool_registry,
            terminal_backend,
            skill_provider,
            Some(Arc::new(
                hermix_tools::ChannelClarifyBackend::new(clarify_dispatcher.clone()),
            )),
            None,
            Some(todo_event_tx.clone()),
            Some(custom_app_manager.clone()),
            Some(boot_seams.clone()),
        );
        // 总管方案 P1：成员工具（dispatch_member / get_member_result /
        // stop_member）注册进进程级注册表。执行侧（MemberRunner）由各会话
        // 构建时安装（local_agent_service），这里只管工具面。
        hermix_agent::member::runner::register_member_tools(&tool_registry);

        // MCP wiring (mirrors `build_with_hermes_home`). Construct
        // the manager and eagerly connect every enabled server from
        // `$HERMIX_HOME/mcp_servers.json`.
        let mcp_manager = hermix_mcp::ArcMcpManager::new(tool_registry.clone());
        let hermix_home = hermix_config::hermix_home();
        {
            let mcp_list = crate::dashboard::mcp::load_mcp_servers(&hermix_home);
            for entry in mcp_list.servers.iter().filter(|e| e.enabled) {
                match crate::dashboard::mcp::entry_to_server_config(entry) {
                    Ok((cfg_name, cfg)) => {
                        if let Err(e) = mcp_manager.connect(&cfg_name, cfg).await {
                            tracing::warn!(
                                server = %cfg_name,
                                error = %e,
                                "MCP server connect failed at startup; skipping"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            server = %entry.name,
                            error = %e,
                            "MCP server entry invalid; skipping"
                        );
                    }
                }
            }
        }
        let session_persistence = Arc::new(
            hermix_agent::session_persistence::SessionPersistence::new(&hermix_home),
        );
        // Don't swallow ensure_db errors silently — a failed migration
        // means later queries will crash with "no such column: …" and
        // the user has no way to know what went wrong. Log loudly.
        if let Err(e) = session_persistence.ensure_db() {
            tracing::error!(error = %e, "session db ensure_db failed at startup");
        }

        // Sweep pending approvals from any previous session that never
        // got a user decision. Without this, reloading a session that
        // was active right before a server crash would re-render the
        // approval card with its buttons still clickable — but the
        // in-memory approval registry is gone, so clicking would be a
        // silent no-op (the LLM never sees the decision, agent loop is
        // already dead). Marking them `superseded` lets the front-end
        // render the "已超时 · 操作已自动取消" chip instead, preserving
        // visibility without dangling buttons. Errors are logged but
        // don't fail startup — a stuck sweep is much better than a
        // server that refuses to boot.
        match session_persistence.supersede_pending_approvals() {
            Ok(0) => {}
            Ok(n) => {
                tracing::info!(
                    superseded = n,
                    "startup: marked pending approvals as superseded (previous session crashed / killed before user decided)"
                );
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "startup: failed to supersede pending approvals; stale cards may render as clickable"
                );
            }
        }

        // Initialize skill store
        let skill_store: Arc<dyn hermix_skills::SkillStore> = Arc::new(
            hermix_skills::FileSkillStore::new(hermix_skills::FileSkillStore::default_dir()),
        );

        // Seed builtin skills into the default skills dir so the front-end
        // "扩展 → 技能" tab has a populated list on first launch. Idempotent.
        crate::dashboard::skills::seed_builtin_skills_default();

        // Load the persistent user allowlist (if present). Same
        // shape as `build_with_hermes_home`; see that fn for the
        // design rationale on the `Option<...>` wrapper.
        let user_allowlist_path = hermix_config::paths::approval_allowlist_path();
        let user_allowlist = UserAllowlist::load(&user_allowlist_path);
        let user_allowlist = if user_allowlist.is_empty() {
            None
        } else {
            Some(user_allowlist)
        };

        let (llm_provider_name, llm_probe_url) = extract_llm_probe_url(&config).unzip();

        let metrics = crate::metrics::MetricsCollector::new(&hermix_home);

        // §5.5 第 3/4 层：设备注册表路径（hermix_home 在字面量里被 move，
        // 提前解析）。首个字面量未用到时会有 unused 警告——两处构造路径
        // 共用同一绑定消除重复。
        let devices_db_path = hermix_home.join("devices.db");
        let approval_timeout_secs = config.approval.timeout_secs;
        hermix_config::set_approval_timeout_secs(approval_timeout_secs);
        let state = Self {
            config: Arc::new(config),
            tool_registry,
            hermix_home,
            session_persistence: session_persistence.clone(),
            skill_store: Some(skill_store),
            boot: Arc::new(boot),
            runtime_gateway_running: None,
            agent_service,
            approval_registry: ApprovalRegistry::with_timeout(std::time::Duration::from_secs(
                approval_timeout_secs,
            )),
            clarify_dispatcher,
            clarify_broadcast,
            expert_registry: hermix_skills::ExpertRegistry::new(),
            automation_scheduler: None,
            resource_patrol: None,
            user_allowlist: Arc::new(std::sync::Mutex::new(user_allowlist)),
            session_bypass: Arc::new(std::sync::Mutex::new(HashMap::new())),
            consciousness_tracker: hermix_agent::ConsciousnessTracker::new(),
            tickloop_manager: hermix_runtime::TickLoopManager::new(
                hermix_runtime::TickLoopConfig {
                    llm_provider_url: llm_probe_url.flatten(),
                    llm_provider_name,
                    ..hermix_runtime::TickLoopConfig::default()
                },
            ),
            mcp_manager,
            // dedup map for retry-storm protection. See
            // the `recent_sends` field doc on the struct. Cleared
            // on server restart, which is fine because the WS
            // clients that produced the storm are also gone.
            recent_sends: Arc::new(std::sync::Mutex::new(HashMap::new())),
            active_sessions: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            active_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_grace: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            completed_keys: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            seq: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            attachment_seq: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            todo_event_tx: Some(todo_event_tx.clone()), // injected after register_builtin_tools below
            orchestration_bus: OrchestrationEventBus::new(),
            metrics,
            task_service: {
                let repo = std::sync::Arc::new(hermix_task::TaskRepository::new(
                    session_persistence.db_path().to_path_buf(),
                ));
                // 不要吞：迁移失败 = 此后每条 task_* 查询都在运行期报
                // "no such column"，用户只会看到任务工具神秘失效。
                if let Err(e) = repo.ensure_schema() {
                    tracing::error!(error = %e, "tasks 表 ensure_schema 失败（任务工具将不可用）");
                }
                Some(std::sync::Arc::new(hermix_task::TaskService::new(repo)))
            },
            // §3.4 L2：TaskRecord 持久状态机与 EffectLog 同库（effects.db）。
            // 单实例复用启动对账时构造的 store（见上方 TaskRecord 对账块）；
            // 单笔持久化失败在端点侧降级 500。
            task_record_store: Some(std::sync::Arc::new(task_record_store)),
            channel_sessions: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            automation_service: {
                let repo = std::sync::Arc::new(hermix_automation::AutomationRepository::new(
                    session_persistence.db_path().to_path_buf(),
                ));
                let _ = repo.ensure_schema();
                Some(std::sync::Arc::new(hermix_automation::AutomationService::new(repo)))
            },
            automation_engine_scheduler: None,
            custom_app_manager: Some(custom_app_manager),
            // §5.5 第 3/4 层：设备注册表打不开 = 无配对能力（远程一律
            // Unpaired），降级不阻断主服务启动。
            device_registry: {
                match crate::pairing::DeviceRegistry::open(&devices_db_path) {
                    Ok(r) => Some(Arc::new(r)),
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            path = %devices_db_path.display(),
                            "devices.db 打开失败：设备配对功能不可用"
                        );
                        None
                    }
                }
            },
            pairing_sessions: Arc::new(crate::pairing::PairingSessions::new()),
            nonce_cache: crate::request_signature::NonceCache::new(),
            relay_settings: Arc::new(tokio::sync::Mutex::new(crate::relay_settings::load())),
            relay_status: crate::relay_client::RelayStatus::new(),
            relay_last_error: Arc::new(tokio::sync::Mutex::new(None)),
            relay_connector_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }; // close Self { ... }
        state.register_task_tracker_tools();
        Ok(state)
    }

    pub fn with_runtime_gateway_running(
        mut self,
        runtime_gateway_running: Arc<AtomicBool>,
    ) -> Self {
        self.runtime_gateway_running = Some(runtime_gateway_running);
        self
    }

    /// Start the automation scheduler background loop and stash the
    /// handle in the state so callers can later shut it down. Idempotent:
    /// re-calling this replaces the prior handle.
    pub fn with_automation_scheduler(mut self) -> Self {
        let scheduler =
            crate::dashboard::automation::AutomationScheduler::start(self.clone());
        self.automation_scheduler = Some(scheduler);
        // cronjob 工具换上真实后端：Signal 占位实现只返回没人消费的请求
        // JSON。面板调度器已随本调用启动，对话里创建的定时任务由此真正
        // 落到自动化任务表并进入 30s 调度。
        hermix_tools::tools::cronjob::register_with_backend(
            &self.tool_registry,
            Arc::new(
                crate::dashboard::automation::DashboardCronjobBackend::new(self.clone()),
            ),
        );
        self
    }
    /// 启动资源水位巡检（总管方案 P0.5）：C 盘水位 + scratch 配额，
    /// 越限/恢复的边沿上 `OrchestrationEventBus`（→ WS → 桌面端）。
    /// 停派门由成员派单侧在发工单前查询（`dispatch_blocked`）。
    pub fn with_resource_patrol(mut self) -> Self {
        let patrol = Arc::new(crate::resource_patrol::ResourcePatrol::default_c_drive(
            self.orchestration_bus.clone(),
        ));
        // 停派门接线（D5）：成员工单派发前查询巡检状态；巡检未跑过
        // 第一拍前放行（首次 tick 在 run 循环里 60s 内发生）。
        let gate_patrol = patrol.clone();
        hermix_agent::member::runner::install_dispatch_gate(Arc::new(move || {
            gate_patrol.dispatch_blocked()
        }));
        let runner = patrol.clone();
        tokio::spawn(async move {
            runner.run().await;
        });
        self.resource_patrol = Some(patrol);
        self
    }

    /// 启动 relay 出站连接循环。
    ///
    /// ## 为什么必须显式调用
    ///
    /// `relay_client::connect_and_serve` 在本文件之前**没有任何生产调用方**——
    /// 整个 relay 子系统（`relay_client` / `relay_transport` / `relay_identity` /
    /// `relay_proto`）写完了但一根线没接，桌面端从来没有连过 relay。这个方法
    /// 就是那根线；它做成 builder 而不是构造函数里自动跑，是为了让测试与 CLI
    /// 模式不必真的去连公网。
    ///
    /// 重复调用受 `relay_connector_running` 闸保护，至多一个循环在跑。
    pub fn with_relay_client(self) -> Self {
        self.spawn_relay_connector();
        self
    }

    /// 拉起出站连接任务：没凭据先自动签发（enroll-node），然后进入重连循环。
    ///
    /// 启动时（`with_relay_client`）与 `relay_settings::reset_identity`
    /// （重置后凭据为空、原任务早已退出）共用，所以闸在这里而不在调用方。
    pub(crate) fn spawn_relay_connector(&self) {
        // 克隆一份进后台任务：本方法要立即返回，而任务需要 `'static` 的
        // 所有权。`HttpServerState` 内部几乎全是 `Arc`，这一次全量克隆的代价
        // 只在拉起时付一次。
        let worker = Arc::new(self.clone());
        if worker
            .relay_connector_running
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            tracing::info!("relay 出站连接任务已在运行，不重复拉起");
            return;
        }
        tokio::spawn(async move {
            let (cfg, token) = {
                let s = worker.relay_settings.lock().await;
                (s.relay_config(), s.token().to_string())
            };
            if !cfg.enabled() {
                tracing::info!("未配置 relay 地址，不启动出站连接");
                worker
                    .relay_connector_running
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                return;
            }
            if token.trim().is_empty() {
                // 首次启动还没有凭据：向 relay 自动签发（enroll-node），
                // 成功即落盘并继续连接——用户不需要登服务器手工签发。
                // 失败不 panic、不重试刷接口：保持人工兜底（界面里可重试、
                // 可重置本机身份，或登服务器 relayctl add-node），下次拉起再试。
                tracing::info!(relay = %cfg.url, "还没有 relay 凭据，尝试自动签发（enroll-node）");
                if let Err(e) = crate::relay_settings::ensure_node_credential(
                    &cfg.url,
                    &worker.relay_settings,
                )
                .await
                {
                    tracing::warn!(relay = %cfg.url, error = %e, "relay 凭据自动签发失败");
                    *worker.relay_last_error.lock().await = Some(format!(
                        "{e}。可在「手机端 → 中转连接」重试或重置本机身份，也可在 relay 服务器上用 relayctl add-node 手动签发。"
                    ));
                    worker
                        .relay_connector_running
                        .store(false, std::sync::atomic::Ordering::SeqCst);
                    return;
                }
            }
            crate::relay_client::connect_and_serve(
                cfg,
                worker.relay_status.clone(),
                // 网络变化时打断退避：切 Wi-Fi / 拔网线后不必干等满一轮。
                Some(crate::net_watch::subscribe()),
                |cfg| {
                    let worker = worker.clone();
                    async move {
                        // 每次尝试都从设置里取当前凭据：401 自愈重签后，
                        // 下一轮拿到的是新 token，不依赖外层捕获的旧值。
                        let token = {
                            let s = worker.relay_settings.lock().await;
                            s.token().to_string()
                        };
                        match crate::relay_client::serve_relay_socket(
                            cfg.clone(),
                            token,
                            worker.clone(),
                            // 握手成功后由 serve_relay_socket 置为 Connected，
                            // 界面才能从「正在连接（第 N 次）」翻到「已连上中转」。
                            worker.relay_status.clone(),
                        )
                        .await
                        {
                            Err(e) => {
                                let msg = e.to_string();
                                // 401 = relay 不认这枚凭据（被撤销 / relay 重置过）。
                                // enroll-node 对已撤销节点放行（先到先得），
                                // 重签一枚落盘，退避后即用新凭据重连。
                                if crate::relay_settings::is_relay_auth_reject(&msg) {
                                    tracing::warn!(
                                        relay = %cfg.url,
                                        "relay 拒绝本机凭据，尝试重新签发（enroll-node）"
                                    );
                                    if let Err(enroll_err) = crate::relay_settings::
                                        ensure_node_credential(&cfg.url, &worker.relay_settings)
                                        .await
                                    {
                                        tracing::error!(error = %enroll_err, "relay 凭据重签失败");
                                        *worker.relay_last_error.lock().await = Some(format!(
                                            "{msg}；自动重签也失败：{enroll_err}"
                                        ));
                                        return;
                                    }
                                    tracing::info!(relay = %cfg.url, "relay 凭据已重签，退避后重连");
                                    *worker.relay_last_error.lock().await =
                                        Some("relay 凭据已重签，正在重连".into());
                                } else {
                                    tracing::warn!(error = %msg, "relay 连接失败");
                                    *worker.relay_last_error.lock().await = Some(msg);
                                }
                            }
                            Ok(()) => *worker.relay_last_error.lock().await = None,
                        }
                    }
                },
            )
            .await;
            // connect_and_serve 返回 = 配置被置为禁用。闸放开，允许之后再拉起。
            worker
                .relay_connector_running
                .store(false, std::sync::atomic::Ordering::SeqCst);
        });
    }
}

#[cfg(test)]
mod task_record_reconciliation_tests {
    use super::reconcile_task_records;
    use hermix_task::{TaskRecord, TaskRecordStore, TaskState};

    fn store() -> (tempfile::TempDir, TaskRecordStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskRecordStore::new(dir.path().join("effects.db"));
        (dir, store)
    }

    #[test]
    fn running_record_is_marked_faulted_on_startup() {
        let (_dir, store) = store();
        store
            .save(&TaskRecord::new_running("subagent-1", "conv-1"))
            .unwrap();

        reconcile_task_records(&store);

        let rec = store.get("subagent-1").unwrap().unwrap();
        match rec.state {
            TaskState::Terminal { outcome } => match outcome {
                hermix_task::TaskOutcome::Faulted { error } => {
                    assert!(error.contains("进程重启"))
                }
                other => panic!("expected Faulted, got {other:?}"),
            },
            other => panic!("expected Terminal, got {other:?}"),
        }
        // 对账后扫描恒为空：恢复视图不留僵尸。
        assert!(store.scan_unfinished().unwrap().is_empty());
    }

    #[test]
    fn terminal_records_are_untouched() {
        let (_dir, store) = store();
        store
            .save(&TaskRecord::terminal(
                "subagent-2",
                "conv-1",
                hermix_task::TaskOutcome::Completed { result: None },
            ))
            .unwrap();

        reconcile_task_records(&store);

        let rec = store.get("subagent-2").unwrap().unwrap();
        match rec.state {
            TaskState::Terminal { outcome } => {
                assert!(matches!(
                    outcome,
                    hermix_task::TaskOutcome::Completed { .. }
                ))
            }
            other => panic!("expected untouched Terminal, got {other:?}"),
        }
    }

    #[test]
    fn empty_store_is_noop() {
        let (_dir, store) = store();
        reconcile_task_records(&store); // 不 panic，不写任何记录
        assert!(store.scan_unfinished().unwrap().is_empty());
    }
}

