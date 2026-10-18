//! 「有没有人看着」——进程级在场信号。
//!
//! 只回答一个问题：**现在有没有一个能看见、能答复请求的客户端在**。
//!
//! ## 为什么审批超时要问这个
//!
//! 审批是一种消息（`approval.request`），它的超时本意是「防止 agent 无限等一个
//! 不会来的回答」。但只要没有任何客户端在线，这个回答**在物理上不可能到达**——
//! 倒计时照常流逝，等价于保证失败：人在地铁上、手机被系统杀掉后台，回来只看
//! 到一条「已超时自动拒绝」，那一轮任务白跑。
//!
//! 所以超时按**可送达时间**计费：有人在才计时。
//!
//! ## 两个信号，不是一个
//!
//! 第一版把「最近有过鉴权 HTTP 请求」当成在场，结果被自己的桌面端打败：
//! `App.tsx` 为了防休眠每 10 秒轮询一次 `activeAutomationRuns`，窗口最小化也照轮。
//! 于是只要有桌面进程在，在场永远为真、暂停永不生效——**等于没做**。
//!
//! 现在拆成两条：
//!
//! - **设备活动**（[`note_device_activity`]）：手机经 relay 发的帧、以及手机走
//!   代理的已鉴权请求。手机后台被杀后自然停止，是可信的「手机在手上」信号。
//! - **桌面注意力**（[`set_attention`]）：桌面端显式上报窗口是否在前台，带心跳。
//!   它自己的后台轮询**不参与**判断——轮询只证明进程活着，不证明有人在看。
//!
//! ## 「未知」按「有人」算
//!
//! 进程刚起来、或纯单测环境里两条信号都没出现过，此时无从判断。**未知一律当作
//! 有人**，也就是保持旧的固定超时行为——宁可让测试与无头场景保持确定性，也不要在
//! 「不知道有没有人」时把任务挂住。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// 设备活动的判定窗口。
///
/// 15 秒的依据：手机审批页按秒轮询、WS 有心跳，正常在线时活动密度远高于此；
/// 人离开/应用被杀之后，15 秒内就会掉出窗口，倒计时随即暂停。
pub const PRESENCE_WINDOW: Duration = Duration::from_secs(15);

/// 桌面注意力的有效期。前端每 30 秒心跳一次，这里给 3 倍容错——
/// 单次心跳丢了不该让审批瞬间变成「无人在场」。
pub const ATTENTION_TTL: Duration = Duration::from_secs(90);

fn base() -> Instant {
    static BASE: OnceLock<Instant> = OnceLock::new();
    *BASE.get_or_init(Instant::now)
}

fn now_ms() -> u64 {
    base().elapsed().as_millis() as u64
}

/// 最近一次手机活动（0 = 从未记录）。
static DEVICE_SEEN_MS: AtomicU64 = AtomicU64::new(0);
/// 桌面端最近一次上报「窗口在前台」（0 = 不在前台 / 从未上报）。
static ATTENTION_MS: AtomicU64 = AtomicU64::new(0);
/// 桌面端是否上报过注意力。上报过之后，判定就不再依赖「未知按有人」。
static ATTENTION_REPORTED: AtomicBool = AtomicBool::new(false);

/// 记一笔「手机在活动」。手机帧与手机走代理的请求都会调它，代价是一个原子写。
pub fn note_device_activity() {
    DEVICE_SEEN_MS.store(now_ms(), Ordering::Relaxed);
}

/// 桌面端上报窗口注意力。`true` 表示「人就在电脑前」，需要定期重报（见 [`ATTENTION_TTL`]）。
pub fn set_attention(active: bool) {
    ATTENTION_REPORTED.store(true, Ordering::Relaxed);
    ATTENTION_MS.store(if active { now_ms() } else { 0 }, Ordering::Relaxed);
}

/// 纯判定：`last_ms == 0` 表示从未记录过。
fn recent(last_ms: u64, now_ms: u64, window: Duration) -> bool {
    if last_ms == 0 {
        return false;
    }
    now_ms.saturating_sub(last_ms) <= window.as_millis() as u64
}

/// 组合两条信号。抽成纯函数是因为它们背后是进程级全局——
/// 并行测试各自记录会互相干扰，负向断言只能喂固定输入来测。
fn resolve(
    attention_reported: bool,
    attention_ms: u64,
    device_ms: u64,
    now_ms: u64,
    window: Duration,
) -> bool {
    let device = recent(device_ms, now_ms, window);
    let attention = recent(attention_ms, now_ms, ATTENTION_TTL);
    if attention_reported {
        return attention || device;
    }
    // 还没人上报过注意力：设备也没活动过就说明无从判断 —— 未知按有人算。
    device || device_ms == 0
}

/// 现在是否有人能看见并答复。
pub fn seen_within(window: Duration) -> bool {
    resolve(
        ATTENTION_REPORTED.load(Ordering::Relaxed),
        ATTENTION_MS.load(Ordering::Relaxed),
        DEVICE_SEEN_MS.load(Ordering::Relaxed),
        now_ms(),
        window,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 没人上报过注意力且设备无记录时按在场算() {
        assert!(
            resolve(false, 0, 0, 12_345, PRESENCE_WINDOW),
            "未知 → 保持旧的固定超时语义"
        );
    }

    #[test]
    fn 设备窗口内算在场_窗口外不算() {
        assert!(resolve(false, 0, 1_000, 1_100, Duration::from_millis(200)));
        assert!(!resolve(false, 0, 1_000, 1_500, Duration::from_millis(200)));
    }

    /// 桌面端明确说「我不在前台」时，若手机也没有活动就该判无人
    /// ——这正是「电脑开着但人走开了」。
    #[test]
    fn 上报过且都不在时判无人() {
        assert!(
            !resolve(true, 0, 0, 5_000, PRESENCE_WINDOW),
            "桌面说不在前台 + 手机无活动 → 无人"
        );
        // 两端任一在都算有人：手机刚说过话时，桌面失焦不影响判定。
        assert!(
            resolve(true, 0, 1_000, 5_000, PRESENCE_WINDOW),
            "手机有活动就是有人"
        );
    }

    /// 桌面在前台（注意力的 TTL 比设备窗口长）也算有人。
    #[test]
    fn 桌面在前台算有人() {
        let now = 200_000;
        assert!(resolve(true, now - 30_000, 0, now, PRESENCE_WINDOW));
        assert!(
            !resolve(true, now - 120_000, 0, now, PRESENCE_WINDOW),
            "心跳超 TTL 视为离开"
        );
    }

    /// 桌面在前台时，手机有活动同样算有人（两端任一在都行）。
    #[test]
    fn 两条信号取或() {
        let now = 100_000;
        assert!(resolve(true, 0, now - 1_000, now, PRESENCE_WINDOW));
    }

    /// 时间戳超前（时钟回拨 / 读数错乱）不该 panic，也不该当成过期。
    #[test]
    fn 时间戳超前时不崩溃() {
        let _ = resolve(true, 2_000, 2_000, 1_000, Duration::from_millis(1));
    }

    #[test]
    fn 记录接口可重入且不崩溃() {
        note_device_activity();
        set_attention(true);
        set_attention(false);
        // 全局状态受其它并行测试影响，这里只断言调用本身安全、可重入。
        let _ = seen_within(PRESENCE_WINDOW);
    }
}
