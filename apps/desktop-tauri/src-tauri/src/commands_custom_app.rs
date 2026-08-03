//! Tauri IPC commands for custom app management.
//!
//! These commands provide a bridge between the frontend and the
//! PluginManager. They handle:
//! - Initializing the PluginManager with the loader path
//! - Opening packaged apps in standalone windows (spawned dist exe)

use std::path::PathBuf;
use std::sync::Arc;

use hermix_config::paths::hermix_home;
use hermix_plugin::{
    node_setup, resolve_hx_shell_path, resolve_webview2_loader_path,
    run_smoke_test_async, validate_custom_app,
    PackageMode, PluginManager,
};

/// Application state holding the PluginManager.
pub struct CustomAppState {
    pub manager: Arc<PluginManager>,
}

/// Resolve the custom apps directory.
fn resolve_apps_dir() -> PathBuf {
    hermix_home().join("custom-apps")
}

/// Initialize the custom app system. Called once at startup.
#[tauri::command]
pub async fn custom_app_init() -> Result<CustomAppInitResult, String> {
    let loader_path = resolve_hx_shell_path();
    let webview2_dll = resolve_webview2_loader_path();
    let apps_dir = resolve_apps_dir();

    // Ensure the apps directory exists
    std::fs::create_dir_all(&apps_dir)
        .map_err(|e| format!("创建应用目录失败: {e}"))?;

    // hx-shell 架构: 不需要 Node/neu CLI，只检查模板壳 + WebView2Loader 是否就位
    let hx_shell_ready = loader_path.exists();
    let webview2_ready = webview2_dll.exists();
    let env_status = node_setup::get_environment_status();
    let neu_build_ready = env_status["neu_build_ready"].as_bool().unwrap_or(false);

    Ok(CustomAppInitResult {
        loader_path: loader_path.to_string_lossy().to_string(),
        loader_exists: hx_shell_ready,
        apps_dir: apps_dir.to_string_lossy().to_string(),
        neu_build_ready,
        node_version: env_status["node"].as_str().map(|s| s.to_string()),
        neu_available: env_status["neu_available"].as_bool().unwrap_or(false),
        env_message: if !hx_shell_ready {
            format!("hx-shell 模板未就位: {} (请将 hx-shell.exe 部署到 hermix loader 目录)", loader_path.display())
        } else if !webview2_ready {
            format!("WebView2Loader.dll 缺失: {}", webview2_dll.display())
        } else {
            "✓ hx-shell 模板 + WebView2 就绪".to_string()
        },
    })
}

#[derive(serde::Serialize)]
pub struct CustomAppInitResult {
    pub loader_path: String,
    pub loader_exists: bool,
    pub apps_dir: String,
    pub neu_build_ready: bool,
    pub node_version: Option<String>,
    pub neu_available: bool,
    pub env_message: String,
}

/// Scan all custom apps and return their summaries.
#[tauri::command]
pub async fn custom_app_list() -> Result<serde_json::Value, String> {
    let apps_dir = resolve_apps_dir();

    let manager = PluginManager::new(apps_dir);
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
/// `build_mode` can be "sidecar" (default) or "neu-build".
#[tauri::command]
pub async fn custom_app_package(
    id: String,
    build_mode: Option<String>,
) -> Result<serde_json::Value, String> {
    let apps_dir = resolve_apps_dir();

    let manager = PluginManager::new(apps_dir);
    manager
        .scan_plugins()
        .map_err(|e| format!("扫描应用失败: {e}"))?;

    let mode = match build_mode.as_deref().unwrap_or("sidecar") {
        "neu-build" | "neubuild" | "neu" => PackageMode::NeuBuild,
        _ => PackageMode::Sidecar,
    };

    let exe_path = manager
        .package_plugin_with_mode(&id, mode)
        .await
        .map_err(|e| format!("打包应用失败: {e}"))?;

    Ok(serde_json::json!({
        "success": true,
        "message": format!("应用 '{id}' 打包成功 ({:?} 模式)", mode),
        "exe_path": exe_path.to_string_lossy(),
        "build_mode": format!("{:?}", mode).to_lowercase(),
    }))
}

/// Run a custom app (build latest source then launch).
#[tauri::command]
pub async fn custom_app_run(id: String) -> Result<serde_json::Value, String> {
    let apps_dir = resolve_apps_dir();

    let manager = PluginManager::new(apps_dir);
    manager
        .scan_plugins()
        .map_err(|e| format!("扫描应用失败: {e}"))?;

    // 先编译最新源码，确保运行的是最新代码（测试场景：改完即看）
    manager
        .package_plugin(&id)
        .await
        .map_err(|e| format!("编译应用失败: {e}"))?;

    // 启动已打包的 exe (Neutralinojs sidecar 产物; 独立进程)
    manager
        .run_plugin(&id)
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

/// 开发期自验证: 在打包/运行前校验应用源码, 把打包期错误提前到开发期暴露。
///
/// 校验内容 (见 `hermix_plugin::validate_custom_app`):
/// - manifest 完整性 + HTML 结构 + Neutralino API 调用 + 权限一致性 + JS 括号匹配
///
/// 返回结构化报告 `{ ok, issues: [{level, code, message}] }`。
/// agent 开发循环应在生成/修改应用代码后调用本命令, `ok == false` 时继续
/// 修复直至通过, 才视为「开发完成」, 从而让用户只测功能、不测打包/运行错误。
#[tauri::command]
pub async fn custom_app_validate(id: String) -> Result<serde_json::Value, String> {
    let apps_dir = resolve_apps_dir();
    let app_dir = apps_dir.join(&id);
    if !app_dir.exists() {
        return Err(format!("应用目录不存在: {}", app_dir.display()));
    }
    let report = validate_custom_app(&app_dir);
    serde_json::to_value(&report).map_err(|e| format!("序列化校验报告失败: {e}"))
}

/// 运行时冒烟测试: 先刷新打包产物, 再启动 neutralino exe 并捕获错误。
///
/// 返回结构化报告 `{ ok, ready, packaged, errors: [{message}], detail }`。
#[tauri::command]
pub async fn custom_app_smoke_test(id: String) -> Result<serde_json::Value, String> {
    let apps_dir = resolve_apps_dir();
    let app_dir = apps_dir.join(&id);
    if !app_dir.exists() {
        return Err(format!("应用目录不存在: {}", app_dir.display()));
    }

    let manager = PluginManager::new(apps_dir);
    manager
        .scan_plugins()
        .map_err(|e| format!("扫描应用失败: {e}"))?;

    // 刷新打包产物 (sidecar 模式)
    manager
        .package_plugin(&id)
        .await
        .map_err(|e| format!("编译应用失败: {e}"))?;

    let report = run_smoke_test_async(app_dir).await;
    serde_json::to_value(&report).map_err(|e| format!("序列化冒烟报告失败: {e}"))
}

/// Check the neu build environment (Node.js + neu CLI availability).
/// Returns a JSON object with environment status info.
#[tauri::command]
pub async fn custom_app_check_env() -> Result<serde_json::Value, String> {
    Ok(node_setup::get_environment_status())
}

/// Install the @neutralinojs/neu CLI globally via npm.
#[tauri::command]
pub async fn custom_app_install_neu() -> Result<serde_json::Value, String> {
    match node_setup::install_neu_cli() {
        Ok(msg) => Ok(serde_json::json!({
            "success": true,
            "message": msg,
        })),
        Err(e) => Ok(serde_json::json!({
            "success": false,
            "error": e,
        })),
    }
}
