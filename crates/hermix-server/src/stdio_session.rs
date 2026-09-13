//! stdio 多路复用服务：单进程 stdin/stdout 承载多个虚拟连接。
//!
//! 协议（JSON Lines，一帧一行）：
//! - 入站 `{"channel": "stream:<session_id>", "payload": "<JSON 文本>"}`
//! - 出站同形；payload 按字符串嵌入，对端需两层 parse
//!
//! channel 命名对齐原 WS 路由语义（`stream:` / `groupchat:` / 其余为主通道），
//! 首次出现的 channel 会 spawn 对应 handler；handler 退出后该通道自动摘除，
//! 同名 channel 再次出现视为重连并新建虚拟连接。

use std::collections::HashMap;
use std::sync::Arc;

use axum::{body::Body, http::Method};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

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
}

fn parse_inbound(line: &str) -> Option<InboundFrame> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let channel = v.get("channel")?.as_str()?.to_string();
    let payload = match v.get("payload")? {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    Some(InboundFrame { channel, payload })
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

    // HTTP /shutdown handler 用 Extension(watch::Sender<bool>) 驱动 graceful
    // shutdown，主循环通过同一个 watch channel 监听关闭信号。两者共用一个
    // sender/receiver pair：HTTP 请求触发发送 true，主循环收到后退出 while。
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    let app = crate::router(state.clone()).layer(axum::Extension(shutdown_tx));

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
                    tokio::spawn(async move {
                        let reply = handle_http_frame(app, &frame.payload).await;
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
    Ok(())
}

/// 处理一帧 REST 请求：payload 形如
/// `{"method":"GET","path":"/api/...","body":"...","request_id":"n"}`。
/// 响应信封 payload 为 `{"status":S,"body":"...","enc":"utf8"|"base64"}`。
async fn handle_http_frame(app: axum::Router, payload: &str) -> String {
    let parsed: serde_json::Value = serde_json::from_str(payload).unwrap_or(serde_json::Value::Null);
    let request_id = parsed
        .get("request_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
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
    let request = match builder.body(Body::from(body.unwrap_or_default())) {
        Ok(r) => r,
        Err(e) => return respond(400, e.to_string(), "utf8", &request_id),
    };

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
            Some(("groupchat", room_id)) => {
                crate::ws::handle_ws_groupchat(transport, state, room_id.to_string()).await;
            }
            Some(("stream", session_id)) => {
                crate::ws::handle_ws_stream(transport, state, session_id.to_string()).await;
            }
            _ => {
                // 主通道：简单 request-reply 语义，session_id 用 channel 原名。
                crate::ws::handle_ws(transport, state, channel.clone()).await;
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
