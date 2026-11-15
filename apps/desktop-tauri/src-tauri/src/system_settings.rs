//! 系统级设置命令（L4 桌面通知 / 开机自启 / 托盘，L3.6 壁纸）。
//!
//! 这几项此前没有实现，不是后端能力缺失，而是既没人加插件依赖、也没有
//! 前端入口。集中放在一个模块里，是因为它们共享同一条约束：**都是本机
//! 副作用，不是数据操作**。失败一律回 `Err(中文串)`，前端据此提示，
//! 绝不静默吞掉——「开机自启看起来开了但没生效」这类问题最难查。
//!
//! 托盘模型（2026-10-14 重做）：托盘存在 + 「关闭时最小化」勾选 → 点主窗
//! X 只隐藏窗口（agent / 定时任务后台继续）；任一不满足则点 X 走原生销毁。
//! 两个开关持久化在 `HERMIX_HOME/desktop.json`，启动时恢复——Rust（关窗
//! 拦截）与前端（设置页回显）都要读同一份，不能各存一处。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager};

pub const TRAY_ID: &str = "hermix-main";


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

/// 桌面层 UI 行为开关（托盘 + 关窗去向）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct DesktopSettings {
    /// 托盘常驻图标是否存在。
    pub tray: bool,
    /// 主窗点 X 时：true = 隐藏到托盘，false = 直接退出进程。
    pub close_to_tray: bool,
}

impl Default for DesktopSettings {
    fn default() -> Self {
        // 默认全开：本产品的任务（agent 回合 / 定时任务）在后台跑，关窗即杀
        // 进程会中断它们。想改回「点 X 退出」的用户走托盘菜单或设置页。
        Self { tray: true, close_to_tray: true }
    }
}

/// 数据根与 lib.rs 的 fallback 同一逻辑：env（dev 注入）→ `~/.hermix`。
fn hermix_home() -> PathBuf {
    std::env::var("HERMIX_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".hermix")
        })
}

/// 解析失败按默认处理：设置文件损坏不该拦住「托盘在不在」这种基础行为。
pub fn read_desktop_settings() -> DesktopSettings {
    std::fs::read_to_string(hermix_home().join("desktop.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_desktop_settings(s: &DesktopSettings) -> Result<(), String> {
    let path = hermix_home().join("desktop.json");
    let body = serde_json::to_string_pretty(s).map_err(|e| format!("序列化设置失败：{e}"))?;
    std::fs::write(&path, body).map_err(|e| format!("写设置文件失败：{e}"))
}

/// 托盘菜单里需要动态更新的项。建托盘时存进 managed state，command 与
/// 菜单事件里拿引用改文本 / 勾选态；托盘移除时置 None。
struct TrayMenuHandles {
    tasks: MenuItem<tauri::Wry>,
    close_to_tray: CheckMenuItem<tauri::Wry>,
}

#[derive(Default)]
pub struct TrayState(Mutex<Option<TrayMenuHandles>>);

/// 设置页回显用：tray 字段实读（启动恢复可能失败，文件说了不算），
/// 关窗行为只有 desktop.json 一份真相。
#[tauri::command]
pub fn get_desktop_settings(app: AppHandle) -> Result<DesktopSettings, String> {
    let mut s = read_desktop_settings();
    s.tray = app.tray_by_id(TRAY_ID).is_some();
    Ok(s)
}

/// 创建 / 移除托盘图标，并持久化开关（重启后恢复）。
#[tauri::command]
pub fn sync_tray(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut s = read_desktop_settings();
    s.tray = enabled;
    // 先落盘再动图标：文件写不进去 = 重启后开关回弹，必须报错暴露。
    write_desktop_settings(&s)?;
    if enabled {
        build_tray(&app)
    } else {
        remove_tray(&app);
        Ok(())
    }
}

/// 切换「点 X 最小化到托盘 / 直接退出」。托盘菜单里的同名勾选项同步。
#[tauri::command]
pub fn set_close_action(app: AppHandle, close_to_tray: bool) -> Result<(), String> {
    let mut s = read_desktop_settings();
    s.close_to_tray = close_to_tray;
    write_desktop_settings(&s)?;
    if let Some(st) = app.try_state::<TrayState>() {
        if let Some(h) = st.0.lock().unwrap().as_ref() {
            let _ = h.close_to_tray.set_checked(close_to_tray);
        }
    }
    Ok(())
}

/// 前端上报正在流式执行的会话数，驱动托盘菜单「正在执行的任务」项的
/// 文本与可用态。托盘不存在时 no-op（设置页关掉托盘后调用是正常的）。
#[tauri::command]
pub fn tray_set_task_count(app: AppHandle, count: u32) -> Result<(), String> {
    let Some(st) = app.try_state::<TrayState>() else {
        return Ok(());
    };
    let guard = st.0.lock().unwrap();
    let Some(h) = guard.as_ref() else {
        return Ok(());
    };
    let (text, enabled) = if count == 0 {
        ("没有正在执行的任务".to_string(), false)
    } else {
        (format!("正在执行的任务（{count} 个）"), true)
    };
    h.tasks
        .set_text(text)
        .map_err(|e| format!("更新托盘菜单失败：{e}"))?;
    let _ = h.tasks.set_enabled(enabled);
    Ok(())
}

/// 首次隐藏到托盘时发一条系统通知——否则「点了关闭程序却没退」会被当成
/// 僵尸进程。每进程只提醒一次，之后的隐藏是用户明知的选择。
static HIDDEN_NOTIFIED: AtomicBool = AtomicBool::new(false);

pub fn notify_hidden_once(app: &AppHandle) {
    if HIDDEN_NOTIFIED.swap(true, Ordering::Relaxed) {
        return;
    }
    let _ = send_notification(
        app.clone(),
        "Hermix 仍在后台运行".into(),
        "窗口已最小化到系统托盘，任务继续执行；右键托盘图标可选择退出。".into(),
    );
}

fn remove_tray(app: &AppHandle) {
    app.remove_tray_by_id(TRAY_ID);
    if let Some(st) = app.try_state::<TrayState>() {
        *st.0.lock().unwrap() = None;
    }
}

fn show_main_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

fn show_about_dialog(app: &AppHandle) {
    use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
    let version = app.config().version.clone().unwrap_or_default();
    app.dialog()
        .message(format!(
            "Hermix Desktop v{version}\n\n便携式 AI 助手：数据随程序目录走，不写系统盘。\n\nhermix.cssso.com"
        ))
        .title("关于 Hermix Desktop")
        .kind(MessageDialogKind::Info)
        .show(|_| {});
}

/// 建托盘（重建语义：先删旧的，勾选态与 desktop.json 对齐）。
/// 菜单项：显示主窗 / 任务 / 更新 / 关于 / 关闭行为勾选 / 退出。
/// 任务与更新两项只负责唤起主窗 + emit 事件——列表与检查逻辑都在前端
/// （前端本来就持有 streaming / updater 状态），托盘不另起一条数据链。
pub fn build_tray(app: &AppHandle) -> Result<(), String> {
    app.remove_tray_by_id(TRAY_ID);
    let s = read_desktop_settings();

    let mk = |e: tauri::Error| format!("创建托盘菜单项失败：{e}");
    let show_i = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>).map_err(mk)?;
    // 任务项初始按「无任务」渲染，前端 mount 后立即上报真实计数覆盖。
    let tasks_i =
        MenuItem::with_id(app, "tasks", "没有正在执行的任务", false, None::<&str>).map_err(mk)?;
    let update_i = MenuItem::with_id(app, "update", "检查更新", true, None::<&str>).map_err(mk)?;
    let about_i =
        MenuItem::with_id(app, "about", "关于 Hermix Desktop", true, None::<&str>).map_err(mk)?;
    let close_i = CheckMenuItem::with_id(
        app,
        "close_to_tray",
        "点击关闭按钮时最小化到托盘",
        true,
        s.close_to_tray,
        None::<&str>,
    )
    .map_err(mk)?;
    let quit_i = MenuItem::with_id(app, "quit", "退出", true, None::<&str>).map_err(mk)?;
    let sep = PredefinedMenuItem::separator(app).map_err(|e| e.to_string())?;
    let menu = Menu::with_items(
        app,
        &[&show_i, &sep, &tasks_i, &update_i, &about_i, &sep, &close_i, &quit_i],
    )
    .map_err(mk)?;

    // Tauri 2.12 的 menu / tray builder 一律收 `&M`（M: Manager），不收
    // `AppHandle` 本身——传值编译不过，别照抄旧版示例。
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(app.default_window_icon().cloned().ok_or(
            "没有可用的窗口图标，托盘无法创建".to_string(),
        )?)
        .tooltip("Hermix Desktop")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(handle_menu_event)
        .on_tray_icon_event(|tray, event| {
            // 左键单击唤起主窗：托盘最常用的一次点击。
            if let tauri::tray::TrayIconEvent::Click {
                button: tauri::tray::MouseButton::Left,
                button_state: tauri::tray::MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)
        .map_err(|e| format!("创建托盘图标失败：{e}"))?;

    // manage 幂等（已注册不替换），所以首次注册空壳、之后原地写入。
    if app.try_state::<TrayState>().is_none() {
        app.manage(TrayState(Mutex::new(None)));
    }
    *app.state::<TrayState>().0.lock().unwrap() = Some(TrayMenuHandles {
        tasks: tasks_i,
        close_to_tray: close_i,
    });
    Ok(())
}

fn handle_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    match event.id().as_ref() {
        "show" => show_main_window(app),
        "tasks" => {
            show_main_window(app);
            let _ = app.emit("hermix:tray-open-tasks", ());
        }
        "update" => {
            show_main_window(app);
            let _ = app.emit("hermix:tray-check-update", ());
        }
        "about" => show_about_dialog(app),
        "close_to_tray" => {
            // 点击后系统已翻转勾选态；把新值落盘。
            let checked = app
                .try_state::<TrayState>()
                .and_then(|st| {
                    st.0.lock()
                        .unwrap()
                        .as_ref()
                        .map(|h| h.close_to_tray.is_checked().unwrap_or(true))
                })
                .unwrap_or(true);
            let mut s = read_desktop_settings();
            s.close_to_tray = checked;
            let _ = write_desktop_settings(&s);
        }
        "quit" => {
            // 清理收口在 `shutdown::cleanup`（sidecar + 索引守护进程），
            // 与 `RunEvent::Exit` 共用同一份实现；"先清理再退"合成
            // `cleanup_and_exit`，与主窗销毁路径同一入口。
            crate::shutdown::cleanup_and_exit(app, "tray-quit");
        }
        _ => {}
    }
}
