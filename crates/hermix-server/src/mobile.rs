//! 手机端（Hermix M）经 relay 接入的两个控制面端点。
//!
//! **归属过滤是不可省的**，不是加固项：连接层（relay 按 token 哈希把帧路由到
//! 对应电脑）保证了「A 的帧到不了 B 的电脑」，但如果这里按 sid 返回数据而
//! 不判断归属，A 依然能列出 B 的会话名与首行（那可是用户 prompt 正文）——
//! 隔离就在 API 层漏了。
//!
//! 设备会话 id 由待拍板 1 的决定固定为 `im:device:{device_id}`（App 以合成
//! `ImPort` 身份进 channel，platform = `device`、user = device_id），所以归属
//! 判定就是「sid 是否等于本设备的会话 id」。

use axum::extract::{Extension, Path, State};

/// 阿黎（Twin）的会话 id 与人格 slug。
///
/// 她是一个**常驻桌面端的执行主体**（工具 / 工作区 / 派 agent），不是某台
/// 手机的私有会话——桌面 TwinView 与手机端两个入口读写同一条 `twin:ali`，
/// 所以历史只有一份，两边看到的是同一个她。手机帧照常走自己的设备通道
/// （`im:device:{id}`，E2EE 不动），到达后由 ws.rs 把这一轮投到这条会话。
pub(crate) const TWIN_SESSION_ID: &str = "twin:ali";
/// 阿黎的人格（`hermix_agent::context::BUILTIN_PERSONALITIES` 里的 slug）。
pub(crate) const TWIN_PERSONALITY: &str = "ali";
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use hermix_core::origin::Capability;

use crate::state::HttpServerState;

/// 本设备可见的那一个会话 id。Local 来源不适用（桌面端看全部）。
fn own_session_id(origin: &hermix_core::Origin) -> Option<String> {
    origin.device_id().map(|d| {
        crate::channels::session::session_id(crate::channels::session::DEVICE_PLATFORM, d)
    })
}

/// sid 归属闸门：Local 全放行；配对设备只能碰自己那一条。
///
/// `pub(crate)` 因为 relay 帧的鉴权也要用同一把闸（`mobile_ws::verify_frame_auth`
/// 在验完签名后必须再判归属——**签名有效不代表这条会话属于该设备**）。
pub(crate) fn session_visible_to(origin: &hermix_core::Origin, sid: &str) -> bool {
    let Some(own) = own_session_id(origin) else {
        return true; // Local：桌面端调试任意会话
    };
    sid == own
}

/// `pub(crate)`：审批 REST 面（`list_pending_approvals` / `approve_via_rest`）
/// 也走同一把闸——它们原来只在桌面段，手机端经 relay 接入后同样可达，
/// 签名通过≠这条会话属于该设备。
pub(crate) fn require_own_session(origin: &hermix_core::Origin, sid: &str) -> Result<(), Response> {
    if session_visible_to(origin, sid) {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            "session does not belong to this device",
        )
            .into_response())
    }
}

/// `POST /v1/sessions/{session_id}/interrupt`
///
/// 中断该会话正在跑的一轮。走 `AgentService::interrupt_session`（P1 落地），
/// 不碰 `interrupt_all` —— 后者是关机原语，用在这里等于让一个手机掐掉全机。
pub(crate) async fn interrupt_session(
    Extension(origin): Extension<hermix_core::Origin>,
    Extension(device_scopes): Extension<Option<std::collections::HashSet<Capability>>>,
    State(state): State<HttpServerState>,
    Path(session_id): Path<String>,
) -> Response {
    if let Err(resp) = crate::routes::check_capability_from_extensions(
        Capability::Chat,
        &origin,
        &device_scopes,
    ) {
        return resp.into_response();
    }
    if let Err(resp) = require_own_session(&origin, &session_id) {
        return resp;
    }
    let interrupted = state.agent_service().interrupt_session(&session_id);
    Json(serde_json::json!({ "interrupted": interrupted })).into_response()
}

/// `GET /v1/tasks`
///
/// 运行中 + 排队的会话列表。**按设备过滤**：配对设备只看到自己那一条。
/// 数据由 `ChannelRuntime` 发布到 `state.channel_sessions` —— 推导留在通道侧
/// （那里才有 running / queues 的真相），这里只做过滤与格式化。
pub(crate) async fn list_tasks(
    Extension(origin): Extension<hermix_core::Origin>,
    Extension(device_scopes): Extension<Option<std::collections::HashSet<Capability>>>,
    State(state): State<HttpServerState>,
) -> Response {
    if let Err(resp) = crate::routes::check_capability_from_extensions(
        Capability::Chat,
        &origin,
        &device_scopes,
    ) {
        return resp.into_response();
    }
    let own = own_session_id(&origin);
    let all = state.channel_sessions.lock().unwrap_or_else(|e| e.into_inner());
    let visible: Vec<&crate::state::ChannelSessionView> = match &own {
        // 配对设备：只看自己那条。Local：看全部（桌面端调试用）。
        Some(_) => all.iter().filter(|v| own_session_id(&origin).as_deref() == Some(v.key.as_str())).collect(),
        None => all.iter().collect(),
    };
    Json(serde_json::json!({ "tasks": visible })).into_response()
}

/// `GET /v1/vault/entries` — 手机端凭据列表（**永不含密码**）。
///
/// 信任边界（决策笔记 2026-10-09-vault-credentials）：仅配对设备可访问
/// （Local 也放行供桌面调试）；E2EE 由 relay_transport 层透明完成，这里
/// 返回普通 JSON。列表不含密码——密码走 `/v1/vault/reveal/{name}`。
pub(crate) async fn vault_entries(
    Extension(origin): Extension<hermix_core::Origin>,
    Extension(device_scopes): Extension<Option<std::collections::HashSet<Capability>>>,
) -> Response {
    if let Err(resp) = crate::routes::check_capability_from_extensions(
        Capability::Chat,
        &origin,
        &device_scopes,
    ) {
        return resp.into_response();
    }
    if origin.device_id().is_none() {
        // 未配对来源（非 Local 也非 PairedDevice）：拒绝。
        return (StatusCode::FORBIDDEN, "vault requires a paired device").into_response();
    }
    match hermix_vault::Vault::open() {
        Ok(v) => match v.list() {
            Ok(entries) => Json(serde_json::json!({ "entries": entries })).into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("vault 读取失败: {e}"),
            )
                .into_response(),
        },
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("vault 不可用: {e}"),
        )
            .into_response(),
    }
}

/// `POST /v1/vault/reveal/{name}` — 手机端按名称取单条（含密码）。
///
/// 用户在手机对话里向 agent 要某平台凭据的场景（决策笔记需求源头）。
/// 每次都现取（不缓存）；归途经 E2EE，relay 永不见明文。
pub(crate) async fn vault_reveal(
    Extension(origin): Extension<hermix_core::Origin>,
    Extension(device_scopes): Extension<Option<std::collections::HashSet<Capability>>>,
    Path(name): Path<String>,
) -> Response {
    if let Err(resp) = crate::routes::check_capability_from_extensions(
        Capability::Chat,
        &origin,
        &device_scopes,
    ) {
        return resp.into_response();
    }
    if origin.device_id().is_none() {
        return (StatusCode::FORBIDDEN, "vault requires a paired device").into_response();
    }
    match hermix_vault::Vault::open() {
        Ok(v) => match v.get_by_name(&name) {
            Ok(entry) => Json(serde_json::json!({
                "name": entry.name,
                "url": entry.url,
                "username": entry.username,
                "password": entry.secret,
                "notes": entry.notes,
            }))
            .into_response(),
            Err(hermix_vault::VaultError::NotFound(_)) => (
                StatusCode::NOT_FOUND,
                format!("保险库里没有名为「{name}」的条目"),
            )
                .into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("vault 读取失败: {e}"),
            )
                .into_response(),
        },
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("vault 不可用: {e}"),
        )
            .into_response(),
    }
}

/// 通道侧调用：把当前 running / queues 重新发布成一份快照。
///
/// 全量重建而不是增量维护：IM 规模下（几十个会话）重建成本可忽略，而增量维护
/// 意味着每次 insert/remove/drain 都要记得同步——漏一处就是陈旧数据，比慢更糟。
pub(crate) fn publish_channel_sessions(
    state: &HttpServerState,
    views: Vec<crate::state::ChannelSessionView>,
) {
    let mut slot = state.channel_sessions.lock().unwrap_or_else(|e| e.into_inner());
    *slot = views;
}

/// `/v1/tasks` 的响应形状（serde 派生的 view 直接序列化给 App）。
#[allow(dead_code)]
fn _assert_view_is_serializable(v: &crate::state::ChannelSessionView) {
    let _ = serde_json::to_string(v);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device_origin(id: &str) -> hermix_core::Origin {
        hermix_core::Origin::PairedDevice {
            device_id: id.to_string(),
        }
    }

    #[test]
    fn 设备能碰到通道真正分配的那条会话() {
        // 最强的一条：拿通道自己的 `session_id()` 造一条真实会话 id，
        // 闸门必须放行。手拼格式的实现（漏 `im:` 前缀那种）在这里必红。
        let o = device_origin("dev-aaa");
        let real = crate::channels::session::session_id(
            crate::channels::session::DEVICE_PLATFORM,
            "dev-aaa",
        );
        assert!(
            require_own_session(&o, &real).is_ok(),
            "设备必须能中断/查看通道分配给它的真实会话"
        );
    }

    #[test]
    fn 设备只能碰自己那条会话() {
        let o = device_origin("dev-aaa");
        let own = own_session_id(&o).expect("设备有会话 id");
        assert!(require_own_session(&o, &own).is_ok());
        // 别人的会话：403
        assert!(require_own_session(&o, "im:device:dev-bbb").is_err());
        // 别人的平台（哪怕是 IM 会话）也不行
        assert!(require_own_session(&o, "im:qq:someone").is_err());
    }

    #[test]
    fn local_不受归属限制() {
        let o = hermix_core::Origin::Local;
        assert!(own_session_id(&o).is_none());
        assert!(require_own_session(&o, "im:qq:anyone").is_ok());
        assert!(require_own_session(&o, "im:device:dev-x").is_ok());
    }

    #[test]
    fn 会话id_与通道同格式_且不同设备不撞() {
        let a = own_session_id(&device_origin("dev-aaa")).unwrap();
        let b = own_session_id(&device_origin("dev-bbb")).unwrap();
        assert_ne!(a, b);
        // 与通道的 `session_id()` 逐字相等——**不能**只断言"长得像"：
        // 手拼漏了 `im:` 前缀时，闸门对真实会话永远匹配不上（手机对自己的
        // 会话也 403），而 assert_ne 照样通过。
        assert_eq!(
            a,
            crate::channels::session::session_id(
                crate::channels::session::DEVICE_PLATFORM,
                "dev-aaa"
            )
        );
        assert!(a.starts_with("im:device:"));
    }
}
