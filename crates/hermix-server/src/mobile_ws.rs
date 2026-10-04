//! 手机端流式面的 axum upgrade handler。
//!
//! **这个端点曾经被删**：`/v1/ws` 与 `/v1/ws-stream` 的 upgrade handler 在
//! `181a51aa`（2026-09-26 端点审计）里被删掉，理由是「桌面端全量迁 stdio
//! bridge、前端零裸 `new WebSocket`、本项目没有移动端 crate」。桌面端那半确实
//! 迁完了，但「没有移动端」这个前提现在不成立——手机端跨网络，stdio 够不着。
//!
//! **不是回退**：流式协议本体 `ws::handle_ws_stream` 从未改动（stdio 侧一直在
//! 用），本模块只补「HTTP 升级 → 鉴权 → 包成 `Transport` → 交给既有 handler」
//! 这一段。看到这里请不要把这个端点当回退删掉。
//!
//! 鉴权是 **Bearer + 请求签名**双要素（§5.5 T4），判定链与 HTTP 面同源：
//! `security::authenticate_device_request` → `derive_origin` → 能力位交集。
//! 自己重写一套就等于有第二套判定，绕过未配对闸门与失败预算是迟早的事。

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::http::StatusCode;

use crate::state::HttpServerState;

/// `GET /v1/ws-stream/{session_id}` —— 升级为 WebSocket。
pub(crate) async fn ws_stream_upgrade(
    upgrade: WebSocketUpgrade,
    State(state): State<HttpServerState>,
    Path(session_id): Path<String>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
) -> Response {
    // 归属闸门放在最前面：sid 不属于本设备时，连鉴权都不用做（也不该泄漏
    // 「这个 sid 存不存在」）。
    let origin = match device_origin(&state, &headers, method.as_str(), uri.path()) {
        Ok(o) => o,
        Err(resp) => return resp,
    };
    let own = match origin.device_id() {
        Some(d) => crate::channels::session::session_id(
            crate::channels::session::DEVICE_PLATFORM,
            d,
        ),
        None => {
            // Local（loopback）：桌面端本机调试放行，session id 原样使用。
            session_id.clone()
        }
    };
    if origin.device_id().is_some() && session_id != own {
        return (
            StatusCode::FORBIDDEN,
            "session does not belong to this device",
        )
            .into_response();
    }
    upgrade.on_upgrade(move |socket| async move {
        let transport = crate::ws_transport::WsTransport::new(socket);
        crate::ws::handle_ws_stream(transport, state, session_id).await;
    })
}

/// 跑一遍与非 HTTP 面同源的设备鉴权，产出 `Origin`。
///
/// 只在**凭据命中**时返回 `PairedDevice`；拿不到设备就不给 `Local`——WS 面
/// 没有 loopback 可言，把失败降级成 Local 等于把鉴权整个绕开。
fn device_origin(
    state: &HttpServerState,
    headers: &axum::http::HeaderMap,
    method: &str,
    path: &str,
) -> Result<hermix_core::Origin, Response> {
    if state.device_registry.is_none() {
        // 注册表都打不开（测试 / 只读环境）→ 一律 Unpaired，不放行。
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "device registry unavailable; refusing websocket upgrade",
        )
            .into_response());
    }
    match crate::security::authenticate_device_request(
        headers,
        method,
        path,
        state.device_registry.as_ref(),
        &state.nonce_cache,
    ) {
        Ok(device) => Ok(crate::security::derive_origin(Some(&device), None)),
        Err(e) => {
            hermix_telemetry::record_http_reject();
            Err(e.as_response())
        }
    }
}
