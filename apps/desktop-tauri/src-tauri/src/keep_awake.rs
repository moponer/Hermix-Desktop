//! 运行会话时保持电脑唤醒（自动化页的同名开关）。
//!
//! Windows 的 `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)`
//! 是**按线程**生效的：Tauri 命令处理线程不固定，直接在命令里调会把状态
//! 撒在不同线程上。所以把调用收拢到一条常驻线程，on/off 请求经 channel
//! 送到那里顺序执行；进程退出线程即亡，系统自动恢复休眠策略。

use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;

use windows::Win32::System::Power::{
    SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED,
};

fn worker() -> &'static Sender<bool> {
    static WORKER: OnceLock<Sender<bool>> = OnceLock::new();
    WORKER.get_or_init(|| {
        let (tx, rx) = channel::<bool>();
        let spawned = std::thread::Builder::new()
            .name("keep-awake".into())
            .spawn(move || {
                for on in rx {
                    // ES_CONTINUOUS 单独发 = 撤销要求、恢复默认休眠策略。
                    let flags = if on {
                        ES_CONTINUOUS | ES_SYSTEM_REQUIRED
                    } else {
                        ES_CONTINUOUS
                    };
                    unsafe { SetThreadExecutionState(flags) };
                }
            });
        match spawned {
            Ok(_) => tx,
            // 线程起不来极端罕见；channel 随 send 失败上报，不 panic。
            Err(_) => {
                let (tx, _rx) = channel::<bool>();
                tx
            }
        }
    })
}

/// `on = true`：会话流式运行期间阻止系统睡眠；`false`：恢复默认策略。
#[tauri::command]
pub fn set_keep_awake(on: bool) -> Result<(), String> {
    worker()
        .send(on)
        .map_err(|e| format!("keep-awake 线程已退出：{e}"))
}
