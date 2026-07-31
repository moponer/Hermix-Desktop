//! Time-aware recall — high-level façade around [`time_recall`].
//!
//! Reference: `docs/PROPOSAL_BAILONGMA_INSPIRED.md` §3 H4 (时间词召回).
//!
//! ## What this adds on top of [`crate::time_recall`]
//!
//! `time_recall` exposes two loose free functions: `extract_first` and
//! `extract_time_window`. They're convenient for one-off lookups, but
//! the agent loop needs more:
//!
//! 1. **Classification of which kind of recall the user is asking for**
//!    — a bare "昨天" hits a different code path than "上周我们做的那个
//!    bug 修复" (which also implies a search intent). [`RecallIntent`]
//!    exposes the typed answer the agent loop / router can dispatch on.
//! 2. **Per-backend hints** — some backends (e.g. Hindsight, Honcho)
//!    accept a `since` / `until` parameter in their native query shape;
//!    the embedded SQLite backend wants a `TimeWindow`. [`RecallHint`]
//!    is the backend-neutral envelope so each backend picks what it
//!    can honour.
//! 3. **Stable span description** for logging / debug overlays —
//!    `RecallSpan::description()` returns "last 7 days" or "since
//!    00:00 UTC" depending on the precision we recovered.
//! 4. **Multi-match** — a single query may contain two time
//!    expressions ("上周的 plan，本周三 review"). The first is the
//!    primary filter; secondary ones are kept for hinting the agent
//!    context but not used for filtering.
//!
//! This struct is the entry point. [`crate::time_recall`] stays the
//! regex + numeral parser; this file glues the policy on top.
//!
//! ## Why a struct, not another free function
//!
//! - **Testability** — inject a fixed `now` for deterministic span
//!   boundaries (the existing free functions all use `Utc::now()` and
//!   have to use `w.contains(now ± slack)` assertions).
//! - **Composition** — `MemoryRouter` can hold a `TimeAwareRecall`
//!   instance and call `analyze(query)` from `recognize()` without
//!   re-parsing the query N times per backend.
//! - **Future-proofing** — when we add multi-language time grammar
//!   (Japanese "先週", Spanish "la semana pasada"), the parser plug-in
//!   can live as a struct field without breaking call sites.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::memory_layer::{Intent, TimeWindow};
use crate::time_recall::{self, TimeExpression};

/// Classification of the recall intent derived from the time
/// expression found in the query (or lack thereof).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RecallIntent {
    /// "What did I/we do in this window?" — surface relevant past
    /// frames. The most common case ("昨天我们聊了什么").
    Recall,
    /// "Continue from where we left off" — most-recent-first. Triggered
    /// by "刚才", "刚才那条消息", "earlier".
    Resume,
    /// "Summarize session/conversation" — prefer focus conclusions.
    /// Triggered by "今天总结", "本周总结".
    Summarize,
    /// No time expression at all. The agent loop falls back to plain
    /// recall without a time filter.
    Unfiltered,
}

impl RecallIntent {
    /// Pick the [`Intent`] variant the router should pass to
    /// `MemoryLayer::recognize`. Backend-specific bias lives there.
    pub fn to_memory_intent(&self) -> Intent {
        match self {
            RecallIntent::Resume => Intent::Resume,
            RecallIntent::Summarize => Intent::Summarize,
            RecallIntent::Recall | RecallIntent::Unfiltered => Intent::Recall,
        }
    }

    /// Heuristic classifier from a time expression.
    /// Mapping:
    /// - [`TimeExpression::JustNow`] / `JustNow` text markers
    /// ("earlier", "a moment ago") → `Resume`
    /// - [`TimeExpression::Today`] with a `Summarize` keyword in
    /// the query → `Summarize`, else `Recall`
    /// - everything else with a time expression → `Recall`
    /// - no time expression → `Unfiltered`
    fn from_query(query: &str, expr: Option<&TimeExpression>) -> Self {
        let lower = query.to_lowercase();
        let summarize_kw = ["总结", "summary", "summarize", "小结"];
        let has_summarize = summarize_kw.iter().any(|kw| lower.contains(kw));

        match expr {
            None => RecallIntent::Unfiltered,
            Some(TimeExpression::JustNow) => RecallIntent::Resume,
            Some(_) if has_summarize => RecallIntent::Summarize,
            Some(_) => RecallIntent::Recall,
        }
    }
}

/// A single time expression extracted from the query, with the
/// resolved window and a span description suitable for logs and UI
/// overlays. The `confidence` is a [0.0, 1.0] score reflecting how
/// precise the recovered window is (a named day is more confident
/// than a fuzzy "上周" relative to the current weekday).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallSpan {
    /// The raw text the regex matched.
    pub raw: String,
    /// The typed expression we resolved it to.
    pub expression: TimeExpression,
    /// The window it resolves to (computed against `now` at
    /// construction time).
    pub window: TimeWindow,
}

impl RecallSpan {
    /// Precision score in [0.0, 1.0]:
    /// - 1.0 for absolute dates (none yet — we don't parse those)
    /// - 0.9 for exact "N days/hours ago"
    /// - 0.8 for named day boundaries ("昨天", "今天")
    /// - 0.6 for named week boundaries ("上周", "本周")
    /// - 0.4 for named month boundaries ("上个月", "本月")
    pub fn confidence(&self) -> f32 {
        match self.expression {
            TimeExpression::DaysAgo(_) | TimeExpression::HoursAgo(_) => 0.9,
            TimeExpression::Yesterday
            | TimeExpression::Today
            | TimeExpression::DayBeforeYesterday
            | TimeExpression::JustNow => 0.8,
            TimeExpression::ThisWeek | TimeExpression::LastWeek => 0.6,
            TimeExpression::ThisMonth | TimeExpression::LastMonth => 0.4,
            //forward-tense 时间词 — 召回未来 reminder/plan
            // 时置信度低（当前无数据，模型只能凭语义判断）。
            TimeExpression::Tomorrow
            | TimeExpression::DayAfterTomorrow
            | TimeExpression::HoursLater(_) => 0.5,
            TimeExpression::NextWeek
            | TimeExpression::DaysLater(_)
            | TimeExpression::WeeksLater(_) => 0.4,
            TimeExpression::NextMonth => 0.3,
        }
    }

    /// Human-readable description of the window for log / UI overlays.
    /// Examples: "last 24h", "3 days ago", "this week", "last month".
    pub fn description(&self) -> String {
        match &self.expression {
            TimeExpression::JustNow => "last 1h".to_string(),
            TimeExpression::Today => "last 24h".to_string(),
            TimeExpression::Yesterday => "yesterday (24h–48h)".to_string(),
            TimeExpression::DayBeforeYesterday => "day before (48h–72h)".to_string(),
            TimeExpression::ThisWeek => "this week (Mon → now)".to_string(),
            TimeExpression::LastWeek => "last week (Mon→Sun)".to_string(),
            TimeExpression::ThisMonth => "this month (1st → now)".to_string(),
            TimeExpression::LastMonth => "last month".to_string(),
            TimeExpression::HoursAgo(n) => format!("{n}h ago"),
            TimeExpression::DaysAgo(n) => format!("{n}d ago"),
            //forward-tense descriptions.
            TimeExpression::Tomorrow => "tomorrow (0h–48h)".to_string(),
            TimeExpression::DayAfterTomorrow => "day after (0h–72h)".to_string(),
            TimeExpression::NextWeek => "next week (now → +14d)".to_string(),
            TimeExpression::NextMonth => "next month".to_string(),
            TimeExpression::HoursLater(n) => format!("+{n}h"),
            TimeExpression::DaysLater(n) => format!("+{n}d"),
            TimeExpression::WeeksLater(n) => format!("+{}w", n * 7),
        }
    }
}

/// Backend-neutral hint to apply to a recall. Backends that have a
/// native time filter can consume this; backends that don't can ignore
/// it and let the router apply the post-filter.
#[derive(Debug, Clone)]
pub struct RecallHint {
    /// Optional window the backend can apply directly.
    pub window: Option<TimeWindow>,
    /// Suggested [`Intent`] for the backend to bias by.
    pub intent: Intent,
    /// Span description for logging / UI.
    pub description: String,
    /// Precision confidence in [0.0, 1.0].
    pub confidence: f32,
}

impl RecallHint {
    /// No time filter; backend uses its default scoring.
    pub fn unfiltered() -> Self {
        Self {
            window: None,
            intent: Intent::Recall,
            description: "no time filter".to_string(),
            confidence: 0.0,
        }
    }

    /// True if the hint is more than just "no filter" — i.e. there
    /// was a time expression to apply.
    pub fn is_active(&self) -> bool {
        self.window.is_some()
    }
}

/// The high-level façade. Construct with a fixed `now` for
/// deterministic tests, or use [`TimeAwareRecall::for_now`] for
/// production (uses `Utc::now()`).
#[derive(Debug, Clone)]
pub struct TimeAwareRecall {
    now: DateTime<Utc>,
}

impl TimeAwareRecall {
    /// Build a recall engine pinned to a specific instant. The
    /// returned windows are stable for that instant; pass
    /// `Utc::now()` for production or a fixed time in tests.
    pub fn new(now: DateTime<Utc>) -> Self {
        Self { now }
    }

    /// Convenience constructor using the current wall clock.
    pub fn for_now() -> Self {
        Self::new(Utc::now())
    }

    /// The reference time this engine is pinned to. Exposed for tests
    /// that want to assert "the window starts at now - 24h exactly".
    pub fn now(&self) -> DateTime<Utc> {
        self.now
    }

    /// Extract all time expressions from a free-form query.
    /// Order of returned spans matches order of appearance in the
    /// query. The first span is the *primary* filter; the rest are
    /// kept for context (e.g. an agent that wants to mention "also
    /// saw something last week") but are not used for filtering.
    pub fn extract_spans(&self, query: &str) -> Vec<RecallSpan> {
        // We delegate the regex / numeral work to the existing
        // `time_recall` module, but the public surface there is
        // `extract_first` only. Re-implement the "find all" pass here
        // using the same regexes so we don't widen the time_recall API
        // for a single caller.
        let mut spans = Vec::new();
        let mut byte_cursor = 0usize;
        while byte_cursor < query.len() {
            let remaining = &query[byte_cursor..];
            // Ensure we always re-enter the loop with a char-boundary
            // cursor. Defensive: callers pass `query` whole, so the
            // initial cursor is at 0, and we only ever *add* `match.len()`
            // to it (which is always a char boundary).
            if !query.is_char_boundary(byte_cursor) {
                break;
            }
            match next_expression(remaining) {
                Some((raw, _start_in_remaining, expr)) => {
                    let window = expr.to_window(self.now);
                    spans.push(RecallSpan {
                        raw: raw.to_string(),
                        expression: expr,
                        window,
                    });
                    // `raw.len()` is the byte length of the match.
                    // We must advance past `start + raw.len()` to skip
                    // over it entirely. The match begins at
                    // `byte_cursor + start_in_remaining` in `query`.
                    byte_cursor += _start_in_remaining + raw.len();
                }
                None => break,
            }
        }
        spans
    }

    /// Extract the *primary* time expression — the first one in the
    /// query. Returns `None` if no time expression was found.
    pub fn primary_span(&self, query: &str) -> Option<RecallSpan> {
        self.extract_spans(query).into_iter().next()
    }

    /// Classify the recall intent from the query. Considers both the
    /// time expression (if any) and a small set of summarize / resume
    /// keywords in the surrounding text.
    pub fn classify(&self, query: &str) -> RecallIntent {
        let primary = self.primary_span(query);
        RecallIntent::from_query(query, primary.as_ref().map(|s| &s.expression))
    }

    /// One-shot helper that returns a [`RecallHint`] the router can
    /// pass to its backends. This is the canonical entry point —
    /// `MemoryRouter::recognize` calls this and threads the hint
    /// through.
    /// Behaviour:
    /// - No time expression → `RecallHint::unfiltered()` with
    /// `Recall` intent.
    /// - Time expression present → window + classified intent +
    /// description.
    /// - If classification yields `Summarize`, the window is
    /// widened by 1 day on each side: summaries usually span
    /// "the day that just ended" → "yesterday and today".
    pub fn analyze(&self, query: &str) -> RecallHint {
        let primary = match self.primary_span(query) {
            Some(p) => p,
            None => return RecallHint::unfiltered(),
        };
        let intent = RecallIntent::from_query(query, Some(&primary.expression));
        let mut window = primary.window.clone();
        let mut description = primary.description();

        if matches!(intent, RecallIntent::Summarize) {
            // Widen the window for "今天总结" / "本周总结" so the
            // summary naturally covers the just-finished day/week as
            // well as the current one.
            window = TimeWindow {
                start: window.start - chrono::Duration::days(1),
                end: window.end,
            };
            description.push_str(" (widened for summary)");
        }

        let confidence = match primary.expression {
            TimeExpression::DaysAgo(_) | TimeExpression::HoursAgo(_) => 0.9,
            TimeExpression::Yesterday
            | TimeExpression::Today
            | TimeExpression::DayBeforeYesterday
            | TimeExpression::JustNow => 0.8,
            TimeExpression::ThisWeek | TimeExpression::LastWeek => 0.6,
            TimeExpression::ThisMonth | TimeExpression::LastMonth => 0.4,
            //forward-tense 时间词 — 置信度低于过去时（无未来数据）。
            TimeExpression::Tomorrow
            | TimeExpression::DayAfterTomorrow
            | TimeExpression::HoursLater(_) => 0.5,
            TimeExpression::NextWeek
            | TimeExpression::DaysLater(_)
            | TimeExpression::WeeksLater(_) => 0.4,
            TimeExpression::NextMonth => 0.3,
        };

        RecallHint {
            window: Some(window),
            intent: intent.to_memory_intent(),
            description,
            confidence,
        }
    }
}

// --- internal: walk-the-string extractor --------------------------------
// We need every match in order, not just the first one. The existing
// `time_recall::extract_first` only returns one. The regex set is
// duplicated here (kept private) so we can scan in a loop without
// widening the lower-level API for a single caller.

fn next_expression(remaining: &str) -> Option<(&str, usize, TimeExpression)> {
    use once_cell::sync::Lazy;
    use regex::Regex;
    static RE_CHINESE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(今天|昨天|前天|刚才|本周|这周|上周|这个月|这月|本月|上个月|上月|(\d+|[零一二三四五六七八九十百两]+)\s*(?:个)?\s*(?:小时|天|日|周|个月)前)",
        )
        .unwrap()
    });
    static RE_ENGLISH: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)\b(today|yesterday|just now|this week|last week|this month|last month|(\d+)\s+(hours?|days?|weeks?|months?)\s+ago)\b",
        )
        .unwrap()
    });

    let cn = RE_CHINESE.find(remaining);
    let en = RE_ENGLISH.find(remaining);
    let pick = match (cn, en) {
        (Some(c), Some(e)) => {
            if c.start() <= e.start() {
                c
            } else {
                e
            }
        }
        (Some(c), None) => c,
        (None, Some(e)) => e,
        (None, None) => return None,
    };
    let start = pick.start();
    let raw = pick.as_str();
    let expr = parse_expression(raw)?;
    Some((raw, start, expr))
}

fn parse_expression(s: &str) -> Option<TimeExpression> {
    let t = s.trim();
    match t.to_lowercase().as_str() {
        "today" => Some(TimeExpression::Today),
        "yesterday" => Some(TimeExpression::Yesterday),
        "just now" => Some(TimeExpression::JustNow),
        "this week" => Some(TimeExpression::ThisWeek),
        "last week" => Some(TimeExpression::LastWeek),
        "this month" => Some(TimeExpression::ThisMonth),
        "last month" => Some(TimeExpression::LastMonth),
        _ => {
            // Defer numeric/Chinese-numeral parsing to the canonical
            // time_recall helper so we don't duplicate numeral logic.
            time_recall::extract_first(s)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone, Utc};

    fn fixed_now() -> DateTime<Utc> {
        //12:00:00 UTC (a Tuesday — start_of_week = )
        Utc.with_ymd_and_hms(2026, 6, 9, 12, 0, 0).unwrap()
    }

    #[test]
    fn no_time_expression_returns_unfiltered_hint() {
        let r = TimeAwareRecall::new(fixed_now());
        let hint = r.analyze("hello world");
        assert!(!hint.is_active());
        assert_eq!(hint.intent, Intent::Recall);
        assert_eq!(hint.description, "no time filter");
    }

    #[test]
    fn chinese_today_yields_recall_with_24h_window() {
        let now = fixed_now();
        let r = TimeAwareRecall::new(now);
        let hint = r.analyze("今天我们做了什么");
        assert!(hint.is_active());
        assert_eq!(hint.intent, Intent::Recall);
        assert_eq!(hint.description, "last 24h");
        let w = hint.window.as_ref().unwrap();
        assert_eq!(w.end, Some(now));
        assert_eq!(w.start, now - Duration::hours(24));
    }

    #[test]
    fn chinese_yesterday_window_is_24_48h_ago() {
        let now = fixed_now();
        let r = TimeAwareRecall::new(now);
        let hint = r.analyze("昨天");
        let w = hint.window.as_ref().unwrap();
        assert_eq!(w.end, Some(now - Duration::hours(24)));
        assert_eq!(w.start, now - Duration::hours(48));
    }

    #[test]
    fn english_last_week_is_recall() {
        let now = fixed_now();
        let r = TimeAwareRecall::new(now);
        let hint = r.analyze("what did we work on last week?");
        assert!(hint.is_active());
        assert_eq!(hint.intent, Intent::Recall);
        assert_eq!(hint.description, "last week (Mon→Sun)");
    }

    #[test]
    fn just_now_is_resume_intent() {
        let now = fixed_now();
        let r = TimeAwareRecall::new(now);
        let hint = r.analyze("刚才那条消息在哪");
        assert!(hint.is_active());
        assert_eq!(hint.intent, Intent::Resume);
    }

    #[test]
    fn summarize_keyword_widens_window() {
        let now = fixed_now();
        let r = TimeAwareRecall::new(now);
        let hint = r.analyze("今天总结一下进度");
        assert!(hint.is_active());
        assert_eq!(hint.intent, Intent::Summarize);
        // Window should be 1 day wider than a plain "今天" call.
        let plain = r.analyze("今天我们做了什么");
        let plain_w = plain.window.as_ref().unwrap();
        let sum_w = hint.window.as_ref().unwrap();
        assert_eq!(
            sum_w.start,
            plain_w.start - Duration::days(1),
            "summarize window should extend 1 day earlier"
        );
    }

    #[test]
    fn multiple_time_expressions_collected_in_order() {
        let now = fixed_now();
        let r = TimeAwareRecall::new(now);
        let spans = r.extract_spans("上周的 plan，本周三 review");
        assert_eq!(spans.len(), 2);
        // First = 上周, second = 本周三 (which falls under "this week"
        // since we don't yet have a "本周X" parser — the regex matches
        // the substring "本周").
        assert!(spans[0].description().contains("last week"));
    }

    #[test]
    fn primary_span_returns_first() {
        let now = fixed_now();
        let r = TimeAwareRecall::new(now);
        let span = r.primary_span("三天前我们说好的，昨天又做了一些").unwrap();
        assert_eq!(span.description(), "3d ago");
    }

    #[test]
    fn classify_unfiltered_when_no_expression() {
        let r = TimeAwareRecall::new(fixed_now());
        assert_eq!(r.classify("hello"), RecallIntent::Unfiltered);
    }

    #[test]
    fn classify_resume_on_just_now() {
        let r = TimeAwareRecall::new(fixed_now());
        assert_eq!(r.classify("刚才那条"), RecallIntent::Resume);
    }

    #[test]
    fn classify_summarize_on_keyword_with_time() {
        let r = TimeAwareRecall::new(fixed_now());
        assert_eq!(r.classify("本周总结一下"), RecallIntent::Summarize);
    }

    #[test]
    fn classify_recall_for_plain_time_expression() {
        let r = TimeAwareRecall::new(fixed_now());
        assert_eq!(r.classify("昨天我们做了什么"), RecallIntent::Recall);
        assert_eq!(r.classify("yesterday we did X"), RecallIntent::Recall);
    }

    #[test]
    fn confidence_scores_are_ordered() {
        let r = TimeAwareRecall::new(fixed_now());
        let n_days = r.analyze("3 days ago");
        let yesterday = r.analyze("yesterday");
        let last_month = r.analyze("last month");
        assert!(n_days.confidence > yesterday.confidence);
        assert!(yesterday.confidence > last_month.confidence);
    }

    #[test]
    fn hint_unfiltered_is_inactive() {
        let h = RecallHint::unfiltered();
        assert!(!h.is_active());
        assert_eq!(h.intent, Intent::Recall);
    }

    #[test]
    fn span_descriptions_are_human_readable() {
        let r = TimeAwareRecall::new(fixed_now());
        let cases = [
            ("今天", "last 24h"),
            ("yesterday", "yesterday (24h–48h)"),
            ("5 小时前", "5h ago"),
            ("3 天前", "3d ago"),
        ];
        for (q, want) in cases {
            let span = r.primary_span(q).expect(q);
            assert_eq!(span.description(), want, "for query: {q}");
        }
    }
}
