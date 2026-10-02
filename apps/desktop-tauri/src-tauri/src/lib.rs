pub mod browser_cleanup;
pub mod commands;
pub mod commands_custom_app;
pub mod commands_cua;
pub mod commands_local_search;
pub mod commands_workbench;
pub mod markitdown;
pub mod officecli;
pub mod portable_env;
pub mod sidecar;
pub mod stdio_bridge;
pub mod system_settings;
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

/// Default `SOUL.md` (persona) shipped *inside the binary* via `include_str!`.
/// On first launch we write it to `<HERMIX_HOME>/SOUL.md` ONLY if that file is
/// absent — so a fresh install ships with the curated persona, but any user
/// edit to their SOUL.md is never clobbered on upgrade or re-launch.
///
/// To change what new installs ship with, edit `builtin-soul/SOUL.md` in this
/// crate (NOT the runtime `~/.hermix/SOUL.md`) and rebuild — same single-source
/// pattern as `BUILTIN_SKILLS`.
const DEFAULT_SOUL_MD: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/builtin-soul/SOUL.md"
));

/// Seed `<HERMIX_HOME>/SOUL.md` on first launch if it does not already exist.
/// Never overwrites an existing file, so user customizations persist across
/// upgrades. The default content is embedded at compile time, so this works
/// in both dev and packaged builds without touching the Tauri resource dir.
fn ensure_default_soul_md(hermix_home: &std::path::Path) {
    let target = hermix_home.join("SOUL.md");
    if target.exists() {
        return; // user already has a SOUL.md — never clobber it
    }
    if let Some(parent) = target.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("[Hermix] failed to create HERMIX_HOME {}: {e}", parent.display());
            return;
        }
    }
    match std::fs::write(&target, DEFAULT_SOUL_MD) {
        Ok(()) => println!("[Hermix] seeded default SOUL.md to {}", target.display()),
        Err(e) => eprintln!("[Hermix] failed to write default SOUL.md {}: {e}", target.display()),
    }
}

/// Write built-in skills into `skills_dir` on first launch. If the on-disk
/// SKILL.md has a lower `version` in its YAML frontmatter than the embedded
/// copy, the file is overwritten — this ensures schema/API changes in the
/// skill documentation propagate to existing installations without requiring
/// the user to manually delete old files. User-edited files whose version
/// matches or exceeds the embedded version are left untouched.
fn ensure_builtin_skills(skills_dir: &std::path::Path) {
    let mut wrote = 0u32;
    for (rel, content) in BUILTIN_SKILLS {
        let target = skills_dir.join(rel).join("SKILL.md");
        if target.exists() {
            // Check if the embedded version is newer than the on-disk version.
            let embedded_ver = extract_skill_version(content);
            let disk_ver = std::fs::read_to_string(&target)
                .ok()
                .map(|s| extract_skill_version(&s))
                .unwrap_or(0);
            if disk_ver >= embedded_ver {
                continue; // on-disk is up to date (or user has newer)
            }
            // Embedded version is newer — overwrite.
            eprintln!(
                "[Hermix] updating builtin skill {rel}: disk version {disk_ver} < embedded version {embedded_ver}"
            );
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

/// Extract the `version` field from a SKILL.md YAML frontmatter.
/// Returns 0 if the frontmatter is missing or the version cannot be parsed.
/// The frontmatter is the text between the first `---` and second `---`.
fn extract_skill_version(content: &str) -> u32 {
    let mut start = None;
    let mut end = None;
    for (i, line) in content.lines().enumerate() {
        if line.trim() == "---" {
            if start.is_none() {
                start = Some(i);
            } else {
                end = Some(i);
                break;
            }
        }
    }
    let (Some(s), Some(e)) = (start, end) else {
        return 0;
    };
    let frontmatter: String = content
        .lines()
        .enumerate()
        .filter(|(i, _)| *i > s && *i < e)
        .map(|(_, l)| l)
        .collect::<Vec<&str>>()
        .join("\n");
    for line in frontmatter.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("version:") {
            return rest.trim().parse::<u32>().unwrap_or(0);
        }
    }
    0
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

/// Extract the custom-app runtime pieces (neutralino binary + client library) from
/// the bundled Tauri resources into `HERMIX_HOME` so they are reachable
/// at a stable path regardless of whether the app is installed (portable)
/// or run from source.
///
/// `commands_custom_app` resolves these via `hermix_home()/loader/`;
/// this function makes that path concrete on first launch. If the bundled
/// resources are absent (dev build without `bundle.resources`, or resources
/// already extracted), it is a silent no-op — the custom-app commands
/// then fall back to the `current_exe/resources/` location, so packaging
/// still works.
fn ensure_loader(
    resource_dir: &std::path::Path,
    hermix_home: &std::path::Path,
) {
    // Neutralino runtime: neutralino-win-x64.exe + neutralino.js, unpacked
    // to HERMIX_HOME/loader on first launch.
    let pairs = [
        ("resources/loader", "loader"),
    ];
    for (src_rel, dst_rel) in pairs {
        let bundled = resource_dir.join(src_rel);
        if !bundled.exists() {
            continue; // not bundled in this build — skip silently
        }
        let target = hermix_home.join(dst_rel);
        if let Err(e) = std::fs::create_dir_all(&target) {
            eprintln!(
                "[Hermix] failed to create {} dir {}: {e}",
                dst_rel,
                target.display()
            );
            continue;
        }
        let mut copied = 0u32;
        let mut entries = match std::fs::read_dir(&bundled) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("[Hermix] cannot read bundled {src_rel}: {e}");
                continue;
            }
        };
        while let Some(entry) = entries.next() {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let src_path = entry.path();
            if !src_path.is_file() {
                continue;
            }
            // Skip zero-byte placeholders. These are gitignored stubs that
            // exist only to satisfy Tauri's resource check in offline/sandbox
            // builds and must NEVER be copied over a real binary or be the
            // only thing shipped to a user on a fresh install.
            let src_size = match entry.metadata() {
                Ok(m) => m.len(),
                Err(_) => continue,
            };
            if src_size == 0 {
                eprintln!(
                    "[Hermix] skipping zero-byte bundled asset {} — run the build scripts to fetch the real binary",
                    src_path.display()
                );
                continue;
            }
            let dst_path = target.join(entry.file_name());
            // Copy when missing, or when the bundled asset is larger than what
            // we extracted before (covers upgrades and a 0-byte stub that
            // previously slipped through). This way a user never has to
            // manually copy files after installing on a new machine.
            let should_copy = match dst_path.metadata() {
                Ok(m) => src_size > m.len(),
                Err(_) => true,
            };
            if !should_copy {
                continue; // already extracted on a prior launch
            }
            match std::fs::copy(&src_path, &dst_path) {
                Ok(_) => copied += 1,
                Err(e) => eprintln!(
                    "[Hermix] failed to copy {src_rel} asset {}: {e}",
                    src_path.display()
                ),
            }
        }
        if copied > 0 {
            println!(
                "[Hermix] extracted {copied} custom-app {dst_rel} file(s) to {}",
                target.display()
            );
        }
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

/// 最近一次启动阶段文案。启动页用它显示真实进度；setup 早于前端 listen，
/// 所以除 emit 外还要能回查，否则错过事件就永远停在初始文案。
static BOOT_STAGE: std::sync::OnceLock<Mutex<String>> = std::sync::OnceLock::new();

fn set_boot_stage(handle: &tauri::AppHandle, stage: &str) {
    let slot = BOOT_STAGE.get_or_init(|| Mutex::new(String::new()));
    if let Ok(mut g) = slot.lock() {
        *g = stage.to_string();
    }
    let _ = handle.emit("hermix:boot-stage", stage.to_string());
}

#[tauri::command]
fn boot_stage() -> String {
    BOOT_STAGE
        .get()
        .and_then(|s| s.lock().ok().map(|g| g.clone()))
        .unwrap_or_default()
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
    //   4. 限制 V8 老生代 heap（1GB 上限，避免 heap 无限膨胀；太小会引发频繁 full GC）
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
        // 256MB 会让长会话 V8 频繁 full GC（全 UI 卡顿的主因之一），放宽到 1GB
        " --js-flags=--max-old-space-size=1024",
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
        // L4 系统能力：桌面通知 + 开机自启。托盘不引插件（用 tauri 的
        // `tray-icon` feature，见 Cargo.toml）。
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            // ── 启动白屏修复（2026-09-30）────────────────────────────
            // 窗口在配置里 `visible: false`：webview 首帧渲染完成之前用户
            // 看到的是空桌面而不是白窗。这里在页面加载完成时显示窗口 ——
            // 第一眼直接是完整启动屏（吉祥物 + 进度条），零白屏。
            // 兜底：2.5s 后无条件 show，防止页面加载失败时窗口永不出现。
            {
                // 页面加载完成的 show 挂在 Builder::on_page_load（见下方）；
                // 这里只放兜底定时器：页面加载失败时窗口也不能永远不出现。
                if let Some(win) = app.get_webview_window("main") {
                    let fallback = win.clone();
                    std::thread::spawn(move || {
                        // 4s：dev 模式 vite 冷启动可能超过 2.5s，太早 show 会把
                        // 白屏放回来；生产（本地资源）通常 <1s 就走正常路径。
                        std::thread::sleep(std::time::Duration::from_millis(4000));
                        let _ = fallback.show();
                    });
                }
            }

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

            // 1a-2. Runtime asset-protocol scope. The static scope in
            //    tauri.conf.json (`$HOME/.hermix/**`) fails on Windows when
            //    (a) portable mode redirects HERMIX_HOME elsewhere, or (b)
            //    the drive-letter case / path separators don't match the
            //    glob (WebView2 requests then 403 on every workbench
            //    asset). Authorize the REAL hermix_home directory here —
            //    one source of truth, matches whatever mode we booted in.
            {
                use tauri::Manager;
                let wb_root = hermix_home.join("workbenches");
                let scope = app.asset_protocol_scope();
                if let Err(e) = scope.allow_directory(&wb_root, true) {
                    eprintln!(
                        "[Hermix] asset scope: failed to allow {}: {e}",
                        wb_root.display()
                    );
                }
            }

            // 启动阶段广播：写入静态最近阶段并 emit，供启动页显示真实进度。
            // 早期阶段可能早于前端 listen，故另提供 boot_stage command 拉取当前值。
            let handle = app.handle().clone();
            set_boot_stage(&handle, "正在准备运行环境…");

            // 1b. Materialize built-in skills into HERMIX_HOME/skills/ on first
            //     run. These ship *inside the binary* (include_str!), so no
            //     resource-dir lookup is required — they work even in dev
            //     builds. Bundled (marketplace) skills still come from the
            //     Tauri resource dir below.
            set_boot_stage(&handle, "正在解包内置技能…");
            let skills_dir = hermix_home.join("skills");
            ensure_builtin_skills(&skills_dir);
            if let Ok(resource_dir) = app.path().resource_dir() {
                set_boot_stage(&handle, "正在准备运行时组件…");
                ensure_bundled_skills(&resource_dir, &skills_dir);
                ensure_loader(&resource_dir, &hermix_home);
            } else {
                eprintln!("[Hermix] cannot resolve resource_dir — bundled skills and loader will not be extracted");
            }

            // 1c. Seed the default SOUL.md (persona) on first launch if the
            //     user hasn't created one yet. Embedded, so no resource lookup.
            ensure_default_soul_md(&hermix_home);

            let sidecar = Arc::new(Mutex::new(HermixSidecar::new()));
            app.manage(AppState {
                sidecar: sidecar.clone(),
            });

            set_boot_stage(&handle, "正在启动本地服务…");

            // Start hermix serve in background
            let state = handle.state::<AppState>();
            let started = {
                let mut sb = state.sidecar.lock().unwrap();
                sb.start(&handle).is_ok()
            };

            if started {
                // Start the watchdog: if the bridge is unhealthy 30 times
                // in a row (5s interval = 150s of total outage), automatically
                // restart the sidecar with exponential backoff.
                // After 5 consecutive restart failures, gives up and
                // requires manual restart.
                let _wd_handle = HermixSidecar::start_watchdog(
                    &sidecar,
                    WatchdogConfig::default(),
                    Some(handle.clone()),
                );

                let handle2 = handle.clone();
                set_boot_stage(&handle, "正在等待服务就绪…");
                tauri::async_runtime::spawn(async move {
                    // Round-trip /health through the stdio bridge until
                    // the server is actually serving requests, then emit
                    // the ready event so the frontend can transition
                    // out of the splash screen. `wait_until_ready` is a
                    // free function (no `&self` borrow) so the spawned
                    // future stays Send-able across the `.await`.
                    let ready =
                        HermixSidecar::wait_until_ready(std::time::Duration::from_secs(30)).await;
                    if ready {
                        println!("[Hermix] Server ready (health check passed)");
                    } else {
                        println!("[Hermix] Server health check timed out after 30s, emitting ready anyway");
                    }
                    let _ = handle2.emit("hermix:server-ready", ());
                    // 事件已带 handle2（clone），这里用原 handle 标注最终阶段。
                    set_boot_stage(&handle, "服务已就绪，正在进入主界面");
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
        // ── 启动白屏修复（2026-09-30）──────────────────────────────
        // 窗口在配置里 `visible: false`：webview 首帧渲染前用户看到的是
        // 空桌面而不是白窗。页面加载完成（splash 的 HTML 是内联的，此刻
        // 必然可绘制）再显示窗口 —— 第一眼直接是完整启动屏，零白屏。
        // Builder::on_page_load 对所有 webview 生效；show 幂等。
        .on_page_load(|webview: &tauri::Webview, payload: &tauri::webview::PageLoadPayload| {
            if payload.event() == tauri::webview::PageLoadEvent::Finished {
                // Webview::window() 拿主 Window（不走 Manager trait，避免作用域问题）。
                let window = webview.window();
                if window.label() == "main" {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            system_settings::get_autostart,
            system_settings::set_autostart,
            system_settings::send_notification,
            system_settings::get_tray_enabled,
            system_settings::sync_tray,
            system_settings::get_wallpaper,
            system_settings::set_wallpaper,
            system_settings::clear_wallpaper,
            commands::get_settings_dir,
            commands::get_data_dir,
            commands::pick_directory,
            commands::open_external_url,
            commands::restart_app,
            commands::list_backend_log_files,
            commands::read_backend_log_tail,
            commands::open_log_directory,
            commands::get_server_status,
            commands::read_artifact_file,
            commands::read_artifact_file_bytes,
            commands::artifact_path_exists,
            commands::list_directory,
            commands::restart_sidecar,
            commands_local_search::local_search,
            commands_local_search::local_search_open,
            commands_local_search::local_search_preview,
            commands::render_office_preview,
            commands::document_to_markdown,
            commands::get_runtime_info,
            boot_stage,
            // stdio bridge chokepoints — every REST call and every
            // WS-style upstream frame the frontend needs now flows
            // through one of these three commands.
            commands::backend_http_request,
            commands::backend_channel_send,
            commands::backend_ready,
            // Settings → Backend → Python 运行时探测。`python_detect`
            // 列出 override / env / sibling
            // / PATH 四档候选，给 UI 看；`set_python_override` 把
            // 用户填的路径写到当前进程的 `HERMIX_PYTHON`，下一次
            // `execute_code` / `read_pdf` 调用立即生效（无重启）。
            commands::python_detect,
            commands::set_python_override,
            // 内置运行时探测（Node / Python / Git）：内置位 → PATH → override。
            commands::runtime_detect,
            commands_cua::screen::cua_screenshot,
            commands_cua::screen::cua_screen_info,
            commands_cua::screen::cua_left_click,
            commands_cua::screen::cua_type_text,
            commands_cua::screen::cua_press_key,
            commands_cua::window::cua_list_windows,
            commands_cua::window::cua_window_at_point,
            commands_cua::window::cua_click_window,
            // Phase 2-C: UIA element-tree commands
            commands_cua::ui::cua_ui_tree,
            commands_cua::ui::cua_ui_find,
            commands_cua::ui::cua_ui_click,
            commands_cua::ui::cua_ui_focus,
            commands_cua::ui::cua_ui_invoke,
            // Phase 2-E: Browser automation (self-hosted CDP)
            commands_cua::browser::cua_browser_navigate,
            commands_cua::browser::cua_browser_screenshot,
            commands_cua::browser::cua_browser_evaluate,
            commands_cua::browser::cua_browser_click,
            // Custom app management
            commands_custom_app::custom_app_init,
            commands_custom_app::custom_app_list,
            commands_custom_app::custom_app_package,
            commands_custom_app::custom_app_run,
            commands_custom_app::custom_app_open_dir,
            commands_custom_app::custom_app_validate,
            commands_custom_app::custom_app_smoke_test,
            commands_workbench::workbench_create,
            commands_workbench::workbench_list,
            commands_workbench::workbench_set_primary,
            commands_workbench::workbench_delete,
            commands_workbench::workbench_read,
            commands_workbench::workbench_write,
            commands_workbench::workbench_files,
            commands_workbench::workbench_entry_path,
            commands_workbench::workbench_bind_session,
            commands_workbench::workbench_ensure_session,
            commands_workbench::workbench_lint,
            commands_workbench::workbench_set_deps,
            commands_workbench::workbench_vendor_index,
            commands_workbench::workbench_vendor_dir,
            commands_workbench::workbench_module_index,
            commands_workbench::workbench_db_query,
            commands_workbench::workbench_db_exec,
            commands_workbench::workbench_fs_read,
            commands_workbench::workbench_fs_write,
            commands_workbench::workbench_fs_list,
            commands_workbench::workbench_undo,
            commands_workbench::workbench_history,
        ])
        .run(tauri::generate_context!())
        .expect("error while running hermix-desktop");
}
