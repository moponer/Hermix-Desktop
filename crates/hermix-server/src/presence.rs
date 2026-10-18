//! 「有没有人看着」——进程级在场信号。
//!
//! 只回答一个问题：**最近有没有已鉴权的客户端在跟我们说话**。
//!
//! ## 为什么审批超时要问这个
//!
//! 审批是一种消息（`approval.request`），它的超时本意是「防止 agent 无限等一个
//! 不会来的回答」。但只要没有任何客户端在线，这个回答**在物理上不可能到达**——
//! 倒计时照常流逝，等价于保证失败：人在地铁上、手机被系统杀掉后台，回来只看
//! 到一条「已超时自动拒绝」，那一轮任务白跑。
//!
//! 所以超时应该按**可送达时间**计费：有人在（能看见、能答复）才计时，无人时暂停，
//! 外加一个绝对上限防止真挂死。
//!
//! ## 为什么不放进 `ws.rs`
//!
//! 判定只需要一个「最近有过鉴权活动」的时间戳，而每个 HTTP 请求都会经过
//! `security::request_guard`、每个手机帧都会经过 `mobile_ws::verify_frame_auth`。
//! 在这两处各记一笔就够了，不必去读 WS 的连接表（那属于传输层内部状态）。
//!
//! ## 「未知」按「有人」算
//!
//! 进程刚起来、或者纯单测环境里没有任何活动，此时无从判断。**未知一律当作有人**，
//! 也就是保持旧的固定超时行为——宁可让测试与无头场景保持确定性，也不要在
//! 「不知道有没有人」时把任务无限挂住。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// 判定窗口：这么久之内有过鉴权活动就算「有人在」。
///
/// 15 秒的依据：手机审批页按秒轮询、桌面端 WS 有心跳，正常在线时活动密度远高于此；
/// 而人离开/应用被杀之后，15 秒内就会掉出窗口，倒计时随即暂停。
pub const PRESENCE_WINDOW: Duration = Duration::from_secs(15);

fn base() -> Instant {
    static BASE: OnceLock<Instant> = OnceLock::new();
    *BASE.get_or_init(Instant::now)
}

/// 最近一次鉴权活动的时刻（相对 `base` 的毫秒数）。0 = 还没有任何活动。
static LAST_SEEN_MS: AtomicU64 = AtomicU64::new(0);

/// 记一笔「刚有已鉴权的客户端活动」。每个请求调一次，代价是一个原子写。
pub fn touch() {
    LAST_SEEN_MS.store(base().elapsed().as_millis() as u64, Ordering::Relaxed);
}

/// 纯判定：`last_ms == 0`（没有任何活动记录）算在场。
///
/// 抽成纯函数是为了**可测且不受并发影响**：时间戳是进程级全局，多个测试并行
/// 各自 `touch()` 会互相干扰，把负向断言写成「全局当前值」必然偶发失败。
fn seen_at(last_ms: u64, now_ms: u64, window: Duration) -> bool {
    if last_ms == 0 {
        return true;
    }
    now_ms.saturating_sub(last_ms) <= window.as_millis() as u64
}

/// 最近 `window` 内是否有人活动过。
///
/// **没有任何活动记录时返回 `true`**（见模块注释：未知按有人算）。
pub fn seen_within(window: Duration) -> bool {
    let last = LAST_SEEN_MS.load(Ordering::Relaxed);
    seen_at(last, base().elapsed().as_millis() as u64, window)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 没人活动过时按在场算() {
        assert!(seen_at(0, 12_345, Duration::ZERO), "未知 → 保持旧的固定超时语义");
    }

    #[test]
    fn 窗口内算在场_窗口外不算() {
        assert!(seen_at(1_000, 1_100, Duration::from_millis(200)));
        assert!(!seen_at(1_000, 1_500, Duration::from_millis(200)));
    }

    #[test]
    fn 零窗口只在同一毫秒内算在场() {
        assert!(seen_at(1_000, 1_000, Duration::ZERO));
        assert!(!seen_at(1_000, 1_001, Duration::ZERO));
    }

    /// 时间戳大于当前时刻（时钟回拨 / 读数错乱）不该 panic 也不该当成过期。
    #[test]
    fn 时间戳超前时按在场算() {
        assert!(seen_at(2_000, 1_000, Duration::from_millis(1)));
    }

    #[test]
    fn touch_之后窗口内算在场() {
        touch();
        assert!(seen_within(PRESENCE_WINDOW));
    }
}
