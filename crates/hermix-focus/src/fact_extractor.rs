//! : facts 表 caller — 从 user turn 抽取结构化事实。
//!
//! 设计参考 `docs/MEMORY_LAYER_DEEP_ANALYSIS.md` §3 ：
//! `embedded.rs::upsert_fact` 函数 + schema 已就位，
//! `recognize()` 已经在 `fts_facts_visible` 查询 facts 表，
//! 但**没有任何 caller 把 turn 内容写入 facts** — 导致跨 session 召回
//! 永远命中不到 facts 表。
//!
//! 这个文件补上 caller：用一组 trigger pattern（中文 + 英文）抽取
//! user 的关键陈述（名字 / 显式「记住」指令），调用
//! `EmbeddedMemoryBackend::upsert_fact`。
//!
//! ## 收窄（2026-10-08，污染优先）
//!
//! 旧实现覆盖 preference / dislike / avoid / tool 等会话式触发词
//! （「我喜欢」「我用」「不要」「别」…）。真机事故：LLM 注入 5 条记忆
//! 3 条是碎片（`avoid:写3` / `avoid:测试` / `avoid:问直接清理掉`），
//! 且语义反转（「不用忘了测试」被抠成 avoid:测试——用户恰恰要求测试）。
//! **regex 无法理解语义，会话式触发词的碎片是定向污染**，宁可全砍。
//!
//! 现在只保留两类 regex 能高精度处理的显式陈述：
//! - 名字 / 身份：`我叫 / 我的名字是 / my name is` —— 结构固定。
//! - 显式记忆指令：`记住 X / 请记住 X / remember X` —— 用户明确
//!   要求记住，内容即约束本身。
//!
//! 隐式偏好（喜欢什么、讨厌什么、用什么工具）由 `memory` 工具
//! （LLM 自行判断值得记的）与 session md → facts 晋升负责；它们
//! 有语义理解，regex 没有。
//!
//! ## 防御
//!
//! - 否定前置（「我不叫」「没记住的是」）→ 丢弃，防语义反转。
//! - 内容以闭括号开头 → 丢弃（触发词命中在括号内的碎片）。
//! - 助手名（Hermix/Hermes）不进 user.name。
//! - 同一 turn 同一 kind 多次命中：只在 content 不同时保留。
//!
//! ## 数据形态
//!
//! 每个 fact 是三元组：
//! - `canonical_key` (e.g. `user.name:张三`, `user.note:临时文件直接清`)
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
    /// Whether to enforce the clause-boundary check (Chinese only).
    /// False for `请记住`（trigger 自带位置信息）；true for the rest.
    enforce_clause_boundary: bool,
}

/// 触发词白名单（2026-10-08 收窄）：见模块文档「收窄」节。会话式
/// 触发词（我喜欢/我用/不要/别…）的碎片是定向污染，全部移除；
/// 隐式偏好由 `memory` 工具与 md 晋升路径负责。
const TRIGGERS: &[Trigger] = &[
    // 中文 — 名字/身份：结构固定的显式陈述
    Trigger { pattern: "我叫",        base_kind: "user.name",        min_chars: 2, lang: Lang::Zh, enforce_clause_boundary: true },
    Trigger { pattern: "我的名字是",   base_kind: "user.name",        min_chars: 2, lang: Lang::Zh, enforce_clause_boundary: true },
    Trigger { pattern: "我的名字叫",   base_kind: "user.name",        min_chars: 2, lang: Lang::Zh, enforce_clause_boundary: true },
    // 中文 — 显式记忆指令：用户明确要求记住
    Trigger { pattern: "请记住",      base_kind: "user.note",        min_chars: 2, lang: Lang::Zh, enforce_clause_boundary: false },
    Trigger { pattern: "记住",        base_kind: "user.note",        min_chars: 2, lang: Lang::Zh, enforce_clause_boundary: true },
    // 英文 — 同上两类
    Trigger { pattern: r"\bmy name is\b",   base_kind: "user.name",       min_chars: 2, lang: Lang::En, enforce_clause_boundary: true },
    Trigger { pattern: r"\bi'm called\b",   base_kind: "user.name",       min_chars: 2, lang: Lang::En, enforce_clause_boundary: true },
    Trigger { pattern: r"\bremember\b",     base_kind: "user.note",       min_chars: 2, lang: Lang::En, enforce_clause_boundary: true },
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
        '。' | '，' | '、' | '：' | '！' | '？' | ',' | '.' | ';' | '；' | ':' | '!' | '?' | '\n' | '\r' | '\t'
    )
}

/// After the trigger, take the next content run until the first
/// punctuation. **Whitespace handling rule:**
/// - Pure-ASCII content: whitespace ends the window (`I like Rust`
/// → content = `Rust`).
/// - Mixed / CJK content: if the next non-whitespace char after a
/// space is CJK, treat the space as intra-phrase and keep going
/// (`我喜欢 Rust 编程` → content = `Rust 编程`, not just `Rust`).
/// **Leading punctuation** right after the trigger is skipped
/// （「记住：临时文件直接清」的冒号不是内容——存量事故里 content
/// 曾以「：」「）」开头）。
fn find_content_window(user: &str, trigger_end: usize) -> Option<(usize, usize)> {
    let mut start = trigger_end;
    for (i, c) in user[trigger_end..].char_indices() {
        if c.is_whitespace() || is_punctuation(c) {
            start = trigger_end + i + c.len_utf8();
        } else {
            start = trigger_end + i;
            break;
        }
    }
    if start >= user.len() {
        return None;
    }
    let mut end = start;
    for (i, c) in user[start..].char_indices() {
        if is_punctuation(c) {
            return Some((start, i));
        }
        if c == ' ' {
            // Peek ahead: if the next non-space char is CJK, this
            // space is intra-phrase — keep going.
            let rest = &user[start + i + c.len_utf8()..];
            let next = rest.chars().find(|x| !x.is_whitespace());
            if matches!(next, Some(c) if !c.is_ascii()) {
                end = start + i + c.len_utf8();
            } else {
                return Some((start, i));
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

            // 否定前置：语义反转风险（「我不叫张三」「别记住这个」），
            // 统一丢弃——宁漏勿错。
            if negated {
                continue;
            }
            let kind = trigger.base_kind;

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

    /// 显式记忆指令：「记住：X」是用户明确要求，content 是约束本身。
    /// 全角冒号必须被跳过（存量事故：content 曾以「：」开头）。
    #[test]
    fn note_extraction_zh() {
        let facts = extract_facts_from_turn("记住：临时文件直接清理，不询问", "s1", 1);
        assert_eq!(kinds(&facts), vec!["user.note"]);
        assert_eq!(facts[0].payload["content"].as_str(), Some("临时文件直接清理"));
    }

    #[test]
    fn note_extraction_zh_qing() {
        let facts = extract_facts_from_turn("请记住以后先跑测试再提交", "s1", 1);
        assert_eq!(kinds(&facts), vec!["user.note"]);
        assert_eq!(facts[0].payload["content"].as_str(), Some("以后先跑测试再提交"));
    }

    #[test]
    fn english_name_extraction() {
        let facts = extract_facts_from_turn("My name is Alice", "s1", 1);
        assert_eq!(kinds(&facts), vec!["user.name"]);
        assert_eq!(facts[0].payload["content"].as_str(), Some("Alice"));
    }

    /// 污染回归钉（2026-10-08 LLM 实报）：会话式触发词
    /// （preference/dislike/avoid/tool）整族移除——regex 理解不了
    /// 语义，「不用忘了测试」曾被抠成 avoid:测试，语义完全反转。
    /// 隐式偏好由 memory 工具与 md 晋升路径负责。
    #[test]
    fn conversational_triggers_produce_no_facts() {
        for text in [
            "我喜欢 Rust 编程",
            "我用 Tauri 写桌面应用",
            "不要写 3 遍",
            "别测试了直接上",
            "不用管这个",
            "I love Rust and Tauri",
            "I don't like Gradle",
            "I never use Slack",
        ] {
            let facts = extract_facts_from_turn(text, "s1", 1);
            assert!(
                facts.is_empty(),
                "conversational trigger must not produce facts: '{text}' → {facts:?}"
            );
        }
    }

    #[test]
    fn no_trigger_returns_no_facts() {
        let facts = extract_facts_from_turn("今天天气不错", "s1", 1);
        assert!(facts.is_empty());
    }

    /// 否定前置的名字陈述必须丢弃：「我不叫张三」存成 user.name:张三
    /// 是定向污染。
    #[test]
    fn negated_name_is_dropped() {
        let facts = extract_facts_from_turn("我不叫张三，你记错了", "s1", 1);
        assert!(facts.is_empty(), "negated name must be dropped, got {facts:?}");
    }

    /// 碎片守卫：触发词紧跟闭括号时，内容窗口会吞进「）、笔记」这类
    /// 以闭括号开头的碎片——必须整条丢弃（真实事故：存量库里出现过
    /// `user.avoid:）、笔记（个人的记录）…`）。
    #[test]
    fn fragment_starting_with_closing_bracket_is_dropped() {
        let facts = extract_facts_from_turn("记住）、笔记（个人的记录）、平台系统", "s1", 1);
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
        let facts = extract_facts_from_turn("我叫小林", "s99", 42);
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
        let facts = extract_facts_from_turn("我叫张三, 记住临时文件直接清", "s1", 1);
        let k: Vec<&str> = facts.iter().map(|f| f.payload["kind"].as_str().unwrap()).collect();
        assert!(k.contains(&"user.name"));
        assert!(k.contains(&"user.note"));
    }
}

impl ExtractedFact {
    /// Helper for tests — returns the fact kind.
    #[cfg(test)]
    fn kind(&self) -> &str {
        self.payload["kind"].as_str().unwrap_or("")
    }
}
