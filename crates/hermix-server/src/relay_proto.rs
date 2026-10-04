//! relay 线路格式的**唯一真相源**。
//!
//! relay（`apps/relay/`，Node）**不解析内层帧**：它只认下面这五类外层记录，
//! `payload` 整体透传。所以本文件刻意不定义任何业务消息类型——内层是
//! `ProtocolMessage`（hermix-transport 的 envelope），由
//! [`crate::mobile_protocol`] 负责与 App 侧事件名对齐。
//!
//! **relay 侧不得再写一份**。Node 端的 `src/proto.js` 只能持有这五个 tag 与
//! 版本号；帧格式的真值在这里。版本号是双端漂移的运行时兜底：App 与 relay
//! 握手时互相比对 `proto_version`，不一致直接断开而不是带着错位的帧跑。
//!
//! **relay 不得改写的东西**（每一条都会让手机端的请求签名失效，见
//! `request_signature.rs:6-10`——签名覆盖 `METHOD\npath`）：
//! HTTP method、path、`Authorization`、`X-Hermix-Signature`。
//! 所以 relay 是**字节级透明管道 + 按 token 哈希路由**，不是重组请求的中转。

use serde::{Deserialize, Serialize};

/// 线路格式版本。**改动任何 tag 或字段都 +1**，并同步 relay 侧 `proto.js`。
pub const PROTO_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrameTag {
    /// 客户端 → relay：建立连接并出示 token。relay 据此把该连接绑到某台电脑。
    Hello,
    /// 客户端 ↔ relay：转发一帧。`payload` 是 hermix 的 envelope，relay 不看内容。
    Route,
    /// relay → 客户端：已登记 / 已撤销。
    Ack,
    /// 任一方 → relay：关闭。
    Close,
}

/// 外层信封。字段刻意少——多一个字段就多一处两端可能漂移的地方。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub proto_version: u32,
    pub tag: FrameTag,
    /// 设备 token 的**哈希**（不是明文 token）。relay 用它做路由键。
    /// 解析权在 hermix（`DeviceRegistry::authenticate`），relay 不验真伪。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_hash: Option<String>,
    /// 目标电脑的稳定标识（relay 侧按它选路）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// 内层帧原文，relay 整体透传。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
}

impl Envelope {
    pub fn new(tag: FrameTag) -> Self {
        Self {
            proto_version: PROTO_VERSION,
            tag,
            token_hash: None,
            node_id: None,
            payload: None,
        }
    }

    pub fn route(payload: String) -> Self {
        Self {
            payload: Some(payload),
            ..Self::new(FrameTag::Route)
        }
    }

    /// 握手期版本比对。**不做降级**：版本不同直接失败。
    ///
    /// 降级比失败糟得多——错位的帧会一路透传到 hermix，表现为「能连上但消息
    /// 莫名其妙消失」，那是最难定位的一类故障。
    pub fn version_ok(&self) -> bool {
        self.proto_version == PROTO_VERSION
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 往返序列化保持字段() {
        let e = Envelope {
            token_hash: Some("abc123".into()),
            node_id: Some("node-1".into()),
            payload: Some(r#"{"type":"user_message"}"#.into()),
            ..Envelope::route(String::new())
        };
        let s = serde_json::to_string(&e).expect("ser");
        let back: Envelope = serde_json::from_str(&s).expect("de");
        assert_eq!(back.tag, FrameTag::Route);
        assert_eq!(back.token_hash.as_deref(), Some("abc123"));
        assert_eq!(back.node_id.as_deref(), Some("node-1"));
        assert_eq!(back.payload.as_deref(), Some(r#"{"type":"user_message"}"#));
    }

    #[test]
    fn 版本不一致直接判否() {
        let mut e = Envelope::new(FrameTag::Hello);
        assert!(e.version_ok());
        e.proto_version = PROTO_VERSION + 1;
        assert!(!e.version_ok());
    }

    #[test]
    fn payload_原样透传_不做任何改写() {
        // relay 的核心性质：内层是**不透明字节**。这里塞一段带转义、
        // 带中文、带换行的文本，往返后必须逐字节相同。
        let raw = "{\"a\":\"换行\\n与引号 \\\" 和 emoji 🛠️\"}";
        let e = Envelope::route(raw.to_string());
        let s = serde_json::to_string(&e).expect("ser");
        let back: Envelope = serde_json::from_str(&s).expect("de");
        assert_eq!(back.payload.as_deref(), Some(raw), "内层不得被规范化或改写");
    }

    #[test]
    fn 可选字段缺省时不出现() {
        // 少一个字段就少一处两端可能不一致的地方。
        let s = serde_json::to_string(&Envelope::new(FrameTag::Ack)).expect("ser");
        assert!(!s.contains("token_hash"));
        assert!(!s.contains("node_id"));
        assert!(!s.contains("payload"));
    }

    #[test]
    fn 未知tag_解不开而不是静默接受() {
        // 宁可握手失败也不要带着错位的 tag 跑。
        let bad = r#"{"proto_version":1,"tag":"teleport"}"#;
        assert!(serde_json::from_str::<Envelope>(bad).is_err());
    }
}
