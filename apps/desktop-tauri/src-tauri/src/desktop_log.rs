//! 桌面端 stderr 落盘（2026-10-27）。
//!
//! `windows_subsystem = "windows"` 的进程没有控制台，`GetStdHandle(STD_ERROR_HANDLE)`
//! 是 NULL —— 于是**所有 `eprintln!` 都被丢进虚空**，而它们恰好只出现在失败
//! 路径上（「恢复托盘失败」「sidecar 启动失败」「单实例被挡」）。排查"托盘里
//! 没有图标但进程还在"时，日志为零，只能靠猜。
//!
//! 修法用原生能力一次收口：把 `STD_ERROR_HANDLE` 指到
//! `<data>/logs/desktop.log`。Rust 的 `std::io::stderr()` 每次写都走
//! `GetStdHandle`，所以既有 87 处 `eprintln!` 无需逐条替换。
//!
//! 不做日志轮转：本文件只承载桌面外壳的诊断行（不含 agent/tool 日志，
//! 那些走 `hermix-telemetry` 的 `server.log` / `tools.log`），日常量级在
//! KB 级；真出问题时更需要"上一次到底为什么失败"，而不是省磁盘。

use std::io::Write;

/// 路径与 `commands::log_dir` 同一解析链（`HERMIX_HOME` → 便携目录 → 安装根）。
fn log_path() -> Option<std::path::PathBuf> {
    let dir = crate::commands::resolve_data_dir().join("logs");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("desktop.log"))
}

/// 进程启动最早期调用一次（早于单实例检查与 sidecar 拉起）。
pub fn init() {
    let Some(path) = log_path() else {
        return;
    };
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    let _ = writeln!(
        file,
        "\n===== desktop start pid={} unix={} =====",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    );
    let _ = file.flush();

    #[cfg(windows)]
    unsafe {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE};

        let handle = HANDLE(file.as_raw_handle() as _);
        let _ = SetStdHandle(STD_ERROR_HANDLE, handle);
        // 句柄必须活到进程结束：`file` 在函数末尾 drop 会关掉它，
        // 之后每条 eprintln 写入都会失败（表现为"日志到此为止"）。
        std::mem::forget(file);
    }
}
