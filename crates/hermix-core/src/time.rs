//! 北京时间（UTC+8）统一助手。
//!
//! 产品面向中国用户：**一切用户可见 / 模型可见的时间一律北京时间**，
//! 不依赖机器时区（机器 TZ 配错时 Local 会给出错误时间）。存储层
//! （DB 时间戳、用量分桶、鉴权签名）仍用 UTC——那是内部一致性约定，
//! 与本模块无关。
//!
//! 2026-11-14 起：系统提示词 "Conversation started"、会话日志 "Date:"、
//! 成员任务清单、存储分析报告、ACUI 卡片日期等展示点全部改走这里。

use chrono::{DateTime, FixedOffset, SecondsFormat, Utc};

/// 北京时区偏移（UTC+8，无夏令时）。
pub fn beijing() -> FixedOffset {
    FixedOffset::east_opt(8 * 3600).expect("UTC+8 恒为合法偏移")
}

/// 当前北京时间。
pub fn beijing_now() -> DateTime<FixedOffset> {
    Utc::now().with_timezone(&beijing())
}

/// 当前北京时间的 RFC3339 字符串（秒精度，`+08:00` 后缀），
/// 用于会话起始时间等需要"会话内恒定 + 跨端可解析"的场景。
pub fn beijing_rfc3339_secs() -> String {
    beijing_now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// 任意 Utc 时间转北京墙钟（用于展示已存储的 UTC 时间戳）。
pub fn utc_to_beijing(dt: &DateTime<Utc>) -> DateTime<FixedOffset> {
    dt.with_timezone(&beijing())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beijing_offset_is_exactly_east_8h() {
        let t = beijing_now();
        assert_eq!(t.offset().local_minus_utc(), 8 * 3600);
    }

    #[test]
    fn rfc3339_carries_beijing_suffix() {
        let s = beijing_rfc3339_secs();
        assert!(
            s.ends_with("+08:00"),
            "北京时间 RFC3339 必须带 +08:00 后缀，实际: {s}"
        );
        // 秒精度：无小数秒
        assert!(!s.contains('.'), "应为秒精度（无小数秒）: {s}");
    }

    #[test]
    fn utc_to_beijing_shifts_wall_clock_by_8h() {
        let utc = DateTime::parse_from_rfc3339("2026-11-14T02:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let b = utc_to_beijing(&utc);
        assert_eq!(b.format("%Y-%m-%d %H:%M:%S").to_string(), "2026-11-14 10:30:00");
        // 跨日边界：UTC 凌晨 → 北京已是当天上午
        let utc2 = DateTime::parse_from_rfc3339("2026-11-13T19:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            utc_to_beijing(&utc2).format("%Y-%m-%d").to_string(),
            "2026-11-14"
        );
    }
}
