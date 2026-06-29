//! Time-aware recall — extract time expressions from a query and convert
//! them to a `TimeWindow` for filtering `MemoryLayer::recognize` results.
//!
//! Reference: `docs/PROPOSAL_BAILONGMA_INSPIRED.md` §2.1 (时间词召回) +
//! §三 H4 (时间词召回, 1-2 天).
//!
//! Supports both Chinese and English time expressions:
//!
//! | Expression | Window |
//! |-----------|--------|
//! | "昨天" / "yesterday" | last 24h–48h |
//! | "今天" / "today" | last 0h–24h |
//! | "前天" / "day before yesterday" | last 48h–72h |
//! | "上周" / "last week" | last 7d–14d |
//! | "本周" / "this week" | last 0d–7d |
//! | "上个月" / "last month" | last 30d–60d |
//! | "本月" / "this month" | last 0d–30d |
//! | "刚才" / "just now" | last 0h–1h |
//! | "N 小时前" / "N hours ago" | last N-1h..Nh |
//! | "N 天前" / "N days ago" | last N-1d..Nd |
//!
//! If no time expression is found, returns `None` (no filter applied).

use chrono::{DateTime, Datelike, Duration, Utc, Weekday};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::memory_layer::TimeWindow;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TimeExpression {
    Yesterday,
    Today,
    DayBeforeYesterday,
    LastWeek,
    ThisWeek,
    LastMonth,
    ThisMonth,
    JustNow,
    HoursAgo(i64),
    DaysAgo(i64),
    // Phase 18 (P0-4): forward-tense 时间词（明天/下周/N 天后）。
    // 默认窗口为 [now, now + N days]，recall 在该时间范围内查找
    // 未来事件。当前没有未来数据，所以这类 query 通常返回空集——
    // 但语义正确，当未来 reminder / plan 入库后会自动命中。
    Tomorrow,
    DayAfterTomorrow,
    NextWeek,
    NextMonth,
    HoursLater(i64),
    DaysLater(i64),
    WeeksLater(i64),
}

impl TimeExpression {
    pub fn to_window(&self, now: DateTime<Utc>) -> TimeWindow {
        match self {
            TimeExpression::JustNow => TimeWindow {
                start: now - Duration::hours(1),
                end: Some(now),
            },
            TimeExpression::Today => TimeWindow {
                start: now - Duration::hours(24),
                end: Some(now),
            },
            TimeExpression::Yesterday => TimeWindow {
                start: now - Duration::hours(48),
                end: Some(now - Duration::hours(24)),
            },
            TimeExpression::DayBeforeYesterday => TimeWindow {
                start: now - Duration::hours(72),
                end: Some(now - Duration::hours(48)),
            },
            TimeExpression::ThisWeek => TimeWindow {
                start: start_of_week(now),
                end: Some(now),
            },
            TimeExpression::LastWeek => TimeWindow {
                start: start_of_week(now) - Duration::days(7),
                end: Some(start_of_week(now)),
            },
            TimeExpression::ThisMonth => TimeWindow {
                start: start_of_month(now),
                end: Some(now),
            },
            TimeExpression::LastMonth => TimeWindow {
                start: start_of_prev_month(now),
                end: Some(start_of_month(now)),
            },
            TimeExpression::HoursAgo(n) => TimeWindow {
                start: now - Duration::hours(*n + 1),
                end: Some(now - Duration::hours(*n - 1).max(Duration::hours(0))),
            },
            TimeExpression::DaysAgo(n) => TimeWindow {
                start: now - Duration::days(*n + 1),
                end: Some(now - Duration::days(*n - 1).max(Duration::days(0))),
            },
            // Forward-tense windows: [now, now + offset].
            // For "next week/month" we anchor to the calendar
            // boundary so the user actually means "the week after
            // this one", not "some random 7-day span starting now".
            TimeExpression::Tomorrow => TimeWindow {
                start: now,
                end: Some(now + Duration::hours(48)),
            },
            TimeExpression::DayAfterTomorrow => TimeWindow {
                start: now,
                end: Some(now + Duration::hours(72)),
            },
            TimeExpression::NextWeek => TimeWindow {
                start: now,
                end: Some(start_of_week(now) + Duration::days(14)),
            },
            TimeExpression::NextMonth => TimeWindow {
                start: now,
                end: Some(start_of_month(start_of_month(now) + Duration::days(32))
                          + Duration::days(31)),
            },
            TimeExpression::HoursLater(n) => TimeWindow {
                start: now,
                end: Some(now + Duration::hours(*n + 1)),
            },
            TimeExpression::DaysLater(n) => TimeWindow {
                start: now,
                end: Some(now + Duration::days(*n + 1)),
            },
            TimeExpression::WeeksLater(n) => TimeWindow {
                start: now,
                end: Some(now + Duration::days(*n * 7 + 1)),
            },
        }
    }
}

fn start_of_week(now: DateTime<Utc>) -> DateTime<Utc> {
    let weekday = now.weekday();
    let offset = match weekday {
        Weekday::Mon => 0,
        Weekday::Tue => 1,
        Weekday::Wed => 2,
        Weekday::Thu => 3,
        Weekday::Fri => 4,
        Weekday::Sat => 5,
        Weekday::Sun => 6,
    };
    now.date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        - Duration::days(offset)
}

fn start_of_month(now: DateTime<Utc>) -> DateTime<Utc> {
    let nd = now.date_naive();
    nd.with_day(1)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|d| d.and_utc())
        .unwrap_or(now)
}

fn start_of_prev_month(now: DateTime<Utc>) -> DateTime<Utc> {
    let first = start_of_month(now);
    first - Duration::days((first.day() - 1) as i64)
}

// --- regex patterns (Chinese + English) -------------------------------

static RE_CHINESE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(今天|昨天|前天|明天|后天|刚才|本周|这周|上周|下周|这个月|这月|本月|下个月|下月|上个月|上月|(\d+|[零一二三四五六七八九十百两]+)\s*(?:个)?\s*(?:小时|天|日|周|个月)(?:前|后))",
    )
    .unwrap()
});

static RE_ENGLISH: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\b(today|yesterday|tomorrow|day after tomorrow|just now|this week|last week|next week|this month|last month|next month|(\d+)\s+(hours?|days?|weeks?|months?)\s+(ago|later))\b",
    )
    .unwrap()
});

/// Extract time expressions from a free-form query. Returns all matches
/// in order of appearance. The first match's window is typically used
/// downstream; secondary matches are ignored.
pub fn extract_time_window(query: &str) -> Option<TimeWindow> {
    let expr = extract_first(query)?;
    Some(expr.to_window(Utc::now()))
}

/// Same as [`extract_time_window`] but returns the typed expression.
pub fn extract_first(query: &str) -> Option<TimeExpression> {
    // Prefer Chinese first (more specific phrases like "上周", "刚才")
    if let Some(c) = RE_CHINESE.find(query) {
        if let Some(expr) = parse_chinese(c.as_str()) {
            return Some(expr);
        }
    }
    if let Some(c) = RE_ENGLISH.find(query) {
        if let Some(expr) = parse_english(c.as_str()) {
            return Some(expr);
        }
    }
    None
}

fn parse_chinese(s: &str) -> Option<TimeExpression> {
    let t = s.trim();
    match t {
        "刚才" => Some(TimeExpression::JustNow),
        "今天" => Some(TimeExpression::Today),
        "昨天" => Some(TimeExpression::Yesterday),
        "前天" => Some(TimeExpression::DayBeforeYesterday),
        "明天" => Some(TimeExpression::Tomorrow),
        "后天" => Some(TimeExpression::DayAfterTomorrow),
        "本周" | "这周" => Some(TimeExpression::ThisWeek),
        "上周" => Some(TimeExpression::LastWeek),
        "下周" => Some(TimeExpression::NextWeek),
        "本月" | "这月" | "这个月" => Some(TimeExpression::ThisMonth),
        "上个月" | "上月" => Some(TimeExpression::LastMonth),
        "下个月" | "下月" => Some(TimeExpression::NextMonth),
        _ => {
            // Either Arabic ("3 天前") or Chinese ("三天前") numerals,
            // already captured by RE_CHINESE. Use the regex captures
            // directly: group 1 is the number, group 2 is the unit.
            let re = Regex::new(
                r"^(\d+|[零一二三四五六七八九十百两]+)\s*(?:个)?\s*(小时|天|日|周|个月)(前|后)$",
            )
            .unwrap();
            let c = re.captures(t)?;
            let num_part = c.get(1)?.as_str();
            let unit = c.get(2)?.as_str();
            let tense = c.get(3)?.as_str();
            let n: i64 = if let Ok(arabic) = num_part.parse::<i64>() {
                arabic
            } else {
                chinese_numeral_to_i64(num_part)?
            };
            return Some(match (unit, tense) {
                ("小时", "前") => TimeExpression::HoursAgo(n),
                ("天" | "日", "前") => TimeExpression::DaysAgo(n),
                ("周", "前") => TimeExpression::DaysAgo(n * 7),
                ("个月", "前") => TimeExpression::DaysAgo(n * 30),
                ("小时", "后") => TimeExpression::HoursLater(n),
                ("天" | "日", "后") => TimeExpression::DaysLater(n),
                ("周", "后") => TimeExpression::WeeksLater(n),
                ("个月", "后") => TimeExpression::DaysLater(n * 30),
                _ => return None,
            });
        }
    }
}

/// Convert a simple Chinese numeral to i64. Supports:
///   零 (0) 一二三四五六七八九 十百 两
///   e.g. 零=0, 一=1, ..., 九=9, 十=10, 十一=11, ... 十九=19, 二十=20, ...
///   一百=100, 一百二十三=123, 两=2
fn chinese_numeral_to_i64(s: &str) -> Option<i64> {
    if s.is_empty() {
        return None;
    }
    let mut total: i64 = 0;
    let mut current: i64 = 0;
    for ch in s.chars() {
        match ch {
            '零' => {} // 零 is a placeholder
            '一' => current += 1,
            '二' | '两' => current += 2,
            '三' => current += 3,
            '四' => current += 4,
            '五' => current += 5,
            '六' => current += 6,
            '七' => current += 7,
            '八' => current += 8,
            '九' => current += 9,
            '十' => {
                if current == 0 {
                    current = 1; // 十 alone = 10
                }
                total += current * 10;
                current = 0;
            }
            '百' => {
                if current == 0 {
                    current = 1;
                }
                total += current * 100;
                current = 0;
            }
            _ => return None,
        }
    }
    Some(total + current)
}

fn parse_english(s: &str) -> Option<TimeExpression> {
    let t = s.trim().to_lowercase();
    match t.as_str() {
        "today" => Some(TimeExpression::Today),
        "yesterday" => Some(TimeExpression::Yesterday),
        "tomorrow" => Some(TimeExpression::Tomorrow),
        "day after tomorrow" => Some(TimeExpression::DayAfterTomorrow),
        "just now" => Some(TimeExpression::JustNow),
        "this week" => Some(TimeExpression::ThisWeek),
        "last week" => Some(TimeExpression::LastWeek),
        "next week" => Some(TimeExpression::NextWeek),
        "this month" => Some(TimeExpression::ThisMonth),
        "last month" => Some(TimeExpression::LastMonth),
        "next month" => Some(TimeExpression::NextMonth),
        _ => {
            let re = Regex::new(r"(\d+)\s+(hours?|days?|weeks?|months?)\s+(ago|later)").unwrap();
            if let Some(c) = re.captures(&t) {
                let n: i64 = c.get(1)?.as_str().parse().ok()?;
                let unit = c.get(2)?.as_str();
                let tense = c.get(3)?.as_str();
                return Some(match (unit, tense) {
                    ("hour" | "hours", "ago") => TimeExpression::HoursAgo(n),
                    ("day" | "days", "ago") => TimeExpression::DaysAgo(n),
                    ("week" | "weeks", "ago") => TimeExpression::DaysAgo(n * 7),
                    ("month" | "months", "ago") => TimeExpression::DaysAgo(n * 30),
                    ("hour" | "hours", "later") => TimeExpression::HoursLater(n),
                    ("day" | "days", "later") => TimeExpression::DaysLater(n),
                    ("week" | "weeks", "later") => TimeExpression::WeeksLater(n),
                    ("month" | "months", "later") => TimeExpression::DaysLater(n * 30),
                    _ => return None,
                });
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chinese_today() {
        let w = extract_time_window("今天我们做了什么").unwrap();
        let now = Utc::now();
        assert!(w.contains(now));
        assert!(w.contains(now - Duration::hours(23)));
    }

    #[test]
    fn chinese_yesterday() {
        let w = extract_time_window("昨天我们聊了 Rust").unwrap();
        let now = Utc::now();
        assert!(w.contains(now - Duration::hours(36)));
    }

    #[test]
    fn chinese_just_now() {
        let w = extract_time_window("刚才那条消息").unwrap();
        assert!(w.contains(Utc::now()));
    }

    #[test]
    fn chinese_n_days_ago() {
        let w = extract_time_window("3 天前看了什么").unwrap();
        let now = Utc::now();
        assert!(w.contains(now - Duration::days(3)));
    }

    #[test]
    fn chinese_n_hours_ago() {
        let w = extract_time_window("5 小时前").unwrap();
        let now = Utc::now();
        assert!(w.contains(now - Duration::hours(5)));
    }

    #[test]
    fn chinese_last_week() {
        let w = extract_time_window("上周的对话").unwrap();
        let now = Utc::now();
        // "last week" = [start_of_prev_week, start_of_this_week).
        // The window should NOT contain now (we're in "this week", not "last week").
        assert!(!w.contains(now));
        // The window should be 7 days wide.
        let width = w.end.unwrap() - w.start;
        assert_eq!(width, Duration::days(7));
    }

    #[test]
    fn english_today() {
        let w = extract_time_window("what did we do today").unwrap();
        assert!(w.contains(Utc::now()));
    }

    #[test]
    fn english_yesterday() {
        let w = extract_time_window("yesterday's session").unwrap();
        let now = Utc::now();
        assert!(w.contains(now - Duration::hours(36)));
    }

    #[test]
    fn english_last_week() {
        let w = extract_time_window("last week's project").unwrap();
        let now = Utc::now();
        // "last week" = [start_of_prev_week, start_of_this_week).
        // The window should NOT contain now.
        assert!(!w.contains(now));
        // The window should be 7 days wide.
        let width = w.end.unwrap() - w.start;
        assert_eq!(width, Duration::days(7));
    }

    #[test]
    fn english_n_days_ago() {
        let w = extract_time_window("3 days ago we met").unwrap();
        let now = Utc::now();
        assert!(w.contains(now - Duration::days(3)));
    }

    #[test]
    fn no_time_expr_returns_none() {
        assert!(extract_time_window("hello world").is_none());
        assert!(extract_time_window("今天聊点啥").is_some()); // sanity
        assert!(extract_time_window("").is_none());
    }

    // ---- Phase 18 (P0-4): forward-tense 时间词 ----

    #[test]
    fn chinese_tomorrow() {
        let w = extract_time_window("明天的计划是什么").unwrap();
        let now = Utc::now();
        // Forward-tense window contains now (start) and a future time.
        assert!(w.contains(now));
        assert!(w.end.is_some());
        let end = w.end.unwrap();
        assert!(end > now + Duration::hours(23));
        assert!(end < now + Duration::hours(49));
    }

    #[test]
    fn chinese_day_after_tomorrow() {
        let w = extract_time_window("后天开会").unwrap();
        let now = Utc::now();
        assert!(w.contains(now));
        assert!(w.end.unwrap() > now + Duration::hours(48));
    }

    #[test]
    fn chinese_next_week() {
        let w = extract_time_window("下周要交付什么").unwrap();
        let now = Utc::now();
        assert!(w.contains(now));
        let end = w.end.unwrap();
        // Next week spans from now to start_of_week(now) + 14d.
        let start_of_this_week = start_of_week(now);
        assert!(end >= start_of_this_week + Duration::days(13));
    }

    #[test]
    fn chinese_n_hours_later() {
        let w = extract_time_window("3 小时后提醒我").unwrap();
        let now = Utc::now();
        assert!(w.contains(now));
        assert!(w.end.unwrap() > now + Duration::hours(3));
    }

    #[test]
    fn chinese_n_days_later() {
        let w = extract_time_window("5 天后").unwrap();
        let now = Utc::now();
        assert!(w.contains(now));
        assert!(w.end.unwrap() > now + Duration::days(5));
    }

    #[test]
    fn chinese_n_weeks_later() {
        let w = extract_time_window("2 周后看看").unwrap();
        let now = Utc::now();
        assert!(w.contains(now));
        assert!(w.end.unwrap() > now + Duration::days(14));
    }

    #[test]
    fn english_tomorrow() {
        let w = extract_time_window("tomorrow's meeting").unwrap();
        let now = Utc::now();
        assert!(w.contains(now));
        assert!(w.end.unwrap() > now + Duration::hours(23));
    }

    #[test]
    fn english_next_week() {
        let w = extract_time_window("next week release").unwrap();
        let now = Utc::now();
        assert!(w.contains(now));
        let end = w.end.unwrap();
        let start_of_this_week = start_of_week(now);
        assert!(end >= start_of_this_week + Duration::days(13));
    }

    #[test]
    fn english_n_days_later() {
        let w = extract_time_window("3 days later reminder").unwrap();
        let now = Utc::now();
        assert!(w.contains(now));
        assert!(w.end.unwrap() > now + Duration::days(3));
    }

    #[test]
    fn english_n_hours_later() {
        let w = extract_time_window("5 hours later check").unwrap();
        let now = Utc::now();
        assert!(w.contains(now));
        assert!(w.end.unwrap() > now + Duration::hours(5));
    }

    #[test]
    fn chinese_numerals_days_ago() {
        let w = extract_time_window("三天前").unwrap();
        let now = Utc::now();
        assert!(w.contains(now - Duration::days(3)));
    }

    #[test]
    fn chinese_numerals_hours_ago() {
        let w = extract_time_window("五个小时前").unwrap();
        let now = Utc::now();
        assert!(w.contains(now - Duration::hours(5)));
    }

    #[test]
    fn chinese_numerals_weeks_ago() {
        let w = extract_time_window("两周前").unwrap();
        let now = Utc::now();
        assert!(w.contains(now - Duration::days(14)));
    }

    #[test]
    fn chinese_numerals_complex() {
        // 二十天前 = 20 days ago
        let w = extract_time_window("二十天前").unwrap();
        let now = Utc::now();
        assert!(w.contains(now - Duration::days(20)));
    }

    #[test]
    fn chinese_numerals_with_context() {
        let w = extract_time_window("我们三天前聊过 Rust").unwrap();
        let now = Utc::now();
        assert!(w.contains(now - Duration::days(3)));
    }

    #[test]
    fn time_window_contains() {
        let w = TimeWindow::last_hours(24);
        let now = Utc::now();
        assert!(w.contains(now));
        assert!(w.contains(now - Duration::hours(12)));
        assert!(!w.contains(now - Duration::days(2)));
    }
}
