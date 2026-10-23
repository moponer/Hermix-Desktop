//! AgentLoop construction, builders, ACUI emit, tool routing, steer, session notes.
use std::sync::{Arc, Mutex};


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
        // Transition contract (C6): a phase that skips a stage is how a bad
        // branch turns into "the turn silently stopped". Warn, don't abort —
        // the loop must survive, but the trace names the illegal edge.
        let prev_index = self.last_loop_phase.swap(
            phase.as_index(),
            std::sync::atomic::Ordering::Relaxed,
        );
        if let Some(prev) = LoopPhase::from_index(prev_index) {
            if !prev.can_transition_to(phase) {
                tracing::warn!(
                    from = prev.as_str(),
                    to = phase_str,
                    turn = turn,
                    "illegal loop phase transition — a stage was skipped"
                );
            }
        }
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

    /// 把本次会话跑通的工具序列沉淀成 auto skill。
    ///
    /// ## 为什么加这一行
    ///
    /// `should_auto_create_skill` / `write_auto_skill`（`skill_auto_create.rs`）
    /// 早就写好，连 slug 清洗、不覆盖手动 skill、16KB 上限都有——但**从未被
    /// run loop 调用**。于是"总结经验为 skill"完全依赖模型自愿调
    /// `skill_manage auto_extract_from_conversation`，而它经常不调：用户明确
    /// 要求沉淀时，会话结束了，什么都没留下。
    ///
    /// ## 为什么用确定性模板而不是再问一次 LLM
    ///
    /// 收尾阶段多一次 round-trip 直接加到用户等待上。而"按顺序调了哪些工具、
    /// 关键参数是什么"本身就是可复用的操作手册——比一段 LLM 摘要更可靠
    /// （不会被幻觉污染），也更适合当 runbook。想要更丰富的说明，模型仍可
    /// 用 `skill_manage patch` 补写。
    ///
    /// ## 错误也是素材（Harness² 的教训分级）
    ///
    /// 门槛从「自然完成 **且** 全程无错」放宽为「自然完成」。理由：Harness²
    /// 的核心是"对比同一任务的不同执行来提炼教训"，而**错误-恢复对**正是最
    /// 有信息量的对比——顺利跑通只说明"这样做可以"，失败后绕通才说明"哪条
    /// 路不通"。旧门槛把这类会话整条丢掉，恰好丢掉最有价值的部分。
    /// 带错误的会话因此会多出一节「踩过的坑」，与「操作步骤」并列。
    ///
    /// 任何 IO 失败都只记 debug 日志——沉淀是赠品，不能让它弄坏会话。
    fn maybe_record_experience(&self) {
        let home = self.hermix_home_dir();

        // The counter lives on the loop (one lock acquisition, released before
        // any file IO so a slow disk cannot block the next turn's error
        // bookkeeping).
        let counter = match self.tool_error_counter.lock() {
            Ok(c) => c,
            Err(_) => return,
        };

        // `run_self_edit` is the module's **complete** entry point — tool
        // bursts, convergence and feedback in one pass, then experience +
        // rule proposal. Calling the three helpers by hand (as an earlier
        // revision of this method did) silently drops the feedback channel
        // and the rule proposal, which is exactly the "half-wired integration"
        // this change is closing.
        let Ok(feedback) =
            crate::feedback_loop::FeedbackLog::open_at(&home.join("feedback").join("agent.jsonl"))
        else {
            return;
        };
        let Ok(rules) =
            crate::self_improvement::SelfRuleStore::open_at(&home.join("self_rules").join("rules.jsonl"))
        else {
            return;
        };

        // `convergence: None` — a convergence stop is detected inside the loop,
        // not at finalize time. Wiring that path means passing the reason down;
        // until then convergence-triggered lessons are absent, which is a gap
        // recorded in the decision note rather than a silent no-op here.
        let result = crate::harness_self_edit::run_self_edit(
            &counter,
            &feedback,
            None,
            &home.join("experience"),
            &rules,
        );
        drop(counter);

        if result.experience_written {
            tracing::info!(
                experience_id = result.experience_id.as_deref().unwrap_or("?"),
                rule_proposed = result.rule_proposed,
                "self-edit: failure patterns recorded for future sessions"
            );
            self.emit_status(
                "lifecycle",
                "已把本轮的失败模式记入经验档案，下次开工会先提醒",
            );
        }
    }

    /// App home, honouring the per-loop override (tests point it at a tempdir).
    fn hermix_home_dir(&self) -> std::path::PathBuf {
        self.config
            .hermix_home
            .as_deref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(hermix_config::paths::hermix_home)
    }

    /// 从上下文里提取「任务原文 + 按序的工具调用」。
    ///
    /// assistant 消息的 `tool_calls` 是唯一同时带有名字与原始参数的位置；
    /// `execution_timeline` 只有预览。抽成独立函数是因为 auto skill 与
    /// Harness² 探针两条路径都要这份 trace，各写一遍必然漂移。
    fn collect_run_trace(ctx: &ContextManager) -> Option<(String, Vec<(String, String)>)> {
        let mut calls: Vec<(String, String)> = Vec::new();
        let mut task_hint = String::new();
        for msg in ctx.get_messages() {
            if msg.role == hermix_core::MessageRole::User && task_hint.is_empty() {
                task_hint = msg
                    .content
                    .as_deref()
                    .unwrap_or("")
                    .chars()
                    .take(160)
                    .collect();
            }
            if msg.role != hermix_core::MessageRole::Assistant {
                continue;
            }
            if let Some(tcs) = msg.tool_calls.as_ref() {
                for tc in tcs {
                    if !tc.function.name.is_empty() {
                        calls.push((tc.function.name.clone(), tc.function.arguments.clone()));
                    }
                }
            }
        }
        if calls.is_empty() {
            return None;
        }
        Some((task_hint, calls))
    }

    /// Harness² 任务级递归的唯一编排点：Judge → Probe → Compose。
    ///
    /// - **Judge**：用可核验信号判定本次执行（`harness_probe::judge`）。用户
    ///   重发同一请求是最强的失败证据，比 `finished_naturally` 更强。
    /// - **Probe**：把这次执行记进 `probes/<sig>.jsonl`。**不自动重放**同一
    ///   任务——它通常带副作用（写文件、提交表单），自动跑 K 次会真的执行
    ///   两次。这里把用户的真实重试当作天然 probe，跨会话累积。
    /// - **Compose**：同一签名有 ≥2 条记录时，对比成功与失败的行为差异，
    ///   提炼成技能并**覆盖**同名文件（同一任务的迭代版本，不是新技能）。
    ///
    /// 首次执行（只有 1 条记录）时什么都不写——骨架 skill 由
    /// `maybe_write_auto_skill` 负责，两者分工：一次执行给"步骤"，多次执行
    /// 给"哪条路通、哪条不通"。
    fn maybe_run_task_recursion(
        &self,
        ctx: &ContextManager,
        finished_naturally: bool,
        interrupted: bool,
        tool_errors: &[hermix_core::ToolErrorRecord],
        total_turns: u32,
    ) {
        let Some((task_hint, steps)) = Self::collect_run_trace(ctx) else {
            return;
        };
        let sig = crate::harness_probe::task_signature(&task_hint);
        let Ok(store) =
            crate::harness_probe::ProbeStore::open(self.hermix_home_dir().join("probes"))
        else {
            return;
        };

        // 上一次同任务是否失败过 —— 这就是 `judge` 里的"用户重发"信号。
        let repeated = matches!(
            store.last_outcome(&sig),
            Ok(Some(crate::harness_probe::TaskOutcome::Failed))
        );
        // 末轮是否仍有错：错误记录里出现过最后一轮的错误。
        let last_turn_had_errors = tool_errors.iter().any(|e| e.turn >= total_turns);

        let verdict = crate::harness_probe::judge(
            finished_naturally,
            interrupted,
            tool_errors,
            last_turn_had_errors,
            repeated,
        );

        let composed = match crate::harness_probe::run_task_recursion(
            &store, &sig, &verdict, steps, tool_errors, total_turns,
        ) {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(error = %e, "probe record failed (non-fatal)");
                return;
            }
        };

        let Some(composed) = composed else {
            return;
        };

        // 覆盖写：`probe-<sig>` 是**同一任务**的迭代产物，不适用
        // `write_auto_skill` 的 AlreadyExists 保护（那保护的是"别覆盖手动
        // 创建的技能"，而这里是覆盖自己上一版的提炼结果）。
        let dir = self
            .hermix_home_dir()
            .join("skills")
            .join(crate::skill_auto_create::AUTO_SKILL_DIR)
            .join(&composed.slug);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::debug!(error = %e, "composed skill dir failed (non-fatal)");
            return;
        }
        let md = crate::skill_auto_create::render_skill_md(&composed.slug, &composed.body);
        match std::fs::write(dir.join("SKILL.md"), md) {
            Ok(()) => {
                tracing::info!(
                    slug = %composed.slug,
                    outcome = verdict.outcome.as_str(),
                    "Harness²: probe recorded and skill composed from execution history"
                );
                self.emit_status(
                    "lifecycle",
                    "已按多次执行对比更新技能（有效路径 + 失败分歧点）",
                );
            }
            Err(e) => {
                tracing::debug!(error = %e, "composed skill write failed (non-fatal)");
            }
        }
    }

    fn maybe_write_auto_skill(
        &self,
        ctx: &ContextManager,
        success: bool,
        tool_errors: &[hermix_core::ToolErrorRecord],
    ) {
        if !success {
            return;
        }
        let Some((task_hint, calls)) = Self::collect_run_trace(ctx) else {
            return;
        };
        let names: Vec<String> = calls.iter().map(|(n, _)| n.clone()).collect();
        if !crate::skill_auto_create::should_auto_create_skill(&names) {
            return;
        }

        // slug 优先取任务原文；但 `skill_slug` 只保留 ASCII，中文任务名会被
        // 剥成空并退化成 "auto"——于是每个中文任务都叫 auto，第二个开始
        // `write_auto_skill` 以 AlreadyExists 静默跳过，用户永远只看到一个
        // 技能。退化时改用工具序列命名（ASCII、可区分、仍是操作手册的准确摘要）。
        let slug = {
            let from_task = crate::skill_auto_create::skill_slug(&task_hint);
            if from_task == "auto" {
                let mut distinct: Vec<&str> = Vec::new();
                for (name, _) in &calls {
                    if !distinct.contains(&name.as_str()) {
                        distinct.push(name.as_str());
                    }
                }
                crate::skill_auto_create::skill_slug(&distinct.join("-"))
            } else {
                from_task
            }
        };
        let body = render_auto_skill_body(&slug, &task_hint, &calls, tool_errors);
        let root = self.hermix_home_dir().join("skills");
        match crate::skill_auto_create::write_auto_skill(&root, &slug, &body) {
            Ok(path) => {
                tracing::info!(
                    slug = %slug,
                    steps = calls.len(),
                    path = %path.display(),
                    "auto skill written from a successful tool sequence"
                );
                self.emit_status(
                    "lifecycle",
                    &format!(
                        "已把本次 {steps} 步操作沉淀为技能 `/{slug}`，下次可直接复用",
                        steps = calls.len()
                    ),
                );
            }
            Err(crate::skill_auto_create::AutoSkillError::Io(e))
                if e.kind() == std::io::ErrorKind::AlreadyExists =>
            {
                // 同名 auto skill 已存在：`write_auto_skill` 刻意不覆盖，
                // 避免一次会话里反复重写。安静跳过即可。
                tracing::debug!(slug = %slug, "auto skill already exists; skipping");
            }
            Err(e) => {
                tracing::debug!(slug = %slug, error = %e, "auto skill write failed (non-fatal)");
            }
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
        //经验档案的**写入侧**：本会话累积的失败模式 → 跨会话文件。
        //放在 drain 之后，因为 `classify_tool_failures` 读的就是那个计数器。
        self.maybe_record_experience();
        //emit Finalize phase boundary
        self.notify_phase(&LoopPhase::Finalize, total_turns);
        //notify upstream that the task is complete.
        // success = finished naturally without interruption.
        let success = finished_naturally && !interrupted;
        //Harness² 任务级递归：判定 → 记录探针 → 多执行对比提炼。
        //先跑它：它总是记录（成败都记），而 compose 只在有 ≥2 条同任务
        //记录时才产出技能。
        self.maybe_run_task_recursion(
            ctx,
            finished_naturally,
            interrupted,
            tool_errors,
            total_turns,
        );
        //单次执行的骨架 skill：第一次跑通时给出"步骤"，等第二次同任务执行
        //后由上面的 Compose 覆盖成"有效路径 + 分歧点"的对比版。
        self.maybe_write_auto_skill(ctx, success, tool_errors);
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
        acceptance_gate: &mut crate::acceptance_gate::AcceptanceGate,
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
            acceptance_gate,
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
        let loop_state = Self {
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
            last_loop_phase: std::sync::atomic::AtomicU8::new(0),
        };
        loop_state.restore_pending_steer();
        loop_state
    }

    /// Create a new agent loop with a shared interrupt controller.
    pub fn with_interrupt(
        config: AgentConfig,
        tool_registry: Arc<ToolRegistry>,
        llm_provider: Arc<dyn LlmProvider>,
        interrupt: InterruptController,
    ) -> Self {
        let loop_state = Self {
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
            last_loop_phase: std::sync::atomic::AtomicU8::new(0),
        };
        loop_state.restore_pending_steer();
        loop_state
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
        // L1 Domain Mode bias: resolve the current domain (user pin
        // wins, else auto-detect) and boost its preset tools via the
        // `PriorityBoost` wrapper in `default_tool_router`. Weighting,
        // not gating — unlisted tools remain routable.
        let domain_mode =
            crate::prompt_assembly::resolve_domain_mode(&self.config, &user_message);
        let ctx = TurnContext {
            user_message,
            recent_messages: recent,
            active_toolsets: self.config.active_toolsets.clone(),
            max_tools: candidates.len().min(self.config.max_routed_tools),
            priority_tools: hermix_intelligence::preset_for(domain_mode)
                .priority_tools
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
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
        // ★ Sort the final array by name.
        //
        // The provider's prefix cache is keyed on the request bytes, and
        // the `tools` array is part of the request. The router returns
        // schemas in **score** order, and the score depends on the last
        // user message — so the same tool *set* can arrive in a
        // different order from one turn to the next, and the array's
        // bytes change for no reason the model can observe.
        //
        // Measured against the kilo gateway on 2026-10-23 (system
        // prompt sent the way this app sends it — as a `system` role
        // entry in `messages`): the tool array sits at the **end** of
        // the cacheable region, so a reshuffled array costs its own
        // ~512 tokens and nothing else. Sorting by name makes the
        // array a pure function of the tool *set*: same set → same
        // bytes → that ~512 tokens stays cached across turns.
        //
        // The score order had no meaning of its own — it was a side
        // effect of the truncation that decides set membership, and
        // PriorityBoost's job (keeping priority tools inside the cut)
        // is done before this point.
        let mut tool_schemas = assembly.tool_schemas;
        tool_schemas.sort_by(|a, b| a.name.cmp(&b.name));
        tool_schemas
    }

    /// Queue mid-run user guidance (Python `AIAgent.steer`).
    /// Queue mid-run user guidance (Python `AIAgent.steer`).
    /// Returns `false` when the text is empty/whitespace-only.
    pub fn steer(&self, text: &str) -> bool {
        let accepted = crate::steer::steer(&self.pending_steer, text);
        if accepted {
            // Crash-safe spool: a steer accepted mid-turn but not yet
            // drained dies with the process otherwise (the user's
            // instruction silently disappears).
            self.persist_pending_steer();
        }
        accepted
    }

    /// Spool path for this session's pending steer, if we know the session.
    fn pending_steer_path(&self) -> Option<std::path::PathBuf> {
        let session = self.config.session_id.as_deref()?;
        let home = self
            .config
            .hermix_home
            .as_deref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(hermix_config::paths::hermix_home);
        Some(
            home.join("pending_steer")
                .join(format!("{}.txt", crate::session_persistence::session_stem(session))),
        )
    }

    /// Mirror the current buffer to disk (`None` ⇒ delete the spool).
    pub(crate) fn persist_pending_steer(&self) {
        let Some(path) = self.pending_steer_path() else {
            return;
        };
        let text = self
            .pending_steer
            .lock()
            .ok()
            .and_then(|guard| guard.clone());
        match text {
            Some(text) if !text.trim().is_empty() => {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if let Err(e) = std::fs::write(&path, text) {
                    tracing::warn!(error = %e, "failed to spool pending steer");
                }
            }
            _ => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    /// Restore a steer that was spooled before a restart/crash.
    pub(crate) fn restore_pending_steer(&self) {
        let Some(path) = self.pending_steer_path() else {
            return;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        if text.trim().is_empty() {
            let _ = std::fs::remove_file(&path);
            return;
        }
        if let Ok(mut guard) = self.pending_steer.lock() {
            if guard.is_none() {
                *guard = Some(text);
                tracing::info!("restored spooled mid-run steer from a previous process");
            }
        }
    }

    /// 交出 steer 缓冲的句柄，让**外部**（`LocalAgentService` 的按会话
    /// `/queue steer`）能往这一轮里塞指引。
    ///
    /// `pending_steer` 是本模块私有字段，外部拿不到；与其把整个
    /// `AgentLoop` 存进 service（那要求 `AgentLoop: Send + Sync`，并把整个
    /// 引擎的生命周期挂到服务上），不如只交出这一块缓冲——它本来就是
    /// `Arc`，agent 侧 drain 的和外部写的是同一块。
    pub fn steer_buffer(&self) -> std::sync::Arc<std::sync::Mutex<Option<String>>> {
        self.pending_steer.clone()
    }

    /// Clear any queued `/steer` guidance without applying it.
    pub fn clear_pending_steer(&self) {
        crate::steer::clear_pending_steer(&self.pending_steer);
        self.persist_pending_steer();
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

/// 渲染 auto skill 的正文：任务原文 + 按序的操作步骤 + 每条的关键参数 + 证据/教训。
///
/// 参数截断到 200 字符：步骤的顺序与工具名才是可复用信息，完整参数（可能
/// 含几十 KB 的文件内容）只会把 skill 撑爆、把提示词预算吃光。
///
/// 「证据」节是 Harness² judge 的**最小形态**：不判断任务是否"做对了"
/// （那需要 LLM 评判或人工反馈），只如实记录这次执行的可核验事实——几步、
/// 几次错、是否在错误后继续跑通。读者（模型或人）据此决定要不要信这份 skill。
fn render_auto_skill_body(
    slug: &str,
    task_hint: &str,
    calls: &[(String, String)],
    errors: &[hermix_core::ToolErrorRecord],
) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str(&format!(
        "本技能由 agent 自动沉淀（`/{slug}`），来源是一次自然完成的任务。\n\n"
    ));
    out.push_str("## 当时的目标\n\n");
    let hint = task_hint.trim();
    if hint.is_empty() {
        out.push_str("（未记录到用户原文）\n");
    } else {
        out.push_str(hint);
        out.push('\n');
    }
    out.push_str("\n## 操作步骤\n\n");
    for (i, (name, args)) in calls.iter().enumerate() {
        let args_preview: String = args.chars().take(200).collect();
        let ellipsis = if args.chars().count() > 200 { "…" } else { "" };
        out.push_str(&format!("{}. `{name}` — `{args_preview}{ellipsis}`\n", i + 1));
    }

    out.push_str("\n## 证据\n\n");
    out.push_str(&format!(
        "- 工具调用 {} 次，其中失败 {} 次。\n",
        calls.len(),
        errors.len()
    ));
    if errors.is_empty() {
        out.push_str("- 全程无工具错误：这是**顺利跑通**的路径。\n");
    } else {
        out.push_str(
            "- 中途出错但最终自然完成：这是**试错后绕通**的路径，下面的坑要当真。\n",
        );
    }

    if !errors.is_empty() {
        out.push_str("\n## 踩过的坑\n\n");
        for e in errors.iter().take(8) {
            let snippet: String = e.error.chars().take(180).collect();
            let ellipsis = if e.error.chars().count() > 180 { "…" } else { "" };
            out.push_str(&format!(
                "- 第 {} 轮 `{}` 失败：{}{}\n",
                e.turn, e.tool_name, snippet, ellipsis
            ));
        }
        if errors.len() > 8 {
            out.push_str(&format!("- （另有 {} 条错误未列出）\n", errors.len() - 8));
        }
        out.push_str(
            "\n照做时**优先避开上面这些失败形态**，而不是先重试再绕过——\
             这是本技能最有价值的部分。\n",
        );
    }

    out.push_str(
        "\n## 使用注意\n\n\
         - 步骤顺序与工具名来自实际执行路径，可直接照做；**参数必须按当前环境核对**\
         （路径、ID、URL 会变）。\n\
         - 这是自动生成的骨架。要补充背景、适用条件或失败处理，用 \
         `skill_manage patch` 增量改写，不要整篇重写。\n\
         - 若目标站点/工具的行为已变化，先 `skill_view` 读一遍再决定是否更新。\n",
    );
    out
}

#[cfg(test)]
mod auto_skill_render_tests {
    use super::render_auto_skill_body;

    #[test]
    fn body_lists_steps_in_order_with_truncated_args() {
        let calls = vec![
            ("browser_navigate".to_string(), r#"{"url":"https://x.com"}"#.to_string()),
            ("browser_type".to_string(), format!("{{\"text\":\"{}\"}}", "x".repeat(500))),
            ("browser_click".to_string(), r#"{"ref":"e12"}"#.to_string()),
        ];
        let body = render_auto_skill_body("demo", "登录超星并看课程", &calls, &[]);

        assert!(body.contains("登录超星并看课程"));
        assert!(body.contains("1. `browser_navigate`"));
        assert!(body.contains("2. `browser_type`"));
        assert!(body.contains("3. `browser_click`"));
        // 500 字符的参数被截断，且不把整个 skill 撑爆。
        assert!(body.contains('…'), "超长参数必须出现截断标记");
        assert!(body.len() < 2000, "正文必须保持精简: {}", body.len());
    }

    #[test]
    fn body_survives_an_empty_task_hint() {
        let calls = vec![("read_file".to_string(), "{}".to_string())];
        let body = render_auto_skill_body("demo", "", &calls, &[]);
        assert!(body.contains("未记录到用户原文"));
        assert!(body.contains("1. `read_file`"));
    }

    /// Harness² 的教训分级：错误-恢复对是最有价值的对比，必须进正文。
    /// 旧门槛（"全程无错才沉淀"）会把这类会话整条丢掉。
    #[test]
    fn errors_become_a_pitfalls_section() {
        let calls = vec![
            ("terminal".to_string(), r#"{"command":"cargo build"}"#.to_string()),
            ("read_file".to_string(), r#"{"path":"a.txt"}"#.to_string()),
        ];
        let errors = vec![hermix_core::ToolErrorRecord {
            tool_name: "terminal".to_string(),
            error: "per-call timeout after 600s: tool 'terminal' did not return in time".to_string(),
            turn: 2,
        }];
        let body = render_auto_skill_body("demo", "构建项目", &calls, &errors);

        assert!(body.contains("## 踩过的坑"), "错误必须成节: {body}");
        assert!(body.contains("第 2 轮 `terminal` 失败"), "{body}");
        assert!(body.contains("试错后绕通"), "证据节要标出这是错误-恢复路径: {body}");
        assert!(body.contains("失败 1 次"), "{body}");
        // 无错会话不应出现坑位节。
        let clean = render_auto_skill_body("demo", "构建项目", &calls, &[]);
        assert!(!clean.contains("## 踩过的坑"));
        assert!(clean.contains("顺利跑通"));
    }
}
