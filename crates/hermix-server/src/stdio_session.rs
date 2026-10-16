//! stdio 多路复用服务：单进程 stdin/stdout 承载多个虚拟连接。
//!
//! 协议（JSON Lines，一帧一行）：
//! - 入站 `{"channel": "stream:<session_id>", "payload": "<JSON 文本>"}`
//! - 出站同形；payload 按字符串嵌入，对端需两层 parse
//!
//! channel 命名对齐原 WS 路由语义（`stream:` / 其余为主通道），
//! 首次出现的 channel 会 spawn 对应 handler；handler 退出后该通道自动摘除，
//! 同名 channel 再次出现视为重连并新建虚拟连接。

use std::collections::HashMap;
use std::sync::Arc;

use axum::{body::Body, http::Method};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc};

use crate::state::HttpServerState;
use crate::transport::{ChannelSink, ChannelSource, Transport};

const OUTBOUND_BUFFER: usize = 256;
/// oneshot 响应体收集上限（与 HTTP 层 body limit 同量级，覆盖大文件/大结果导出场景）。
const HTTP_RESPONSE_LIMIT: usize = 64 * 1024 * 1024;

/// 入站帧：信封拆包失败或 channel 缺失时忽略并告警（对端 bug，不应崩溃服务）。
#[derive(Debug)]
struct InboundFrame {
    channel: String,
    payload: String,
    /// 信封级 request_id。桥以它为键配对 REST 应答（`pending_http`）；
    /// 不回填则应答永远带空 id，桥找不到 waiter，每个请求都会挂到
    /// 120s 超时才失败——前端表现为"发送失败 / 后端未启动"。
    request_id: Option<String>,
}

fn parse_inbound(line: &str) -> Option<InboundFrame> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let channel = v.get("channel")?.as_str()?.to_string();
    let payload = match v.get("payload")? {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let request_id = v
        .get("request_id")
        .and_then(|r| r.as_str())
        .filter(|r| !r.is_empty())
        .map(String::from);
    Some(InboundFrame { channel, payload, request_id })
}

/// stdio 主循环：读 stdin → 路由到虚拟连接；各虚拟连接的出站统一写 stdout。
pub async fn run_stdio_server(state: HttpServerState) -> Result<(), hermix_core::AgentError> {
    // 汇聚 writer：所有 ChannelSink 都把信封塞进这条队列，单一 task 顺序写
    // stdout，避免多 handler 并发写导致行交错。
    let (writer_tx, mut writer_rx) = mpsc::channel::<String>(OUTBOUND_BUFFER);
    tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(line) = writer_rx.recv().await {
            if stdout.write_all(line.as_bytes()).await.is_err()
                || stdout.write_all(b"\n").await.is_err()
                || stdout.flush().await.is_err()
            {
                // stdout 断了（父进程退出）——静默退出 writer；stdin 读循环
                // 也会随之结束。
                break;
            }
        }
    });

    // 浏览器小窗帧转发：会话（headless Edge + screencast）跑在 sidecar 进程，
    // 桌面端 browser_view_controller 读自己的 screencast::receiver() 永远是 None
    // （进程内 static 单例互不共享）。这里订阅本进程的 receiver，把帧经 stdout
    // 推给桌面端——std::sync 层面只有一个 sidecar，跨进程的桥就是这条 stdio。
    spawn_browser_frame_forwarder(writer_tx.clone());

    // HTTP /shutdown handler 用 Extension(watch::Sender<bool>) 驱动 graceful
    // shutdown，主循环通过同一个 watch channel 监听关闭信号。两者共用一个
    // sender/receiver pair：HTTP 请求触发发送 true，主循环收到后退出 while。
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    let app = crate::router(state.clone()).layer(axum::Extension(shutdown_tx));

    // IM 通道（QQ / 企业微信）。HTTP 模式在 run_server 里起，stdio（桌面端
    // IPC）此前漏了——重启应用后通道不自动拉起，状态接口永远返回
    // connected=false 且无故障原文。凭据成对齐全才启（ImRuntime::start 自己判断）。
    let im_runtime = crate::channels::ImRuntime::start(state.clone());
    crate::channels::install_runtime(im_runtime.clone());

    // 虚拟连接注册表：channel → 该连接的入站队列发送端。
    let connections: Arc<
        tokio::sync::Mutex<HashMap<String, mpsc::Sender<String>>>,
    > = Arc::new(tokio::sync::Mutex::new(HashMap::new()));

    let mut stdin = BufReader::new(tokio::io::stdin()).lines();
    loop {
        // 用 futures::select 同时监听 stdin 与 shutdown watch；任何一方就绪
        // 都继续下一次迭代：shutdown 信号 → break；stdin 帧 → 正常路由。
        tokio::select! {
            biased;
            line = stdin.next_line() => {
                let line = match line {
                    Ok(Some(line)) => line,
                    Ok(None) => break, // stdin 关闭（父进程退出）→ 自然退出。
                    Err(e) => {
                        tracing::warn!("stdio: stdin read error, exiting: {e}");
                        break;
                    }
                };
                let Some(frame) = parse_inbound(&line) else {
                    tracing::warn!(len = line.len(), "stdio: unparseable inbound frame, ignored");
                    continue;
                };

                // shutdown 帧：客户端主动请求优雅退出，立即终止循环。
                if frame.channel == "shutdown" {
                    tracing::info!("stdio: shutdown frame received, exiting gracefully");
                    break;
                }

                // REST 通道：request-response 语义，不走虚拟连接，直接 oneshot。
                if frame.channel == "http" {
                    let app = app.clone();
                    let writer = writer_tx.clone();
                    // 信封级 request_id 透传进 handler，由它回填到应答信封，
                    // 桥才能按 id 配对（否则应答 request_id 为空，请求悬挂）。
                    let request_id = frame.request_id.clone();
                    tokio::spawn(async move {
                        let reply = handle_http_frame(app, &frame.payload, request_id.as_deref()).await;
                        let _ = writer.send(reply).await;
                    });
                    continue;
                }

                let mut conns = connections.lock().await;
                if !conns.contains_key(&frame.channel) {
                    let (inbound_tx, inbound_rx) = mpsc::channel::<String>(OUTBOUND_BUFFER);
                    conns.insert(frame.channel.clone(), inbound_tx);

                    let sink = ChannelSink::new(frame.channel.clone(), writer_tx.clone());
                    let source = ChannelSource::new(inbound_rx);
                    spawn_connection_handler(state.clone(), frame.channel.clone(), sink, source, connections.clone());
                }
                // 虚拟连接尚未消费完队列时丢弃策略：mpsc 满则 send 失败，
                // 此处静默丢弃并告警（对端应限速，与 WS 背压语义一致）。
                if let Some(tx) = conns.get(&frame.channel) {
                    if tx.send(frame.payload).await.is_err() {
                        tracing::warn!(channel = %frame.channel, "stdio: inbound queue closed, frame dropped");
                    }
                }
            }
            _ = shutdown_rx.changed() => {
                tracing::info!("stdio: shutdown signal via watch channel, exiting");
                break;
            }
        }
    }

    tracing::info!("stdio: stdin closed, exiting");
    if let Some(rt) = im_runtime.as_ref() {
        rt.stop().await;
    }
    Ok(())
}

/// 浏览器小窗帧转发器（sidecar 进程内）。
///
/// 浏览器会话（headless Edge + screencast）只在 sidecar 进程建立，桌面进程
/// 读不到进程内单例。本任务多路轮询**本进程**所有活动会话的 `receiver(key)`
/// （键 = `_session_id`，多会话并行时各路帧流独立），把每帧 JPEG 以
/// `channel="browser-frame"`、payload 带 `session` 键的信封经 stdout 写回
/// 桌面端——桌面端 `stdio_bridge` 读线程转成 `stdio-frame` 事件，前端
/// `BrowserView` 按会话切换显示。
///
/// 没有会话时 100ms 轮询；会话建立后对每个 key 非阻塞排空 broadcast
/// （Lagged 即丢帧——监控流宁可跳帧不可积压）。
fn spawn_browser_frame_forwarder(writer_tx: mpsc::Sender<String>) {
    tokio::spawn(async move {
        use base64::Engine as _;
        use hermix_tools::tools::os_auto::browser::screencast;
        let mut receivers: HashMap<String, broadcast::Receiver<Arc<Vec<u8>>>> =
            HashMap::new();
        loop {
            let keys = screencast::active_keys().await;
            // 清掉已消失会话的 receiver。
            receivers.retain(|k, _| keys.contains(k));
            if keys.is_empty() {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                continue;
            }
            let mut forwarded = 0usize;
            for key in &keys {
                if !receivers.contains_key(key) {
                    if let Some(rx) = screencast::receiver(key).await {
                        receivers.insert(key.clone(), rx);
                    }
                    continue;
                }
                let rx = receivers.get_mut(key).expect("checked above");
                loop {
                    match rx.try_recv() {
                        Ok(frame) => {
                            let b64 = base64::engine::general_purpose::STANDARD.encode(&*frame);
                            let payload = serde_json::json!({
                                "data": format!("data:image/jpeg;base64,{b64}"),
                                "bytes": frame.len(),
                                "session": key,
                            })
                            .to_string();
                            let envelope = serde_json::json!({
                                "channel": "browser-frame",
                                "payload": payload,
                            })
                            .to_string();
                            // writer 队列满（前端掉线/卡顿）→ 丢旧帧保新帧：监控帧不需要积压。
                            // 队列已关闭（stdout 断）→ 退出转发任务。
                            match writer_tx.try_send(envelope) {
                                Ok(()) => forwarded += 1,
                                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
                                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => return,
                            }
                        }
                        Err(broadcast::error::TryRecvError::Empty) => break,
                        Err(broadcast::error::TryRecvError::Lagged(_)) => continue, // 丢帧保新
                        Err(broadcast::error::TryRecvError::Closed) => {
                            receivers.remove(key);
                            break;
                        }
                    }
                }
            }
            // 有活动会话时高频排空（20fps×N 路），空闲再降频。
            let idle_ms = if forwarded > 0 { 50 } else { 200 };
            tokio::time::sleep(std::time::Duration::from_millis(idle_ms)).await;
        }
    });
}

/// 处理一帧 REST 请求：payload 形如
/// `{"method":"GET","path":"/api/...","body":"...","request_id":"n"}`。
/// 响应信封 payload 为 `{"status":S,"body":"...","enc":"utf8"|"base64"}`。
async fn handle_http_frame(
    app: axum::Router,
    payload: &str,
    envelope_request_id: Option<&str>,
) -> String {
    let parsed: serde_json::Value = serde_json::from_str(payload).unwrap_or(serde_json::Value::Null);
    // 优先用信封级 id：桥以信封 id 为配对键，payload 内的同名字段是历史
    // 遗留（HTTP 时代客户端自带），两者不一致时必须以信封为准。
    let request_id = envelope_request_id
        .map(String::from)
        .or_else(|| {
            parsed
                .get("request_id")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .unwrap_or_default();
    let method = parsed
        .get("method")
        .and_then(|v| v.as_str())
        .and_then(|m| Method::from_bytes(m.as_bytes()).ok())
        .unwrap_or(Method::GET);
    let path = parsed.get("path").and_then(|v| v.as_str()).unwrap_or("/");
    let body: Option<String> = parsed
        .get("body")
        .and_then(|v| v.as_str())
        .map(String::from);

    let respond = |status: u16, body: String, enc: &str, request_id: &str| -> String {
        serde_json::json!({
            "channel": "http",
            "request_id": request_id,
            "payload": serde_json::json!({"status": status, "body": body, "enc": enc}).to_string(),
        })
        .to_string()
    };

    let mut builder = axum::http::Request::builder().method(method).uri(path);
    if body.is_some() {
        // axum 的 Json extractor 校验 content-type；统一按 JSON 请求体处理。
        builder = builder.header("content-type", "application/json");
    }
    let mut request = match builder.body(Body::from(body.unwrap_or_default())) {
        Ok(r) => r,
        Err(e) => return respond(400, e.to_string(), "utf8", &request_id),
    };
    // stdio 桥是桌面端进程自身的管道（sidecar 的 stdin 只可能被本机
    // Tauri 壳写入），没有 TCP 对端可取 ConnectInfo。注入 loopback 合成
    // ConnectInfo：client_ip() 返回 127.0.0.1 → derive_origin 判 Local
    // （桌面 = 本机，T1 关口语义下最可信来源，与 stdio 迁移前的 TCP
    // loopback 行为一致）。缺这一步时 request_guard 的 T1 关口会把
    // 桌面端自己的全部 REST 请求判 Unpaired 而 403。
    if let Ok(peer) = "127.0.0.1:0".parse::<std::net::SocketAddr>() {
        request.extensions_mut().insert(axum::extract::ConnectInfo(peer));
    }

    match tower::ServiceExt::oneshot(app, request).await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            match axum::body::to_bytes(resp.into_body(), HTTP_RESPONSE_LIMIT).await {
                Ok(bytes) => match std::str::from_utf8(&bytes) {
                    Ok(s) => respond(status, s.to_string(), "utf8", &request_id),
                    Err(_) => {
                        use base64::Engine as _;
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                        respond(status, b64, "base64", &request_id)
                    }
                },
                Err(e) => respond(502, format!("collect body: {e}"), "utf8", &request_id),
            }
        }
        Err(e) => respond(500, e.to_string(), "utf8", &request_id),
    }
}

/// 按 channel 前缀选择 handler 并 spawn。通道摘除由 connections 锁在
/// handler 结束后完成（简化：handler 结束时移除注册表条目）。
fn spawn_connection_handler(
    state: HttpServerState,
    channel: String,
    sink: ChannelSink,
    source: ChannelSource,
    connections: Arc<tokio::sync::Mutex<HashMap<String, mpsc::Sender<String>>>>,
) {
    // 把 (sink, source) 拆进 handler 的泛型接口：handler 接收 impl Transport，
    // 这里用一个小适配 struct 承载 ChannelSink/ChannelSource 的组合。
    let transport = ChannelTransport { sink, source };

    let registry = connections;
    tokio::spawn(async move {
        let channel_for_log = channel.clone();
        match channel.split_once(':') {
            Some(("stream", session_id)) => {
                crate::ws::handle_ws_stream(transport, state, session_id.to_string()).await;
            }
            _ => {
                // 主通道：简单 request-reply 语义，session_id 用 channel 原名。
                crate::ws::handle_ws(
                    transport,
                    state,
                    channel.clone(),
                    // Local stdio pipe: the operator driving this channel
                    // already has host shell access, so a Dangerous tool
                    // is not a privilege escalation. Network transports
                    // pass Deny instead.
                    hermix_core::approval::ApprovalFallback::Allow,
                )
                .await;
            }
        }
        registry.lock().await.remove(&channel_for_log);
    });
}

/// (ChannelSink, ChannelSource) 的组合，满足 `Transport` 以复用现有 handler 签名。
struct ChannelTransport {
    sink: ChannelSink,
    source: ChannelSource,
}

impl Transport for ChannelTransport {
    fn split(self) -> (Box<dyn crate::transport::TransportSink>, Box<dyn crate::transport::TransportSource>) {
        (Box::new(self.sink), Box::new(self.source))
    }
}

#[cfg(test)]
mod envelope_tests {
    use super::*;

    /// 信封级 request_id 必须被解析出来。桥以它为键配对 REST 应答；
    /// 丢了这个字段，应答会带空 id，桥永远找不到 waiter，每个请求都
    /// 要等满 120s 超时才失败（前端表现为"后端不可达"）。
    #[test]
    fn parse_inbound_keeps_envelope_request_id() {
        let line = r#"{"channel":"http","request_id":"42","payload":"{\"method\":\"GET\"}"}"#;
        let f = parse_inbound(line).expect("frame parses");
        assert_eq!(f.request_id.as_deref(), Some("42"));
        assert_eq!(f.channel, "http");
    }

    /// 非 http 通道（stream）没有 request_id 是正常的，
    /// 不应因此丢掉整帧。
    #[test]
    fn parse_inbound_tolerates_missing_request_id() {
        let line = r#"{"channel":"stream:s1","payload":"{}"}"#;
        let f = parse_inbound(line).expect("frame parses");
        assert!(f.request_id.is_none());
    }

    /// 空字符串等同于缺失：桥不会用空串做配对键，
    /// 回一个空 id 与不回没有区别。
    #[test]
    fn parse_inbound_treats_empty_request_id_as_absent() {
        let line = r#"{"channel":"http","request_id":"","payload":"{}"}"#;
        let f = parse_inbound(line).expect("frame parses");
        assert!(f.request_id.is_none());
    }

    /// T1 回归保护：`handle_http_frame` 派发的请求必须注入合成 loopback
    /// `ConnectInfo`。缺它时 `request_guard::client_ip` 返回 None →
    /// `derive_origin` 判 `Unpaired` → T1 关口把桌面端自己的全部 REST
    /// 请求 403（真机故障：桌面端除 /health 与配对端点外全被拦）。
    /// 驱动方式：最小 router 的 handler 用 `ConnectInfo` 提取器读对端，
    /// 注入缺失时 axum 返回 500（extension 不存在），断言必红。
    #[tokio::test]
    async fn http_frame_injects_loopback_connect_info() {
        use axum::extract::ConnectInfo;
        use std::net::SocketAddr;

        let app = axum::Router::new().route(
            "/echo-peer",
            axum::routing::get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move {
                peer.to_string()
            }),
        );
        let resp = handle_http_frame(
            app,
            r#"{"method":"GET","path":"/echo-peer"}"#,
            Some("1"),
        )
        .await;
        let envelope: serde_json::Value = serde_json::from_str(&resp).unwrap();
        let payload: serde_json::Value =
            serde_json::from_str(envelope["payload"].as_str().unwrap()).unwrap();
        assert_eq!(payload["status"], 200, "ConnectInfo 已注入 → 200");
        assert_eq!(payload["body"], "127.0.0.1:0", "对端是注入的合成 loopback");
    }
}
