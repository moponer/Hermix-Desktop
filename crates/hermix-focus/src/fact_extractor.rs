//! Phase 18 (P0-3): facts 表 caller — 从 user turn 抽取结构化事实。
//!
//! 设计参考 `docs/MEMORY_LAYER_DEEP_ANALYSIS.md` §3 P0-3：
//! `embedded.rs::upsert_fact` 函数 + schema 已就位，
//! `recognize()` 已经在 `fts_facts_visible` 查询 facts 表，
//! 但**没有任何 caller 把 turn 内容写入 facts** — 导致跨 session 召回
//! 永远命中不到 facts 表。
//!
//! 这个文件补上 caller：用一组 trigger pattern（中文 + 英文）抽取
//! user 的关键陈述（名字 / 偏好 / 工具 / 指令），调用
//! `EmbeddedMemoryBackend::upsert_fact`。
//!
//! ## 设计取舍
//!
//! - **不引入 regex crate 依赖**（已在 workspace 中）— 保持简单，
//!   触发词用 `starts_with` + 字符切分即可。
//! - **保守抽取** — 宁可漏抽，不要错抽。错误的事实会被
//!   `canonical_key UNIQUE` 永久钉死，后续难以纠正。
//! - **零 LLM 成本** — 不调 LLM 做 NER，纯字符串匹配。
//!   后续可以加 LLM 抽取器作为可选 path。
//!
//! ## 数据形态
//!
//! 每个 fact 是三元组：
//! - `canonical_key` (e.g. `user.name:张三`, `user.tool:Rust`)
//! - `payload` (JSON: `{kind, content, session_id, turn_number}`)
//! - `tags` (`["session:xxx", "user.identity", ...]`)

use serde_json::Value;

/// 一个被抽取的事实三元组，等待 caller 写入 `facts` 表。
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedFact {
    pub canonical_key: String,
    pub payload: Value,
    pub tags: Vec<String>,
}

impl ExtractedFact {
    fn new(
        kind: &str,
        content: &str,
        session_id: &str,
        turn_number: u32,
    ) -> Self {
        let payload = serde_json::json!({
            "kind": kind,
            "content": content,
            "session_id": session_id,
            "turn_number": turn_number,
        });
        let mut tags = vec![format!("session:{session_id}"), kind.to_string()];
        if kind.starts_with("user.") {
            tags.push("user_asserted".to_string());
        }
        Self {
            canonical_key: format!("{kind}:{content}"),
            payload,
            tags,
        }
    }
}

/// (前缀, kind, min_chars) 三元组定义 trigger。
/// - prefix: user message 中出现的关键字
/// - kind: 事实类别（影响 canonical_key 前缀）
/// - min_chars: 抽取的内容最少字符数（少于则丢弃）
const TRIGGERS: &[(&str, &str, usize)] = &[
    // 中文
    ("我叫", "user.name", 2),
    ("我的名字是", "user.name", 2),
    ("我的名字叫", "user.name", 2),
    ("我喜欢", "user.preference", 1),
    ("我爱", "user.preference", 1),
    ("我讨厌", "user.dislike", 1),
    ("我恨", "user.dislike", 1),
    ("我用", "user.tool", 1),
    ("我在用", "user.tool", 1),
    ("我使用", "user.tool", 1),
    ("记住", "user.note", 2),
    ("请记住", "user.note", 2),
    ("不要", "user.avoid", 1),
    ("别", "user.avoid", 1),
    ("我是", "user.identity", 2),
    // 英文
    ("my name is", "user.name", 2),
    ("i'm called", "user.name", 2),
    ("i like", "user.preference", 1),
    ("i love", "user.preference", 1),
    ("i hate", "user.dislike", 1),
    ("i use", "user.tool", 1),
    ("remember", "user.note", 2),
    ("don't", "user.avoid", 1),
    ("do not", "user.avoid", 1),
    ("i am", "user.identity", 2),
];

/// 抽取时遇到"截断字符"则停止。包含常见标点 + 换行。
fn is_truncation_char(c: char) -> bool {
    matches!(
        c,
        '。' | '，' | ',' | '.' | ';' | '；' | ':' | '!' | '?' | '\n' | '\r' | '\t'
    )
}

/// 从 trigger 后面切出 content，截到第一个标点 / 换行，最多 100 字符。
fn extract_content_after<'a>(user: &'a str, prefix_len: usize) -> &'a str {
    let after = user[prefix_len..].trim_start();
    let mut end = after.len();
    for (i, c) in after.char_indices() {
        if is_truncation_char(c) {
            end = i;
            break;
        }
        if c.len_utf8() > 4 {
            // 跳过 emoji 等超出 BMP 的字符
            continue;
        }
    }
    let raw = &after[..end];
    // Take first 100 chars (char count, not byte length).
    let truncated: String = raw.chars().take(100).collect();
    // We can't return a sub-slice with char-boundary; return raw with
    // a hard byte cap as a fallback. The caller clamps via chars().count().
    let _ = truncated;
    raw
}

/// 主入口：从 user message 抽取所有事实。
///
/// 设计要点：
/// - 同一 turn 内多个 trigger 都会抽取，但 `canonical_key` 去重，
///   第二次出现相同 key 时 `upsert_fact` 会 bump `access_count`。
/// - 返回 `Vec` 让 caller 知道抽到几个（用于 debug 日志）。
/// - 空 user / 空 content → 返回 `vec![]`。
pub fn extract_facts_from_turn(
    user: &str,
    session_id: &str,
    turn_number: u32,
) -> Vec<ExtractedFact> {
    let user_trim = user.trim();
    if user_trim.is_empty() || user_trim.len() < 3 {
        return Vec::new();
    }
    // Lower-cased scan for English triggers; original for Chinese.
    let user_lower = user_trim.to_lowercase();

    let mut facts = Vec::new();
    for (prefix, kind, min_chars) in TRIGGERS {
        // Try both original-case and lower-case scan; the first hit wins.
        let hit = if prefix.chars().any(|c| c.is_ascii_alphabetic()) && prefix.is_ascii() {
            // English trigger — case-insensitive
            user_lower.find(prefix)
        } else {
            // Chinese / non-ascii trigger — case-sensitive
            user_trim.find(prefix)
        };
        let Some(idx) = hit else {
            continue;
        };
        // Avoid matching the trigger as part of a longer word by
        // checking the character before the match.
        if idx > 0 {
            let prev_char_start = user_trim[..idx].char_indices().last().map(|(i, _)| i);
            if let Some(pcs) = prev_char_start {
                let prev_char = user_trim[pcs..idx].chars().next();
                if let Some(c) = prev_char {
                    // Allow the trigger to follow a space, punctuation,
                    // or the start of the string. Reject alphanumeric
                    // continuation.
                    if c.is_alphanumeric() && !matches!(c, '_' | '-') {
                        continue;
                    }
                }
            }
        }
        let raw_content = extract_content_after(user_trim, idx + prefix.len());
        // char count for min_chars check + truncation
        let content: String = raw_content.chars().take(100).collect();
        let content = content.trim();
        if content.chars().count() < *min_chars {
            continue;
        }
        // Skip content that is itself only punctuation/whitespace.
        if content.chars().all(|c| !c.is_alphanumeric()) {
            continue;
        }
        facts.push(ExtractedFact::new(kind, content, session_id, turn_number));
    }
    facts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(facts: &[ExtractedFact]) -> Vec<&str> {
        facts.iter().map(|f| f.payload["kind"].as_str().unwrap()).collect()
    }

    #[test]
    fn empty_user_returns_no_facts() {
        assert!(extract_facts_from_turn("", "s1", 1).is_empty());
        assert!(extract_facts_from_turn("   ", "s1", 1).is_empty());
    }

    #[test]
    fn name_extraction_zh() {
        let facts = extract_facts_from_turn("我叫张三, 是个开发者", "s1", 1);
        assert_eq!(kinds(&facts), vec!["user.name"]);
        assert_eq!(facts[0].payload["content"].as_str(), Some("张三"));
    }

    #[test]
    fn name_extraction_zh_long_form() {
        let facts = extract_facts_from_turn("我的名字是李四, 请记住", "s1", 1);
        // Both "我的名字是" and "请记住" may trigger; key dedup is
        // handled by upsert_fact not here.
        assert!(facts.iter().any(|f| f.canonical_key.starts_with("user.name")));
    }

    #[test]
    fn preference_extraction_zh() {
        let facts = extract_facts_from_turn("我喜欢 Rust 编程", "s1", 1);
        assert_eq!(kinds(&facts), vec!["user.preference"]);
        assert_eq!(facts[0].payload["content"].as_str(), Some("Rust 编程"));
    }

    #[test]
    fn tool_extraction_zh() {
        let facts = extract_facts_from_turn("我用 Tauri 写桌面应用", "s1", 1);
        assert_eq!(kinds(&facts), vec!["user.tool"]);
        assert_eq!(facts[0].payload["content"].as_str(), Some("Tauri 写桌面应用"));
    }

    #[test]
    fn english_name_extraction() {
        let facts = extract_facts_from_turn("My name is Alice", "s1", 1);
        assert_eq!(kinds(&facts), vec!["user.name"]);
        assert_eq!(facts[0].payload["content"].as_str(), Some("Alice"));
    }

    #[test]
    fn english_preference_extraction() {
        let facts = extract_facts_from_turn("I love Rust and Tauri", "s1", 1);
        assert_eq!(kinds(&facts), vec!["user.preference"]);
    }

    #[test]
    fn avoid_extraction_zh() {
        let facts = extract_facts_from_turn("不要用渐变色", "s1", 1);
        assert_eq!(kinds(&facts), vec!["user.avoid"]);
        assert_eq!(facts[0].payload["content"].as_str(), Some("用渐变色"));
    }

    #[test]
    fn no_trigger_returns_no_facts() {
        let facts = extract_facts_from_turn("今天天气不错", "s1", 1);
        assert!(facts.is_empty());
    }

    #[test]
    fn too_short_content_is_dropped() {
        // "我是" requires min 2 chars; "x" is too short
        let facts = extract_facts_from_turn("我是x", "s1", 1);
        assert!(facts.is_empty());
    }

    #[test]
    fn trigger_in_middle_of_word_rejected() {
        // "use" should not match if it's part of "because"
        let facts = extract_facts_from_turn("because i like Rust", "s1", 1);
        // Only "i like" should trigger; "use" inside "because" should not.
        assert_eq!(kinds(&facts), vec!["user.preference"]);
    }

    #[test]
    fn canonical_key_is_stable() {
        let facts = extract_facts_from_turn("我叫张三", "s1", 1);
        assert_eq!(facts[0].canonical_key, "user.name:张三");
    }

    #[test]
    fn tags_include_session_and_kind() {
        let facts = extract_facts_from_turn("我叫王五", "session42", 7);
        assert!(facts[0].tags.contains(&"session:session42".to_string()));
        assert!(facts[0].tags.contains(&"user.name".to_string()));
        assert!(facts[0].tags.contains(&"user_asserted".to_string()));
    }

    #[test]
    fn payload_carries_session_and_turn() {
        let facts = extract_facts_from_turn("我喜欢 Rust", "s99", 42);
        assert_eq!(facts[0].payload["session_id"].as_str(), Some("s99"));
        assert_eq!(facts[0].payload["turn_number"].as_u64(), Some(42));
    }

    #[test]
    fn punctuation_truncates_content() {
        let facts = extract_facts_from_turn("我叫张三, 住在上海", "s1", 1);
        assert_eq!(facts[0].payload["content"].as_str(), Some("张三"));
    }

    #[test]
    fn multiple_triggers_in_same_turn() {
        let facts = extract_facts_from_turn("我叫张三, 我用 Rust, 我喜欢 Tokio", "s1", 1);
        // 3 different kinds
        let k: Vec<&str> = facts.iter().map(|f| f.payload["kind"].as_str().unwrap()).collect();
        assert!(k.contains(&"user.name"));
        assert!(k.contains(&"user.tool"));
        assert!(k.contains(&"user.preference"));
    }
}