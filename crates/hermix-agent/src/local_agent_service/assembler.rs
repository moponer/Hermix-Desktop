//! Agent 装配器：把配置 + 注册表 + 回调拼成一台可跑的 `AgentLoop`。
//!
//! 从 `local_agent_service` 搬出的第六批（P1-13）。这是组合根里最长的一段
//! （原本 300 行），也是最"什么都不属于"的一段：它只读 `self` 上的几个
//! `Option<Arc<..>>` 字段，然后按固定顺序把 builder 串起来。
//!
//! 搬出来之后，`mod.rs` 剩下的每一段都能用一句话说清职责（会话读写、
//! 工具派发、AgentService trait 实现）；而这里的每一步都是装配顺序，
//! 放在一起才看得出哪些是可选的、哪些是必须的。

use std::path::PathBuf;
use std::sync::Arc;

use hermix_core::{LlmProvider, StreamChunk};

use crate::agent_loop::{AgentConfig, AgentLoop};

use super::prefetch_adapter::SafeRegistryWrapper;
use super::LocalAgentService;

impl LocalAgentService {
    pub(super) fn spawn_agent_loop(
        &self,
        session_id: &str,
        agent_config: AgentConfig,
        agent_tool_registry: Arc<crate::agent_loop::ToolRegistry>,
        provider: Arc<dyn LlmProvider>,
        on_chunk: Arc<dyn Fn(StreamChunk) + Send + Sync>,
        member_events_live: bool,
    ) -> AgentLoop {
        let hermix_home = agent_config.hermix_home.clone();
        let compressor_model = agent_config.model.clone();
        let mut agent = AgentLoop::new(agent_config, agent_tool_registry, provider);

        //Wire H1: tool schema router (KeywordRouter + AlwaysInclude).
        // `with_tool_router` consumes `agent`, so the call order is:
        // build the router first (borrows `&agent`), then move `agent`
        // into `with_tool_router`. Pre-the router was a
        // static call that ignored `agent.always_include`.
        let router = agent.default_tool_router();
        agent = agent.with_tool_router(router);

        //Wire : time-aware recall via MemoryRouter.
        // `all_plugins_as_router` synchronously constructs and
        // `initialize()`s every bundled memory plugin, including
        // OpenViking which spins up a `reqwest::blocking::Client`.
        // Calling that on a tokio worker thread would panic with
        // "Cannot drop a runtime in a context where blocking is not
        // allowed" when the client's internal blocking pool tries
        // to tear down.  `block_in_place` converts the current
        // worker into a dedicated blocking thread, so the inner
        // `reqwest::blocking::Client::build()` is free to spin up
        // its own runtime.  `block_in_place` returns the closure's
        // `Output` directly (it panics on current-thread, but the
        // production Tauri/HTTP server is multi-thread).
        //previously this whole block was gated on
        // `if let Some(ref hh) = hermix_home`, which meant any
        // caller that didn't set `AgentConfig::hermix_home` (e.g.
        // a fresh dev server with config loaded from cwd) got a
        // `memory_router: None` agent with zero warning. Fall
        // back to the central `hermix_config::paths::hermix_home()`
        // helper so the router is always built, and log the
        // outcome so a future regression is visible in logs.
        let hh_owned: String = hermix_home.clone().unwrap_or_else(|| {
            hermix_config::paths::hermix_home().to_string_lossy().into_owned()
        });
        // `block_in_place` is safe here: `spawn_agent_loop` only runs
        // inside `tokio::spawn`, which pins to a multi-thread worker.
        // `CurrentThread` panics cannot happen. (same
        // rationale as `build_shared_memory_router`'s runtime_flavor
        // detection — but that one is a public fn, this is a private
        // call site under a known guard.)
        let router = tokio::task::block_in_place(|| {
            crate::memory_layer_adapter::all_plugins_as_router(session_id, &hh_owned)
        });
        //safety net: bind every backend to the session
        // even if the adapter forgot to. Idempotent.
        router.initialize_all(session_id, &hh_owned);

        // Ensure today's daily-memory skeleton exists so the
        // operating-principles workflow has a row to append to even
        // before the agent's first write.
        if let Ok(store) = hermix_tools::tools::daily_memory::DailyMemoryStore::open() {
            if let Err(e) = store.ensure_today() {
                tracing::warn!(target: "hermix.agent", "daily_memory ensure_today failed: {e}");
            }
        }

        let backend_count = router.backend_count();
        tracing::info!(
            target: "hermix.agent",
            session_id,
            home = %hh_owned,
            backends = backend_count,
            "spawn_agent_loop: MemoryRouter built"
        );
        agent = agent.with_memory_router(Arc::new(router));

        // The MemoryRouter (built by `all_plugins_as_router`)
        // already contains the embedded SQLite backend. No separate
        // PluginManager memory_providers registration needed.

        //H2: intent-aware prefetcher. Wrap the full
        // ToolRegistry so the executor can dispatch Safe tools
        // in parallel before the LLM call.
        use hermix_intelligence::prefetch::{LearnedPrefetcher, Prefetcher, SafeToolExecutor};

        let learned_prefetcher = LearnedPrefetcher::new();
        let executor = Arc::new(SafeToolExecutor::new(Arc::new(SafeRegistryWrapper(self.tool_registry.clone()))));
        let prefetcher = Prefetcher::with_learned(
            executor,
            learned_prefetcher,
        );
        agent = agent.with_prefetcher(
            prefetcher,
            hermix_intelligence::prefetch::IntentStats::default(),
        );
        //forward focus-stack depth transitions to the
        // consciousness tracker so the desktop ECG reflects nested
        // delegation depth in real time.
        if let Some(ref tracker) = self.consciousness_tracker {
            agent = agent.with_consciousness_tracker(tracker.clone());
        }

        // Advanced context compressor: builds an AuxiliaryClient from
        // config.yaml + env vars and wires the LLM-powered compressor
        // into the agent loop. Falls back gracefully if no API keys are
        // available — the simple character-based compressor in context.rs
        // handles that case.
        // pass the GatewayConfig so the auxiliary client reads
        // API keys from config.yaml's [llm_providers] section — same source
        // as the main conversation provider. Without this, a user who
        // configured their key only in config.yaml (not as an env var)
        // would see 401 errors from the compression health check.
        let gw_snapshot = self.config.lock().unwrap_or_else(|e| e.into_inner()).clone();
        match crate::auxiliary_builder::build_auxiliary_client_with_config(
            hermix_intelligence::auxiliary::AuxiliaryConfig::default(),
            &gw_snapshot,
            Some(&compressor_model),
        ) {
            (aux_client, summary) if !summary.registered.is_empty() => {
                let aux = Arc::new(aux_client);
                // Day 4: pull breaker threshold / cooldown from
                // `GatewayConfig.compaction` (loaded from config.yaml
                // `[compaction]` section or `HERMIX_COMPACT_BREAKER_*`
                // env overrides). Falls back to the in-code defaults
                // (`DEFAULT_BREAKER_THRESHOLD = 3`,
                // `DEFAULT_BREAKER_COOLDOWN_SECS = 600`) when the user
                // hasn't customised the field — backward compatible.
                //also pull
                // `min_summary_tokens` / `summary_tokens_ceiling` from
                // `compaction:` so the desktop Settings → Advanced tab
                // can tune the LLM-compressor's token budget live. Each
                // field is `Option<u64>` on the config side; `None`
                // means "use the agent's hardcoded default" — no
                // regression for existing config.yaml files.
                let (breaker_threshold, breaker_cooldown_secs, summary_model,
                     min_summary_tokens, summary_tokens_ceiling,
                     pressure_threshold_percent) = {
                    let cfg = self
                        .config
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    (
                        cfg.compaction.circuit_breaker_threshold.unwrap_or(
                            crate::compression::DEFAULT_BREAKER_THRESHOLD,
                        ),
                        cfg.compaction.circuit_breaker_cooldown_secs.unwrap_or(
                            crate::compression::DEFAULT_BREAKER_COOLDOWN_SECS,
                        ),
                        // 压缩摘要默认跟随用户当前主模型 (config.yaml `model:`),
                        // 而不是固定走 auxiliary 链的默认模型 (如 MiniMax-M3)。
                        // 用户配了 gpt / custom-* / 第三方网关时, 压缩用同一个
                        // provider, 行为一致; 显式配了 `compaction.summary_model`
                        // 则尊重显式配置。注意: override 是纯 model id
                        // (如 "agnes-2.5-flash"), 不是 "provider:model" ——
                        // AuxiliaryClient::call 会把它直接当 model id 发给候选。
                        cfg.compaction.summary_model.clone().or_else(|| {
                            Some(
                                crate::agent_builder::parse_model_string(&compressor_model)
                                    .1
                                    .to_string(),
                            )
                        }),
                        cfg.compaction.min_summary_tokens,
                        cfg.compaction.summary_tokens_ceiling,
                        cfg.compaction.pressure_threshold_percent,
                    )
                };
                // Resolve token-budget overrides against CompressorConfig
                // defaults so a `None` value still picks up the existing
                // 2_000 / 12_000 baseline (no surprise regressions).
                let mut comp_cfg = crate::compression::CompressorConfig::default();
                // Set context_length to the actual model's context window.
                // Pass `llm_providers` so per-provider overrides (e.g.
                // `custom-agnes2-0.context_window`) take effect for
                // custom-provider models that aren't in the static catalog;
                // the bare `get_model_context_length` would silently fall
                // back to DEFAULT_FALLBACK_CONTEXT (128K) otherwise.
                {
                    let cfg = self
                        .config
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    comp_cfg.context_length =
                        hermix_intelligence::model_metadata::get_model_context_length_with(
                            &compressor_model,
                            Some(&cfg.llm_providers),
                        ) as u64;
                }
                if let Some(v) = min_summary_tokens {
                    comp_cfg.min_summary_tokens = v;
                }
                if let Some(v) = summary_tokens_ceiling {
                    comp_cfg.summary_tokens_ceiling = v;
                }
                comp_cfg.summary_model_override = summary_model;
                // Forward user-configured pressure threshold to the compressor
                // so it uses the same ratio as ContextManager.
                if let Some(v) = pressure_threshold_percent {
                    comp_cfg.threshold_percent = v;
                }
                let compressor = crate::compression::ContextCompressor::new(
                    comp_cfg,
                    aux.clone(),
                )
                .with_breaker_config(breaker_threshold, breaker_cooldown_secs)
                // C2: the summary gets the session's task tree appended,
                // read through the same `task_list` tool the model
                // itself uses.
                .with_task_source(self.tool_registry.clone(), session_id.to_string());
                // validate that the LLM-powered summariser
                // actually has a model to call. Without this, every
                // compression attempt would silently fail at runtime
                // (the aux call returns an error, `compress()` records
                // it as a summary_error, and `auto_compress_if_over_threshold`
                // only logs the failure without aborting the loop).
                if let Err(msg) = compressor.config().validate(compressor.auxiliary()) {
                    tracing::warn!(
                        "Disabling advanced context compressor: {msg}. \
                         Falling back to character-based compressor."
                    );
                } else {
                    // extract the summary model BEFORE moving the
                    // compressor into the agent loop (with_compressor
                    // consumes it). When no `compaction.summary_model`
                    // override is set, pass `None` so the health check
                    // pings the auxiliary client's own default model
                    // (e.g. `MiniMax-M3`) instead of the literal "default",
                    // which would be sent to the provider as a (invalid) model id.
                    let check_model = compressor
                        .config()
                        .summary_model_override
                        .clone();
                    // Reserved for the upcoming summary health check; suppress
                    // the unused-variable warning until that wiring lands.
                    let _ = check_model;
                    agent = agent.with_compressor(compressor);
                    tracing::info!(
                        "Advanced context compressor wired (aux providers: {:?}, breaker threshold={}, cooldown={}s)",
                        summary.registered,
                        breaker_threshold,
                        breaker_cooldown_secs,
                    );
                }
            }
            _ => {
                tracing::info!(
                    "No auxiliary API keys found — using simple context compressor"
                );
            }
        }

        // Wire the sub-agent orchestrator so `delegate_task` actually runs.
        // Previously the orchestrator field was never attached in production,
        // so sub-agents silently did nothing. The orchestrator also carries
        // the WS chunk emitter so sub-agent lifecycle events stream to the
        // frontend in real time (the "子代理" panel).
        let sub_agent_hermix_home: PathBuf = hh_owned.clone().into();
        let mut orch = crate::sub_agent_orchestrator::SubAgentOrchestrator::from_parent(
            &agent,
            sub_agent_hermix_home.clone(),
            Some(on_chunk.clone()),
        );
        // §3.3 热路径生产者：服务携带的 TaskRecord sink 透传给
        // 每个 orchestrator（未设置 = None，orchestrator 静默）。
        if let Some(sink) = self.task_record_sink.clone() {
            orch = orch.with_task_record_sink(sink);
        }
        agent = agent.with_sub_agent_orchestrator(Arc::new(orch));

        // 总管方案 P1：成员运行器（dispatch_member 的执行侧）。与上面的
        // 子代理编排器并存且互不沾染——成员是兄弟会话（depth 重置、无
        // 12 轮/8K 上限），不是 delegate_task 子代理（决策 D0）。工具本体
        // 在 server state 构建时注册一次；执行侧进程级**首装一次**（running
        // map 必须跨会话存活，否则 stop 对在跑成员失效），后续会话调用只
        // 刷新成员事件 emitter（跟当前 WS 通道走）。
        let member_runner = Arc::new(crate::member::runner::MemberRunner::from_parent(
            &agent,
            sub_agent_hermix_home,
            Some(on_chunk.clone()),
        ));
        crate::member::runner::install_member_runner(member_runner, member_events_live);

        agent
    }
}
