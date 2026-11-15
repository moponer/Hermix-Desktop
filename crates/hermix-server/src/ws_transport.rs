//! over-WebSocket 的 `Transport` 实现。
//!
//! **这个面为什么复活**：`/v1/ws`、`/v1/ws-stream` 的 axum upgrade handler 在
//! `181a51aa`（2026-09-26 端点审计）里被删掉了，理由是「桌面端全量迁 stdio
//! bridge，前端零裸 `new WebSocket`，本项目没有 Web bundle / 移动端 crate」。
//! 桌面端确实迁完了，但「没有移动端」这个前提现在不成立了——手机端跨网络，
//! stdio 管道够不着。
//!
//! **不是回退**：`crate::ws::handle_ws_stream`（`ws.rs:2563`）本体一直在，
//! `stdio_session.rs:254` 还在用它喂管道。本文件只补上「把一个 WebSocket
//! 包装成 `Transport`」这一层，**流式协议零改动**。看到这里若以为端点复活是
//! 回退，请先读这段。
//!
//! **保活不归这里管**：`handle_ws_stream` 自己按 `WS_HEARTBEAT_INTERVAL_SECS`
//! 发 WS 协议级 Ping（见 `ws.rs:210-214` 的说明：刷 NAT / 中间盒空闲计时器，
//! 约 2 字节，不进业务消息流）。所以这里**不覆写** `TransportSink::ping`——
//! 覆写了就是两处各发一轮，双倍心跳。

use async_trait::async_trait;
use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};

use crate::transport::{Transport, TransportResult, TransportSink, TransportSource};

/// 把一条已升级的 WebSocket 变成 `Transport`。
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
        // 拆两半而不是共享一把 Mutex：handler 需要**同时**推流与读取
        // （审批响应等 out-of-band 消息必须在推流途中被读到，否则堆积在
        // 接收缓冲区直到超时）。共用一个 Mutex 会在 send 的 await 期间
        // 锁住整个 socket，recv 拿不到锁——恰好毁掉拆分的目的。
        let (sink, stream) = self.socket.split();
        (Box::new(WsSink { sink }), Box::new(WsSource { stream }))
    }
}

struct WsSink {
    sink: futures::stream::SplitSink<WebSocket, Message>,
}

#[async_trait]
impl TransportSink for WsSink {
    async fn send(&mut self, text: String) -> TransportResult<()> {
        self.sink.send(Message::Text(text.into())).await?;
        Ok(())
    }

    async fn close(&mut self) -> TransportResult<()> {
        // 关的是出站方向；对端可能还在发（例如正在补发审批决定），
        // 所以发 Close 帧后不强行终止整条连接。
        self.sink.send(Message::Close(None)).await?;
        Ok(())
    }
}

struct WsSource {
    stream: futures::stream::SplitStream<WebSocket>,
}

#[async_trait]
impl TransportSource for WsSource {
    async fn recv(&mut self) -> Option<String> {
        loop {
            match self.stream.next().await {
                // axum 0.8 的 `Message::Text` 装的是 `Utf8Bytes` 不是 `String`。
                Some(Ok(Message::Text(t))) => return Some(t.as_str().to_string()),
                // 客户端把一条业务帧当二进制发也接受：内容仍是 JSON 文本，
                // 丢在这里等于让用户看到「连上了但没反应」，很难自查。
                Some(Ok(Message::Binary(b))) => {
                    return Some(String::from_utf8_lossy(&b).to_string())
                }
                // 控制帧不产生业务消息。Pong / Ping 由 tungstenite 层自动
                // 处理，不进业务流——这也是本文件不覆写 `ping` 的原因。
                Some(Ok(_)) => continue,
                // 对端关闭或链路出错：都归一成 None，handler 侧统一走断连
                // 收尾（`ws.rs` 的 `ws_disconnected` 路径），不区分错误类型，
                // 避免把「正常关闭」当成故障上报。
                Some(Err(_)) | None => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Bytes;

    #[test]
    fn 二进制帧按lossy解出文本() {
        // 客户端误用二进制帧时，内容仍是 JSON 文本——必须能读出来，
        // 否则表现为「连上了但没反应」。
        let b = Bytes::from_static(br#"{"type":"user_message","content":"hi"}"#);
        let s = String::from_utf8_lossy(&b).to_string();
        assert!(s.contains("user_message"));
    }

    #[test]
    fn 非utf8二进制不panic() {
        // 坏字节走 lossy，不能因为一帧脏数据把整条连接炸掉。
        let b = Bytes::from_static(&[0xff, 0xfe, 0x00]);
        let s = String::from_utf8_lossy(&b).to_string();
        assert!(!s.is_empty());
    }
}
