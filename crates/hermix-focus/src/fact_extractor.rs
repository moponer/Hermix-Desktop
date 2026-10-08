//! : facts 表 caller — 从 user turn 抽取结构化事实。
//!
//! 设计参考 `docs/MEMORY_LAYER_DEEP_ANALYSIS.md` §3 ：
//! `embedded.rs::upsert_fact` 函数 + schema 已就位，
//! `recognize()` 已经在 `fts_facts_visible` 查询 facts 表，
//! 但**没有任何 caller 把 turn 内容写入 facts** — 导致跨 session 召回
//! 永远命中不到 facts 表。
//!
//! 这个文件补上 caller：用一组 trigger pattern（中文 + 英文）抽取
//! user 的关键陈述（名字 / 偏好 / 工具 / 指令），调用
//! `EmbeddedMemoryBackend::upsert_fact`。
//!
//! ## 重写
//!
//! 旧实现用 `str::find` 子串匹配 + 字符级 prev-char 检查。两个真 bug：
//! 1. **否定吞噬** — "我不喜欢 Rust" → `find("我喜欢")` 在 index=1 命中，
//!    存为 `user.preference:Rust`（应存为 dislike 或不存）。
//! 2. **跨词匹配** — "use" 在 "because" 里也命中（已有 alphanumeric 检查
//!    缓解，但中文没边界）。
//!
//! 新实现：
//! - 中文 trigger：要求 trigger 在 clause 开头（前一个字符是 `,.;!?` 或
//!   字符串开头），且**前一个 token 不是否定词**（不/没/无/非/未/别）。
//! - 英文 trigger：用 `\b{prefix}\b` regex + case-insensitive。优先
//!   regex 匹配 `not|never|don't|do not` 前置否定 → 翻转为 dislike。
//! - 同一 turn 同一 kind 多次命中：只在 content 不同时保留，避免重复。
//!
//! ## 设计取舍
//!
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

use once_cell::sync::Lazy;
use regex::Regex;
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

/// (regex, base_kind, min_chars, language) trigger definition.
/// `regex` is matched against `user_lower` (English) or `user_trim`
/// (Chinese — regex still works, just less common). When the regex
/// matches, the substring AFTER the trigger word(s) becomes the
/// `content`.
/// `language` is `Zh` for CJK-only triggers (require clause-boundary
/// semantics) or `En` for ASCII triggers (use regex `\b`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lang {
    Zh,
    En,
}

struct Trigger {
    pattern: &'static str,
    base_kind: &'static str,
    min_chars: usize,
    lang: Lang,
    /// Optional: if this trigger is preceded by a negation word, flip
    /// `base_kind` to this alternative. For `"我喜欢"` (preference), a
    /// preceding `"不"` should produce a `user.dislike` instead.
    negated_kind: Option<&'static str>,
    /// Whether to enforce the clause-boundary check (Chinese only).
    /// False for triggers that *start with* a negation marker
    /// (`不爱`, `不喜欢`, `不用`, `不要`, `别`) — we want these to
    /// match regardless of clause position because they carry their
    /// own clause context. True for everything else (`我喜欢`, `我爱`,
    /// `我用`, `我叫`, etc.) — these need to sit at a clause boundary
    /// or they double-fire from substring noise.
    enforce_clause_boundary: bool,
}

const TRIGGERS: &[Trigger] = &[
    // 中文 — 用直接匹配的 trigger（不是 negation detection），因为中文
    // 否定词嵌入动词后，"我喜欢" 不是 "我不喜欢" 的子串，必须把
    // "不喜欢" / "不爱" / "不用" / "不要" 单独列为 dislike trigger。
    // enforce_clause_boundary = true 让 "我爱" 在 "我不爱" 里不重复触发。
    Trigger { pattern: "我叫",        base_kind: "user.name",        min_chars: 2, lang: Lang::Zh, negated_kind: None, enforce_clause_boundary: true },
    Trigger { pattern: "我的名字是",   base_kind: "user.name",        min_chars: 2, lang: Lang::Zh, negated_kind: None, enforce_clause_boundary: true },
    Trigger { pattern: "我的名字叫",   base_kind: "user.name",        min_chars: 2, lang: Lang::Zh, negated_kind: None, enforce_clause_boundary: true },
    Trigger { pattern: "我喜欢",      base_kind: "user.preference",  min_chars: 1, lang: Lang::Zh, negated_kind: None, enforce_clause_boundary: true },
    Trigger { pattern: "我爱",        base_kind: "user.preference",  min_chars: 1, lang: Lang::Zh, negated_kind: None, enforce_clause_boundary: true },
    // 否定动词 — clause boundary OFF：让"不爱"/"不喜欢"在"我不爱"中触发
    Trigger { pattern: "不喜欢",      base_kind: "user.dislike",     min_chars: 1, lang: Lang::Zh, negated_kind: None, enforce_clause_boundary: false },
    Trigger { pattern: "不爱",        base_kind: "user.dislike",     min_chars: 1, lang: Lang::Zh, negated_kind: None, enforce_clause_boundary: false },
    Trigger { pattern: "我讨厌",      base_kind: "user.dislike",     min_chars: 1, lang: Lang::Zh, negated_kind: None, enforce_clause_boundary: true },
    Trigger { pattern: "我恨",        base_kind: "user.dislike",     min_chars: 1, lang: Lang::Zh, negated_kind: None, enforce_clause_boundary: true },
    Trigger { pattern: "我用",        base_kind: "user.tool",        min_chars: 1, lang: Lang::Zh, negated_kind: None, enforce_clause_boundary: true },
    Trigger { pattern: "我在用",      base_kind: "user.tool",        min_chars: 1, lang: Lang::Zh, negated_kind: None, enforce_clause_boundary: true },
    Trigger { pattern: "我使用",      base_kind: "user.tool",        min_chars: 1, lang: Lang::Zh, negated_kind: None, enforce_clause_boundary: true },
    // avoid 类 min_chars=2：「别管」「别提了」这类单字宾语（"管"）不是
    // 可执行的偏好，存下来只是碎片噪音。
    Trigger { pattern: "不用",        base_kind: "user.avoid",       min_chars: 2, lang: Lang::Zh, negated_kind: None, enforce_clause_boundary: false },
    Trigger { pattern: "记住",        base_kind: "user.note",        min_chars: 2, lang: Lang::Zh, negated_kind: None, enforce_clause_boundary: true },
    Trigger { pattern: "请记住",      base_kind: "user.note",        min_chars: 2, lang: Lang::Zh, negated_kind: None, enforce_clause_boundary: true },
    Trigger { pattern: "不要",        base_kind: "user.avoid",       min_chars: 2, lang: Lang::Zh, negated_kind: None, enforce_clause_boundary: false },
    Trigger { pattern: "别",          base_kind: "user.avoid",       min_chars: 2, lang: Lang::Zh, negated_kind: None, enforce_clause_boundary: false },
    Trigger { pattern: "我是",        base_kind: "user.identity",    min_chars: 2, lang: Lang::Zh, negated_kind: None, enforce_clause_boundary: true },
    // 英文 — 否定词（not/never/don't/do not）独立于动词，所以用单字
    // "like"/"love"/"use" trigger + 前置 negation detection 就能覆盖
    // "I don't like Rust" / "I never use Slack" 这种模式。
    Trigger { pattern: r"\bmy name is\b",   base_kind: "user.name",       min_chars: 2, lang: Lang::En, negated_kind: None, enforce_clause_boundary: true },
    Trigger { pattern: r"\bi'm called\b",   base_kind: "user.name",       min_chars: 2, lang: Lang::En, negated_kind: None, enforce_clause_boundary: true },
    Trigger { pattern: r"\blike\b",         base_kind: "user.preference", min_chars: 1, lang: Lang::En, negated_kind: Some("user.dislike"), enforce_clause_boundary: true },
    Trigger { pattern: r"\blove\b",         base_kind: "user.preference", min_chars: 1, lang: Lang::En, negated_kind: Some("user.dislike"), enforce_clause_boundary: true },
    Trigger { pattern: r"\bhate\b",         base_kind: "user.dislike",    min_chars: 1, lang: Lang::En, negated_kind: None, enforce_clause_boundary: true },
    Trigger { pattern: r"\buse\b",          base_kind: "user.tool",       min_chars: 1, lang: Lang::En, negated_kind: None, enforce_clause_boundary: true },
    Trigger { pattern: r"\bremember\b",     base_kind: "user.note",       min_chars: 2, lang: Lang::En, negated_kind: None, enforce_clause_boundary: true },
    Trigger { pattern: r"\bdon'?t\b",       base_kind: "user.avoid",      min_chars: 1, lang: Lang::En, negated_kind: None, enforce_clause_boundary: true },
    Trigger { pattern: r"\bdo not\b",       base_kind: "user.avoid",      min_chars: 1, lang: Lang::En, negated_kind: None, enforce_clause_boundary: true },
    Trigger { pattern: r"\bi am\b",         base_kind: "user.identity",   min_chars: 2, lang: Lang::En, negated_kind: None, enforce_clause_boundary: true },
];

/// Pre-compiled regexes — one per trigger, cached process-wide.
/// ### Why this still panics on a bad pattern (and why it's safe)
/// `TRIGGER_REGEXES` is built from a static, hand-curated `TRIGGERS`
/// table — never from user data. Each entry's pattern is reviewed
/// by hand; a typo in the source table is a programming bug that
/// must fail loudly at first use rather than silently drop the
/// trigger. In practice this means a future patch that introduces
/// a bad pattern will crash the agent on startup instead of
/// corrupting memory — both are bugs, but the loud failure is
/// debuggable.
/// For **runtime** (user-supplied) patterns, see
/// `CompiledRuleSet::try_compile` which returns a `Result`.
static TRIGGER_REGEXES: Lazy<Vec<(Regex, &Trigger)>> = Lazy::new(|| {
    TRIGGERS
        .iter()
        .map(|t| {
            // English triggers are already regex; Chinese triggers are
            // passed through `regex::escape` so any future regex
            // meta-chars in the literal are matched verbatim. The
            // `regex` crate does NOT support `\Q...\E` (PCRE-only).
            let pat = match t.lang {
                Lang::En => t.pattern.to_string(),
                Lang::Zh => regex::escape(t.pattern),
            };
            // See the module-level note above: the table is static and
            // reviewed by hand, so a panic here is the right behaviour
            // (loud failure beats silently dropping a trigger).
            let re = Regex::new(&pat)
                .unwrap_or_else(|e| panic!("trigger regex compile failed for {:?}: {e}", t.pattern));
            (re, t)
        })
        .collect()
});

/// Negation words for Chinese (one-char / two-char prefixes).
/// When found immediately before a trigger, the trigger's kind flips
/// to `negated_kind` (or the fact is dropped if `negated_kind` is None).
const ZH_NEGATIONS: &[&str] = &["不", "没", "无", "非", "未", "别"];

/// English negation phrases — checked before an English trigger.
/// Matches `\b(?:not|never|don't|do not|no longer)\b`.
static EN_NEGATION: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(not|never|don'?t|do not|no longer)\b")
        .expect("negation regex")
});

/// Punctuation chars that always end the content window.
fn is_punctuation(c: char) -> bool {
    matches!(
        c,
        '。' | '，' | ',' | '.' | ';' | '；' | ':' | '!' | '?' | '\n' | '\r' | '\t'
    )
}

/// After the trigger, take the next content run until the first
/// punctuation. **Whitespace handling rule:**
/// - Pure-ASCII content: whitespace ends the window (`I like Rust`
/// → content = `Rust`).
/// - Mixed / CJK content: if the next non-whitespace char after a
/// space is CJK, treat the space as intra-phrase and keep going
/// (`我喜欢 Rust 编程` → content = `Rust 编程`, not just `Rust`).
/// This matches the previous behaviour the test suite expects
/// (`preference_extraction_zh` asserts `Rust 编程`) while also
/// cleanly bounding pure-ASCII recall so a sentence like `I like
/// Rust and I love Tauri` doesn't produce a single giant blob.
fn find_content_window(user: &str, trigger_end: usize) -> Option<(usize, usize)> {
    let after = user[trigger_end..].trim_start();
    let leading_ws = user[trigger_end..].len() - after.len();
    let start = trigger_end + leading_ws;
    let mut end = start;
    for (i, c) in user[start..].char_indices() {
        if is_punctuation(c) {
            return Some((start, start + i - start));
        }
        if c == ' ' {
            // Peek ahead: if the next non-space char is CJK, this
            // space is intra-phrase — keep going.
            let rest = &user[start + i + c.len_utf8()..];
            let next = rest.chars().find(|x| !x.is_whitespace());
            if matches!(next, Some(c) if !c.is_ascii()) {
                end = start + i + c.len_utf8();
            } else {
                return Some((start, start + i - start));
            }
        } else {
            end = start + i + c.len_utf8();
        }
    }
    Some((start, end - start))
}

/// Check whether `position` (byte offset in `user`) is immediately
/// preceded by a Chinese negation token (不/没/无/非/未/别).
/// "Immediately" means: scan backwards from `position`, skipping
/// whitespace. If the next non-whitespace character is one of the
/// negation single chars, OR a two-char combo like "我不" / "我没",
/// return `true`. For longer phrases ("我并不"/"我从未") we still
/// detect them because we walk back across non-punctuation chars.
fn preceded_by_zh_negation(user: &str, position: usize) -> bool {
    if position == 0 {
        return false;
    }
    // Walk back over up to 4 characters looking for a negation glyph.
    // We stop if we hit punctuation (because punctuation itself resets
    // the clause and the trigger wouldn't be in the same clause).
    let bytes = user.as_bytes();
    let mut i = position;
    let mut scan_count = 0;
    while i > 0 && scan_count < 4 {
        // Walk back one UTF-8 char (using char_indices reverse is
        // expensive — fall back to byte-walk and validate UTF-8).
        let prev_char_len = {
            let mut j = i - 1;
            // Skip continuation bytes (10xxxxxx).
            while j > 0 && (bytes[j] & 0xC0) == 0x80 {
                j -= 1;
            }
            i - j
        };
        i -= prev_char_len;
        let c = match user[i..i + prev_char_len].chars().next() {
            Some(c) => c,
            None => break,
        };
        scan_count += 1;
        // Stop scanning at punctuation — clause boundary resets context.
        if is_punctuation(c) || c.is_whitespace() {
            break;
        }
        if ZH_NEGATIONS.contains(&c.to_string().as_str()) {
            return true;
        }
    }
    false
}

/// Check whether `match_start` (byte offset of the trigger match in
/// `user_lower`) is preceded by an English negation phrase within a
/// 3-token window. Returns the negation match position if found.
fn preceded_by_en_negation(user_lower: &str, match_start: usize) -> bool {
    let window_start = match_start.saturating_sub(40);
    let window = &user_lower[window_start..match_start];
    // Find the LAST negation match in the window — the most recent
    // negation governs.
    if let Some(m) = EN_NEGATION.find_iter(window).last() {
        // Only count if there's no punctuation/clause-break between
        // the negation and the trigger.
        let between = &window[m.end()..];
        if !between
            .chars()
            .any(|c| c == '.' || c == '!' || c == '?' || c == ';')
        {
            return true;
        }
    }
    false
}

/// 主入口：从 user message 抽取所有事实。
/// 设计要点：
/// - 同一 turn 内多个 trigger 都会抽取，但 `canonical_key` 去重，
/// 第二次出现相同 key 时 `upsert_fact` 会 bump `access_count`。
/// - 否定检测：trigger 前有"我不/我没/我从不/我不爱"等否定词 → kind 翻转
/// 到 `negated_kind`（通常是 dislike），或完全跳过该 fact。
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

    let mut facts: Vec<ExtractedFact> = Vec::new();
    // Track seen (kind, content) to dedup within a single turn.
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();

    for (re, trigger) in TRIGGER_REGEXES.iter() {
        let haystack = match trigger.lang {
            Lang::En => user_lower.as_str(),
            Lang::Zh => user_trim,
        };
        for m in re.find_iter(haystack) {
            let trigger_end = m.end();

            //negation detection — must run BEFORE clause boundary check so
            // we can capture "我不喜欢" (which sits mid-clause) as a
            // dislike fact.
            let negated = match trigger.lang {
                Lang::Zh => preceded_by_zh_negation(user_trim, m.start()),
                Lang::En => preceded_by_en_negation(&user_lower, m.start()),
            };

            //clause-boundary check (Chinese) — the trigger must sit at the
            // start of a clause. We allow it after any punctuation OR at
            // offset 0 OR after whitespace. Without this, "我喜欢你 我喜欢你"
            // would double-fire from "喜欢" too.
            // EXCEPTION: when the match is negated, we want to capture
            // it regardless of clause position — "我不喜欢 Rust" must
            // produce a dislike fact even though "我喜欢" sits in the
            // middle of "我不喜欢".
            if trigger.lang == Lang::Zh && trigger.enforce_clause_boundary && m.start() > 0 && !negated {
                let before = user_trim[..m.start()].chars().last();
                if let Some(c) = before {
                    if !is_punctuation(c) && !c.is_whitespace() {
                        // Not at clause boundary — skip.
                        continue;
                    }
                }
            }

            let kind = if negated {
                match trigger.negated_kind {
                    Some(k) => k,
                    None => continue, // negation present but no flip kind → drop
                }
            } else {
                trigger.base_kind
            };

            // Find the content window AFTER the trigger.
            let Some((cs, cl)) = find_content_window(user_trim, trigger_end) else {
                continue;
            };
            let raw = &user_trim[cs..cs + cl];
            let content: String = raw.chars().take(100).collect();
            let content = content.trim();
            if content.chars().count() < trigger.min_chars {
                continue;
            }
            // Skip content that is itself only punctuation/whitespace.
            if content.chars().all(|c| !c.is_alphanumeric()) {
                continue;
            }
            // 碎片守卫：内容以闭括号/闭引号开头，说明触发词命中在括号
            // 内部（如「不要动台账（档案）、笔记…」窗口吞进「）、笔记」），
            // 截断语义已破——宁漏勿错。
            if matches!(
                content.chars().next(),
                Some('）') | Some('」') | Some('』') | Some('》') | Some(']') | Some('}')
            ) {
                continue;
            }
            // 助手名守卫：agent 自己的名字不该被记成 user.name
            //（此前出现过 user.name:Hermix 的错误事实）。
            if trigger.base_kind == "user.name"
                && matches!(content.to_lowercase().as_str(), "hermix" | "hermes")
            {
                continue;
            }
            let key = (kind.to_string(), content.to_string());
            if !seen.insert(key) {
                continue;
            }
            facts.push(ExtractedFact::new(kind, content, session_id, turn_number));
        }
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

    /// 碎片守卫：触发词命中在括号内部且紧跟闭括号时，内容窗口会吞进
    /// 「）、笔记」这类以闭括号开头的碎片——必须整条丢弃（真实事故：
    /// 存量库里出现过 `user.avoid:）、笔记（个人的记录）…`）。
    #[test]
    fn fragment_starting_with_closing_bracket_is_dropped() {
        let facts =
            extract_facts_from_turn("台账（不要）、笔记（个人的记录）、平台系统", "s1", 1);
        assert!(
            facts.is_empty(),
            "bracket-fragment must be dropped, got {facts:?}"
        );
    }

    /// 助手名守卫：agent 自己的名字不该被记成 user.name（真实事故：
    /// 存量库里出现过 `user.name:Hermix`）。
    #[test]
    fn agent_name_is_not_recorded_as_user_name() {
        let facts = extract_facts_from_turn("我叫 Hermix", "s1", 1);
        assert!(
            !facts.iter().any(|f| f.kind() == "user.name"),
            "agent name must not become user.name, got {facts:?}"
        );
    }

    /// avoid 类单字宾语碎片：「别管」→ content「管」只有 1 字，低于
    /// min_chars=2，丢弃（真实事故：存量库里出现过 `user.avoid:管`）。
    #[test]
    fn single_char_avoid_object_is_dropped() {
        let facts = extract_facts_from_turn("别管这个了，先跑测试", "s1", 1);
        assert!(
            !facts.iter().any(|f| f.payload["content"] == "管"),
            "single-char avoid content must be dropped, got {facts:?}"
        );
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

    // ------------------------------------------------------------------
    //negation regression tests — the original bug
    // ------------------------------------------------------------------

    /// (主报告 #5): "我不喜欢 Rust" must NOT be stored as a
    /// preference. Old code stored `user.preference:Rust` (wrong);
    /// new code either drops or flips to `user.dislike:Rust`.
    /// **Content window caveat:** the CJK verb "喜欢" extends past
    /// the "不喜欢" trigger, so the captured content may begin with
    /// the trailing 欢 (verb complement). We don't require exact
    /// content match — only the *kind* must flip and the object
    /// must surface in the content.
    #[test]
    fn m3_negation_zh_dislike_flips_preference() {
        let facts = extract_facts_from_turn("我不喜欢 Rust", "s1", 1);
        assert!(
            !facts.iter().any(|f| f.kind() == "user.preference"),
            "negated preference must not be stored as preference: got {:?}",
            facts.iter().map(|f| f.canonical_key.clone()).collect::<Vec<_>>()
        );
        let dislike: Vec<&ExtractedFact> = facts.iter().filter(|f| f.kind() == "user.dislike").collect();
        assert_eq!(dislike.len(), 1, "expected 1 dislike fact, got {dislike:?}");
        // Content should at least contain "Rust" (the object of dislike).
        let content = dislike[0].payload["content"].as_str().unwrap_or("");
        assert!(
            content.contains("Rust"),
            "dislike content should mention the object 'Rust', got '{content}'"
        );
    }

    /// 同样地 "我不爱 X" 应该 dislike
    #[test]
    fn m3_negation_zh_wo_ai_flips_preference() {
        let facts = extract_facts_from_turn("我不爱 Python", "s1", 1);
        assert!(!facts.iter().any(|f| f.kind() == "user.preference"));
        let dislike: Vec<&ExtractedFact> = facts.iter().filter(|f| f.kind() == "user.dislike").collect();
        assert_eq!(dislike.len(), 1, "expected 1 dislike fact");
        let content = dislike[0].payload["content"].as_str().unwrap_or("");
        assert!(content.contains("Python"), "got '{content}'");
    }

    /// 英文 "I don't like Rust" → dislike. English uses real
    /// negation detection (the verb is intact, the negation word is
    /// separate), so content extraction is clean.
    #[test]
    fn m3_negation_en_dont_flips_preference() {
        let facts = extract_facts_from_turn("I don't like Rust", "s1", 1);
        assert!(!facts.iter().any(|f| f.kind() == "user.preference"));
        let dislike: Vec<&ExtractedFact> = facts.iter().filter(|f| f.kind() == "user.dislike").collect();
        assert_eq!(dislike.len(), 1, "expected 1 dislike fact");
        assert_eq!(dislike[0].payload["content"].as_str(), Some("Rust"));
    }

    /// "I do not love Java" → dislike
    #[test]
    fn m3_negation_en_do_not_flips_preference() {
        let facts = extract_facts_from_turn("I do not love Java", "s1", 1);
        let dislike: Vec<&ExtractedFact> = facts.iter().filter(|f| f.kind() == "user.dislike").collect();
        assert_eq!(dislike.len(), 1, "expected 1 dislike fact");
        let content = dislike[0].payload["content"].as_str().unwrap_or("");
        assert!(content.contains("Java"), "got '{content}'");
    }

    /// "I never use Slack" → dislike (negation + i use)
    #[test]
    fn m3_negation_en_never_drops_tool() {
        // "i use" has negated_kind = None, so negated → drop entirely.
        let facts = extract_facts_from_turn("I never use Slack", "s1", 1);
        assert!(
            !facts.iter().any(|f| f.kind() == "user.tool"),
            "negated tool must be dropped, got {:?}",
            facts.iter().map(|f| f.canonical_key.clone()).collect::<Vec<_>>()
        );
    }

    /// positive cases still work after negation logic added.
    #[test]
    fn m3_positive_still_works_after_negation_logic() {
        let facts = extract_facts_from_turn("I like Rust and I love Tauri", "s1", 1);
        let prefs: Vec<&str> = facts.iter().map(|f| f.payload["content"].as_str().unwrap()).collect();
        assert!(prefs.contains(&"Rust"));
        assert!(prefs.contains(&"Tauri"));
        for f in &facts {
            assert_eq!(f.kind(), "user.preference");
        }
    }

    /// clause boundary — "我不" must not bleed across clauses.
    /// "我不讨厌 Rust, 我喜欢 Python" — the first clause ("I don't
    /// dislike Rust") has no good fact-kind mapping (it's a weak
    /// preference, but our kinds only have like/dislike/avoid/tool/etc,
    /// not "weak-preference") so the new code drops it. The second
    /// clause ("我喜欢 Python") cleanly maps to `user.preference`.
    #[test]
    fn m3_negation_does_not_bleed_across_clauses() {
        let facts = extract_facts_from_turn("我不讨厌 Rust, 我喜欢 Python", "s1", 1);
        // No dislike facts (the "我不讨厌" clause is dropped because
        // 否定 + dislike trigger with no flip kind → continue).
        let dislikes: Vec<&ExtractedFact> = facts.iter().filter(|f| f.kind() == "user.dislike").collect();
        assert_eq!(dislikes.len(), 0, "expected 0 dislikes (我不讨厌 drops), got {dislikes:?}");
        // 1 preference from "我喜欢 Python".
        let preferences: Vec<&ExtractedFact> = facts.iter().filter(|f| f.kind() == "user.preference").collect();
        assert_eq!(preferences.len(), 1, "expected 1 preference");
        assert_eq!(preferences[0].payload["content"].as_str(), Some("Python"));
    }

    /// English word boundary — "use" inside "because" must NOT match.
    /// "i use" has `\b` word boundary — old code matched substring.
    #[test]
    fn m3_english_word_boundary_strict() {
        let facts = extract_facts_from_turn("because I use Rust", "s1", 1);
        // Only "I use" should match (preceded by space, word boundary ok)
        // The "use" inside "because" is NOT preceded by word boundary.
        let tools: Vec<&ExtractedFact> = facts.iter().filter(|f| f.kind() == "user.tool").collect();
        assert_eq!(tools.len(), 1, "expected exactly 1 tool fact, got {}", tools.len());
    }
}

impl ExtractedFact {
    /// Helper for tests — returns the fact kind.
    #[cfg(test)]
    fn kind(&self) -> &str {
        self.payload["kind"].as_str().unwrap_or("")
    }
}
