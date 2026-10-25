pub mod backdrop;
pub mod cdp_probe;
pub mod commands;
pub mod commands_custom_app;
pub mod commands_workbench;
pub mod file_search_controller;
pub mod hotkey;
pub mod keep_awake;
pub mod markitdown;
pub mod portable_env;
pub mod screenshot;
pub mod screenshot_controller;
pub mod sidecar;
pub mod single_instance;
pub mod stdio_bridge;
pub mod system_settings;
pub mod updater;
pub mod webview2_probe;

// 被嵌进二进制的前端内容指纹（build.rs 生成，见 build.rs 头注）。
// 前端产物变了它就变 → crate 必然重编 → 不会出现「exe 里是旧 UI」。
include!(concat!(env!("OUT_DIR"), "/frontend_fingerprint.rs"));

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
// 工作流方法论三篇（自写，非 superpowers 原文搬运）：测试先行 / 系统化调试 /
// 完成前验证。与 ponytail（简化纪律）互补。
const WORKFLOW_TDD_SKILL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/builtin-skills/hermix/workflow-tdd/SKILL.md"
));
const WORKFLOW_DEBUGGING_SKILL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/builtin-skills/hermix/workflow-debugging/SKILL.md"
));
const WORKFLOW_VERIFICATION_SKILL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/builtin-skills/hermix/workflow-verification/SKILL.md"
));
// DOCX 文档编写方法论（路由选择 / python-docx 模板 / 字体与表格硬规则 /
// dsh 引擎渲染验证闭环）。脚本由 agent 落到工作区，不随包分发。
const DOCX_SKILL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/builtin-skills/hermix/docx/SKILL.md"
));
// 浏览器操作/测试方法论（snapshot→act→wait→verify 循环、懒加载、截图取证）。
const WEB_GUI_TESTER_SKILL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/builtin-skills/hermix/web-gui-tester/SKILL.md"
));
// 助手阿黎的能力地图（12 类能力路由 + 三条硬规则 + 外部 CLI 任务书模板）。
// 存在的理由：按关键词搜工具永远可能漏，而「她该知道你能做到什么」不能靠
// system prompt 硬编码 —— 新工具上线后这里不用改代码。
const TWIN_SKILL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/builtin-skills/hermix/twin/SKILL.md"
));
// 自诊断方法论（日志位置、症状→关键字对照表、配置项、报告格式）。
const SELF_DIAGNOSIS_SKILL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/builtin-skills/hermix/self-diagnosis/SKILL.md"
));

// 技能附带的脚本/资源（非 SKILL.md）：首启写入，已存在则不覆盖（用户改动优先）。
// docx 的 build_docx.py 把中文字体/页边距/定宽表格等硬规则固化在生成器里。
const DOCX_BUILD_SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/builtin-skills/hermix/docx/scripts/build_docx.py"
));

const BUILTIN_SKILL_EXTRA_FILES: &[(&str, &str)] = &[(
    "hermix/docx/scripts/build_docx.py",
    DOCX_BUILD_SCRIPT,
)];

// 内置子代理档案（ZCode subagent-markdown analog）：首启写入
// <HERMIX_HOME>/agents/，已存在则不覆盖（用户可自建同名档案）。
const BUILTIN_AGENT_PROFILES: &[(&str, &str)] = &[
    (
        "explore.md",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/builtin-agents/explore.md"
        )),
    ),
    (
        "general-purpose.md",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/builtin-agents/general-purpose.md"
        )),
    ),
];

const BUILTIN_SKILLS: &[(&str, &str)] = &[
    ("hermix/storage-analyzer", STORAGE_ANALYZER_SKILL),
    ("hermix/ponytail", PONYTAIL_SKILL),
    ("hermix/custom-app-development", CUSTOM_APP_SKILL),
    ("hermix/workflow-tdd", WORKFLOW_TDD_SKILL),
    ("hermix/workflow-debugging", WORKFLOW_DEBUGGING_SKILL),
    ("hermix/workflow-verification", WORKFLOW_VERIFICATION_SKILL),
    ("hermix/docx", DOCX_SKILL),
    ("hermix/web-gui-tester", WEB_GUI_TESTER_SKILL),
    ("hermix/self-diagnosis", SELF_DIAGNOSIS_SKILL),
    ("hermix/twin", TWIN_SKILL),
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

    // 技能附带文件（脚本等）：只在缺失时写入——它们没有 version 语义，
    // 用户可能按需改过。
    for (rel, content) in BUILTIN_SKILL_EXTRA_FILES {
        let target = skills_dir.join(rel);
        if target.exists() {
            continue;
        }
        if let Some(parent) = target.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("[Hermix] failed to create {}: {e}", parent.display());
                continue;
            }
        }
        match std::fs::write(&target, content) {
            Ok(()) => println!("[Hermix] wrote builtin skill file {rel}"),
            Err(e) => eprintln!("[Hermix] failed to write builtin skill file {rel}: {e}"),
        }
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
        // Version-aware refresh instead of "group exists → skip forever":
        // a skill shipped in v1.0 stayed frozen on disk even after the
        // installer advanced to v1.2, so documentation fixes never reached
        // existing users. Rules:
        //   · SKILL.md — overwrite only when the bundled frontmatter
        //     `version` is strictly greater than the installed one (user
        //     edits at the same/greater version are preserved);
        //   · any other file — copy when missing, never overwrite.
        match refresh_skill_group(&group.path(), &target) {
            Ok(copied) => total_copied += copied,
            Err(e) => {
                eprintln!("[Hermix] failed to refresh bundled skills group {group_name}: {e}");
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

/// Default loopback CDP port for the embedded browser panel. Fixed so the
/// agent tools can find it even after an app restart; the window is a
/// singleton (label `browser-panel`), so two panels cannot collide.
const BROWSER_PANEL_CDP_PORT: u16 = 9333;

/// Open the **visible browser panel** (WebView2 window with CDP debugging).
///
/// The agent tools do not launch their own Edge when a panel is registered
/// (`browser::panel::current()`): they attach to this window's page over CDP,
/// so the user watches the automation inside the app.
///
/// `--remote-debugging-port` is passed as a WebView2 additional browser
/// argument. It binds to loopback only; the port registry file is deleted on
/// close so tools never attach to a stale port.
#[tauri::command]
async fn browser_panel_open(
    app: tauri::AppHandle,
    url: Option<String>,
) -> Result<String, String> {
    let label = "browser-panel";
    if let Some(existing) = app.get_webview_window(label) {
        let _ = existing.close();
    }

    let start_url = url.unwrap_or_else(|| "https://www.bing.com".to_string());
    let parsed: tauri::Url = start_url
        .parse()
        .map_err(|e| format!("无效的 URL `{start_url}`: {e}"))?;

    let builder = tauri::WebviewWindowBuilder::new(
        &app,
        label,
        tauri::WebviewUrl::External(parsed),
    )
    .title("浏览器面板")
    .inner_size(1100.0, 780.0)
    .additional_browser_args(&format!(
        "--remote-debugging-port={BROWSER_PANEL_CDP_PORT}"
    ));

    builder
        .build()
        .map_err(|e| format!("打开浏览器面板失败: {e}"))?;

    hermix_tools::tools::os_auto::browser::panel::register(BROWSER_PANEL_CDP_PORT, None)
        .map_err(|e| format!("登记面板端口失败: {e}"))?;

    Ok(format!(
        "浏览器面板已打开（CDP 端口 {BROWSER_PANEL_CDP_PORT}）。agent 的 browser_* 工具将驱动这个可见面板。"
    ))
}

/// Close the panel and unregister its port (tools fall back to a fresh Edge).
#[tauri::command]
fn browser_panel_close(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("browser-panel") {
        let _ = window.close();
    }
    hermix_tools::tools::os_auto::browser::panel::clear();
    Ok(())
}

/// Seed the built-in sub-agent profiles (`<home>/agents/*.md`). Files that
/// already exist are user-owned and never overwritten.
fn ensure_builtin_agent_profiles(home: &std::path::Path) {
    let dir = home.join("agents");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("[Hermix] failed to create agents dir {}: {e}", dir.display());
        return;
    }
    for (file, content) in BUILTIN_AGENT_PROFILES {
        let target = dir.join(file);
        if target.exists() {
            continue;
        }
        match std::fs::write(&target, content) {
            Ok(()) => println!("[Hermix] seeded agent profile {}", target.display()),
            Err(e) => eprintln!("[Hermix] failed to write agent profile {file}: {e}"),
        }
    }
}

/// Refresh one bundled skill group into `target`, returning the number of
/// files written.
///
/// Rules (kept here so the caller's doc block stays honest):
/// * `SKILL.md` — overwritten only when the bundled frontmatter `version` is
///   strictly greater **and both sides parse**; unparsable versions are
///   treated as user-owned and left alone;
/// * any other file — copied only when missing (never overwritten).
fn refresh_skill_group(src: &std::path::Path, target: &std::path::Path) -> std::io::Result<u32> {
    std::fs::create_dir_all(target)?;
    let mut copied = 0u32;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dest = target.join(entry.file_name());
        if path.is_dir() {
            copied += refresh_skill_group(&path, &dest)?;
            continue;
        }
        let is_skill_md = entry.file_name().to_string_lossy() == "SKILL.md";
        if !dest.exists() {
            std::fs::copy(&path, &dest)?;
            copied += 1;
            continue;
        }
        if is_skill_md {
            let bundled_ver = std::fs::read_to_string(&path)
                .map(|s| extract_skill_version_ord(&s))
                .unwrap_or((0, 0, 0));
            let installed_ver = std::fs::read_to_string(&dest)
                .map(|s| extract_skill_version_ord(&s))
                .unwrap_or((0, 0, 0));
            // An unparsable version (e.g. `1.0.0-beta`, a trailing comment,
            // or a hand-edited file) yields (0,0,0) — treat it as
            // "not comparable" and keep the user's file. Overwriting on an
            // unparsable version would silently destroy local edits.
            let comparable = bundled_ver != (0, 0, 0) && installed_ver != (0, 0, 0);
            if comparable && bundled_ver > installed_ver {
                eprintln!(
                    "[Hermix] refreshing bundled skill {}: installed {:?} < bundled {:?}",
                    dest.display(),
                    installed_ver,
                    bundled_ver
                );
                std::fs::copy(&path, &dest)?;
                copied += 1;
            } else if !comparable {
                tracing::debug!(
                    path = %dest.display(),
                    "skill version not comparable — keeping the installed file"
                );
            }
        }
        // Non-SKILL.md files that already exist are treated as user-owned.
    }
    Ok(copied)
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

/// Parse a skill `version` for comparison: supports both the plain integer
/// form (`version: 1`, used by the include_str! builtins) and the dotted
/// semver-ish form the modelscope skills ship (`metadata: version: "1.0.0"`).
/// Returns `(major, minor, patch)`; an unparsable/missing version is `(0,0,0)`.
fn extract_skill_version_ord(content: &str) -> (u32, u32, u32) {
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
        return (0, 0, 0);
    };
    for line in content.lines().skip(s + 1).take(e.saturating_sub(s + 1)) {
        let Some(rest) = line.trim().strip_prefix("version:") else {
            continue;
        };
        let raw = rest.trim().trim_matches('"').trim_matches('\'');
        let mut parts = raw.split('.');
        let major = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        let minor = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        let patch = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        return (major, minor, patch);
    }
    (0, 0, 0)
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
    // ── 单实例守护（2026-10-25）────────────────────────────────
    // 必须放在一切副作用（Builder / sidecar / 全局热键 / viewer 拉起）
    // 之前：第二个进程在此直接退出，双开引发的服务端口抢占、热键冲突、
    // viewer 重复拉起从根上消失。机制见 single_instance.rs 模块注释。
    if !single_instance::acquire_or_notify() {
        return;
    }

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
    //
    // 2026-10-23：这里是 WebView2 浏览器参数的**唯一来源**。
    // 此前有三份：① 本环境变量 ② tauri.conf.json 的 additionalBrowserArgs
    // ③ 截图覆盖窗自己传的 additional_browser_args。三份不一致时 WebView2
    // 建窗直接失败 0x8007139F（ERROR_INVALID_STATE）——"截图打不开"的根因。
    // 同一 user data folder 下所有 webview 必须共享同一套浏览器参数，
    // 所以现在只留这一份：conf 里的 additionalBrowserArgs 与覆盖窗的
    // additional_browser_args 全部删除（见 2026-10-23 决策笔记）。
    //
    // `--disable-features` 只能出现一次：Chromium 取最后一次出现的值，
    // 分散写多个等于前面的全丢（此前 NetworkService 覆盖掉了前两项）。
    let webview_args = concat!(
        // ── 内存优化 ──
        "--disable-gpu-vsync",
        // ── 所有要关的 feature 写进同一个开关（多次出现只有最后一次生效）──
        "--disable-features=msSmartScreenProtection,EdgeFeed,CalculateNativeWinOcclusion,IntensiveWakeUpThrottling,RendererCodeIntegrity",
        "--disable-renderer-backgrounding",
        "--disable-background-timer-throttling",
        // ── 缓存优化（关键：默认无上限能涨到 GB） ──
        "--disk-cache-size=20971520",
        "--media-cache-size=2097152",
        // ── 性能优化 ──
        // 256MB 会让长会话 V8 频繁 full GC（全 UI 卡顿的主因之一），放宽到 1GB
        "--js-flags=--max-old-space-size=1024",
        "--no-pings",
        "--disable-background-networking",
        "--disable-component-update",
        "--disable-domain-reliability",
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
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        // 按快捷键分发：截图 / 文件搜索。
                        //
                        // **必须按"当前生效的热键"比对，不能写死 "alt+q"**：
                        // ① `HotKey` 的 Display 是 "alt+KeyQ" 而不是 "alt+Q"，
                        //    写死字符串的判断永远不成立 —— 热键注册成功、
                        //    按下去却什么都不发生（用户看到的就是"热键被占"）。
                        // ② 用户在设置里换了热键后，写死的判断同样不成立 ——
                        //    改完热键功能直接哑掉。
                        // 规范化比较统一在 hotkey.rs（详见那里的三条坑注记）。
                        let app = app.clone();
                        if screenshot_controller::hotkey_matches(shortcut) {
                            // spawn_blocking：抓屏 + 建窗走 GDI/IPC，不占主 UI 线程。
                            // 幕布 JPEG 编码已在 run_session 内起后台线程，主线程零阻塞。
                            tauri::async_runtime::spawn_blocking(move || {
                                if let Err(e) = screenshot_controller::run_session(&app) {
                                    eprintln!("[screenshot] 热键触发失败: {e}");
                                    let _ = app.emit("hermix:screenshot-error", e);
                                }
                            });
                        } else if file_search_controller::hotkey_matches(shortcut) {
                            // toggle 本身轻量（建窗/显隐），直接当前线程跑。
                            file_search_controller::toggle(&app);
                        }
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            // 单实例监听：第二个进程 SetEvent 过来时弹主窗（关到托盘的
            // 情况也能唤回）。见 single_instance.rs。
            single_instance::spawn_show_listener(app.handle().clone());
            // 在线更新检查：启动 30s 后首查 + 每 2h，失败静默（updater.rs）。
            updater::spawn_check_scheduler(app.handle().clone());
            // ── viewer 自动拉起（2026-10-25）───────────────────────────
            // 阿黎的浏览器/文件自动化依赖 viewer MCP（3911）。viewer 不是
            // sidecar，不随主程序拉起——用户不开查看器，阿黎第一个任务就
            // 卡在"查看器工具不在工具列表里"（实测）。这里启动 10s 后静默
            // spawn 它：exe 在主程序旁（安装版）或 workspace target（dev），
            // 失败完全静默（viewer 缺席时 server 的 MCP 重试会兜底，
            // 用户手动开查看器也能用）。
            //
            // **必须带 --hidden**（2026-10-25 用户定死的硬约束）：启动主程序
            // 绝不能弹出查看器窗口。--hidden 下 viewer 窗口保持隐藏、MCP
            // 照常就绪；用户打开文件时单实例投递会把窗口弹到前面。
            {
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(10));
                    let exe = match std::env::current_exe() {
                        Ok(p) => p.parent().map(|d| d.join("hermix-viewer.exe")),
                        _ => None,
                    }
                    .filter(|p| p.exists())
                    .or_else(|| {
                        // dev：workspace target 下找最新的（debug 优先）。
                        ["target/debug/hermix-viewer.exe", "target/release/hermix-viewer.exe"]
                            .iter()
                            .map(|rel| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                                .join("../../../").join(rel))
                            .find(|p| p.exists())
                    });
                    let Some(exe) = exe else { return };
                    use std::os::windows::process::CommandExt;
                    // DETACHED_PROCESS：viewer 有自己的窗口与生命周期，
                    // 不随主程序退出；也不需要控制台。
                    match std::process::Command::new(&exe)
                        .arg("--hidden")
                        .creation_flags(0x0000_0008)
                        .spawn()
                    {
                        Ok(child) => {
                            eprintln!("[viewer] 已自动拉起查看器（pid {}）。", child.id());
                        }
                        // 单实例互斥（viewer 自身）或其它启动期错误：静默。
                        Err(e) => {
                            eprintln!("[viewer] 自动拉起失败（静默忽略）：{e}");
                        }
                    }
                });
            }
            // ── N0 CDP 探针门控（批次 N 技术验证）──────────────────
            // HERMIX_CDP_PROBE=1 启动时，等主 webview 就绪后自动跑
            // Runtime.evaluate / DOMSnapshot / Accessibility 三个 CDP
            // 方法，结果打 stderr（带 [CDP-PROBE] 前缀），然后照常启动。
            // 无 GUI 交互即可拿到真机证据；不加该 env 时零开销。
            if std::env::var("HERMIX_CDP_PROBE").as_deref() == Ok("1") {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    // 等主窗口 + webview 就绪（首帧渲染后 CDP 才有页面可测）。
                    std::thread::sleep(std::time::Duration::from_secs(6));
                    // WebviewWindow 实现 AsRef<Webview>（webview_window.rs:1446），
                    // as_ref 转换后与 probe_via_tauri 的参数类型精确匹配。
                    let Some(webview_window) = handle.get_webview_window("main") else {
                        eprintln!("[CDP-PROBE] FAIL: 主窗口未就绪");
                        return;
                    };
                    let webview = webview_window.as_ref();
                    match cdp_probe::probe_via_tauri(
                        webview,
                        std::time::Duration::from_secs(60),
                    ) {
                        Ok(results) => {
                            eprintln!("[CDP-PROBE] === N0 真机探针结果 ===");
                            for (name, summary) in results {
                                eprintln!("[CDP-PROBE] {name}: {summary}");
                            }
                            eprintln!("[CDP-PROBE] === 完 ===");
                        }
                        Err(e) => eprintln!("[CDP-PROBE] FAIL: {e}"),
                    }
                });
            }

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

            // 截图全局热键（默认 Alt+Q）：setup 里注册兜底；设置 → 图像里
            // 的自定义配置由前端启动时经 screenshot_apply_config 回放覆盖。
            // 被别的程序占着就换备选键并通知前端（不再无声失效）。
            screenshot_controller::register_default_hotkey(app.handle());

            // 文件搜索全局热键（默认 Alt+F）：setup 兜底；搜索窗设置面板的
            // 自定义配置由前端经 file_search_apply_config 回放覆盖。
            file_search_controller::register_default_hotkey(app.handle());

            // ── 截图覆盖窗预热（2026-10-24）────────────────────────
            // 首次截图原本要付「建 WebView2 窗口 + 加载页面」的开销，
            // 用户感知是"按了热键，等一会儿才出画布"。这里在应用起来
            // 之后提前把窗建好，之后每次截图只剩「定位 + 显示 + 推帧」。
            //
            // 刻意延后 6s：两个 WebView2 抢 CPU 会拖慢首屏与 sidecar
            // 启动，而截图热键在启动头几秒几乎不会被按。
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(6));
                    screenshot_controller::prewarm(&handle);
                });
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
            // 内置子代理档案（explore / general-purpose），同样随二进制分发。
            ensure_builtin_agent_profiles(&hermix_home);
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

            // 1d. Tray restore. desktop.json 里开着就重建托盘图标；关窗→
            //     隐藏的拦截在 on_window_event（见下）。失败只打日志——
            //     托盘建不起来（如图标缺失）不该拦住整个启动。
            {
                let _ = app.manage(system_settings::TrayState::default());
                if system_settings::read_desktop_settings().tray {
                    if let Err(e) = system_settings::build_tray(app.handle()) {
                        eprintln!("[Hermix] 恢复托盘失败: {e}");
                    }
                }
            }

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
            match event {
                // 托盘模式的关窗去向：托盘存在 + 「关闭时最小化」开 → 只隐
                // 藏窗口（agent / 定时任务后台继续，托盘可再唤起）；否则走
                // 原生销毁。splash 的 sp-close 走同一条 close()，被同样拦
                // 下——启动早期隐藏意味着应用继续在后台起服务，托盘可恢复。
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    let app = window.app_handle();
                    if window.label() == "main"
                        && app.tray_by_id(system_settings::TRAY_ID).is_some()
                        && system_settings::read_desktop_settings().close_to_tray
                    {
                        api.prevent_close();
                        let _ = window.hide();
                        system_settings::notify_hidden_once(app);
                    }
                }
                tauri::WindowEvent::Destroyed => {
                    let state = window.state::<AppState>();
                    let mut sb = state.sidecar.lock().unwrap();
                    sb.stop();
                }
                _ => {}
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
            updater::update_check,
            updater::update_download,
            updater::update_install,
            system_settings::get_autostart,
            system_settings::set_autostart,
            system_settings::send_notification,
            system_settings::sync_tray,
            system_settings::get_desktop_settings,
            system_settings::set_close_action,
            system_settings::tray_set_task_count,
            keep_awake::set_keep_awake,
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
            commands::open_in_viewer,
            commands::file_assoc_list,
            commands::file_assoc_set,
            screenshot_controller::start_screenshot,
            screenshot_controller::screenshot_session_info,
            screenshot_controller::screenshot_select,
            screenshot_controller::screenshot_overlay_ready,
            screenshot_controller::screenshot_backdrop_hide,
            screenshot_controller::screenshot_commit_edited,
            screenshot_controller::screenshot_window_at,
            screenshot_controller::screenshot_cancel,
            screenshot_controller::screenshot_apply_config,
            screenshot_controller::screenshot_hotkey_status,
            file_search_controller::file_search_apply_config,
            file_search_controller::file_search_hotkey_status,
            cdp_probe::cdp_n0_probe,
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
            // Custom app management。list/package/run/open_dir 的 HTTP 面
            // （/api/custom-apps*）已被前端全面采用（lib/customAppApi.ts），
            // 这四个 IPC 命令零调用，2026-10-18 移除注册；validate /
            // smoke_test 仍由前端直接 invoke，保留。
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
            browser_panel_open,
            browser_panel_close,
        ])
        .run(tauri::generate_context!())
        .expect("error while running hermix-desktop");
}

