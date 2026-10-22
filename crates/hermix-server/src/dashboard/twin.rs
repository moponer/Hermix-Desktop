//! 助手阿黎的会话通道。
//!
//! 阿黎页输入口提交的文字 → 后台 `agent_service.send_message()` 跑一轮 →
//! 回复落在一条专属会话里。与 automation / workflow 同模式（铁律 DD：
//! 非请求驱动也走与对话相同的派发路径），所以工具、审批、记忆全部一致。
//!
//! 会话 id 由前端解析（找到或创建标题为「助手阿黎」的会话）后传来 ——
//! 不在后端固定 id 的原因：`send_message` 会 UPSERT 会话行，固定 id 一旦
//! 被用户删掉就会静默复活成无标题孤儿（`decide_delivery_target` 的注释里
//! 有同一条教训）。
//!
//! 并发闸：同一时刻至多一轮在跑。她不是并发的消息队列 —— 两条消息同时进
//! 会变成交错的两个轮次，谁也说不清哪条回复对应哪条提问。

use std::sync::atomic::{AtomicBool, Ordering};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use hermix_core::traits::AgentOverrides;
use serde::Deserialize;

use crate::state::HttpServerState;

static TWIN_RUNNING: AtomicBool = AtomicBool::new(false);

/// 运行结束后放闸。用 Drop 而不是在任务尾部手动 store：
/// `send_message` 若 panic，任务尾部那行不会执行，闸就永远关上了。
struct RunningGuard;

impl Drop for RunningGuard {
    fn drop(&mut self) {
        TWIN_RUNNING.store(false, Ordering::SeqCst);
    }
}

#[derive(Deserialize)]
pub struct TwinMessageRequest {
    pub text: String,
    /// 阿黎的专属会话 id（前端负责找到或创建）。
    pub session_id: String,
}

/// POST /api/twin/message — 给阿黎发一条消息，后台跑一轮。
///
/// 立即返回「已开始」；回复逐条写进会话，前端轮询或直接打开会话查看。
pub async fn send_twin_message(
    State(state): State<HttpServerState>,
    Json(body): Json<TwinMessageRequest>,
) -> Response {
    let text = body.text.trim().to_string();
    if text.is_empty() {
        return (StatusCode::BAD_REQUEST, "消息不能为空").into_response();
    }
    let session_id = body.session_id.trim().to_string();
    if session_id.is_empty() {
        return (StatusCode::BAD_REQUEST, "缺少会话 id").into_response();
    }
    if TWIN_RUNNING.swap(true, Ordering::SeqCst) {
        return (
            StatusCode::CONFLICT,
            "她正在处理上一条消息，等这一轮结束再发",
        )
            .into_response();
    }

    let agent = state.agent_service.clone();
    // `async move` 会把 `session_id` 移进闭包，所以响应体要用的那份得留一份
    // 副本在这里（否则闭包之后的 `session_id` 已被移动）。
    let run_session_id = session_id.clone();
    tokio::spawn(async move {
        let _guard = RunningGuard;
        let overrides = AgentOverrides::default();
        if let Err(e) = agent.send_message(&run_session_id, &text, overrides).await {
            tracing::warn!(session_id = %run_session_id, error = %e, "阿黎的一轮运行失败");
        }
    });

    Json(serde_json::json!({
        "ok": true,
        "session_id": session_id,
    }))
    .into_response()
}
