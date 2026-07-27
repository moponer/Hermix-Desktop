//! Tauri IPC commands for custom app management.
//!
//! These commands provide a bridge between the frontend and the
//! PluginManager. They handle:
//! - Initializing the PluginManager with the correct paths
//! - Providing shell and esbuild paths to the frontend
//! - Opening packaged apps in standalone windows

use std::path::PathBuf;
use std::sync::Arc;

use hermix_config::paths::hermix_home;
use hermix_plugin::{PluginManager, PluginWindowManager};

/// Application state holding the PluginManager.
pub struct CustomAppState {
    pub manager: Arc<PluginManager>,
}

/// Resolve the path to the precompiled app shell.
///
/// Priority:
///   1. `HERMIX_HOME/shell/app-shell.exe` — extracted on first launch by
///      `ensure_app_shell_and_esbuild` (lib.rs setup).
///   2. `current_exe/resources/shell/app-shell.exe` — portable/installed
///      build where the bundler placed the shell next to the exe.
///   3. `current_exe/shell/app-shell.exe` — fallback for adjacent layout.
///   4. `HERMIX_HOME/shell/app-shell.exe` — final default (may not exist;
///      the packager reports a clear `ShellNotFound` error in that case).
///
/// Uses the canonical `hermix_home()` (same source as the rest of the app)
/// rather than reading `HERMIX_HOME` directly, so portable-data-dir and
/// env-override precedence are always correct.
fn resolve_shell_path() -> PathBuf {
    let home = hermix_home();

    let candidates = [
        Some(home.join("shell").join("app-shell.exe")),
        std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.join("resources").join("shell").join("app-shell.exe"))),
        std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.join("shell").join("app-shell.exe"))),
    ]
    .into_iter()
    .flatten();

    for c in candidates {
        if c.exists() {
            return c;
        }
    }

    home.join("shell").join("app-shell.exe")
}

/// Resolve the path to the bundled esbuild executable.
fn resolve_esbuild_path() -> PathBuf {
    let home = hermix_home();

    let candidates = [
        Some(home.join("esbuild").join("esbuild.exe")),
        std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.join("resources").join("esbuild").join("esbuild.exe"))),
        std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.join("esbuild").join("esbuild.exe"))),
    ]
    .into_iter()
    .flatten();

    for c in candidates {
        if c.exists() {
            return c;
        }
    }

    home.join("esbuild").join("esbuild.exe")
}

/// Resolve the custom apps directory.
fn resolve_apps_dir() -> PathBuf {
    hermix_home().join("custom-apps")
}

/// Initialize the custom app system. Called once at startup.
#[tauri::command]
pub async fn custom_app_init() -> Result<CustomAppInitResult, String> {
    let shell_path = resolve_shell_path();
    let esbuild_path = resolve_esbuild_path();
    let apps_dir = resolve_apps_dir();

    // Ensure the apps directory exists
    std::fs::create_dir_all(&apps_dir)
        .map_err(|e| format!("创建应用目录失败: {e}"))?;

    Ok(CustomAppInitResult {
        shell_path: shell_path.to_string_lossy().to_string(),
        shell_exists: shell_path.exists(),
        esbuild_path: esbuild_path.to_string_lossy().to_string(),
        esbuild_exists: esbuild_path.exists(),
        apps_dir: apps_dir.to_string_lossy().to_string(),
    })
}

#[derive(serde::Serialize)]
pub struct CustomAppInitResult {
    pub shell_path: String,
    pub shell_exists: bool,
    pub esbuild_path: String,
    pub esbuild_exists: bool,
    pub apps_dir: String,
}

/// Scan all custom apps and return their summaries.
#[tauri::command]
pub async fn custom_app_list() -> Result<serde_json::Value, String> {
    let shell_path = resolve_shell_path();
    let esbuild_path = resolve_esbuild_path();
    let apps_dir = resolve_apps_dir();

    let manager = PluginManager::new(apps_dir, shell_path, esbuild_path);
    let entries = manager
        .scan_plugins()
        .map_err(|e| format!("扫描应用失败: {e}"))?;

    let summaries: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            serde_json::json!({
                "id": e.manifest.id,
                "name": e.manifest.name,
                "version": e.manifest.version,
                "description": e.manifest.description,
                "icon": e.manifest.icon,
                "category": e.manifest.category,
                "author": e.manifest.author,
                "is_packaged": e.dir.join("dist").exists(),
            })
        })
        .collect();

    Ok(serde_json::json!({
        "success": true,
        "apps": summaries,
        "count": summaries.len(),
    }))
}

/// Package a custom app as a standalone EXE.
#[tauri::command]
pub async fn custom_app_package(id: String) -> Result<serde_json::Value, String> {
    let shell_path = resolve_shell_path();
    let esbuild_path = resolve_esbuild_path();
    let apps_dir = resolve_apps_dir();

    let manager = PluginManager::new(apps_dir, shell_path, esbuild_path);
    manager
        .scan_plugins()
        .map_err(|e| format!("扫描应用失败: {e}"))?;

    let exe_path = manager
        .package_plugin(&id)
        .await
        .map_err(|e| format!("打包应用失败: {e}"))?;

    Ok(serde_json::json!({
        "success": true,
        "message": format!("应用 '{id}' 打包成功"),
        "exe_path": exe_path.to_string_lossy(),
    }))
}

/// Run a custom app (build latest source then launch).
#[tauri::command]
pub async fn custom_app_run(id: String) -> Result<serde_json::Value, String> {
    let shell_path = resolve_shell_path();
    let esbuild_path = resolve_esbuild_path();
    let apps_dir = resolve_apps_dir();

    let manager = PluginManager::new(apps_dir, shell_path.clone(), esbuild_path);
    manager
        .scan_plugins()
        .map_err(|e| format!("扫描应用失败: {e}"))?;

    let entry = manager
        .get_plugin(&id)
        .ok_or_else(|| format!("应用不存在: {id}"))?;

    // 先编译最新源码，确保运行的是最新代码（测试场景：改完即看）
    manager
        .package_plugin(&id)
        .await
        .map_err(|e| format!("编译应用失败: {e}"))?;

    let wm = PluginWindowManager::new(shell_path);
    wm.open_plugin(&entry)
        .await
        .map_err(|e| format!("启动应用失败: {e}"))?;

    Ok(serde_json::json!({
        "success": true,
        "message": format!("应用 '{id}' 已启动"),
    }))
}

/// Open a file explorer at the app's directory.
#[tauri::command]
pub async fn custom_app_open_dir(id: String) -> Result<(), String> {
    let apps_dir = resolve_apps_dir();
    let app_dir = apps_dir.join(&id);

    if !app_dir.exists() {
        return Err(format!("应用目录不存在: {}", app_dir.display()));
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(&app_dir)
            .spawn()
            .map_err(|e| format!("打开资源管理器失败: {e}"))?;
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&app_dir)
            .spawn()
            .map_err(|e| format!("打开 Finder 失败: {e}"))?;
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(&app_dir)
            .spawn()
            .map_err(|e| format!("打开文件管理器失败: {e}"))?;
    }

    Ok(())
}
