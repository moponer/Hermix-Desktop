//! 识图插件开关的 REST 面。
//!
//! 前端输入框上方的眼睛图标通过这两个端点读写开关状态。状态本身由
//! [`hermix_agent::vision_plugin`] 持有（会话级、落盘 `$HERMIX_HOME/vision-plugin.json`），
//! 这里只做 HTTP 适配。
//!
//! ## 为什么把「模型能不能看图」也一起返回
//!
//! 关掉插件时，图片直接进上下文，**纯文本模型根本收不了**。前端要在用户
//! 粘图之前就知道这件事，否则用户会看到一条发送失败的错误，却不知道原因。
//! 把 `modelAcceptsImages` 一起给出去，UI 就能在按钮上直接提示「当前模型不支持识图」。

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::state::HttpServerState;

#[derive(Debug, Deserialize)]
pub(crate) struct ToggleBody {
    session_id: String,
    enabled: bool,
    /// 当前选中模型的 `supports_vision`。省略 = 上游没说，按「可能支持」处理。
    #[serde(default)]
    supports_vision: Option<bool>,
}

/// `GET /api/vision-plugin?session_id=...` — 读开关状态。
pub(crate) async fn get_vision_plugin(
    State(_state): State<HttpServerState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let session_id = q.get("session_id").cloned().unwrap_or_default();
    let supports_vision = q
        .get("supports_vision")
        .and_then(|v| v.parse::<bool>().ok());
    Json(json!({
        "enabled": hermix_agent::vision_plugin::is_enabled(Some(session_id.as_str())),
        "modelAcceptsImages":
            hermix_agent::vision_plugin::model_accepts_images(supports_vision),
    }))
    .into_response()
}

/// `POST /api/vision-plugin` — 写开关状态。
pub(crate) async fn set_vision_plugin(
    State(_state): State<HttpServerState>,
    Json(body): Json<ToggleBody>,
) -> Response {
    match hermix_agent::vision_plugin::set_enabled(&body.session_id, body.enabled) {
        Ok(()) => Json(json!({
            "enabled": body.enabled,
            "modelAcceptsImages":
                hermix_agent::vision_plugin::model_accepts_images(body.supports_vision),
        }))
        .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "vision_plugin_write_failed", "message": e })),
        )
            .into_response(),
    }
}
