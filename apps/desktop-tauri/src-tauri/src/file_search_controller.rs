// 文件搜索小窗控制器：独立窗口生命周期 + 全局热键。
// 搜索引擎是提权守护进程 hermix-filesearch.exe（FilePulse core vendor，
// 见 crates/hermix-filesearch 与 crates/hermix-server/src/filesearch.rs），
// 本控制器只管「窗」与「热键」——数据面走 server 的 /v1/filesearch/*。
//
// 窗口模式：关窗=隐藏（保留搜索词与滚动位置），热键再按秒级唤回；
// Alt+F4 等真实销毁后下次 toggle 重建。先建后亮（hidden）+ 前端
// mount 后 show，与 BrowserView 同款防白屏路径。

use crate::hotkey::{HotkeySlot, HotkeyStatus};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

const WINDOW_LABEL: &str = "file-search";
/// 默认热键；设置面板自定义后由前端经 file_search_apply_config 回放。
const DEFAULT_HOTKEY: &str = "alt+F";
/// 前端页面没就绪时的兜底亮窗时限。页面是小图（不拉主应用依赖图），
/// 正常百毫秒级 show；这里只是"永不亮窗"保险。
const SHOW_FALLBACK_MS: u64 = 8000;

/// 与主窗口 tauri.conf.json 的 additionalBrowserArgs **逐字一致**：
/// 同一 user data folder 下所有 webview 共享一个浏览器进程，环境级
/// 选项不一致时建窗直接报 0x8007139F（见 screenshot_controller 同款注记）。
const FS_BROWSER_ARGS: &str = "--disable-features=msSmartScreenProtection,EdgeFeed,CalculateNativeWinOcclusion,IntensiveWakeUpThrottling,RendererCodeIntegrity --disable-renderer-backgrounding --disable-background-timer-throttling --disk-cache-size=20971520 --js-flags=--max-old-space-size=1024";

/// 功能开关。
static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// 文件搜索热键槽。与截图共用 hotkey.rs 的实现（幂等 / 冲突兜底 /
/// 规范化比较三件事都在那里，只此一份）。
static HOTKEY: HotkeySlot = HotkeySlot::new(
    "文件搜索",
    DEFAULT_HOTKEY,
    &["ctrl+Alt+F", "ctrl+Alt+G", "ctrl+Alt+K"],
);

/// 热键入口：可见则隐藏、隐藏则唤回、不存在则创建。
pub fn toggle(app: &tauri::AppHandle) {
    if !ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    if let Some(win) = app.get_webview_window(WINDOW_LABEL) {
        if win.is_visible().unwrap_or(false) {
            let _ = win.hide();
        } else {
            let _ = win.show();
            let _ = win.set_focus();
        }
        return;
    }
    if let Err(e) = create_window(app) {
        eprintln!("[filesearch] 搜索窗创建失败: {e}");
    }
}

fn create_window(app: &tauri::AppHandle) -> Result<(), String> {
    let win = WebviewWindowBuilder::new(
        app,
        WINDOW_LABEL,
        WebviewUrl::App("index.html#/file-search".into()),
    )
    .title("文件搜索")
    .additional_browser_args(FS_BROWSER_ARGS)
    .inner_size(880.0, 620.0)
    .min_inner_size(640.0, 420.0)
    .center()
    .decorations(false)
    .resizable(true)
    .skip_taskbar(false)
    .visible(false)
    .build()
    .map_err(|e| format!("建窗失败: {e}"))?;

    // 兜底亮窗：前端 mount 后会主动 show；页面异常时这里收尾。
    let fallback = win.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(SHOW_FALLBACK_MS));
        if !fallback.is_visible().unwrap_or(true) {
            let _ = fallback.show();
        }
    });
    Ok(())
}

/// 前端页面就绪后自行 show（BrowserView 同款）。

/// 应用配置（搜索窗设置面板）：开关 + 全局热键。
///
/// 返回真正生效的热键状态（与截图同款语义）：请求的键被占时会换备选键
/// 并回报 `substituted`，前端提示"已改用 X"而不是笼统报错。
#[tauri::command]
pub fn file_search_apply_config(
    app: tauri::AppHandle,
    enabled: bool,
    hotkey: String,
) -> Result<HotkeyStatus, String> {
    ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
    if !enabled {
        HOTKEY.disable(&app);
        return Ok(HotkeyStatus::default());
    }
    let status = HOTKEY.apply(&app, &hotkey)?;
    if status.substituted {
        eprintln!(
            "[filesearch] 热键 {hotkey} 被占用，已改用 {}",
            status.active.as_deref().unwrap_or("?")
        );
    }
    Ok(status)
}

/// 查询文件搜索热键当前状态。
#[tauri::command]
pub fn file_search_hotkey_status() -> HotkeyStatus {
    HOTKEY.status()
}

/// 热键是否属于文件搜索（lib.rs 的全局派发用它）。
pub fn hotkey_matches(shortcut: &tauri_plugin_global_shortcut::Shortcut) -> bool {
    HOTKEY.matches(shortcut)
}

/// setup 时的默认热键注册（前端回放自定义配置前生效）。
pub fn register_default_hotkey(app: &tauri::AppHandle) -> HotkeyStatus {
    HOTKEY.apply_default(app)
}
