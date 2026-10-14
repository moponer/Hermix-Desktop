//! 系统级设置命令（L4 桌面通知 / 开机自启 / 托盘，L3.6 壁纸）。
//!
//! 这四项此前没有实现，不是后端能力缺失，而是既没人加插件依赖、也没有
//! 前端入口。集中放在一个模块里，是因为它们共享同一条约束：**都是本机
//! 副作用，不是数据操作**。失败一律回 `Err(中文串)`，前端据此提示，
//! 绝不静默吞掉——「开机自启看起来开了但没生效」这类问题最难查。

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager};


// ── 开机自启动 ────────────────────────────────────────────────────

/// 当前是否已注册开机自启。
#[tauri::command]
pub fn get_autostart(app: AppHandle) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch()
        .is_enabled()
        .map_err(|e| format!("读取开机自启状态失败：{e}"))
}

/// 开启 / 关闭开机自启。Windows 下写注册表 `Run` 键。
#[tauri::command]
pub fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let launcher = app.autolaunch();
    let r = if enabled {
        launcher.enable()
    } else {
        launcher.disable()
    };
    r.map_err(|e| format!("{}开机自启失败：{e}", if enabled { "开启" } else { "关闭" }))
}

// ── 桌面通知 ──────────────────────────────────────────────────────

/// 弹一条系统通知。设置页的「测试通知」与 agent 完成回报共用这一个入口。
#[tauri::command]
pub fn send_notification(app: AppHandle, title: String, body: String) -> Result<(), String> {
    use tauri_plugin_notification::NotificationExt;
    app.notification()
        .builder()
        .title(&title)
        .body(&body)
        .show()
        .map_err(|e| format!("发送系统通知失败：{e}"))
}

// ── 托盘 ──────────────────────────────────────────────────────────

/// 托盘开关状态。托盘图标本身由 `sync_tray` 决定是否创建，这里只回读。
#[tauri::command]
pub fn get_tray_enabled(app: AppHandle) -> Result<bool, String> {
    Ok(app.tray_by_id("hermix-main").is_some())
}

/// 创建 / 移除托盘图标。
///
/// 菜单只给两项：显示主窗、退出。**刻意不给设置项**——托盘上的设置入口
/// 会让人分不清"托盘里的设置"和"主窗里的设置"是哪一份状态。设置只在
/// 主窗里改，改完立刻反映到托盘的存在与否。
#[tauri::command]
pub fn sync_tray(app: AppHandle, enabled: bool) -> Result<(), String> {
    let existing = app.tray_by_id("hermix-main");
    if enabled && existing.is_some() {
        return Ok(());
    }
    if !enabled {
        if existing.is_some() {
            app.remove_tray_by_id("hermix-main");
        }
        return Ok(());
    }

    // Tauri 2.12 的 menu / tray builder 一律收 `&M`（M: Manager），不收
    // `AppHandle` 本身——传值编译不过，别照抄旧版示例。
    let show_i = MenuItem::with_id(&app, "show", "显示主窗口", true, None::<&str>)
        .map_err(|e| format!("创建托盘菜单失败：{e}"))?;
    let quit_i = MenuItem::with_id(&app, "quit", "退出", true, None::<&str>)
        .map_err(|e| format!("创建托盘菜单失败：{e}"))?;
    let sep = PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?;
    let menu = Menu::with_items(&app, &[&show_i, &sep, &quit_i])
        .map_err(|e| format!("创建托盘菜单失败：{e}"))?;

    TrayIconBuilder::with_id("hermix-main")
        .icon(app.default_window_icon().cloned().ok_or(
            "没有可用的窗口图标，托盘无法创建".to_string(),
        )?)
        .tooltip("Hermix Desktop")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => {
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.show();
                    let _ = win.unminimize();
                    let _ = win.set_focus();
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // 左键单击唤起主窗：托盘最常用的一次点击。
            if let tauri::tray::TrayIconEvent::Click {
                button: tauri::tray::MouseButton::Left,
                button_state: tauri::tray::MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.show();
                    let _ = win.set_focus();
                }
            }
        })
        .build(&app)
        .map_err(|e| format!("创建托盘图标失败：{e}"))?;

    Ok(())
}
