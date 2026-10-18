//! 网络变化感知。
//!
//! 只回答一个问题：**链路可能变了，别再干等退避**。
//!
//! ## 背景
//!
//! 切 Wi-Fi / 拔网线时 TCP 常常是**半开**的——对端没回 RST，包就那么丢了。
//! `relay_client` 只能靠「90 秒没任何入帧」判定死亡，断开后再叠上退避上限
//! 60 秒，最坏 150 秒才恢复。手机那头看到的就是一直转圈。
//!
//! 这里用 `NotifyAddrChange`（地址/路由变化）把这段压到秒级。
//!
//! ## 边界
//!
//! 平台 API 只出现在本模块，不外溢到 `relay_client` / `relay_transport`。
//! 非 Windows 编译成空实现——测试和将来的跨平台构建都不受影响，代价只是
//! 「没有主动信号，回到纯超时判定」。
//!
//! **不做休眠/唤醒**：`WM_POWERBROADCAST` 需要窗口与消息循环，
//! `RegisterSuspendResumeNotification` 要维护回调的 FFI 生命周期。留给下一批，
//! 且唤醒时网络栈重新绑定通常也会触发地址变化，能覆盖大半场景。

use std::sync::{Arc, OnceLock};

use tokio::sync::Notify;

/// 订阅网络变化与系统唤醒。进程内只起一次监听，重复调用共享同一个信号源。
///
/// 返回的 `Notify` 用法是 `notify.notified().await`——调用方（relay 重连循环）
/// 只需要「被打断」这一件事，不需要知道变化的具体内容。
pub fn subscribe() -> Arc<Notify> {
    static SIGNAL: OnceLock<Arc<Notify>> = OnceLock::new();
    SIGNAL
        .get_or_init(|| {
            let notify = Arc::new(Notify::new());
            spawn_watcher(notify.clone());
            spawn_power_watcher(notify.clone());
            notify
        })
        .clone()
}

/// 两次通知之间的最小间隔。
///
/// 防「通知立刻返回 → 立刻重连 → 又立刻通知」把 CPU 烧掉：地址变化在
/// DHCP 续租、虚拟网卡抖动时会连着来好几次，而重连本身也需要时间。
const MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

#[cfg(windows)]
fn spawn_watcher(notify: Arc<Notify>) {
    use windows::Win32::Foundation::{BOOL, CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::NetworkManagement::IpHelper::NotifyAddrChange;
    use windows::Win32::System::IO::OVERLAPPED;
    use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
    use windows::core::PCWSTR;

    let spawned = std::thread::Builder::new()
        .name("hermix-net-watch".into())
        .spawn(move || loop {
            // 每次重新建事件与 OVERLAPPED：一次通知只消费一次，
            // 复用同一个 OVERLAPPED 是未定义行为（微软文档明确要求）。
            let event = unsafe { CreateEventW(None, BOOL::from(true), BOOL::from(false), PCWSTR::null()) };
            let Ok(event) = event else {
                // 建不出事件就彻底退出：重试只会刷日志，而这里本来就只是
                // 一个「更快」的优化，退回纯超时判定不影响正确性。
                tracing::warn!("网络监听起不来（CreateEventW 失败），回退到超时判定");
                return;
            };
            let mut overlapped = OVERLAPPED {
                hEvent: event,
                ..Default::default()
            };
            let mut handle = HANDLE::default();
            // 返回 ERROR_IO_PENDING(997) 是正常语义：通知会在地址变化时投递。
            // 返回别的错误时事件永远不会 signal，线程就停在这儿——
            // 等价于「没有主动信号」，不影响正确性。
            unsafe {
                let _ = NotifyAddrChange(&mut handle, &mut overlapped);
            }
            let waited = unsafe { WaitForSingleObject(event, INFINITE) };
            unsafe {
                let _ = CloseHandle(event);
            }
            if waited == WAIT_OBJECT_0 {
                tracing::info!("检测到网络地址/路由变化，唤醒 relay 重连");
                notify.notify_waiters();
            }
            std::thread::sleep(MIN_INTERVAL);
        });

    if let Err(e) = spawned {
        tracing::warn!(error = %e, "网络监听线程起不来，回退到超时判定");
    }
}

#[cfg(not(windows))]
fn spawn_watcher(_notify: Arc<Notify>) {
    // 非 Windows：没有主动信号，重连完全依赖心跳超时判定。
    // 这里刻意不报错——「少一个优化」不是错误。
}

/// 休眠/唤醒监听。
///
/// 用 `RegisterSuspendResumeNotification` 的**回调**模式（`DEVICE_NOTIFY_CALLBACK`），
/// 不需要窗口和消息循环——这是它比 `WM_POWERBROADCAST` 省事的地方，也是这一批
/// 敢做的原因。
///
/// 注册参数与返回句柄都必须活到进程结束：句柄一释放，注册就解除。所以两者都
/// 泄漏成 `'static`——本函数只执行一次，泄漏量固定且极小。
#[cfg(windows)]
fn spawn_power_watcher(notify: Arc<Notify>) {
    use windows::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
    use windows::Win32::System::Power::{
        DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, RegisterSuspendResumeNotification,
    };
    use windows::Win32::UI::WindowsAndMessaging::DEVICE_NOTIFY_CALLBACK;

    unsafe extern "system" fn on_power(
        context: *const core::ffi::c_void,
        event: u32,
        _setting: *const core::ffi::c_void,
    ) -> u32 {
        if context.is_null() {
            return ERROR_SUCCESS.0;
        }
        // SAFETY: 指针来自下面的 `Box::into_raw`，且永不释放（进程级生命周期）。
        let notify = &*(context as *const Arc<Notify>);
        tracing::info!(event, "系统电源状态变化，唤醒 relay 重连");
        notify.notify_waiters();
        ERROR_SUCCESS.0
    }

    let context = Box::into_raw(Box::new(notify));
    let params = Box::into_raw(Box::new(DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
        Callback: Some(on_power),
        // 字段类型是 `*mut`；我们只读，但签名如此。
        Context: context as *mut core::ffi::c_void,
    }));
    let registered = unsafe {
        RegisterSuspendResumeNotification(
            HANDLE(params as *mut core::ffi::c_void),
            DEVICE_NOTIFY_CALLBACK,
        )
    };
    match registered {
        Ok(handle) => {
            std::mem::forget(handle);
            tracing::info!("已订阅系统休眠/唤醒通知");
        }
        Err(e) => tracing::warn!(error = %e, "订阅休眠/唤醒通知失败，回退到超时判定"),
    }
}

#[cfg(not(windows))]
fn spawn_power_watcher(_notify: Arc<Notify>) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// 冒烟：订阅要能在本机建起监听（CreateEventW + 线程），且进程内唯一。
    ///
    /// 不去证明「变化会真的触发」——那要改网卡配置，测试不该干这事。
    /// 这里只保证接线不 panic、不会每次调用都多起一个线程。
    #[test]
    fn 订阅是进程内单例() {
        let a = subscribe();
        let b = subscribe();
        assert!(Arc::ptr_eq(&a, &b), "重复订阅必须共享同一个信号源");
    }

    /// `notify_waiters` 只唤醒当前等待者、不留存 permit —— 正是重连循环要的
    /// 语义：刚连上就白重连一次比「漏一次唤醒」更糟。
    #[tokio::test]
    async fn 信号能唤醒等待者() {
        let signal = subscribe();
        let waiter = signal.clone();
        let task = tokio::spawn(async move {
            waiter.notified().await;
        });
        // 让 task 真的进到等待里，否则 notify 打在空气上。
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        signal.notify_waiters();
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .expect("等待者应被唤醒")
            .expect("任务不应 panic");
    }
}
