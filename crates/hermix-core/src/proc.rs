//! 子进程启动的统一约定：Windows 上不弹控制台窗口。
//!
//! 桌面程序自身是 GUI 子系统（无控制台）。此时 spawn 任何控制台程序，
//! Windows 都会给它**新开一扇控制台窗口**（黑窗）——对用户是纯噪音：
//! 所有需要展示的信息（命令输出、工具结果、错误）都已经在前端界面上
//! 呈现了。`CREATE_NO_WINDOW` 让子进程继承"无窗口"状态，输出照旧走管道。
//!
//! 顺带一提：npm 系（`npm`/`npx`/`node`）在 Windows 上只有 `.cmd` 批处理
//! shim，系统必须借 `cmd.exe` 执行 —— 不加这个标志，spawn 一个 MCP
//! server 就会同时闪出 `npm exec ...` 和 `cmd.exe` 两扇窗。
//!
//! 非 Windows 是 no-op（那里没有"控制台窗口"这个概念）。

use std::process::Command;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 让 `cmd` 启动时不带控制台窗口。链式可用：
/// `hide_console(&mut Command::new("docker").arg("ps"))`。
///
/// 需要合并自己已有的 `creation_flags`（比如 job object 的
/// `CREATE_SUSPENDED`）时用 [`hide_console_flags`] 取标志值。
pub fn hide_console(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(hide_console_flags());
    }
    #[cfg(not(windows))]
    let _ = cmd;
    cmd
}

/// `hide_console` 用的原始标志值（Windows），供需要与其他
/// `creation_flags` 合并的调用方使用。非 Windows 返回 0。
pub const fn hide_console_flags() -> u32 {
    #[cfg(windows)]
    {
        CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    {
        0
    }
}

/// tokio 版 [`hide_console`]。tokio 的 `Command` 自己在 Windows 上有
/// `creation_flags` 方法，不共用 std 的 trait 实现，所以这里单列一个。
pub fn hide_console_tokio(cmd: &mut tokio::process::Command) -> &mut tokio::process::Command {
    #[cfg(windows)]
    cmd.creation_flags(hide_console_flags());
    #[cfg(not(windows))]
    let _ = cmd;
    cmd
}

/// 已设好"不弹控制台"的 std Command 构造器。**新起子进程一律走这里**，
/// 不要直接 `Command::new` —— 直接构造就会漏标志、漏一次闪一扇黑窗。
///
/// ```ignore
/// let out = hermix_core::proc::cmd("docker").args(["ps"]).output()?;
/// ```
pub fn cmd(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut c = Command::new(program);
    hide_console(&mut c);
    c
}

/// tokio 版 [`cmd`]（`tokio::process::Command`）。
pub fn cmd_tokio(program: impl AsRef<std::ffi::OsStr>) -> tokio::process::Command {
    let mut c = tokio::process::Command::new(program);
    hide_console_tokio(&mut c);
    c
}
