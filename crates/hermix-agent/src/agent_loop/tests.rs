use std::collections::HashMap;
use std::sync::Arc;

use super::*;
    use hermix_core::JsonSchema;
    use crate::cost_guard::{WarnEffect, estimate_usage_cost_usd};
    use crate::smart_model_routing::TurnRouteSignature;

    #[test]
    pub(crate) fn p1_loop_state_defaults_to_zero() {
        //: `LoopState::default` must produce a
        // zero-initialized struct that is `#[derive(Default)]` so the
        // refactor of `run()` / `run_stream()` can keep using `..Default::default()`.
        //: `total_api_time_ms` / `total_tool_time_ms`
        // moved into `state.timings`; access path now `s.timings.*`.
        // `last_checkpoint_messages` / `last_content_with_tools` moved
        // into `state.message_cache`; access path now
        // `s.message_cache.checkpoint().is_none()` /
        // `s.message_cache.last_content_with_tools.is_none()`.
        let s = LoopState::default();
        assert_eq!(s.total_turns, 0);
        assert_eq!(s.timings.total_api_time_ms, 0);
        assert_eq!(s.timings.total_tool_time_ms, 0);
        assert!(s.cost_guard.accumulated_usage.is_none());
        assert_eq!(s.cost_guard.session_cost_usd, 0.0);
        assert!(!s.cost_guard.cost_warned);
        assert!(s.route_state.forced_runtime_route.is_none());
        assert!(s.message_cache.checkpoint().is_none());
        assert_eq!(s.retry_counters.invalid_tool, 0);
        assert_eq!(s.retry_counters.invalid_json, 0);
        assert_eq!(s.retry_counters.truncated_tool, 0);
        assert!(s.message_cache.last_content_with_tools.is_none());
        assert_eq!(s.context_pressure.warned_tier, 0.0);
        assert!(s.context_pressure.last_warn_at.is_none());
        assert_eq!(s.context_pressure.last_warn_percent, 0.0);
    }

    #[test]
    pub(crate) fn p1_loop_state_counters_increment_independently() {
        // Sanity: the per-turn counters can be updated without touching
        // unrelated state — required because `run()` and `run_stream()`
        // will both mutate these fields on every turn.
        //: timings now use `state.timings.record_*`
        // (saturating-add) instead of a top-level `+= 1`. We exercise
        // the new path here and assert it doesn't bleed into other
        // counters.
        let mut s = LoopState::default();
        s.total_turns += 1;
        s.retry_counters.bump_invalid_tool();
        s.retry_counters.bump_invalid_json();
        s.retry_counters.bump_invalid_json();
        s.cost_guard.session_cost_usd += 0.001;
        s.timings.record_api(42);
        s.timings.record_tool(99);
        assert_eq!(s.total_turns, 1);
        assert_eq!(s.retry_counters.invalid_tool, 1);
        assert_eq!(s.retry_counters.invalid_json, 2);
        assert!(s.cost_guard.session_cost_usd > 0.0);
        // Unrelated fields stay zero.
        assert_eq!(s.timings.total_api_time_ms, 42);
        assert_eq!(s.timings.total_tool_time_ms, 99);
        assert!(s.cost_guard.accumulated_usage.is_none());
    }

    #[test]
    pub(crate) fn p5_turn_timings_record_independently() {
        //: `record_api` and `record_tool` must
        // accumulate independently (each on its own `u64`), saturating
        // on overflow (a long session shouldn't wrap around). Default
        // must be all zeros so `LoopState::default()` semantics stay
        // unchanged from the pre-extraction two-`u64`-field version.
        let mut t = TurnTimings::default();
        assert_eq!(t.total_api_time_ms, 0);
        assert_eq!(t.total_tool_time_ms, 0);

        t.record_api(100);
        t.record_api(50);
        t.record_tool(200);
        assert_eq!(t.total_api_time_ms, 150);
        assert_eq!(t.total_tool_time_ms, 200);

        // Saturating: u64::MAX + 1 stays at u64::MAX instead of wrapping
        // to 0 (which would silently under-report a long session).
        let mut t2 = TurnTimings::default();
        t2.record_api(u64::MAX);
        t2.record_api(1);
        assert_eq!(t2.total_api_time_ms, u64::MAX);
    }

    #[test]
    pub(crate) fn p5_message_cache_store_and_take_independently() {
        //: `store_checkpoint` /
        // `store_content_with_tools` / `take_content_with_tools` /
        // `checkpoint` / `content_with_tools` must all behave
        // independently (the two caches are unrelated fields, not
        // aliases of each other) and `take_content_with_tools` must
        // clear the slot behind it (so a second `take` returns
        // `None`). Default must be all-`None` so
        // `LoopState::default()` semantics stay unchanged.
        let mut c = MessageCache::default();
        assert!(c.checkpoint().is_none());
        assert!(c.last_content_with_tools.is_none());

        let snapshot = vec![Message::user("checkpoint text")];
        c.store_checkpoint(snapshot.clone());
        c.store_content_with_tools(Some("visible text".to_string()));
        assert_eq!(c.checkpoint().unwrap(), &snapshot);
        assert_eq!(
            c.last_content_with_tools.as_deref().unwrap(),
            "visible text"
        );

        // take_content_with_tools moves the value out and clears the
        // slot; checkpoint is untouched (take is per-slot).
        let taken = c.take_content_with_tools();
        assert_eq!(taken, Some("visible text".to_string()));
        assert!(c.last_content_with_tools.is_none());
        assert!(c.checkpoint().is_some());

        // store_content_with_tools(None) is a no-op for callers that
        // want to clear without consuming (in case `None` means
        // "explicitly nothing" vs. the implicit `None` after a take).
        c.store_content_with_tools(None);
        assert!(c.last_content_with_tools.is_none());
    }

    #[test]
    pub(crate) fn p1_1_loop_state_round_trip() {
        //: the bridge between `run`'s 14
        // loop-local aliases and the 14 `state` fields. Publishing the
        // locals back into `state` is what makes the future
        // `run_shared_loop(state, llm_caller)` extraction possible
        // (the shared body only sees `&mut LoopState`).
        //: the `publish_from` bridge shrank from
        // 9 args to 5 args. The 4 cost-guard args (accumulated_usage /
        // session_cost_usd / cost_warned / forced_runtime_route) are
        // owned by `state.cost_guard` and are no longer bridged. The 3
        // retry-counter args and the 3 context-pressure args were
        //already removed in earlier sub-commits.
        //(, 1st follow-up): shrank from 5 args to
        // 3 args. The 2 timing args (`total_api_time_ms` /
        // `total_tool_time_ms`) are owned by `state.timings` and
        // mutated in place by `record_api` / `record_tool`.
        //(, 2nd follow-up): shrank from 3 args to
        // 1 arg. The 2 message-cache args
        // (`last_checkpoint_messages` / `last_content_with_tools`) are
        // owned by `state.message_cache` and mutated in place by
        // `store_checkpoint` / `store_content_with_tools`. We
        // exercise all 4 in-place paths in this test.
        //(, 3rd follow-up): the `publish_from`
        // helper is removed entirely. The single remaining arg
        // (`total_turns`) is written directly to `state.total_turns`
        // in both `run()` and `run_stream()`. The bridge is dead.
        let mut s = LoopState::default();
        // total_turns — the only field still bridged, now assigned
        // directly without going through `publish_from`.
        s.total_turns = 7;
        assert_eq!(s.total_turns, 7);
        // timings live on the inner struct — exercise the in-place
        // record path that replaced the old `+= api_elapsed` locals.
        s.timings.record_api(1234);
        s.timings.record_tool(567);
        assert_eq!(s.timings.total_api_time_ms, 1234);
        assert_eq!(s.timings.total_tool_time_ms, 567);
        // message_cache lives on the inner struct — exercise the
        // store / take / checkpoint / content_with_tools paths that
        // replaced the old `last_*` local aliases.
        let snapshot = vec![Message::user("checkpoint text")];
        s.message_cache.store_checkpoint(snapshot.clone());
        s.message_cache.store_content_with_tools(Some("visible text".to_string()));
        assert_eq!(s.message_cache.checkpoint().unwrap(), &snapshot);
        assert_eq!(
            s.message_cache.last_content_with_tools.as_deref().unwrap(),
            "visible text"
        );
        let taken = s.message_cache.take_content_with_tools();
        assert_eq!(taken, Some("visible text".to_string()));
        assert!(s.message_cache.last_content_with_tools.is_none());
        // cost_guard defaults are unchanged by the bridge — owned by
        // the inner struct and mutated in place by the runtime.
        assert!(s.cost_guard.accumulated_usage.is_none());
        assert_eq!(s.cost_guard.session_cost_usd, 0.0);
        assert!(!s.cost_guard.cost_warned);
        //forced_runtime_route moved out of
        // cost_guard into the sibling route_state manager.
        assert!(s.route_state.forced_runtime_route.is_none());
        // retry_counters live on the inner struct — set directly here
        // to mirror pre-extraction semantics (the inner struct is
        // mutated in place by bump_* / reset_* methods).
        s.retry_counters.invalid_tool = 2;
        s.retry_counters.invalid_json = 3;
        s.retry_counters.truncated_tool = 1;
        assert_eq!(s.retry_counters.invalid_tool, 2);
        assert_eq!(s.retry_counters.invalid_json, 3);
        assert_eq!(s.retry_counters.truncated_tool, 1);
        // context_pressure is on the inner struct — set directly here to
        // mirror pre-extraction semantics (the inner struct is mutated
        // in place by should_emit_warning).
        s.context_pressure.warned_tier = 0.65;
        s.context_pressure.last_warn_percent = 0.71;
        assert_eq!(s.context_pressure.warned_tier, 0.65);
        assert_eq!(s.context_pressure.last_warn_percent, 0.71);
    }

    #[test]
    pub(crate) fn test_agent_config_default() {
        let config = AgentConfig::default();
        // (claude-era regression): `default_max_turns()`
        // changed to 0 (= "unlimited", governed by ErrorLoop hard
        // stop / Cost Guard / LLM natural stop). Test was still
        // asserting 30. Aligning the assertion with the new contract;
        // see `agent_config.rs::default_max_turns()` for the rationale.
        assert_eq!(config.max_turns, 0);
        assert_eq!(config.model, "gpt-4o");
        assert!(!config.stream);
        assert_eq!(config.max_concurrent_delegates, 1);
        assert_eq!(config.memory_flush_interval, 5);
        assert_eq!(config.api_mode, ApiMode::ChatCompletions);
        assert_eq!(config.retry.max_retries, 3);
        assert!(config.session_id.is_none());
        assert!(!config.skip_memory);
        assert!(config.platform.is_none());
        assert!(!config.pass_session_id);
        assert!(config.max_cost_usd.is_none());
        assert_eq!(config.cost_guard_degrade_at_ratio, 0.8);
        assert!(config.cost_guard_degrade_model.is_none());
        assert_eq!(config.checkpoint_interval_turns, 3);
        assert_eq!(config.rollback_on_tool_error_threshold, 3);
        assert!(!config.smart_model_routing.enabled);
        assert!(config.background_review_metrics_enabled);
    }

    //── : build_session_summary regression tests ──
    // These exercise the helper that produces the session-level
    // FocusConclusion written to memory.db at session end. Without
    // this, single-shot sessions (no delegate_task) leave memory.db
    // empty and cross-session recall finds nothing.

    pub(crate) fn msg_user(s: &str) -> Message {
        Message::user(s)
    }
    pub(crate) fn msg_assistant(s: &str) -> Message {
        Message::assistant(s)
    }

    #[test]
    pub(crate) fn build_session_summary_returns_none_without_user_turns() {
        let msgs = vec![msg_assistant("hi")];
        assert!(crate::memory_hooks::build_session_summary(&msgs).is_none());
    }

    #[test]
    pub(crate) fn build_session_summary_returns_none_for_empty_user_content() {
        let msgs = vec![msg_user("   ")];
        assert!(crate::memory_hooks::build_session_summary(&msgs).is_none());
    }

    #[test]
    pub(crate) fn build_session_summary_picks_last_user_and_assistant() {
        let msgs = vec![
            msg_user("first question"),
            msg_assistant("first answer"),
            msg_user("second question"),
            msg_assistant("second answer"),
        ];
        let s = crate::memory_hooks::build_session_summary(&msgs).expect("some summary");
        assert!(s.contains("second question"));
        assert!(s.contains("second answer"));
        assert!(!s.contains("first question"));
    }

    #[test]
    pub(crate) fn build_session_summary_handles_user_only() {
        let msgs = vec![msg_user("hello")];
        let s = crate::memory_hooks::build_session_summary(&msgs).expect("some summary");
        assert_eq!(s, "USER: hello");
    }

    #[test]
    pub(crate) fn build_session_summary_truncates_long_user_and_assistant() {
        let long_user: String = "x".repeat(500);
        let long_assistant: String = "y".repeat(1000);
        let msgs = vec![msg_user(&long_user), msg_assistant(&long_assistant)];
        let s = crate::memory_hooks::build_session_summary(&msgs).expect("some summary");
        // The 200-char user truncation produces 200 chars + "…" appended
        // to the prefix — assert no runaway growth.
        assert!(
            s.chars().count() < 700,
            "summary should be truncated, got {} chars",
            s.chars().count()
        );
        assert!(s.contains("…"), "long content should be truncated with …");
    }

    #[test]
    pub(crate) fn test_plugin_hooks_include_api_and_session_finalize_paths() {
        use crate::plugins::{Plugin, PluginContext, PluginMeta};
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("done"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        struct CaptureHooksPlugin {
            seen: Arc<std::sync::Mutex<Vec<String>>>,
        }
        #[async_trait::async_trait]
        impl Plugin for CaptureHooksPlugin {
            fn meta(&self) -> PluginMeta {
                PluginMeta {
                    name: "capture-hooks".to_string(),
                    version: "0.1.0".to_string(),
                    description: "capture hook events".to_string(),
                    author: None,
                }
            }

            async fn initialize(&self) -> Result<(), AgentError> {
                Ok(())
            }

            async fn shutdown(&self) -> Result<(), AgentError> {
                Ok(())
            }

            fn register(&self, ctx: &mut PluginContext) {
                let hook_types = [
                    HookType::PreApiRequest,
                    HookType::PostApiRequest,
                    HookType::OnSessionEnd,
                    HookType::OnSessionFinalize,
                    HookType::OnSessionReset,
                ];
                for hook in hook_types {
                    let seen = self.seen.clone();
                    ctx.on(
                        hook,
                        Arc::new(move |_ctx| {
                            seen.lock()
                                .expect("seen lock")
                                .push(hook.as_str().to_string());
                            HookResult::Ok
                        }),
                    );
                }
            }
        }

        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let mut pm = PluginManager::new();
        pm.register(Arc::new(CaptureHooksPlugin { seen: seen.clone() }));
        let pm = Arc::new(std::sync::Mutex::new(pm));

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        )
        .with_plugins(pm);

        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let _ = rt
            .block_on(agent.run(vec![Message::user("/reset")], None))
            .expect("agent run should succeed");

        let seen = seen.lock().expect("seen lock");
        for required in [
            "pre_api_request",
            "post_api_request",
            "on_session_end",
            "on_session_finalize",
            "on_session_reset",
        ] {
            assert!(
                seen.iter().any(|name| name == required),
                "missing required hook: {required}; seen={:?}",
                *seen
            );
        }
    }

    #[test]
    pub(crate) fn test_plugin_hooks_include_api_paths_in_stream_mode() {
        use crate::plugins::{Plugin, PluginContext, PluginMeta};
        use futures::stream::BoxStream;

        struct StreamingProvider;
        #[async_trait::async_trait]
        impl LlmProvider for StreamingProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("done"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::iter(vec![
                    Ok(StreamChunk {
                        delta: Some(hermix_core::StreamDelta {
                            content: Some("stream-done".to_string()),
                            tool_calls: None,
                            extra: None,
                        }),
                        finish_reason: None,
                        usage: None,
                    }),
                    Ok(StreamChunk {
                        delta: None,
                        finish_reason: Some("stop".to_string()),
                        usage: None,
                    }),
                ])
                .boxed()
            }
        }

        struct CaptureHooksPlugin {
            seen: Arc<std::sync::Mutex<Vec<String>>>,
        }
        #[async_trait::async_trait]
        impl Plugin for CaptureHooksPlugin {
            fn meta(&self) -> PluginMeta {
                PluginMeta {
                    name: "capture-hooks-stream".to_string(),
                    version: "0.1.0".to_string(),
                    description: "capture stream hook events".to_string(),
                    author: None,
                }
            }

            async fn initialize(&self) -> Result<(), AgentError> {
                Ok(())
            }

            async fn shutdown(&self) -> Result<(), AgentError> {
                Ok(())
            }

            fn register(&self, ctx: &mut PluginContext) {
                for hook in [HookType::PreApiRequest, HookType::PostApiRequest] {
                    let seen = self.seen.clone();
                    ctx.on(
                        hook,
                        Arc::new(move |_ctx| {
                            seen.lock()
                                .expect("seen lock")
                                .push(hook.as_str().to_string());
                            HookResult::Ok
                        }),
                    );
                }
            }
        }

        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let mut pm = PluginManager::new();
        pm.register(Arc::new(CaptureHooksPlugin { seen: seen.clone() }));
        let pm = Arc::new(std::sync::Mutex::new(pm));

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(StreamingProvider),
        )
        .with_plugins(pm);

        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let _ = rt
            .block_on(agent.run_stream(
                vec![Message::user("stream hello")],
                None,
                Some(Box::new(|_| {})),
            ))
            .expect("stream run should succeed");

        let seen = seen.lock().expect("seen lock");
        assert!(seen.iter().any(|name| name == "pre_api_request"));
        assert!(seen.iter().any(|name| name == "post_api_request"));
    }

    #[test]
    pub(crate) fn test_llm_request_model_strips_provider_prefix() {
        use futures::stream::BoxStream;

        struct CaptureModelProvider {
            seen_model: Arc<std::sync::Mutex<Option<String>>>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for CaptureModelProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                *self.seen_model.lock().expect("seen_model lock") = model.map(|m| m.to_string());
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("done"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let seen_model = Arc::new(std::sync::Mutex::new(None));
        let provider = CaptureModelProvider {
            seen_model: seen_model.clone(),
        };
        let config = AgentConfig {
            model: "openrouter:deepseek/deepseek-v3.2".to_string(),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(config, Arc::new(ToolRegistry::new()), Arc::new(provider));

        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let _ = rt
            .block_on(agent.run(vec![Message::user("hello")], None))
            .expect("agent run should succeed");

        let seen = seen_model.lock().expect("seen_model lock");
        assert_eq!(seen.as_deref(), Some("deepseek/deepseek-v3.2"));
    }

    /// end-to-end check that
    /// `auto_compress_if_over_threshold` emits at least one
    /// `compaction_status` snapshot (pre- and post-attempt) and that
    /// the snapshot carries the live pressure numbers. Mirrors the
    /// `status_callback_receives_context_pressure_messages` test
    /// above — that one covered the *string* `lifecycle` event, this
    /// one covers the *structured* `BreakerSnapshot` event the
    /// `CompactionStatusBanner` consumes.
    #[tokio::test]
    pub(crate) async fn compaction_status_callback_receives_snapshots() {
        use crate::compression::{CompressorConfig, ContextCompressor};
        use futures::stream::BoxStream;
        use hermix_intelligence::auxiliary::{
            AuxiliaryClient, AuxiliarySource, ProviderCandidate,
        };
        use std::sync::Arc;

        struct LocalDummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for LocalDummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        // Capture every snapshot the callback sees — the test
        // asserts both the pressure-detected pre-snapshot and the
        // post-attempt snapshot arrive with non-zero pressure.
        let captured: Arc<std::sync::Mutex<Vec<crate::compression::BreakerSnapshot>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            on_compaction_status: Some(Arc::new(move |snap| {
                cap_ref
                    .lock()
                    .expect("compaction capture lock")
                    .push(snap.clone());
            })),
            ..Default::default()
        };

        // Build a minimal compressor with an in-process auxiliary
        // provider so `compress()` can produce a real summary.
        // Pattern mirrored from `compression::tests::aux_with_provider`.
        let aux = Arc::new(
            AuxiliaryClient::builder()
                .add_candidate(ProviderCandidate::new(
                    AuxiliarySource::Custom,
                    "test-model",
                    Arc::new(LocalDummyProvider) as Arc<dyn LlmProvider>,
                ))
                .build(),
        );
        let mut cfg = CompressorConfig::default();
        // Match `context_length` to the ctx max below (100 tokens) so the
        // engine's own `threshold_tokens` (100 * 0.8 = 80) agrees with the
        // outer `ContextManager.pressure_threshold_tokens()` (also 80). When
        //they disagreed (cfg 1000 vs ctx 100) the post-
        // `should_compress` opt-out in `auto_compress_if_over_threshold`
        // short-circuited before the post-attempt snapshot fired.
        cfg.context_length = 100;
        cfg.threshold_percent = 0.8;
        cfg.quiet_mode = true;
        let compressor = ContextCompressor::new(cfg, aux);

        // The agent LLM and the compressor's auxiliary client hold
        // independent provider chains. `LocalDummyProvider` is a zero
        // sized struct with empty state, so a fresh `Arc::new` per
        // use site costs nothing and shares nothing.
        let mut agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(LocalDummyProvider),
        )
        .with_callbacks(callbacks);
        agent.compression = Some(Arc::new(tokio::sync::Mutex::new(compressor)));

        // Force the context over threshold. `ContextManager::new(100)`
        // sets max=100; we zero the output reserve so the pressure
        // threshold is the documented 80 tokens. The user message
        // needs to tokenise to > 80 — `estimate_tokens` divides by
        // 4, so 400 chars yields 100 tokens, comfortably above.
        // `compress()` requires `n_messages > protect_first_n + 4`
        // (7 messages by default), so we add several user+assistant
        // pairs to give the compressor real middle-turns to summarise.
        let mut ctx = ContextManager::new(100).with_output_reserve(0);
        for i in 0..12 {
            ctx.add_message(Message::user(format!("user {} {}", i, "x".repeat(80))));
            ctx.add_message(Message::assistant(format!("assistant {} {}", i, "y".repeat(80))));
        }
        agent.auto_compress_if_over_threshold(&mut ctx).await;

        let snaps = captured.lock().expect("snaps lock");
        assert!(
            snaps.len() >= 2,
            "expected pressure + post-attempt snapshots, got {}",
            snaps.len()
        );
        // Pre-snapshot must carry the live pressure so the banner
        // can render "x / 80 tokens" before any LLM work.
        assert!(snaps[0].used_tokens > 0, "pre-snap used_tokens=0");
        assert_eq!(snaps[0].threshold_tokens, 90, "pre-snap threshold");
        // Post-snapshot — state stays Closed (no failures).
        // `compression_count` is always 0 because the engine-agnostic
        // snapshot bridge (`CompressionEngineSnapshot` → `BreakerSnapshot`)
        // defaults it. What we CAN assert: state is Closed and the
        // threshold is populated.
        let last = snaps.last().expect("at least one snap");
        assert_eq!(last.state, crate::compression::BreakerState::Closed);
        assert!(last.used_tokens > 0, "post-snap used_tokens=0");
        assert_eq!(last.threshold_tokens, 90, "post-snap threshold");
    }

    #[test]
    pub(crate) fn summarize_background_review_nothing_to_save() {
        let msgs = vec![Message::assistant("Nothing to save.")];
        let out = summarize_background_review_result(&msgs);
        assert!(out.is_none());
    }

    #[test]
    pub(crate) fn summarize_background_review_counts_tool_calls() {
        let msgs = vec![
            Message::tool_result(
                "tc_mem",
                "{\"success\":true,\"message\":\"Skill 'prospect-scanner' created.\"}",
                Some("memory"),
            ),
            Message::tool_result(
                "tc_skill",
                "{\"success\":true,\"message\":\"Entry added\",\"target\":\"memory\"}",
                Some("skill"),
            ),
            Message::tool_result("tc_skip", "{\"success\":false,\"message\":\"failed\"}", Some("skip")),
        ];
        let out = summarize_background_review_result(&msgs).expect("summary should exist");
        assert!(out.starts_with("💾 "));
        assert!(out.contains("Skill 'prospect-scanner' created."));
        assert!(out.contains("Memory updated"));
    }

    #[tokio::test]
    pub(crate) async fn status_callback_receives_context_pressure_messages() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        )
        .with_callbacks(callbacks);

        let mut ctx = ContextManager::new(100);
        ctx.add_message(Message::user("x".repeat(90)));
        agent.auto_compress_if_over_threshold(&mut ctx).await;

        let rows = captured.lock().expect("captured lock");
        assert!(rows
            .iter()
            .any(|(kind, msg)| kind == "lifecycle" && msg.contains("triggering compression")));
    }

    #[tokio::test]
    pub(crate) async fn status_callback_receives_empty_response_retry_notice() {
        use futures::stream::BoxStream;

        #[derive(Default)]
        struct RetryDummyProvider {
            calls: std::sync::Mutex<u32>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for RetryDummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                let mut n = self.calls.lock().expect("calls lock");
                *n += 1;
                let msg = if *n == 1 {
                    Message::assistant("")
                } else {
                    Message::assistant("ok")
                };
                Ok(hermix_core::LlmResponse {
                    message: msg,
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let cfg = AgentConfig {
            // 软上限语义（run_loop 重构后）：边界轮不再硬停，而是先发一次
            // 额外的进度总结调用再继续；且 do_pre_turn_setup 先自增
            // total_turns 再判边界，max_turns=1 会让首轮预处理就触发软上限、
            // 吞掉本次要测的首个空响应调用。给足余量，隔离出被测的
            // 「空响应内层重试」路径。
            max_turns: 10,
            empty_content_max_retries: 1,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            cfg,
            Arc::new(ToolRegistry::new()),
            Arc::new(RetryDummyProvider::default()),
        )
        .with_callbacks(callbacks);

        let result = agent.run(vec![Message::user("hello")], None).await;
        assert!(result.is_ok());
        let rows = captured.lock().expect("captured lock");
        assert!(rows.iter().any(|(kind, msg)| {
            kind == "lifecycle" && msg.contains("Empty assistant response — retrying")
        }));
    }

    #[tokio::test]
    pub(crate) async fn stream_collect_emits_activity_while_waiting_for_chunks() {
        use async_stream::stream;
        use futures::stream::BoxStream;

        struct DelayedStreamProvider;

        #[async_trait::async_trait]
        impl LlmProvider for DelayedStreamProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("noop"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                // Delay long enough for both warm (1s) and stall (1s) timers to fire.
                // Use 5s to give tokio scheduler plenty of room.
                Box::pin(stream! {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    yield Ok(StreamChunk {
                        delta: Some(hermix_core::StreamDelta {
                            content: Some("hi".into()),
                            tool_calls: None,
                            extra: None,
                        }),
                        finish_reason: None,
                        usage: None,
                    });
                    yield Ok(StreamChunk {
                        delta: None,
                        finish_reason: Some("stop".into()),
                        usage: None,
                    });
                })
            }
        }

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let cfg = AgentConfig {
            stream_activity_interval_secs: 1,
            stream_activity_stall_secs: 1,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            cfg,
            Arc::new(ToolRegistry::new()),
            Arc::new(DelayedStreamProvider),
        )
        .with_callbacks(callbacks);

        let mut ctx = ContextManager::new(10_000);
        ctx.add_message(Message::user("ping"));

        let noop: &(dyn Fn(StreamChunk) + Send + Sync) = &|_chunk| {};
        let outcome = agent
            .collect_stream_llm_response(&ctx, &[], None, "openai:gpt-4o", noop)
            .await
            .expect("stream collect");

        assert!(matches!(outcome, StreamCollectOutcome::Complete(_)));
        let rows = captured.lock().expect("captured lock");
        let warm = rows
            .iter()
            .any(|(k, m)| k == "activity" && m.contains("Still waiting"));
        let stall = rows
            .iter()
            .any(|(k, m)| k == "activity" && m.contains("may be stuck"));
        // At least one activity message should fire during a 5s stall.
        // Both warm and stall are expected but the exact timing depends on
        // tokio scheduling, so we accept either one being present.
        assert!(
            warm || stall,
            "expected at least one activity message (warm or stall); got {:?}",
            *rows
        );
    }

    #[tokio::test]
    pub(crate) async fn quiet_mode_suppresses_status_callback() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let cfg = AgentConfig {
            quiet_mode: true,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), Arc::new(DummyProvider))
            .with_callbacks(callbacks);
        let mut ctx = ContextManager::new(100);
        ctx.add_message(Message::user("x".repeat(90)));
        agent.auto_compress_if_over_threshold(&mut ctx).await;

        assert!(captured.lock().expect("captured lock").is_empty());
    }

    #[test]
    pub(crate) fn test_builtin_personality_injected_into_system_prompt() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let config = AgentConfig {
            personality: Some("coder".to_string()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let prompt = crate::prompt_assembly::build_system_prompt(&agent.config, &agent.prompt_cache, agent.memory_router.as_deref(), "", &[], "gpt-4o", 0);
        assert!(prompt.contains("## Active Personality (coder)"));
        // Localized to Chinese — verify the role label is present.
        assert!(prompt.contains("工程师"));
    }

    #[test]
    pub(crate) fn test_unknown_personality_name_does_not_add_overlay_block() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let config = AgentConfig {
            personality: Some("unknown_persona".to_string()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let prompt = crate::prompt_assembly::build_system_prompt(&agent.config, &agent.prompt_cache, agent.memory_router.as_deref(), "", &[], "gpt-4o", 0);
        assert!(!prompt.contains("## Active Personality (unknown_persona)"));
    }

    #[test]
    pub(crate) fn test_formatting_discipline_guidance_in_system_prompt() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let config = AgentConfig::default();
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        // No personality, no tools, no plugin context — thinking discipline
        // (which includes output format) must still be present in every prompt.
        let prompt = crate::prompt_assembly::build_system_prompt(&agent.config, &agent.prompt_cache, agent.memory_router.as_deref(), "", &[], "gpt-4o", 0);
        assert!(
            prompt.contains("Output format"),
            "output format block missing from system prompt"
        );
        assert!(prompt.contains("think"));
        assert!(
            prompt.contains("Always close"),
            "think-block closing discipline missing"
        );
    }

    #[test]
    pub(crate) fn test_active_workspace_block_injected_when_set() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        // No workspace → no block.
        let no_ws_agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let prompt = crate::prompt_assembly::build_system_prompt(&no_ws_agent.config, &no_ws_agent.prompt_cache, no_ws_agent.memory_router.as_deref(), "", &[], "gpt-4o", 0);
        assert!(
            !prompt.contains("## Active Workspace"),
            "workspace block should be absent when no workspace is configured"
        );

        // With workspace → block surfaces the path verbatim.
        let mut config = AgentConfig::default();
        config.workspace = Some("/work/qcc".to_string());
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let prompt = crate::prompt_assembly::build_system_prompt(&agent.config, &agent.prompt_cache, agent.memory_router.as_deref(), "", &[], "gpt-4o", 0);
        assert!(
            prompt.contains("## Active Workspace"),
            "workspace block missing"
        );
        assert!(
            prompt.contains("/work/qcc"),
            "workspace path missing from block"
        );
    }

    #[test]
    pub(crate) fn test_tool_usage_guidance_in_system_prompt() {
        // TOOL_USAGE_GUIDANCE must be present in every prompt (any model, any toolset)
        // because the "use a generic tool when a purpose-built one exists" failure
        // mode is universal. Don't gate it on tool_enforcement model list.
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let config = AgentConfig::default();
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );

        // 1. Guidance present for any model (even one not in the tool-enforcement list).
        let prompt = crate::prompt_assembly::build_system_prompt(&agent.config, &agent.prompt_cache, agent.memory_router.as_deref(), "", &[], "unknown-model-xyz", 0);
        assert!(
            prompt.contains("Tool selection discipline"),
            "TOOL_USAGE_GUIDANCE block must be unconditional, not gated on model"
        );
        assert!(
            prompt.contains("scan the available tool list"),
            "decision pattern step 1 (scan tools) missing"
        );
        assert!(
            prompt.contains("purpose-built alternative"),
            "decision pattern step 2 (use purpose-built tool first) missing"
        );
        assert!(
            prompt.contains("Forbidden shapes"),
            "forbidden-shapes block (anti-pattern equivalent) missing"
        );

        // 2. MiniMax models must now be in the tool-enforcement whitelist so they
        // get the "must use tools" guidance in addition to the selection discipline.
        assert!(
            crate::prompt_assembly::should_inject_tool_enforcement("minimax-m2.7"),
            "MiniMax M2.7 should be in tool-enforcement whitelist"
        );
        assert!(
            crate::prompt_assembly::should_inject_tool_enforcement("M2-highspeed"),
            "M2 variant should be in tool-enforcement whitelist"
        );
        // Spot-check existing whitelist still works.
        assert!(crate::prompt_assembly::should_inject_tool_enforcement("gpt-4o"));
        assert!(!crate::prompt_assembly::should_inject_tool_enforcement("claude-sonnet-4"));
    }

    #[test]
    pub(crate) fn test_explore_efficiency_and_parallel_guidance_in_system_prompt() {
        // EXPLORE_EFFICIENCY_GUIDANCE + PARALLEL_EXECUTION_GUIDANCE must be
        // present in the prompt so the agent stops the "recon-then-act"
        // anti-pattern (list dirs / fetch metadata / write temp scripts before
        // touching the target) and issues independent calls in parallel.
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let config = AgentConfig::default();
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );

        // 1. Exploration-efficiency guidance present on the FIRST turn (empty
        // user message => FirstTurn route), regardless of model/toolset.
        // This is what fixes the "list dir then read" recon anti-pattern.
        let first_turn = crate::prompt_assembly::build_system_prompt(
            &agent.config,
            &agent.prompt_cache,
            agent.memory_router.as_deref(),
            "",
            &[],
            "unknown-model-xyz",
            0,
        );
        assert!(
            first_turn.contains("Exploration efficiency"),
            "EXPLORE_EFFICIENCY_GUIDANCE block must be injected on FirstTurn"
        );
        assert!(
            first_turn.contains("Don't re-read"),
            "explore anti-pattern (don't re-read/re-list) missing"
        );
        assert!(
            first_turn.contains("list the same directory twice"),
            "explore de-dup rule (no double dir listing) missing"
        );
        assert!(
            first_turn.contains("temp script files"),
            "explore rule (no temp scripts) missing"
        );

        // 2. Parallel-execution guidance must be model-family-agnostic (Always
        // route) — previously it was only inside GOOGLE_MODEL_OPERATIONAL_GUIDANCE,
        // so non-Google models serialized every call. Assert it shows up for a
        // model NOT in the tool-enforcement whitelist.
        assert!(
            !crate::prompt_assembly::should_inject_tool_enforcement("claude-sonnet-4"),
            "precondition: claude must be outside the whitelist for this test to be meaningful"
        );
        let non_google = crate::prompt_assembly::build_system_prompt(
            &agent.config,
            &agent.prompt_cache,
            agent.memory_router.as_deref(),
            "analyze the rendering code",
            &[],
            "claude-sonnet-4",
            0,
        );
        assert!(
            non_google.contains("Parallel tool execution"),
            "PARALLEL_EXECUTION_GUIDANCE must be unconditional (non-Google model)"
        );
        assert!(
            non_google.contains("in the same turn in parallel"),
            "parallel instruction (issue calls in parallel) missing"
        );
    }

    #[test]
    pub(crate) fn test_smart_model_routing_cheap_route_for_simple_turn() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "openai".to_string(),
            RuntimeProviderConfig {
                api_key: Some("sk-test-key".to_string()),
                base_url: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("openai".to_string()),
                    model: Some("gpt-4o-mini".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let messages = vec![Message::user("帮我总结一下今天要做什么")];
        let selected = agent.resolve_smart_runtime_route(&messages);
        assert_eq!(
            selected.as_ref().map(|r| r.model.as_str()),
            Some("gpt-4o-mini")
        );
    }

    #[test]
    pub(crate) fn test_runtime_provider_command_args_override_primary_acp_metadata() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "openai".to_string(),
            RuntimeProviderConfig {
                api_key: Some("sk-test-key".to_string()),
                base_url: Some("https://api.openai.com/v1".to_string()),
                command: Some("copilot-language-server".to_string()),
                args: vec![
                    "--stdio".to_string(),
                    "--model".to_string(),
                    "gpt-4o-mini".to_string(),
                ],
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            provider: Some("openai".to_string()),
            runtime_providers,
            acp_command: Some("global-acp".to_string()),
            acp_args: vec!["--global".to_string()],
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let primary = crate::cost_routing::primary_runtime_snapshot(
            &agent.config,
            agent.primary_credential_pool.as_ref(),
        );
        assert_eq!(primary.command.as_deref(), Some("copilot-language-server"));
        assert_eq!(
            primary.args,
            vec![
                "--stdio".to_string(),
                "--model".to_string(),
                "gpt-4o-mini".to_string()
            ]
        );
    }

    #[test]
    pub(crate) fn test_smart_model_routing_codex_provider_alias_builds_runtime() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "codex".to_string(),
            RuntimeProviderConfig {
                api_key: Some("sk-test-key".to_string()),
                base_url: Some("https://api.openai.com/v1".to_string()),
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("codex".to_string()),
                    model: Some("gpt-5-mini".to_string()),
                    base_url: Some("https://api.openai.com/v1".to_string()),
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let messages = vec![Message::user("总结一下这个需求")];
        let selected = agent.resolve_smart_runtime_route(&messages);
        assert_eq!(
            selected.as_ref().map(|r| r.model.as_str()),
            Some("gpt-5-mini")
        );
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("codex")
        );
        assert_eq!(
            selected.as_ref().and_then(|r| r.api_mode.as_ref()),
            Some(&ApiMode::CodexResponses)
        );
    }

    #[test]
    pub(crate) fn test_smart_model_routing_qwen_oauth_alias_builds_runtime() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "qwen-oauth".to_string(),
            RuntimeProviderConfig {
                api_key: Some("sk-qwen-oauth".to_string()),
                base_url: Some("https://dashscope.aliyuncs.com/compatible-mode/v1".to_string()),
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("qwen-oauth".to_string()),
                    model: Some("qwen3-coder-plus".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let selected = agent.resolve_smart_runtime_route(&[Message::user("给我一段简短总结")]);
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("qwen-oauth")
        );
    }

    #[test]
    pub(crate) fn test_smart_model_routing_openai_codex_reads_auth_store_token() {
        use futures::stream::BoxStream;
        use std::time::{SystemTime, UNIX_EPOCH};

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let home = std::env::temp_dir().join(format!("hermes-oauth-fixture-{}", nonce));
        let auth_dir = home.join("auth");
        std::fs::create_dir_all(&auth_dir).expect("create auth dir");
        std::fs::write(
            auth_dir.join("tokens.json"),
            r#"{
  "openai-codex": {
    "provider": "openai-codex",
    "access_token": "codex-oauth-token",
    "token_type": "bearer",
    "refresh_token": null,
    "scope": null,
    "expires_at": "2099-01-01T00:00:00Z"
  }
}"#,
        )
        .expect("write token store");

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "openai-codex".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                base_url: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            hermix_home: Some(home.to_string_lossy().to_string()),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("openai-codex".to_string()),
                    model: Some("gpt-5-codex".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let selected = agent.resolve_smart_runtime_route(&[Message::user("帮我总结这段话")]);
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("openai-codex")
        );
        // Best effort cleanup.
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    pub(crate) fn test_self_evolution_skill_counter_ticks_each_iteration() {
        use futures::stream::BoxStream;
        use hermix_core::JsonSchema;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("done"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut registry = ToolRegistry::new();
        registry.register(
            "skill_manage",
            ToolSchema::new("skill_manage", "Manage skills", JsonSchema::new("object")),
            Arc::new(|_args| Ok("{\"success\":true}".to_string())),
        );

        let config = AgentConfig {
            skill_creation_nudge_interval: 10,
            // Opt out: these tests assert on iteration counters, which
            // require the loop to terminate after the model emits a final
            // text-only answer. autonomous_mode (default true) would keep
            // nudging the model back into action.
            autonomous_mode: false,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(config, Arc::new(registry), Arc::new(DummyProvider));
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let _ = rt
            .block_on(agent.run(vec![Message::user("hello")], None))
            .expect("agent run should succeed");

        let counters = agent.evolution_counters.lock().expect("counter lock");
        assert_eq!(counters.iters_since_skill, 1);
    }

    #[test]
    pub(crate) fn test_self_evolution_parity_fixtures_v2026_4_13_memory_nudge() {
        use futures::stream::BoxStream;
        use hermix_core::JsonSchema;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("done"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        //Fixture-style cases distilled from Python :
        // - counter persists across runs
        // - resets to 0 when hitting interval threshold
        #[derive(Clone, Copy)]
        struct Case {
            runs: u32,
            expected_turns_since_memory: u32,
        }
        let cases = vec![
            Case {
                runs: 1,
                expected_turns_since_memory: 1,
            },
            Case {
                runs: 2,
                expected_turns_since_memory: 0,
            },
        ];

        for case in cases {
            let mut registry = ToolRegistry::new();
            registry.register(
                "memory",
                ToolSchema::new("memory", "Memory tool", JsonSchema::new("object")),
                Arc::new(|_args| Ok("{\"success\":true}".to_string())),
            );

            let config = AgentConfig {
                memory_nudge_interval: 2,
                ..AgentConfig::default()
            };
            let agent = AgentLoop::new(config, Arc::new(registry), Arc::new(DummyProvider));
            let rt = tokio::runtime::Runtime::new().expect("runtime");
            for _ in 0..case.runs {
                let _ = rt
                    .block_on(agent.run(vec![Message::user("hello")], None))
                    .expect("agent run should succeed");
            }
            let counters = agent.evolution_counters.lock().expect("counter lock");
            assert_eq!(
                counters.turns_since_memory, case.expected_turns_since_memory,
                "fixture runs={} mismatch",
                case.runs
            );
        }
    }

    #[test]
    pub(crate) fn test_iters_since_skill_resets_then_reincrements_on_followup_iteration() {
        use futures::stream::BoxStream;
        use std::sync::atomic::{AtomicU32, Ordering};

        struct TwoStepProvider {
            calls: Arc<AtomicU32>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for TwoStepProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                let msg = if n == 0 {
                    Message::assistant_with_tool_calls(
                        None,
                        vec![hermix_core::ToolCall {
                            id: "tc_skill".to_string(),
                            function: hermix_core::FunctionCall {
                                name: "skill_manage".to_string(),
                                arguments: "{}".to_string(),
                            },
                        }],
                    )
                } else {
                    Message::assistant("done")
                };
                Ok(hermix_core::LlmResponse {
                    message: msg,
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut registry = ToolRegistry::new();
        registry.register(
            "skill_manage",
            ToolSchema::new("skill_manage", "Manage skills", JsonSchema::new("object")),
            Arc::new(|_args| Ok("{\"success\":true}".to_string())),
        );

        let config = AgentConfig {
            skill_creation_nudge_interval: 10,
            // Opt out: these tests assert on iteration counters, which
            // require the loop to terminate after the model emits a final
            // text-only answer. autonomous_mode (default true) would keep
            // nudging the model back into action.
            autonomous_mode: false,
            ..AgentConfig::default()
        };
        let provider = TwoStepProvider {
            calls: Arc::new(AtomicU32::new(0)),
        };
        let agent = AgentLoop::new(config, Arc::new(registry), Arc::new(provider));
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let _ = rt
            .block_on(agent.run(vec![Message::user("hello")], None))
            .expect("agent run should succeed");

        let counters = agent.evolution_counters.lock().expect("counter lock");
        // Iteration #1 increments then skill_manage resets to 0.
        // Iteration #2 (final assistant turn) increments again to 1.
        // Python follows the same cadence because `_iters_since_skill += 1`
        // happens at each loop iteration before the tool/reset branch.
        assert_eq!(counters.iters_since_skill, 1);
    }

    #[test]
    pub(crate) fn test_smart_model_routing_copilot_acp_missing_cli_falls_back() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "copilot-acp".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                base_url: Some("acp://copilot".to_string()),
                command: Some("definitely-not-installed-copilot-cli".to_string()),
                args: vec!["--acp".to_string(), "--stdio".to_string()],
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("copilot-acp".to_string()),
                    model: Some("gpt-4o-mini".to_string()),
                    base_url: Some("acp://copilot".to_string()),
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let selected = agent.resolve_smart_runtime_route(&[Message::user("帮我总结这段话")]);
        assert!(
            selected.is_none(),
            "missing ACP CLI should fail cheap-route and fall back"
        );
    }

    #[test]
    pub(crate) fn test_smart_model_routing_copilot_acp_tcp_mode_skips_cli_check() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "copilot-acp".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                base_url: Some("acp+tcp://127.0.0.1:8765".to_string()),
                command: Some("definitely-not-installed-copilot-cli".to_string()),
                args: vec!["--acp".to_string(), "--stdio".to_string()],
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("copilot-acp".to_string()),
                    model: Some("gpt-4o-mini".to_string()),
                    base_url: Some("acp+tcp://127.0.0.1:8765".to_string()),
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let selected = agent.resolve_smart_runtime_route(&[Message::user("帮我总结这段话")]);
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("copilot-acp")
        );
    }

    #[test]
    pub(crate) fn test_smart_model_routing_skips_complex_turn() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("openai".to_string()),
                    model: Some("gpt-4o-mini".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let messages = vec![Message::user("请帮我 debug 这段 traceback 并修复错误")];
        let selected = agent.resolve_smart_runtime_route(&messages);
        assert!(selected.is_none());
    }

    #[test]
    pub(crate) fn test_deduplicate_tool_calls() {
        let calls = vec![
            ToolCall {
                id: "1".into(),
                function: hermix_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"a.txt"}"#.into(),
                },
            },
            ToolCall {
                id: "2".into(),
                function: hermix_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"a.txt"}"#.into(),
                },
            },
            ToolCall {
                id: "3".into(),
                function: hermix_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"b.txt"}"#.into(),
                },
            },
        ];
        let deduped = AgentLoop::deduplicate_tool_calls(&calls);
        assert_eq!(deduped.len(), 2);
        assert_eq!(deduped[0].id, "1");
        assert_eq!(deduped[1].id, "3");
    }

    #[test]
    pub(crate) fn test_memory_write_event_from_tool_call_add() {
        let tc = ToolCall {
            id: "c1".into(),
            function: hermix_core::FunctionCall {
                name: "memory".into(),
                arguments:
                    r#"{"action":"add","target":"user","content":"Prefers concise answers"}"#.into(),
            },
        };
        let event = crate::memory_hooks::memory_write_event_from_tool_call(&tc).unwrap();
        assert_eq!(event.0, "add");
        assert_eq!(event.1, "user");
        assert_eq!(event.2, "Prefers concise answers");
    }

    #[test]
    pub(crate) fn test_memory_write_event_from_tool_call_remove_uses_old_text() {
        let tc = ToolCall {
            id: "c2".into(),
            function: hermix_core::FunctionCall {
                name: "memory".into(),
                arguments: r#"{"action":"remove","target":"memory","old_text":"obsolete fact"}"#
                    .into(),
            },
        };
        let event = crate::memory_hooks::memory_write_event_from_tool_call(&tc).unwrap();
        assert_eq!(event.0, "remove");
        assert_eq!(event.1, "memory");
        assert_eq!(event.2, "obsolete fact");
    }

    #[test]
    pub(crate) fn test_hydrate_session_search_args_injects_current_session_id() {
        use futures::stream::BoxStream;
        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<hermix_core::StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let config = AgentConfig {
            session_id: Some("sess-auto-1".into()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let mut tc = ToolCall {
            id: "s1".into(),
            function: hermix_core::FunctionCall {
                name: "session_search".into(),
                arguments: r#"{"query":"previous issue","limit":3}"#.into(),
            },
        };
        agent.hydrate_session_search_args(&mut tc);
        let args: Value = serde_json::from_str(&tc.function.arguments).unwrap();
        assert_eq!(
            args.get("current_session_id").and_then(|v| v.as_str()),
            Some("sess-auto-1")
        );
    }

    #[test]
    pub(crate) fn test_hydrate_session_search_args_keeps_existing_current_session_id() {
        use futures::stream::BoxStream;
        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<hermix_core::StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let config = AgentConfig {
            session_id: Some("sess-outer".into()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let mut tc = ToolCall {
            id: "s2".into(),
            function: hermix_core::FunctionCall {
                name: "session_search".into(),
                arguments: r#"{"query":"abc","current_session_id":"sess-explicit"}"#.into(),
            },
        };
        agent.hydrate_session_search_args(&mut tc);
        let args: Value = serde_json::from_str(&tc.function.arguments).unwrap();
        assert_eq!(
            args.get("current_session_id").and_then(|v| v.as_str()),
            Some("sess-explicit")
        );
    }

    #[test]
    pub(crate) fn test_budget_warning() {
        let config = AgentConfig {
            max_turns: 10,
            ..AgentConfig::default()
        };
        let registry = Arc::new(ToolRegistry::new());
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let agent = AgentLoop::new(config, registry, Arc::new(DummyProvider));

        let max = agent.config.max_turns;
        assert!(budget_pressure_text(6, max, 0.7, 0.9, true).is_none());
        assert!(budget_pressure_text(7, max, 0.7, 0.9, true).is_some());
        let w = budget_pressure_text(9, max, 0.7, 0.9, true).unwrap();
        assert!(w.contains("BUDGET WARNING"), "{w}");
        let w10 = budget_pressure_text(10, max, 0.7, 0.9, true).unwrap();
        assert!(w10.contains("BUDGET WARNING"), "{w10}");
        let _ = agent;
    }

    #[test]
    pub(crate) fn test_tool_registry_new() {
        let registry = ToolRegistry::new();
        assert!(registry.names().is_empty());
    }

    #[test]
    pub(crate) fn test_merge_usage() {
let a = UsageStats {
    prompt_tokens: 100,
    completion_tokens: 50,
    total_tokens: 150,
    estimated_cost: Some(0.01),
    cache_read_tokens: 0,
    cache_creation_tokens: 0,
    reasoning_tokens: 0,
};
let b = UsageStats {
    prompt_tokens: 200,
    completion_tokens: 100,
    total_tokens: 300,
    estimated_cost: Some(0.02),
    cache_read_tokens: 0,
    cache_creation_tokens: 0,
    reasoning_tokens: 0,
};
        let merged = merge_usage(Some(a), &b);
        assert_eq!(merged.prompt_tokens, 300);
        assert_eq!(merged.completion_tokens, 150);
        assert_eq!(merged.total_tokens, 450);
        assert_eq!(merged.estimated_cost, Some(0.03));
    }

    #[test]
    pub(crate) fn test_merge_usage_none() {
let b = UsageStats {
    prompt_tokens: 200,
    completion_tokens: 100,
    total_tokens: 300,
    estimated_cost: None,
    cache_read_tokens: 0,
    cache_creation_tokens: 0,
    reasoning_tokens: 0,
};
        let merged = merge_usage(None, &b);
        assert_eq!(merged.prompt_tokens, 200);
    }

    #[test]
    pub(crate) fn test_estimate_usage_cost_prefers_reported_estimate() {
        let cfg = AgentConfig::default();
let u = UsageStats {
    prompt_tokens: 1000,
    completion_tokens: 1000,
    total_tokens: 2000,
    estimated_cost: Some(0.42),
    cache_read_tokens: 0,
    cache_creation_tokens: 0,
    reasoning_tokens: 0,
};
        let cost = estimate_usage_cost_usd(&u, "openai:gpt-4o", &cfg).unwrap();
        assert!((cost - 0.42).abs() < 1e-9);
    }

    #[test]
    pub(crate) fn test_estimate_usage_cost_uses_model_fallback_table() {
        let cfg = AgentConfig::default();
let u = UsageStats {
    prompt_tokens: 1_000_000,
    completion_tokens: 1_000_000,
    total_tokens: 2_000_000,
    estimated_cost: None,
    cache_read_tokens: 0,
    cache_creation_tokens: 0,
    reasoning_tokens: 0,
};
        let cost = estimate_usage_cost_usd(&u, "openai:gpt-4o-mini", &cfg).unwrap();
        assert!((cost - 0.75).abs() < 1e-9);
    }

    //── ContextPressureMonitor unit tests ──
    // The helper is the gate that decides whether `run()` / `run_stream()`
    // should fire a `lifecycle` warning when the context window fills up.
    // It had no direct unit tests — coverage only came through the
    // integration path. These tests exercise the three decision rules
    // (tier upgrade, cooldown, percent step) using deterministic state
    // and rely on the helper's own `Instant::now()` for time, so no
    // wall-clock waiting is needed.

    #[test]
    pub(crate) fn cp_monitor_default_is_all_zero_and_none() {
        //the freshly-extracted ContextPressureMonitor must
        // behave like the old 3 inline fields when not yet primed —
        // otherwise the first call inside run() / run_stream() would
        // see stale defaults.
        let m = ContextPressureMonitor::default();
        assert_eq!(m.warned_tier, 0.0);
        assert!(m.last_warn_at.is_none());
        assert_eq!(m.last_warn_percent, 0.0);
    }

    #[test]
    pub(crate) fn cp_monitor_returns_false_when_tier_zero() {
        let mut m = ContextPressureMonitor::default();
        let emit = m.should_emit_warning(0.42, 0.0);
        assert!(!emit);
        // No state mutation when no emission.
        assert_eq!(m.warned_tier, 0.0);
        assert!(m.last_warn_at.is_none());
        assert_eq!(m.last_warn_percent, 0.0);
    }

    #[test]
    pub(crate) fn cp_monitor_emits_on_first_call_above_zero() {
        let mut m = ContextPressureMonitor::default();
        let emit = m.should_emit_warning(0.65, 0.65);
        assert!(emit);
        // Tier upgrade always emits and records tier + warn_at + percent.
        assert_eq!(m.warned_tier, 0.65);
        assert!(m.last_warn_at.is_some());
        assert_eq!(m.last_warn_percent, 65.0);
    }

    #[test]
    pub(crate) fn cp_monitor_blocks_within_cooldown_without_tier_upgrade() {
        let mut m = ContextPressureMonitor::default();
        // Seed: same tier, recent emit at 65%.
        m.warned_tier = 0.65;
        m.last_warn_at = Some(Instant::now());
        m.last_warn_percent = 65.0;
        // Same tier, <5% advance, immediately after prior emit → cooldown blocks.
        let emit = m.should_emit_warning(0.66, 0.65);
        assert!(!emit);
        // State unchanged.
        assert_eq!(m.warned_tier, 0.65);
        assert_eq!(m.last_warn_percent, 65.0);
    }

    #[test]
    pub(crate) fn cp_monitor_emits_on_tier_upgrade_even_within_cooldown() {
        let mut m = ContextPressureMonitor::default();
        // Seed: tier=0.65, just warned at 65%.
        m.warned_tier = 0.65;
        m.last_warn_at = Some(Instant::now());
        m.last_warn_percent = 65.0;
        // Tier upgrade overrides the cooldown.
        let emit = m.should_emit_warning(0.66, 0.85);
        assert!(emit);
        assert_eq!(m.warned_tier, 0.85);
        assert!(m.last_warn_at.is_some());
    }

    //── : RetryCounters unit tests ──
    // The 3 sibling retry counters on LoopState (invalid_tool_retries,
    // invalid_json_retries, truncated_tool_call_retries) collapsed into
    // a single RetryCounters struct. These tests pin the bump + reset
    // contract; the `< max` / `>= max` decision stays at the call site
    // because the action on max differs between the three.

    #[test]
    pub(crate) fn retry_counters_default_is_all_zero() {
        //the freshly-constructed RetryCounters must start at
        // zero for all three slots — otherwise a stale value would leak
        // into the first call inside run() / run_stream().
        let c = RetryCounters::default();
        assert_eq!(c.invalid_tool, 0);
        assert_eq!(c.invalid_json, 0);
        assert_eq!(c.truncated_tool, 0);
    }

    #[test]
    pub(crate) fn retry_counters_bump_returns_saturating_incremented_value() {
        // All three bump methods return the new value and saturate
        // instead of overflowing — required because a buggy provider
        // could otherwise pin a counter at u32::MAX forever.
        let mut c = RetryCounters::default();
        assert_eq!(c.bump_invalid_tool(), 1);
        assert_eq!(c.bump_invalid_tool(), 2);
        assert_eq!(c.bump_invalid_json(), 1);
        assert_eq!(c.bump_invalid_json(), 2);
        assert_eq!(c.bump_truncated_tool(), 1);
        // Saturating behavior: simulate pathological case.
        c.invalid_tool = u32::MAX;
        assert_eq!(c.bump_invalid_tool(), u32::MAX);
        // Other counters untouched by the saturating bump.
        assert_eq!(c.invalid_json, 2);
        assert_eq!(c.truncated_tool, 1);
    }

    #[test]
    pub(crate) fn retry_counters_reset_clears_only_the_targeted_slot() {
        // The three reset methods are independent — resetting one
        // counter must not touch the other two, otherwise a successful
        // turn (which resets invalid_tool + invalid_json) could
        // accidentally wipe the streaming-only truncated_tool counter.
        let mut c = RetryCounters::default();
        c.bump_invalid_tool();
        c.bump_invalid_tool();
        c.bump_invalid_json();
        c.bump_truncated_tool();
        c.reset_invalid_tool();
        assert_eq!(c.invalid_tool, 0);
        assert_eq!(c.invalid_json, 1);
        assert_eq!(c.truncated_tool, 1);
        c.reset_invalid_json();
        assert_eq!(c.invalid_tool, 0);
        assert_eq!(c.invalid_json, 0);
        assert_eq!(c.truncated_tool, 1);
        c.reset_truncated_tool();
        assert_eq!(c.truncated_tool, 0);
    }

    #[test]
    pub(crate) fn retry_counters_truncated_tool_bump_at_max_still_increments() {
        // The truncated_tool call site checks `< max` BEFORE bumping
        // (run_stream's "Truncated tool arguments — retrying" branch
        // only fires when under the cap). After the bump-method
        // refactor, the bump happens unconditionally and the call
        // site does the `<= max` check after. The end-state observable
        // behavior is the same: at the cap, the bump-then-check path
        // is a no-op for the emit, and the next reset() wipes the
        // counter. This test pins the bumped value so a future
        // refactor that introduces an early-return would be caught.
        let mut c = RetryCounters::default();
        c.truncated_tool = 2; // simulate "just under the cap of 3"
        assert_eq!(c.bump_truncated_tool(), 3); // bumps to 3
        assert_eq!(c.bump_truncated_tool(), 4); // would be 4 (over the cap)
        c.reset_truncated_tool();
        assert_eq!(c.truncated_tool, 0);
    }

    //── : CostGuard struct + evaluate_thresholds ──
    // The 4 cost-guard fields that used to be 4 sibling `&mut` args on
    // `process_post_response` now live on a single `CostGuard` struct.
    // `evaluate_thresholds` returns an enum (NoAction / Downgrade /
    // Warn / Trip) so the caller — not the helper — owns the side
    // effects (route build + system message).

    #[test]
    pub(crate) fn cost_guard_default_is_all_zeros_and_none() {
        let cg = CostGuard::default();
        assert!(cg.accumulated_usage.is_none());
        assert_eq!(cg.session_cost_usd, 0.0);
        assert!(!cg.cost_warned);
        //forced_runtime_route moved out
        // (route_state owns it now).
    }

    #[test]
    pub(crate) fn cost_guard_record_usage_merges_into_accumulated_and_adds_cost() {
        let mut cg = CostGuard::default();
        // First usage: 10 prompt + 20 completion → starts a new total.
        cg.record_usage(
            Some(&UsageStats {
                prompt_tokens: 10,
                completion_tokens: 20,
                total_tokens: 30,
                estimated_cost: Some(0.001),
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
                reasoning_tokens: 0,
            }),
            "test-model",
            &AgentConfig::default(),
        );
        assert!(cg.accumulated_usage.is_some());
        assert!(cg.session_cost_usd >= 0.0);
    }

    #[test]
    pub(crate) fn cost_guard_record_usage_no_op_when_turn_usage_is_none() {
        let mut cg = CostGuard::default();
        cg.record_usage(None, "test-model", &AgentConfig::default());
        assert!(cg.accumulated_usage.is_none());
        assert_eq!(cg.session_cost_usd, 0.0);
    }

    #[test]
    pub(crate) fn cost_guard_evaluate_returns_no_action_when_no_limit() {
        let mut cg = CostGuard::default();
        cg.session_cost_usd = 999.0;
        let rs = RouteState::default();
        let action = cg.evaluate_thresholds(&rs, None, 0.5, || None);
        assert_eq!(action, CostGuardAction { warn: None, trip: None });
        assert!(!cg.cost_warned);
    }

    #[test]
    pub(crate) fn cost_guard_evaluate_returns_downgrade_when_warn_threshold_first_crossed() {
        let mut cg = CostGuard::default();
        cg.session_cost_usd = 0.06; // above 0.1 * 0.5
        let rs = RouteState::default();
        let action = cg.evaluate_thresholds(&rs, Some(0.1), 0.5, || Some("openai:gpt-4o-mini".into()));
        match action.warn {
            Some(WarnEffect { kind: WarnKind::Downgrade { model }, .. }) => {
                assert_eq!(model, "openai:gpt-4o-mini");
            }
            other => panic!("expected Downgrade warn effect, got {:?}", other),
        }
        assert!(action.trip.is_none());
        assert!(cg.cost_warned);
        // forced_runtime_route is NOT set by evaluate_thresholds —
        // the caller must call `RouteState::install_downgrade` with
        // the built `TurnRuntimeRoute`. This is the whole reason the
        // helper doesn't construct the route itself.
        assert!(rs.forced_runtime_route.is_none());
    }

    #[test]
    pub(crate) fn cost_guard_evaluate_returns_warn_when_degrade_model_unresolvable() {
        let mut cg = CostGuard::default();
        cg.session_cost_usd = 0.06;
        let rs = RouteState::default();
        let action = cg.evaluate_thresholds(&rs, Some(0.1), 0.5, || None);
        assert!(matches!(
            action.warn,
            Some(WarnEffect { kind: WarnKind::Warn, .. })
        ));
        assert!(action.trip.is_none());
        assert!(cg.cost_warned);
    }

    #[test]
    pub(crate) fn cost_guard_evaluate_is_idempotent_after_first_warn() {
        let mut cg = CostGuard::default();
        cg.session_cost_usd = 0.06;
        let mut rs = RouteState::default();
        // First crossing fires the downgrade side effect.
        let first = cg.evaluate_thresholds(&rs, Some(0.1), 0.5, || Some("cheap".into()));
        assert!(matches!(
            first.warn,
            Some(WarnEffect { kind: WarnKind::Downgrade { .. }, .. })
        ));
        // Caller installs the route (mirrors what process_post_response
        // does in production). We only need *some* route to verify the
        // forced_runtime_route field gets populated; a partial struct
        // literal is fine.
        rs.install_downgrade(TurnRuntimeRoute {
            model: "cheap".into(),
            provider: None,
            base_url: None,
            api_key_env: None,
            api_mode: None,
            command: None,
            args: Vec::new(),
            credential_pool: None,
            credential_pool_fallback: true,
            route_label: None,
            routing_reason: Some("cost_guard".into()),
            signature: TurnRouteSignature {
                model: "cheap".into(),
                provider: None,
                base_url: None,
                api_mode: ApiMode::ChatCompletions,
                command: None,
                args: Vec::new(),
            },
        });
        assert!(rs.forced_runtime_route.is_some());
        // Second evaluation: already past warn, no degrade, not yet
        // over the absolute limit → no warn + no trip.
        let second = cg.evaluate_thresholds(&rs, Some(0.1), 0.5, || Some("another".into()));
        assert_eq!(second, CostGuardAction { warn: None, trip: None });
    }

    #[test]
    pub(crate) fn cost_guard_evaluate_returns_trip_when_spend_exceeds_limit() {
        let mut cg = CostGuard::default();
        cg.session_cost_usd = 0.5;
        let rs = RouteState::default();
        let action = cg.evaluate_thresholds(&rs, Some(0.1), 0.5, || Some("cheap".into()));
        let trip = action
            .trip
            .expect("expected trip effect, got None");
        assert!((trip.session_cost_usd - 0.5).abs() < 1e-9);
        assert!((trip.limit - 0.1).abs() < 1e-9);
        // The trip path also flipped cost_warned on the way through
        // (since 0.5 > 0.1 * 0.5 = 0.05). Both warn AND trip fired —
        // caller fires both side effects in order.
        assert!(cg.cost_warned);
        assert!(matches!(
            action.warn,
            Some(WarnEffect { kind: WarnKind::Downgrade { .. }, .. })
        ));
    }

    //── .3a : process_post_response unit tests ──
    // The shared post-LLM helper extracted in 1.3a replaces ~200 lines
    // of duplication between `run()` and `run_stream()`. These tests
    // exercise the two return paths (Finalize / Continue) and the
    // usage-accumulation side effect.

    #[test]
    pub(crate) fn p1_3a_process_post_response_finalize_on_no_tool_calls() {
        // No tool calls in response → Finalize. The helper should also
        // copy the assistant visible text into `last_content_with_tools`
        // for ack-continuation tracking.
        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                // This stub should never be called — these tests construct
                // an LlmResponse directly and pass it to
                // \. If that ever changes, surface
                // a clear Err instead of panicking.
                Err(AgentError::LlmApi("test stub: should not be called".into()))
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let mut ctx = ContextManager::for_model(&agent.config.model);
        let response = hermix_core::LlmResponse {
            message: Message::assistant("final answer"),
            usage: None,
            model: "test".into(),
            finish_reason: Some("stop".into()),
        };
        let mut cost_guard = CostGuard::default();
        //: `&mut Option<String>` arg became
        // `&mut MessageCache`; the old `last_content` local is gone,
        // we exercise the same final-answer path through
        // `message_cache.last_content_with_tools`.
        //added `&mut RouteState` — the
        // forced-route state used to live on `CostGuard`.
        let mut route_state = RouteState::default();
        let mut message_cache = MessageCache::default();
        let action = agent
            .process_post_response(
                &mut ctx,
                &response,
                None,
                0, // total_turns
                0, // api_elapsed
                &mut cost_guard,
                &mut route_state,
                &mut message_cache,
                false, // stream_mode
            )
            .unwrap();
        assert!(matches!(action, PostResponseAction::Finalize));
        assert_eq!(message_cache.last_content_with_tools.as_deref(), Some("final answer"));
    }

    #[test]
    pub(crate) fn p1_3a_process_post_response_cost_guard_trips_when_limit_exceeded() {
        // `session_cost_usd` already over `max_cost_usd` → CostGuardTripped
        // (caller should return finalize with finished_naturally=false).
        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                // This stub should never be called — these tests construct
                // an LlmResponse directly and pass it to
                // \. If that ever changes, surface
                // a clear Err instead of panicking.
                Err(AgentError::LlmApi("test stub: should not be called".into()))
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }
        let mut cfg = AgentConfig::default();
        cfg.max_cost_usd = Some(0.01);
        let agent = AgentLoop::new(
            cfg,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let mut ctx = ContextManager::for_model(&agent.config.model);
        let response = hermix_core::LlmResponse {
            message: Message::assistant("anything"),
            usage: None,
            model: "test".into(),
            finish_reason: Some("stop".into()),
        };
        let mut cost_guard = CostGuard::default();
        cost_guard.session_cost_usd = 0.5; // already 50x over the 0.01 limit
        let mut route_state = RouteState::default();
        let mut message_cache = MessageCache::default();
        let action = agent
            .process_post_response(
                &mut ctx,
                &response,
                None,
                0,
                0,
                &mut cost_guard,
                &mut route_state,
                &mut message_cache,
                false,
            )
            .unwrap();
        assert_eq!(action, PostResponseAction::CostGuardTripped);
    }

    #[test]
    pub(crate) fn p1_3a_process_post_response_continue_on_tool_calls() {
        // Tool calls in response → Continue. The helper should leave
        // `last_content_with_tools` untouched (caller handles that).
        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                // This stub should never be called — these tests construct
                // an LlmResponse directly and pass it to
                // \. If that ever changes, surface
                // a clear Err instead of panicking.
                Err(AgentError::LlmApi("test stub: should not be called".into()))
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let mut ctx = ContextManager::for_model(&agent.config.model);
        let mut msg = Message::assistant("calling tool");
        msg.tool_calls = Some(vec![ToolCall {
            id: "tc-1".into(),
            function: hermix_core::FunctionCall {
                name: "echo".into(),
                arguments: "{}".into(),
            },
        }]);
        let response = hermix_core::LlmResponse {
            message: msg,
            usage: None,
            model: "test".into(),
            finish_reason: Some("tool_calls".into()),
        };
        let mut cost_guard = CostGuard::default();
        let mut route_state = RouteState::default();
        let mut message_cache = MessageCache::default();
        let action = agent
            .process_post_response(
                &mut ctx,
                &response,
                None,
                0,
                0, // api_elapsed
                &mut cost_guard,
                &mut route_state,
                &mut message_cache,
                false,
            )
            .unwrap();
        assert!(matches!(action, PostResponseAction::Continue));
        // last_content_with_tools untouched (set later by the loop after
        // the assistant message is appended to ctx).
        assert!(message_cache.last_content_with_tools.is_none());
    }

    #[tokio::test]
    pub(crate) async fn resolve_pre_dispatch_allow_when_hook_unset() {
        // No hook installed → resolve_pre_dispatch returns an empty map and
        // never blocks.
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(stub_provider::EmptyProvider),
        );
        let tcs = vec![ToolCall {
            id: "tc1".into(),
            function: hermix_core::FunctionCall {
                name: "read_file".into(),
                arguments: "{}".into(),
            },
        }];
        let decisions = agent.resolve_pre_dispatch(&tcs).await;
        assert!(decisions.is_empty());
    }

    #[tokio::test]
    pub(crate) async fn resolve_pre_dispatch_routes_through_hook() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let callbacks = AgentCallbacks {
            on_pre_tool_dispatch: Some(Box::new(move |tc: &ToolCall| {
                calls_clone.fetch_add(1, Ordering::SeqCst);
                if tc.function.name == "dangerous_tool" {
                    ToolDispatchDecision::Deny {
                        reason: "blocked by test".to_string(),
                    }
                } else {
                    ToolDispatchDecision::Allow
                }
            })),
            ..Default::default()
        };
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(stub_provider::EmptyProvider),
        )
        .with_callbacks(callbacks);
        let tcs = vec![
            ToolCall {
                id: "a".into(),
                function: hermix_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: "{}".into(),
                },
            },
            ToolCall {
                id: "b".into(),
                function: hermix_core::FunctionCall {
                    name: "dangerous_tool".into(),
                    arguments: "{}".into(),
                },
            },
        ];
        let decisions = agent.resolve_pre_dispatch(&tcs).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(decisions.get("a").unwrap().is_ok());
        let b_err = decisions.get("b").unwrap().as_ref().err().unwrap();
        assert_eq!(
            b_err,
            &crate::tool_dispatch::ToolDenial {
                kind: crate::tool_dispatch::DenialKind::Policy,
                reason: "blocked by test".to_string(),
            }
        );
    }

    #[tokio::test]
    pub(crate) async fn resolve_pre_dispatch_await_approval_blocks_then_resolves() {
        let callbacks = AgentCallbacks {
            on_pre_tool_dispatch: Some(Box::new(|_tc: &ToolCall| {
                let (_tx, rx) =
                    tokio::sync::oneshot::channel::<hermix_core::ApprovalOutcome>();
                ToolDispatchDecision::AwaitApproval(rx)
            })),
            ..Default::default()
        };
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(stub_provider::EmptyProvider),
        )
        .with_callbacks(callbacks);
        let tcs = vec![ToolCall {
            id: "x".into(),
            function: hermix_core::FunctionCall {
                name: "x".into(),
                arguments: "{}".into(),
            },
        }];
        // Drop the spawned sender right away → receiver yields Err, hook
        // converts that into a ChannelClosed denial.
        let decisions = agent.resolve_pre_dispatch(&tcs).await;
        let err = decisions.get("x").unwrap().as_ref().err().unwrap();
        assert!(
            err.reason.contains("closed"),
            "unexpected err: {}",
            err.reason
        );
        assert_eq!(err.kind, crate::tool_dispatch::DenialKind::ChannelClosed);
    }

    // -----------------------------------------------------------------------
    //push/pop pair — `note_delegation_start` is wired into the
    // tool-dispatch paths and `note_delegation_end` matches by empty id
    // (topmost Delegation). Verify the pair generates exactly one
    // FocusConclusion per delegation.
    // -----------------------------------------------------------------------

    #[test]
    pub(crate) fn m162_delegation_push_pop_pair_emits_one_conclusion() {
        use hermix_focus::{FocusStack, SessionFrame, SessionFrameStatus};

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(stub_provider::EmptyProvider),
        )
        .with_focus_stack(Arc::new(Mutex::new(FocusStack::<SessionFrame>::new())));

        // session root
        agent.note_session_started("parent turn");
        // push with empty sub_agent_id (matches dispatch path behaviour
        // — the real sub_agent_id is parsed later from tool result content)
        let fid = agent
            .note_delegation_start("sub-task A", "")
            .expect("push succeeds");
        assert!(fid.is_nil() == false, "frame id is non-nil");

        // Stack depth should be 2 (root + delegation).
        let stack_depth = agent
            .focus_stack
            .lock()
            .expect("lock")
            .frames()
            .len();
        assert_eq!(stack_depth, 2, "expected root + delegation");

        // Pop with empty id → matches topmost Delegation.
        let popped_depth = agent
            .note_delegation_end("", SessionFrameStatus::Completed)
            .expect("pop succeeds");
        // `note_delegation_end` returns the frame's *payload* depth
        // field (delegate_depth counter, not stack depth). The default
        // agent has delegate_depth=0, so the popped frame's depth is 0.
        // Stack depth assertion happens below.
        assert_eq!(popped_depth, 0, "popped frame's depth field");

        // Stack should now have 1 frame (just root).
        let stack_depth_after = agent
            .focus_stack
            .lock()
            .expect("lock")
            .frames()
            .len();
        assert_eq!(stack_depth_after, 1);

        // Drain conclusions — expect exactly one.
        // (drain_focus_conclusions_to_memory_router returns 0 when
        // memory_router is None, so check the focus stack directly.)
        let drained = agent
            .focus_stack
            .lock()
            .expect("lock")
            .drain_conclusions()
            .len();
        assert_eq!(drained, 1, "push/pop pair should emit exactly one conclusion");
    }

    #[test]
    pub(crate) fn m162_push_idempotent_when_session_root_already_present() {
        use hermix_focus::{FocusStack, SessionFrame};

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(stub_provider::EmptyProvider),
        )
        .with_focus_stack(Arc::new(Mutex::new(FocusStack::<SessionFrame>::new())));

        // Calling note_session_started twice should be idempotent (no duplicate root).
        agent.note_session_started("first");
        agent.note_session_started("second");
        let depth = agent
            .focus_stack
            .lock()
            .expect("lock")
            .frames()
            .len();
        assert_eq!(depth, 1, "idempotent — only one root frame");
    }

    #[tokio::test]
    pub(crate) async fn drain_focus_conclusions_persists_to_memory_router() {
        use hermix_focus::{EmbeddedMemoryBackend, FocusStack, MemoryRouter, SessionFrame};
        use std::sync::Mutex;

        // Create a temporary embedded backend
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("memory.db");
        let embedded = Arc::new(
            EmbeddedMemoryBackend::open(&db_path).expect("open embedded backend"),
        );
        let router = Arc::new(MemoryRouter::new_with_embedded(
            vec![embedded.clone()],
            embedded.clone(),
        ));

        // Create agent with focus stack and memory router
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(stub_provider::EmptyProvider),
        )
        .with_focus_stack(Arc::new(Mutex::new(FocusStack::<SessionFrame>::new())))
        .with_memory_router(router);

        // Push a delegation frame and pop it to generate a conclusion
        agent.note_session_started("test-session");
        agent.note_delegation_start("test task summary", "sub-agent-1");
        agent.note_delegation_end("sub-agent-1", hermix_focus::SessionFrameStatus::Completed);

        // Drain conclusions
        let count = agent.drain_focus_conclusions_to_memory_router();
        assert!(count >= 1, "should drain at least one conclusion");

        // Wait for async write
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Verify the conclusion was persisted - search for conclusion tag
        let query = hermix_focus::MemoryQuery::recall("conclusion");
        let (hits, _) = embedded.recall(&query).await.unwrap();
        assert!(!hits.is_empty(), "conclusion should be retrievable from embedded backend");
    }

    //ACUI emit tests ---------------------------------------------

    /// Helper: build a minimal agent with no callbacks wired.
    pub(crate) fn bare_agent() -> AgentLoop {
        AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(stub_provider::EmptyProvider),
        )
    }

    #[test]
    pub(crate) fn m5_emit_acui_no_callback_is_noop() {
        // Without `on_acui_directive` wired, emit_acui should silently drop.
        let agent = bare_agent();
        agent.emit_acui_show("c1", "weather", serde_json::json!({"city": "Beijing"}));
        agent.emit_acui_update("c1", serde_json::json!({"temp": 23}));
        agent.emit_acui_hide("c1");
        // No panic, no return value to assert.
    }

    #[test]
    pub(crate) fn m5_emit_acui_show_invokes_callback_with_json() {
        use std::sync::Mutex as StdMutex;
        let captured: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let cap_clone = captured.clone();

        let mut agent = bare_agent();
        let cb: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |payload: &str| {
            cap_clone.lock().unwrap().push(payload.to_string());
        });
        // We own the only Arc to `agent.callbacks` here, so get_mut
        // succeeds and we can install the callback directly.
        Arc::get_mut(&mut agent.callbacks)
            .expect("callbacks Arc should be uniquely owned in test")
            .on_acui_directive = Some(cb);

        agent.emit_acui_show("c1", "weather", serde_json::json!({"city": "Beijing"}));
        let got = captured.lock().unwrap();
        assert_eq!(got.len(), 1);
        let v: serde_json::Value = serde_json::from_str(&got[0]).unwrap();
        assert_eq!(v["op"], "show");
        assert_eq!(v["type"], "weather");
        assert_eq!(v["id"], "c1");
        assert_eq!(v["props"]["city"], "Beijing");
        assert_eq!(v["position"], "after");
    }

    #[test]
    pub(crate) fn m5_emit_acui_update_serializes_id_and_patch() {
        use std::sync::Mutex as StdMutex;
        let captured: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let cap_clone = captured.clone();

        let mut agent = bare_agent();
        let cb: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |payload: &str| {
            cap_clone.lock().unwrap().push(payload.to_string());
        });
        Arc::get_mut(&mut agent.callbacks)
            .expect("callbacks Arc should be uniquely owned in test")
            .on_acui_directive = Some(cb);

        agent.emit_acui_update("card-7", serde_json::json!({"step": 5}));
        let got = captured.lock().unwrap();
        let v: serde_json::Value = serde_json::from_str(&got[0]).unwrap();
        assert_eq!(v["op"], "update");
        assert_eq!(v["id"], "card-7");
        assert_eq!(v["patch"]["step"], 5);
    }

    #[test]
    pub(crate) fn m5_emit_acui_hide_serializes_id() {
        use std::sync::Mutex as StdMutex;
        let captured: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let cap_clone = captured.clone();

        let mut agent = bare_agent();
        let cb: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |payload: &str| {
            cap_clone.lock().unwrap().push(payload.to_string());
        });
        Arc::get_mut(&mut agent.callbacks)
            .expect("callbacks Arc should be uniquely owned in test")
            .on_acui_directive = Some(cb);

        agent.emit_acui_hide("c1");
        let got = captured.lock().unwrap();
        let v: serde_json::Value = serde_json::from_str(&got[0]).unwrap();
        assert_eq!(v["op"], "hide");
        assert_eq!(v["id"], "c1");
    }

    /// WW regression: `PostResponseAction::Finalize` (the
    /// visible-text branch) used to call `persist_session_terminal(&ctx, …)`
    /// and `finalize_agent_result(&ctx, …)` **without** first appending the
    /// final assistant message into `ctx`. The `Continue` branch had a
    /// `prepare_assistant_turn` fallback path; `Finalize` did not. The
    /// symptom in production: owner reloaded the
    /// "看下目录下的文件" session and the post-tool final reply
    /// ("用户，目录共 198 条...") was gone — only user + assistant1(tool_call)
    /// + tool landed in the DB. Streaming had shown the text live via WS
    /// chunks but the server-side ctx / persist call had skipped it.
    /// The fix in `run_loop.rs`: call `prepare_assistant_turn` inside the
    /// `Finalize → has_visible_text` branch **before** the terminal
    /// persist / finalize. This regression test simulates exactly that
    /// two-turn shape and asserts the final assistant message survives
    /// in the returned `AgentResult.messages`.
    #[tokio::test]
    pub(crate) async fn ww_run_persists_final_assistant_message_into_ctx_after_tool() {
        use async_trait::async_trait;
        use futures::stream::BoxStream;
        use hermix_core::{
            tool_schema, AgentError, FunctionCall, JsonSchema, LlmProvider, LlmResponse, Message,
            ToolCall, ToolError, ToolSchema,
        };
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct TwoTurnProvider {
            calls: AtomicUsize,
        }
        #[async_trait]
        impl LlmProvider for TwoTurnProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<LlmResponse, AgentError> {
                let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
                let message = if n == 1 {
                    // Turn 1: empty visible content + a tool_call to a
                    // registered "list_files" tool. Mirrors how
                    // MiniMax emits a thinking-only turn followed by a
                    // tool call.
                    let mut msg = Message::assistant("");
                    msg.tool_calls = Some(vec![ToolCall {
                        id: "call-1".into(),
                        function: FunctionCall {
                            name: "list_files".into(),
                            arguments: "{}".into(),
                        },
                    }]);
                    msg
                } else {
                    // Turn 2: full visible final response (the text that
                    // the owner saw live in the chat but that *used to*
                    // not be persisted into ctx on the Finalize branch).
                    Message::assistant(
                        "用户，目录共 198 条(含 3 个 .py 脚本 + 17 个主车文件夹)。",
                    )
                };
                Ok(LlmResponse {
                    message,
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        // `agent_helpers::ToolRegistry::register` takes a sync
        // `Fn(Value) -> Result<String, ToolError>` — so the mock tool is
        // a plain closure rather than a `ToolHandler` impl.
        let mut registry = ToolRegistry::new();
        registry.register(
            "list_files",
            tool_schema("list_files", "List files", JsonSchema::new("object")),
            Arc::new(|_params: serde_json::Value| -> Result<String, ToolError> {
                Ok("mock directory listing (198 entries)".into())
            }),
        );

        let provider = Arc::new(TwoTurnProvider {
            calls: AtomicUsize::new(0),
        });

        let cfg = AgentConfig {
            max_turns: 4,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(registry), provider);

        let result = agent
            .run(vec![Message::user("看下目录下的文件")], None)
            .await
            .expect("agent run should succeed");

        // The core WW invariant: after the post-tool LLM turn
        // produces a visible final reply, that reply MUST be in
        // `result.messages` (and therefore in the persisted DB /
        // disk `.md`). Before the fix the ctx used to terminate
        // *before* `prepare_assistant_turn`, so the final assistant
        // bubble was lost on persist and reload came back truncated.
        // We don't pin the total message count — the system prompt
        // injection and cost-guard may insert extra System rows —
        // but we assert the shape that survives:
        // * at least one Tool role (the list_files result)
        // * the LAST Assistant row is the post-tool final reply
        // (i.e. it was appended AFTER the tool result, in the
        // Finalize→has_visible_text branch we just fixed)
        // * an earlier Assistant row carried the tool_call so
        // the chat view renders the tool pill correctly
        use hermix_core::MessageRole;
        let roles: Vec<_> = result.messages.iter().map(|m| m.role).collect();
        assert!(
            roles.iter().any(|r| *r == MessageRole::Tool),
            "expected at least one Tool message; got roles={:?}",
            roles
        );

        let last_assistant = result
            .messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::Assistant)
            .expect("expected a final Assistant message — the regression");
        let final_content = last_assistant.content.as_deref().unwrap_or("");
        assert!(
            final_content.contains("用户，目录共 198 条"),
            "final assistant message must contain the post-tool reply; got: {:?}",
            final_content
        );

        // The FINAL assistant must come AFTER the tool result in the
        // message list — otherwise the assistant message would have
        // been added before the tool result (the Continue-branch
        // fallback path), which doesn't actually exercise the
        // Finalize→has_visible_text fix.
        let last_tool_idx = result
            .messages
            .iter()
            .rposition(|m| m.role == MessageRole::Tool)
            .expect("at least one Tool message");
        let last_assistant_idx = result
            .messages
            .iter()
            .rposition(|m| m.role == MessageRole::Assistant)
            .expect("at least one Assistant message");
        assert!(
            last_assistant_idx > last_tool_idx,
            "final assistant (idx={}) must come after the last tool result (idx={}); roles={:?}",
            last_assistant_idx,
            last_tool_idx,
            roles
        );

        // Sanity: an earlier Assistant row must carry the tool_call so
        // the merged `tool_calls` field of the chat view is correct.
        let pre_tool_assistant = result
            .messages
            .iter()
            .take(last_tool_idx)
            .find(|m| m.role == MessageRole::Assistant)
            .expect("expected an assistant message before the tool result");
        let tcs = pre_tool_assistant
            .tool_calls
            .as_ref()
            .expect("pre-tool assistant must carry tool_calls");
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].function.name, "list_files");
}
