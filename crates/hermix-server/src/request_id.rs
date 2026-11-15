//! 请求级关联 id（`x-request-id`）。
//!
//! 为什么需要：WS 侧每个流事件都带 `trace_id`，而 HTTP 侧此前**没有任何**
//! 请求级标识——`TraceLayer` 的 span 不落盘，用户报障时给出的时间点无法与
//! `server.log` 对上。"这条 500 是哪个请求"只能靠猜。
//!
//! 行为：
//! - 客户端带了合法 `x-request-id`（`[A-Za-z0-9_-]{1,64}`）就沿用它（便于
//!   端到端串联），否则生成 `<毫秒时间戳十六进制>-<进程内序号>`；
//! - 响应头原样回写，前端/脚本可直接引用；
//! - 5xx 追加一条带 `request_id` 的 error 日志（4xx 是客户端错误，默认不刷屏）。

use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use std::sync::atomic::{AtomicU64, Ordering};

const HEADER: &str = "x-request-id";
/// 上限 64：够长（uuid/hash）又不会让日志行被注入撑爆。
const MAX_ID_LEN: usize = 64;

/// 只接受 `[A-Za-z0-9_-]`。拒绝空格/引号/换行等可以污染日志或响应头的字符。
fn sanitize_request_id(raw: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    if raw.is_empty() || raw.len() > MAX_ID_LEN {
        return None;
    }
    raw.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        .then(|| raw.to_string())
}

fn generate_request_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{ms:x}-{:x}", SEQ.fetch_add(1, Ordering::Relaxed))
}

pub(crate) async fn middleware(req: Request, next: Next) -> Response {
    let id = sanitize_request_id(
        req.headers()
            .get(HEADER)
            .and_then(|v| v.to_str().ok()),
    )
    .unwrap_or_else(generate_request_id);

    let path = req.uri().path().to_string();
    let mut res = next.run(req).await;

    if res.status().is_server_error() {
        tracing::error!(
            request_id = %id,
            status = res.status().as_u16(),
            path = %path,
            "http request failed",
        );
    }

    if let Ok(value) = HeaderValue::from_str(&id) {
        res.headers_mut()
            .insert(HeaderName::from_static(HEADER), value);
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_ids_and_rejects_injection() {
        assert_eq!(sanitize_request_id(Some("abc-123_X")), Some("abc-123_X".into()));
        assert_eq!(sanitize_request_id(Some("  padded  ")), Some("padded".into()));
        // 日志注入 / 响应头注入的常见载体
        assert_eq!(sanitize_request_id(Some("a b")), None);
        assert_eq!(sanitize_request_id(Some("a\nb")), None);
        assert_eq!(sanitize_request_id(Some("a\"b")), None);
        assert_eq!(sanitize_request_id(Some("")), None);
        assert_eq!(sanitize_request_id(Some(&"x".repeat(MAX_ID_LEN + 1))), None);
        assert_eq!(sanitize_request_id(None), None);
    }

    #[test]
    fn generated_ids_are_unique_and_sanitizable() {
        let a = generate_request_id();
        let b = generate_request_id();
        assert_ne!(a, b);
        assert_eq!(sanitize_request_id(Some(&a)), Some(a));
    }
}
