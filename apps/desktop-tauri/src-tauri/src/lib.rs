pub mod browser_cleanup;
pub mod commands;
pub mod commands_cua;
pub mod officecli;
pub mod portable_env;
pub mod sidecar;

use sidecar::{HermixSidecar, WatchdogConfig};
use std::sync::{Arc, Mutex};
use tauri::Emitter;
use tauri::Manager;

pub struct AppState {
    /// Wrapped in `Arc<Mutex<...>>` so the watchdog background task can hold
    /// an independent reference and call `restart()` even if a command is
    /// currently inspecting `is_running()`.
    pub sidecar: Arc<Mutex<HermixSidecar>>,
}

pub fn run() {
    // ── WebView2 / Chromium 启动参数 ──
    //
    // 必须在 `tauri::Builder::default()` 跑之前设置。
    // Tauri 2 创建 WebView 时会读取这个环境变量并把这些参数
    // 透传给 msedgewebview2.exe。
    //
    // 目标（按收益从大到小）：
    //   1. 磁盘缓存从无上限 → 20MB（节省 ~200MB 磁盘）
    //   2. 关闭 dev 模式捆绑的 DevTools 扩展（节省 28MB 磁盘）
    //   3. 减少 GPU 进程合成开销（运行时 -200-400MB）
    //   4. 限制 V8 老生代 heap（避免长会话后 heap 涨到 1GB+）
    //
    // 注意：参数用空格分隔，多个 flag 之间也是空格。
    // 完整 Chromium flag 列表：https://peter.sh/experiments/chromium-command-line-switches/
    let webview_args = concat!(
        // ── 内存优化 ──
        "--disable-gpu-vsync",
        " --disable-features=CalculateNativeWinOcclusion",
        " --disable-renderer-backgrounding",
        " --disable-features=IntensiveWakeUpThrottling",
        // ── 缓存优化（关键：默认无上限能涨到 GB） ──
        " --disk-cache-size=20971520",
        " --media-cache-size=2097152",
        " --disable-features=NetworkService",
        // ── 性能优化 ──
        " --js-flags=--max-old-space-size=256",
        " --no-pings",
        " --disable-background-networking",
        " --disable-component-update",
        " --disable-domain-reliability",
    );
    // set_var 在多线程下需要 unsafe，且必须在创建 WebView 之前调用。
    // 我们的 portable_env::install_portable_env 已经会在 setup 里跑，
    // 这里抢在 Builder 之前更安全。
    #[allow(unused_unsafe)]
    unsafe {
        std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", webview_args);
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .setup(|app| {
            // 1. Install portable-mode env vars FIRST. This resolves
            //    the install root and ensures data/cache/logs/webview
            //    subdirs exist before any child process is spawned.
            //    The sidecar below will inherit these env vars
            //    (Command::new does not clear them).
            match portable_env::install_portable_env() {
                Ok(root) => {
                    println!("[Hermix] install root: {}", root.display());
                }
                Err(e) => {
                    eprintln!("[Hermix] failed to install portable env: {e}");
                }
            }

            let sidecar = Arc::new(Mutex::new(HermixSidecar::new()));
            app.manage(AppState {
                sidecar: sidecar.clone(),
            });

            // Start hermix serve in background
            let handle = app.handle().clone();
            let state = handle.state::<AppState>();
            let started = {
                let mut sb = state.sidecar.lock().unwrap();
                sb.start().is_ok()
            };

            if started {
                // Start the watchdog: if /health fails 3 times in a row
                // (5s interval = 15s of total outage), automatically
                // restart the sidecar. The handle is dropped immediately
                // (detached), so the task runs for the lifetime of the
                // process.
                let _wd_handle = HermixSidecar::start_watchdog(&sidecar, WatchdogConfig::default());

                let handle2 = handle.clone();
                tauri::async_runtime::spawn(async move {
                    // Poll /health until the server is actually ready,
                    // then emit the ready event so the frontend can
                    // transition out of the splash screen.
                    let deadline = std::time::Instant::now()
                        + std::time::Duration::from_secs(30);
                    let url = "http://127.0.0.1:3000/health";
                    let mut ready = false;
                    while std::time::Instant::now() < deadline {
                        if let Ok(resp) = reqwest::get(url).await {
                            if resp.status().is_success() {
                                ready = true;
                                break;
                            }
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                    if ready {
                        println!("[Hermix] Server ready (health check passed)");
                    } else {
                        println!("[Hermix] Server health check timed out after 30s, emitting ready anyway");
                    }
                    let _ = handle2.emit("hermix:server-ready", ());
                });
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                let state = window.state::<AppState>();
                let mut sb = state.sidecar.lock().unwrap();
                sb.stop();
                // Also reap any Chrome we previously spawned via the CDP
                // bootstrap — its `Child` handle was deliberately detached
                // (see `crates/hermix-tools/src/tools/cdp_bootstrap.rs`), so
                // without this hook the browser outlives us and squats on
                // `127.0.0.1:9222` across restarts.
                browser_cleanup::kill_spawned_chrome();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_settings_dir,
            commands::get_data_dir,
            commands::pick_directory,
            commands::open_external_url,
            commands::restart_app,
            commands::get_server_status,
            commands::read_artifact_file,
            commands::read_artifact_file_bytes,
            commands::artifact_path_exists,
            commands::restart_sidecar,
            commands::render_office_preview,
            // Phase L1.4 (2026-06-29): Settings → Backend → Python
            // 运行时探测。`python_detect` 列出 override / env / sibling
            // / PATH 四档候选，给 UI 看；`set_python_override` 把
            // 用户填的路径写到当前进程的 `HERMIX_PYTHON`，下一次
            // `execute_code` / `read_pdf` 调用立即生效（无重启）。
            commands::python_detect,
            commands::set_python_override,
            commands_cua::cua_screenshot,
            commands_cua::cua_screen_info,
            commands_cua::cua_left_click,
            commands_cua::cua_type_text,
            commands_cua::cua_press_key,
            commands_cua::cua_list_windows,
            commands_cua::cua_window_at_point,
            commands_cua::cua_click_window,
            // Phase 2-C: UIA element-tree commands
            commands_cua::cua_ui_tree,
            commands_cua::cua_ui_find,
            commands_cua::cua_ui_click,
            commands_cua::cua_ui_focus,
            commands_cua::cua_ui_invoke,
            // Phase 2-D: OCR
            commands_cua::cua_ocr_recognize,
            // Phase 2-E: Browser automation (self-hosted CDP)
            commands_cua::cua_browser_navigate,
            commands_cua::cua_browser_screenshot,
            commands_cua::cua_browser_evaluate,
            commands_cua::cua_browser_click,
        ])
        .run(tauri::generate_context!())
        .expect("error while running hermix-desktop");
}
