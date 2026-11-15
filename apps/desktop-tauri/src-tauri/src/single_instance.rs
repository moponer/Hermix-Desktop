//! 单实例守护（2026-10-25）。
//!
//! 第二个进程启动时拿不到命名互斥锁 → 打开命名事件 SetEvent 通知已有
//! 实例弹主窗，然后本进程退出（在 Builder / sidecar / 全局热键
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
//! **唤起失败要能自愈**（2026-10-30）：老实例可能是个「没有主窗的僵尸」
//! （主窗已销毁、进程还活着、锁还在它手里）。此时 SetEvent 过去它什么也
//! 弹不出来，而新版会在监听线程里发现主窗不存在 → 自己收尾退出。为了让
//! 用户只点一次图标就能起来，本进程在通知之后**继续轮询抢锁最多 6s**
//! （退出流程要先跑完 sidecar 的 5s 宽限），抢到就照常启动、抢不到才退出。
//!
//! `release()` 必须在 `app.restart()` 前调用：Tauri 的重启是「先 spawn
//! 新进程再退出本进程」，不提前放锁，新进程会把自己误判成第二实例直接
//! 退出（表现为点重启后应用整个消失）。

use std::sync::Mutex;
use std::time::Duration;
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

/// 通知已有实例之后，等它放锁的轮询预算：100ms × 60 = 6s。
///
/// 正常实例不用等（它弹窗后继续跑），最坏只是本进程多活 6s 再退；
/// 僵尸实例要走完 `cleanup()` 的 5s sidecar 宽限才真正退出，所以预算
/// 必须大于 5s，否则「点一次图标自愈」会退化成「点两次」。
const ACQUIRE_RETRY_ROUNDS: u32 = 60;

/// 一次抢锁尝试的结果。
enum Attempt {
    /// 本进程拿到锁（句柄已存进静态槽）。
    Acquired,
    /// 同一 exe 已有实例（句柄已关，什么都没改）。
    Exists,
    /// 命名对象创建失败。放行单开：宁可极端情况下双开，不能把单开也挡死。
    Unavailable,
}

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

/// 单次抢锁。成功时把互斥锁与唤起事件两个句柄都留到进程退出（或 restart
/// 前 `release()`），OS 兜底回收。
fn try_acquire(names: &(Vec<u16>, Vec<u16>)) -> Attempt {
    unsafe {
        let mutex = match CreateMutexW(None, false, PCWSTR(names.0.as_ptr())) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("[single-instance] 创建互斥锁失败（放行启动）: {e}");
                return Attempt::Unavailable;
            }
        };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            // 这一代的锁不归我们，句柄必须关掉，否则对象永远不消失。
            let _ = CloseHandle(mutex);
            return Attempt::Exists;
        }
        *INSTANCE_MUTEX.lock().unwrap() = Some(mutex.0 as isize);
        match CreateEventW(None, false, false, PCWSTR(names.1.as_ptr())) {
            Ok(ev) => *SHOW_EVENT.lock().unwrap() = Some(ev.0 as isize),
            Err(e) => eprintln!("[single-instance] 创建事件失败（双开将不弹窗）: {e}"),
        }
        Attempt::Acquired
    }
}

/// 请已有实例把主窗亮出来（关到托盘时窗口是隐藏的）。返回它是否存在
/// 唤起事件（老版本实例没有 → 通知不了）。
fn notify_existing(event_name: &[u16]) -> bool {
    unsafe {
        match OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(event_name.as_ptr())) {
            Ok(ev) => {
                let _ = SetEvent(ev);
                let _ = CloseHandle(ev);
                true
            }
            Err(e) => {
                // 老版本实例没有唤起事件（升级前启动的）：通知不了就静默
                // 退出，行为退化为"纯单实例、不弹窗"，但至少留下原因。
                eprintln!("[single-instance] 已有实例不支持唤起事件（{e}）");
                false
            }
        }
    }
}

/// 尝试成为唯一实例。返回 true = 本进程持有锁，照常启动；
/// false = 同一 exe 已有实例在跑（已通知它弹主窗），调用方应立即退出。
pub fn acquire_or_notify() -> bool {
    let names = object_names();
    match try_acquire(&names) {
        Attempt::Acquired => return true,
        Attempt::Unavailable => return true,
        Attempt::Exists => {}
    }

    let notified = notify_existing(&names.1);
    if notified {
        eprintln!("[single-instance] 已请求已有实例显示主窗口，等待其让出单实例锁…");
    }

    // 已有实例可能是僵尸（主窗已销毁／进程卡住）：它收到唤起事件后会自己
    // 收尾退出，锁随之释放，这里接管启动。**必须打印每次接管**，否则
    // 「点了一次图标却没反应」的现场只剩一行沉默退出。
    for _ in 0..ACQUIRE_RETRY_ROUNDS {
        std::thread::sleep(Duration::from_millis(100));
        match try_acquire(&names) {
            Attempt::Acquired => {
                eprintln!("[single-instance] 已有实例已退出，转由本进程启动。");
                return true;
            }
            Attempt::Unavailable => return true,
            Attempt::Exists => {}
        }
    }

    eprintln!(
        "[single-instance] 同一程序已有实例在运行（已唤起其主窗口），本进程退出。\
         若界面没有出现，说明那个实例已失去主窗口，请先在任务管理器结束它。\
         要同时跑 dev 与安装版，请让两者使用不同的 exe 路径（本锁按 exe 路径区分）。"
    );
    false
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
            match handle.get_webview_window("main") {
                Some(win) => {
                    let _ = win.unminimize();
                    let _ = win.show();
                    let _ = win.set_focus();
                }
                // 主窗不存在 = 本进程是个僵尸（没有可见界面还活着、还占着
                // 单实例锁与新实例抢不到的资源）。退出是唯一诚实的反应：
                // 新进程正在等这把锁（见 acquire_or_notify）。
                None => {
                    eprintln!(
                        "[single-instance] 收到唤起请求，但主窗口已不存在 —— \
                         本进程是失去界面的残留实例，收尾退出以便新实例启动"
                    );
                    crate::shutdown::cleanup_and_exit(&handle, "single-instance-no-main-window");
                    break;
                }
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
