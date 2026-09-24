//! Shared HTTP server state: HttpServerState and its methods.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;


use hermix_config::GatewayConfig;
use hermix_core::AgentError;
use hermix_tools::ToolRegistry;

use super::*;

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
    /// 一次性配对会话管理器（进程内，TTL 120s）。
    pub pairing_sessions: Arc<crate::pairing::PairingSessions>,
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
            let config_path = hermix_home.join("config.yaml");
            if let Err(e) = hermix_config::save_config_yaml(&config_path, &config) {
                tracing::warn!(
                    error = %e,
                    path = %config_path.display(),
                    "startup: failed to persist auto-corrected config (continuing with in-memory fix)"
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
        let clarify_dispatcher = hermix_tools::ClarifyDispatcher::new();
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
        // with whatever the user last persisted in
        // `enabled_toolsets.json`. Without this, every restart
        // would silently reset every toggle the user made in
        // the Settings UI to "on" — the LLM would start seeing
        // the toolsets the user thought they had disabled.
        crate::dashboard::skills::apply_persisted_toolset_state(
            &tool_registry,
            &hermix_home,
        );


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

        // Create LocalAgentService, wiring in the shared
        // consciousness tracker so the agent loop emits ECG
        // vitals for the desktop UI.
        let agent_service_inner = hermix_agent::LocalAgentService::new(
            Arc::new(config.clone()),
            tool_registry.clone(),
            session_persistence.clone(),
        )
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
        let state = Self {
            config: Arc::new(config),
            tool_registry,
            hermix_home,
            session_persistence: session_persistence.clone(),
            skill_store: Some(skill_store),
            boot: Arc::new(boot),
            runtime_gateway_running: None,
            agent_service,
            approval_registry: ApprovalRegistry::new(),
            clarify_dispatcher,
            clarify_broadcast,
            expert_registry: hermix_skills::ExpertRegistry::new(),
            automation_scheduler: None,
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
            // Todo event bus — created lazily below in `build()` after
            // we know the runtime config. Initialised to None here so
            // the early struct literal doesn't need to await anything.
            todo_event_tx: Some(todo_event_tx.clone()),
            orchestration_bus: OrchestrationEventBus::new(),
            metrics,
            task_service: {
                let repo = std::sync::Arc::new(hermix_task::TaskRepository::new(
                    session_persistence.db_path().to_path_buf(),
                ));
                let _ = repo.ensure_schema();
                Some(std::sync::Arc::new(hermix_task::TaskService::new(repo)))
            },
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
        };
        state.register_task_tracker_tools();

        // : Start the automation scheduler with the unified
        // dispatch path. The trigger callback calls `agent_service.send_message()`,
        // which internally uses `ToolRegistry::dispatch_async()` — the same
        // path as HTTP chat requests (铁律 DD).
        if let (Some(auto_svc), Some(task_svc)) = (
            &state.automation_service,
            &state.task_service,
        ) {
            let handle = automation_trigger::start_automation_scheduler(
                auto_svc.clone(),
                state.agent_service(),
                state.orchestration_bus.clone(),
                Some(task_svc.clone()),
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
        // Register builtin tools (file, shell, browser, etc.)
        let terminal_backend: Arc<dyn hermix_core::TerminalBackend> =
            Arc::new(hermix_environments::LocalBackend::default());
        let skill_store = Arc::new(hermix_skills::FileSkillStore::new(
            hermix_skills::FileSkillStore::default_dir(),
        ));
        let skill_provider: Arc<dyn hermix_core::SkillProvider> =
            Arc::new(hermix_skills::SkillManager::new(skill_store));
        let clarify_dispatcher = hermix_tools::ClarifyDispatcher::new();
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
        let state = Self {
            config: Arc::new(config),
            tool_registry,
            hermix_home,
            session_persistence: session_persistence.clone(),
            skill_store: Some(skill_store),
            boot: Arc::new(boot),
            runtime_gateway_running: None,
            agent_service,
            approval_registry: ApprovalRegistry::new(),
            clarify_dispatcher,
            clarify_broadcast,
            expert_registry: hermix_skills::ExpertRegistry::new(),
            automation_scheduler: None,
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
            todo_event_tx: Some(todo_event_tx.clone()), // injected after register_builtin_tools below
            orchestration_bus: OrchestrationEventBus::new(),
            metrics,
            task_service: {
                let repo = std::sync::Arc::new(hermix_task::TaskRepository::new(
                    session_persistence.db_path().to_path_buf(),
                ));
                let _ = repo.ensure_schema();
                Some(std::sync::Arc::new(hermix_task::TaskService::new(repo)))
            },
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
        self
    }
}

