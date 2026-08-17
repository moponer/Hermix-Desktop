//! AgentLoop construction, builders, ACUI emit, tool routing, steer, session notes.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use super::*;

impl AgentLoop {
    /// Populate [`AgentConfig::stored_system_prompt`] from SQLite (`sessions.system_prompt`) for session continuation.
    /// Call before [`AgentLoop::run`] when resuming a gateway/CLI session so Anthropic prefix cache matches Python.
    pub fn hydrate_stored_system_prompt_from_hermes_home(
        config: &mut AgentConfig,
        hermix_home: &std::path::Path,
    ) -> Result<(), AgentError> {
        crate::session_lifecycle::hydrate_stored_system_prompt_from_hermes_home(config, hermix_home)
    }

    pub(crate) fn graceful_interrupt_result(
        &self,
        ctx: &ContextManager,
        total_turns: u32,
        tool_errors: &[hermix_core::ToolErrorRecord],
        accumulated_usage: Option<UsageStats>,
        session_cost_usd: f64,
        session_started_hooks_fired: bool,
        persist_user_idx: Option<usize>,
    ) -> AgentResult {
        // Default (non-error) interrupt path: a user-initiated stop or
        // WS disconnect. Surfaces `finish_reason = "interrupted"` so the
        // frontend shows a neutral "已中断" chip (never a red error
        // banner — the user expected this).
        self.graceful_interrupt_result_with(
            ctx,
            total_turns,
            tool_errors,
            accumulated_usage,
            session_cost_usd,
            session_started_hooks_fired,
            persist_user_idx,
            false,
            None,
        )
    }

    /// Like [`Self::graceful_interrupt_result`] but flags the interrupt
    /// as a *backend* failure (LLM/transport error, mid-stream chunk
    /// failure) so the server emits a structured `Error` event and the
    /// frontend shows a red "出错了" banner + error prompt.
    pub(crate) fn graceful_interrupt_result_errored(
        &self,
        ctx: &ContextManager,
        total_turns: u32,
        tool_errors: &[hermix_core::ToolErrorRecord],
        accumulated_usage: Option<UsageStats>,
        session_cost_usd: f64,
        session_started_hooks_fired: bool,
        persist_user_idx: Option<usize>,
        last_error: Option<String>,
    ) -> AgentResult {
        self.graceful_interrupt_result_with(
            ctx,
            total_turns,
            tool_errors,
            accumulated_usage,
            session_cost_usd,
            session_started_hooks_fired,
            persist_user_idx,
            true,
            last_error,
        )
    }

    /// Like [`Self::graceful_interrupt_result`] but lets the caller flag
    /// that the interrupt was caused by a *backend* failure (LLM/transport
    /// error, mid-stream chunk failure) rather than a user stop.
    ///
    /// Backend failures set `finish_reason = "error"` so the server can
    /// emit a structured `StreamEvent::Error` and the frontend shows a
    /// red "出错了" banner + error prompt. Without this distinction the
    /// server sent `Done` for backend failures too, so the frontend
    /// silently displayed the partial/last content as a completed turn
    /// with no error indication.
    pub(crate) fn graceful_interrupt_result_with(
        &self,
        ctx: &ContextManager,
        total_turns: u32,
        tool_errors: &[hermix_core::ToolErrorRecord],
        accumulated_usage: Option<UsageStats>,
        session_cost_usd: f64,
        session_started_hooks_fired: bool,
        persist_user_idx: Option<usize>,
        errored: bool,
        last_error: Option<String>,
    ) -> AgentResult {
        // `errored` distinguishes a *backend* failure from a
        // *user-initiated* interrupt (stop button, WS disconnect).
        // Both are "graceful" (we persist partial content), but only a
        // backend failure should be surfaced to the frontend as an
        // error — a user interrupt is expected and must NOT show a red
        // "出错了" banner.
        let finish_reason = if errored {
            Some("error".to_string())
        } else {
            Some("interrupted".to_string())
        };
        let mut result = self.finalize_agent_result(
            ctx,
            total_turns,
            tool_errors,
            accumulated_usage,
            session_cost_usd,
            session_started_hooks_fired,
            persist_user_idx,
            false,
            true,
            finish_reason,
        );
        // Post-set the captured backend error so it can be surfaced to
        // the frontend as the error card's "技术详情" subtext. Done here
        // (rather than threading a new param through `finalize_agent_result`)
        // to keep that signature stable for the ~20 non-stream callers in
        // `run_loop.rs`.
        result.last_error = last_error;
        result
    }

    /// Notify upstream listeners that the loop entered a new phase.
    /// Calls the `on_loop_phase_changed` callback if set. The callback
    /// receives `(phase_name, stage_name, turn)` as `&str, &str, u32`.
    /// The upstream layer (e.g. `LocalAgentService`) bridges this into
    /// a `LoopPhaseChanged` WS event via the orchestration bus.
    pub(crate) fn notify_phase(&self, phase: &LoopPhase, turn: u32) {
        let stage = phase.as_orchestration_stage();
        let phase_str = phase.as_str();
        let stage_str = stage.as_str();
        tracing::debug!(
            phase = phase_str,
            stage = stage_str,
            turn = turn,
            "loop phase boundary"
        );
        if let Some(cb) = &self.callbacks.on_loop_phase_changed {
            cb(phase_str, stage_str, turn);
        }
    }

    /// Notify upstream listeners that the agent loop is finalizing.
    /// Calls the `on_task_complete` callback if set. The callback
    /// receives `(success, task_id)` where `success` is true for
    /// natural completion and false for errors/interrupts.
    /// The upstream layer uses this to transition the Task status
    /// and trigger result delivery.
    pub(crate) fn notify_task_complete(&self, success: bool, task_id: Option<&str>) {
        if let Some(cb) = &self.callbacks.on_task_complete {
            cb(success, task_id);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn finalize_agent_result(
        &self,
        ctx: &ContextManager,
        total_turns: u32,
        tool_errors: &[hermix_core::ToolErrorRecord],
        accumulated_usage: Option<UsageStats>,
        session_cost_usd: f64,
        session_started_hooks_fired: bool,
        persist_user_idx: Option<usize>,
        finished_naturally: bool,
        interrupted: bool,
        finish_reason: Option<String>,
    ) -> AgentResult {
        //wire: drain this run's tool errors into the cross-run
        // counter so the next prompt can surface the demotion block.
        self.drain_tool_errors_into_counter(tool_errors);
        //emit Finalize phase boundary
        self.notify_phase(&LoopPhase::Finalize, total_turns);
        //notify upstream that the task is complete.
        // success = finished naturally without interruption.
        let success = finished_naturally && !interrupted;
        self.notify_task_complete(success, None);
        let result = crate::session_lifecycle::finalize_agent_result(
            &self.config,
            &self.focus_stack,
            self.memory_router.as_ref(),
            self.plugin_manager.as_ref(),
            &self.latest_compressed_summary,
            &self.latest_compression_archived_turns,
            &self.latest_compression_metrics,
            ctx,
            total_turns,
            tool_errors,
            accumulated_usage,
            session_cost_usd,
            session_started_hooks_fired,
            persist_user_idx,
            finished_naturally,
            interrupted,
            finish_reason,
            Some(&self.callbacks),
        );
        result
    }

    /// .3a : the post-LLM logic that was duplicated
    /// ~200 lines in `run()` and `run_stream()`. Fires the PostLlmCall
    /// hook, accumulates usage, runs the cost guard (with auto-degrade
    /// to the configured cheap model), and decides between
    /// `Finalize` (no tool calls → caller returns) and `Continue`
    /// (tool calls → caller falls through to `execute_tool_calls`).
    /// `stream_mode` is `true` for `run_stream()` — the helper uses
    /// this to emit a stream-specific `tracing` line and to forward
    /// the `finish_reason` through the finalize path.
    /// 7 mutable scalar args collapsed to
    /// `&mut CostGuard` + `&mut Option<String>` (4 args shrunk to 1
    /// struct). `record_usage` / `evaluate_thresholds` / `install_downgrade`
    /// on [`CostGuard`] absorb the 4-field atomic-mutation, so the
    /// helper signature dropped from 11 args to 7. The two remaining
    /// mutable scalars — `last_content_with_tools` — stay on the
    /// helper signature because they're stored on `LoopState` (used by
    /// the codex-ack continuation logic on the next turn) and not on
    /// `CostGuard`.
    /// (follow-up): `&mut Option<String>` for
    /// `last_content_with_tools` collapsed to `&mut MessageCache`,
    /// dropping the helper to 6 mutable args (the `Option<String>` is
    /// now reached via `cache.store_content_with_tools(...)` inside
    /// the helper). The companion helper `prepare_assistant_turn`
    /// also takes `&mut MessageCache` for the same reason.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn process_post_response(
        &self,
        ctx: &mut ContextManager,
        response: &hermix_core::LlmResponse,
        turn_usage_acc: Option<&UsageStats>,
        total_turns: u32,
        api_elapsed: u64,
        cost_guard: &mut CostGuard,
        route_state: &mut RouteState,
        message_cache: &mut MessageCache,
        stream_mode: bool,
    ) -> Result<PostResponseAction, AgentError> {
        crate::session_lifecycle::process_post_response(
            &self.config,
            self.plugin_manager.as_ref(),
            ctx,
            response,
            turn_usage_acc,
            total_turns,
            api_elapsed,
            cost_guard,
            route_state,
            message_cache,
            stream_mode,
            || self.resolve_cost_degrade_model(),
            |model| self.turn_route_cost_guard(model),
        )
    }

    /// shared per-turn post-LLM setup that
    /// `run()` and `run_stream()` previously open-coded after the
    /// `process_post_response` call. Builds the local `assistant_msg`,
    /// appends it to the context, updates `last_content_with_tools` if
    /// the assistant content came bundled with tool calls (used by the
    /// codex-ack continuation logic on the next turn), and reports
    /// whether any prior tool result exists in the history.
    /// Returns the three locals the caller needs:
    /// - `history_includes_tool`: did any earlier turn leave a `Tool`
    /// message in the context? Used by
    /// `decide_empty_response` to know whether tool
    /// execution is part of the protocol context.
    /// - `assistant_msg`: the LLM's response, freshly appended to
    /// the context.
    /// - `tool_calls`: the LLM's claimed tool calls, cloned for the
    /// caller to dispatch / dedup.
    #[allow(clippy::type_complexity)]
    pub(crate) fn prepare_assistant_turn(
        &self,
        ctx: &mut ContextManager,
        response: &hermix_core::LlmResponse,
        message_cache: &mut MessageCache,
    ) -> (bool, Message, Option<Vec<ToolCall>>) {
        crate::session_lifecycle::prepare_assistant_turn(ctx, response, message_cache)
    }

    /// centralised `mute_post_response`
    /// control chunk emit. The same AtomicBool-swap-then-conditional-
    /// emit pattern was open-coded in two places inside `run_stream()`:
    /// the housekeeping block after tool dispatch (line ~5980) and the
    /// terminal finalize path (line ~5936). This helper takes the
    /// stream callback, the mute flag, and the desired next state, and
    /// only fires the control chunk when the state actually changes
    /// (the frontend only needs to know about transitions, not every
    /// dispatch). Both call sites become a single 1-line call.
    pub(crate) fn emit_mute_transition(
        on_chunk: &Box<dyn Fn(StreamChunk) + Send + Sync>,
        stream_mute: &std::sync::atomic::AtomicBool,
        next_muted: bool,
    ) {
        crate::session_lifecycle::emit_mute_transition(on_chunk, stream_mute, next_muted)
    }

    /// apply the three-variant
    /// `EmptyResponseAction` outcome produced by
    /// `crate::empty_response_handler::decide_empty_response`.
    /// Returns `true` when a `ContinueWithUserMsg` or
    /// `ContinueWithSystemNudge` variant injected a message into the
    /// context — the caller should `continue;` to restart the loop
    /// with the injected message. Returns `false` when the variant
    /// was `Finalize` (a no-op) — the caller should
    /// fall through to its terminal finalize path.
    pub(crate) fn apply_fallback_outcome(
        &self,
        ctx: &mut ContextManager,
        action: crate::empty_response_handler::EmptyResponseAction,
        stream_mode: bool,
        total_turns: u32,
        autonomous_nudge_count: u32,
    ) -> bool {
        crate::session_lifecycle::apply_fallback_outcome(
            ctx,
            action,
            stream_mode,
            total_turns,
            autonomous_nudge_count,
        )
    }

    /// most recent archive refs produced by
    /// `auto_compress_if_over_threshold`. Empty when the context has
    /// never crossed the 80% pressure threshold. Frontend uses this
    /// to render the "see archived context" affordance.
    pub fn last_archive_refs(&self) -> Vec<ArchiveRef> {
        self.last_archive_refs
            .lock()
            .ok()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Create a new agent loop.
    pub fn new(
        config: AgentConfig,
        tool_registry: Arc<ToolRegistry>,
        llm_provider: Arc<dyn LlmProvider>,
    ) -> Self {
        Self {
            config,
            tool_registry,
            llm_provider,
            interrupt: InterruptController::new(),
            plugin_manager: None,
            callbacks: Arc::new(AgentCallbacks::default()),
            delegate_depth: 0,
            primary_credential_pool: None,
            evolution_counters: Arc::new(Mutex::new(EvolutionCounters::default())),
            tool_error_counter: Arc::new(Mutex::new(
                crate::tool_error_demotion::ToolErrorCounter::default(),
            )),
            oauth_refresh: OAuthRefresh::default(),
            sub_agent_orchestrator: None,
            pending_steer: Arc::new(Mutex::new(None)),
            tool_router: None,
            memory_router: None,
always_include: vec![
"read_file".to_string(),
"write_file".to_string(),
"move_file".to_string(),
"memory".to_string(),
],
            prefetcher: None,
            learned_prefetch_stats: Arc::new(Mutex::new(
                hermix_intelligence::prefetch::IntentStats::default(),
            )),
            focus_stack: Arc::new(Mutex::new(FocusStack::new())),
            consciousness_tracker: None,
            compression: None,
            latest_compressed_summary: std::sync::Mutex::new(None),
            latest_compression_archived_turns: std::sync::Mutex::new(None),
            latest_compression_metrics: std::sync::Mutex::new(None),
            last_archive_refs: std::sync::Mutex::new(Vec::new()),
            memory_recall_seeded: std::sync::atomic::AtomicBool::new(false),
            prompt_cache: std::sync::Mutex::new(crate::agent_helpers::PromptCache::default()),
            context_epoch: std::sync::atomic::AtomicU64::new(0),
            stored_prompt_epoch: std::sync::atomic::AtomicU64::new(0),
            embedding_provider: None,
            compression_in_flight: std::sync::Mutex::new(None),
            background_tasks: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Create a new agent loop with a shared interrupt controller.
    pub fn with_interrupt(
        config: AgentConfig,
        tool_registry: Arc<ToolRegistry>,
        llm_provider: Arc<dyn LlmProvider>,
        interrupt: InterruptController,
    ) -> Self {
        Self {
            config,
            tool_registry,
            llm_provider,
            interrupt,
            plugin_manager: None,
            callbacks: Arc::new(AgentCallbacks::default()),
            delegate_depth: 0,
            primary_credential_pool: None,
            evolution_counters: Arc::new(Mutex::new(EvolutionCounters::default())),
            tool_error_counter: Arc::new(Mutex::new(
                crate::tool_error_demotion::ToolErrorCounter::default(),
            )),
            oauth_refresh: OAuthRefresh::default(),
            sub_agent_orchestrator: None,
            pending_steer: Arc::new(Mutex::new(None)),
            tool_router: None,
            memory_router: None,
always_include: vec![
"read_file".to_string(),
"write_file".to_string(),
"move_file".to_string(),
"memory".to_string(),
],
            prefetcher: None,
            learned_prefetch_stats: Arc::new(Mutex::new(
                hermix_intelligence::prefetch::IntentStats::default(),
            )),
            focus_stack: Arc::new(Mutex::new(FocusStack::new())),
            consciousness_tracker: None,
            compression: None,
            latest_compressed_summary: std::sync::Mutex::new(None),
            latest_compression_archived_turns: std::sync::Mutex::new(None),
            latest_compression_metrics: std::sync::Mutex::new(None),
            last_archive_refs: std::sync::Mutex::new(Vec::new()),
            memory_recall_seeded: std::sync::atomic::AtomicBool::new(false),
            prompt_cache: std::sync::Mutex::new(crate::agent_helpers::PromptCache::default()),
            context_epoch: std::sync::atomic::AtomicU64::new(0),
            stored_prompt_epoch: std::sync::atomic::AtomicU64::new(0),
            embedding_provider: None,
            compression_in_flight: std::sync::Mutex::new(None),
            background_tasks: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Attach a memory router for time-aware recall.
    pub fn with_memory_router(mut self, router: Arc<hermix_focus::MemoryRouter>) -> Self {
        self.memory_router = Some(router);
        self
    }

    /// Attach an embedding provider for vector recall in memory prefetch.
    pub fn with_embedding_provider(
        mut self,
        provider: Arc<dyn hermix_intelligence::embedding::EmbeddingProvider>,
    ) -> Self {
        self.embedding_provider = Some(provider);
        self
    }

    /// Bump the context epoch to invalidate the prompt cache's
    /// context layer. Call this when memory/skills/context-files
    /// are modified so the next LLM call gets a fresh system prompt.
    pub fn bump_context_epoch(&self) {
        self.context_epoch.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Get the current context epoch value.
    pub fn context_epoch(&self) -> u64 {
        self.context_epoch.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Attach an in-process sub-agent orchestrator. When set, `delegate_task`
    /// tool calls are actually executed by the orchestrator instead of just
    /// returning a signal envelope. See
    /// [`crate::sub_agent_orchestrator::SubAgentOrchestrator`].
    pub fn with_sub_agent_orchestrator(
        mut self,
        orchestrator: Arc<crate::sub_agent_orchestrator::SubAgentOrchestrator>,
    ) -> Self {
        self.sub_agent_orchestrator = Some(orchestrator);
        self
    }

    /// replace the per-loop `FocusStack<SessionFrame>`. Default
    /// is an empty stack (constructed in `new` / `with_interrupt`).
    /// Tests use this to inject a pre-populated stack; production
    /// callers should leave the default.
    pub fn with_focus_stack(mut self, stack: Arc<Mutex<FocusStack<SessionFrame>>>) -> Self {
        self.focus_stack = stack;
        self
    }

    /// attach the consciousness tracker so the focus stack
    /// depth is reflected in the "ECG" stream the desktop renders.
    /// Called from `LocalAgentService::spawn_agent_loop`; tests
    /// typically leave the default (`None`).
    pub fn with_consciousness_tracker(
        mut self,
        tracker: Arc<crate::consciousness::ConsciousnessTracker>,
    ) -> Self {
        self.consciousness_tracker = Some(tracker);
        self
    }

    /// Attach the primary runtime credential pool (API key rotation).
    pub fn with_primary_credential_pool(mut self, pool: Arc<CredentialPool>) -> Self {
        self.primary_credential_pool = Some(pool);
        self
    }

    /// Set the plugin manager.
    pub fn with_plugins(mut self, pm: Arc<std::sync::Mutex<PluginManager>>) -> Self {
        self.plugin_manager = Some(pm);
        self
    }

    /// Set the callbacks.
    pub fn with_callbacks(mut self, cb: AgentCallbacks) -> Self {
        self.callbacks = Arc::new(cb);
        self
    }

    /// Set the delegate depth.
    pub fn with_delegate_depth(mut self, depth: u32) -> Self {
        self.delegate_depth = depth;
        self
    }

    /// Attach a tool schema router. When set, the LLM only sees the
    /// router's output instead of the full tool registry .
    /// Wrap your router with [`AlwaysInclude`] if you want a fixed
    /// must-have set to bypass the router.
    pub fn with_tool_router(mut self, router: Arc<dyn ToolSchemaRouter>) -> Self {
        self.tool_router = Some(router);
        self
    }

    /// Override the always-include tool whitelist.
    pub fn with_always_include(mut self, names: Vec<String>) -> Self {
        self.always_include = names;
        self
    }

    /// Attach an intent-aware prefetcher .
    /// When set, the agent loop runs the prefetcher before each LLM call,
    /// dispatches Safe tools in parallel, and injects results as a
    /// system-prompt block.
    pub fn with_prefetcher(
        mut self,
        prefetcher: Prefetcher,
        learned_stats: hermix_intelligence::prefetch::IntentStats,
    ) -> Self {
        self.prefetcher = Some(Arc::new(prefetcher));
        self.learned_prefetch_stats = Arc::new(Mutex::new(learned_stats));
        self
    }

    /// Attach an advanced LLM-powered context compressor. When set,
    /// `auto_compress_if_over_threshold` uses this instead of the
    /// simple character-based compressor, producing structured
    /// summaries that preserve goals, progress, decisions, and files.
    pub fn with_compressor(
        mut self,
        compressor: crate::compression::ContextCompressor,
    ) -> Self {
        self.compression = Some(Arc::new(tokio::sync::Mutex::new(compressor)));
        self
    }

    //ACUI emit ------------------------------------------------------

    /// Emit an ACUI directive to upstream listeners (if any).
    /// No-op if no `on_acui_directive` callback was wired (e.g. unit
    /// tests, batched runs). The directive is JSON-serialized and passed
    /// to the callback as a string; the upstream layer is responsible
    /// for deserializing and forwarding it as a `StreamEvent::Acui`.
    /// This method never blocks and never returns an error — the ACUI
    /// pipeline is best-effort. A failure in the callback is logged
    /// and swallowed (the agent keeps running).
    pub fn emit_acui(&self, directive: AcuiDirective) {
        crate::acui_dispatch::emit_acui(&self.callbacks, directive)
    }

    /// Convenience: emit a `Show` directive.
    pub fn emit_acui_show(
        &self,
        id: impl Into<String>,
        kind: impl Into<String>,
        props: serde_json::Value,
    ) {
        crate::acui_dispatch::emit_acui_show(&self.callbacks, id, kind, props)
    }

    /// Convenience: emit an `Update` directive.
    pub fn emit_acui_update(&self, id: impl Into<String>, patch: serde_json::Value) {
        crate::acui_dispatch::emit_acui_update(&self.callbacks, id, patch)
    }

    /// Convenience: emit a `Hide` directive.
    pub fn emit_acui_hide(&self, id: impl Into<String>) {
        crate::acui_dispatch::emit_acui_hide(&self.callbacks, id)
    }

    /// Build a default router (KeywordRouter + AlwaysInclude).
    /// Threads `self.always_include` through so user-configured
    /// always-include names actually reach the `AlwaysInclude` wrapper.
    /// Pre-this method took no `self` and ignored the
    /// field — `always_include` was a write-only field.
    pub fn default_tool_router(&self) -> Arc<dyn ToolSchemaRouter> {
        crate::acui_dispatch::default_tool_router(&self.always_include)
    }

    /// Resolve the tool schemas to send to the LLM this turn.
    /// Behavior:
    /// - If `tool_router` is `None`: return `candidates` unchanged.
    /// - If `tool_router` is `Some`: build a [`TurnContext`] from the
    /// last user message + recent history, call `route`, and return
    /// the routed schemas. The `always_include` whitelist is enforced
    /// by the router itself if the caller wrapped it with
    /// [`AlwaysInclude`]; this method does not re-wrap.
    pub fn route_tools(
        &self,
        candidates: Vec<ToolSchema>,
        messages: &[Message],
    ) -> Vec<ToolSchema> {
        let Some(router) = self.tool_router.as_ref() else {
            return candidates;
        };
        let user_message = messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, MessageRole::User))
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        let recent: Vec<Message> = messages
            .iter()
            .rev()
            .take(6)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let ctx = TurnContext {
            user_message,
            recent_messages: recent,
            active_toolsets: self.config.active_toolsets.clone(),
            max_tools: candidates.len().min(self.config.max_routed_tools),
            priority_tools: Vec::new(),
        };
        let routed: Vec<ToolSchema> = router
            .route(&ctx, candidates)
            .into_iter()
            .map(|r| r.schema)
            .collect();
        // Progressive disclosure: if deferred tools exceed threshold,
        // replace them with tool_search/tool_describe/tool_call bridges.
        let assembly = hermix_tools::assemble_tool_schemas(routed);
        if assembly.activated {
            tracing::info!(
                deferred = assembly.deferred_count,
                "tool_search: {} deferred tools behind bridge",
                assembly.deferred_count,
            );
        }
        assembly.tool_schemas
    }

    /// Queue mid-run user guidance (Python `AIAgent.steer`).
    /// Queue mid-run user guidance (Python `AIAgent.steer`).
    /// Returns `false` when the text is empty/whitespace-only.
    pub fn steer(&self, text: &str) -> bool {
        crate::steer::steer(&self.pending_steer, text)
    }

    /// Clear any queued `/steer` guidance without applying it.
    pub fn clear_pending_steer(&self) {
        crate::steer::clear_pending_steer(&self.pending_steer)
    }

    //── : FocusStack plumbing ────────────────────────────────────
    // The agent loop owns a per-session `FocusStack<SessionFrame>`.
    // Tool-dispatch paths can call `note_delegation_start` to push a
    // frame for each `delegate_task` invocation and
    // `note_delegation_end` to pop it on completion. Popped
    // `Cleared` frames generate a `FocusConclusion` that is
    // buffered in `pending_conclusions` until
    // `drain_focus_conclusions_to_memory_router` ships them to the
    // long-term memory backend.

    /// Push a `SessionRoot` frame for the current session. Idempotent
    /// — a second call is a no-op if a root frame is already on the
    /// stack. Callers should invoke this once at `run()` start.
    pub fn note_session_started(&self, task: impl Into<String>) {
        crate::focus_stack_ops::note_session_started(&self.focus_stack, task);
    }

    /// Push a `Delegation` frame for a `delegate_task` invocation.
    /// `sub_agent_id` is the UUID the orchestrator / sub-agent
    /// returns; pass an empty string if not yet known (it will be
    /// matched by depth in `note_delegation_end`).
    /// Returns the frame id, or `None` if the lock is poisoned.
    pub fn note_delegation_start(
        &self,
        task: impl Into<String>,
        sub_agent_id: impl Into<String>,
    ) -> Option<hermix_focus::FocusFrameId> {
        crate::focus_stack_ops::note_delegation_start(
            &self.focus_stack,
            self.delegate_depth,
            task,
            sub_agent_id,
        )
    }

    /// Pop the topmost `Delegation` frame whose payload matches
    /// `sub_agent_id` (or the topmost delegation frame if
    /// `sub_agent_id` is empty). The popped frame is `mark_clear`-ed
    /// first so the next `pop` generates a `FocusConclusion`.
    /// Returns the depth the popped frame was at, or `None` if no
    /// matching frame was on the stack.
    pub fn note_delegation_end(
        &self,
        sub_agent_id: &str,
        status: SessionFrameStatus,
    ) -> Option<u32> {
        crate::focus_stack_ops::note_delegation_end(&self.focus_stack, sub_agent_id, status)
    }

    /// Current depth of the focus stack (number of frames on it).
    /// Returned as `u8` because the consciousness tracker's
    /// `set_focus_depth` takes `u8` and the stack never grows past
    /// `MAX_DELEGATE_DEPTH` (typically 5) in practice.
    pub fn current_focus_depth(&self) -> u8 {
        crate::focus_stack_ops::current_focus_depth(&self.focus_stack)
    }
}
