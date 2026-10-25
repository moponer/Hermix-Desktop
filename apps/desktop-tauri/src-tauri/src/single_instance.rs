//! 单实例守护（2026-10-25）。
//!
//! 第二个进程启动时拿不到命名互斥锁 → 打开命名事件 SetEvent 通知已有
//! 实例弹主窗，然后本进程立即退出（在 Builder / sidecar / 全局热键
//! 之前，双开引发的服务端口与热键抢占从根上消失）。
//!
//! 不引 tauri-plugin-single-instance：`windows` crate 已在依赖里且所需
//! feature 全开，40 行原生实现足够（六步决策阶梯第 3 步「原生平台」）。
//!
//! 命名对象用 `Local\` 前缀 = 每登录会话一份：同一会话内只允许一个
//! 实例；不同会话/用户的桌面互不可见窗口，单实例锁到 Global 反而会把
//! 别人会话里的弹窗请求路由过来。
//!
//! `release()` 必须在 `app.restart()` 前调用：Tauri 的重启是「先 spawn
//! 新进程再退出本进程」，不提前放锁，新进程会把自己误判成第二实例直接
//! 退出（表现为点重启后应用整个消失）。

use std::sync::Mutex;
use tauri::Manager;
use windows::core::w;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, WAIT_OBJECT_0, ERROR_ALREADY_EXISTS, HANDLE,
};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, OpenEventW, SetEvent, WaitForSingleObject,
    EVENT_MODIFY_STATE, INFINITE,
};

const MUTEX_NAME: windows::core::PCWSTR = w!("Local\\HermixDesktop.SingleInstance");
const SHOW_EVENT_NAME: windows::core::PCWSTR = w!("Local\\HermixDesktop.ShowWindow");

// HANDLE 是裸指针不实现 Send，静态槽里存裸 isize，用时再包回 HANDLE。
static INSTANCE_MUTEX: Mutex<Option<isize>> = Mutex::new(None);
static SHOW_EVENT: Mutex<Option<isize>> = Mutex::new(None);

/// 尝试成为唯一实例。返回 true = 本进程持有锁，照常启动；
/// false = 已有实例在跑（已通知它弹主窗），调用方应立即退出。
///
/// 互斥锁创建失败时放行单开（宁可极端情况下双开，不能把单开也挡死）。
pub fn acquire_or_notify() -> bool {
    unsafe {
        let mutex = match CreateMutexW(None, false, MUTEX_NAME) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("[single-instance] 创建互斥锁失败（放行启动）: {e}");
                return true;
            }
        };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            // 老版本实例在跑但没有 ShowWindow 事件（升级前启动的）：通知
            // 不了就静默退出，行为退化为"纯单实例、不弹窗"。
            if let Ok(ev) = OpenEventW(EVENT_MODIFY_STATE, false, SHOW_EVENT_NAME) {
                let _ = SetEvent(ev);
                let _ = CloseHandle(ev);
            }
            return false;
        }
        // 两个句柄都留到进程退出（或 restart 前 release），OS 兜底回收。
        *INSTANCE_MUTEX.lock().unwrap() = Some(mutex.0 as isize);
        match CreateEventW(None, false, false, SHOW_EVENT_NAME) {
            Ok(ev) => *SHOW_EVENT.lock().unwrap() = Some(ev.0 as isize),
            Err(e) => eprintln!("[single-instance] 创建事件失败（双开将不弹窗）: {e}"),
        }
        true
    }
}

/// setup 里调用：监听「弹主窗」事件。主窗可能被关到托盘隐藏，所以
/// show + unminimize + focus 三连，幂等。
pub fn spawn_show_listener(handle: tauri::AppHandle) {
    let Some(ev) = *SHOW_EVENT.lock().unwrap() else { return };
    std::thread::spawn(move || unsafe {
        let ev = HANDLE(ev as _);
        loop {
            // 非 WAIT_OBJECT_0 = 句柄被 release() 关闭（重启流程）或出错。
            if WaitForSingleObject(ev, INFINITE) != WAIT_OBJECT_0 {
                break;
            }
            if let Some(win) = handle.get_webview_window("main") {
                let _ = win.unminimize();
                let _ = win.show();
                let _ = win.set_focus();
            }
        }
    });
}

/// 主动释放单实例锁与事件。仅 `commands::restart_app` 在 `app.restart()`
/// 前调用，其余路径靠进程退出由 OS 回收。
pub fn release() {
    unsafe {
        if let Some(h) = INSTANCE_MUTEX.lock().unwrap().take() {
            let _ = CloseHandle(HANDLE(h as _));
        }
        if let Some(h) = SHOW_EVENT.lock().unwrap().take() {
            let _ = CloseHandle(HANDLE(h as _));
        }
    }
}
