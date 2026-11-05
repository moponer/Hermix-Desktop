//! Shared tool-execution turn (post-tool iteration + tool turn + convergence)
//!
//! Moved verbatim from `run_loop.rs` (pure relocation, no behavior
//! change). Methods are `pub(super)` because their callers live in the
//! parent `run_loop` module. All names resolve via the parent glob.

#[allow(unused_imports)]
use super::*;
use crate::agent_loop::extract_file_change;
use crate::budget;
use crate::context::ContextManager;
use crate::spill;
use crate::tool_result_processor::push_tool_result;
use hermix_core::{format_token_count, LlmResponse, ToolErrorRecord};

impl AgentLoop {
    /// O1.1: shared post-tool iteration block — was ~200 lines of
    /// duplication between run() and run_stream(). Handles: post-tool
    /// hooks, file-change events, memory/delegation notifications,
    /// budget enforcement, budget pressure, push results, steer,
    /// execute_code deduction, background review, context pressure,
    /// auto-compression, state writeback.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn do_post_tool_iteration(
        &self,
        ctx: &mut ContextManager,
        tool_calls: &[ToolCall],
        results: Vec<hermix_core::ToolResult>,
        state: &mut LoopState,
        _session_id: &str,
        _tool_errors: &mut Vec<ToolErrorRecord>,
        _session_started_hooks_fired: bool,
        _persist_user_idx: Option<usize>,
        _review_memory_at_end: bool,
        total_turns: &mut u32,
        tool_call_ids: &HashMap<String, String>,
        stream_needs_break: Option<&AtomicBool>,
        on_chunk: Option<&(dyn Fn(StreamChunk) + Send + Sync)>,
    ) -> bool {
        // Post-tool hooks + callbacks
        for res in &results {
            let Some(tc) = tool_calls.iter().find(|tc| tc.id == res.tool_call_id) else {
                continue;
            };
            let tc_ctx = serde_json::json!({"tool": &tc.function.name, "is_error": res.is_error, "turn": total_turns});
            self.invoke_hook(HookType::PostToolCall, &tc_ctx);
            if let Some(ref cb) = self.callbacks.on_tool_complete {
                let tool_call_id = tool_call_ids
                    .get(&tc.id)
                    .cloned()
                    .unwrap_or_else(|| format!("legacy-{}", tc.function.name));
                cb(
                    &tool_call_id,
                    &tc.function.name,
                    &res.content,
                    res.denial_kind.as_deref().unwrap_or(""),
                    res.is_error,
                );
            }
        }

        // File-change summary events
        for res in &results {
            let Some(tc) = tool_calls.iter().find(|tc| tc.id == res.tool_call_id) else {
                continue;
            };
            if res.is_error {
                continue;
            }
            if let Some(change) = extract_file_change(&tc.function.name, &tc.function.arguments, &res.content) {
                if let Some(ref cb) = self.callbacks.on_tool_complete {
                    let synth_id = format!("file_change::{}", change.path);
                    let synth_name = format!("file_change::{}", change.path);
                    let payload = serde_json::json!({
                        "change_type": change.change_type,
                        "file_path": change.path,
                        "file_summary": change.summary,
                    }).to_string();
                    cb(&synth_id, &synth_name, &payload, "", false);
                }
            }
        }

        self.notify_memory_writes(tool_calls, &results);
        self.notify_delegations(tool_calls, &results);

        // Enforce budget on tool results. Oversized results are spilled to
        // disk first so the model can retrieve them with read_file instead
        // of only seeing a lossy truncation marker. Sink is built lazily:
        // no oversized result means no filesystem work at all.
        let mut results = results;

        // 空的 tool_result 会被替换成一句人类可读的说明，而不是原样送进上下文。
        // 抄 cc-haha `toolResultStorage.ts:287-296`：提示词末尾挂一条裸空结果时，
        // 某些模型会直接把它当成"说完了"而吐停止序列——一次工具调用白跑一轮。
        // 只处理成功的空结果：错误结果本身就有文字。
        {
            let name_by_id: std::collections::HashMap<&str, &str> = tool_calls
                .iter()
                .map(|tc| (tc.id.as_str(), tc.function.name.as_str()))
                .collect();
            for r in &mut results {
                if r.is_error || !r.content.trim().is_empty() {
                    continue;
                }
                let name = name_by_id
                    .get(r.tool_call_id.as_str())
                    .copied()
                    .unwrap_or("tool");
                r.content = format!("({name} completed with no output)");
            }
        }

        // 落盘只在本轮确实有结果超过它**自己那一档**的上限时才准备 —— 分档
        // 之后最小的一档是 3 K（`list_files`），仍用固定的 20 K 判据会让
        // 3–20 K 的结果既超出上限又拿不到取回指针。
        let smallest_cap = tool_calls
            .iter()
            .map(|tc| hermix_tools::result_budget::model_visible_cap(&tc.function.name))
            .min()
            .unwrap_or(spill::SPILL_INLINE_CHARS)
            .min(spill::SPILL_INLINE_CHARS);
        let spill_sink = results
            .iter()
            .any(|r| r.content.chars().count() > smallest_cap)
            .then(spill::SpillSink::default_dir);
        budget::enforce_budget_with_spill(
            &mut results,
            &self.config.budget,
            spill_sink.as_ref(),
            tool_calls,
        );

        // Removed verification notes injection and budget
        // pressure injection into tool results. These were polluting the
        // context with system-level messages that interfered with the
        // LLM's natural reasoning. The tool results themselves already
        // contain execution status. Cost guard warnings go to the UI,
        // not into the LLM's context.

        // (type-B diagnostic): emit an INFO log *before* the
        // post-tool writeback loop consumes `results` via `into_iter()`
        // — the count + error count would otherwise be unavailable.
        // Pair this with the `do_pre_turn_setup: entering` INFO at the
        // top of the next phase: if you see this INFO but never see
        // the pre-turn-setup INFO, the hang is in this body
        // (writeback / hooks / cost guard / `auto_compress`). If you
        // see the pre-turn-setup INFO but no `llm_response_received`
        // for the next turn, the hang is in `do_pre_turn_setup` or
        // `collect_stream_llm_response`.
        tracing::info!(
            turn    = state.total_turns,
            results = results.len(),
            errors  = results.iter().filter(|r| r.is_error).count(),
            "do_post_tool_iteration: post-tool results accepted, entering writeback & auto-compress"
        );

        // (v2 — zero-context-overhead model, inspired by
        // Hermes Agent's delegate_task design): When the LLM calls
        // `read_image` in the main context, NO image base64 is ever
        // injected into the main context. All base64 is stripped from
        // tool results (see `strip_image_base64` in
        // `tool_result_processor.rs`). Instead, a system nudge is
        // injected telling the LLM to use `delegate_task` for image
        // analysis — the sub-agent's isolated context absorbs the
        // base64 cost and destroys it when done.
        // This is the "zero-context-overhead" model: the main context
        // NEVER contains image base64, period. Even a single image
        // costs 2,000–8,000 tokens that are paid on every subsequent
        // turn — that's always wasteful.
        //
        // A `delegate_task` child is exempt, and takes the opposite path:
        // it is already the isolation boundary this rule exists to create,
        // so its `read_image` result is re-injected as a real multimodal
        // content block. The nudge below is skipped there because it points
        // at `delegate_task`, which `BLOCKED_TOOLS_FOR_SUBAGENT` removes from
        // the child's registry — following it would be impossible.
        let mut read_image_paths: Vec<String> = Vec::new();
        let mut child_images: Vec<(String, String)> = Vec::new();
        for result in &results {
            if result.is_error {
                continue;
            }
            let Some(tc) = tool_calls.iter().find(|t| t.id == result.tool_call_id) else {
                continue;
            };
            // read_image 的结果 JSON 里带整帧 base64，提取成
            // 子 agent 的多模态内容块。
            if tc.function.name != "read_image" {
                continue;
            }
            if self.config.is_sub_agent {
                // 子 agent：把像素转成真正的多模态内容块（下方统一注入）。
                // 文本结果保持原样不剥——同一份 JSON 还要供 UI 的
                // ScreenshotPreview 渲染，剥了预览就瞎；子 agent 上下文
                // 是隔离边界，任务结束即销毁，双份成本有界。
                let payload = crate::tool_result_processor::extract_image_payload(&result.content);
                if let Some(payload) = payload {
                    child_images.push(payload);
                }
                continue;
            }
            let Ok(parsed) = serde_json::from_str::<Value>(&result.content) else {
                continue;
            };
            let img_path = parsed
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("<unknown>");
            read_image_paths.push(img_path.to_string());
        }

        if !read_image_paths.is_empty() {
            let total = read_image_paths.len();
            tracing::info!(
                paths = ?read_image_paths,
                total = total,
                "read_image called in main context — injecting delegate_task nudge (zero-context-overhead model)"
            );
            // Universal nudge: ALWAYS use delegate_task for image analysis.
            let paths_str = read_image_paths.join(", ");
            ctx.add_message(Message::system(&format!(
                "[SYSTEM — 图片分析必须使用 delegate_task]\n\
                 你刚才在主上下文中调用了 read_image 读取了 {total} 张图片：{paths}\n\n\
                 ⚠️ 图片 base64 已从工具结果中剥离，你无法在主上下文中看到图片内容。\n\n\
                 【必须操作】使用 `delegate_task` 将图片分析任务派给子 agent：\n\
                 - 子 agent 拥有隔离的上下文，可以自由调用 read_image 查看和分析图片\n\
                 - 子 agent 完成后只回传纯文字结论，图片 base64 不会进入你的上下文\n\
                 - 如有多张图片，可分批派发给多个子 agent（建议每批 3-5 张）\n\n\
                 示例：\n\
                 delegate_task(task=\"读取并分析图片，返回图片中可见的所有关键信息（文字、数字、证件字段等）的 JSON 摘要\", \\\n\
                 context=\"图片路径：{paths}\")\n\n\
                 🔴 禁止在主上下文中再次调用 read_image。所有图片分析必须通过 delegate_task 进行。\
                 这条规则没有例外——即使是单张图片也必须走子 agent。",
                total = total,
                paths = paths_str,
            )));
        }

        for result in results {
            // Record failed tools for the terminal hint's ping-pong
            // prevention: if a structured alternative already failed,
            // the hint won't suggest it again.
            if result.is_error {
                if let Some(name) = tool_calls
                    .iter()
                    .find(|tc| tc.id == result.tool_call_id)
                    .map(|tc| tc.function.name.as_str())
                {
                    crate::tool_result_processor::record_tool_failure(name);
                }
            }
            // Apply terminal failure hint: if the terminal command failed
            // due to shell-compatibility issues, append a structured
            // suggestion to use move_file / read_file / execute_code instead.
            let tool_name = tool_calls
                .iter()
                .find(|tc| tc.id == result.tool_call_id)
                .map(|tc| tc.function.name.as_str());
            let content = crate::tool_result_processor::maybe_append_terminal_failure_hint(
                &result.content,
                tool_name,
                result.is_error,
            );
            push_tool_result(ctx, tool_calls, &result.tool_call_id, &content);
            // (human-approval fix): if this tool call was approved by the
            // USER clicking 允许 on the approval card (not an allowlist /
            // session-bypass auto-allow), inject an explicit system note so
            // the LLM knows the tool ran because the *user* consciously
            // approved it. Without this, the model cannot tell a manual
            // approval from a silent rule-based allow, and may report the
            // action as if it auto-passed. We consume the id so the note
            // fires exactly once, on the turn the tool actually executes.
            if let Some(ids) = &self.callbacks.human_approved_ids {
                let mut guard = ids.lock().expect("human_approved_ids poisoned");
                if guard.remove(&result.tool_call_id) {
                    drop(guard);
                    let name = tool_name.unwrap_or("<未知工具>");
                    ctx.add_message(Message::system(format!(
                        "[人工审批确认] 你刚才请求的 `{name}` 工具调用，已由用户在审批卡片上点击「允许」人工审查通过。\
                         这不是自动放行或白名单放行，而是用户有意识的人工批准。请尊重这一人工决策，\
                         并在向用户汇报时如实说明：该操作是经用户手动批准后才执行的。",
                        name = name,
                    )));
                }
            }
            // if the result is a *denial* (user拒绝 /
            // 超时 / 连接中断), inject the structured "tell the user"
            // system note right after the tool_result so the LLM's
            // next turn obeys it instead of going quiet or silently
            // retrying.
            crate::tool_result_processor::inject_denial_note_if_needed(
                ctx,
                tool_calls,
                &result,
            );
            // (sandbox-permission fix): if the tool failed with
            // a PERMISSION_DENIED error, inject the "ask the user to grant
            // access" guidance right after the denial note (the two are
            // orthogonal — a denial is a user refusal, a permission error is a
            // runtime restriction).
            crate::tool_result_processor::inject_permission_note_if_needed(
                ctx,
                tool_calls,
                &result,
            );
        }
        // Sub-agent vision: the tool result text is metadata only; the pixels
        // go in as `content_parts`, the shape every provider's vision path
        // expects (`provider.rs` rewrites `content_parts` → `content`).
        //
        // Emitted AFTER the whole writeback loop above, never inside it — a
        // `user` message wedged between an assistant's `tool_calls` and its
        // `tool` results breaks the strict pairing that
        // `provider.rs::enforce_strict_tool_pairing` exists to protect
        // (MiniMax-M3 answers HTTP 400 `(2013) tool call result does not
        // follow tool call`).
        if !child_images.is_empty() {
            let n = child_images.len();
            tracing::info!(
                images = n,
                "read_image in sub-agent context — injecting multimodal content blocks"
            );
            let refs: Vec<(&str, &str)> = child_images
                .iter()
                .map(|(m, b)| (m.as_str(), b.as_str()))
                .collect();
            ctx.add_message(Message::user_with_images(
                &format!("[已加载 {n} 张图片，以下为图像内容 —— 请直接查看并分析，不要再次调用 read_image]"),
                &refs,
            ));
        }
        self.post_tool_apply_pending_steer(ctx);
        // Removed execute_code turn-counter deduction hack.
        // It was compensating for execute_code not counting as a "real"
        // turn, but this conflated turn accounting with tool semantics.
        // Trust the turn counter — every loop iteration is a turn.

        // Stream break control (stream-only)
        if let (Some(snb), Some(on_c)) = (stream_needs_break, on_chunk) {
            snb.store(true, Ordering::Release);
            on_c(StreamChunk {
                delta: Some(hermix_core::StreamDelta {
                    content: None,
                    tool_calls: None,
                    extra: Some(serde_json::json!({"control": "stream_break"})),
                }),
                finish_reason: None,
                usage: None,
            });
        }

        self.emit_background_review_metrics(state.total_turns, ctx);

        // NOTE: no per-round tool-result roll-up here.
        //
        // This used to call `rollup_tool_results(ctx, 30, 8_000)` on every
        // tool round. Its predicate is **distance from the tail**, so as new
        // messages arrive one more already-sent tool result crosses the
        // frontier and gets rewritten in the middle of the history. Provider
        // prefix caches (MiniMax/DeepSeek/Anthropic/OpenAI) match on bytes
        // from position 0, so rewriting one mid-history row invalidates
        // everything after it — measured cost was the whole last-30-message
        // window (~15-40K tokens) re-billed on **every** call, which pinned
        // the cache hit rate at ~77% no matter how long the session ran.
        //
        // Content already sent once is therefore frozen: context is only
        // shrunk at insertion time (tool-result budget) or by a deliberate
        // compression event (`compression_ops`, which pays one cache reset
        // and is rare). Pass1 inside `compress()` still rolls stale bodies
        // up as the final safety net, and archives them there.

        // Context pressure check
        let total_tokens = ctx.total_tokens();
        let threshold = ctx.pressure_threshold_tokens();
        let max_tokens = ctx.max_context_tokens();
        let mut compacted = false;
        // Fire a context_usage event every turn so the frontend can
        // render a persistent context-usage bar. We send it through
        // the on_chunk callback (same path as status/compaction events).
        if let Some(on_c) = on_chunk {
            let percent = if max_tokens > 0 {
                ((total_tokens as f64 / max_tokens as f64) * 100.0).min(100.0)
            } else {
                0.0
            };
            // Include cumulative cache hit/miss data from
            // the CostGuard so the frontend can display "缓存命中 45k"
            // alongside the context usage bar.
            let (cache_read, cache_creation) = state
                .cost_guard
                .accumulated_usage
                .as_ref()
                .map(|u| (u.cache_read_tokens, u.cache_creation_tokens))
                .unwrap_or((0, 0));
            on_c(StreamChunk {
                delta: Some(hermix_core::StreamDelta {
                    content: None,
                    tool_calls: None,
                    extra: Some(serde_json::json!({
                        "event": "context_usage",
                        "used_tokens": total_tokens as u64,
                        "max_tokens": max_tokens as u64,
                        "threshold_tokens": threshold as u64,
                        "percent": percent,
                        "cache_read_tokens": cache_read,
                        "cache_creation_tokens": cache_creation,
                    })),
                }),
                finish_reason: None,
                usage: None,
            });
        }
        if threshold > 0 {
            let progress = total_tokens as f64 / threshold as f64;
            let tier = if progress >= 0.95 {
                0.95
            } else if progress >= 0.85 {
                0.85
            } else {
                0.0
            };
            if state.context_pressure.should_emit_warning(progress, tier) {
                let message = format!(
                    "Context pressure {:.0}% of compaction threshold ({} / {})",
                    progress * 100.0,
                    format_token_count(total_tokens),
                    format_token_count(threshold)
                );
                tracing::warn!("{}", message);
                self.emit_status("lifecycle", &message);
            }
        }

        self.auto_compress_if_over_threshold(ctx).await;

        // After compression, emit an updated context_usage event so the
        // frontend progress bar reflects the reduced token count immediately
        // (the pre-compression event was already sent above with the old value).
        let new_total = ctx.total_tokens();
        if new_total < total_tokens {
            compacted = true;
            if let Some(on_c) = on_chunk {
                let new_percent = if max_tokens > 0 {
                    ((new_total as f64 / max_tokens as f64) * 100.0).min(100.0)
                } else {
                    0.0
                };
                on_c(StreamChunk {
                    delta: Some(hermix_core::StreamDelta {
                        content: None,
                        tool_calls: None,
                        extra: Some(serde_json::json!({
                            "event": "context_usage",
                            "used_tokens": new_total as u64,
                            "max_tokens": max_tokens as u64,
                            "threshold_tokens": threshold as u64,
                            "percent": new_percent,
                            "cache_read_tokens": 0,
                            "cache_creation_tokens": 0,
                        })),
                    }),
                    finish_reason: None,
                    usage: None,
                });
            }
        }
        // `total_turns` lives on `state` now (the previous `&mut u32`
        // parameter was hoisted into `LoopState`); no copy-back needed.
        compacted
    }

    //: shared tool-execution + post-tool + convergence +
    // TurnReport block. Both `run()` and `run_stream()` had identical
    // ~60-line copies of this code. The only real differences are:
    // - log level (info vs debug for phase notification)
    // - `finalize_agent_result` params (is_stream flag + last_finish_reason)
    // - `do_post_tool_iteration` stream params (None vs Some)
    // All three are parameterized here.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn do_tool_turn(
        &self,
        ctx: &mut ContextManager,
        state: &mut LoopState,
        tool_calls: &[ToolCall],
        total_turns: u32,
        tool_errors: &mut Vec<ToolErrorRecord>,
        session_id: &str,
        session_started_hooks_fired: bool,
        persist_user_idx: Option<usize>,
        review_memory_at_end: bool,
        tool_call_ids: &HashMap<String, String>,
        response: &LlmResponse,
        is_stream: bool,
        last_finish_reason: Option<String>,
        stream_needs_break: Option<&Arc<AtomicBool>>,
        on_chunk: Option<&(dyn Fn(StreamChunk) + Send + Sync)>,
    ) -> ToolTurnOutcome {
        // --- tool execution ---
        let tool_start = Instant::now();
        if is_stream {
            tracing::debug!(
                phase = %LoopPhase::ToolExecution,
                turn = total_turns,
                tools = tool_calls.len(),
                "do_tool_turn: executing tool calls"
            );
        } else {
            tracing::info!(
                phase = %LoopPhase::ToolExecution,
                turn = total_turns,
                tools = tool_calls.len(),
                "do_tool_turn: executing tool calls"
            );
        }
        self.notify_phase(&LoopPhase::ToolExecution, total_turns);

        // Pre-dispatch scope gates, in priority order:
        //  1. Plan mode (read-only research) — denies known mutators.
        //  2. Skill `allowed-tools` (subtract-only) — denies tools outside
        //     the union declared by this turn's auto-activated skills.
        // Denied calls get an envelope instead of executing; no approval
        // card is ever shown for them.
        let plan_mode = self.tool_registry.plan_mode_active();
        let (scoped_calls, mut scope_denials): (Vec<ToolCall>, Vec<hermix_core::ToolResult>) = {
            let mut keep = Vec::new();
            let mut denied = Vec::new();
            for tc in tool_calls {
                let name = &tc.function.name;
                if plan_mode && hermix_tools::tools::plan_mode::is_blocked_in_plan_mode(name) {
                    denied.push(hermix_core::ToolResult::denied(
                        &tc.id,
                        format!(
                            "工具 `{name}` 在计划模式下被禁止（只读调研阶段）。\
                             先用读取/搜索/网页类工具完成调研并给出计划，\
                             再调用 exit_plan_mode 恢复执行。"
                        ),
                        "plan_mode",
                    ));
                    continue;
                }
                if !crate::tool_executor::tool_allowed_by_active_skill(
                    state.active_skill_tools.as_ref(),
                    name,
                ) {
                    let mut available: Vec<String> = state
                        .active_skill_tools
                        .as_ref()
                        .map(|s| s.iter().cloned().collect())
                        .unwrap_or_default();
                    available.sort();
                    denied.push(hermix_core::ToolResult::denied(
                        &tc.id,
                        format!(
                            "工具 `{name}` 不在当前激活技能的 allowed-tools 白名单内。\
                             本回合可用白名单：{}。如确需该工具，先用 skill_view 查看技能声明，\
                             或在不需要该技能的轮次再调用。",
                            available.join(", ")
                        ),
                        "skill_scope",
                    ));
                    continue;
                }
                keep.push(tc.clone());
            }
            (keep, denied)
        };

        let mut results = self
            .execute_tool_calls(
                &scoped_calls,
                total_turns,
                self.config
                    .max_cost_usd
                    .map(|limit| (limit - state.cost_guard.session_cost_usd).max(0.0)),
                tool_errors,
            )
            .await;
        results.append(&mut scope_denials);
        state
            .timings
            .record_tool(tool_start.elapsed().as_millis() as u64);

        // --- Rollback check ---
        let turn_error_count = results.iter().filter(|r| r.is_error).count() as u32;
        if self.config.rollback_on_tool_error_threshold > 0
            && turn_error_count >= self.config.rollback_on_tool_error_threshold
        {
            if let Some(snapshot) = state.message_cache.checkpoint().cloned() {
                *ctx.get_messages_mut() = snapshot;
                ctx.add_message(Message::system(format!(
                    "Auto-rollback: {} tool call(s) failed in one turn. \
                     Restored latest checkpoint and continuing.",
                    turn_error_count
                )));
                ctx.recompute_total_tokens();
                return ToolTurnOutcome::Rollback;
            }
        }

        // --- 零新信息守卫（收敛）---
        // 指纹只看「工具名 + 是否失败 + 结果正文」，故意不含 tool_call_id
        // （每轮新生成）。写完文件重读同一路径会得到不同内容 → 不算重复；
        // 把同一批只读调用再发一遍 → 逐字相同 → 判定零进展。命中阈值就
        // 取消后续轮次的工具 schema（真正的硬约束，见 RepeatGuard）。
        {
            let mut parts: Vec<(String, bool, &str)> = results
                .iter()
                .map(|r| {
                    let name = tool_calls
                        .iter()
                        .find(|tc| tc.id == r.tool_call_id)
                        .map(|tc| tc.function.name.as_str())
                        .unwrap_or("");
                    (name.to_string(), r.is_error, r.content.as_str())
                })
                .collect();
            parts.sort();
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            use std::hash::{Hash, Hasher};
            for p in &parts {
                p.hash(&mut hasher);
            }
            let fingerprint = hasher.finish();

            let streak = state.repeat_guard.observe_batch(fingerprint);
            if state.repeat_guard.should_disable_tools() && state.repeat_guard.disable_tools() {
                tracing::warn!(
                    turn = total_turns,
                    streak,
                    calls = results.len(),
                    "no new information: identical tool results repeated — disabling tools to force a wrap-up"
                );
                self.emit_status(
                    "lifecycle",
                    &format!(
                        "连续 {streak} 轮工具结果与之前完全相同（零新信息）——已停用工具，直接汇报。"
                    ),
                );
                ctx.add_message(Message::system(format!(
                    "[SYSTEM] The last {streak} tool batches returned results identical to earlier \
                     batches — they added no new information. Tools are now DISABLED for this run. \
                     Reply to the user in one message: what is done, what was actually verified, \
                     and what is blocked. Do not ask for more tool calls."
                )));
            }
        }

        // --- 零推进诊断（L2，认知层，不停止）---
        // 判据是「这一轮有没有产生**新的世界状态**」：签名在工具执行出口由
        // `hermix_tools::tool_state` 写进 `ToolResult::state`（文件 stat /
        // 命令+输出 / 页面落点），这里只消费字段，不再解析文本。与工具是否
        // 报错、是否同一工具、模型有没有写正文都无关。2026-11-05 实测：同一块
        // 登录页上连续 18 轮 navigate + eval，每轮 errors=0、结果逐字不同，
        // ErrorLoop 与 RepeatGuard 都没反应。
        {
            let observed: Vec<String> = results
                .iter()
                .filter_map(|r| r.state.clone())
                .collect();
            if let Some(text) = state.progress_ledger.observe_batch(&observed) {
                tracing::info!(
                    turn = total_turns,
                    zero_progress_streak = crate::progress_ledger::ZERO_PROGRESS_STREAK,
                    "zero new world state for several turns — injecting progress diagnosis"
                );
                self.emit_status(
                    "lifecycle",
                    "连续多轮没有拿到新的世界状态——已把卡点分析交给模型换路径。",
                );
                ctx.add_message(Message::system(text));
            }
        }

        // --- post-tool execution ---
        tracing::debug!(
            phase = %LoopPhase::PostToolExec,
            turn = total_turns,
            "do_tool_turn: post-tool iteration"
        );
        self.notify_phase(&LoopPhase::PostToolExec, total_turns);

        let mut total_turns_mut = total_turns;
        let compacted = self
            .do_post_tool_iteration(
                ctx,
                tool_calls,
                results.clone(),
                state,
                session_id,
                tool_errors,
                session_started_hooks_fired,
                persist_user_idx,
                review_memory_at_end,
                &mut total_turns_mut,
                tool_call_ids,
                stream_needs_break.map(|a| a.as_ref()),
                on_chunk,
            )
            .await;

        // Rapid-refill circuit breaker (ZCode evaluateRapidRefill analog):
        // ≥3 successful compactions within the last 5 turns means ctx is
        // re-filling faster than compression can shrink it — stop the run
        // instead of burning tokens on a compression storm.
        if compacted && state.context_pressure.record_compaction(total_turns) {
            tracing::warn!(
                turn = total_turns,
                "rapid context refill detected — hard-stopping to prevent a compression storm"
            );
            self.emit_status(
                "lifecycle",
                "上下文增速异常（5 轮内已压缩 3 次）——为避免压缩风暴已停止本轮任务。\
                 请开新会话，或把大输出改为落盘后按需读取/委托子代理分析。",
            );
            self.persist_session_terminal(
                ctx,
                session_id,
                total_turns,
                review_memory_at_end,
            );
            let result = self.finalize_agent_result(
                ctx,
                total_turns,
                tool_errors,
                state.cost_guard.accumulated_usage.clone(),
                state.cost_guard.session_cost_usd,
                session_started_hooks_fired,
                persist_user_idx,
                is_stream,
                false,
                last_finish_reason,
            );
            return ToolTurnOutcome::Converge(Ok(result));
        }

        // --- Convergence detection ---
        let visible_text_this_turn = response.message.content.as_deref().unwrap_or("");
        let has_visible = Self::assistant_visible_text_after_think_blocks(&response.message);
        // Build per-tool failure/success lists for the convergence detector.
        // This lets it detect "terminal keeps failing even though read_file
        // succeeds" — a pattern the old aggregate error count missed.
        // Infra denials (`timed_out` / `channel_closed`) are excluded: the
        // tool never executed, so they are transport failures, not model
        // stuckness — counting them let a WS blip + approval timeouts
        // manufacture an ErrorLoop hard stop.
        let genuine_error_count = results
            .iter()
            .filter(|r| r.is_error && !r.is_infra_denial())
            .count();
        let failed_tool_names: Vec<&str> = results
            .iter()
            .filter(|r| r.is_error && !r.is_infra_denial())
            .filter_map(|r| {
                tool_calls
                    .iter()
                    .find(|tc| tc.id == r.tool_call_id)
                    .map(|tc| tc.function.name.as_str())
            })
            .collect();
        let succeeded_tool_names: Vec<&str> = results
            .iter()
            .filter(|r| !r.is_error)
            .filter_map(|r| {
                tool_calls
                    .iter()
                    .find(|tc| tc.id == r.tool_call_id)
                    .map(|tc| tc.function.name.as_str())
            })
            .collect();
        // 「本轮有没有输出」要连思考一起算：thinking 模型（MiniMax
        // M3.1-Flash 等）把推演写进 `reasoning_content`、正文留空，而前端
        // 是把这段思考原样渲染给用户的。只认 `content` 等于把「用户正在读
        // 它的思考」记成「它什么都没说」，累计 18 轮就触发
        // NoProgressWithTools 硬停（2026-11-05 实测：22 轮的课程目录探查
        // 被砍在半路）。真空转由 `RepeatGuard`（工具结果逐字重复）兜底。
        let produced_output = crate::message_helpers::assistant_produced_output(&response.message);
        let convergence_signal = state.convergence_detector.check(
            produced_output,
            tool_calls.len(),
            genuine_error_count,
            visible_text_this_turn,
            &failed_tool_names,
            &succeeded_tool_names,
        );
        match convergence_signal {
            crate::convergence_detector::ConvergenceSignal::Converge {
                reason,
                consecutive_count,
            } => {
                tracing::info!(
                    reason = ?reason,
                    consecutive = consecutive_count,
                    "convergence detected (hard stop) — concluding run"
                );
                // 硬停必须自己解释（2026-11-05）：以前这里直接 finalize，
                // 一条用户可见的话都不留 —— 界面停在「正在回复…」，用户
                // 只能手动发一句「继续」才知道为什么停。只在本轮没有任何
                // 正文时补，避免盖掉模型刚说过的话。
                if !has_visible {
                    let note = convergence_wrap_up(&reason, consecutive_count);
                    self.emit_status("lifecycle", &note);
                    ctx.add_message(hermix_core::Message::assistant(note));
                }
                // Harness²：**收敛停止本身就是最值得跨会话留存的教训**
                // （"这条路走不通"）。在这里就地记录，而不是等 finalize——
                // finalize 的入参里没有 convergence reason，要传它得改
                // `finalize_agent_result` 的签名与两处调用点；而"发生地记录"
                // 语义更准，也不会因为将来多一条停止路径而漏传。
                if let Some(pattern) = crate::harness_self_edit::classify_convergence(
                    &reason,
                    consecutive_count,
                    "hard stop",
                ) {
                    let entry = crate::harness_self_edit::generate_experience(&pattern);
                    let dir = hermix_config::paths::hermix_home().join("experience");
                    match crate::harness_self_edit::persist_experience(&dir, &entry) {
                        Ok(()) => tracing::info!(
                            kind = pattern.kind.label(),
                            count = consecutive_count,
                            "convergence lesson written to the experience archive"
                        ),
                        Err(e) => {
                            tracing::debug!(error = %e, "convergence experience not persisted");
                        }
                    }
                }
                self.persist_session_terminal(
                    ctx,
                    session_id,
                    total_turns,
                    review_memory_at_end,
                );
                let result = self.finalize_agent_result(
                    ctx,
                    total_turns,
                    tool_errors,
                    state.cost_guard.accumulated_usage.clone(),
                    state.cost_guard.session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                    is_stream,
                    false,
                    last_finish_reason,
                );
                return ToolTurnOutcome::Converge(Ok(result));
            }
            _ => {}
        }

        // --- TurnReport ---
        crate::turn_report::TurnReport {
            turn: total_turns,
            end_phase: LoopPhase::PostToolExec,
            stage: LoopPhase::PostToolExec.as_orchestration_stage(),
            api_ms: state.timings.total_api_time_ms,
            tool_ms: state.timings.total_tool_time_ms,
            // 此前硬编码 `vec![]`，于是 turn report 的 `tool_count` 恒为 0
            // —— 实际执行了工具也看不出来，排障时会被误导成「空转」。
            // `results` 由 `execute_tool_calls` 按 `scoped_calls` 顺序生成
            // （后面追加的 scope_denials 同样带 `is_error`），按索引取即可。
            //
            // `duration_ms` 仍为 0：timings 只记整轮工具耗时，没有单调用的
            // 计时，这里不编造数字——先看数量与错误态是否够用。
            tool_calls: tool_calls
                .iter()
                .enumerate()
                .map(|(i, tc)| crate::turn_report::TurnToolCall {
                    name: tc.function.name.clone(),
                    is_error: results.get(i).map(|r| r.is_error).unwrap_or(false),
                    is_replay: false,
                    duration_ms: 0,
                })
                .collect(),
            replay_count: 0,
            has_visible_text: has_visible,
            has_thinking: response
                .message
                .reasoning_content
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty()),
            prompt_tokens: response
                .usage
                .as_ref()
                .map(|u| u.prompt_tokens)
                .unwrap_or(0),
            completion_tokens: response
                .usage
                .as_ref()
                .map(|u| u.completion_tokens)
                .unwrap_or(0),
            // 前缀缓存命中数必须与 prompt 同排出现，否则「这一轮是冷启动
            // 还是正常命中」只能事后翻库（2026-10-30 实测：冷启动的
            // cache_read 恒为 184，而 prompt 是 22 万）。
            cache_read_tokens: response
                .usage
                .as_ref()
                .map(|u| u.cache_read_tokens)
                .unwrap_or(0),
            cache_creation_tokens: response
                .usage
                .as_ref()
                .map(|u| u.cache_creation_tokens)
                .unwrap_or(0),
            context_tokens: ctx.total_tokens() as u64,
            context_max_tokens: ctx.max_context_tokens() as u64,
            // 压缩阈值：与 `context_tokens` 一起看，才知道「离压缩还有多远」。
            // 长窗口模型上这个值可能是 94 万，于是 22 万的会话永远不压缩 ——
            // 这正是 2026-10-30 观测到的上下文膨胀形态。
            context_threshold_tokens: ctx.pressure_threshold_tokens() as u64,
            // provider 回显的模型：与 `config.model` 不一致时，能一眼看出是
            // 会话覆盖 / 路由 / 归一化中的哪一环。
            model: response.model.clone(),
            outcome: crate::turn_report::TurnOutcome::Continue,
            mode: None,
            task_id: None,
        }
        .emit();

        ToolTurnOutcome::Continue
    }
}

/// 收敛硬停时写给用户看的一句话：为什么停、怎么接着走。
///
/// 硬停以前完全静默：`finalize_agent_result` 只带已有消息，最后一条
/// assistant 正文往往是空的，界面就停在「正在回复…」，用户唯一的知情
/// 途径是手动发一句「继续」去问。这条消息把答案直接放进对话里。
fn convergence_wrap_up(
    reason: &crate::convergence_detector::ConvergenceReason,
    count: u32,
) -> String {
    use crate::convergence_detector::ConvergenceReason;
    let head = match reason {
        ConvergenceReason::NoProgressWithTools => format!(
            "我停在这里了：连续 {count} 轮只在调用工具、没有向你汇报，触发了「无进展」保护。"
        ),
        ConvergenceReason::ErrorLoop => format!(
            "我停在这里了：连续 {count} 轮工具调用全部失败，触发了「死循环」保护。"
        ),
        ConvergenceReason::SimilarContent => format!(
            "我停在这里了：连续 {count} 轮输出高度重复，触发了「重复内容」保护。"
        ),
    };
    format!(
        "{head}\n要接着做，回一句「继续」就行（或直接说下一步方向）；\
         上面工具卡里是已经完成的部分。"
    )
}
