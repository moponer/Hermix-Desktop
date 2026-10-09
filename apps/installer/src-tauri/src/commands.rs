//! 安装器 Tauri 命令集。
//!
//! 命令必须住在独立模块、generate_handler 走路径引用——定义在 lib.rs（crate
//! 根）会触发 E0255：`#[tauri::command]` 生成的 `__cmd__*` 宏与 generate_handler
//! 展开的同名 re-import 在同一作用域冲突（desktop-tauri 同款组织方式）。

use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use std::path::PathBuf;
use std::sync::Mutex;

/// 安装会话状态：install 成功后写入 install_dir，finish 共用。
#[derive(Default)]
pub struct InstallState {
    pub install_dir: Mutex<Option<PathBuf>>,
}

#[derive(Serialize)]
pub struct InstallerMeta {
    pub version: String,
    pub default_dir: String,
}

/// 产品版本：release 脚本经 HERMIX_PRODUCT_VERSION 注入（build.rs 转发）；
/// 未注入（dev）回落安装器自身版本。
fn product_version() -> String {
    let pv = env!("HERMIX_PRODUCT_VERSION");
    if pv.is_empty() {
        env!("CARGO_PKG_VERSION").into()
    } else {
        pv.into()
    }
}

fn default_install_dir() -> String {
    let local = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| "C:\\Users\\Public".into());
    PathBuf::from(local)
        .join("Programs")
        .join("Hermix")
        .to_string_lossy()
        .to_string()
}

/// 元信息：版本 + 默认目录（前端目录页初始值）。
#[tauri::command]
pub fn installer_meta() -> Result<InstallerMeta, String> {
    Ok(InstallerMeta {
        version: product_version(),
        default_dir: default_install_dir(),
    })
}

/// 许可文本：优先读 exe 旁 license.rtf；缺失时前端回落固定文案。
/// rtf 直接展示原文（安装器一期不渲染富文本，纯文本可读性足够）。
#[tauri::command]
pub fn installer_license() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let rtf = exe
        .parent()
        .map(|d| d.join("license.rtf"))
        .ok_or("no exe dir")?;
    std::fs::read_to_string(rtf).map_err(|e| format!("license.rtf 读取失败: {e}"))
}

/// 原生目录选择对话框（rfd）。**必须 async + spawn_blocking**：sync 命令跑在
/// 主线程，模态对话框期间向导窗口会失去响应（同 restart_sidecar 教训）。
#[tauri::command]
pub async fn installer_pick_dir() -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let picked = rfd::FileDialog::new()
            .set_title("选择安装目录")
            .pick_folder();
        Ok(picked.map(|p| p.to_string_lossy().to_string()))
    })
    .await
    .map_err(|e| format!("目录选择任务崩溃: {e}"))?
}

/// 执行安装：提取载荷 → 快捷方式 → 卸载登记。
/// 进度经 `installer://progress` event 发 `{phase, received, total}`。
/// **必须 async + spawn_blocking**：sync 命令跑在主线程，解压数百 MB 会把
/// 向导 UI 卡死（restart_sidecar 同款教训）。
#[tauri::command]
pub async fn installer_install(
    app: AppHandle,
    state: State<'_, InstallState>,
    install_dir: String,
    desktop_shortcut: bool,
) -> Result<(), String> {
    let dir = PathBuf::from(install_dir.trim());
    if dir.as_os_str().is_empty() {
        return Err("安装目录不能为空".into());
    }
    // Program Files 警告已在 UI 层给过；这里不拦（用户自担更新提权，笔记有记）。
    let handle = app.clone();
    let dir2 = dir.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::payload::install_to(&handle, &dir2, desktop_shortcut)
    })
    .await
    .map_err(|e| format!("安装任务崩溃: {e}"))??;
    *state.install_dir.lock().unwrap() = Some(dir);
    Ok(())
}

/// 完成页「完成」：可选启动 Hermix 后退出安装器。
#[tauri::command]
pub fn installer_finish(
    app: AppHandle,
    state: State<'_, InstallState>,
    run_now: bool,
) -> Result<(), String> {
    if run_now {
        let target = state
            .install_dir
            .lock()
            .unwrap()
            .clone()
            .map(|d| d.join("hermix.exe"))
            .filter(|p| p.exists());
        if let Some(t) = target {
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                std::process::Command::new(&t)
                    .creation_flags(CREATE_NO_WINDOW)
                    .spawn()
                    .map_err(|e| format!("启动 Hermix 失败: {e}"))?;
            }
            #[cfg(not(windows))]
            {
                let _ = t;
            }
        }
    }
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.close();
    }
    Ok(())
}
