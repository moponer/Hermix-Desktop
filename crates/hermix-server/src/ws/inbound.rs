//! 入站帧分派：流式会话里客户端消息的统一入口（自 ws.rs 拆分，P1-13）。
use super::*;

/// Fold a 4-state `ApprovalChoice` plus the user's optional
/// denial feedback into the registry-facing `ApprovalOutcome`.
/// `deny` + non-empty note becomes `DeniedWithFeedback`, which the
/// tool layer surfaces to the LLM as an actionable reason so it
/// changes course instead of retrying the denied call.
pub(super) fn approval_outcome_for(
    choice: hermix_transport::ApprovalChoice,
    note: Option<&str>,
) -> hermix_core::ApprovalOutcome {
    if choice.is_approved() {
        hermix_core::ApprovalOutcome::Approved
    } else if let Some(n) = note.map(str::trim).filter(|s| !s.is_empty()) {
        hermix_core::ApprovalOutcome::DeniedWithFeedback { note: n.to_string() }
    } else {
        hermix_core::ApprovalOutcome::DeniedByUser
    }
}

/// Handle one inbound frame of a streaming session.
///
/// Extracted from the session loop so a turn that consumed a mid-stream
/// user message can hand the text straight back and have the caller
/// re-drive it, instead of the message dying with the advanced cursor.
pub(super) async fn handle_inbound_frame(
    sender: &mut dyn TransportSink,
    receiver: &mut dyn TransportSource,
    state: &HttpServerState,
    session_id: &str,
    trace_id_base: &str,
    text: String,
) -> StreamSendOutcome {
    let trace_id = trace_id_base;
    // Tdiagnostic — log every WS message receipt.
    tracing::info!(
        session = %session_id,
        text_preview = %text.chars().take(80).collect::<String>(),
        "ws_stream: received Text message"
    );

    // Try structured WsClientMessage first (v2 protocol).
    if let Ok(ws_msg) = serde_json::from_str::<WsClientMessage>(&text) {
        match ws_msg {
            WsClientMessage::ApprovalResponse { request_id, decision, note } => {
                // INFO-level receipt log. The
                // previous `debug!` (line 5753 below) was
                // invisible at the default log level, so
                // when the user's click "disappeared" and
                // the registry timed out at 30s, we had
                // no way to distinguish "click never left
                // the client" from "click arrived but
                // server mis-routed it". Now the
                // receipt is loud enough to grep and
                // pairs naturally with the WARN
                // `approval request timed out`.
                tracing::info!(
                    session = %session_id,
                    request_id = %request_id,
                    decision = %decision,
                    "ws_stream: received approval_response",
                );
                // Parse the 4-state decision. Unknown /
                // empty values log and downgrade to
                // `Deny` (fail-closed — never auto-
                // approve on garbage input).
                let choice = hermix_transport::ApprovalChoice::from_decision_str(
                    &decision,
                );
                let choice = match choice {
                    Some(c) => c,
                    None => {
                        tracing::warn!(
                            session = %session_id,
                            request_id = %request_id,
                            decision = %decision,
                            "unknown approval decision; downgrading to Deny (fail-closed)"
                        );
                        hermix_transport::ApprovalChoice::Deny
                    }
                };
                // The legacy 2-state semantics
                // (`approved` / `denied`) and the 4-state
                // semantics share the same registry
                // resolution: any non-Deny choice approves
                // the current call.
                let approved = choice.is_approved();
                let outcome = approval_outcome_for(choice, note.as_deref());
                let matched = state.approval_registry.resolve(
                    &session_id,
                    &request_id,
                    outcome,
                );
                // matched/miss diagnostic.
                // `matched=false` means the request_id was
                // not found in the pending approvals map —
                // either the entry was already timed out,
                // the (session, request_id) key never
                // existed, or this is a duplicate resolve.
                // Paired with the `received` log above and
                // the `timed out (default deny)` WARN in
                // ApprovalRegistry, this one line tells us
                // whether the click reached the right slot.
                tracing::info!(
                    session = %session_id,
                    request_id = %request_id,
                    matched = matched,
                    "approval_response resolve: {}",
                    if matched { "hit" } else { "miss — no pending entry" },
                );
                // Persist decision + side effects (single source of truth).
                util::record_approval_resolution(
                    &state,
                    &session_id,
                    &request_id,
                    choice,
                    matched,
                )
                .await;
                tracing::debug!(
                    session = %session_id,
                    request_id = %request_id,
                    ?choice,
                    approved,
                    "resolved approval response (4-state)"
                );
                return StreamSendOutcome::Completed;
            }
            WsClientMessage::ApprovalBatchResponse { batch_id, decision, note } => {
                tracing::info!(
                    session = %session_id,
                    batch_id = %batch_id,
                    decision = %decision,
                    has_note = note.is_some(),
                    "ws_stream: received approval_responses_batch",
                );
                let choice =
                    hermix_transport::ApprovalChoice::from_decision_str(&decision)
                        .unwrap_or_else(|| {
                            tracing::warn!(
                                session = %session_id,
                                batch_id = %batch_id,
                                decision = %decision,
                                "unknown approval decision in batch; downgrading to Deny (fail-closed)"
                            );
                            hermix_transport::ApprovalChoice::Deny
                        });
                let outcome = approval_outcome_for(choice, note.as_deref());
                let pairs = state.approval_registry.resolve_batch(
                    &session_id,
                    &batch_id,
                    outcome,
                );
                tracing::info!(
                    session = %session_id,
                    batch_id = %batch_id,
                    ?choice,
                    items = pairs.len(),
                    "approval_responses_batch resolve",
                );
                let mut resolved_count = 0usize;
                for (rid, ok) in pairs {
                    if ok {
                        util::record_approval_resolution(
                            &state,
                            &session_id,
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
                    "resolved approval_responses_batch (4-state)"
                );
                return StreamSendOutcome::Completed;
            }
            WsClientMessage::ClarifyResponse { request_id, answer } => {
                // Wake the blocked `clarify` tool call. If the
                // request id is unknown (timed out, dropped,
                // resolved twice) we just log — never fail the
                // WS exchange.
                if let Err(e) = state
                    .clarify_dispatcher
                    .respond_by_id(&request_id, answer)
                    .await
                {
                    tracing::warn!(
                        error = %e,
                        request_id = %request_id,
                        "no pending clarify to resolve"
                    );
                }
                return StreamSendOutcome::Completed;
            }
            WsClientMessage::Send {
                text,
                model,
                provider,
                personality,
                system_prompt,
                tools,
                workspace,
                permission,
                user_id,
                request_id,
                images,
                image,
                image_mime_type,
                skills,
                target,
            } => {
            // 帧内 target：手机在自己的设备通道上点名把这一轮投给谁。
            // 信封 session_id 恒为设备会话（E2EE 对端公钥由 sid 前缀推导、
            // 归属闸门按它判），target 在**密封载荷里**——relay 读不到。
            // 目前只有 `twin`（阿黎）一个合法值；乱写的 target 当作没带，
            // 按原会话跑（fail-open 到安全默认，而不是凭空造一条会话）。
            let agent_session = match target.as_deref() {
                Some("twin") => crate::mobile::TWIN_SESSION_ID.to_string(),
                _ => session_id.to_string(),
            };
            // 阿黎（twin:ali）：这条会话固定用**她的人格**——桌面 TwinView 与
            // 手机端两个入口看到的是同一个她。请求显式带人格时尊重请求，
            // 兜底只发生在「没指定」的情况（手机端目前从不带）。
            let personality = if agent_session == crate::mobile::TWIN_SESSION_ID
                && personality.is_none()
            {
                Some(crate::mobile::TWIN_PERSONALITY.to_string())
            } else {
                personality
            };
            let request = SendMessageRequest {
                text,
                model,
                provider,
                personality,
                system_prompt,
                tools,
                workspace,
                permission,
                user_id,
                request_id,
                images,
                image,
                image_mime_type,
                supports_vision: None,
                skills,
                workbench_id: None,
                domain_override: None,
                expert_toolset: None,
                attached_files: None,
                attached_file_paths: None,
                enable_thinking: None,
            };
                // Return the inner outcome verbatim. This turn may have
                // consumed a mid-stream user message and needs it re-driven
                // by the caller's loop — swallowing the value here (the
                // previous bare call + `return ...Completed`) silently
                // dropped that text, so a message sent during streaming
                // never ran and the chat just never answered. The legacy
                // fallback at the bottom of this function returns verbatim
                // too; the asymmetry between these two arms was the bug.
                return handle_ws_stream_send(
                    &mut *sender,
                    &mut *receiver,
                    &state,
                    &agent_session,
                    &trace_id,
                    request,
                )
                .await;
            }
            WsClientMessage::ModelSelect { model } => {
                // 手机切默认模型：**只改默认值**，`enabled` 保持桌面那份
                // （手机回写整份清单会把付费模型挤出可见范围——真实事故）。
                match crate::dashboard::config::set_default_model(&state, &model) {
                    Ok(()) => {
                        tracing::info!(
                            session = %session_id,
                            model = %model,
                            "手机切换默认模型成功",
                        );
                        // 回推新清单，手机端立刻看到自己选中的那一个。
                        push_model_list(&state, &mut *sender, trace_id_base).await;
                    }
                    Err(e) => {
                        tracing::warn!(
                            session = %session_id,
                            error = %e,
                            "手机切换默认模型失败",
                        );
                        let err = WsEnvelope {
                            version: 1,
                            request_id: None,
                            trace_id: trace_id_base.to_string(),
                            seq: None,
                            attachment_id: None,
                            event: StreamEvent::Error {
                                code: "model_select_failed".to_string(),
                                message: format!("切换默认模型失败：{e}"),
                                detail: None,
                            },
                        };
                        let _ = sender
                            .send(serde_json::to_string(&err).unwrap_or_default())
                            .await;
                    }
                }
                return StreamSendOutcome::Completed;
            }
            WsClientMessage::Ping => {
                //: respond to client application-level
                // ping with Pong even when no stream is active.
                let pong = WsEnvelope {
                    version: 1,
                    request_id: None,
                    trace_id: trace_id_base.to_string(),
                     seq: None,
                     attachment_id: None,
                    event: StreamEvent::Pong,
                };
                let _ = sender
                    .send(serde_json::to_string(&pong).unwrap_or_default())
                    .await;
                return StreamSendOutcome::Completed;
            }
            WsClientMessage::Stop { .. } => {
                // 这条连接当前没有在转发的流式轮次——但 run 可能从别的路
                // 径在跑：后台 spawn 的 twin/队列续跑、断线重连后旧任务
                // 仍在执行。这里 no-op 的话，用户点停止（日志确认服务端
                // 收到了 `{"type":"stop"}`）完全无效，UI 卡在"正在思考"
                // 只能等轮次自己结束或重启应用（2026-11-15 实测事故）。
                //
                // 兜底：按会话键走 `interrupt_session`——turn_registry 的
                // `InterruptController::interrupt()` 对该会话所有在跑轮次
                // 生效，(a) 竞速 in-flight LLM 调用（llm_retry O1.2 select），
                // (b) 解锁审批等待，(c) 触发每个 `check_interrupt()` 轮询。
                // 没有在跑轮次时返回 false，等于原 no-op，幂等。
                let interrupted = state.agent_service().interrupt_session(session_id);
                if interrupted {
                    tracing::info!(
                        session = %session_id,
                        "ws_stream: Stop outside streaming turn — interrupted registered run via interrupt_session"
                    );
                } else {
                    tracing::debug!(
                        session = %session_id,
                        "ws_stream: Stop outside streaming turn — no in-flight task, no-op"
                    );
                }
                return StreamSendOutcome::Completed;
            }
        }
    }

    // Legacy plain-text / SendMessageRequest fallback.
    let parsed: Option<SendMessageRequest> = match serde_json::from_str(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(error = %e, "failed to parse WS text as SendMessageRequest");
            None
        }
    };
    let request = parsed.unwrap_or_else(|| SendMessageRequest {
        text: text.to_string(),
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
        supports_vision: None,
        skills: None,
        workbench_id: None,
        domain_override: None,
        expert_toolset: None,
        attached_files: None,
        attached_file_paths: None,
        enable_thinking: None,
    });
    return handle_ws_stream_send(
        &mut *sender,
        &mut *receiver,
        &state,
        &session_id,
        &trace_id,
        request,
    )
    .await;
}

#[cfg(test)]
mod inbound_frame_outcome_tests {
    /// Every arm that dispatches a turn must return the inner
    /// `StreamSendOutcome` verbatim. `handle_inbound_frame` has two such
    /// arms (`WsClientMessage::Send` and the legacy fallback); one of them
    /// used to call `handle_ws_stream_send` bare and then discard the value
    /// with `return StreamSendOutcome::Completed`, which silently dropped
    /// any user message consumed mid-stream — the chat just never answered.
    /// Types can't catch that (a swallowed value still type-checks), so
    /// assert the shape of the source instead. Verified to fail on the
    /// buggy revision (`propagated == 1`) and pass on the fix.
    #[test]
    fn every_dispatch_arm_returns_its_outcome() {
        let src = concat!(include_str!("mod.rs"), include_str!("inbound.rs"));
        // 匹配串拆开写，否则本断言的两个字面量会把自己也数进去。
        let call = concat!("handle_ws_stream_send", "(");
        let ret_call = concat!("return handle_ws_stream_send", "(");
        let dispatches = src.matches(call).count();
        let propagated = src.matches(ret_call).count();
        // 1 definition + 2 call sites; both call sites must `return`.
        assert_eq!(
            dispatches, 3,
            "unexpected number of handle_ws_stream_send references"
        );
        assert_eq!(
            propagated, 2,
            "a dispatch arm stopped returning its StreamSendOutcome — \
             a deferred mid-stream user message would be dropped again"
        );
    }
}
