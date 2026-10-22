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

/// hermix `StreamEvent` 的类型 → App 侧事件名。
///
/// 返回 `None` = 这类帧不推给手机（桌面端专属的内部事件），**不静默改名**。
///
/// **命名对齐 2026-10-21**：早期映射停留在旧 `ProtocolMessage` 命名
///（`user_message` / `assistant_message` / `tool_call`），而中继实际转发的
/// 是 `#[serde(tag="type", rename_all="snake_case")]` 的 `StreamEvent`
///（`text` / `thinking` / `tool_start` / `tool_result` …）。旧名与线上输出
/// 对不上，等于把所有 agent 事件都静默丢掉了——手机端此前收不到任何回复。
/// 这里改回与 `StreamEvent` 真实 `type` 一一对应。
pub fn to_mobile_event(msg_type: &str) -> Option<&'static str> {
    Some(match msg_type {
        "text" => "assistant.message",
        // 思考过程折叠块（与桌面端同一个 `<think>` 解析源）
        "thinking" => "assistant.thinking",
        "tool_start" => "tool.start",
        // 手机端语音识别配置（voice_stt.rs）：随 WS 会话建立下发
        "voice_stt_config" => "voice.stt_config",
        "tool_result" => "tool.end",
        "request_approval" => "approval.request",
        "done" => "turn.done",
        "error" => "turn.error",
        // 桌面推来的模型清单（手机连上即下发）——手机端唯一的模型来源
        "model_list" => "model.list",
        // 上下文窗口占用（持久条）/ 单轮 token 用量（cache 命中率）
        "context_usage" => "context.usage",
        "usage_update" => "usage.update",
        // 待办列表（对方 To-dos 卡片）
        "todo_snapshot" => "todo.snapshot",
        "todo_item_upserted" => "todo.item_upserted",
        "todo_item_removed" => "todo.item_removed",
        "todo_group_started" => "todo.group_started",
        // 兼容旧命名（当前 StreamEvent 已无此变体，永不触发，保留以防回归）
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

/// App → PC：请求补齐 `since_seq` 之后的事件。
///
/// **不走 `to_hermes_type`**：这不是给 agent 的消息，`handle_ws_stream` 只认
/// `WsClientMessage`，把它混进业务帧会被整帧丢弃且不报错。它在
/// `RelaySource::recv` 就被截下，回一个 [`MOBILE_SYNC_RESULT`]。
pub const MOBILE_SYNC_REQUEST: &str = "sync";

/// PC → App：补发结果。`{type, since_seq, truncated, events[]}`。
pub const MOBILE_SYNC_RESULT: &str = "sync.result";

/// 认出补发请求，返回手机已经看到的最后一个序号（缺省 0 = 全量）。
pub fn parse_sync_request(plain: &str) -> Option<u64> {
    let v: Value = serde_json::from_str(plain).ok()?;
    if v.get("type")?.as_str()? != MOBILE_SYNC_REQUEST {
        return None;
    }
    // `since_seq` 缺失 / 非数字一律当 0（全量）：宁可多发几条重复事件，
    // 也不要因为一个字段格式问题让手机永远看不到断连那一段。
    Some(v.get("since_seq").and_then(Value::as_u64).unwrap_or(0))
}

/// 把一条 hermix 帧转成 App 侧帧。
///
/// 不可映射的类型返回 `None`（调用方跳过），**不产出半成品帧**——给 App 一个
/// 它不认识的 `type`，UI 会静默忽略并留下"消息丢了"的假象。
///
/// **2026-10-21 关键修复**：中继实际收到的是嵌套 `WsEnvelope`
///（`{"event": {"type": ...}, "seq": ...}`），桌面端读 `event` 那一层，而手机端
/// 此前直接读顶层 `type` —— 顶层没有 `type`，于是**所有** agent 事件都被判成
/// "不面向 App" 而丢弃，手机端收不到任何回复。这里先摊平到 `event` 再映射；
/// 扁平帧（`voice_stt_config`）仍按原样处理。
pub fn to_mobile_frame(frame: &Value) -> Option<Value> {
    // 嵌套 WsEnvelope → 取 event 层；否则（扁平帧）用自身。
    let inner = frame.get("event").filter(|v| v.is_object()).unwrap_or(frame);
    let ty = inner.get("type")?.as_str()?;
    let event = to_mobile_event(ty)?;
    let mut out = json!({
        "type": event,
        "proto_version": crate::relay_proto::PROTO_VERSION,
    });
    // 原样带过已知字段；不猜未知字段。
    for key in [
        "session_id",
        "message_id",
        "request_id",
        "content",
        "tool",
        "arguments",
        // voice.stt_config 的配置体（voice_stt.rs），仅该类型帧携带
        "voice_stt",
        // 工具轨迹（tool.start / tool.end）
        "tool_call_id",
        "denial_kind",
        "is_error",
        // **错误原因**（StreamEvent::Error 的字段是 code/message/detail，
        // 不是 content）。不透传它们，手机端只能显示兜底文案"出错了"，
        // 真实原因（LLM 401 / 参数非法 / 5xx）被整个吞掉——用户看到的是
        // 一个没有信息量的横幅，排查时只能去翻电脑的日志。
        "code",
        "message",
        "detail",
        // 上下文窗口占用（context.usage）
        "used_tokens",
        "max_tokens",
        "threshold_tokens",
        "percent",
        // 单轮 token 用量（usage.update）
        "prompt_tokens",
        "completion_tokens",
        "cache_read_tokens",
        "cache_creation_tokens",
        "reasoning_tokens",
        // 桌面推来的模型清单（model.list）
        "models",
        "default_model",
        // 待办列表（todo.*）
        "items",
        "item",
        "id",
        "group_id",
        "group_title",
    ] {
        if let Some(v) = inner.get(key) {
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
            "text",
            "thinking",
            "tool_start",
            "tool_result",
            "request_approval",
            "done",
            "error",
            "context_usage",
            "usage_update",
            "todo_snapshot",
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
            "type": "tool_start",
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
    fn 嵌套_wsenvelope_被摊平再映射() {
        // 中继真实收到的是 WsEnvelope：{event:{type:...}, seq, ...}。
        // 此前直接读顶层 type（不存在）→ 全部丢弃，手机收不到任何回复。
        // 这里验证嵌套帧能被正确摊平并映射。
        let env = json!({
            "version": 1,
            "seq": 7,
            "event": {
                "type": "thinking",
                "content": "让我先想想",
            },
        });
        let m = to_mobile_frame(&env).expect("嵌套帧应可映射");
        assert_eq!(m["type"], "assistant.thinking");
        assert_eq!(m["content"], "让我先想想");

        // 扁平帧（voice_stt_config）仍按原样处理
        let flat = json!({"type": "voice_stt_config", "voice_stt": {"ok": true}});
        let m2 = to_mobile_frame(&flat).expect("扁平帧应可映射");
        assert_eq!(m2["type"], "voice.stt_config");
    }

    #[test]
    fn 错误帧必须带出原因_否则手机端只剩兜底文案() {
        // StreamEvent::Error 的字段是 code/message/detail——**没有 content**。
        // 映射只透传 content 时，手机端拿到的错误帧是空的，只能显示兜底文案，
        // 真实原因（LLM 401 / 参数非法 / 5xx）被整个吞掉。
        let env = json!({
            "event": {
                "type": "error",
                "code": "llm_error",
                "message": "401 invalid api_key",
            },
        });
        let m = to_mobile_frame(&env).expect("应可映射");
        assert_eq!(m["type"], "turn.error");
        assert_eq!(m["code"], "llm_error");
        assert_eq!(m["message"], "401 invalid api_key");
    }

    #[test]
    fn 模型清单映射成手机事件() {
        let env = json!({
            "event": {
                "type": "model_list",
                "models": [{"provider_id": "deepseek", "model_id": "chat"}],
                "default_model": "deepseek:chat",
            },
        });
        let m = to_mobile_frame(&env).expect("应可映射");
        assert_eq!(m["type"], "model.list");
        assert_eq!(m["models"][0]["provider_id"], "deepseek");
        assert_eq!(m["models"][0]["model_id"], "chat");
        assert_eq!(m["default_model"], "deepseek:chat");
    }

    #[test]
    fn context_usage_与_todo_字段被带过() {
        let env = json!({
            "event": {
                "type": "context_usage",
                "used_tokens": 48000,
                "max_tokens": 200000,
                "threshold_tokens": 160000,
                "percent": 24.0,
            },
        });
        let m = to_mobile_frame(&env).expect("应可映射");
        assert_eq!(m["type"], "context.usage");
        assert_eq!(m["used_tokens"], 48000);
        assert_eq!(m["max_tokens"], 200000);
        assert_eq!(m["percent"], 24.0);

        let snap = json!({
            "event": {
                "type": "todo_snapshot",
                "items": [{"id": "t1", "title": "写代码", "status": "in_progress"}],
            },
        });
        let m2 = to_mobile_frame(&snap).expect("应可映射");
        assert_eq!(m2["type"], "todo.snapshot");
        assert_eq!(m2["items"][0]["title"], "写代码");
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
