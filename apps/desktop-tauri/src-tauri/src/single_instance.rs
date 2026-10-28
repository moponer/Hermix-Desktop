//! 单实例守护（2026-10-25）。
//!
//! 第二个进程启动时拿不到命名互斥锁 → 打开命名事件 SetEvent 通知已有
//! 实例弹主窗，然后本进程立即退出（在 Builder / sidecar / 全局热键
//! 之前，双开引发的服务抢占与热键冲突从根上消失）。
//!
//! 不引 tauri-plugin-single-instance：`windows` crate 已在依赖里且所需
//! feature 全开，40 行原生实现足够（六步决策阶梯第 3 步「原生平台」）。
//!
//! 命名对象用 `Local\` 前缀 = 每登录会话一份：同一会话内只允许一个
//! 实例；不同会话/用户的桌面互不可见窗口，单实例锁到 Global 反而会把
//! 别人会话里的弹窗请求路由过来。
//!
//! **锁按 exe 绝对路径打键**（2026-10-27）：此前全局一个名字，导致
//! 「安装版关到托盘后仍持锁 → `cargo run` 的 dev 进程拿到
//! `ERROR_ALREADY_EXISTS` 后静默 `return`」，控制台只看到 `Running …`
//! 就结束，看起来像"dev 启动不了"。dev（`target\debug\`）与安装版
//! （`D:\…\Hermix Desktop\`）本来就是两个独立部署，理应并存；单实例
//! 要防的是**同一个可执行文件**被双开。
//!
//! `release()` 必须在 `app.restart()` 前调用：Tauri 的重启是「先 spawn
//! 新进程再退出本进程」，不提前放锁，新进程会把自己误判成第二实例直接
//! 退出（表现为点重启后应用整个消失）。

use std::sync::Mutex;
use tauri::Manager;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, WAIT_OBJECT_0, ERROR_ALREADY_EXISTS, HANDLE,
};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, OpenEventW, SetEvent, WaitForSingleObject,
    EVENT_MODIFY_STATE, INFINITE,
};

// HANDLE 是裸指针不实现 Send，静态槽里存裸 isize，用时再包回 HANDLE。
static INSTANCE_MUTEX: Mutex<Option<isize>> = Mutex::new(None);
static SHOW_EVENT: Mutex<Option<isize>> = Mutex::new(None);

/// FNV-1a 64：跨进程、跨 Rust 版本稳定（`DefaultHasher` 只保证同一进程内
/// 一致，std 升级换算法会让新旧实例认不出彼此）。
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 本 exe 专属的（互斥锁名, 唤起事件名），已转成 NUL 结尾的宽字符。
fn object_names() -> (Vec<u16>, Vec<u16>) {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_lowercase())
        .unwrap_or_else(|_| "hermix-desktop".to_string());
    let key = format!("{:016x}", fnv1a(exe.as_bytes()));
    let wide = |s: String| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    (
        wide(format!("Local\\HermixDesktop.SingleInstance.{key}")),
        wide(format!("Local\\HermixDesktop.ShowWindow.{key}")),
    )
}

/// 尝试成为唯一实例。返回 true = 本进程持有锁，照常启动；
/// false = 同一 exe 已有实例在跑（已通知它弹主窗），调用方应立即退出。
///
/// 互斥锁创建失败时放行单开（宁可极端情况下双开，不能把单开也挡死）。
pub fn acquire_or_notify() -> bool {
    let (mutex_name, event_name) = object_names();
    unsafe {
        let mutex = match CreateMutexW(None, false, PCWSTR(mutex_name.as_ptr())) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("[single-instance] 创建互斥锁失败（放行启动）: {e}");
                return true;
            }
        };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            // 同一程序已在运行：请它把主窗亮出来（关到托盘时窗口是隐藏的），
            // 本进程退出。**必须打印**——dev 控制台里"只看到 Running 就结束"
            // 曾被误判成启动失败（2026-10-27）。
            match OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(event_name.as_ptr())) {
                Ok(ev) => {
                    let _ = SetEvent(ev);
                    let _ = CloseHandle(ev);
                    eprintln!(
                        "[single-instance] 同一程序已有实例在运行，已唤起它的主窗口，本进程退出。\
                         要同时跑 dev 与安装版，请让两者使用不同的 exe 路径（本锁按 exe 路径区分）。"
                    );
                }
                Err(e) => {
                    // 老版本实例没有唤起事件（升级前启动的）：通知不了就静默
                    // 退出，行为退化为"纯单实例、不弹窗"，但至少留下原因。
                    eprintln!(
                        "[single-instance] 同一程序已有实例在运行（且不支持唤起事件，{e}），本进程退出。"
                    );
                }
            }
            let _ = CloseHandle(mutex);
            return false;
        }
        // 两个句柄都留到进程退出（或 restart 前 release），OS 兜底回收。
        *INSTANCE_MUTEX.lock().unwrap() = Some(mutex.0 as isize);
        match CreateEventW(None, false, false, PCWSTR(event_name.as_ptr())) {
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
