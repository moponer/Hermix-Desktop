//! Pre-turn setup + per-turn prefetch (O1.1)
//!
//! Moved verbatim from `run_loop.rs` (pure relocation, no behavior
//! change). Methods are `pub(super)` because their callers live in the
//! parent `run_loop` module. All names resolve via the parent glob.

#[allow(unused_imports)]
use super::*;
use crate::agent_loop::extract_last_user_assistant;
use crate::context::ContextManager;
use hermix_core::ToolErrorRecord;
use serde_json::json;

impl AgentLoop {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn do_pre_turn_setup(
        &self,
        ctx: &mut ContextManager,
        state: &mut LoopState,
        session_id: &str,
        tool_errors: &[ToolErrorRecord],
        session_started_hooks_fired: bool,
        persist_user_idx: Option<usize>,
        _finish_reason: Option<&str>,
    ) -> Result<PreTurnAction, AgentError> {
        // (type-B diagnostic): INFO log every pre-turn setup
        // entry so a future hang between turns shows up in stdout.
        // Without this, the only signal between `PostToolExec` and the
        // `PreTurnSetup` phase is a `tracing::debug!` that the default
        // log level never emits — and the user sees an unexplained
        // "● ● ● 正在思考" forever. Pair this with the matching
        // "post-tool finished" INFO at the end of
        // `do_post_tool_iteration` and you can pinpoint the hang to
        // exactly one frame.
        tracing::info!(
            turn    = state.total_turns + 1,
            session = session_id,
            "do_pre_turn_setup: entering"
        );

        // --- Graceful interrupt ---
        if self.interrupt.take_interrupt_graceful().is_some() {
            return Ok(PreTurnAction::Stop(self.graceful_interrupt_result(
                ctx,
                state.total_turns,
                tool_errors,
                state.cost_guard.accumulated_usage.clone(),
                state.cost_guard.session_cost_usd,
                session_started_hooks_fired,
                persist_user_idx,
            )));
        }

        // poll any background-spawned compression ---
        // Non-blocking probe: if a previous spawn finished while the
        // agent loop was processing the last turn, roll the result
        // into `ctx` *before* anything else reads `ctx` (memory
        // recall, skill injection, LLM call). This is the only point
        // in the loop where the in-flight handle is reliably polled
        // every turn.
        let _ = self.poll_in_flight_compression(ctx, false).await;

        // --- `/steer` pre-API drain ---
        self.pre_api_apply_pending_steer(ctx);

        // --- Max turns: soft cap ---
        // max_turns == 0 means unlimited. The real hard stops are:
        // ErrorLoop (convergence detector), Cost Guard (max_cost_usd),
        // and the LLM's natural stopping (no tool calls → finalize).
        // max_turns is NOT a hard stop — hitting it triggers a progress
        // summary (using the conversation's active model) and resets the
        // turn counter so long tasks keep running. Without this, every
        // task longer than max_turns turns is silently truncated.

        // --- Turn counter ---
        state.total_turns += 1;
        tracing::debug!("Agent turn {}", state.total_turns);

        // --- OAuth refresh ---
        self.oauth_refresh
            .refresh_if_needed(&self.auth_tokens_path(), &self.config.runtime_providers)
            .await;

        // --- Skill nudge counter ---
        if self.config.skill_creation_nudge_interval > 0
            && self
                .tool_registry
                .names()
                .iter()
                .any(|n| n == "skill_manage")
        {
            if let Ok(mut c) = self.evolution_counters.lock() {
                c.iters_since_skill = c.iters_since_skill.saturating_add(1);
            }
        }

        // --- Checkpoint ---
        if self.config.checkpoint_interval_turns > 0
            && (state.total_turns - 1).is_multiple_of(self.config.checkpoint_interval_turns)
        {
            state.message_cache.store_checkpoint(ctx.get_messages().to_vec());
        }

        // --- Memory turn-start notification ---
        self.memory_on_turn_start(state.total_turns, "");

        // Removed Memory review hint (every 20 turns).
        // The system prompt already contains memory management guidance.
        // Injecting "review and consolidate" hints pollutes the context
        // and distracts the LLM from the user's actual task.

        // --- Memory sync at flush interval ---
        if state.total_turns.is_multiple_of(self.config.memory_flush_interval) && state.total_turns > 0 {
            let msgs = ctx.get_messages();
            let (u, a) = extract_last_user_assistant(msgs);
            self.memory_sync(&u, &a, session_id);
        }

        // --- Background snapshot + frame cap ---
        self.maybe_freeze_snapshot(session_id, state.total_turns);
        self.maybe_enforce_frame_cap(state.total_turns);

        //(A): per-turn memory recall ---
        // Cheap heuristic: every Nth turn (default 5) OR when the
        // user turn's first token differs from the previous recall,
        // we re-pull the top-N salient facts and emit a
        // `memory_recall` status event. The drift check uses the
        // first meaningful token of the user text (no embedding).
        let user_text: String = ctx
            .get_messages()
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::User)
            .and_then(|m| m.content.as_deref())
            .unwrap_or("")
            .to_string();
        if let Some(plan) = self.maybe_recall(state, &user_text) {
            // Track keys for next-turn drift detection.
            state.last_recall_keys =
                plan.items.iter().map(|i| i.canonical_key.clone()).collect();
            // Inject recall block into ctx so the LLM can actually
            // use it. Previously this was emit-only (user could see
            // "thought of N items" but the agent couldn't). Now the
            // rendered block lands in message history as a system
            // message, same pattern as `memory_prefetch` at session
            // start. Compression will fold old recall blocks when
            // the context grows past the threshold.
            let recall_block = crate::recall_scheduler::render_prompt_block(&plan);
            if !recall_block.is_empty() {
                ctx.add_message(Message::system(&recall_block));
            }
        }

        //跨会话教训注入（Harness² 的 harnessing 级递归，读取侧）。
        //
        //`harness_self_edit` 的写入链与 `experience_archive` 的读取链都早已
        //写好，但**两端都没有生产调用点**——`classify_convergence` 的注释
        //自己写着 "once the harness self-edit integration is complete"。写入
        //侧在 `finalize_agent_result`（`maybe_record_experience`），这里补
        //读取侧：把"与本轮工具相关、且上次真的重复失败过"的教训提前摆到
        //模型面前，避免同一个坑掉两次。
        //
        //预算 1200 字符（`experience_prompt_for_tools` 内部还会按 top-5 截断），
        //因为这是"提醒"不是"教案"——它排在 skills 正文之前，不抢主任务的
        //注意力。
        {
            let exp_root = hermix_config::paths::hermix_home().join("experience");
            if exp_root.is_dir() {
                if let Ok(archive) =
                    crate::experience_archive::ExperienceArchive::open(&exp_root)
                {
                    let tool_names: Vec<String> =
                        self.tool_registry.names().into_iter().collect();
                    match crate::experience_archive::experience_prompt_for_tools(
                        &archive,
                        &tool_names,
                        1200,
                    ) {
                        Ok(Some(block)) => ctx.add_message(Message::system(&block)),
                        Ok(None) => {}
                        Err(e) => {
                            tracing::debug!(error = %e, "experience prompt skipped");
                        }
                    }
                }
            }
        }

        //: auto-activate matching skills ---
        // Cheap keyword + subsequence-fuzzy match the user's message
        // against the skill directory and inject up to 3 skills'
        // full bodies into ctx. The skill bodies are appended as a
        // system message so the LLM sees them alongside the recall
        // block. Compression folds them on context pressure.
        if !user_text.is_empty() {
            let skills_dir =
                hermix_config::paths::hermix_home().join("skills");
            match build_active_skills_block_with_tools(
                &skills_dir,
                &user_text,
                self.config.skill_injection_char_budget,
            ) {
                Some((trimmed, tools)) => {
                    ctx.add_message(Message::system(&trimmed));
                    // allowed-tools enforcement (subtract-only): the tools
                    // union of the activated skills narrows this turn's
                    // dispatch surface. Recomputed every turn so a stale
                    // restriction can never leak into the next one.
                    state.active_skill_tools = tools;
                }
                None => state.active_skill_tools = None,
            }
        } else {
            state.active_skill_tools = None;
        }

        // Plan mode reminder: one short system note per turn while the mode
        // is on, skipped when a recent message already carries it (so
        // repeated turns don't stack duplicates before compression).
        if self.tool_registry.plan_mode_active() {
            let already_present = ctx
                .get_messages()
                .iter()
                .rev()
                .take(8)
                .any(|m| m.content.as_deref().unwrap_or("").contains("[PLAN MODE]"));
            if !already_present {
                ctx.add_message(Message::system(
                    "[PLAN MODE] 当前处于计划模式（只读调研）：写文件、执行命令、自动化类工具会被拒绝。\
                     先完成调研并给出清晰计划，用户确认后调用 exit_plan_mode 恢复执行。",
                ));
            }
        }

        // --- Pre-LLM hook + route resolution ---
        let turn_runtime_route = state
            .route_state
            .forced_runtime_route
            .clone()
            .or_else(|| self.resolve_smart_runtime_route(ctx.get_messages()));
        let active_model = turn_runtime_route
            .as_ref()
            .map(|r| r.model.as_str())
            .unwrap_or(self.config.model.as_str())
            .to_string();
        let hook_ctx = serde_json::json!({"turn": state.total_turns, "model": active_model});

        // Soft turn cap: at the boundary, summarize progress with the
        // conversation's own model, inject the summary, reset the counter
        // and keep going. Long tasks are no longer truncated at
        // `max_turns`; CostGuard / ErrorLoop remain the hard stops.
        if self.config.max_turns > 0 && state.total_turns >= self.config.max_turns {
            tracing::warn!(
                "Max turns ({}) reached — summarizing progress and continuing",
                self.config.max_turns
            );
            self.emit_status(
                "lifecycle",
                &format!(
                    "已执行 {} 轮，正在整理进度并继续…",
                    self.config.max_turns
                ),
            );
            let summary_msg = self.handle_max_iterations(ctx, &active_model).await?;
            if let Some(msg) = summary_msg {
                ctx.add_message(msg);
            }
            state.total_turns = 0;
        }

        let pre_results = self.invoke_hook(HookType::PreLlmCall, &hook_ctx);
        self.inject_hook_context(&pre_results, ctx);

        Ok(PreTurnAction::Continue {
            turn_runtime_route,
            active_model,
        })
    }

    /// O1.1: shared prefetch block — was ~80 lines of duplication between
    /// run() and run_stream(). Identical logic: intent classification,
    /// safe tool dispatch, thinking mode selection, ACUI emission.
    pub(super) async fn do_prefetch(
        &self,
        ctx: &mut ContextManager,
        tool_schemas: &[ToolSchema],
        total_turns: u32,
    ) {
        let Some(ref prefetcher) = self.prefetcher else {
            return;
        };
        let tool_names: Vec<String> = tool_schemas.iter().map(|t| t.name.clone()).collect();
        let user_text: String = ctx
            .get_messages()
            .iter()
            .filter(|m| m.role == MessageRole::User)
            .filter_map(|m| m.content.as_deref())
            .collect::<Vec<_>>()
            .join("\n");
        let recent_msgs: Vec<Message> = ctx
            .get_messages()
            .iter()
            .cloned()
            .rev()
            .take(10)
            .collect();
        let tc = hermix_intelligence::TurnContext::from_user_message(user_text.clone())
            .with_recent_messages(recent_msgs);
        if let Some(ref outcome) = prefetcher.run(&tc, &tool_names).await {
            let block = outcome.to_system_prompt_block();
            if !block.is_empty() {
                ctx.add_message(Message::system(&block));
                tracing::trace!(intent = ?outcome.intent, confidence = outcome.confidence, "prefetch result injected as system prompt block");
            }
            if let Some(ref on_prefetch_intent) = self.callbacks.on_prefetch_intent {
                let json = serde_json::to_string(&json!({
                    "intent": serde_json::to_value(outcome.intent.clone()).unwrap_or_default(),
                    "confidence": outcome.confidence,
                    "results": outcome.results.iter().map(|r| json!({
                        "tool": &r.tool,
                        "args": &r.args,
                        "output": &r.output,
                        "errored": r.errored,
                        "duration_ms": r.duration_ms,
                    })).collect::<Value>(),
                }));
                if let Ok(ref payload) = json {
                    on_prefetch_intent(payload);
                }
            }
        }

        // L4: dual-layer thinking mode selection
        let thinking_decision = {
            use hermix_intelligence::{select_mode, ThinkingConfig};
            let cfg = ThinkingConfig::default();
            let history_len = ctx.get_messages().len();
            select_mode(user_text.as_str(), history_len, &cfg)
        };
        tracing::trace!(
            mode = thinking_decision.mode.as_str(),
            reason = thinking_decision.reason.as_str(),
            confidence = thinking_decision.confidence,
            "L4 thinking mode selected for this turn"
        );
        if let Some(ref on_acui) = self.callbacks.on_acui_directive {
            let directive = hermix_core::AcuiDirective::Show(
                hermix_core::AcuiShow::new(
                    format!("thinking-mode-{}", total_turns),
                    "thinking_mode",
                    serde_json::json!({
                        "mode": thinking_decision.mode.as_str(),
                        "reason": thinking_decision.reason.as_str(),
                        "confidence": thinking_decision.confidence,
                    }),
                ),
            );
            if let Ok(json) = serde_json::to_string(&directive) {
                on_acui(&json);
            }
        }
    }
}
