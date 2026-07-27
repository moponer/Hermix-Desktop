pub mod browser_cleanup;
pub mod commands;
pub mod commands_custom_app;
pub mod commands_cua;
pub mod officecli;
pub mod portable_env;
pub mod sidecar;
pub mod webview2_probe;

use sidecar::{HermixSidecar, WatchdogConfig};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::Emitter;
use tauri::Manager;

/// Built-in skills shipped *inside the binary* (via `include_str!`), not as
/// loose resource files. On first launch we materialize each under
/// `HERMIX_HOME/skills/<rel>/SKILL.md` if it is missing, so a fresh install
/// always has them — no `tauri.conf.json` resource entries, no separate sync
/// step, single source of truth right here in this crate.
///
/// To add a built-in skill: drop its `SKILL.md` under
/// `builtin-skills/<group>/<name>/` and add one line to `BUILTIN_SKILLS`
/// below. Nothing else to wire up.
const STORAGE_ANALYZER_SKILL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/builtin-skills/hermix/storage-analyzer/SKILL.md"
));
const PONYTAIL_SKILL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/builtin-skills/hermix/ponytail/SKILL.md"
));
const CUSTOM_APP_SKILL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/builtin-skills/hermix/custom-app-development/SKILL.md"
));

const BUILTIN_SKILLS: &[(&str, &str)] = &[
    ("hermix/storage-analyzer", STORAGE_ANALYZER_SKILL),
    ("hermix/ponytail", PONYTAIL_SKILL),
    ("hermix/custom-app-development", CUSTOM_APP_SKILL),
];

/// Write built-in skills into `skills_dir` on first launch. Skips any skill
/// whose target already exists, so user customizations are never clobbered.
fn ensure_builtin_skills(skills_dir: &std::path::Path) {
    let mut wrote = 0u32;
    for (rel, content) in BUILTIN_SKILLS {
        let target = skills_dir.join(rel).join("SKILL.md");
        if target.exists() {
            continue;
        }
        if let Some(parent) = target.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("[Hermix] failed to create builtin-skill dir {}: {e}", parent.display());
                continue;
            }
        }
        match std::fs::write(&target, content) {
            Ok(()) => wrote += 1,
            Err(e) => eprintln!("[Hermix] failed to write builtin skill {rel}: {e}"),
        }
    }
    if wrote > 0 {
        println!("[Hermix] wrote {wrote} built-in skills to {}", skills_dir.display());
    }
}

/// Ensure bundled skills (packaged via `tauri.conf.json` → `resources`)
/// are present under the user's `HERMIX_HOME/skills/` directory on first
/// launch. Only copies files that do not already exist — subsequent runs
/// are no-ops.
///
/// Every subdirectory of `resources/skills/<group>/` is a skill group; each
/// group is extracted to `HERMIX_HOME/skills/<group>/` exactly once (skipped
/// if the target group already exists, so user modifications are never
/// clobbered). This keeps the bundling data-driven: drop a skill under
/// `resources/skills/<group>/<skill>/` and list it in `tauri.conf.json`
/// `resources` — no Rust change required.
fn ensure_bundled_skills(resource_dir: &std::path::Path, skills_dir: &std::path::Path) {
    let bundled_root = resource_dir.join("resources/skills");
    if !bundled_root.exists() {
        // No bundled skills at this resource path — the binary may not
        // have been packaged (e.g. dev build, non-Windows). Silent no-op.
        return;
    }
    let mut total_copied = 0u32;
    let mut had_error = false;
    for entry in match std::fs::read_dir(&bundled_root) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[Hermix] cannot read bundled-skills dir: {e}");
            return;
        }
    } {
        let group = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !group.path().is_dir() {
            continue;
        }
        let group_name = group.file_name().to_string_lossy().into_owned();
        let target = skills_dir.join(&group_name);
        if target.exists() {
            // Already extracted on a prior launch; skip this group to avoid
            // overwriting any user modifications.
            continue;
        }
        if let Err(e) = std::fs::create_dir_all(&target) {
            eprintln!("[Hermix] failed to create bundled-skills dir {group_name}: {e}");
            had_error = true;
            continue;
        }
        match copy_dir_recursively(&group.path(), &target) {
            Ok(copied) => total_copied += copied,
            Err(e) => {
                eprintln!("[Hermix] failed to copy bundled skills group {group_name}: {e}");
                had_error = true;
            }
        }
    }
    if total_copied > 0 {
        println!(
            "[Hermix] extracted {total_copied} bundled skill files to {}",
            skills_dir.display()
        );
    } else if !had_error {
        // Nothing new to extract (all groups already present) — normal no-op.
    }
}

/// Copy bundled OCR models (packaged via `tauri.conf.json` → `resources`)
/// into `HERMIX_HOME/ocr/` on first launch. Idempotent: each model file is
/// skipped if it already exists, so re-runs are no-ops and an updated model
/// shipped in a later release only lands when the user hasn't customized it.
///
/// This is what makes the local OCR engine (`hermix_ocr` / oar-ocr /
/// PP-OCRv5) work fully offline — `ocr_engine()` (in `hermix-tools`) loads
/// these exact files from `HERMIX_HOME/ocr/` at runtime. If the models are
/// missing (dev build, or a release that omitted `bundle.resources`), OCR
/// simply stays disabled and callers fall back to a sub-agent's `read_image`
/// vision path. No models present → silent no-op, never an error.
fn ensure_ocr_models(resource_dir: &std::path::Path, ocr_dir: &std::path::Path) {
    let bundled = resource_dir.join("resources/ocr");
    if !bundled.exists() {
        // No bundled models at this resource path — dev build or unpackaged.
        // Silent no-op; OCR stays disabled until models are staged.
        return;
    }
    if let Err(e) = std::fs::create_dir_all(ocr_dir) {
        eprintln!("[Hermix] failed to create ocr dir {}: {e}", ocr_dir.display());
        return;
    }
    let mut copied = 0u32;
    for entry in match std::fs::read_dir(&bundled) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[Hermix] cannot read bundled ocr dir: {e}");
            return;
        }
    } {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let src_path = entry.path();
        if !src_path.is_file() {
            continue;
        }
        let dst_path = ocr_dir.join(entry.file_name());
        if dst_path.exists() {
            // Already extracted on a prior launch; skip so updates are
            // intentional (re-copy only when the target is absent).
            continue;
        }
        match std::fs::copy(&src_path, &dst_path) {
            Ok(_) => copied += 1,
            Err(e) => eprintln!(
                "[Hermix] failed to copy ocr model {}: {e}",
                src_path.display()
            ),
        }
    }
    if copied > 0 {
        println!(
            "[Hermix] extracted {copied} OCR model files to {}",
            ocr_dir.display()
        );
    }
}

/// Recursive directory copy. Returns the number of files copied.
fn copy_dir_recursively(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<u32> {
    let mut count = 0u32;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let file_name = entry.file_name();
        let src_path = entry.path();
        let dst_path = dst.join(&file_name);
        if ty.is_dir() {
            std::fs::create_dir_all(&dst_path)?;
            count += copy_dir_recursively(&src_path, &dst_path)?;
        } else {
            // Skip copy if the destination already has the file.
            if !dst_path.exists() {
                std::fs::copy(&src_path, &dst_path)?;
                count += 1;
            }
        }
    }
    Ok(count)
}

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
            // 0. WebView2 runtime probe (Windows only). Tauri 2's
            //    own WebView creation step fails with a cryptic
            //    HRESULT when the Evergreen runtime is missing —
            //    run a registry probe here so we can emit a
            //    friendly hint to stderr before Tauri shows its
            //    own dialog. Non-Windows targets and the
            //    `Unknown` path are silent so dev builds on
            //    macOS / Linux don't see noisy stderr.
            match webview2_probe::probe() {
                webview2_probe::WebView2Status::Missing => {
                    eprintln!("{}", webview2_probe::explain_missing_runtime());
                }
                webview2_probe::WebView2Status::Available
                | webview2_probe::WebView2Status::Unknown => {
                    // No-op: Tauri will report its own error if
                    // the runtime really is broken.
                }
            }

            // 1. Install portable-mode env vars FIRST. This resolves
            //    the install root and ensures data/cache/logs/webview
            //    subdirs exist before any child process is spawned.
            //    The sidecar below will inherit these env vars
            //    (Command::new does not clear them).
            let hermix_home = match portable_env::install_portable_env() {
                Ok(root) => {
                    println!("[Hermix] install root: {}", root.display());
                    root
                }
                Err(e) => {
                    eprintln!("[Hermix] failed to install portable env: {e}");
                    // Fallback so the rest of setup can still proceed.
                    std::env::var("HERMIX_HOME")
                        .map(PathBuf::from)
                        .unwrap_or_else(|_| {
                            dirs::home_dir()
                                .unwrap_or_else(|| PathBuf::from("."))
                                .join(".hermix")
                        })
                }
            };

            // 1b. Materialize built-in skills into HERMIX_HOME/skills/ on first
            //     run. These ship *inside the binary* (include_str!), so no
            //     resource-dir lookup is required — they work even in dev
            //     builds. Bundled (marketplace) skills still come from the
            //     Tauri resource dir below.
            let skills_dir = hermix_home.join("skills");
            ensure_builtin_skills(&skills_dir);
            if let Ok(resource_dir) = app.path().resource_dir() {
                ensure_bundled_skills(&resource_dir, &skills_dir);
                ensure_ocr_models(&resource_dir, &hermix_home.join("ocr"));
            } else {
                eprintln!("[Hermix] cannot resolve resource_dir — bundled skills and OCR models will not be extracted");
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
                // restart the sidecar with exponential backoff.
                // After 5 consecutive restart failures, gives up and
                // requires manual restart.
                let _wd_handle = HermixSidecar::start_watchdog(
                    &sidecar,
                    WatchdogConfig::default(),
                    Some(handle.clone()),
                );

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
            commands::list_directory,
            commands::restart_sidecar,
            commands::render_office_preview,
            commands::get_runtime_info,
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
            // Phase 2-E: Browser automation (self-hosted CDP)
            commands_cua::cua_browser_navigate,
            commands_cua::cua_browser_screenshot,
            commands_cua::cua_browser_evaluate,
            commands_cua::cua_browser_click,
            // Custom app management
            commands_custom_app::custom_app_init,
            commands_custom_app::custom_app_list,
            commands_custom_app::custom_app_package,
            commands_custom_app::custom_app_run,
            commands_custom_app::custom_app_open_dir,
        ])
        .run(tauri::generate_context!())
        .expect("error while running hermix-desktop");
}
