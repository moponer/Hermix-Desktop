//! Hermes Viewer — 独立文件浏览器（决策笔记 2026-10-09-standalone-file-viewer）。
//!
//! 单实例：命名互斥体判定；第二实例把文件路径经 loopback TCP 投递给
//! 首实例后退出（零插件依赖——不引 tauri-plugin-single-instance）。
//!
//! 组织：命令在 commands.rs（lib.rs 根定义会与 generate_handler 冲突）。

pub mod commands;
pub mod filesearch;
pub mod image_ops;
pub mod paths;
pub mod pdf_ops;

// 被嵌进二进制的前端内容指纹（build.rs 生成，见 build.rs 头注）。
// dist 变了它就变 → crate 必然重编 → 不会出现「exe 里是旧 UI」。
include!(concat!(env!("OUT_DIR"), "/dist_fingerprint.rs"));

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use tauri::{Builder, Emitter, Manager};

/// 值得打开的目标：文件与目录都算。
/// 目录必须收——主程序「在查看器中打开」传目录是常规动作，
/// 只收文件的话目录会被静默丢弃，用户看到的是「点了没反应」。
fn is_openable(p: &Path) -> bool {
    p.is_file() || p.is_dir()
}

/// 用命令行参数里的文件路径初始化待开队列。
fn seed_pending(app: &tauri::AppHandle, args: &[String]) {
    let state = app.state::<commands::ViewerState>();
    let mut pending = state.pending.lock().unwrap();
    for a in args {
        let p = PathBuf::from(a);
        if is_openable(&p) {
            pending.push(p);
        }
    }
}

/// 首实例：监听 loopback，收第二实例投递的路径 → emit 给前端。
fn spawn_singleton_listener(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        let listener = match std::net::TcpListener::bind(("127.0.0.1", commands::SINGLETON_PORT)) {
            Ok(l) => l,
            Err(e) => {
                // 端口被占（非本族进程）：单实例投递不可用，但不致命——
                // 用户仍可手动拖文件进窗口。如实记日志。
                eprintln!("[viewer] singleton 端口监听失败（{e}）；拖放打开不受影响");
                return;
            }
        };
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = String::new();
            if stream.read_to_string(&mut buf).is_err() {
                continue;
            }
            let path = buf.trim().to_string();
            if path.is_empty() {
                continue;
            }
            let state = app.state::<commands::ViewerState>();
            state.pending.lock().unwrap().push(PathBuf::from(&path));
            let _ = app.emit("viewer-open-file", path);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::is_openable;
    use std::fs;

    #[test]
    fn openable_accepts_files_and_dirs_but_not_missing_paths() {
        let dir = std::env::temp_dir().join(format!("hermix-viewer-openable-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let file = dir.join("a.txt");
        fs::write(&file, b"x").expect("write");

        assert!(is_openable(&file), "文件应当可打开");
        assert!(is_openable(&dir), "目录也必须可打开（主程序会传目录进来）");
        assert!(!is_openable(&dir.join("missing.txt")), "不存在的路径不该进待开队列");

        let _ = fs::remove_dir_all(&dir);
    }
}

/// 入口（main.rs 调用；pub 因为 bin 目标跨 crate 引用它）。
pub fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        // GetLastError 在 Foundation（同 updater.rs 教训：跨模块声明依赖它）。
        use windows_sys::Win32::Foundation::GetLastError;
        use windows_sys::Win32::System::Threading::CreateMutexW;
        let wide: Vec<u16> = std::ffi::OsStr::new(commands::SINGLETON_MUTEX)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            CreateMutexW(std::ptr::null(), 0, wide.as_ptr());
            // ERROR_ALREADY_EXISTS (183)：已有实例 → 投递路径后退出。
            if GetLastError() == 183 {
                drop(args);
                // args 已被 move，重新取环境参数投递。
                let paths: Vec<String> = std::env::args().skip(1).collect();
                if let Ok(mut s) = std::net::TcpStream::connect((
                    "127.0.0.1",
                    commands::SINGLETON_PORT,
                )) {
                    let _ = s.write_all(paths.join("\n").as_bytes());
                }
                return;
            }
        }
    }

    // 非 Windows（仅编译通过；项目 Windows-only）直接走正常启动。
    #[cfg(windows)]
    let init_args = args;
    #[cfg(not(windows))]
    let init_args = args;

    Builder::default()
        .manage(commands::ViewerState::default())
        .invoke_handler(tauri::generate_handler![
            commands::viewer_read_file,
            commands::viewer_list_dir,
            commands::viewer_stat,
            commands::viewer_parent,
            commands::viewer_drives,
            commands::viewer_home_dir,
            commands::viewer_reveal_in_explorer,
            commands::viewer_search_dir,
            commands::viewer_open_with_system,
            commands::viewer_run_executable,
            filesearch::viewer_search_global,
            filesearch::viewer_filesearch_status,
            commands::viewer_take_pending,
            commands::ai_send,
            image_ops::image_info,
            image_ops::image_rasterize,
            image_ops::image_apply,
            image_ops::image_batch,
            image_ops::image_save_dialog,
            image_ops::image_open_dialog,
            image_ops::image_montage,
            image_ops::image_write_composite,
            pdf_ops::pdf_info,
            pdf_ops::pdf_ops,
            pdf_ops::pdf_replace_text,
            pdf_ops::images_to_pdf,
            pdf_ops::write_text,
        ])
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            seed_pending(app.handle(), &init_args);
            spawn_singleton_listener(app.handle().clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("viewer: failed to run tauri app");
}
