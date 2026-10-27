//! WebSocket stream infrastructure: send, event bridging (stdio 通道复用)。
//!
//! 裸 WebSocket 路由（`/v1/ws`、`/v1/ws-stream`）随 2026-09-26 端点审计
//! 批次删除；本模块保留 stdio_session 复用的流转发/心跳/断连宽限基础设施。

use std::sync::Arc;

use hermix_tools::extract_trust_signature;
use super::*;
use crate::orchestration_bus::OrchestrationEvent;
use crate::transport::{Transport, TransportSink, TransportSource};


mod broadcast;
mod dedup;
mod inbound;
mod lookup;
mod outbound;

use broadcast::{next_envelope_seq, session_broadcast_drop, session_broadcast_get_or_create};
pub(crate) use dedup::{check_recent_send, dedup_key};
// `DEDUP_WINDOW_SECS` 的唯一 crate 内消费者是 approval.rs 的 #[cfg(test)]
// 用例（窗口边界断言）；非 test 编译下这个 re-export 空闲。
#[allow(unused_imports)]
pub(crate) use dedup::DEDUP_WINDOW_SECS;
use inbound::{approval_outcome_for, handle_inbound_frame};
pub(crate) use lookup::{lookup_pending_approval, lookup_pending_resource};
use outbound::{push_model_list, task_in_flight_status, todo_change_to_stream};

/// Non-streaming request-reply handler.
///
/// approval_fallback is passed in rather than hardcoded because this
/// function serves transports with different exposure:
///
/// - stdio channel (stdio_session.rs, the only caller) is a local
///   process pipe: the operator already has shell access on the host,
///   so a Dangerous tool call is not a privilege escalation -> Allow.
/// - The LAN-reachable non-streaming HTTP route lives in routes.rs
///   and declares Deny on its own override.
pub(crate) async fn handle_ws(
    transport: impl Transport,
    state: HttpServerState,
    session_id: String,
    approval_fallback: hermix_core::approval::ApprovalFallback,
) {
    let (mut sink, mut source) = transport.split();
    let _ = sink
        .send(format!("connected session={}", session_id))
        .await;
    while let Some(text) = source.recv().await {
        let parsed: Option<SendMessageRequest> = match serde_json::from_str(&text) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(error = %e, "failed to parse WS text as SendMessageRequest");
                None
            }
        };
        let request = parsed.unwrap_or_else(|| SendMessageRequest {
            text: text.clone(),
            model: None,
            provider: None,
            personality: None,
            system_prompt: None,
            tools: None,
            workspace: None,
            permission: None,
            user_id: None,
            request_id: None,
            images: None,
            image: None,
            image_mime_type: None,
            skills: None,
            workbench_id: None,
            domain_override: None,
            expert_toolset: None,
            attached_files: None,
            attached_file_paths: None,
            enable_thinking: None,
        });

        // Non-image documents: bytes → disk, so the agent gets real paths.
        let mut attached_paths = crate::attachments::persist_attached_files(
            request.attached_files.as_deref().unwrap_or(&[]),
        );
        if let Some(direct) = &request.attached_file_paths {
            attached_paths.extend(direct.iter().cloned());
        }
        // Build AgentOverrides from request
        let overrides = hermix_core::traits::AgentOverrides {
            model: request.model.clone(),
            provider: request.provider.clone(),
            personality: request.personality.clone(),
            system_prompt: request.system_prompt.clone(),
            tools: request.tools.clone(),
            workspace: request.workspace.clone(),
            permission: parse_permission_level(request.permission.as_deref()),
            images: convert_wire_images(&request.images),
            image: None,
            image_mime_type: None,
            skills: request.skills.clone(),
            workbench_id: request.workbench_id.clone(),
            domain_override: request.domain_override.clone(),
            expert_toolset: request.expert_toolset.clone(),
            attached_file_paths: (!attached_paths.is_empty())
                .then_some(attached_paths),
            enable_thinking: request.enable_thinking,
            memory_dir: None,
            // An interactive chat turn is by definition user-initiated;
            // only the automation scheduler sets this. Keeping it
            // `None` here is what makes ordinary turns persist as a
            // real `user` message.
            external_delivery: None,
            approval_requester: None,
            on_event: None,
            pre_approval_checker: None,
            // This is the local stdio pipe (see stdio_session.rs, the
            // only caller). It has no remote attack surface for an
            // approval decision, so Dangerous tools proceed. The
            // LAN-reachable non-streaming path is routes.rs, which
            // declares Deny.
            approval_fallback: Some(approval_fallback),
            on_prefetch_intent: None,
            // type-B: see sibling `/api/sessions/.../messages`
            // initializer; no WS → no shutdown token → falls back to
            // plain `rx.await`.
            shutdown: None,
        };

        // Call agent service
        match state
            .agent_service
            .send_message(&session_id, &request.text, overrides)
            .await
        {
            Ok(reply) => {
                let _ = sink.send(reply.text).await;
            }
            Err(err) => {
                let _ = sink.send(err.to_string()).await;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Streaming WebSocket endpoint — real-time Agent events
// ---------------------------------------------------------------------------


/// server-side WS heartbeat cadence.
/// Background: long-running spawn_blocking tools (storage_analyzer
/// walking C:\, code review, plan generation) produce zero
/// application frames for many seconds. WebView2 / OS / NAT
/// middleboxes silently drop idle sockets (60–600s). The forward
/// loop only detects this on the *next* `socket.send` failure —
/// during a long tool there is no next send, so the WS dies
/// invisibly.
/// We emit a WS-protocol Ping every WS_HEARTBEAT_INTERVAL_SECS.
/// Pings are auto-ACKed by the browser at the tungstenite layer
/// (no app message), cost ~2 bytes on the wire, and force every
/// hop to refresh its idle timer. 15s is well below the shortest
/// observed idle-timeout (60s for NAT) and well above the per-send
/// overhead (~5µs).
const WS_HEARTBEAT_INTERVAL_SECS: u64 = 15;
/// 断连宽限期（秒）。检测到 WS 断开（瞬时抖动或真断连）后，不立即
/// 软杀 agent loop，而是延迟 `DISCONNECT_GRACE_SECS` 秒再通知。
/// 宽限期内 agent 继续运行——其事件虽发不出去，但对话已持久化到 DB，
/// 最终状态不丢；前端若在宽限期内重连并重发同一 session，新 send 通过
/// stale_handles 接管正在盲跑的旧 task，任务无缝恢复，无需用户手动发消息。
/// 这是"任务执行中突然停止"的根因修复：把瞬时网络抖动从"立即杀任务"
/// 降级为"延迟确认 + 允许恢复"。
const DISCONNECT_GRACE_SECS: u64 = 90;
/// 转发循环单次发送失败的重试次数。HMR / 网络瞬时抖动会让某次
/// `socket.send` 偶发失败——若直接断开转发循环，agent loop 会因前端
/// 收不到事件而"跑着跑着突然停"。真死连接 send 会快速连续失败，重试
/// 不会挂起；瞬时抖动重试一次多半恢复，循环继续把事件推给前端。
const WS_SEND_RETRY_MAX: u32 = 3;
const WS_SEND_RETRY_DELAY_MS: u64 = 50;

/// Outcome of one `handle_ws_stream_send` call.
///
/// The forward loop reads the same inbound channel it is driven by
/// (approval clicks arrive mid-stream), and `TransportSource` is a plain
/// read cursor — so a frame consumed there cannot be pushed back and has
/// to be handed to the caller instead.
pub(crate) enum StreamSendOutcome {
    /// The turn completed normally; the main loop takes the next frame.
    Completed,
    /// A user message arrived *while the turn was streaming*. Its raw
    /// wire text is carried here and must be re-driven by the caller.
    /// The frontend has already been told to keep waiting via a
    /// `task_in_flight` `Status`.
    DeferredUserMessage(String),
}

pub(crate) async fn handle_ws_stream_send(
    sender: &mut dyn TransportSink,
    receiver: &mut dyn TransportSource,
    state: &HttpServerState,
    session_id: &str,
    trace_id: &str,
    request: SendMessageRequest,
) -> StreamSendOutcome {
    use tokio::sync::mpsc;

    //( / ): replaced unconditional eprintln
    // with tracing::debug! behind the "ws_stream_send" target. In
    // production (default log filter) this produces zero output; enable
    // with RUST_LOG=ws_stream_send=debug to diagnose WS send issues.
    //
    //WS 静默丢消息 — handle_ws_stream_send 永久诊断
    //WebView2 known issue 导致 WS 静默丢消息，需要 entry 点
    //诊断日志来排查。原为 eprintln 永久输出， 改为可关闭的
    //   tracing::debug!。
    //   测试：handle_ws_stream_send_tests（隐式 — 通过 dedup 逻辑覆盖）
    //INFO（原 debug）: 入口日志是排查「发送后一直转圈、消息未落库」的
    // 第一锚点 —— 默认日志级别滤掉 debug 会让现场完全不可见。
    tracing::info!(
        target: "ws_stream_send",
        session = %session_id,
        text_len = request.text.len(),
        text_preview = %&request.text.chars().take(40).collect::<String>(),
        trace_id = %trace_id,
        "handle_ws_stream_send entry"
    );

    // (type-B root-cause): soft-shutdown `Notify` for the
    // agent task. Cloned into `AgentOverrides::shutdown` so the agent
    // loop's `resolve_pre_dispatch` can race the approval oneshot
    // against it. We then `notify_one()` here when the WS is observed
    // disconnected (heartbeat ping fail or socket.send fail) — letting
    // the task self-drain via `stream_first_chunk_timeout_secs=30s`
    // instead of `JoinHandle::abort()` killing it mid-`rx.await`. The
    // stale-handle override above (user-retry path) still hard-aborts
    // because the user explicitly wants to cancel.
    let shutdown_notify: Arc<tokio::sync::Notify> = Arc::new(tokio::sync::Notify::new());

    // user-retry recovery. The key is
    // `dedup_key(session_id, text)` — the same collision domain used by
    // `active_sessions` and `active_handles`.
    // Order of checks (most-specific first):
    // 1. LIVE TASK (override): if the same (session, text) STILL has a
    // spawned agent task, the user has re-sent the original question
    // after the LLM self-interrupted (or the backend task hung
    // without emitting `Done`, so the frontend gave up via its
    // stale-timeout but the lock is still held). This is an explicit
    // retry — abort the stale task, free BOTH dedup locks, and let
    // the new send take over. This is the fix for "LLM stopped on
    // its own; re-sending the question does nothing".
    // 2. RECENT SEND (storm guard): no live task, but identical content
    // arrived within DEDUP_WINDOW_SECS — that's a genuine client
    // retry storm / queue re-drain, still coalesced as before.
    let active_key = dedup_key(session_id, &request.text);

    // Peek without removing: the `!is_finished()` branch below DECLINES this
    // send and must leave the live handle registered. Removing it up front
    // stranded the in-flight task — its cleanup `remove` never runs (the task
    // is still running), so `active_handles` lost the entry permanently and
    // `active_sessions` stayed locked forever. Every later retry then fell
    // into the dedup branch and produced a silent empty reply.
    let stale_handle = {
        let handles = state.active_handles.lock().unwrap_or_else(|e| e.into_inner());
        handles.get(&active_key).map(|h| h.is_finished())
    };
    if let Some(handle_finished) = stale_handle {
        // (autonomous-continuation): same (session, text) has a live
        // task — was the previous forward loop's agent still running
        // when this retry arrived? Two sub-cases:
        //   a. handle is `is_finished()` — the agent already exited
        //      (Done was sent, forward loop is also exited). We treat
        //      this as a zombie handle and abort + free locks so the
        //      new send below can take over the active_sessions slot.
        //      This is the previous behavior.
        //   b. handle is NOT finished — the agent is genuinely still
        //      running. With the autonomous-continuation policy (see
        //      ws_grace.rs), we MUST NOT abort it: doing so would
        //      strand in-flight tool calls and the user-retry message
        //      (which is identical to the in-flight one) doesn't
        //      actually want a new task — it's a retry of a stuck UI.
        //      We therefore decline the new send: surface a Status
        //      event on the CURRENT socket (not a new agent task) so
        //      the front-end knows "this session already has a live
        //      task; the previous one is still progressing — please
        //      wait or click Stop". The DB transcript snapshot the
        //      front-end already pulled at connect shows past turns;
        //      live progress continues via the broadcast channel.
        if !handle_finished {
            tracing::info!(
                session = %session_id,
                text_len = request.text.len(),
                "reconnect-while-running: declining retry — agent task is still in flight (autonomous continuation)"
            );
            // A live re-send proves a client is attached to this session
            // again — the grace timer scheduled when the old socket died
            // is obsolete. Left armed it fires `notify_one()` into the
            // still-running turn and auto-denies an approval wait the
            // task_in_flight hydrate is about to re-show (the zombie
            // takeover branch below aborts it for the same reason).
            ws_grace::abort_pending_grace(&state.pending_grace, &active_key);
            // Send a structured Status describing the situation. The
            // front-end surfaces this as a non-blocking banner (no
            // red error UI). Wording avoids LLM-generated copy —
            // it is a fixed protocol-level message.
            let envelope = WsEnvelope {
                version: 1,
                request_id: request.request_id.clone().or_else(|| Some(trace_id.to_string())),
                trace_id: trace_id.to_string(),
                 seq: None,
                 attachment_id: None,
                event: StreamEvent::Status {
                    content: format!(
                        "{{\"event\":\"task_in_flight\",\"message\":\"此会话已有正在运行的智能体任务（自动继续中）。新消息未触发新任务；请等待当前任务完成或在 UI 中点击「停止」。\"}}"
                    ),
                },
            };
            if let Ok(json) = serde_json::to_string(&envelope) {
                let _ = sender.send(json).await;
            }
            // Status alone is NOT terminal — the front-end's handler only
            // updates the status line and returns, so the send promise never
            // settles and the UI keeps rendering "正在思考…" next to a live
            // task that will never emit into THIS send. A terminal `Done`
            // settles this send cleanly while the in-flight task keeps
            // running and its real output lands via the broadcast channel /
            // the next reconnect.
            let done = WsEnvelope {
                version: 1,
                request_id: request.request_id.clone().or_else(|| Some(trace_id.to_string())),
                trace_id: trace_id.to_string(),
                 seq: None,
                 attachment_id: None,
                event: StreamEvent::Done {
                    content: String::new(),
                    finish_reason: Some("task_in_flight".to_string()),
                    outcome: Some("completed".to_string()),
                    error: None,
                },
            };
            if let Ok(json) = serde_json::to_string(&done) {
                let _ = sender.send(json).await;
            }
            return StreamSendOutcome::Completed;
        }
        tracing::warn!(
            session = %session_id,
            text_len = request.text.len(),
            "retry override: same (session,text) has a zombie handle (agent already exited) — cleaning up and taking over"
        );
        // Take the finished handle out of the registry now — the peek above
        // deliberately left the map untouched so the decline branch could
        // keep a live handle registered.
        let zombie = state
            .active_handles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&active_key);
        // Abort the zombie task (same mechanism as the Stop button's
        // `agent_handle.abort()`). Its cleanup `remove` won't run, so we
        // free the locks here ourselves.
        if let Some(handle) = zombie {
            handle.abort();
        }
        state.active_sessions.lock().unwrap_or_else(|e| e.into_inner()).remove(&active_key);
        state.recent_sends.lock().unwrap_or_else(|e| e.into_inner()).remove(&active_key);
        // Cancel the soft-disconnect's pending grace timer for this key.
        // Otherwise it fires later and `remove`s the NEW handle we're about
        // to insert for this reconnect — orphaning the reconnect's agent and
        // letting a 2nd retry spawn a duplicate task for the same prompt.
        ws_grace::abort_pending_grace(&state.pending_grace, &active_key);
        // P5 多端 attachment（设计 §3.5）：zombie 接管是"本 attachment
        // 自己的重连"语义——只 abort 已完成（zombie）的 handle，不动
        // live handle。第二端（新 attachment_id）发同一条消息时，
        // 第一端的 handle 若仍 live，走上方 `!handle_finished` 拒绝
        // 分支（`task_in_flight` Done + broadcast 镜像扇出），不踢线。
        // 只有当该 attachment 的 handle 已 zombie（`is_finished()`）
        // 才 abort + 接管，让新 send 重新跑 agent task。
        // Fall through: the `active_sessions` insert below re-takes the
        // lock for THIS send.
    } else if let Some(elapsed) = check_recent_send(&state, session_id, &request.text) {
        tracing::warn!(
            session = %session_id,
            text_len = request.text.len(),
            elapsed_secs = elapsed,
            "dedup: identical content sent within window (no live task), skipping (client retry storm or queue re-drain)"
        );
        // Send a synthetic Done so the client settles cleanly
        // (otherwise the WS would hang until the 120s timeout).
        let envelope = WsEnvelope {
            version: 1,
            request_id: request.request_id.clone().or_else(|| Some(trace_id.to_string())),
            trace_id: trace_id.to_string(),
             seq: None,
             attachment_id: None,
            event: StreamEvent::Done {
                content: String::new(),
                finish_reason: Some("deduped".to_string()),
                outcome: Some("completed".to_string()),
                error: None,
            },
        };
        if let Ok(json) = serde_json::to_string(&envelope) {
            let _ = sender.send(json).await;
        }
        return StreamSendOutcome::Completed;
    }

    // Item 2: if this exact (session, text) already COMPLETED and we still
    // hold its final content, the client's `Done` frame was almost certainly
    // lost on the prior disconnect (agent finished but nobody was listening).
    // Serve a synthetic Done (carrying the answer) instead of spawning a fresh
    // agent — re-running would double-execute non-idempotent tools and make
    // the task appear to "run again". The reconnect's `Connected` → resend is
    // exactly the scenario that previously triggered this.
    let replay_content = ws_grace::take_completed_replay(&state.completed_keys, &active_key);
    if let Some(content) = replay_content {
        tracing::info!(
            session = %session_id,
            text_len = request.text.len(),
            "completed-key replay: serving synthetic Done, skipping re-run"
        );
        let envelope = WsEnvelope {
            version: 1,
            request_id: request.request_id.clone().or_else(|| Some(trace_id.to_string())),
            trace_id: trace_id.to_string(),
             seq: None,
             attachment_id: None,
            event: StreamEvent::Done {
                content: content.clone(),
                finish_reason: Some("completed".to_string()),
                outcome: Some("completed".to_string()),
                error: None,
            },
        };
        if let Ok(json) = serde_json::to_string(&envelope) {
            let _ = sender.send(json).await;
        }
        return StreamSendOutcome::Completed;
    }

    // session-level active-stream lock. After the override
    // above freed a stale entry, this insert succeeds for the new send.
    // The key is derived from `dedup_key` so both checks use the same
    // collision domain. This catches retries that arrive AFTER the
    // window (when `check_recent_send` above misses them) and is a
    // safety net for any entry that slipped past the override.
    let is_duplicate = {
        let mut active = state.active_sessions.lock().unwrap_or_else(|e| e.into_inner());
        if !active.insert(active_key.clone()) {
            true
        } else {
            false
        }
    };
    if is_duplicate {
        tracing::warn!(
            session = %session_id,
            text_len = request.text.len(),
            "active_stream: duplicate (session_id, text) still locked; settling as deduped (safety net)"
        );
        // 之前这个分支返回 `StreamEvent::Error`(code=
        // "duplicate_stream")，桌面端把它渲染成红色的「发送失败」横幅——
        // 这是误导：消息并没有失败，它正在被处理。这里改为与上方
        // `check_recent_send` 去重路径一致，发一个合成的 `Done`
        // (finish_reason: "deduped")，让客户端干净地落定这条重复请求，
        // 而原本在跑的 agent 不受影响。去重锁依然生效（重复发送被丢弃），
        // 只是不再伪装成「失败」。
        let envelope = WsEnvelope {
            version: 1,
            request_id: request.request_id.clone().or_else(|| Some(trace_id.to_string())),
            trace_id: trace_id.to_string(),
             seq: None,
             attachment_id: None,
            event: StreamEvent::Done {
                content: String::new(),
                finish_reason: Some("deduped".to_string()),
                outcome: Some("completed".to_string()),
                error: None,
            },
        };
        if let Ok(json) = serde_json::to_string(&envelope) {
            let _ = sender.send(json).await;
        }
        return StreamSendOutcome::Completed;
    }

    // Bounded queue to cap memory under slow clients. The `try_send`
    // drops here are acceptable for high-volume text/thinking (the
    // next chunk catches up). The CRITICAL tool lifecycle events
    // (tool_start/tool_stdout/tool_complete) bypass this cap via a
    // fire-and-forget spawn + `send().await` (see the `delta.extra`
    // fall-through below) so they are never silently dropped — that
    // drop is what stranded the running-tool UI as "中断".
    let queue_cap = env_compat("HERMIX_WS_STREAM_BUFFER", "HERMES_WS_STREAM_BUFFER")
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(128);
    let (tx, mut rx) = mpsc::channel::<WsEnvelope>(queue_cap);

    // Spawn a listener that subscribes to the global clarify broadcast
    // and forwards matching `ClarifyRequest` events to the WS client for
    // this session. Resolution (`ClarifyResponse`) comes back through
    // the main WS message loop in `handle_ws_stream`.
    let mut clarify_rx = state.clarify_subscribe();
    let clarify_tx = tx.clone();
    let clarify_session = session_id.to_string();
    let clarify_trace = trace_id.to_string();
    let clarify_rid = request.request_id.clone();
    tokio::spawn(async move {
        while let Ok(env) = clarify_rx.recv().await {
            // Filter: only forward questions for this session. The
            // dispatcher is global; desktop tabs own their own
            // session id.
            if env.session_id != clarify_session {
                continue;
            }
            let envelope = WsEnvelope {
                version: 1,
                request_id: clarify_rid.clone(),
                trace_id: clarify_trace.clone(),
                 seq: None,
                 attachment_id: None,
                event: StreamEvent::ClarifyRequest {
                    request_id: env.request_id,
                    question: env.question,
                    choices: env.choices,
                },
            };
            // If the client has already closed we just drop the event;
            // the dispatcher still has the responder parked and will
            // time out per `HERMES_CLARIFY_TIMEOUT_SECS`.
            let _ = clarify_tx.send(envelope).await;
        }
    });

    // Orchestration event listener. Bridges the process-wide
    // `OrchestrationEventBus` (Task lifecycle / automation triggers /
    // loop phase / plan confirmation) onto this WS connection. Without
    // this bridge the events died on the bus — `to_stream_event` had
    // no production subscriber, so the desktop client never saw them.
    //
    // Routing: `LoopPhaseChanged` carries a `session_id` — `Some` is
    // filtered to the owning session, `None` (CLI / legacy bootstrap)
    // fans out like the todo bridge below. The remaining variants
    // have no session context (task_id only) and fan out: the desktop
    // is single-user and the front-end filters per active session.
    {
        let orch_tx = tx.clone();
        let orch_session = session_id.to_string();
        let orch_trace = trace_id.to_string();
        let orch_rid = request.request_id.clone();
        let mut orch_rx = state.orchestration_bus.subscribe();
        tokio::spawn(async move {
            loop {
                match orch_rx.recv().await {
                    Ok(ev) => {
                        if let OrchestrationEvent::LoopPhaseChanged {
                            session_id: Some(ref s),
                            ..
                        } = ev
                        {
                            if s != &orch_session {
                                continue;
                            }
                        }
                        let envelope = WsEnvelope {
                            version: 1,
                            request_id: orch_rid.clone(),
                            trace_id: orch_trace.clone(),
                             seq: None,
                             attachment_id: None,
                            event: ev.to_stream_event(),
                        };
                        let _ = orch_tx.send(envelope).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(
                            session = %orch_session,
                            skipped,
                            "orchestration_rx lagged; some orchestration events were dropped"
                        );
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    // Todo event listener. The `todo` tool stamps each `TodoChange`
    // with the **active session id** (set by `agent_loop` via
    // `TodoHandler::set_active_session` at the top of each
    // tool-call batch). We forward only events whose session
    // matches this WS connection's session, so a session-A tool
    // call does not leak into session-B's pane.
    // Rationale for per-session filtering here, not in the
    // front-end: the TodoPanel already keys state by session id,
    // but if we shipped raw events to every connection the React
    // store would still have to discard cross-session updates.
    // Filtering at the transport layer keeps the wire clean and
    // prevents another tab from snooping on a sibling session.
    // `None` session changes (CLI mode / legacy bootstrap)
    // intentionally route to every session — the right fallback
    // when no active session context is available.
    if let Some(todo_tx) = state.todo_event_tx.clone() {
        let todo_inner_tx = tx.clone();
        let todo_session = session_id.to_string();
        let todo_trace = trace_id.to_string();
        let todo_rid = request.request_id.clone();
        let mut todo_rx = todo_tx.subscribe();
        tokio::spawn(async move {
            //per-change
            // `originating_message_id` extracted from the change itself
            // (the `todo` tool stamps it from `set_active_message_id`
            // at the top of the turn) instead of a hardcoded `None`.
            // The TodoHandler is process-wide and shared across all
            // sessions, so we MUST read the id off the event payload
            // — not from a captured local — otherwise a turn in
            // session-B would stamp session-A's id onto its own
            // events.
            loop {
                match todo_rx.recv().await {
                    Ok(change) => {
                        // Per-session filter: drop mismatches
                        // (None = legacy fan-out, always accept).
                        if let Some(ref s) = change.session_id {
                            if s != &todo_session {
                                continue;
                            }
                        }
                        // Translate to stream event using the
                        // change's own session id (not the
                        // listener's) so the TodoPanel always
                        // sees the originating session.
                        let originating = change
                            .session_id
                            .clone()
                            .unwrap_or_else(|| todo_session.clone());
                        // Pull the originating message id off the
                        // change itself. Only `GroupStarted` carries
                        // it; for `Upsert` / `Delete` it's `None` —
                        // the front-end already anchors those by
                        // group_id, not by message_id.
                        let originating_message_id: Option<String> =
                            match &change.change {
                                hermix_tools::tools::todo::TodoChangeKind::GroupStarted {
                                    message_id, ..
                                } => message_id.clone(),
                                _ => None,
                            };
                        let event =
                            todo_change_to_stream(&originating, originating_message_id.as_deref(), change);
                        if let Some(ev) = event {
                            let envelope = WsEnvelope {
                                version: 1,
                                request_id: todo_rid.clone(),
                                trace_id: todo_trace.clone(),
                                 seq: None,
                                 attachment_id: None,
                                event: ev,
                            };
                            let _ = todo_inner_tx.send(envelope).await;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(
                            session = %todo_session,
                            skipped,
                            "todo_rx lagged; some todo events were dropped"
                        );
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    // Non-image documents: bytes → disk, so the agent gets real paths.
    let mut attached_paths = crate::attachments::persist_attached_files(
        request.attached_files.as_deref().unwrap_or(&[]),
    );
    if let Some(direct) = &request.attached_file_paths {
        attached_paths.extend(direct.iter().cloned());
    }
    // Build AgentOverrides from request
    let overrides = hermix_core::traits::AgentOverrides {
        model: request.model.clone(),
        provider: request.provider.clone(),
        personality: request.personality.clone(),
        system_prompt: request.system_prompt.clone(),
        tools: request.tools.clone(),
        workspace: request.workspace.clone(),
        permission: parse_permission_level(request.permission.as_deref()),
        images: convert_wire_images(&request.images),
        image: request.image.clone(),
        image_mime_type: request.image_mime_type.clone(),
        memory_dir: None,
        skills: request.skills.clone(),
        workbench_id: request.workbench_id.clone(),
        domain_override: request.domain_override.clone(),
        expert_toolset: request.expert_toolset.clone(),
        attached_file_paths: (!attached_paths.is_empty())
            .then_some(attached_paths),
        enable_thinking: request.enable_thinking,
        approval_requester: {
            let registry = state.approval_registry.clone();
            Some(Arc::new(move |sess_id: String, req_id: String| {
                registry.request_approval(&sess_id, &req_id)
            }))
        },
        pre_approval_checker: {
            // The pre-approval shortcut consults both the persistent
            // `UserAllowlist` (loaded from
            // `$HERMIX_HOME/approval_allowlist.json` at server
            // startup) and the per-session in-memory bypass map
            // (populated when the user picks "Allow for this
            // session"). A hit on either is enough to skip the WS
            // round-trip.
            // The closure is sync (it holds a `std::sync::Mutex` for
            // both maps — the agent loop calls this on a tokio
            // worker thread) and extracts a tool-specific signature
            // string from the params (e.g. first word for terminal,
            // exact path for write_file, domain for web_extract)
            // via `extract_trust_signature`.
            // The allowlist gates on `tool` so a write_file
            // entry can't accidentally match a terminal call.
            let user_allowlist = state.user_allowlist.clone();
            let session_bypass = state.session_bypass.clone();
            let sid_for_check = session_id.to_string();
            Some(Arc::new(move |tool: &str, params: &serde_json::Value| -> bool {
                let Some(signature) = extract_trust_signature(tool, params) else {
                    return false;
                };
                // Session-bypass first — it has fewer entries (in
                // practice) and avoids the disk-backed allowlist
                // lock when both would match.
                if let Ok(map) = session_bypass.lock() {
                    if let Some(patterns) = map.get(&sid_for_check) {
                        for re in patterns {
                            if re.is_match(&signature) {
                                return true;
                            }
                        }
                    }
                }
                // Persistent allowlist (now tool-gated).
                if let Ok(guard) = user_allowlist.lock() {
                    if let Some(al) = guard.as_ref() {
                        if al.matches(tool, &signature) {
                            return true;
                        }
                    }
                }
                false
            }))
        },
        on_event: {
            let tx = tx.clone();
            let trace = trace_id.to_string();
            let rid = request.request_id.clone();
            let sid = session_id.to_string();
            let persistence = state.session_persistence.clone();
            // Coalesce N parallel `request_approval` events emitted
            // in the same synchronous pre-dispatch pass into one
            // batch envelope — see `coalescer::ApprovalCoalescer`.
            // The single-item `RequestApproval` envelope still flows
            // through `coalescer` for non-batched sessions: with one
            // item the batch carries just that one item under a
            // synthetic batch_id, which the client must handle.
            let coalescer = crate::coalescer::ApprovalCoalescer::with_default_window(
                tx,
                state.approval_registry.clone(),
                rid.clone().unwrap_or_default(),
                trace.clone(),
            );
            Some(Arc::new(move |extra: serde_json::Value| {
                let request_id = extra
                    .get("request_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let tool = extra
                    .get("tool")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let arguments = extra
                    .get("arguments")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let risk = extra
                    .get("risk")
                    .and_then(|v| v.as_str())
                    .unwrap_or("caution")
                    .to_string();
                // LLM-supplied rationale from the tool call's `reason`
                // parameter (terminal tool schema). Surfaced verbatim on
                // the approval card so the user knows what the command
                // is for before deciding.
                let reason = arguments
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                // Backend-computed before/after diff for file-mutating calls
                // (see `hermix_agent::approval_diff`). `None` for shapes that
                // can't be diffed — the client keeps its plain-description
                // fallback.
                let diff = extra
                    .get("diff")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                // Compute the deadline once and share it between the
                // WS envelope (live countdown) and the persisted
                // `ApprovalRecord` (reload countdown). Two separate
                // `compute_approval_deadline_ms()` calls would
                // diverge by a few µs — invisible to the user, but
                // it would mean the reload-rendered countdown
                // doesn't match what they saw live.
                let expires_at_ms = compute_approval_deadline_ms();
                // Hand off to the coalescer. It owns the WS push:
                // batches items emitted within the window into one
                // `RequestApprovalsBatch` envelope, or sends a
                // single-item batch envelope for non-batched cases.
                // The earlier fire-and-forget `tokio::spawn { tx.send }`
                // dance was needed only because `on_event` is a sync
                // closure invoked from a tokio task — `coalescer.push`
                // handles the spawn itself, so the envelope never
                // gets dropped on a momentarily-full channel.
                coalescer.push(
                    sid.clone(),
                    hermix_transport::RequestApprovalItem {
                        request_id: request_id.clone(),
                        tool: tool.clone(),
                        arguments: arguments.clone(),
                        risk: risk.clone(),
                        reason: reason.clone(),
                        expires_at: expires_at_ms,
                        diff: diff.clone(),
                    },
                );
                // Persist the approval row to the session timeline so it
                // can be replayed across reloads. The decision will be
                // filled in by `resolve_approval_message` when the user
                // clicks allow/deny. We don't have the linked assistant
                // id here (the assistant message has not been finalized
                // yet — we're mid-stream), so we leave it as None; the
                // UI will position the card by its DB row order.
                // The deadline uses the SAME value the WS envelope
                // carried — the countdown rendered after a reload
                // must agree with the original countdown the user
                // saw live. Batch path is purely a UI convenience for
                // the live turn; each `request_id` still gets its
                // own row, and `resolve_approval_message` works
                // identically whether the resolve came via a batch
                // client frame or an individual one.
                let record = hermix_agent::session_persistence::ApprovalRecord {
                    request_id: request_id.clone(),
                    tool: tool.clone(),
                    arguments,
                    risk,
                    decision: None,
                    decided_at: None,
                    expires_at: expires_at_ms,
                    linked_assistant_id: None,
                    diff,
                };
                if let Err(e) = persistence.append_approval_message(&sid, &record) {
                    tracing::warn!(
                        error = %e,
                        request_id = %request_id,
                        "failed to persist approval row; UI will not show it across reloads"
                    );
                }
            }))
        },
        // This path wires the approval registry above, so Dangerous tools
        // are always routed to a real card. The fallback is only consulted
        // if a future refactor drops the requester, so fail closed.
        approval_fallback: Some(hermix_core::approval::ApprovalFallback::Deny),
        on_prefetch_intent: {
            let tx = tx.clone();
            let trace = trace_id.to_string();
            let rid = request.request_id.clone();
            Some(Arc::new(move |payload: &str| {
                if let Ok(obj) = serde_json::from_str::<serde_json::Value>(payload) {
                    let intent = obj.get("intent")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let confidence = obj.get("confidence")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0) as f32;
                    let results: Vec<hermix_transport::PrefetchResultEntry> = obj.get("results")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter().filter_map(|r| {
                                serde_json::from_value(r.clone()).ok()
                            }).collect()
                        })
                        .unwrap_or_default();
                    let _ = tx.try_send(WsEnvelope {
                        version: 1,
                        request_id: rid.clone(),
                        trace_id: trace.clone(),
                         seq: None,
                         attachment_id: None,
                        event: StreamEvent::PrefetchIntent {
                            intent,
                            confidence,
                            results,
                        },
                    });
                }
            }))
        },
        // (type-B root-cause): forward the WS-side
        // soft-shutdown `Notify` so the agent loop can race it
        // against the approval oneshot (see
        // `tool_dispatch::resolve_pre_dispatch` and the `Notify` arm
        // of the inner `tokio::select!`).
        shutdown: Some(shutdown_notify.clone()),
        // A WS turn is a human typing. `external_delivery` is set only by
        // the automation scheduler, which calls `send_message`
        // in-process rather than over the socket.
        external_delivery: None,
    };

    let tx_clone = tx.clone();
    let request_id = request.request_id.clone();
    let trace_id_for_stream = trace_id.to_string();
    // Captured by the `on_chunk` closure below so the ThinkBlockParser
    // can route extracted think bodies to the client as
    // `StreamEvent::Thinking` (for the collapsible "思考过程" block
    // in MessageBubble). Without this capture the parsed-out
    // `_think` text was discarded per the previous "think is internal
    //scaffolding" product decision — but 用户 asked to
    // surface it as a foldable block, so we forward it.
    let tx_for_think = tx.clone();
    // One parser per WS stream — its state is meaningless across turn
    // boundaries.  Some providers (M2.7, Qwen, DeepSeek-R1) embed
    // `<think>...</think>` inside `delta.content` rather than a
    // separate reasoning field, and the block can straddle two
    // chunks.  The `agent_loop` parses this internally for its own
    // assistant message bookkeeping, but the *WS-side* text payload
    // we forward to the desktop client is the *raw* delta.content —
    // which means think tags leak into the user's terminal stream.
    // Re-parsing on the server side here lets the desktop client
    // see a clean `Text` event stream with no think blocks.  We
    // deliberately do *not* forward the reasoning text to the
    // client (per product decision: think is internal scaffolding,
    // not user output) — it stays in-process for telemetry.
    let think_parser = Arc::new(std::sync::Mutex::new(ThinkBlockParser::new()));
    // The tool-call XML sanitizer is *stateful*: an opening
    // `<minimax:tool_call>` and its matching `</minimax:tool_call>`
    // may arrive in *different* chunks. The previous regex pass
    // (`strip_tool_call_markers`) only worked on per-chunk strings
    // and would leak a chunk that held the opener with no closer.
    // The stateful `ToolCallSanitizer` (see
    // `hermix_core::tool_call_sanitizer`) is now the authoritative
    // redactor at the WS layer too. The agent loop also runs a
    // per-stream copy, so the redactor is applied *twice* on every
    // chunk — the second pass is a no-op once the text is clean.
    // The double pass is intentional belt-and-suspenders: the
    // agent-loop copy cleans the persisted message, the WS copy
    // cleans the live stream.
    let tool_call_sanitizer = Arc::new(std::sync::Mutex::new(
        hermix_core::ToolCallSanitizer::new(),
    ));
    // Clone the Arc for the `Done` handler below — the on_chunk
    // closure takes ownership of the original.
    let tool_call_sanitizer_owned = tool_call_sanitizer.clone();
    let on_chunk = Arc::new(move |chunk: StreamChunk| {
        // Re-shape the chunk so `delta.content` is think-stripped
        // before we look at it.  We keep `delta.extra` (and any
        // tool_call deltas) intact so the tool/status branches below
        // still see the original payload.
        let mut chunk = chunk;
        if let Some(ref mut delta) = chunk.delta {
            if let Some(ref mut text) = delta.content {
                if !text.is_empty() {
                    let (clean, think) = think_parser.lock().unwrap_or_else(|e| e.into_inner()).feed(text);
                    // forward the extracted think block to
                    // the client as `StreamEvent::Thinking` so the
                    // chat layer can render a collapsible "思考过程"
                    // block. `ThinkBlockParser::feed` returns the body
                    // (without `<think>` markers) once a closing tag
                    // is seen, or empty when the block is still
                    // streaming. We only emit when non-empty to avoid
                    // spamming the client with empty thinking events.
                    if !think.is_empty() {
                        let _ = tx_for_think.try_send(WsEnvelope {
                            version: 1,
                            request_id: request_id.clone(),
                            trace_id: trace_id_for_stream.clone(),
                             seq: None,
                             attachment_id: None,
                            event: StreamEvent::Thinking {
                                content: redact_sensitive(&think),
                            },
                        });
                    }
                    if clean.is_empty() {
                        delta.content = None;
                    } else {
                        *text = clean;
                    }
                }
            }
        }
        if let Some(delta) = &chunk.delta {
            // 1. Text content (LLM response, already think-stripped
            // by the local `think_parser` above).  We then run
            // the *stateful* tool-call sanitizer so a
            // `<minimax:tool_call>…</minimax:tool_call>` block
            // split across chunks is fully suppressed (the
            // previous per-chunk regex would leak the tail of any
            // block whose opener had not yet been seen).  The
            // redactor also strips the bare `minimax:tool_call`
            // literal and orphan closing tags, so even a malformed
            // LLM response is sanitised.
            if let Some(text) = &delta.content {
                if !text.is_empty() {
                    let mut guard = tool_call_sanitizer.lock().unwrap_or_else(|e| e.into_inner());
                    let cleaned = guard.feed(text);
                    drop(guard);
                    let _ = tx_clone.try_send(WsEnvelope {
                        version: 1,
                        request_id: request_id.clone(),
                        trace_id: trace_id_for_stream.clone(),
                         seq: None,
                         attachment_id: None,
                        event: StreamEvent::Text {
                            content: redact_sensitive(&cleaned),
                        },
                    });
                }
            }
            // 2. Tool lifecycle / stdout / status events (delta.extra)
            if let Some(extra) = &delta.extra {
                if let Some(event_type) = extra.get("event").and_then(|e| e.as_str()) {
                    let envelope = match event_type {
                        "tool_start" => {
                            let tool = extra.get("tool")
                                .and_then(|t| t.as_str())
                                .unwrap_or("tool")
                                .to_string();
                            let content = extra.get("arguments").map(|a| a.to_string()).unwrap_or_default();
                            let tool_call_id = extra.get("tool_call_id")
                                .and_then(|t| t.as_str())
                                .map(String::from)
                                .unwrap_or_else(|| format!("{}-{}", tool, uuid::Uuid::new_v4()));
                            WsEnvelope {
                                version: 1,
                                request_id: request_id.clone(),
                                trace_id: trace_id_for_stream.clone(),
                                 seq: None,
                                 attachment_id: None,
                                event: StreamEvent::ToolStart { tool_call_id, tool, content },
                            }
                        }
                        "tool_stdout" => {
                            let tool = extra.get("tool")
                                .and_then(|t| t.as_str())
                                .unwrap_or("tool")
                                .to_string();
                            let content = extra.get("content")
                                .and_then(|c| c.as_str())
                                .unwrap_or("")
                                .to_string();
                            let chunk_idx = extra.get("chunk_index")
                                .and_then(|i| i.as_u64())
                                .unwrap_or(0);
                            let chunk_total = extra.get("chunk_total")
                                .and_then(|t| t.as_u64())
                                .unwrap_or(1);
                            let tool_call_id = extra.get("tool_call_id")
                                .and_then(|t| t.as_str())
                                .map(String::from)
                                .unwrap_or_else(|| format!("{}-anon-{}", tool, chunk_idx));
                            WsEnvelope {
                                version: 1,
                                request_id: request_id.clone(),
                                trace_id: trace_id_for_stream.clone(),
                                 seq: None,
                                 attachment_id: None,
                                event: StreamEvent::ToolChunk {
                                    tool_call_id,
                                    tool,
                                    content,
                                    chunk_index: chunk_idx,
                                    chunk_total,
                                },
                            }
                        }
                        "tool_complete" => {
                            let tool = extra.get("tool")
                                .and_then(|t| t.as_str())
                                .unwrap_or("tool")
                                .to_string();
                            let content = extra.get("content")
                                .and_then(|c| c.as_str())
                                .unwrap_or("")
                                .to_string();
                            let tool_call_id = extra.get("tool_call_id")
                                .and_then(|t| t.as_str())
                                .map(String::from)
                                .unwrap_or_else(|| format!("{}-anon", tool));
                            // (A+C product fix): surface a
                            // user-facing denial so the front-end can
                            // show a "this call was blocked" banner
                            // that matches the agent's injected
                            // "tell the user" note. `None` for normal
                            // successes; `"policy"` is a silent gate
                            // the LLM already knows about.
                            let denial_kind = extra
                                .get("denial_kind")
                                .and_then(|d| d.as_str())
                                .map(String::from);
                            let is_error = extra.get("is_error").and_then(|e| e.as_bool());
                            WsEnvelope {
                                version: 1,
                                request_id: request_id.clone(),
                                trace_id: trace_id_for_stream.clone(),
                                 seq: None,
                                 attachment_id: None,
                                event: StreamEvent::ToolComplete { tool_call_id, tool, content, denial_kind, is_error },
                            }
                        }
                        "file_change" => {
                            // File-mutation summary event. Routed
                            // straight to the front-end as a
                            // StreamEvent::FileChange so the chat UI
                            // can render the "文件变更 (N个)" row.
                            // The server is the single source of
                            // truth for what changed on disk during
                            // this turn; the front-end never tries
                            // to infer this from tool output.
                            let file_path = extra.get("file_path")
                                .and_then(|p| p.as_str())
                                .unwrap_or("")
                                .to_string();
                            if file_path.is_empty() {
                                // Skip malformed events silently;
                                // log via tracing so the issue is
                                // visible in sidecar diagnostics.
                                // We can't `continue` from inside the
                                // on_chunk closure, so we just
                                // fall through to the next match arm
                                // by returning Unit early.
                                tracing::warn!("file_change event missing file_path; ignoring");
                                return;
                            }
                            let change_type = extra.get("change_type")
                                .and_then(|t| t.as_str())
                                .unwrap_or("modified")
                                .to_string();
                            let file_summary = extra.get("file_summary")
                                .and_then(|s| s.as_str())
                                .map(String::from);
                            let tool_call_id = extra.get("tool_call_id")
                                .and_then(|t| t.as_str())
                                .map(String::from)
                                .unwrap_or_else(|| {
                                    format!("file_change::{}", file_path)
                                });
                            WsEnvelope {
                                version: 1,
                                request_id: request_id.clone(),
                                trace_id: trace_id_for_stream.clone(),
                                 seq: None,
                                 attachment_id: None,
                                event: StreamEvent::FileChange {
                                    tool_call_id,
                                    file_path,
                                    change_type,
                                    file_summary,
                                },
                            }
                        }
                        "thinking" => {
                            // Live reasoning chunk from the LLM
                            // (think-block body). Routed to the
                            // frontend as `StreamEvent::Thinking`,
                            // which the chat layer appends to
                            // `message.reasoningContent` for the
                            // collapsible "思考过程" block in
                            // MessageBubble. Without this dispatch
                            // arm the user never sees the model
                            // thinking live — it only became
                            // visible on session reload via the
                            // persisted `reasoning_content` field.
                            let content = extra.get("content")
                                .and_then(|c| c.as_str())
                                .unwrap_or("")
                                .to_string();
                            if content.is_empty() {
                                return;
                            }
                            WsEnvelope {
                                version: 1,
                                request_id: request_id.clone(),
                                trace_id: trace_id_for_stream.clone(),
                                 seq: None,
                                 attachment_id: None,
                                event: StreamEvent::Thinking {
                                    content: redact_sensitive(&content),
                                },
                            }
                        }
                        "status" => {
                            let kind = extra.get("kind")
                                .and_then(|k| k.as_str())
                                .unwrap_or("status");
                            let content = extra.get("content")
                                .and_then(|c| c.as_str())
                                .unwrap_or("")
                                .to_string();
                            // Map to proper StreamEvent variant so the
                            // front-end can display it as a status chip
                            // instead of injecting it into message text.
                            let event = match kind {
                                "activity" => StreamEvent::Activity { content },
                                _ => StreamEvent::Status { content },
                            };
                            WsEnvelope {
                                version: 1,
                                request_id: request_id.clone(),
                                trace_id: trace_id_for_stream.clone(),
                                 seq: None,
                                 attachment_id: None,
                                event,
                            }
                        }
                        "context_usage" => {
                            let used = extra.get("used_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            let max = extra.get("max_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            let threshold = extra.get("threshold_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            let percent = extra.get("percent")
                                .and_then(|v| v.as_f64())
                                .unwrap_or(0.0);
                            WsEnvelope {
                                version: 1,
                                request_id: request_id.clone(),
                                trace_id: trace_id_for_stream.clone(),
                                 seq: None,
                                 attachment_id: None,
                                event: StreamEvent::ContextUsage {
                                    used_tokens: used,
                                    max_tokens: max,
                                    threshold_tokens: threshold,
                                    percent,
                                },
                            }
                        }
                        "compaction_status" => {
                            let snapshot = extra.get("snapshot")
                                .cloned()
                                .unwrap_or(serde_json::Value::Null);
                            WsEnvelope {
                                version: 1,
                                request_id: request_id.clone(),
                                trace_id: trace_id_for_stream.clone(),
                                 seq: None,
                                 attachment_id: None,
                                event: StreamEvent::CompactionStatus { snapshot },
                            }
                        }
                        "usage_update" => {
                            // Per-turn usage snapshot for the
                            // front-end cache-hit chip. Mirrors the
                            // `context_usage` branch shape but carries the
                            // token breakdown instead of window pressure.
                            // `cache_*` fields default to 0 so providers
                            // that don't report cache stats (e.g. MiniMax
                            // today) still round-trip cleanly.
                            let prompt = extra.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                            let completion = extra.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                            let cache_read = extra.get("cache_read_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                            let cache_creation = extra.get("cache_creation_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                            let reasoning = extra.get("reasoning_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                            WsEnvelope {
                                version: 1,
                                request_id: request_id.clone(),
                                trace_id: trace_id_for_stream.clone(),
                                 seq: None,
                                 attachment_id: None,
                                event: StreamEvent::UsageUpdate {
                                    prompt_tokens: prompt,
                                    completion_tokens: completion,
                                    cache_read_tokens: cache_read,
                                    cache_creation_tokens: cache_creation,
                                    reasoning_tokens: reasoning,
                                },
                            }
                        }
                        "section_gate" | "prefetch_intent" | "acui" => {
                            // Forward these structured events as raw
                            // JSON so the frontend's `useStreamChat`
                            // handler can parse them. We wrap the entire
                            // `extra` object as the WS event payload.
                            let raw = extra.clone();
                            WsEnvelope {
                                version: 1,
                                request_id: request_id.clone(),
                                trace_id: trace_id_for_stream.clone(),
                                 seq: None,
                                 attachment_id: None,
                                event: StreamEvent::Status {
                                    content: raw.to_string(),
                                },
                            }
                        }
                        "memory_update" => {
                            // surface memory writes/updates/
                            // deletes/summaries in the chat so the user can
                            // see what the agent is remembering. Mirrors the
                            // `usage_update` branch shape but carries the
                            // memory-operation fields instead.
                            let action = extra
                                .get("action")
                                .and_then(|v| v.as_str())
                                .unwrap_or("write")
                                .to_string();
                            let memory_type = extra
                                .get("memory_type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("fact")
                                .to_string();
                            let key = extra
                                .get("key")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let summary = extra
                                .get("summary")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let count = extra.get("count").and_then(|v| v.as_u64());
                            WsEnvelope {
                                version: 1,
                                request_id: request_id.clone(),
                                trace_id: trace_id_for_stream.clone(),
                                 seq: None,
                                 attachment_id: None,
                                event: StreamEvent::MemoryUpdate {
                                    action,
                                    memory_type,
                                    key,
                                    summary,
                                    count,
                                },
                            }
                        }
                        "memory_recall" => {
                            //the agent recalled
                            // N related facts from memory this turn. We
                            // surface a small preview (top-3 keys + count)
                            // to the chat so the user can see "the agent
                            // thought of something". The full recall
                            // content is injected into the system prompt
                            // by `prompt_assembly`, not via this event.
                            let trigger = extra
                                .get("trigger")
                                .and_then(|v| v.as_str())
                                .unwrap_or("interval")
                                .to_string();
                            let count = extra
                                .get("count")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0) as u32;
                            let total_chars = extra.get("total_chars").and_then(|v| v.as_u64());
                            let preview_keys: Vec<String> = extra
                                .get("preview_keys")
                                .and_then(|v| v.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                        .collect()
                                })
                                .unwrap_or_default();
                            let turn_number = extra
                                .get("turn_number")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0) as u32;
                            WsEnvelope {
                                version: 1,
                                request_id: request_id.clone(),
                                trace_id: trace_id_for_stream.clone(),
                                 seq: None,
                                 attachment_id: None,
                                event: StreamEvent::MemoryRecall {
                                    trigger,
                                    count,
                                    total_chars,
                                    preview_keys,
                                    turn_number,
                                },
                            }
                        }
                        "sub_agent_update" => {
                            // Real-time sub-agent (delegate_task) lifecycle
                            // event. Routed to the frontend as
                            // `StreamEvent::SubAgentEvent` so the chat UI can
                            // render a live "子代理" panel (task goal + phase
                            // badge + turns + cost). The payload is produced
                            // by `sub_agent_orchestrator::emit_sub_agent`.
                            let sub_agent_id = extra
                                .get("sub_agent_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            if sub_agent_id.is_empty() {
                                return;
                            }
                            let phase = extra
                                .get("phase")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown")
                                .to_string();
                            let task = extra
                                .get("task")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let role = extra
                                .get("role")
                                .and_then(|v| v.as_str())
                                .map(String::from);
                            let turns = extra
                                .get("turns")
                                .and_then(|v| v.as_u64())
                                .map(|v| v as u32);
                            let cost_usd = extra.get("cost_usd").and_then(|v| v.as_f64());
                            let error = extra
                                .get("error")
                                .and_then(|v| v.as_str())
                                .map(String::from);
                            // Rolling step log — the orchestrator emits it
                            // (sub_agent_orchestrator.rs); forward it so the
                            // front-end panel's `recent_steps` isn't always
                            // undefined.
                            let recent_steps = extra.get("recent_steps").and_then(|v| v.as_array()).map(
                                |arr| {
                                    arr.iter()
                                        .filter_map(|s| s.as_str().map(String::from))
                                        .collect::<Vec<String>>()
                                },
                            );
                            WsEnvelope {
                                version: 1,
                                request_id: request_id.clone(),
                                trace_id: trace_id_for_stream.clone(),
                                 seq: None,
                                 attachment_id: None,
                                event: StreamEvent::SubAgentEvent {
                                    sub_agent_id,
                                    phase,
                                    task,
                                    role,
                                    turns,
                                    cost_usd,
                                    error,
                                    recent_steps,
                                },
                            }
                        }
                        "member_update" => {
                            // 成员工单实时事件（总管方案 P4.4）：runner 的
                            // chunk emitter 产出，转发为 `MemberEvent` 供
                            // 前端「成员」面板滚动显示。与 sub_agent_update
                            // 平行但通道独立（成员 ≠ 子代理，D0）。
                            let member_id = extra
                                .get("member_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            if member_id.is_empty() {
                                return;
                            }
                            let phase = extra
                                .get("phase")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown")
                                .to_string();
                            let task = extra
                                .get("task")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let turns = extra
                                .get("turns")
                                .and_then(|v| v.as_u64())
                                .map(|v| v as u32);
                            let error = extra
                                .get("error")
                                .and_then(|v| v.as_str())
                                .map(String::from);
                            let recent_steps = extra
                                .get("recent_steps")
                                .and_then(|v| v.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|s| s.as_str().map(String::from))
                                        .collect::<Vec<String>>()
                                });
                            WsEnvelope {
                                version: 1,
                                request_id: request_id.clone(),
                                trace_id: trace_id_for_stream.clone(),
                                 seq: None,
                                 attachment_id: None,
                                event: StreamEvent::MemberEvent {
                                    member_id,
                                    phase,
                                    task,
                                    turns,
                                    error,
                                    recent_steps,
                                },
                            }
                        }
                        _ => {
                            tracing::debug!(event = %event_type, "unknown extra event type; skipping");
                            return;
                        },
                    };
                    // CRITICAL: tool lifecycle + agent events here must
                    // NEVER be silently dropped — a dropped
                    // `tool_complete` strands the frontend running-tool
                    // UI as "中断". The bounded channel's `try_send`
                    // would drop on Full, so instead spawn a task that
                    // owns a sender clone and awaits `send()`: it parks
                    // until the recv loop drains (real backpressure is
                    // the socket write in the recv loop), guaranteeing
                    // delivery. `on_chunk` is a sync closure, hence the
                    // spawn; the send is FIFO-ordered on the channel,
                    // and per-tool start<->complete ordering holds
                    // because they are temporally separated by the
                    // tool's own execution.
                    let tx_spawn = tx_clone.clone();
                    let evt = event_type.to_string();
                    tokio::spawn(async move {
                        if let Err(e) = tx_spawn.send(envelope).await {
                            tracing::warn!(
                                error = %e,
                                event = %evt,
                                "ws tool/agent event dropped: client receiver closed"
                            );
                        }
                    });
                }
            }
        }
    });

    // Call agent service with streaming
    let agent_service = state.agent_service.clone();
    let active_sessions = state.active_sessions.clone();
    let active_handles = state.active_handles.clone();
    let recent_sends = state.recent_sends.clone();
    let session_id_clone = session_id.to_string();
    let request_text = request.text.clone();
    let trace_id_owned = trace_id.to_string();
    let request_id_owned = request.request_id.clone();

    // Track whether the WS client has disconnected. When the user
    // clicks "stop" / switches sessions, the WS closes and the
    // forwarding loop below breaks. Without this guard the spawned
    // agent task continues running, eventually persists the full
    // response to DB, and sends a Done event that nobody reads.
    // On session re-entry the front-end fetches the now-complete
    // message — making it look like the session "resumed" and
    // produced output after being stopped.
    let ws_disconnected = Arc::new(AtomicBool::new(false));
    let active_key_clone = active_key.clone();
    // Clone for the agent task so it can check whether the WS died
    // before or after the agent finished — critical for diagnosing
    // "Done event failed to send" warnings.
    let ws_disconnected_for_task = ws_disconnected.clone();

    //Clone hermix_home + session_persistence for crash dump
    // path inside the agent task's catch_unwind recovery.
    let hermix_home_clone = state.hermix_home.clone();
    let session_persistence_clone = state.session_persistence.clone();
    let completed_keys_clone = state.completed_keys.clone();

    //Clone trace_id for the heartbeat arm before the spawn
    // moves trace_id_owned into the agent task closure.
    let trace_id_heartbeat = trace_id_owned.clone();
    tracing::info!(
        target: "ws_stream_send",
        session = %session_id_clone,
        text_len = request_text.len(),
        "handle_ws_stream_send: spawning agent task"
    );
    let agent_handle = tokio::spawn(async move {
        let agent_task_started = std::time::Instant::now();
        //( / ): wrap the agent call in
        // catch_unwind so a panic in a tool handler or LLM provider
        // doesn't silently kill the WS connection. The panic is
        // converted to a structured Error event that the frontend
        // renders as a user-visible error message, and the session
        // cleanup still runs normally. Requires panic=unwind (set in
        // Cargo.toml [profile.release]).
        let result = AssertUnwindSafe(
            agent_service
                .send_message_stream(&session_id_clone, &request_text, overrides, on_chunk)
        )
        .catch_unwind()
        .await;

        // Convert a caught panic into an AgentError so the existing
        // error-handling path below handles it uniformly.
        let result = match result {
            Ok(inner) => inner,
            Err(panic_payload) => {
                let msg = panic_payload
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic_payload.downcast_ref::<String>().map(|s| s.clone()))
                    .unwrap_or_else(|| "internal panic (unknown payload type)".to_string());
                tracing::error!(
                    session = %session_id_clone,
                    panic_msg = %msg,
                    "agent task panicked — converted to error event for WS client"
                );
                //( / ): Write a crash dump for
                // post-mortem diagnostics. Best-effort — failure to write
                // the dump is logged inside write_crash_dump and does NOT
                //block the error recovery path ( F10).
                let recent_messages = session_persistence_clone
                    .load_session(&session_id_clone)
                    .unwrap_or_default();
                crash_dump::write_crash_dump(
                    &hermix_home_clone,
                    &session_id_clone,
                    &msg,
                    &recent_messages,
                );
                // Not `ToolExecution`'s fault by default: this catch_unwind
                // wraps the whole agent task, so a panic in `apply_overrides`,
                // prompt assembly or persistence lands here too and used to be
                // reported as "Tool execution error" — which sends the reader
                // hunting through the tool layer for a bug that is not there.
                // 2026-10-21: a real `serde_json::Map` IndexMut panic in
                // `apply_overrides` was chased as an MCP problem for three
                // rounds because of that wording.
                Err(hermix_core::AgentError::ToolExecution(format!(
                    "internal error (panic) in the agent pipeline: {msg}. \
                     This is an agent-side bug and NOT necessarily a tool \
                     failure — check the panic/crash dump under \
                     <hermix_home>/logs/ for the exact location."
                )))
            }
        };

        // FIX: Previously, if ws_disconnected was true, we
        // skipped sending the terminal Done/Error event entirely. This
        // caused the frontend to hang or show "interrupted" because it
        // never received a closing signal. Now, we always attempt to send
        // the terminal event (best-effort). If the socket is dead, the
        // send fails silently, but the session reload from DB will still
        // show accurate status via execution_timeline.
        
        // Always release session/active locks and recent_sends entry,
        // regardless of whether WS disconnected. Without this, an aborted
        // agent task after ws_close_frame would leave the session locked
        // forever, blocking subsequent messages.
        {
            let mut active_s = active_sessions.lock().unwrap_or_else(|e| e.into_inner());
            active_s.remove(&active_key_clone);
            let mut active_h = active_handles.lock().unwrap_or_else(|e| e.into_inner());
            active_h.remove(&active_key_clone);
            let mut rs = recent_sends.lock().unwrap_or_else(|e| e.into_inner());
            rs.remove(&active_key_clone);
        }
        // Drop the per-session broadcast sender so any still-attached
        // reconnect forwarder (from a stale-handle takeover) sees
        // `Closed` on its next `recv()` and exits cleanly. Must happen
        // AFTER the lock-release block above: `session_broadcast_drop`
        // also takes the broadcast registry mutex, and concurrent
        // forking could otherwise observe a half-released session.
        session_broadcast_drop(&session_id_clone);

        // If the socket is dead, the send will fail silently;
        // log it for diagnostics. The frontend will get
        // the result by reloading the session from DB.

        match result {
            Ok(reply) => {
                // Apply the stateful tool-call redactor to strip
                // `<minimax:tool_call>...</minimax:tool_call>` XML that
                // some models emit instead of using the OpenAI `tools`
                // field. We do NOT strip <think> blocks here — the
                // frontend's resolveMessage handles that split for
                // display. We send the LLM's raw content as-is.
                let cleaned = {
                    let mut guard = tool_call_sanitizer_owned.lock().unwrap_or_else(|e| e.into_inner());
                    guard.flush();
                    guard.feed(&reply.text)
                };
                // Always send Done — never skip it. Even when content
                // is empty, the front-end needs the terminal event to
                // settle the stream (otherwise it hangs until the 120s
                // timeout). The message was already persisted to the DB
                // by `persist_session_with_compression` before we get
                // here, so the row exists regardless.
                let done_content = redact_sensitive(&cleaned);
                // Item 2: record the final answer for this (session, text) so a
                // later reconnect that lost the `Done` frame can short-circuit
                // to a synthetic Done instead of re-running the task.
                let task_completed =
                    reply.finish_reason.as_deref() != Some("error") && !reply.interrupted;
                if task_completed {
                    ws_grace::record_completed(&completed_keys_clone, &active_key_clone, done_content.clone());
                }
                let done_finish_reason = reply.finish_reason.clone();
                // A backend LLM/transport failure is surfaced as a
                // structured `Error` event so the frontend shows a red
                // "出错了" banner + error prompt, instead of silently
                // displaying the partial/last content as a completed
                // turn. A user-initiated interrupt (`interrupted`) or
                // any other reason still uses the normal `Done` event.
                if reply.finish_reason.as_deref() == Some("error") {
                    // Classify the raw backend error so the frontend
                    // shows a specific, actionable message instead of a
                    // generic "backend error" banner. `last_error` carries
                    // the provider's raw response (e.g. MiniMax 529
                    // overloaded_error), which `user_facing_agent_error`
                    // turns into (code, message) pairs like
                    // ("provider_overloaded", "AI 服务过载: ...").
                    let raw_error = reply.last_error.clone().unwrap_or_default();
                    let (code, message) = user_facing_agent_error(&raw_error);
                    if let Err(e) = tx
                        .send(WsEnvelope {
                            version: 1,
                            request_id: request_id_owned.clone(),
                            trace_id: trace_id_owned.clone(),
                             seq: None,
                             attachment_id: None,
                            event: StreamEvent::Error {
                                code,
                                message: redact_sensitive(&message),
                                detail: Some(raw_error),
                            },
                        })
                        .await
                    {
                        let elapsed_ms = agent_task_started.elapsed().as_millis();
                        tracing::warn!(
                            session = %session_id_clone,
                            error = %e,
                            elapsed_ms = elapsed_ms,
                            "Error event (backend failure) failed to send — WS already closed, \
                             user will not see this error in chat"
                        );
                    }
                } else if let Err(e) = tx
                    .send(WsEnvelope {
                        version: 1,
                        request_id: request_id_owned.clone(),
                        trace_id: trace_id_owned.clone(),
                         seq: None,
                         attachment_id: None,
                        event: StreamEvent::Done {
                            content: done_content,
                            finish_reason: done_finish_reason.clone(),
                            outcome: Some(if reply.interrupted
                                || reply.finish_reason.as_deref() == Some("error")
                            {
                                "interrupted".to_string()
                            } else {
                                "completed".to_string()
                            }),
                            // Always surface the raw backend error (e.g.
                            // an LLM 401/402/429 body) so the front-end
                            // can show it as a collapsible "技术详情".
                            // The finish_reason=error branch above already
                            // emitted a separate `Error` event carrying a
                            // classified code (quota_exhausted /
                            // provider_overloaded / auth_error /
                            // agent_error); this `Done.error` ensures the
                            // same detail rides along on the terminal
                            // event regardless of which branch the
                            // front-end takes.
                            error: reply.last_error.clone(),
                        },
                    })
                    .await
                {
                    //( defense): tx.send failure means
                    // the client's WS reader side dropped (the forward
                    // loop exited). Log a warning with diagnostics so we
                    // can determine: (a) how long the agent ran before
                    // the channel died, (b) whether the WS disconnect
                    // was already detected by the forward loop (if true,
                    // the client closed first; if false, the agent
                    // finished faster than the forward loop could detect
                    // the disconnect — a race condition).
                    let elapsed_ms = agent_task_started.elapsed().as_millis();
                    let ws_was_disconnected = ws_disconnected_for_task
                        .load(std::sync::atomic::Ordering::SeqCst);
                    // Log level reflects the diagnostic interest:
                    //   * WS already closed (client disconnect / network
                    //     drop / laptop sleep) → the agent finished its
                    //     turn but no one was listening. The next time
                    //     the client reconnects they get the persisted
                    //     messages anyway. This is *expected* during
                    //     background runs, so log at debug to avoid
                    //     log spam on every dormant session.
                    //   * WS still open (ws_was_disconnected=false) →
                    //     the mpsc channel was closed by the forward
                    //     loop while the WS itself was still up — that's
                    //     a genuine race worth surfacing at WARN.
                    if ws_was_disconnected {
                        tracing::debug!(
                            session = %session_id_clone,
                            error = %e,
                            elapsed_ms = elapsed_ms,
                            "Done event missed — client already disconnected, \
                             persisted transcript still reaches them on next reconnect"
                        );
                    } else {
                        tracing::warn!(
                            session = %session_id_clone,
                            error = %e,
                            elapsed_ms = elapsed_ms,
                            ws_was_disconnected = ws_was_disconnected,
                            "Done event failed to send — client may not have received terminal event. \
                             If ws_was_disconnected=true, the WS closed before the agent finished \
                             (client disconnect or network drop). If false, the agent finished but \
                             the mpsc channel was already closed by the forward loop (race)."
                        );
                    }
                }
            }
            Err(e) => {
                // keep the raw error in the log (for debugging)
                // but send the user a classified, actionable message so
                // quota / auth failures aren't a mystery. The original
                // `e.to_string()` survives in the WARN below.
                tracing::warn!(
                    session = %session_id_clone,
                    error = %e,
                    "agent task failed; classifying for user-facing message"
                );
                let (code, message) = user_facing_agent_error(&e.to_string());
                if let Err(send_err) = tx
                    .send(WsEnvelope {
                        version: 1,
                        request_id: request_id_owned.clone(),
                        trace_id: trace_id_owned.clone(),
                         seq: None,
                         attachment_id: None,
                        event: StreamEvent::Error {
                            code,
                            message: redact_sensitive(&message),
                            detail: Some(e.to_string()),
                        },
                    })
                    .await
                {
                    // The rx forward loop has already exited
                    // (likely because the client closed the WS). The error
                    // event is lost — the user won't see it in the chat.
                    // Log this so we can diagnose "error silently eaten"
                    // from post-mortem logs instead of guessing.
                    tracing::warn!(
                        session = %session_id_clone,
                        error = %send_err,
                        user_message = %message,
                        elapsed_ms = agent_task_started.elapsed().as_millis(),
                        ws_was_disconnected = ws_disconnected_for_task
                            .load(std::sync::atomic::Ordering::SeqCst),
                        "Error event FAILED to send — WS already closed, user will not see this error in chat"
                    );
                }
            }
        }

        // Release the per-session stream lock so future sends
        // (same or different content) for this session are allowed.
        active_sessions.lock().unwrap_or_else(|e| e.into_inner()).remove(&active_key_clone);
        // Self-clean the live-task handle. With the autonomous-
        // continuation policy the graceful-disconnect timer no longer
        // unconditionally removes this entry (doing so while the agent
        // is still running would open a double-spawn window on
        // reconnect), so the agent must release its own slot on
        // natural completion.
        active_handles.lock().unwrap_or_else(|e| e.into_inner()).remove(&active_key_clone);
        // drop the recent_sends entry on stream completion
        // (success OR error). Otherwise a failed send ("LLM busy, try
        // later") leaves the text in the dedup map for DEDUP_WINDOW_SECS
        // (30s), and the user's legitimate retry is silently dropped —
        // they saw no response at all. In-flight protection is already
        // covered by active_sessions; recent_sends only needs to guard
        // the brief pre-insert race window.
        recent_sends.lock().unwrap_or_else(|e| e.into_inner()).remove(&active_key_clone);
    });

    // register the live task handle OUTSIDE the spawned
    // future (the handle does not exist inside its own closure) so a
    // user retry of the same (session, text) can abort it if it later
    // hangs without sending a terminal `Done` event. This runs
    // synchronously right after `tokio::spawn`, before the task body
    // executes, so the cleanup `remove` below always finds the entry.
    // A fresh agent is starting for this key — invalidate any prior
    // "completed" shortcut so a deliberate re-run supersedes it.
    ws_grace::invalidate_completed(&state.completed_keys, &active_key);
    state.active_handles.lock().unwrap_or_else(|e| e.into_inner()).insert(active_key.clone(), agent_handle);

    // Forward channel events to WebSocket
    tracing::info!(
        session = %session_id,
        "ws_stream: rx forward loop starting"
    );

    // server-side WS heartbeat.
    // Background: storage_analyzer (and similar spawn_blocking tools)
    // can run for many seconds with **zero application-level frames**
    // on the wire. WebView2 / OS / NAT / antivirus middleboxes have
    // idle disconnects (60–600s) that close the socket silently from
    // our side. The forward loop only learns about the disconnect
    // when the next `socket.send` fails — but during a long tool
    // there is no next send.
    // Fix: drive the forward loop with `tokio::select!` and emit a
    // WS-protocol Ping every WS_HEARTBEAT_INTERVAL_SECS. Pings are
    // auto-ACKed by the browser at the tungstenite layer (never reach
    // the app message stream), cost ~2 bytes on the wire, and force
    // every hop in the path to refresh its idle timer. If the client
    // is dead, the heartbeat's `socket.send` fails the same way as a
    // data send — we hit the existing disconnect cleanup.
    // Why a select arm instead of a separate spawned ticker:
    // - Single owner of `socket` → no `&mut socket` borrowing fight.
    // - When the rx loop exits, the select drops the heartbeat future
    // on the floor automatically — no extra JoinHandle to cancel.
    // - `biased` keeps events from starving under heavy heartbeat
    // cadence (every tick prefers the rx arm first).
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(
        WS_HEARTBEAT_INTERVAL_SECS,
    ));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // First tick is immediate — burn it so we don't ping 0s after
    // connection (the client just got "connected session=..." text,
    // an immediate extra ping is noise).
    heartbeat.tick().await;

    let mut was_client_disconnect = false;
    // (diagnostic): every arm of the rx-forward loop's
    // `select!` (send-fail, heartbeat-fail, ws close/err/none, rx
    // none) sets the same `was_client_disconnect` flag and the same
    // generic exit INFO line — making it impossible to tell from a
    // post-mortem log whether the stream died because the **client**
    // closed the socket or because a **single send** hit a transient
    // ws error. The next "run suddenly stops mid-stream" investigation
    // (master: 跑着就停了) needs that disambiguation. Track which
    // arm we exited through and surface it on the EXITED line.
    let exit_branch: &'static str;

    // Set when a user message arrives through the concurrent read arm
    // below while this turn is still streaming. `receiver` is a read
    // cursor, so the frame is consumed there and cannot be pushed back —
    // the raw text is handed to the caller, which re-drives the main loop
    // on it once this turn returns.
    let mut pending_user_msg: Option<String> = None;

    // 优雅断连：延迟软杀 agent loop。实现细节见
    // `ws_grace::schedule_grace_disconnect`：立即释放 active_sessions
    // （让前端重连的 send 不被 is_duplicate 卡死），grace 后才 notify
    // + 清理 active_handles。exit_branch / was_client_disconnect /
    // ws_disconnected 由调用方在调用前赋值（AtomicBool 是 Copy，闭包
    // 内 store 会写副本而非原值；exit_branch 是 &'static str，闭包内
    // 改写会与循环后读取冲突），模块函数只负责"延期清理"。
    let schedule_grace_disconnect = || {
        ws_grace::schedule_grace_disconnect(
            &state.active_sessions,
            &state.active_handles,
            &state.pending_grace,
            &active_key,
            shutdown_notify.clone(),
            DISCONNECT_GRACE_SECS,
        );
    };
    loop {
        tokio::select! {
            biased;
            ev = rx.recv() => {
                match ev {
                    Some(mut event_json) => {
                        // Phase2：单次发送失败先重试，避免一次抖动就断开
                        // 转发循环。真死连接 send 会快速连续失败，重试不会
                        // 挂起；瞬时抖动重试后多半恢复，事件继续推给前端。
                        let mut send_ok = false;
                        for _attempt in 0..WS_SEND_RETRY_MAX {
                            if sender
                                .send(serde_json::to_string(&event_json).unwrap_or_default())
                                .await
                                .is_ok()
                            {
                                send_ok = true;
                                break;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(
                                WS_SEND_RETRY_DELAY_MS,
                            ))
                            .await;
                        }
                        if !send_ok {
                            exit_branch = "rx_send_fail";
                            was_client_disconnect = true;
                            ws_disconnected.store(true, std::sync::atomic::Ordering::SeqCst);
                            // (type-B root-cause): 重试耗尽仍失败，不再硬杀
                            // agent loop。交给 schedule_grace_disconnect
                            // 延期清理——立即释放 active_sessions（前端
                            // 重连的 send 走 takeover 路径 abort 旧
                            // 任务），grace 秒后才 notify 软杀 + 清理
                            // active_handles。瞬时 WS 抖动不再让任务
                            // "跑着跑着突然停"。
                            schedule_grace_disconnect();
                            break;
                        }
                        // A terminal event completes the turn. Exit on it
                        // instead of waiting for `rx` to close: this function
                        // clones `tx` into long-lived listeners (the clarify
                        // broadcast forwarder and the todo event listener,
                        // both of which run until the process dies), so `rx`
                        // never observes all senders dropped and `recv()`
                        // never returns `None`. Waiting on that would pin this
                        // function — and everything it holds, including a
                        // deferred user message — forever. Checked before the
                        // broadcast mirror below because that call consumes
                        // the envelope.
                        let is_terminal = matches!(
                            event_json.event,
                            StreamEvent::Done { .. } | StreamEvent::Error { .. }
                        );

                        // Mirror the envelope to any reconnecting subscribers
                        // (each connection spawns its own forward task holding
                        // a receiver clone, so a reconnect can pick up mid-turn).
                        // Non-blocking:
                        // a slow subscriber gets `Lagged` on its next recv
                        // and the dropped events are re-pulled from the DB
                        // transcript. No subscriber = no-op (the broadcast
                        // channel drops the event without buffering).
                        // P4 L3：镜像发送点统一取号（设计 §3.5）——
                        // 每条被镜像的事件帧都分配一个进程级单调 seq，
                        // 重连订阅端点（P5）凭 since_seq 对账补发。
                        // 取号在发送前做（seq 是帧的字段，必须在 send 前
                        // 嵌入 envelope；失败时 seq 也被消耗，允许间隙
                        // ——客户端只做大小比较，不要求连续）。
                        if event_json.seq.is_none() {
                            let seq = next_envelope_seq(&state);
                            event_json.seq = Some(seq);
                        }
                        let _ = session_broadcast_get_or_create(session_id)
                            .send(event_json);

                        if is_terminal {
                            exit_branch = "rx_terminal_event";
                            break;
                        }
                    }
                    None => {
                        exit_branch = "rx_none";
                        // rx channel closed — the agent task finished its
                        // terminal events and there is no more data to send.
                        // Exit the loop and let the WS close naturally.
                        break;
                    }
                }
            }
            _ = heartbeat.tick() => {
                tracing::debug!(
                    session = %session_id,
                    "ws_stream: heartbeat ping"
                );
                //: send BOTH a WS-protocol Ping and
                // an application-level Heartbeat text frame. The Ping
                // refreshes middlebox/NAT idle timers and lets the
                // server detect dead connections when the send fails.
                // But browser WebSocket JS API never surfaces pings to
                // `onmessage` — so the client's 300s IDLE/stale timers
                // are never reset by pings alone. The Heartbeat text
                // frame fires `onmessage`, which resets the client
                // timers (the reset happens at the top of the handler,
                // before event-type dispatch). Together: the server
                // catches dead connections in ~15s, the client catches
                // them in ~300s (or immediately if the browser's TCP
                // stack reports the socket close).
                if sender.ping().await.is_err() {
                    exit_branch = "heartbeat_ping_fail";
                    was_client_disconnect = true;
                    ws_disconnected.store(true, std::sync::atomic::Ordering::SeqCst);
                    // (type-B root-cause): 心跳 ping 失败同样走 grace
                    // 延期清理，不硬杀正在跑的任务。
                    schedule_grace_disconnect();
                    break;
                }
                // Application-level heartbeat — resets the client's
                // IDLE/stale timers via `onmessage`. Minimal JSON,
                // ~60 bytes, no per-event allocation beyond the
                // envelope + serde serialize.
                let hb = WsEnvelope {
                    version: 1,
                    request_id: None,
                                        trace_id: trace_id_heartbeat.clone(),
                                        seq: None,
                                        attachment_id: None,
                    event: StreamEvent::Heartbeat,
                };
                if sender
                    .send(serde_json::to_string(&hb).unwrap_or_default())
                    .await
                    .is_err()
                {
                    exit_branch = "heartbeat_text_fail";
                    was_client_disconnect = true;
                    ws_disconnected.store(true, std::sync::atomic::Ordering::SeqCst);
                    schedule_grace_disconnect();
                    break;
                }
            }
            // read incoming WS messages during streaming.
            // Before this arm was added, the select! loop only forwarded
            // agent events TO the client and sent heartbeats — it never
            // read FROM the socket. This meant `approval_response` and
            // `clarify_response` messages sent by the user during
            // streaming sat unread in the WS receive buffer until the
            // agent finished and handle_ws_stream_send returned. By then
            // the 30s ApprovalRegistry timeout had already fired
            // (default-deny), and the user's click was a no-op.
            // Now we read from the socket concurrently. ApprovalResponse
            // and ClarifyResponse are processed immediately (resolving
            // the oneshot in ApprovalRegistry / ClarifyDispatcher so
            // the agent's AwaitApproval unblocks within milliseconds).
            // Other messages (Send, etc.) are logged and dropped — the
            // frontend's streaming guard already prevents sending a new
            // message while a stream is in flight, and the dedup
            // mechanism would reject duplicates anyway.
            text = receiver.recv() => {
                match text {
                    Some(text) => {
                        if let Ok(ws_msg) = serde_json::from_str::<hermix_transport::WsClientMessage>(&text) {
                            match ws_msg {
                                WsClientMessage::ApprovalResponse { request_id, decision, note } => {
                                    tracing::info!(
                                        session = %session_id,
                                        request_id = %request_id,
                                        decision = %decision,
                                        has_note = note.is_some(),
                                        "ws_stream_send: received approval_response during streaming"
                                    );
                                    let choice = hermix_transport::ApprovalChoice::from_decision_str(&decision);
                                    let choice = match choice {
                                        Some(c) => c,
                                        None => {
                                            tracing::warn!(
                                                session = %session_id,
                                                request_id = %request_id,
                                                decision = %decision,
                                                "unknown approval decision during streaming; downgrading to Deny"
                                            );
                                            hermix_transport::ApprovalChoice::Deny
                                        }
                                    };
                                    let outcome = approval_outcome_for(choice, note.as_deref());
                                    let matched = state.approval_registry.resolve(
                                        session_id,
                                        &request_id,
                                        outcome,
                                    );
                                    tracing::info!(
                                        session = %session_id,
                                        request_id = %request_id,
                                        matched,
                                        "approval_response (streaming) resolve: {}",
                                        if matched { "hit" } else { "miss — no pending entry" },
                                    );
                                    // Persist decision + side effects (single source of
                                    // truth in util::record_approval_resolution).
                                    util::record_approval_resolution(
                                        &state,
                                        session_id,
                                        &request_id,
                                        choice,
                                        matched,
                                    )
                                    .await;
                                }
WsClientMessage::ApprovalBatchResponse { batch_id, decision, note } => {
                                    tracing::info!(
                                        session = %session_id,
                                        batch_id = %batch_id,
                                        decision = %decision,
                                        has_note = note.is_some(),
                                        "ws_stream_send: received approval_responses_batch during streaming"
                                    );
                                    let choice =
                                        hermix_transport::ApprovalChoice::from_decision_str(&decision)
                                            .unwrap_or_else(|| {
                                                tracing::warn!(
                                                    session = %session_id,
                                                    batch_id = %batch_id,
                                                    decision = %decision,
                                                    "unknown approval decision in batch; downgrading to Deny"
                                                );
                                                hermix_transport::ApprovalChoice::Deny
                                            });
                                    let outcome = approval_outcome_for(choice, note.as_deref());
                                    let pairs = state.approval_registry.resolve_batch(
                                        session_id,
                                        &batch_id,
                                        outcome,
                                    );
                                    let mut resolved_count = 0usize;
                                    for (rid, ok) in pairs {
                                        if ok {
                                            util::record_approval_resolution(
                                                &state,
                                                session_id,
                                                &rid,
                                                choice,
                                                true,
                                            ).await;
                                            resolved_count += 1;
                                        }
                                    }
                                    tracing::debug!(
                                        session = %session_id,
                                        batch_id = %batch_id,
                                        ?choice,
                                        resolved = resolved_count,
                                        "approval_responses_batch (streaming) resolved"
                                    );
                                }
                                WsClientMessage::ClarifyResponse { request_id, answer } => {
                                    tracing::info!(
                                        session = %session_id,
                                        request_id = %request_id,
                                        "ws_stream_send: received clarify_response during streaming"
                                    );
                                    if let Err(e) = state
                                        .clarify_dispatcher
                                        .respond_by_id(&request_id, answer)
                                        .await
                                    {
                                        tracing::warn!(
                                            error = %e,
                                            request_id = %request_id,
                                            "clarify_response (streaming) resolve failed"
                                        );
                                    }
                                }
                                WsClientMessage::Ping => {
                                    //: respond to client application-level
                                    // ping with Pong so the client can detect
                                    // half-open connections.
                                    let pong = WsEnvelope {
                                        version: 1,
                                        request_id: None,
                                        trace_id: trace_id_heartbeat.clone(),
                                         seq: None,
                                         attachment_id: None,
                                        event: StreamEvent::Pong,
                                    };
                                    let _ = sender
                                        .send(serde_json::to_string(&pong).unwrap_or_default())
                                        .await;
                                }
                                WsClientMessage::Stop { .. } => {
                                    // User pressed Stop. Fire the per-send
                                    // soft-shutdown Notify: the agent loop's
                                    // forward task converts it into
                                    // `InterruptController::interrupt()`, which
                                    // (a) races the in-flight LLM call
                                    // (llm_retry O1.2 select), (b) unblocks an
                                    // approval wait, (c) trips every
                                    // `check_interrupt()` poll. The loop then
                                    // drains to `Done(outcome="interrupted")` —
                                    // that terminal frame, not this Status, is
                                    // what tells the frontend the turn ended.
                                    tracing::info!(
                                        session = %session_id,
                                        "ws_stream_send: Stop received — firing interrupt"
                                    );
                                    shutdown_notify.notify_one();
                                    let stopping = WsEnvelope {
                                        version: 1,
                                        request_id: None,
                                        trace_id: trace_id_heartbeat.clone(),
                                         seq: None,
                                         attachment_id: None,
                                        event: StreamEvent::Status {
                                            content: "正在停止当前任务…".to_string(),
                                        },
                                    };
                                    let _ = sender
                                        .send(serde_json::to_string(&stopping).unwrap_or_default())
                                        .await;
                                }
                                other => {
                                    // A Send arriving while this stream is
                                    // already forwarding is NOT silently
                                    // droppable. The old code assumed the
                                    // frontend's streaming guard made this
                                    // unreachable and dropped the message at
                                    // `debug!` level — so when a user did
                                    // manage to send (or a reconnect re-sent
                                    // queued input), the message vanished with
                                    // zero log and zero UI feedback, and the
                                    // user saw a chat that simply never
                                    // answered.
                                    //
                                    // We cannot start a second concurrent task
                                    // on this session — but we also cannot
                                    // just answer the frame and forget it:
                                    // `receiver` is a read cursor, so once the
                                    // frame is consumed the text is gone.
                                    // Hand it to the caller, which re-drives
                                    // the main loop after this turn returns.
                                    let is_send = matches!(
                                        other,
                                        hermix_transport::WsClientMessage::Send { .. }
                                    );
                                    if is_send {
                                        pending_user_msg = Some(text.clone());
                                        tracing::warn!(
                                            session = %session_id,
                                            msg_type = ?other,
                                            "incoming message during streaming — \
                                             deferred to the main loop (session busy)"
                                        );
                                        // Non-terminal Status only. A terminal
                                        // `Done` would settle the frontend's
                                        // send promise for a turn that never
                                        // ran — the user sees an empty reply and
                                        // no hint that their message is queued.
                                        // The Status renders as a banner and
                                        // keeps the promise alive until the
                                        // deferred turn produces the answer.
                                        let status = WsEnvelope {
                                            version: 1,
                                            request_id: None,
                                            trace_id: trace_id.to_string(),
                                             seq: None,
                                             attachment_id: None,
                                            event: StreamEvent::Status {
                                                content: task_in_flight_status(&text),
                                            },
                                        };
                                        let _ = sender
                                            .send(serde_json::to_string(&status).unwrap_or_default())
                                            .await;
                                    } else {
                                        // Other control frames (ping variants,
                                        // late acks) are still safe to drop.
                                        tracing::debug!(
                                            session = %session_id,
                                            msg_type = ?other,
                                            "ignoring incoming WS message during streaming"
                                        );
                                    }
                                }
                            }
                        } else {
                            // Bare (non-`WsClientMessage`) frames — the desktop
                            // client's legacy `{"type":"message","text":…}`
                            // shape lands here. Previously this branch did not
                            // exist, so the frame fell straight through the
                            // match and vanished: no log, no event, and the
                            // sender's promise never settled, leaving the chat
                            // stuck on "正在思考…".
                            //
                            // Same treatment as the typed `Send` arm above:
                            // the frame is consumed either way, so hand it to
                            // the caller to re-drive and answer with a
                            // non-terminal Status.
                            pending_user_msg = Some(text.clone());
                            tracing::warn!(
                                session = %session_id,
                                len = text.len(),
                                "unparsed frame during streaming — deferred to the main loop"
                            );
                            let status = WsEnvelope {
                                version: 1,
                                request_id: None,
                                trace_id: trace_id.to_string(),
                                 seq: None,
                                 attachment_id: None,
                                event: StreamEvent::Status {
                                    content: task_in_flight_status(&text),
                                },
                            };
                            let _ = sender
                                .send(serde_json::to_string(&status).unwrap_or_default())
                                .await;
                        }
                    }
                    None => {
                        // Close 帧 / 读错误 / 流结束三种情况已在 transport
                        // 层（WsSource::recv）就地记日志并归一为 None。
                        exit_branch = "ws_read_none";
                        was_client_disconnect = true;
                        ws_disconnected.store(true, std::sync::atomic::Ordering::SeqCst);
                        schedule_grace_disconnect();
                        break;
                    }
                }
            }
        }
    }
    tracing::info!(
        session = %session_id,
        exit_branch,
        was_client_disconnect = was_client_disconnect,
        pending_user_msg = pending_user_msg.is_some(),
        "ws_stream: rx forward loop EXITED — handle_ws_stream_send returning"
    );
    match pending_user_msg {
        Some(text) => StreamSendOutcome::DeferredUserMessage(text),
        None => StreamSendOutcome::Completed,
    }
}

/// Streaming WebSocket handler.
pub(crate) async fn handle_ws_stream(
    transport: impl Transport,
    state: HttpServerState,
    session_id: String,
) {
    // split the transport into sender/receiver halves so that
    // handle_ws_stream_send can simultaneously forward agent events TO the
    // client AND read approval_response/clarify_response FROM the client.
    // Before this split, the forward loop only wrote to the socket — it
    // never read from it — so approval clicks sat unread in the WS buffer
    // until the 30s timeout fired.
    let (mut sender, mut receiver) = transport.split();
    let trace_id_base = hermix_transport::new_trace_id();
    // P5 多端 attachment（设计 §3.5）：每条连接建立时分配一个进程级
    // 单调 attachment_id。`Connected` 帧挂载此值，前端据此区分
    // "同一 attachment 重连"与"新 attachment 挂载"。
    let attachment_id = state
        .attachment_seq
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let connected = WsEnvelope {
        version: 1,
        request_id: None,
        trace_id: trace_id_base.clone(),
         seq: None,
         attachment_id: Some(attachment_id),
        event: StreamEvent::Connected {
            session_id: session_id.clone(),
        },
    };
    let _ = sender
        .send(serde_json::to_string(&connected).unwrap_or_default())
        .await;

    // 手机端语音识别配置下发（2026-10-14 拍板）：桌面端配置的云 STT 在
    // 会话建立时随 hydrate 一起推。经 relay 时由 mobile_protocol 映射为
    // `voice.stt_config` 密封帧——relay 只见密文。未配置 = 不发帧，
    // 手机端长按说话显示「桌面端未配置，仅支持文字发送」。
    if let Some(cfg) = crate::dashboard::voice_stt::VoiceSttConfig::load(&state) {
        let frame = serde_json::json!({
            "type": "voice_stt_config",
            "voice_stt": serde_json::to_value(&cfg).unwrap_or_default(),
        });
        let _ = sender.send(frame.to_string()).await;
    }

    // Commit D — WS hydrate: send an authoritative per-session
    // TodoSnapshot right after `Connected` so reconnecting tabs /
    // WS reconnects see the full current todo list without waiting
    // for the next mutation. Builds via `SessionStamper` so non-todo
    // tools pay zero cost (the default returns `None`).
    if let Some(items) = state
        .tool_registry
        .build_snapshot_json_for_tool("todo", &session_id)
        .await
    {
        let wire_items: Vec<hermix_transport::TodoSnapshotItem> = items
            .into_iter()
            .filter_map(|v| {
                let id = v.get("id").and_then(|x| x.as_str())?.to_string();
                let title = v
                    .get("title")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let status = v
                    .get("status")
                    .and_then(|x| x.as_str())
                    .unwrap_or("pending")
                    .to_string();
                let priority = v
                    .get("priority")
                    .and_then(|x| x.as_str())
                    .unwrap_or("medium")
                    .to_string();
                let created_at = v
                    .get("created_at")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let description = v
                    .get("description")
                    .and_then(|x| x.as_str())
                    .map(String::from);
                let last_result = v
                    .get("last_result")
                    .and_then(|x| x.as_str())
                    .map(String::from);
                let execution_count = v
                    .get("execution_count")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0) as u32;
                let depends_on = v
                    .get("depends_on")
                    .and_then(|x| x.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|d| d.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let group_id = v
                    .get("group_id")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let group_title = v
                    .get("group_title")
                    .and_then(|x| x.as_str())
                    .map(String::from);
                let item_session_id = v
                    .get("session_id")
                    .and_then(|x| x.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| session_id.clone());
                let origin_message_id = v
                    .get("origin_message_id")
                    .and_then(|x| x.as_str())
                    .map(String::from);
                Some(hermix_transport::TodoSnapshotItem {
                    id,
                    title,
                    description,
                    status,
                    priority,
                    created_at,
                    depends_on,
                    last_result,
                    execution_count,
                    group_id,
                    group_title,
                    session_id: Some(item_session_id),
                    origin_message_id,
                })
            })
            .collect();
        let hydrate = WsEnvelope {
            version: 1,
            request_id: None,
            trace_id: trace_id_base.clone(),
             seq: None,
             attachment_id: None,
            event: StreamEvent::TodoSnapshot {
                session_id: session_id.clone(),
                items: wire_items,
            },
        };
        let _ = sender
            .send(serde_json::to_string(&hydrate).unwrap_or_default())
            .await;
    }

    // 模型清单快照：手机连上就有清单可看（此前它只能走 HTTP 取，而中继没有
    // 通往节点的 HTTP 通道 → 必然 404 → 选择器永远是空的）。
    push_model_list(&state, &mut *sender, &trace_id_base).await;

    // 桌面会话列表快照：手机端「桌面会话浏览」的数据源。复用 `list_sessions`
    // 的同一份查询（同一张表、同一个 scope 规则），避免两处口径漂移。
    // 扁平帧直发——`to_mobile_frame` 对扁平帧原样映射，无需新协议类型。
    {
        let resp = crate::routes::list_sessions(
            axum::extract::State(state.clone()),
            axum::extract::Query(crate::routes::ListSessionsQuery { workbench_id: None }),
        )
        .await;
        let frame = serde_json::json!({
            "type": "session_list",
            "sessions": resp.0.sessions.iter().take(30).collect::<Vec<_>>(),
        });
        let _ = sender.send(frame.to_string()).await;
    }

    // 手机可见配置（模型配置推送开关）变更即推：每条 WS 会话订阅同一个
    // watch 版本号，`put_mobile_model_push` 保存后自增，这里立刻给**已连接**
    // 的手机重推 `model.list`。没有这条，开关变更要等手机重连才生效——
    // 用户在电脑上打开推送、盯着手机却毫无动静。
    //
    // 时机边界：`handle_inbound_frame` 是长 await（一整轮 agent），变更信号
    // 只在两帧之间被 select 到——正在跑的那一轮结束后必然送达，不丢。
    let mut mobile_config_rx = state.mobile_config_tx.subscribe();

    loop {
        let first = tokio::select! {
            res = receiver.recv() => match res {
                Some(text) => text,
                None => break,
            },
            _ = mobile_config_rx.changed() => {
                let _ = mobile_config_rx.borrow_and_update();
                push_model_list(&state, &mut *sender, &trace_id_base).await;
                continue;
            }
        };
        // A turn's `handle_ws_stream_send` can hand back a user message it
        // consumed mid-stream. Re-drive it here rather than waiting for the
        // user to send something else — the read cursor already advanced
        // past it, so dropping it means the chat silently never answers.
        let mut next = Some(first);
        while let Some(text) = next.take() {
            if let StreamSendOutcome::DeferredUserMessage(deferred) =
                handle_inbound_frame(
                    &mut *sender,
                    &mut *receiver,
                    &state,
                    &session_id,
                    &trace_id_base,
                    text,
                )
                .await
            {
                next = Some(deferred);
            }
        }
    }
}


