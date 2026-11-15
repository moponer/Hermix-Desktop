//! 出站帧整形：排队回执 / 模型清单快照 / todo 事件翻译（自 ws.rs 拆分，P1-13）。
use super::*;

/// `task_in_flight` 排队回执：`queued_text` 带消息预览（截 120 字符），
/// 前端据此立即弹 toast 回显——排队等待期间界面无新事件，只靠状态 chip
/// 用户会把"排队"误读成"卡死"（2026-10-15 实机反馈）。
pub(super) fn task_in_flight_status(queued_text: &str) -> String {
    let preview: String = queued_text.chars().take(120).collect();
    serde_json::json!({
        "event": "task_in_flight",
        "message": "上一条消息仍在处理中，你的新消息已排队，将在当前任务结束后自动发送。",
        "queued_text": preview,
    })
    .to_string()
}
/// 推一份**模型清单快照**给手机。
///
/// 手机端没有独立的模型配置面：清单的唯一真相源在桌面（config.yaml）。
/// 手机此前只能靠 `GET {relay}/api/model/list` 取——而中继是只路由的字节
/// 管道，**没有**通往节点的 HTTP 通道，那条请求必然 404，于是模型选择器
/// 永远是空的、桌面换模型手机也看不见。这里在手机连上时推一份快照过去
/// （与 voice_stt 的「桌面配置 → WS 密封帧下发手机」同一范式）。
pub(super) async fn push_model_list(
    state: &HttpServerState,
    sender: &mut dyn TransportSink,
    trace_id: &str,
) {
    let list = crate::dashboard::config::model_list_for(state);
    // **默认关闭**（主人拍板 2026-10-22）：推给手机的模型配置里带着 provider
    // 凭据（API Key），那是一把「电脑关机也还有效、能烧钱」的钥匙。主人没在
    // 电脑端显式打开，就不推。
    //
    // 关掉时推的是一份**空清单**——手机端据此**删掉本地存的那份**（含 key）。
    // 只停推不清手机那份 = 假安全感：用户以为收干净了，手机其实还在用旧 key。
    if !crate::dashboard::config::MobileModelPushConfig::is_enabled(state) {
        let frame = WsEnvelope {
            version: 1,
            request_id: None,
            trace_id: trace_id.to_string(),
            seq: None,
            attachment_id: None,
            event: StreamEvent::ModelList {
                models: Vec::new(),
                default_model: None,
                providers: Vec::new(),
            },
        };
        let _ = sender
            .send(serde_json::to_string(&frame).unwrap_or_default())
            .await;
        return;
    }
    let models: Vec<hermix_transport::ModelListEntry> = list
        .models
        .iter()
        .map(|m| hermix_transport::ModelListEntry {
            provider_id: m.provider_id.clone(),
            model_id: m.model_id.clone(),
            provider_name: Some(m.provider_name.clone()),
            context_window: m.context_window,
            supports_reasoning: m.supports_reasoning,
        })
        .collect();
    // 凭据只给**已启用模型用到的** provider——手机离线时凭它直连云模型。
    let mut pids: Vec<String> = Vec::new();
    for m in &list.models {
        if !pids.contains(&m.provider_id) {
            pids.push(m.provider_id.clone());
        }
    }
    let providers = crate::dashboard::config::model_credentials_for(state, &pids);
    let frame = WsEnvelope {
        version: 1,
        request_id: None,
        trace_id: trace_id.to_string(),
        seq: None,
        attachment_id: None,
        event: StreamEvent::ModelList {
            models,
            default_model: list.default_model.clone(),
            providers,
        },
    };
    let _ = sender
        .send(serde_json::to_string(&frame).unwrap_or_default())
        .await;
}

/// Handle a single `SendMessageRequest` inside a streaming WS session.
/// Extracted so the `WsClientMessage::Send` path and the legacy
/// `SendMessageRequest` / plain-text path share the same logic.
/// Translate a `TodoChange` (emitted by the `todo` tool) into a
/// wire-level [`StreamEvent`] that the desktop `useStreamChat`
/// hook already knows how to dispatch.
/// every mutation becomes a `TodoItemUpserted`
/// so the front-end can apply upsert-by-id semantics. The
/// `session_id` field on every event is the WS session that
/// produced the change — desktop tabs already filter their todo
/// panel by their own session id, so cross-session bleed is a
/// non-issue as long as the front-end drops events whose
/// `session_id` doesn't match the active tab. A future P2 can
/// add per-session scoping to the `todo` tool itself.
pub(crate) fn todo_change_to_stream(
    session_id: &str,
    message_id: Option<&str>,
    change: hermix_tools::tools::todo::TodoChange,
) -> Option<StreamEvent> {
    use hermix_tools::tools::todo::TodoChangeKind;
    match change.change {
        TodoChangeKind::GroupStarted { group_id, group_title, .. } => {
            Some(StreamEvent::TodoGroupStarted {
                session_id: session_id.to_string(),
                message_id: message_id.map(String::from),
                group_id,
                group_title,
            })
        }
        TodoChangeKind::Upsert(item) => Some(StreamEvent::TodoItemUpserted {
            session_id: session_id.to_string(),
            item: hermix_transport::TodoSnapshotItem {
                id: item.id,
                title: item.title,
                description: item.description,
                status: item.status,
                priority: item.priority,
                created_at: item.created_at,
                depends_on: item.depends_on,
                last_result: item.last_result,
                execution_count: item.execution_count,
                group_id: item.group_id,
                group_title: item.group_title,
                // Commit A: thread the persisted session + origin-message
                // ids onto the wire so the front-end can re-anchor the
                // inline plan card after reload.
                session_id: item.session_id,
                origin_message_id: item.origin_message_id,
            },
        }),
        TodoChangeKind::Delete { id } => Some(StreamEvent::TodoItemRemoved {
            session_id: session_id.to_string(),
            id,
        }),
        // Commit C: authoritative session-scoped snapshot. Items come
        // pre-filtered by `list_for_session` from `TodoHandler::publish_snapshot`.
        // Translate each `serde_json::Value` into the wire
        // `TodoSnapshotItem` shape. We tolerate missing fields (legacy
        // `todos.json`) by defaulting to empty strings / `None` — the
        // front-end already handles missing optionals.
        TodoChangeKind::Snapshot { session_id: snap_sid, items } => {
            let wire_sid = snap_sid.unwrap_or_else(|| session_id.to_string());
            let wire_items = items
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
                        .or_else(|| Some(wire_sid.clone()));
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
                        session_id: item_session_id,
                        origin_message_id,
                    })
                })
                .collect();
            Some(StreamEvent::TodoSnapshot {
                session_id: wire_sid,
                items: wire_items,
            })
        }
    }
}
