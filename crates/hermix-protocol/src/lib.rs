//! L3 协议层：信封 + 事件分类 + 路由 + seq 锚（设计 §3.5，P4）。
//!
//! 对齐 `pi-protocol` 的结构（CBOR → JSON，见设计选型偏离）：
//! 每条下行帧 = `Envelope { kind, route, seq, payload }`。
//!
//! **seq 是对账锚**：
//! - 严格递增，允许间隙（客户端只做大小比较）
//! - 客户端重连 = 用最后收到的 seq 重新 subscribe（幂等）
//! - 服务端发现 `since_seq` 太旧（已被压缩）→ 回退全量快照
//!
//! 本 crate 只定义**结构 + 原语**，不持有全局状态；seq 的生成
//! 与广播路由由 server 侧装配（P5 attachment 模型落地后接管）。
//!
//! 旧补发机制（`SESSION_BROADCASTS` / `completed_keys` /
//! `pending_grace`，见 ws.rs / ws_grace.rs）在 P5 多端挂载模型
//! 就绪前继续在线——P4 只铺信封原语，不下线活代码。

use serde::{Deserialize, Serialize};

/// 事件分类。payload 语义由 kind 决定（与现有 StreamEvent 正交——
/// 现有帧在 P5 迁移前继续用 StreamEvent，新协议路径用 kind）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// 会话条目提交（user / assistant message 持久化）。
    EntryCommitted,
    /// 流式 token（纯展示，不落盘）。
    StreamDelta,
    /// 工具调用开始（Effect Sandwich intent 已落库）。
    ToolIntent,
    /// 工具调用结果（Effect Sandwich outcome 已落库）。
    ToolOutcome,
    /// 任务状态迁移（TaskRecord）。
    TaskTransition,
    /// 审批请求 / 结果。
    Approval,
    /// 会话压缩状态。
    CompactionStatus,
    /// 错误事件。
    Error,
}

/// 路由：服务端 → 会话 → 附件（多端挂载核心，P5 前 session 级 attachment 恒为 0）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    pub server_id: String,
    pub session_id: Option<String>,
    /// 多端挂载：同一 session 多个 attachment（桌面 / 手机 / WebUI）。
    pub attachment_id: u64,
}

impl Route {
    /// 会话级路由（P5 前 attachment 恒 0）。
    pub fn session(server_id: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            server_id: server_id.into(),
            session_id: Some(session_id.into()),
            attachment_id: 0,
        }
    }
}

/// 下行帧信封。`seq` 严格递增（允许间隙），客户端只做大小比较。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub kind: EventKind,
    pub route: Route,
    pub seq: u64,
    /// 语义由 kind 决定；客户端按键解析。
    pub payload: serde_json::Value,
}

impl Envelope {
    pub fn new(
        kind: EventKind,
        route: Route,
        seq: u64,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            kind,
            route,
            seq,
            payload,
        }
    }
}

/// seq 锚：客户端重连时携带最后收到的 seq，服务端据此补发
/// `seq > since_seq` 的条目（快照 + 增量）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SeqAnchor {
    /// 客户端已收到的最大 seq；0 = 全量快照。
    pub since_seq: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_roundtrip() {
        let env = Envelope::new(
            EventKind::EntryCommitted,
            Route::session("srv1", "sess1"),
            42,
            serde_json::json!({"entry_id": "e1"}),
        );
        let json = serde_json::to_string(&env).expect("serialize");
        let back: Envelope = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(env, back);
        assert_eq!(back.seq, 42);
        assert!(matches!(back.kind, EventKind::EntryCommitted));
    }

    #[test]
    fn route_session_defaults_attachment_zero() {
        let r = Route::session("s", "a");
        assert_eq!(r.attachment_id, 0);
        assert_eq!(r.server_id, "s");
    }

    #[test]
    fn seq_anchor_default_is_full_snapshot() {
        assert_eq!(SeqAnchor::default().since_seq, 0);
    }
}
