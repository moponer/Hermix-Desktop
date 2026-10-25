//! Hermes Viewer — 独立文件浏览器（决策笔记 2026-10-09-standalone-file-viewer）。
//!
//! 单实例：命名互斥体判定；第二实例把文件路径经 loopback TCP 投递给
//! 首实例后退出（零插件依赖——不引 tauri-plugin-single-instance）。
//!
//! 组织：命令在 commands.rs（lib.rs 根定义会与 generate_handler 冲突）。

pub mod ai;
pub mod artifacts;
pub mod browser;
pub mod cdp;
pub mod cleanup;
pub mod cleanup_ai;
pub mod clipboard;
pub mod commands;
pub mod docx_edit;
pub mod drive_scan;
pub mod filesearch;
pub mod gui;
pub mod image_ops;
pub mod mcp_http;
pub mod mcp_server;
pub mod paths;
pub mod pdf_font;
pub mod pdf_ocr;

/// 扫描件 OCR 的 Tauri 命令（纯逻辑在 pdf_ocr.rs 里）。
pub mod pdf_ops;
pub mod pin_window;
pub mod procclean;
pub mod markitdown;
pub mod settings;
pub mod sysmon;
pub mod viewer_control;
pub mod webview_cdp;
pub mod webview_host;

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
                // 空投递 = 「打开文件管理器」（主程序的标题栏入口）：没有路径
                // 可开，但要把已开的窗口叫到前面（最小化 / 藏着的都拉回来），
                // 并让前端切到资源管理器模式——否则用户在主程序点了按钮，
                // 这边什么也不会发生。
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.unminimize();
                    let _ = w.show();
                    let _ = w.set_focus();
                }
                let _ = app.emit("viewer-open-explorer", ());
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

/// 无边框窗口会**连带丢掉「拖边缘缩放」**：tao 在 decorations=false 时
/// 去掉 WS_CAPTION 与 WS_THICKFRAME（tao/src/platform_impl/windows/
/// window_state.rs::to_adjusted_window_styles），窗口就变成固定大小——
/// 自绘标题栏换来的干净外观，代价是不能再拉大小。
///
/// 修法：窗口建好后把 WS_THICKFRAME 加回去（**不动** WS_CAPTION，所以
/// 系统仍不画原生标题栏），系统随即恢复边缘命中测试——窗口可以像
/// 原生那样拖边缩放，只是边框不可见。
#[cfg(windows)]
fn keep_resize_border(window: &tauri::Window) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_STYLE, SWP_FRAMECHANGED,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WS_THICKFRAME,
    };
    let Ok(hwnd) = window.hwnd() else {
        return;
    };
    // tauri 用 `windows` crate 的 HWND（newtype），windows-sys 0.52 的
    // HWND 是裸 isize —— 取 .0 换过去。
    let hwnd = hwnd.0 as isize;
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        if style & WS_THICKFRAME as isize != 0 {
            return; // 已经有边缘缩放，别重复改样式
        }
        SetWindowLongPtrW(hwnd, GWL_STYLE, style | WS_THICKFRAME as isize);
        // SWP_FRAMECHANGED：样式改了要让系统重算非客户区，否则命中测试
        // 仍按旧样式走（看起来就是「加了样式也不能拖」）。
        SetWindowPos(
            hwnd,
            0,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// 入口（main.rs 调用；pub 因为 bin 目标跨 crate 引用它）。
pub fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // ── MCP 服务模式 ──
    // stdin/stdout 就是协议通道，**不开窗口**。放在最前面判断：MCP 模式下
    // 单实例互斥体、待开队列、监听端口全都不该起（外部 AI 要的是工具，
    // 不是另一个查看器窗口）。
    if args.iter().any(|a| a == "--mcp") {
        mcp_server::serve_stdio();
        return;
    }

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
            commands::viewer_open_with,
            commands::viewer_available_apps,
            commands::viewer_wordcraft_path,
            commands::viewer_pick_files,
            commands::viewer_copy_files,
            commands::viewer_paste_files,
            commands::viewer_copy_text,
            commands::viewer_paste_text,
            commands::viewer_run_executable,
            filesearch::viewer_search_global,
            filesearch::viewer_filesearch_status,
            filesearch::viewer_filesearch_stop,
            filesearch::viewer_disk_usage,
            commands::viewer_take_pending,
            commands::ai_send,
            commands::viewer_ai_status,
            commands::viewer_ai_models,
            commands::viewer_get_settings,
            commands::viewer_set_settings,
            commands::viewer_set_ai_key,
            commands::viewer_mcp_start,
            commands::viewer_mcp_stop,
            commands::viewer_mcp_status,
            artifacts::write_bytes,
            docx_edit::docx_read,
            docx_edit::docx_write,
            image_ops::image_info,
            image_ops::image_rasterize,
            image_ops::image_apply,
            image_ops::image_compress_to,
            image_ops::image_batch,
            image_ops::image_save_dialog,
            image_ops::image_open_dialog,
            image_ops::image_montage,
            image_ops::image_write_composite,
            pdf_ops::pdf_info,
            pdf_ops::pdf_ops,
            pdf_ops::pdf_replace_text,
            pdf_ops::pdf_add_annotation,
            pdf_ops::pdf_add_annotations,

            pdf_ops::pdf_replace_text_at,
            pdf_ocr_cmd::pdf_ocr_page,
            markitdown::document_to_markdown,
            pdf_ops::images_to_pdf,
            pdf_ops::write_text,
            sysmon::sysmon_processes,
            sysmon::sysmon_kill_process,
            sysmon::sysmon_disks,
            procclean::sysmon_cleanup_scan,
            procclean::sysmon_cleanup_execute,
            cleanup::cleanup_scan,
            cleanup::cleanup_execute,
            cleanup_ai::cleanup_ai_advise,
            drive_scan::cleanup_scan_drive,
            drive_scan::cleanup_remove_entries,
            webview_host::web_open,
            webview_host::web_bounds,
            webview_host::web_close,
            webview_host::web_set_visible,
            webview_host::web_navigate,
            webview_host::web_exec,
            webview_host::web_state,
            browser::browser_open,
            browser::browser_sessions,
            browser::browser_close,
            pin_window::pin_open,
            pin_window::pin_close,
            pin_window::pin_list,
            pin_window::pin_path,
            pin_window::pin_place,
            viewer_control::viewer_state_report,
            viewer_control::viewer_action_reply,
            viewer_control::viewer_open_path,
            viewer_control::viewer_open_url,
        ])
        .plugin(tauri_plugin_dialog::init())
        .on_window_event(|_w, event| {
            // 无边框窗口要补回边缘缩放样式（见 keep_resize_border）。
            // 事件里才拿得到 Window：AppHandle 侧只有 WebviewWindow，
            // 而 hwnd() 在 Window 上。
            #[cfg(windows)]
            {
                use tauri::WindowEvent;
                if matches!(event, WindowEvent::Resized(_) | WindowEvent::Focused(true)) {
                    keep_resize_border(_w);
                }
            }
            #[cfg(not(windows))]
            let _ = event;
        })
        .setup(move |app| {
            seed_pending(app.handle(), &init_args);
            spawn_singleton_listener(app.handle().clone());
            // 登记应用句柄：MCP 工具（web_open 等）要在没有前端参与的情况下
            // 开关内嵌网页视图。
            webview_host::set_app(app.handle().clone());
            // 设置里开了「启动时拉起 MCP 服务」就地起（只监听回环）。
            // 起不来不拦启动：查看器本身跟 MCP 无关，不该因为端口被占
            // 就打不开。
            let settings = settings::ViewerSettings::load();
            if settings.mcp.enabled {
                match mcp_http::start(settings.mcp.port, settings.mcp.allow_remote) {
                    Ok(st) => eprintln!("[viewer-mcp] 本地 MCP 端点：{}", st.endpoint),
                    Err(e) => eprintln!("[viewer-mcp] 本地 MCP 端点启动失败：{e}"),
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("viewer: failed to run tauri app");
}

/// 扫描件 OCR 的 Tauri 命令（纯逻辑在 `pdf_ocr.rs`，这里只是壳）。
///
/// ## 为什么命令体放在这个子模块里，而不是写在 crate 根
///
/// `#[tauri::command]` 会在**它所在的模块**里生成一个 `__cmd__<命令名>` 宏。
/// 命令写在 crate 根时，`generate_handler!` 展开出来的同名 import 与这个宏
/// 撞名 → `E0255: the name __cmd__pdf_ocr_page is defined multiple times`。
/// 放进子模块就没有这个问题（其余命令都在各自模块里，也是这么过的）。
///
/// ## 为什么命令体不直接放进 `pdf_ocr.rs`
///
/// `pdf_ocr.rs` 刻意保持**零 tauri 依赖**：验证探针用 `#[path]` 直接编译**仓库
/// 里这份真实代码**（而不是复制一份会漂移的逻辑）—— 见
/// `docs/notes/implemented/feature/2026-10-21-windows-winrt-ocr-for-scanned-pdf.md`
/// 的验证证据表。
pub mod pdf_ocr_cmd {
    use std::path::PathBuf;

    use super::pdf_ocr;

    /// 给 PDF 的某一页加**隐形文字层**（扫描件 OCR）。
    ///
    /// `png_base64` 是这一页渲染出来的位图（前端用 pdf.js 渲染）—— Rust 侧没有
    /// 渲染器，渲染留在已经具备它的前端。
    ///
    /// 产物是新文件（原 PDF 不动），由 `safe_output_path` 守着。
    #[tauri::command]
    pub async fn pdf_ocr_page(
        src_path: String,
        out_path: Option<String>,
        page: u32,
        png_base64: String,
    ) -> Result<pdf_ocr::OcrPageResult, String> {
        let src = PathBuf::from(&src_path);
        if !src.is_file() {
            return Err(format!("不是文件: {src_path}"));
        }
        let raw = crate::commands::base64_decode(&png_base64)
            .ok_or("页面图片不是合法的 base64")?;
        if raw.is_empty() {
            return Err("页面图片是空的".into());
        }
        let out = crate::paths::safe_output_path(&src, out_path.as_deref(), None)?;
        // WinRT 的 COM apartment 是**每线程**的：整段 OCR 必须固定在一个线程上，
        // 并在那个线程里初始化 apartment（见模块注释）。
        tauri::async_runtime::spawn_blocking(move || {
            pdf_ocr::ocr_page_to_pdf(&src, &out, page, &raw)
        })
        .await
        .map_err(|e| format!("OCR 任务失败：{e}"))?
    }
}

