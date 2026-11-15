//! Per-session broadcast registry + 进程级 envelope seq（自 ws.rs 拆分，P1-13）。
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use super::*;

/// Per-session broadcast channel registry. Lets reconnecting WS clients
/// observe events from an agent task that is already running but lost
/// its previous subscriber (transient WS drop, laptop sleep, switch tab).
///
/// The sender is created lazily on the first event for a given
/// `session_id` and dropped when the agent task naturally completes
/// (`session_broadcast_drop` in the `agent_handle` completion block) —
/// at which point any still-subscribed receivers see `Closed` and exit
/// their forward loop.
///
/// Events emitted BEFORE a subscriber attaches are NOT replayed here.
/// That's covered by the DB transcript snapshot the front-end loads at
/// reconnect time (`/api/sessions/{sid}/messages`). The broadcast is
/// only the running-progress tail.
type SessionBroadcastMap = Arc<Mutex<HashMap<String, tokio::sync::broadcast::Sender<WsEnvelope>>>>;

static SESSION_BROADCASTS: OnceLock<SessionBroadcastMap> = OnceLock::new();

fn session_broadcasts() -> &'static SessionBroadcastMap {
    SESSION_BROADCASTS.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
}

/// Get-or-create the per-session broadcast sender. Returns a cheap
/// clone of the underlying `broadcast::Sender` (which itself wraps an
/// Arc internally), so callers can stash it in a long-lived task
/// without holding the registry mutex.
pub(super) fn session_broadcast_get_or_create(
    session_id: &str,
) -> tokio::sync::broadcast::Sender<WsEnvelope> {
    let map = session_broadcasts();
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(tx) = guard.get(session_id) {
        return tx.clone();
    }
    let (tx, _rx) = tokio::sync::broadcast::channel::<WsEnvelope>(256);
    guard.insert(session_id.to_string(), tx.clone());
    tx
}

/// Drop the per-session broadcast sender. Called when the agent task
/// completes — no further events will ever be produced for this
/// session. Existing subscribers (the per-connection forward tasks
/// each holding a receiver clone) see `Closed` on their next `recv()`
/// and exit cleanly. A no-op if no sender was ever created for this
/// session (e.g. short messages that completed before any event was
/// published).
///
/// 多端并存（P5，设计 §3.5）下这个 drop 是**安全**的：调用点在 agent
/// task 的完成块（`handle_ws_stream_send` 的 `agent_handle` 分支），
/// 此时该 session 的 agent task 已结束、不会再产事件，所有订阅者
/// （无论 attachment_id 是否相同）都该退出。扇出靠 `broadcast::channel`
/// 单 sender 多 receiver 天然支持：每个 WS 连接从
/// `session_broadcast_get_or_create` 拿 sender clone 后自己 spawn 一条
/// forward task、各自持有 receiver。因此本函数无需 attachment-aware
/// 改造——agent task 完成即所有 forwarder 都该退出。
pub(super) fn session_broadcast_drop(session_id: &str) {
    let mut g = session_broadcasts().lock().unwrap_or_else(|e| e.into_inner());
    g.remove(session_id);
}

/// 取下一个进程级 seq（P4 L3，设计 §3.5）。在广播镜像发送点（
/// `session_broadcast_get_or_create` 旁）与快照端点共用同一计数器
/// ——保证重连订阅端点补发增量时与实时广播不重号。
pub(super) fn next_envelope_seq(state: &crate::state::HttpServerState) -> u64 {
    state.seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}
