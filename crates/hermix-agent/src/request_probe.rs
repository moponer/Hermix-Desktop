//! 请求体分段探针 —— 把「哪一段字节在变」变成日志里的一行。
//!
//! # 为什么需要这个
//!
//! provider 的前缀缓存是**块级**的（2026-10-21 对
//! `api.kilo.ai` 直接发对照请求实测）：只有**变了的字节**重新计费。
//! 所以「命中率低」的根因永远是「某一段字节在变」—— 而这件事此前只能靠
//! 推断，因为日志格式只打印消息、不打印 span 字段。
//!
//! # 它报什么
//!
//! 每次 API 调用前，算出三段的**真实字符数**与**指纹**，并与**同会话上一次**
//! 调用比较，列出变了哪几段：
//!
//! ```text
//! prefix_probe: #12 session=a04cab07 system=68796chars tools=31381chars
//!               messages=7352chars | 变了: messages
//! ```
//!
//! `system` = 拼装好的系统提示；`tools` = 工具 schema 数组；`messages` = 对话历史。
//! 「变了」那一列就是**缓存失效的起点** —— 从该段往后，之前建立的缓存全部失效。
//!
//! # 已知局限
//!
//! 指纹是**进程内**的 per-session 记录：进程重启后第一次调用必然显示「首次」。
//! 多会话并发时每个会话各记各的，互不干扰。

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;

use hermix_core::{Message, MessageRole, ToolSchema};

/// 每个会话最近一次的指纹 + 序号。
type SessionState = (SegmentFingerprint, u64);

/// 最多记多少个会话的指纹（防无界增长）。
const MAX_TRACKED_SESSIONS: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq)]
struct SegmentFingerprint {
    system: u64,
    tools: u64,
    messages: u64,
}

static TRACKED: Mutex<Option<HashMap<String, SessionState>>> = Mutex::new(None);

fn hash_str(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn hash_tools(tools: &[ToolSchema]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for t in tools {
        t.name.hash(&mut h);
        // schema 的 JSON 形状也参与指纹：描述改一个字就会让缓存失效。
        serde_json::to_string(t)
            .ok()
            .unwrap_or_default()
            .hash(&mut h);
    }
    h.finish()
}

/// 记录一次请求并把结果打进日志。`session_id` 为 `None` 时用占位键（按进程计）。
pub fn observe(session_id: Option<&str>, messages: &[Message], tools: &[ToolSchema]) {
    // 系统提示是拼装进 messages 的**第一条** system 消息；其余是对话历史。
    let (system_text, rest) = match messages.split_first() {
        Some((first, rest)) if first.role == MessageRole::System => {
            (first.content.clone().unwrap_or_default(), rest)
        }
        _ => (String::new(), messages),
    };

    let system_chars = system_text.len();
    // 分段统计对话历史：**哪一段在变大**就是 fresh token 的来源。
    let mut content_chars = 0usize;
    let mut tool_call_chars = 0usize;
    let mut tool_result_chars = 0usize;
    for m in rest {
        let c = m.content.as_deref().map(str::len).unwrap_or(0);
        let tc = m
            .tool_calls
            .as_ref()
            .map(|calls| {
                calls
                    .iter()
                    .map(|t| t.function.name.len() + t.function.arguments.len())
                    .sum::<usize>()
            })
            .unwrap_or(0);
        if m.role == MessageRole::Tool {
            tool_result_chars += c;
        } else {
            content_chars += c;
            tool_call_chars += tc;
        }
    }
    let messages_chars: usize = content_chars + tool_call_chars + tool_result_chars;
    let tools_chars: usize = tools
        .iter()
        .map(|t| {
            serde_json::to_string(t)
                .map(|s| s.len())
                .unwrap_or_else(|_| t.name.len())
        })
        .sum();

    let fp = SegmentFingerprint {
        system: hash_str(&system_text),
        tools: hash_tools(tools),
        messages: hash_str(&hashable_messages(rest)),
    };

    let key = session_id.unwrap_or("<no-session>").to_string();
    let mut guard = TRACKED.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    if map.len() > MAX_TRACKED_SESSIONS {
        map.clear();
    }
    let (changed, seq) = match map.get(&key) {
        Some((prev, n)) => {
            let mut changed: Vec<String> = Vec::new();
            if prev.system != fp.system {
                changed.push("system".to_string());
            }
            if prev.tools != fp.tools {
                changed.push("tools".to_string());
            }
            if prev.messages != fp.messages {
                changed.push("messages".to_string());
            }
            (changed, n + 1)
        }
        None => (vec!["首次".to_string()], 1),
    };
    map.insert(key, (fp, seq));

    tracing::info!(
        "prefix_probe: #{} system={}chars tools={}chars messages={}chars (内容{} + 工具调用{} + 工具结果{}) | 变了: {}",
        seq,
        system_chars,
        tools_chars,
        messages_chars,
        content_chars,
        tool_call_chars,
        tool_result_chars,
        if changed.is_empty() {
            "无".to_string()
        } else {
            changed.join(",")
        }
    );
}

/// 对话历史的可哈希形式：只取会影响请求字节的字段。
fn hashable_messages(messages: &[Message]) -> String {
    let mut out = String::new();
    for m in messages {
        out.push_str(match m.role {
            MessageRole::System => "S",
            MessageRole::User => "U",
            MessageRole::Assistant => "A",
            MessageRole::Tool => "T",
            _ => "?",
        });
        if let Some(c) = &m.content {
            out.push_str(c);
        }
        // `reasoning_content` 会进请求体（serde 字段），必须参与指纹 ——
        // 它被就地剥离正是「messages 段变了」的典型来源。
        if let Some(r) = &m.reasoning_content {
            out.push_str("|R:");
            out.push_str(r);
        }
        if let Some(calls) = &m.tool_calls {
            for c in calls {
                out.push_str("|C:");
                out.push_str(&c.function.name);
                out.push_str(&c.function.arguments);
            }
        }
        out.push('\n');
    }
    out
}
