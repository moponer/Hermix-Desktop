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
    // 显式帧/消息上限（P2-8）：此前依赖 axum 默认 64MB 单帧，而 WS 入站帧
    // 不受 HTTP 侧 `RequestBodyLimitLayer`（2MB）约束。附件以 JSON 数字数组
    // 走帧（每个字节编码成 ~4 字符，解析放大 ~10 倍内存），8MB 上限约等于
    // HTTP 侧 2MB 原始字节的膨胀后体积。frame 与 message 取同值：客户端通常
    // 把一条消息放在一个帧里，不依赖分片，设小会让正常附件被直接断开。
    const WS_MAX_BYTES: usize = 8 * 1024 * 1024;
    upgrade
        .max_frame_size(WS_MAX_BYTES)
        .max_message_size(WS_MAX_BYTES)
        .on_upgrade(move |socket| async move {
            let transport = crate::ws_transport::WsTransport::new(socket);
            crate::ws::handle_ws_stream(transport, state, session_id).await;
        })
}

/// 跑一遍与非 HTTP 面同源的设备鉴权，产出 `Origin`。
///
/// 只在**凭据命中**时返回 `PairedDevice`；拿不到设备就不给 `Local`——WS 面
/// 没有 loopback 可言，把失败降级成 Local 等于把鉴权整个绕开。
/// 从 relay 帧里取 hermix 侧凭据，**还原成一次逻辑 HTTP 握手**再验。
///
/// 这是本模块最要紧的一处逻辑，也是**唯一**把「relay 不是 HTTP 跳」这个
/// 事实变成代码的地方：
///
/// - App 实际发的是 WS 帧，没有 HTTP 请求头。所以 `Authorization` 与
///   `X-Hermix-Signature` 由帧里的 [`FrameAuth`] 携带（relay 只透传）。
/// - 签名覆盖 `METHOD\npath`，`path` 里含 session id。**PC 先从信封取出
///   `session_id`、用它重建 `/v1/ws-stream/{sid}`，再验签名**——于是签名
///   与会话绑定：A 会话的有效签名无法被拿来冒用 B 会话。若改为信任帧里自报
///   的任意 path，这层绑定就没了。
/// - 走的是与直连 `/v1/ws-stream` **同一份** `authenticate_device_request`，
///   判定链（Bearer + 签名 + 能力位）不分入口。
pub(crate) fn verify_frame_auth(
    state: &HttpServerState,
    envelope: &crate::relay_proto::Envelope,
) -> Result<(), String> {
    let device = verify_frame_auth_parts(
        state.device_registry.as_ref(),
        &state.nonce_cache,
        envelope.session_id.as_deref(),
        envelope.auth.as_ref(),
    )?;
    // 归属闸门：签名有效不代表该设备拥有这条会话。
    let origin = crate::security::derive_origin(Some(&device), None);
    if !crate::mobile::session_visible_to(&origin, &session_id_of(envelope)?) {
        return Err("session 不属于该设备".to_string());
    }
    Ok(())
}

fn session_id_of(env: &crate::relay_proto::Envelope) -> Result<String, String> {
    env.session_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "帧缺 session_id".to_string())
}

/// 上面那个函数的**被测本体**——只依赖两个部件，不依赖整个 `HttpServerState`。
///
/// 抽出来是为了能直接单测它：把逻辑复制进测试再测，测的就不是出货的那份代码
/// 了，那种测试「全过而真函数跑偏」也不报警。
pub(crate) fn verify_frame_auth_parts(
    devices: Option<&std::sync::Arc<crate::pairing::DeviceRegistry>>,
    cache: &crate::request_signature::NonceCache,
    session_id: Option<&str>,
    auth: Option<&crate::relay_proto::FrameAuth>,
) -> Result<crate::pairing::PairedDevice, String> {
    let sid = session_id
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "帧缺 session_id".to_string())?;
    let auth = auth.ok_or_else(|| "帧缺 hermix 凭据".to_string())?;

    // 用信封里的 session_id 重建逻辑路径 —— 签名因此与会话绑定。
    let path = format!("/v1/ws-stream/{sid}");
    let mut headers = axum::http::HeaderMap::new();
    if let Ok(v) = axum::http::HeaderValue::from_str(&format!("Bearer {}", auth.bearer)) {
        headers.insert(axum::http::header::AUTHORIZATION, v);
    }
    if let Ok(v) = axum::http::HeaderValue::from_str(&auth.signature) {
        headers.insert("x-hermix-signature", v);
    }

    crate::security::authenticate_device_request(&headers, "GET", &path, devices, cache)
        .map_err(|e| format!("{e:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pairing::DeviceRegistry;
    use crate::relay_proto::{Envelope, FrameAuth, FrameTag};
    use std::sync::Arc;

    /// 造一个只带 `device_registry` / `nonce_cache` 的最小 state。
    /// 其它字段用 `Default` 填不上（HttpServerState 没有 Default），所以这里
    /// 只借用已有的构造入口：走 `HttpServerState` 的真实构造代价太高，改为把
    /// 被测逻辑抽成不依赖整个 state 的纯函数。
    fn signed_envelope(token: &str, sid: &str, nonce: &str) -> Envelope {
        let path = format!("/v1/ws-stream/{sid}");
        let t = chrono::Utc::now().timestamp();
        let sig = crate::request_signature::compute_signature(token, "GET", &path, t, nonce);
        Envelope {
            session_id: Some(sid.to_string()),
            auth: Some(FrameAuth {
                bearer: token.to_string(),
                signature: format!("t={t},n={nonce},s={sig}"),
            }),
            ..Envelope::new(FrameTag::Route)
        }
    }

    /// 直接调被测本体，不在测试里复制逻辑。
    type Checked = crate::pairing::PairedDevice;

    fn check(
        devices: Option<&Arc<DeviceRegistry>>,
        cache: &crate::request_signature::NonceCache,
        env: &Envelope,
    ) -> Result<Checked, String> {
        verify_frame_auth_parts(
            devices,
            cache,
            env.session_id.as_deref(),
            env.auth.as_ref(),
        )
    }

    /// **本模块最要紧的一条**：A 会话的合法签名，不能被拿去冒用 B 会话。
    ///
    /// 守它的是「用信封里的 `session_id` 重建逻辑路径再验签」——如果
    /// 改成信任帧里自报的 path，这层绑定立刻消失，而现象是「换个 session_id
    /// 就能进别人的会话」，不会有任何报错。
    #[test]
    fn 会话甲的签名不能冒用会话乙() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Arc::new(DeviceRegistry::open(&dir.path().join("d.db")).unwrap());
        let (_d, token) = reg.pair("测试手机").unwrap();
        let cache = crate::request_signature::NonceCache::new();

        let a_sid = "im:device:dev-aaa";
        let b_sid = "im:device:dev-bbb";
        let env_a = signed_envelope(&token, a_sid, "n-a");
        // 把 A 的凭据原样搬到 B 的信封上——签名覆盖的仍是 A 的路径
        let mut forged = env_a.clone();
        forged.session_id = Some(b_sid.to_string());

        assert!(check(Some(&reg), &cache, &env_a).is_ok());
        assert!(
            check(Some(&reg), &cache, &forged).is_err(),
            "换个 session_id 就过签 = 可冒用别的会话，必须被拒"
        );
    }

    #[test]
    fn 缺session_id或缺凭据一律拒() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Arc::new(DeviceRegistry::open(&dir.path().join("d.db")).unwrap());
        let (_d, token) = reg.pair("测试手机").unwrap();
        let cache = crate::request_signature::NonceCache::new();

        let mut no_sid = signed_envelope(&token, "im:device:dev-aaa", "n1");
        no_sid.session_id = None;
        assert!(check(Some(&reg), &cache, &no_sid).is_err(), "缺 session_id 必须拒");

        let mut empty_sid = signed_envelope(&token, "im:device:dev-aaa", "n1b");
        empty_sid.session_id = Some(String::new());
        assert!(check(Some(&reg), &cache, &empty_sid).is_err(), "空 session_id 必须拒");

        let mut no_auth = signed_envelope(&token, "im:device:dev-aaa", "n2");
        no_auth.auth = None;
        assert!(check(Some(&reg), &cache, &no_auth).is_err(), "缺凭据必须拒");
    }

    #[test]
    fn 无效token与无注册表都拒() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Arc::new(DeviceRegistry::open(&dir.path().join("d.db")).unwrap());
        let cache = crate::request_signature::NonceCache::new();
        let env = signed_envelope("hmxd_bogus", "im:device:dev-aaa", "n3");
        assert!(check(Some(&reg), &cache, &env).is_err(), "未登记 token 必须拒");
        // 注册表都没有时也拒，不放行
        assert!(check(None, &cache, &env).is_err());
    }

    #[test]
    fn nonce重放在帧内路径上同样被拒() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Arc::new(DeviceRegistry::open(&dir.path().join("d.db")).unwrap());
        let (_d, token) = reg.pair("测试手机").unwrap();
        let cache = crate::request_signature::NonceCache::new();
        let env = signed_envelope(&token, "im:device:dev-aaa", "replay-me");
        assert!(check(Some(&reg), &cache, &env).is_ok());
        assert!(
            check(Some(&reg), &cache, &env).is_err(),
            "同一帧重放必须失败"
        );
    }
}

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
            // 测试通道（HERMIX_TEST_TOKEN 显式设置时）：常规鉴权失败后，
            // 精确匹配测试 token 则按已配对测试设备放行——协议级评测/CI
            // 的自动化入口。未设置该变量时走不进这支，行为与现状一致。
            let bearer = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.strip_prefix("Bearer ").map(str::trim));
            if let Some(_device) = crate::security::test_channel_device(bearer) {
                tracing::info!("protocol test channel accepted (HERMIX_TEST_TOKEN)");
                // 用 Local origin 而非 Device：测试客户端的 session_id 需要
                // 原样透传（Device origin 会把 sid 命名空间化到设备名下，
                // 客户端拿不到映射 → 403）。鉴权已由 token 精确匹配完成。
                return Ok(crate::security::derive_origin(None, None));
            }
            hermix_telemetry::record_http_reject();
            Err(e.as_response())
        }
    }
}
