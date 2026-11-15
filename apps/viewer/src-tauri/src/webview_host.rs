//! 内嵌网页视图（**通道 A**）：把远程网页渲染在**主窗口内部**。
//!
//! 为什么不是 iframe：① 我们的 CSP 是 `default-src 'self'`，`frame-src` 不放行
//! 外部站点；② 绝大多数站点发 `X-Frame-Options: DENY` / `frame-ancestors` 直接
//! 拒嵌。用顶层子 webview 两道都绕开。
//!
//! 为什么不是系统浏览器（那是**通道 B**，见 `browser.rs`）：用户要的是
//! 「在预览界面展示」，系统浏览器会把人整个带出应用。
//!
//! 安全边界（第一准则）：
//!   - 只放行 http / https（`javascript:` / `file:` / `ms-msdt:` 一律拒），
//!     页面内跳转同样受限（`on_navigation`）——这是 WebView2 已知攻击面的
//!     入口（`ms-msdt:` 之类）。
//!   - 子 webview 与**主窗口共用同一份** WebView2 user data folder：同一进程里
//!     WebView2 不允许分叉（另设 `data_directory` 会报 0x8007139F），隔离靠的是
//!     Chromium 按源划分 cookie / localStorage + 子 webview 不配置 IPC
//!     （capabilities 只覆盖 main）→ 远程站点读写不到主窗口的数据。
//!   - capabilities 只覆盖 main 窗口 → 子 webview 拿不到我们的 IPC，
//!     远程页面无法调用本地命令（也不配 `remote.urls`）。
//!
//! **已知取舍**：wry 默认关掉 SmartScreen
//! （`--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection`），
//! 而 Chromium 的 disable 列表**优先于** enable 列表 → 无法在子 webview 里
//! 单独恢复。补偿手段是上面三条（独立 profile / 无 IPC / 协议白名单）。

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tauri::webview::WebviewBuilder;
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, Rect, Webview, WebviewUrl,
};

/// 子 webview 的 label 前缀（完整 label = `web-<tabId>`，每个网页标签一个）。
const WEB_PREFIX: &str = "web-";
/// 主窗口 label。
const MAIN_LABEL: &str = "main";
/// 内嵌网页视图的**保活池上限**：每个子 webview 是一整棵 WebView2 渲染进程，
/// 无上限保留（每个网页标签一个）内存会线性涨。最多留 3 个，LRU 淘汰——
/// 被淘汰的标签切回来重新加载（丢页面状态），最近切换过的几个保持秒切。
const WEB_KEEPALIVE: usize = 3;

/// 本实例是否已被「真正用过」——预热（`--hidden`）空闲降级的判据。
///
/// 「用过」= 主窗口 show 过 / 开过子 webview / 开过钉住浮窗 / agent 让开过
/// 文件。只要发生过一次，进程进入正常生命周期，不再自动退出。
static USED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 标记本实例已被真正使用（各使用入口调用；幂等）。
pub fn mark_used() {
    USED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// 是否已被真正使用。
pub fn was_used() -> bool {
    USED.load(std::sync::atomic::Ordering::Relaxed)
}

/// 查看器的 WebView2 用户数据目录（**唯一出处**，主窗口与子 webview 共用）。
///
/// 读环境变量而不是每次重算路径：`prepare_user_data_folder()` 在 `main()` 里
/// 已经钉死它，读回来才能保证「谁用谁一致」（CDP 端口文件也在这个目录下）。
pub fn user_data_folder() -> PathBuf {
    std::env::var_os("WEBVIEW2_USER_DATA_FOLDER")
        .map(PathBuf::from)
        .unwrap_or_else(default_user_data_folder)
}

fn default_user_data_folder() -> PathBuf {
    hermix_config::paths::hermix_home().join("viewer-webview")
}

/// 启动时把 `WEBVIEW2_USER_DATA_FOLDER` 钉到查看器自己的目录（幂等）。
///
/// **必须在第一个 webview 创建之前调用**：WebView2 在创建第一个环境时才读这个
/// 变量，之后再设等于没设。
///
/// 为什么要覆盖父进程传进来的值：桌面主程序把它自己的 `webview-cache` 设进了
/// 环境变量，子进程（本查看器）会原样继承。若不覆盖，查看器主窗口就和桌面主
/// 窗口抢同一份用户数据（跨进程锁冲突），而子 webview 无论设不设
/// `data_directory` 都会失败（`0x8007139F` / `ERROR_INVALID_STATE`）。
pub fn prepare_user_data_folder() -> PathBuf {
    let udf = default_user_data_folder();
    let _ = std::fs::create_dir_all(&udf);
    std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", &udf);
    udf
}

/// 网址归一 + 协议白名单：只放行 http / https。
pub fn normalize_url(raw: &str) -> Result<tauri::Url, String> {
    let trimmed = raw.trim();
    let candidate = if trimmed.starts_with("www.") {
        format!("https://{trimmed}")
    } else if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        return Err("只支持 http / https 网址".to_string());
    };
    let url = tauri::Url::parse(&candidate).map_err(|e| format!("网址无法解析：{e}"))?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        other => Err(format!("不支持的协议：{other}")),
    }
}

/// 当前**活动**的子 webview label（前端正在显示的那个网页标签）。
///
/// agent 的 CDP / webview_* 工具都以它为目标——用户与 agent 看的是同一块。
static ACTIVE_WEB: Mutex<Option<String>> = Mutex::new(None);
/// 保活池的 LRU 顺序（队首最旧）。真相源，与存活的 `web-*` label 对应。
static WEB_LRU: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
/// 自增 id：agent（MCP）开视图时前端还没来得及给 tab id，用生成的。
static NEXT_WEB: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// 找指定 label 的子 webview。
fn find_webview(app: &AppHandle, label: &str) -> Option<Webview> {
    let window = app.get_window(MAIN_LABEL)?;
    window
        .webviews()
        .into_iter()
        .find(|w| w.label() == label)
}

/// 当前活动的子 webview。
fn active_webview(app: &AppHandle) -> Option<Webview> {
    let label = active_label();
    find_webview(app, &label)
}

/// 活动 label（可能已失效——调用方用 find_webview 兜底判 None）。
fn active_label() -> String {
    ACTIVE_WEB
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(|| format!("{WEB_PREFIX}none"))
}

/// 把 label 记为活动（LRU touch + 独占可见：其他 `web-*` 全部 hide）。
fn set_active(app: &AppHandle, label: &str) {
    *ACTIVE_WEB.lock().unwrap_or_else(|e| e.into_inner()) = Some(label.to_string());
    let mut lru = WEB_LRU.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(i) = lru.iter().position(|l| l == label) {
        lru.remove(i);
    }
    lru.push_back(label.to_string());
    drop(lru);
    let window = match app.get_window(MAIN_LABEL) {
        Some(w) => w,
        None => return,
    };
    for w in window.webviews() {
        let l = w.label();
        if l.starts_with(WEB_PREFIX) {
            let _ = if l == label { w.show() } else { w.hide() };
        }
    }
}

/// 从 LRU 摘掉一个 label（连同销毁见 `close_label`）。
fn lru_remove(label: &str) {
    let mut lru = WEB_LRU.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(i) = lru.iter().position(|l| l == label) {
        lru.remove(i);
    }
}

/// 关掉指定 label 的子 webview（没有就什么都不做），并清 LRU / 活动指针。
fn close_label(app: &AppHandle, label: &str) {
    lru_remove(label);
    if active_label() == label {
        *ACTIVE_WEB.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    if let Some(webview) = find_webview(app, label) {
        let _ = webview.close();
    }
}

/// 关掉当前活动子 webview（没有就什么都不做）。
pub fn close_webview(app: &AppHandle) {
    let label = active_label();
    close_label(app, &label);
}

/// 网页的**期望可见性**（前端浮层开合通过 `web_set_visible` 写入）。
///
/// 为什么存期望态而不是只做即时 hide/show：`web_open` 会销毁重建子 webview，
/// 与浮层开合并发时，单次 hide/show 可能落在已销毁或尚未建好的实例上——
/// 前端 fire-and-forget 的 show 就此丢失，网页永远隐藏（用户看到的
/// 「点菜单后内嵌浏览器一直空白」）。Rust 侧持有唯一事实：任何时刻新建的
/// 子 webview 都按期望态收尾，交错后必然收敛。
static WEB_VISIBLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// 把期望可见性套用到现存子 webview（没有就不动）。
fn apply_visible(webview: &Webview, visible: bool) {
    let _ = if visible { webview.show() } else { webview.hide() };
}

/// 显示 / 隐藏当前子 webview。
///
/// **为什么菜单展开时必须把它藏起来**：网页是**独立的 HWND**（子 webview），
/// 它永远画在父 webview 的 DOM **之上** —— 不是同一层的东西，CSS 的
/// z-index 根本管不到它。于是标题栏下拉菜单 / 右键菜单一旦展开到内容区
/// 的范围，就被这块网页整个盖住（"菜单被内容区域遮挡"）。
///
/// 菜单收起后要**恢复显示**：直接关掉会丢页面状态（重新加载、滚动位置、
/// 表单内容全没了），hide/show 只动可见性，页面原样还在。
///
/// 先记期望态再套用：即使此刻没有子 webview（正被销毁重建中），期望态也已
/// 落定，`open_sync` 建完会按它收尾——调用方不需要重发。
#[tauri::command]
pub fn web_set_visible(app: AppHandle, visible: bool) {
    WEB_VISIBLE.store(visible, std::sync::atomic::Ordering::Relaxed);
    if let Some(webview) = active_webview(&app) {
        apply_visible(&webview, visible);
    }
}

/// 打开网页：新建（或替换）对应标签的子 webview。
///
/// `id` = 前端网页标签的 key（每个标签一个 label `web-<id>`，切标签不销毁——
/// 保活池 LRU 管理）；不传（agent 经 MCP 开页）则生成一个。多标签共用主窗口，
/// 非活动的处于 hide 态。
///
/// **必须在非主线程调用**：Windows 上 WebView2 的环境创建在主线程调用会死锁
/// （wry#583）。Tauri 的异步命令跑在 tokio 工作线程上，安全。
#[tauri::command]
pub async fn web_open(
    app: AppHandle,
    id: Option<String>,
    url: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) -> Result<(), String> {
    let id = id.unwrap_or_default();
    open_sync(&app, &id, &url, x, y, w, h)
}

/// 建视图的串行闸。
///
/// MCP 是 HTTP 端点，**每条连接一个线程**：两个客户端同时 `web_open` 会对同一个
/// label 并发 `add_child` → WebView2 环境创建与 UI 线程互相等死（用户看到的是
/// 「界面正忙」，整个查看器不可用，比"视图开不了"严重得多）。
static OPEN_LOCK: Mutex<()> = Mutex::new(());

/// 真正开视图的同步实现（异步命令与 MCP 工具共用）。
///
/// MCP 工具也在非主线程上跑（HTTP 端点每条连接一个线程），所以同样安全。
///
/// 纪律：**串行**（见 `OPEN_LOCK`）、**幂等**（同 id 同 URL 已在看就只对齐
/// 可见性）、**等旧视图真的销毁再建**（`close()` 是异步的，同 label 抢先
/// 重建会 `ERROR_INVALID_STATE`）、**保活池上限**（超过 `WEB_KEEPALIVE` 按
/// LRU 淘汰最久未用的标签视图）。
pub fn open_sync(
    app: &AppHandle,
    id: &str,
    url: &str,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) -> Result<(), String> {
    let target = normalize_url(url)?;
    // 开子 webview = 真实使用（agent 自动化也在此列）：预热实例不再降级退出。
    mark_used();
    let _guard = OPEN_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let label = if id.is_empty() {
        // agent（MCP）路径：独立命名空间 `agent-N`，与前端数字 tab id 永不撞
        // label（撞了会把用户正在看的标签视图抢走销毁）。
        format!(
            "{WEB_PREFIX}agent-{}",
            NEXT_WEB.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )
    } else {
        format!("{WEB_PREFIX}{id}")
    };

    // 幂等：同一个标签的同一个页面不销毁重建 —— 重建会丢登录态、滚动位置和
    // 表单内容，而 `web_open` 在自动化流程里会被反复调用（每轮导航前一次）。
    if let Some(webview) = find_webview(app, &label) {
        let same = webview
            .url()
            .map(|u| u.as_str() == target.as_str())
            .unwrap_or(false);
        if same {
            remember_bounds(x, y, w, h);
            set_active(app, &label);
            // 幂等路径也要对齐期望可见性：重建/浮层开合交错时，现存实例的
            // 实际可见性可能落后于期望态。
            let visible = WEB_VISIBLE.load(std::sync::atomic::Ordering::Relaxed);
            apply_visible(&webview, visible);
            return Ok(());
        }
        // 同标签换了 URL：销毁重建（页面状态本来就要换）。
        close_label(app, &label);
        wait_label_gone(app, &label);
    } else {
        // 保活池：超过上限先按 LRU 淘汰最久未用的标签视图（异步 close 即可，
        // label 不同，不会与新视图抢 `ERROR_INVALID_STATE`）。
        loop {
            let victim = {
                let lru = WEB_LRU.lock().unwrap_or_else(|e| e.into_inner());
                if lru.len() < WEB_KEEPALIVE {
                    None
                } else {
                    lru.iter().find(|l| l.as_str() != label).cloned()
                }
            };
            match victim {
                Some(v) => close_label(app, &v),
                None => break,
            }
        }
    }

    let emitter = app.clone();
    let builder = WebviewBuilder::new(&label, WebviewUrl::External(target))
        // **不开独立 data_directory**：同一进程里所有 WebView2 必须共用同一个
        // user data folder，子 webview 若另设 data_directory 会报
        // 0x8007139F（ERROR_INVALID_STATE）。主窗口用的是启动时固定的
        // WEBVIEW2_USER_DATA_FOLDER（查看器自己的目录），子 webview 继承同一份即可。
        // **开调试端口**（端口写 0 = 让浏览器进程自己挑，真实端口写进 profile 的
        // DevToolsActivePort）：这是 agent 能驱动「用户正在看的那块预览区」的
        // 关键 —— 用户与 agent 看到的是同一个页面、同一个登录态。
        // 注意 wry 里 builder 传参是**整体替换**默认串（wry webview2/mod.rs
        // `additional_browser_args.unwrap_or_else(默认串)`），而主窗口走的是默认串；
        // 同一 UDF 下两个 environment 的参数必须逐字一致，否则第二个
        // CreateCoreWebView2EnvironmentWithOptions 直接 0x8007139F
        // （2026-10-23 桌面端三份参数的同一族坑）。所以这里必须写
        // 「wry 默认串 + CDP」，且主窗口 tauri.conf.json 的
        // additionalBrowserArgs 也配成同一份——两处改一处就崩。
        .additional_browser_args(
            "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --remote-debugging-port=0",
        )
        // 页面内跳转同样只放行 http / https。
        .on_navigation(|url| matches!(url.scheme(), "http" | "https"))
        // 页面自己跳了别的地址 → 通知前端（地址栏跟着变，只对对应标签生效）。
        .on_page_load(move |webview, payload| {
            let _ = emitter.emit(
                "web-navigated",
                serde_json::json!({ "id": webview.label(), "url": payload.url() }).to_string(),
            );
        });

    let window = app
        .get_window(MAIN_LABEL)
        .ok_or_else(|| "主窗口不存在".to_string())?;
    window
        .add_child(
            builder,
            PhysicalPosition::new(x as i32, y as i32),
            PhysicalSize::new(w.max(1.0) as u32, h.max(1.0) as u32),
        )
        .map_err(|e| create_error(&e))?;
    remember_bounds(x, y, w, h);
    // 记为活动并独占可见（其他标签视图 hide），再按期望可见性收尾：若浮层
    // 让位的 hide 先于本次创建到达，新建的视图直接藏起（等浮层收起时前端
    // show 回来），否则它会浮在菜单之上。
    set_active(app, &label);
    let visible = WEB_VISIBLE.load(std::sync::atomic::Ordering::Relaxed);
    if let Some(webview) = find_webview(app, &label) {
        apply_visible(&webview, visible);
    }
    // 告诉前端「有个网页视图开了」→ 前端开对应的网页标签（否则用户看到的是
    // 一块没有标签的网页浮在界面上）。载荷带 id：前端把 Rust 侧标签与自己的
    // tab 绑定，后续 web_open / web_bounds 都按这个 id 走。
    let _ = app.emit(
        "web-opened",
        serde_json::json!({ "id": label.trim_start_matches(WEB_PREFIX), "url": url }).to_string(),
    );
    Ok(())
}

/// 等指定 label 的视图真的销毁（`Webview::close()` 只是发销毁请求）。
fn wait_label_gone(app: &AppHandle, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(1);
    while find_webview(app, label).is_some() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// 建视图失败的错误体：带足够事实，让调用方不必再猜。
///
/// 2026-10-30 之前的错误只有 `内嵌网页视图创建失败：{HRESULT}`，于是同一类故障
/// 被反复误判成"UDF 名字不对 / 内核不对 / 拉起方式不对"。
fn create_error(err: &dyn std::fmt::Display) -> String {
    let text = err.to_string();
    let hint = if text.contains("0x8007139F") || text.contains("8007139F") {
        "本进程存在第二套 WebView2 用户数据目录：检查是否给子视图设了 data_directory，\
         或别处改写了 WEBVIEW2_USER_DATA_FOLDER（一个进程只允许一套 UDF）"
    } else if text.contains("0x80070005") || text.contains("拒绝访问") {
        "UDF 目录被另一个进程占用：确认没有第二个查看器进程（不要删 profile，里面是登录态）"
    } else {
        "先稍后重试一次；仍失败就重启查看器（不要删 profile，里面是登录态）"
    };
    format!(
        "内嵌网页视图创建失败：{text} | code=create_failed | udf={} | hint={hint}",
        user_data_folder().display()
    )
}

/// 最近一次的视图位置（物理像素）——agent 侧开视图时用它摆位。
fn last_bounds() -> (f64, f64, f64, f64) {
    bounds_store()
        .lock()
        .unwrap()
        .unwrap_or((0.0, 0.0, 900.0, 700.0))
}

/// 记住视图位置。
fn remember_bounds(x: f64, y: f64, w: f64, h: f64) {
    *bounds_store().lock().unwrap() = Some((x, y, w, h));
}

/// 视图位置（物理像素）。
static BOUNDS: OnceLock<Mutex<Option<(f64, f64, f64, f64)>>> = OnceLock::new();

fn bounds_store() -> &'static Mutex<Option<(f64, f64, f64, f64)>> {
    BOUNDS.get_or_init(|| Mutex::new(None))
}

/// 应用句柄（登记在 `viewer_control`，这里是转发）。
pub fn set_app(app: AppHandle) {
    crate::viewer_control::set_app(app);
}

/// 取应用句柄。
pub fn app() -> Result<AppHandle, String> {
    crate::viewer_control::app()
}

/// 用**上次的位置**开一个网址（给 agent 用：它不知道预览区在哪）。
///
/// id 留空 → Rust 生成一个（前端收到 `web-opened` 载荷里的 id 后，
/// 会把自己的 tab 绑到这个 id 上）。
pub fn open_at_last_bounds(url: &str) -> Result<(), String> {
    let app = app()?;
    let (x, y, w, h) = last_bounds();
    open_sync(&app, "", url, x, y, w, h)
}

/// 按 id（或活动视图）找目标 webview。
fn target_webview(app: &AppHandle, id: Option<&str>) -> Option<Webview> {
    match id {
        Some(id) if !id.is_empty() => find_webview(app, &format!("{WEB_PREFIX}{id}")),
        _ => active_webview(app),
    }
}

/// 位置 / 尺寸同步（前端在窗口缩放、面板开合时调用）。
///
/// 参数是**物理像素**：子 webview 是独立 HWND，CSS 像素定位会随 DPI 偏。
#[tauri::command]
pub fn web_bounds(
    app: AppHandle,
    id: Option<String>,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) -> Result<(), String> {
    let Some(webview) = target_webview(&app, id.as_deref()) else {
        return Ok(());
    };
    let rect = Rect {
        position: PhysicalPosition::new(x as i32, y as i32).into(),
        size: PhysicalSize::new(w.max(1.0) as u32, h.max(1.0) as u32).into(),
    };
    webview
        .set_bounds(rect)
        .map_err(|e| format!("网页视图位置更新失败：{e}"))
}

/// 收起（hide）指定标签的视图但不销毁：切走网页标签时调用，页面状态保留，
/// 保活池满了才被 LRU 淘汰。
#[tauri::command]
pub fn web_release(app: AppHandle, id: String) {
    if let Some(webview) = find_webview(&app, &format!("{WEB_PREFIX}{id}")) {
        let _ = webview.hide();
    }
}

/// 关掉网页视图：带 id 关指定标签的；不带关当前活动的（agent「接管」路径）。
#[tauri::command]
pub fn web_close(app: AppHandle, id: Option<String>) {
    match id {
        Some(id) if !id.is_empty() => close_label(&app, &format!("{WEB_PREFIX}{id}")),
        _ => close_webview(&app),
    }
}

/// 当前内嵌视图的地址。
///
/// CDP 那边要在「浏览器进程的多个 page target」里挑出**当前这个**（超星页 +
/// about:blank 残留页）→ 用这个地址精确匹配。
pub fn active_webview_url() -> Option<String> {
    let app = app().ok()?;
    let webview = active_webview(&app)?;
    webview.url().ok().map(|u| u.to_string())
}

/// 在当前网页视图里跳转。
#[tauri::command]
pub fn web_navigate(app: AppHandle, url: String) -> Result<(), String> {
    let target = normalize_url(&url)?;
    let Some(webview) = active_webview(&app) else {
        return Err("没有活动的网页视图".to_string());
    };
    let literal = serde_json::to_string(target.as_str()).unwrap_or_else(|_| "\"\"".to_string());
    webview
        .eval(format!("window.location.href = {literal};"))
        .map_err(|e| format!("网页跳转失败：{e}"))
}

/// 在网页视图里执行一段脚本（前进 / 后退 / 刷新这类浏览器动作）。
/// 不带 id 作用于活动视图（agent 路径）；带 id 作用于指定标签（前端工具条）。
#[tauri::command]
pub fn web_exec(app: AppHandle, id: Option<String>, script: String) -> Result<(), String> {
    let Some(webview) = target_webview(&app, id.as_deref()) else {
        return Err("没有活动的网页视图".to_string());
    };
    webview
        .eval(script)
        .map_err(|e| format!("网页脚本执行失败：{e}"))
}

/// 当前网页视图的状态（前端拿它决定要不要显示占位 / 做可见性对账）。
#[derive(serde::Serialize)]
pub struct WebState {
    pub open: bool,
    pub url: String,
    /// 期望可见性（浮层收起后前端拿它对账：open 且期望可见 → 实际也应可见）。
    pub visible: bool,
}

#[tauri::command]
pub fn web_state(app: AppHandle) -> WebState {
    let visible = WEB_VISIBLE.load(std::sync::atomic::Ordering::Relaxed);
    match active_webview(&app) {
        Some(webview) => WebState {
            open: true,
            url: webview.url().map(|u| u.to_string()).unwrap_or_default(),
            visible,
        },
        None => WebState {
            open: false,
            url: String::new(),
            visible,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_url;

    #[test]
    fn only_http_and_https_are_allowed() {
        assert!(normalize_url("https://example.com").is_ok());
        assert!(normalize_url("http://example.com/a?b=1").is_ok());
        // www 前缀补协议
        assert_eq!(
            normalize_url("www.example.com").unwrap().as_str(),
            "https://www.example.com/"
        );
        // 危险协议一律拒
        assert!(normalize_url("javascript:alert(1)").is_err());
        assert!(normalize_url("file:///C:/Windows/System32").is_err());
        assert!(normalize_url("ms-msdt:/id").is_err());
        assert!(normalize_url("C:\\Windows").is_err());
    }
}
