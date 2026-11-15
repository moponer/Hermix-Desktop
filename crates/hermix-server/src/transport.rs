//! 传输层实现：stdio 管道与多路复用 channel 两种具体传输。
//!
//! 契约 trait（`Transport` / `TransportSink` / `TransportSource`）已下沉到
//! `hermix-transport`（P0「传输两套合一」：类型与契约单点定义），这里
//! re-export 保持既有调用点 `crate::transport::…` 零改动；本模块只保留
//! 具体实现。裸 WebSocket 传输（`WsTransport`）随 `/v1/ws` 路由删除已
//! 一并移除（2026-09-26 端点审计批次）。

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines, Stdin, Stdout};

pub use hermix_transport::{Transport, TransportError, TransportResult, TransportSink, TransportSource};

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
