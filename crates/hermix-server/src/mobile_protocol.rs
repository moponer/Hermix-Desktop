//! App 侧事件名 ↔ hermix envelope 的映射，**单向收口在此文件**。
//!
//! 为什么要收口：Clawket 式的事件名是给手机端 UI 用的（`tool.start` 这类
//! 点分小写），hermix 的 `ProtocolMessage` 是给传输层用的。两者一旦在多处
//! 各写一份映射，改一边忘另一边就是「某个工具进度条永远不动」这类症状——
//! 极难定位，因为帧是通的、只是名字没对上。
//!
//! **审批三态在 App 侧只暴露两态**（2026-10-04 拍板）：`Origin::PairedDevice`
//! 对 `Capability::ApproveAlways` 是**无条件 Denied**，且
//! `effective_decision_for_paired` 对矩阵 Denied 不做挽救（`security.rs:429-440`），
//! 设备 scopes 里写 `approvealways` 也不生效。所以「永久允许」**不能**经手机端
//! 授予——`to_mobile_approval` 里刻意不提供该态，App 端的第三个按钮也不该存在。
//! 这与 `2026-09-30` 笔记否决的「语音转写授予持久权限」同源：远程端不授予
//! 持久权限。

use serde_json::{json, Value};

/// App 侧看到的审批选项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MobileApproval {
    Deny,
    Once,
    /// **刻意不提供**：`session`（永久）对应 `ApproveAlways`，配对设备拿不到。
    /// 保留在枚举里是为了让这个缺口显式可见，而不是靠"没实现"蒙混。
    #[allow(dead_code)]
    SessionUnavailable,
}

impl MobileApproval {
    pub fn as_wire(&self) -> Option<&'static str> {
        match self {
            Self::Deny => Some("deny"),
            Self::Once => Some("once"),
            Self::SessionUnavailable => None,
        }
    }

    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "deny" => Some(Self::Deny),
            "once" => Some(Self::Once),
            // 收到 `session` 也**不**映射成可用态：宁可让 App 收到一个
            // 明确的「不支持」也不要静默降级成 once（那会让用户以为永久放行
            // 成功了，下次审批照样弹）。
            _ => None,
        }
    }
}

/// hermix `ProtocolMessage` 的类型 → App 侧事件名。
///
/// 返回 `None` = 这类帧不推给手机（桌面端专属的内部事件），**不静默改名**。
pub fn to_mobile_event(msg_type: &str) -> Option<&'static str> {
    Some(match msg_type {
        "user_message" => "user.message",
        "assistant_message" | "text" => "assistant.message",
        "tool_call" => "tool.start",
        "tool_result" => "tool.end",
        "request_approval" => "approval.request",
        "done" => "turn.done",
        "error" => "turn.error",
        "task_in_flight" => "turn.busy",
        _ => return None,
    })
}

/// App 侧事件名 → hermix `ProtocolMessage` 类型。
pub fn to_hermes_type(event: &str) -> Option<&'static str> {
    Some(match event {
        "user.message" => "user_message",
        "approval.respond" => "approval_response",
        "ping" => "ping",
        _ => return None,
    })
}

/// 把一条 hermix 帧转成 App 侧帧。
///
/// 不可映射的类型返回 `None`（调用方跳过），**不产出半成品帧**——给 App 一个
/// 它不认识的 `type`，UI 会静默忽略并留下"消息丢了"的假象。
pub fn to_mobile_frame(frame: &Value) -> Option<Value> {
    let ty = frame.get("type")?.as_str()?;
    let event = to_mobile_event(ty)?;
    let mut out = json!({
        "type": event,
        "proto_version": crate::relay_proto::PROTO_VERSION,
    });
    // 原样带过已知字段；不猜未知字段。
    for key in ["session_id", "message_id", "request_id", "content", "tool", "arguments"] {
        if let Some(v) = frame.get(key) {
            out[key] = v.clone();
        }
    }
    Some(out)
}

/// 审批请求：补上「永久允许」在本端**不可用**的显式标记。
///
/// 刻意让 App 收到 `can_persist: false` 而不是沉默地少一个按钮——手机屏幕小、
/// 按钮少一个用户未必察觉，但下次审批照样弹会让人以为是自己点错了。
pub fn approval_request_to_mobile(frame: &Value) -> Option<Value> {
    let mut out = to_mobile_frame(frame)?;
    set_can_persist_false(&mut out);
    Some(out)
}

/// 就地打上「永久放行不可用」标记。映射与打标分开，是为了让
/// `relay_transport` 在**已映射**的帧上补这个标志时不必再走一遍映射。
pub fn set_can_persist_false(frame: &mut Value) {
    frame["can_persist"] = json!(false);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 事件名双向对齐() {
        for hermes in [
            "user_message",
            "assistant_message",
            "tool_call",
            "tool_result",
            "request_approval",
            "done",
            "error",
        ] {
            let ev = to_mobile_event(hermes).unwrap_or_else(|| panic!("{hermes} 未映射"));
            // 上行只覆盖客户端会发的三类；其余是纯下行，这里只验标签形状
            assert!(ev.contains('.') || ev == "ping", "{ev} 不像事件名");
        }
    }

    #[test]
    fn 未知类型不产出半成品帧() {
        // 最容易出事的一条：给 App 一个它不认识的 type，UI 静默忽略，
        // 现场表现是「消息丢了」而不是「类型不认识」。
        assert!(to_mobile_frame(&json!({"type": "internal_zombie"})).is_none());
        assert!(to_mobile_frame(&json!({})).is_none());
        assert!(to_mobile_frame(&json!({"type": 42})).is_none());
    }

    #[test]
    fn 帧转换带过已知字段() {
        let f = json!({
            "type": "tool_call",
            "session_id": "s1",
            "tool": "read_file",
            "arguments": {"path": "a.rs"},
            "secret_internal": "不该出现",
        });
        let m = to_mobile_frame(&f).expect("应可映射");
        assert_eq!(m["type"], "tool.start");
        assert_eq!(m["session_id"], "s1");
        assert_eq!(m["tool"], "read_file");
        assert_eq!(m["arguments"]["path"], "a.rs");
        assert!(
            m.get("secret_internal").is_none(),
            "未知内部字段不得外泄给手机端"
        );
    }

    #[test]
    fn 审批在手机端拿不到永久放行() {
        assert_eq!(MobileApproval::from_wire("once"), Some(MobileApproval::Once));
        assert_eq!(MobileApproval::from_wire("deny"), Some(MobileApproval::Deny));
        // 收到 session 也映射不成可用态：静默降级成 once 会让用户以为
        // 永久放行成功了。
        assert_eq!(MobileApproval::from_wire("session"), None);
        assert_eq!(MobileApproval::SessionUnavailable.as_wire(), None);

        let f = json!({"type": "request_approval", "request_id": "r1", "tool": "terminal"});
        let m = approval_request_to_mobile(&f).expect("可映射");
        assert_eq!(m["type"], "approval.request");
        assert_eq!(m["request_id"], "r1");
        assert_eq!(
            m["can_persist"], json!(false),
            "必须显式告诉 App 永久放行不可用，否则用户以为按钮没生效"
        );
    }

    #[test]
    fn 上行只认三类_其余不当命令() {
        assert_eq!(to_hermes_type("user.message"), Some("user_message"));
        assert_eq!(to_hermes_type("approval.respond"), Some("approval_response"));
        assert_eq!(to_hermes_type("ping"), Some("ping"));
        // 手机端不能凭空造出别的帧类型
        assert_eq!(to_hermes_type("turn.done"), None);
        assert_eq!(to_hermes_type("assistant.message"), None);
    }
}
