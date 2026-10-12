//! 系统级设置命令（L4 桌面通知 / 开机自启 / 托盘，L3.6 壁纸）。
//!
//! 这四项此前没有实现，不是后端能力缺失，而是既没人加插件依赖、也没有
//! 前端入口。集中放在一个模块里，是因为它们共享同一条约束：**都是本机
//! 副作用，不是数据操作**。失败一律回 `Err(中文串)`，前端据此提示，
//! 绝不静默吞掉——「开机自启看起来开了但没生效」这类问题最难查。

use std::path::PathBuf;

use serde::Serialize;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager};

use crate::commands::resolve_data_dir;

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

// ── 壁纸（L3.6，2026-10-12 升级：整窗平铺 + 鲁棒加载）─────────────

#[derive(Serialize)]
pub struct WallpaperInfo {
    /// 数据目录内的文件名（含扩展名），前端据此拼 asset URL。
    file: String,
    /// 原始来源路径，仅供设置页展示"这张图来自哪"。
    source: String,
    /// base64 data URL（含 MIME 前缀）。前端直接 `url(...)` 渲染，**不依赖
    /// Tauri asset 协议作用域**——之前靠 `convertFileSrc` + `assetProtocol`
    /// 在 Windows 上 `$HOME` 解析失配，导致资产 URL 加载失败、预览与生效
    /// 双双空白。data URL 内联进 CSS 变量，零作用域、零 CORS。
    data_url: String,
}

/// 把壁纸文件读成 base64 data URL。MIME 由扩展名推断（webview 靠扩展名判
/// 类型，无扩展名会被当未知类型拒绝渲染——这就是早期"设了壁纸但没变化"
/// 的另一个成因，这里在编码端就钉死 MIME）。
fn wallpaper_data_url(path: &PathBuf) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("读取壁纸失败：{e}"))?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png")
        .to_ascii_lowercase();
    let mime = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "avif" => "image/avif",
        _ => "image/png",
    };
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:{mime};base64,{encoded}"))
}

/// 壁纸文件名前缀。只允许一张壁纸，存多张就要引入列表 / 缩略图 / 删除整套
/// UI，那是 L3.6 明确说的"低优先"，不该现在做。
const WALLPAPER_STEM: &str = "wallpaper";

/// 允许的扩展名白名单。顺带解决一个真 bug：**文件名必须带扩展名**——
/// webview 靠扩展名判定 MIME，无扩展名的 `wallpaper` 会被当成未知类型
/// 直接拒绝渲染，界面表现是"设了壁纸但没变化"，且没有任何报错。
const WALLPAPER_EXTS: [&str; 7] = ["png", "jpg", "jpeg", "webp", "gif", "bmp", "avif"];

fn wallpaper_dir() -> PathBuf {
    resolve_data_dir().join("assets")
}

fn source_record_path() -> PathBuf {
    wallpaper_dir().join(format!("{WALLPAPER_STEM}.source"))
}

/// 读回已装壁纸的实际文件名。多个残留时取字典序最大者：稳定，且与写入
/// 顺序无关（`set_wallpaper` 正常会清掉旧的，这里只是兜底脏数据）。
fn current_wallpaper_file() -> Option<String> {
    let dir = wallpaper_dir();
    let mut hit: Option<String> = None;
    for entry in std::fs::read_dir(&dir).ok()? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(rest) = name.strip_prefix(WALLPAPER_STEM) else {
            continue;
        };
        let ext = rest.strip_prefix('.')?;
        if !WALLPAPER_EXTS.contains(&ext.to_ascii_lowercase().as_str()) {
            continue;
        }
        if hit.as_deref().is_none_or(|h| name.as_str() > h) {
            hit = Some(name);
        }
    }
    hit
}

#[tauri::command]
pub fn get_wallpaper() -> Result<Option<WallpaperInfo>, String> {
    let Some(file) = current_wallpaper_file() else {
        return Ok(None);
    };
    // 原路径单独存：设置页要如实告诉用户图是从哪挑的，否则用户以为
    // 应用复制了一份原件、删了原件也不影响。
    let source = std::fs::read_to_string(source_record_path())
        .unwrap_or_default()
        .trim()
        .to_string();
    let data_url = wallpaper_data_url(&wallpaper_dir().join(&file))?;
    Ok(Some(WallpaperInfo {
        file,
        source,
        data_url,
    }))
}

/// 选一张图当壁纸。复制进数据目录而不是记原路径——记原路径的话，用户
/// 挪走/删掉原图，界面就变成一片空白且无从解释。
#[tauri::command]
pub fn set_wallpaper(source_path: String) -> Result<WallpaperInfo, String> {
    let src = PathBuf::from(&source_path);
    if !src.is_file() {
        return Err("所选文件不存在".to_string());
    }
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .filter(|e| WALLPAPER_EXTS.contains(&e.as_str()))
        .ok_or_else(|| {
            format!("只支持 {} 格式的图片", WALLPAPER_EXTS.join(" / "))
        })?;

    let dir = wallpaper_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建壁纸目录失败：{e}"))?;

    // 换扩展名 = 换文件名，旧的那张必须删掉，否则 current_wallpaper_file
    // 会同时看到两张，"换完图没换"且行为取决于字典序，最难排查。
    if let Some(old) = current_wallpaper_file() {
        let _ = std::fs::remove_file(dir.join(old));
    }

    let file = format!("{WALLPAPER_STEM}.{ext}");
    std::fs::copy(&src, dir.join(&file)).map_err(|e| format!("复制壁纸失败：{e}"))?;
    std::fs::write(source_record_path(), source_path.as_bytes())
        .map_err(|e| format!("记录壁纸来源失败：{e}"))?;
    // 复制完立刻编码 data URL：前端用内联 data URL 渲染，零 asset 协议作用域，
    // 否则 Windows 上 $HOME 解析失配会把整张壁纸变空白。
    let data_url = wallpaper_data_url(&dir.join(&file))?;
    Ok(WallpaperInfo {
        file,
        source: source_path,
        data_url,
    })
}

#[tauri::command]
pub fn clear_wallpaper() -> Result<(), String> {
    let dir = wallpaper_dir();
    // 壁纸是装饰，删不掉不该阻断"清空"这个动作——但也不能装作成功，
    // 所以把错误带回去由前端提示。
    if let Some(file) = current_wallpaper_file() {
        if let Err(e) = std::fs::remove_file(dir.join(file)) {
            if e.kind() != std::io::ErrorKind::NotFound {
                return Err(format!("删除壁纸文件失败：{e}"));
            }
        }
    }
    let _ = std::fs::remove_file(source_record_path());
    Ok(())
}
