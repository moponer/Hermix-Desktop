//! HTTP error type bridging agent errors to wire responses.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};

use hermix_core::AgentError;

#[derive(Debug)]
pub struct HttpError {
    pub status: StatusCode,
    pub message: String,
}

impl From<AgentError> for HttpError {
    fn from(value: AgentError) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: value.to_string(),
        }
    }
}

/// 工作台服务错误 → HTTP 状态的单一映射点：
/// NotFound/TableNotFound → 404，PermissionDenied → 403，其余 → 500。
impl From<hermix_workspace::ServiceError> for HttpError {
    fn from(value: hermix_workspace::ServiceError) -> Self {
        use hermix_workspace::ServiceError as E;
        let status = match &value {
            E::NotFound(_) | E::TableNotFound(_) => StatusCode::NOT_FOUND,
            E::PermissionDenied(_) => StatusCode::FORBIDDEN,
            E::Io(_) | E::Store(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self {
            status,
            message: value.to_string(),
        }
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

