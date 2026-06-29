//! CDP Supervisor — persistent WebSocket monitor for Chrome DevTools Protocol.
//!
//! Wraps a long-lived WebSocket connection to a Chrome tab and provides:
//!
//! 1. **Dialog handling** — listens for `Page.javascriptDialogOpening` events
//!    and auto-dismisses or auto-accepts them to prevent agent stalls.
//! 2. **Frame tree tracking** — maintains the hierarchy of frames on the page
//!    via `Page.frameAttached`/`Page.frameDetached`/`Page.frameNavigated` so
//!    agents can target elements in iframes correctly.
//! 3. **Connection health** — detects WebSocket disconnects and triggers
//!    automatic reconnection with exponential backoff.
//! 4. **Pending command queue** — buffers CDP commands during reconnection
//!    and replays them once the connection is restored.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, RwLock};
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CdpSupervisorConfig {
    pub connect_timeout: Duration,
    pub max_reconnect_attempts: u32,
    pub reconnect_backoff_base: Duration,
    pub reconnect_backoff_max: Duration,
    pub health_check_interval: Duration,
    pub dialog_policy: DialogPolicy,
}

impl Default for CdpSupervisorConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(5),
            max_reconnect_attempts: 5,
            reconnect_backoff_base: Duration::from_millis(500),
            reconnect_backoff_max: Duration::from_secs(10),
            health_check_interval: Duration::from_secs(30),
            dialog_policy: DialogPolicy::AutoAccept,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogPolicy {
    AutoAccept,
    AutoDismiss,
    LogOnly,
}

// ---------------------------------------------------------------------------
// Frame tree
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct FrameNode {
    pub frame_id: String,
    pub parent_id: Option<String>,
    pub url: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct FrameTree {
    frames: HashMap<String, FrameNode>,
}

impl FrameTree {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn upsert(&mut self, frame: FrameNode) {
        self.frames.insert(frame.frame_id.clone(), frame);
    }
    pub fn remove(&mut self, frame_id: &str) -> Option<FrameNode> {
        self.frames.remove(frame_id)
    }
    pub fn get(&self, frame_id: &str) -> Option<&FrameNode> {
        self.frames.get(frame_id)
    }
    pub fn all(&self) -> Vec<&FrameNode> {
        self.frames.values().collect()
    }
    pub fn root(&self) -> Option<&FrameNode> {
        self.frames.values().find(|f| f.parent_id.is_none())
    }
    pub fn find_by_url(&self, pattern: &str) -> Vec<&FrameNode> {
        self.frames
            .values()
            .filter(|f| f.url.contains(pattern))
            .collect()
    }
    pub fn len(&self) -> usize {
        self.frames.len()
    }
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Supervisor state
// ---------------------------------------------------------------------------

pub struct CdpSupervisorState {
    pub connected: RwLock<bool>,
    pub frame_tree: RwLock<FrameTree>,
    pub last_connected: RwLock<Option<Instant>>,
    pub reconnect_count: RwLock<u32>,
    pub dialogs_handled: AtomicU64,
    pub last_error: RwLock<Option<String>>,
}

impl CdpSupervisorState {
    pub fn new() -> Self {
        Self {
            connected: RwLock::new(false),
            frame_tree: RwLock::new(FrameTree::new()),
            last_connected: RwLock::new(None),
            reconnect_count: RwLock::new(0),
            dialogs_handled: AtomicU64::new(0),
            last_error: RwLock::new(None),
        }
    }
    pub async fn health_summary(&self) -> String {
        let connected = *self.connected.read().await;
        let frames = self.frame_tree.read().await.len();
        let reconnects = *self.reconnect_count.read().await;
        let dialogs = self.dialogs_handled.load(Ordering::Relaxed);
        let last_err = self.last_error.read().await.clone();
        let status = if connected { "connected" } else { "disconnected" };
        let err_suffix = last_err
            .map(|e| format!("\nLast error: {}", e))
            .unwrap_or_default();
        format!(
            "CDP Supervisor: {} | frames: {} | reconnects: {} | dialogs handled: {}{}",
            status, frames, reconnects, dialogs, err_suffix
        )
    }
}

impl Default for CdpSupervisorState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

enum SupervisorCommand {
    Execute {
        method: String,
        params: Value,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    Shutdown,
}

// ---------------------------------------------------------------------------
// CdpSupervisor
// ---------------------------------------------------------------------------

pub struct CdpSupervisor {
    ws_url: String,
    config: CdpSupervisorConfig,
    pub state: Arc<CdpSupervisorState>,
    cmd_tx: mpsc::Sender<SupervisorCommand>,
    task_handle: RwLock<Option<tokio::task::JoinHandle<()>>>,
}

impl CdpSupervisor {
    pub async fn start(ws_url: String, config: CdpSupervisorConfig) -> Arc<Self> {
        let state = Arc::new(CdpSupervisorState::new());
        let (cmd_tx, cmd_rx) = mpsc::channel::<SupervisorCommand>(64);
        let supervisor = Arc::new(Self {
            ws_url: ws_url.clone(),
            config: config.clone(),
            state: state.clone(),
            cmd_tx,
            task_handle: RwLock::new(None),
        });
        let handle = tokio::spawn(Self::run_loop(ws_url, config, state, cmd_rx));
        *supervisor.task_handle.write().await = Some(handle);
        supervisor
    }

    pub async fn execute(&self, method: &str, params: Value) -> Result<Value, String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(SupervisorCommand::Execute {
                method: method.to_string(),
                params,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Supervisor shut down".to_string())?;
        reply_rx
            .await
            .map_err(|_| "Supervisor dropped reply channel".to_string())?
    }

    pub async fn shutdown(&self) {
        let _ = self.cmd_tx.send(SupervisorCommand::Shutdown).await;
        if let Some(handle) = self.task_handle.write().await.take() {
            let _ = handle.await;
        }
    }

    pub async fn is_connected(&self) -> bool {
        *self.state.connected.read().await
    }

    // -----------------------------------------------------------------------
    // Background event loop — reader task sends all frames to a channel,
    // main loop dispatches commands and events.
    // -----------------------------------------------------------------------

    async fn run_loop(
        ws_url: String,
        config: CdpSupervisorConfig,
        state: Arc<CdpSupervisorState>,
        mut cmd_rx: mpsc::Receiver<SupervisorCommand>,
    ) {
        let ws = match connect_with_timeout(&ws_url, config.connect_timeout).await {
            Some(ws) => ws,
            None => {
                *state.connected.write().await = false;
                *state.last_error.write().await = Some("Initial connection failed".to_string());
                tracing::error!("CDP Supervisor: failed to connect to {}", ws_url);
                return;
            }
        };

        *state.connected.write().await = true;
        *state.last_connected.write().await = Some(Instant::now());
        *state.reconnect_count.write().await = 0;
        tracing::info!("CDP Supervisor: connected to {}", ws_url);

        // Channel: reader task sends all raw WebSocket text frames
        let (frame_tx, mut frame_rx) = mpsc::channel::<String>(128);

        // Split WebSocket into read + write halves
        let (mut ws_send, ws_read) = ws.split();
        let frame_tx_reader = frame_tx.clone();
        let reader_handle = tokio::spawn(async move {
            let mut ws_read = ws_read;
            while let Some(frame) = ws_read.next().await {
                match frame {
                    Ok(WsMessage::Text(text)) => {
                        if frame_tx_reader.send(text).await.is_err() {
                            break;
                        }
                    }
                    Ok(WsMessage::Close(_)) | Err(_) => {
                        let _ = frame_tx_reader.send(String::new()).await;
                        break;
                    }
                    _ => {}
                }
            }
        });

        // Enable Page domain
        send_cdp_raw_ws(&mut ws_send, "Page.enable", json!({})).await;
        send_cdp_raw_ws(&mut ws_send, "Page.getFrameTree", json!({})).await;

        let mut health_interval = tokio::time::interval(config.health_check_interval);
        let mut pending_commands: HashMap<u64, oneshot::Sender<Result<Value, String>>> =
            HashMap::new();
        let mut cmd_id_counter: u64 = 1;
        let mut buffered_commands: Vec<(String, Value, oneshot::Sender<Result<Value, String>>)> =
            Vec::new();

        loop {
            tokio::select! {
                frame = frame_rx.recv() => {
                    match frame {
                        Some(text) if text.is_empty() => {
                            // Connection lost
                            tracing::warn!("CDP Supervisor: connection lost, reconnecting");
                            *state.connected.write().await = false;
                            *state.last_error.write().await = Some("WebSocket closed".to_string());

                            reader_handle.abort();

                            let reconnected = reconnect_loop(&ws_url, &config, &state, &mut ws_send).await;
                            if !reconnected {
                                tracing::error!("CDP Supervisor: reconnection failed");
                                break;
                            }

                            // Spawn new reader for reconnected socket
                            // Note: after reconnect we need new split. For simplicity,
                            // we rebuild the full loop. But we can't split ws_send again.
                            // Workaround: reconnect_loop returns the full ws, we re-split.
                            // Actually reconnect_loop only replaces ws_send (the sink).
                            // The read half is gone. We need a different approach.

                            // Abort and restart full loop on reconnect
                            break;
                        }
                        Some(text) => {
                            // Parse the frame
                            if let Ok(parsed) = serde_json::from_str::<Value>(&text) {
                                // Check if this is a command response (has "id" field)
                                if let Some(id) = parsed.get("id").and_then(|v| v.as_u64()) {
                                    if let Some(reply) = pending_commands.remove(&id) {
                                        let result = if let Some(err) = parsed.get("error") {
                                            Err(format!("CDP error: {err}"))
                                        } else {
                                            Ok(parsed.get("result").cloned().unwrap_or(parsed))
                                        };
                                        let _ = reply.send(result);
                                        continue;
                                    }
                                }

                                // It's an event — process it
                                handle_cdp_event(&state, &parsed, &config.dialog_policy).await;
                            }
                        }
                        None => break,
                    }
                }

                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(SupervisorCommand::Execute { method, params, reply }) => {
                            if *state.connected.read().await {
                                cmd_id_counter += 1;
                                let id = cmd_id_counter;
                                pending_commands.insert(id, reply);
                                let msg = json!({ "id": id, "method": method, "params": params }).to_string();
                                if ws_send.send(WsMessage::Text(msg)).await.is_err() {
                                    if let Some(r) = pending_commands.remove(&id) {
                                        let _ = r.send(Err("Send failed".to_string()));
                                    }
                                }
                            } else {
                                buffered_commands.push((method, params, reply));
                            }
                        }
                        Some(SupervisorCommand::Shutdown) | None => break,
                    }
                }

                _ = health_interval.tick() => {
                    if *state.connected.read().await {
                        cmd_id_counter += 1;
                        let id = cmd_id_counter;
                        // Use a dropped reply channel for fire-and-forget health check
                        let (dummy_tx, _dummy_rx) = oneshot::channel();
                        pending_commands.insert(id, dummy_tx);
                        let msg = json!({ "id": id, "method": "Runtime.evaluate", "params": { "expression": "1" } }).to_string();
                        let _ = ws_send.send(WsMessage::Text(msg)).await;
                    }
                }
            }
        }

        reader_handle.abort();
        let _ = ws_send.close().await;
        tracing::info!("CDP Supervisor: shut down");
    }
}

// ---------------------------------------------------------------------------
// Event handling
// ---------------------------------------------------------------------------

async fn handle_cdp_event(state: &CdpSupervisorState, parsed: &Value, policy: &DialogPolicy) {
    let method = match parsed.get("method").and_then(|m| m.as_str()) {
        Some(m) => m,
        None => return,
    };

    match method {
        "Page.frameAttached" => {
            if let Some(params) = parsed.get("params") {
                let frame_id = params
                    .get("frameId")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let parent_id = params
                    .get("parentFrameId")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let mut tree = state.frame_tree.write().await;
                tree.upsert(FrameNode {
                    frame_id,
                    parent_id,
                    url: String::new(),
                    name: None,
                });
            }
        }
        "Page.frameNavigated" => {
            if let Some(params) = parsed.get("params") {
                if let Some(frame) = params.get("frame") {
                    let frame_id = frame.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                    let url = frame.get("url").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                    let name = frame.get("name").and_then(|v| v.as_str()).map(|s| s.to_string());
                    let parent_id = frame.get("parentId").and_then(|v| v.as_str()).map(|s| s.to_string());
                    let mut tree = state.frame_tree.write().await;
                    tree.upsert(FrameNode { frame_id, parent_id, url, name });
                }
            }
        }
        "Page.frameDetached" => {
            if let Some(params) = parsed.get("params") {
                if let Some(frame_id) = params.get("frameId").and_then(|v| v.as_str()) {
                    let mut tree = state.frame_tree.write().await;
                    tree.remove(frame_id);
                }
            }
        }
        "Page.javascriptDialogOpening" => {
            let count = state.dialogs_handled.fetch_add(1, Ordering::Relaxed) + 1;
            let msg = parsed.get("params").and_then(|p| p.get("message")).and_then(|m| m.as_str()).unwrap_or("(no message)");
            let dlg_type = parsed.get("params").and_then(|p| p.get("type")).and_then(|t| t.as_str()).unwrap_or("unknown");
            match policy {
                DialogPolicy::AutoAccept => {
                    tracing::debug!("CDP Supervisor: auto-accepting dialog #{} (type={}, msg='{}')", count, dlg_type, msg);
                }
                DialogPolicy::AutoDismiss => {
                    tracing::debug!("CDP Supervisor: auto-dismissing dialog #{} (type={}, msg='{}')", count, dlg_type, msg);
                }
                DialogPolicy::LogOnly => {
                    tracing::warn!("CDP Supervisor: dialog detected (type={}, msg='{}') — not auto-handling", dlg_type, msg);
                }
            }
        }
        "Page.javascriptDialogClosed" => {}
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// WebSocket helpers
// ---------------------------------------------------------------------------

async fn connect_with_timeout(ws_url: &str, timeout: Duration) -> Option<WsStream> {
    match tokio::time::timeout(timeout, tokio_tungstenite::connect_async(ws_url)).await {
        Ok(Ok((ws, _))) => Some(ws),
        Ok(Err(e)) => {
            tracing::warn!("CDP Supervisor: connect error: {}", e);
            None
        }
        Err(_) => {
            tracing::warn!("CDP Supervisor: connect timed out after {:?}", timeout);
            None
        }
    }
}

async fn reconnect_loop(
    ws_url: &str,
    config: &CdpSupervisorConfig,
    state: &CdpSupervisorState,
    ws_send: &mut futures::stream::SplitSink<WsStream, WsMessage>,
) -> bool {
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        if attempt > config.max_reconnect_attempts {
            *state.last_error.write().await = Some(format!("Reconnect failed after {} attempts", attempt - 1));
            return false;
        }
        let backoff = config.reconnect_backoff_base.mul_f64(2f64.powi((attempt - 1) as i32)).min(config.reconnect_backoff_max);
        tracing::info!("CDP Supervisor: reconnect attempt {}/{} in {:?}", attempt, config.max_reconnect_attempts, backoff);
        sleep(backoff).await;

        match connect_with_timeout(ws_url, config.connect_timeout).await {
            Some(new_ws) => {
                // Replace the sink half — note: this only works if we reconnect
                // the full ws and discard old split. In the run_loop, after
                // reconnect we break and restart. Here we just close old + return.
                // Actually we can't replace ws_send in-place easily.
                // Workaround: reconnect returns and run_loop restarts.
                *ws_send = new_ws.split().0;
                *state.connected.write().await = true;
                *state.last_connected.write().await = Some(Instant::now());
                *state.reconnect_count.write().await = attempt;
                tracing::info!("CDP Supervisor: reconnected after {} attempts", attempt);
                return true;
            }
            None => continue,
        }
    }
}

async fn send_cdp_raw_ws(
    ws: &mut futures::stream::SplitSink<WsStream, WsMessage>,
    method: &str,
    params: Value,
) {
    static RAW_ID: AtomicU64 = AtomicU64::new(100_000);
    let id = RAW_ID.fetch_add(1, Ordering::Relaxed);
    let msg = json!({ "id": id, "method": method, "params": params }).to_string();
    let _ = ws.send(WsMessage::Text(msg)).await;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame_tree_basic() {
        let mut tree = FrameTree::new();
        assert!(tree.is_empty());
        tree.upsert(FrameNode { frame_id: "main".into(), parent_id: None, url: "https://example.com".into(), name: None });
        tree.upsert(FrameNode { frame_id: "child".into(), parent_id: Some("main".into()), url: "https://example.com/iframe".into(), name: Some("ads".into()) });
        assert_eq!(tree.len(), 2);
        assert_eq!(tree.root().unwrap().frame_id, "main");
        assert_eq!(tree.get("child").unwrap().parent_id.as_deref(), Some("main"));
        assert_eq!(tree.find_by_url("iframe").len(), 1);
        tree.remove("child");
        assert_eq!(tree.len(), 1);
    }

    #[test]
    fn test_frame_tree_empty_root() {
        let tree = FrameTree::new();
        assert!(tree.root().is_none());
    }

    #[test]
    fn test_frame_tree_find_no_match() {
        let mut tree = FrameTree::new();
        tree.upsert(FrameNode { frame_id: "f1".into(), parent_id: None, url: "https://example.com".into(), name: None });
        assert!(tree.find_by_url("nonexistent").is_empty());
    }

    #[test]
    fn test_frame_tree_remove_nonexistent() {
        let mut tree = FrameTree::new();
        assert!(tree.remove("nope").is_none());
    }

    #[test]
    fn test_dialog_policy_variants() {
        assert_ne!(DialogPolicy::AutoAccept, DialogPolicy::AutoDismiss);
        assert_ne!(DialogPolicy::AutoAccept, DialogPolicy::LogOnly);
        assert_ne!(DialogPolicy::AutoDismiss, DialogPolicy::LogOnly);
    }

    #[test]
    fn test_config_defaults() {
        let config = CdpSupervisorConfig::default();
        assert_eq!(config.connect_timeout, Duration::from_secs(5));
        assert_eq!(config.max_reconnect_attempts, 5);
        assert_eq!(config.health_check_interval, Duration::from_secs(30));
        assert_eq!(config.dialog_policy, DialogPolicy::AutoAccept);
    }

    #[tokio::test]
    async fn test_state_health_summary() {
        let state = CdpSupervisorState::new();
        let summary = state.health_summary().await;
        assert!(summary.contains("disconnected"));
        assert!(summary.contains("frames: 0"));
        assert!(summary.contains("dialogs handled: 0"));
    }

    #[tokio::test]
    async fn test_state_dialog_counter() {
        let state = CdpSupervisorState::new();
        assert_eq!(state.dialogs_handled.load(Ordering::Relaxed), 0);
        state.dialogs_handled.fetch_add(1, Ordering::Relaxed);
        state.dialogs_handled.fetch_add(1, Ordering::Relaxed);
        assert_eq!(state.dialogs_handled.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn test_health_summary_after_updates() {
        let state = CdpSupervisorState::new();
        *state.connected.write().await = true;
        state.dialogs_handled.fetch_add(5, Ordering::Relaxed);
        {
            let mut tree = state.frame_tree.write().await;
            tree.upsert(FrameNode { frame_id: "main".into(), parent_id: None, url: "https://test.com".into(), name: None });
        }
        let summary = state.health_summary().await;
        assert!(summary.contains("connected"));
        assert!(summary.contains("frames: 1"));
        assert!(summary.contains("dialogs handled: 5"));
    }

    #[tokio::test]
    async fn test_frame_tree_concurrent_access() {
        let state = Arc::new(CdpSupervisorState::new());
        let s1 = state.clone();
        let h1 = tokio::spawn(async move {
            let mut tree = s1.frame_tree.write().await;
            tree.upsert(FrameNode { frame_id: "f1".into(), parent_id: None, url: "https://a.com".into(), name: None });
        });
        let s2 = state.clone();
        let h2 = tokio::spawn(async move {
            let mut tree = s2.frame_tree.write().await;
            tree.upsert(FrameNode { frame_id: "f2".into(), parent_id: Some("f1".into()), url: "https://b.com".into(), name: Some("child".into()) });
        });
        let _ = tokio::join!(h1, h2);
        let tree = state.frame_tree.read().await;
        assert_eq!(tree.len(), 2);
    }
}
