//! HTTP error type bridging agent errors to wire responses.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};

use hermix_core::AgentError;

#[derive(Debug)]
pub struct HttpError {
    pub status: StatusCode,
    pub message: String,
}

/// `AgentError` → HTTP 状态。
///
/// 此前**一律** `400 BAD_REQUEST`：读库失败、上游超时、配置缺失全被报成
/// "你请求错了"，前端无法据此决定"重试 / 改参数 / 报告故障"，监控也看不到
/// 5xx（它们统计不到）。现在按变体分流：
///
/// | 变体 | 状态 |
/// |---|---|
/// | `InvalidToolCall` / `ToolExecution` | 400（参数/工具级，消息可回显） |
/// | `ContextTooLong` | 413 |
/// | `RateLimited` | 429 |
/// | `MaxTurnsExceeded` / `Interrupted` | 409 |
/// | `Timeout` | 504 |
/// | `LlmApi` / `Gateway` / `AuthFailed` | 502（上游故障） |
/// | `Config` / `Io` | 500 |
///
/// 5xx 的响应体只给固定文案：这些 `Display` 里常带路径、SQL、上游 URL，
/// 回给客户端等于泄漏内部布局（细节进 `server.log`）。
impl From<AgentError> for HttpError {
    fn from(value: AgentError) -> Self {
        use hermix_core::AgentError as E;
        let status = match &value {
            E::InvalidToolCall(_) | E::ToolExecution(_) => StatusCode::BAD_REQUEST,
            E::ContextTooLong => StatusCode::PAYLOAD_TOO_LARGE,
            E::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            E::MaxTurnsExceeded | E::Interrupted { .. } => StatusCode::CONFLICT,
            E::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
            E::LlmApi(_) | E::Gateway(_) | E::AuthFailed(_) => StatusCode::BAD_GATEWAY,
            E::Config(_) | E::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let message = if status.is_server_error() {
            tracing::error!(error = %value, "request failed");
            "内部错误（详见 server.log）".to_string()
        } else {
            value.to_string()
        };
        Self { status, message }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_errors_keep_their_message() {
        let err = HttpError::from(AgentError::InvalidToolCall("bad args".into()));
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(err.message.contains("bad args"));
    }

    #[test]
    fn infrastructure_errors_map_to_5xx_and_hide_details() {
        let err = HttpError::from(AgentError::Io("open C:\\Users\\me\\.hermix\\x.db".into()));
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!err.message.contains(".hermix"), "不得回传路径: {}", err.message);

        assert_eq!(
            HttpError::from(AgentError::Timeout("upstream".into())).status,
            StatusCode::GATEWAY_TIMEOUT
        );
        assert_eq!(
            HttpError::from(AgentError::RateLimited { retry_after_secs: Some(3) }).status,
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            HttpError::from(AgentError::ContextTooLong).status,
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }
}


impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.status, self.message)
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

