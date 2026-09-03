//! 传输层抽象：解耦「客户端 ↔ 服务端」全双工通道与具体传输方式。
//!
//! handler 只依赖本模块的 trait，不感知底层是 WebSocket（TCP）还是 stdio 管道。
//! 拆成独立的入/出站半，是为了让 handler 能并发读写——审批响应等 out-of-band
//! 消息必须在推流的同时被读取，否则会堆积在接收缓冲区直到超时。

use async_trait::async_trait;
use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines, Stdin, Stdout};

pub type TransportError = Box<dyn std::error::Error + Send + Sync>;
pub type TransportResult<T> = Result<T, TransportError>;

/// 出站半：服务端 → 客户端，逐条发送 JSON 文本。
#[async_trait]
pub trait TransportSink: Send {
    async fn send(&mut self, text: String) -> TransportResult<()>;
    /// 传输层保活探针。WS 实现发协议级 Ping 帧刷新 NAT/中间盒空闲计时器，
    /// 并通过发送失败及早发现死连接；无帧概念的传输（stdio）默认空操作。
    async fn ping(&mut self) -> TransportResult<()> {
        Ok(())
    }
    /// 正常关闭出站方向；缓冲区刷新失败即视为错误。
    async fn close(&mut self) -> TransportResult<()>;
}

/// 入站半：客户端 → 服务端，逐条读取 JSON 文本；`None` 表示对端已关闭。
#[async_trait]
pub trait TransportSource: Send {
    async fn recv(&mut self) -> Option<String>;
}

/// 全双工传输。拆成独立的入/出站半供 handler 并发持有。
pub trait Transport: Send {
    fn split(self) -> (Box<dyn TransportSink>, Box<dyn TransportSource>);
}

type WsSplitSink = futures::stream::SplitSink<WebSocket, WsMessage>;
type WsSplitStream = futures::stream::SplitStream<WebSocket>;

/// WebSocket 传输。
pub struct WsTransport {
    socket: WebSocket,
}

impl WsTransport {
    pub fn new(socket: WebSocket) -> Self {
        Self { socket }
    }
}

impl Transport for WsTransport {
    fn split(self) -> (Box<dyn TransportSink>, Box<dyn TransportSource>) {
        let (sender, receiver) = self.socket.split();
        (
            Box::new(WsSink { sender }),
            Box::new(WsSource { receiver }),
        )
    }
}

struct WsSink {
    sender: WsSplitSink,
}

#[async_trait]
impl TransportSink for WsSink {
    async fn send(&mut self, text: String) -> TransportResult<()> {
        self.sender.send(WsMessage::Text(text.into())).await?;
        Ok(())
    }

    async fn ping(&mut self) -> TransportResult<()> {
        self.sender
            .send(WsMessage::Ping(Default::default()))
            .await?;
        Ok(())
    }

    async fn close(&mut self) -> TransportResult<()> {
        self.sender.close().await?;
        Ok(())
    }
}

struct WsSource {
    receiver: WsSplitStream,
}

#[async_trait]
impl TransportSource for WsSource {
    async fn recv(&mut self) -> Option<String> {
        while let Some(msg) = self.receiver.next().await {
            match msg {
                Ok(WsMessage::Text(text)) => return Some(String::from(text.as_str())),
                // Close 帧与读错误在这里就地记录诊断日志后归一为 None，
                // handler 层统一按「对端关闭」处理（三种情况无法在
                // `Option<String>` 上区分，诊断粒度留在本层）。
                Ok(WsMessage::Close(_)) => {
                    tracing::debug!("ws transport: close frame received");
                    return None;
                }
                Ok(_) => continue,
                Err(e) => {
                    tracing::warn!(error = %e, "ws transport: socket read error");
                    return None;
                }
            }
        }
        None
    }
}

/// stdio 管道传输：stdin 收客户端消息、stdout 推服务端事件，一帧一行 JSON。
///
/// 父进程（Tauri 主进程）spawn 本进程后接管两端管道即可，无需监听任何 TCP 端口。
pub struct StdioTransport {
    stdin: Stdin,
    stdout: Stdout,
}

impl StdioTransport {
    pub fn new() -> Self {
        Self {
            stdin: tokio::io::stdin(),
            stdout: tokio::io::stdout(),
        }
    }
}

impl Default for StdioTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for StdioTransport {
    fn split(self) -> (Box<dyn TransportSink>, Box<dyn TransportSource>) {
        (
            Box::new(StdioSink {
                stdout: self.stdout,
            }),
            Box::new(StdioSource {
                lines: BufReader::new(self.stdin).lines(),
            }),
        )
    }
}

struct StdioSink {
    stdout: Stdout,
}

#[async_trait]
impl TransportSink for StdioSink {
    async fn send(&mut self, text: String) -> TransportResult<()> {
        self.stdout.write_all(text.as_bytes()).await?;
        self.stdout.write_all(b"\n").await?;
        self.stdout.flush().await?;
        Ok(())
    }

    async fn close(&mut self) -> TransportResult<()> {
        self.stdout.flush().await?;
        Ok(())
    }
}

struct StdioSource {
    lines: Lines<BufReader<Stdin>>,
}

#[async_trait]
impl TransportSource for StdioSource {
    async fn recv(&mut self) -> Option<String> {
        self.lines.next_line().await.ok().flatten()
    }
}

// ---------------------------------------------------------------------------
// Channel-backed halves — 用于 stdio 多路复用：每个虚拟连接拿一对
// mpsc 半，stdio_session 负责在共享 stdin/stdout 与各虚拟连接之间分发。
// ---------------------------------------------------------------------------

/// 入站半：从 stdio_session 的分发器接收本通道的 JSON 帧。
pub struct ChannelSource {
    rx: tokio::sync::mpsc::Receiver<String>,
}

impl ChannelSource {
    pub fn new(rx: tokio::sync::mpsc::Receiver<String>) -> Self {
        Self { rx }
    }
}

#[async_trait]
impl TransportSource for ChannelSource {
    async fn recv(&mut self) -> Option<String> {
        self.rx.recv().await
    }
}

/// 出站半：把本通道的 JSON 帧包上 channel 信封后交给汇聚 writer。
pub struct ChannelSink {
    channel: String,
    tx: tokio::sync::mpsc::Sender<String>,
}

impl ChannelSink {
    pub fn new(channel: impl Into<String>, tx: tokio::sync::mpsc::Sender<String>) -> Self {
        Self {
            channel: channel.into(),
            tx,
        }
    }
}

#[async_trait]
impl TransportSink for ChannelSink {
    async fn send(&mut self, text: String) -> TransportResult<()> {
        // payload 按字符串嵌入：handler 层的 text 已是 JSON 文本，
        // 双层 parse（信封 → payload）由对端完成，本地 IPC 开销可接受。
        let envelope = serde_json::json!({ "channel": self.channel, "payload": text }).to_string();
        self.tx
            .send(envelope)
            .await
            .map_err(|e| Box::new(e) as TransportError)
    }

    async fn close(&mut self) -> TransportResult<()> {
        Ok(())
    }
}
