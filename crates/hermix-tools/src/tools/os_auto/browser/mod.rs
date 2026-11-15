//! Browser automation — CDP over the **system Edge**, in-house.
//!
//! Eight tools, one browser session **per conversation** (keyed by `_session_id`),
//! one essential-floor toolset. Each session owns
//! the pieces that make a browser session safe to leave running:
//!
//! - The process lives in a Job Object with `KILL_ON_JOB_CLOSE`
//!   ([`job`]), so hermix crashing, being killed, or panicking cannot leave an
//!   Edge behind. This replaces the deleted `cdp_bootstrap`'s PID-file +
//!   `taskkill` scheme, which had a race: a browser could start between the
//!   check and the kill.
//! - The debugging port is never hardcoded ([`launch`]) — Edge picks it and
//!   writes it to `DevToolsActivePort` inside *our* throwaway profile.
//! - The model never sees raw HTML ([`a11y`]). It sees an accessibility tree
//!   with our own `ref=` handles, capped on both axes.
//! - The browser is always headless. The human-facing surface is the embedded
//!   WebView2 panel ([`panel`]): when one is registered the tools attach to it
//!   over CDP and the user watches that window; the headless fallback has no
//!   picture for humans — the model works off the AX tree ([`a11y`]).
//!
//! Everything runs on one session guarded by a `tokio::sync::Mutex`: the tools
//! are registered `is_interactive`, so the dispatcher already serialises them,
//! and the mutex keeps a direct concurrent call from interleaving a `click`
//! with a `snapshot` that invalidates its refs.

pub mod a11y;
pub mod job;
pub mod launch;
/// Embedded WebView2 panel bridge (port registry + CDP discovery).
pub mod panel;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use chromiumoxide::cdp::browser_protocol::dom::{BackendNodeId, ResolveNodeParams};
use chromiumoxide::cdp::js_protocol::runtime::CallFunctionOnParams;
use chromiumoxide::{Browser, Page};
use hermix_core::{
    seams::{AttachmentKind, ToolOutput},
    tool_schema,
    traits::RiskLevel,
    JsonSchema, ToolError, ToolHandler, ToolSchema,
};
use indexmap::IndexMap;
use serde_json::{json, Value};
use tokio::sync::{Mutex, MutexGuard};
use tokio::task::JoinHandle;

use crate::tools::os_auto::truncate_chars;
use crate::ToolRegistry;

/// Outermost cap on any tool result. Inner caps (node count, element name)
/// are the real budget; this is the backstop that keeps a pathological page
/// from costing more context than the whole rest of the turn.
/// 快照与所有 browser_* 文本结果的硬顶。模型传参可以调低，
/// 不能调高——schema 的 maximum 与 clamp 上限都锚定在这里，
/// 不存在"传 48000 翻倍"的后门。
const RESULT_CAP: usize = 24_000;

/// Outermost caps on the limits the model may ask for. A model that asks for
/// 50 000 nodes is making a mistake, and obeying it would blow the turn.
const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 4_000;

/// Schemes we will navigate to. `javascript:` in particular would let a page
/// argument run code in the session's own origin, and anything unknown is
/// either a typo or an attempt at something else.
const ALLOWED_PREFIXES: [&str; 5] = ["http://", "https://", "about:", "file://", "data:"];

const STALE_REF: &str = "快照已过期，请重新调用 browser_snapshot";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Navigate,
    Snapshot,
    Click,
    Type,
    Screenshot,
    Close,
    Wait,
    Scroll,
}

const TOOLS: [(&str, Op, RiskLevel, &str); 8] = [
    (
        "browser_navigate",
        Op::Navigate,
        RiskLevel::Caution,
        "Open a URL in the automation browser (a throwaway Edge profile, never your own). Returns the final URL and title; call browser_snapshot next to see the page. For SPAs that render after load, follow with browser_wait.",
    ),
    (
        "browser_snapshot",
        Op::Snapshot,
        RiskLevel::Safe,
        "Dump the page's accessibility tree as one line per element, each with a `ref=` handle. This is how you \"see\" a page — full HTML is never returned. Refs are only valid until the next snapshot.",
    ),
    (
        "browser_click",
        Op::Click,
        RiskLevel::Caution,
        "Click an element. Prefer `ref` from the latest browser_snapshot; `selector` is a CSS fallback for pages you have not snapshotted.",
    ),
    (
        "browser_type",
        Op::Type,
        RiskLevel::Caution,
        "Type text into a field. Prefer `ref` from the latest browser_snapshot; `selector` is a CSS fallback. Set `submit` to press Enter afterwards.",
    ),
    (
        "browser_screenshot",
        Op::Screenshot,
        RiskLevel::Safe,
        "Capture the viewport (or the full page) as a PNG. The image is attached to the tool result and also saved under the hermix home for later re-reading.",
    ),
    (
        "browser_close",
        Op::Close,
        RiskLevel::Safe,
        "Close the automation browser and delete its throwaway profile. The next browser tool call starts a fresh one.",
    ),
    (
        "browser_wait",
        Op::Wait,
        RiskLevel::Safe,
        "Block until a CSS selector exists in the page (polls every 200ms). Use after browser_navigate on SPAs / lazy-loaded pages before calling browser_snapshot, so you don't snapshot a half-rendered page.",
    ),
    (
        "browser_scroll",
        Op::Scroll,
        RiskLevel::Safe,
        "Scroll the page. Give exactly one of: `by` (pixels, negative scrolls up), `target` (\"top\" or \"bottom\"), or `ref`/`selector` to scroll an element into view. Lazy-loaded lists need scrolling before their content enters the snapshot.",
    ),
];

/// Register the eight browser tools under the `browser` toolset.
pub fn register(registry: &ToolRegistry) {
    for (name, op, risk, desc) in TOOLS {
        let handler = Arc::new(BrowserHandler { op });
        registry.register_with_risk(
            name,
            super::BROWSER_TOOLSET,
            handler.schema(),
            handler,
            // Availability is "is there an Edge on this machine", not "did the
            // user opt in" — that is the toolset toggle's job.
            Arc::new(|| launch::resolve_edge_exe().is_some()),
            vec!["HERMIX_BROWSER_PATH".to_string()],
            true,
            desc,
            "🌐",
            // `None` on purpose: the registry truncates on a byte boundary and
            // can split a CJK character. `truncate_chars` does it correctly.
            None,
            risk,
            false,
        );
        // One shared browser page: two of these running at once would interleave        // a click with a snapshot that invalidates its refs.
        registry.mark_interactive(name);
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

struct Session {
    /// 本会话的键（`_session_id`），会话表的键。
    key: String,
    browser: Browser,
    page: Page,
    /// Polls the CDP event stream. chromiumoxide will not answer any command
    /// while nobody drives this.
    _handler_task: JoinHandle<()>,
    /// Holds the Job Object handle. Dropping it kills the Edge tree — that is
    /// what makes "no orphaned browser" a kernel guarantee instead of a
    /// best-effort cleanup. `None` when the session is attached to the
    /// embedded UI panel (the panel's lifetime belongs to the window, not to
    /// us — closing it is the UI's job).
    _launched: Option<launch::LaunchedBrowser>,
    /// `true` when this session drives the embedded UI panel. Such a session
    /// must be rebuilt as soon as the panel closes — its `Page` belongs to a
    /// destroyed webview and every later command would fail.
    attached_to_panel: bool,
    /// `ref` → DOM node, replaced wholesale by every snapshot.
    refs: HashMap<String, BackendNodeId>,
    snapshots: u64,
    /// 空闲回收计时：每 ensure 触碰即刷新。
    last_used: std::time::Instant,
}

/// 每个对话一个浏览器；同时存活的浏览器实例上限（每个 Edge 数百 MB）。
const MAX_SESSIONS: usize = 4;
/// 空闲超过该时长的会话在下次 ensure 时被回收（Drop 杀 Edge 树 + 清 profile）。
const IDLE_REAP_AFTER: std::time::Duration = std::time::Duration::from_secs(15 * 60);

impl Drop for Session {
    fn drop(&mut self) {
        // The handler task waits on a WebSocket owned by `browser`; aborting it
        // first avoids a task that outlives the session it was serving.
        self._handler_task.abort();
        // Panel-attached sessions own no profile: the WebView2 panel lives in
        // the app's own user-data folder and is closed by the UI.
        if let Some(launched) = self._launched.as_ref() {
            let dir = launched.user_data_dir.clone();
            // A throwaway profile is a few MB of cache; leaving thousands of them
            // behind would be worse than the few ms this can cost.
            if let Err(e) = std::fs::remove_dir_all(&dir) {
                tracing::debug!(path = %dir.display(), error = %e, "浏览器 profile 目录清理失败（将留待下次启动）");
            }
        }
    }
}

static SESSIONS: OnceLock<Mutex<HashMap<String, Session>>> = OnceLock::new();

fn sessions() -> &'static Mutex<HashMap<String, Session>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 从工具参数取浏览器会话键。registry 的 `dispatch_with_context` 会把
/// `_session_id` 注入 params（多会话并行用浏览器的隔离键）；无上下文
/// （测试 / headless 调度）落回 `"default"`。
fn session_key(params: &Value) -> String {
    // 持久 profile（P5）：带 profile 的调用键控到 profile 名上 —— 同名
    // profile 的后续调用复用同一浏览器会话（登录态在，不用重登）。
    if let Some(name) = params.get("profile").and_then(Value::as_str) {
        if !name.is_empty() {
            return format!("profile:{name}");
        }
    }
    params
        .get("_session_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("default")
        .to_string()
}

/// The live session for `key`, starting one if needed.
///
/// The browser is always headless and the model has no parameter that could
/// produce a real, focus-stealing window.
///
/// 键控多会话（2026-10-17）：每个对话独立 Edge 实例（独立 profile），
/// 互不串页面。整张表共用一把锁——单次调用串行化，足以
/// 防止 click/snapshot 交错；跨对话的调用序列隔离由键本身保证。
async fn ensure(key: &str) -> Result<MutexGuard<'static, HashMap<String, Session>>, ToolError> {
    let mut guard = sessions().lock().await;
    // 空闲回收先行：释放预算，也避免 Edge 实例无上限累积。
    guard.retain(|k, s| {
        let idle = s.last_used.elapsed();
        if idle >= IDLE_REAP_AFTER {
            tracing::info!(session = %k, idle_secs = idle.as_secs(), "回收空闲浏览器会话");
            false
        } else {
            true
        }
    });
    // 面板关窗后，附着在面板上的页面已随 webview 销毁——这类会话必须
    // 丢弃重建，否则后续每条 CDP 命令都会失败（用户看到的是"工具坏了"）。
    if panel::current().is_none() {
        guard.retain(|_, s| !s.attached_to_panel);
    }
    if let Some(s) = guard.get_mut(key) {
        s.last_used = std::time::Instant::now();
        return Ok(guard);
    }
    if guard.len() >= MAX_SESSIONS {
        return Err(ToolError::ExecutionFailed(format!(
            "浏览器并发会话已达上限（{MAX_SESSIONS}），请先让部分对话调用 browser_close 释放"
        )));
    }
    // Panel-first: if the desktop UI registered an embedded browser panel
    // (WebView2 + remote debugging), drive THAT page — the user watches the
    // automation inside the app instead of a stray Edge window. Any failure
    // here degrades to the standalone Edge path below.
    if let Some(state) = panel::current() {
        match panel::discover_ws_url(state.port).await {
            Ok(ws) => match Browser::connect(ws.clone()).await {
                Ok((browser, mut handler)) => {
                    let handler_task = tokio::spawn(async move {
                        use futures::StreamExt;
                        while handler.next().await.is_some() {}
                    });
                    // Prefer the panel's own tab; fall back to a fresh one.
                    let page = match browser.pages().await {
                        Ok(mut pages) => match pages.pop() {
                            Some(page) => page,
                            None => browser
                                .new_page("about:blank")
                                .await
                                .map_err(|e| {
                                    ToolError::ExecutionFailed(format!("面板新建标签页失败: {e}"))
                                })?,
                        },
                        Err(e) => {
                            tracing::warn!(error = %e, "读取面板标签页失败，新建一个");
                            browser.new_page("about:blank").await.map_err(|e| {
                                ToolError::ExecutionFailed(format!("面板新建标签页失败: {e}"))
                            })?
                        }
                    };
                    if let Err(e) = set_download_dir(&page).await {
                        tracing::warn!(error = %e, "设置面板下载目录失败（保持默认）");
                    }
                    tracing::info!(
                        port = state.port,
                        ws = %ws,
                        session = %key,
                        "attached to embedded browser panel"
                    );
                    guard.insert(
                        key.to_string(),
                        Session {
                            key: key.to_string(),
                            browser,
                            page,
                            _handler_task: handler_task,
                            _launched: None,
                            attached_to_panel: true,
                            refs: HashMap::new(),
                            snapshots: 0,
                            last_used: std::time::Instant::now(),
                        },
                    );
                    return Ok(guard);
                }
                Err(e) => tracing::warn!(
                    error = %e,
                    port = state.port,
                    "面板 CDP 连接失败，回退到独立 Edge"
                ),
            },
            Err(e) => {
                // Stale registry: the user closed the panel via the window's
                // X button, which does not run our close command. Drop the
                // panel-bound sessions so later calls do not keep issuing
                // commands to a destroyed webview.
                guard.retain(|_, s| !s.attached_to_panel);
                tracing::warn!(error = %e, "面板调试端点不可达，回退到独立 Edge");
            }
        }
    }

    // 持久 profile（P5）：键形如 `profile:<name>`（见 session_key）。
    let profile_name = key.strip_prefix("profile:").map(str::to_string);
    let launched = launch::launch_named(profile_name.as_deref())
        .await
        .map_err(ToolError::ExecutionFailed)?;
    let (browser, mut handler) = Browser::connect(launched.ws_url.clone())
        .await
        .map_err(|e| {
            ToolError::ExecutionFailed(format!("连接 Edge 调试端口失败 ({}): {e}", launched.ws_url))
        })?;
    let handler_task = tokio::spawn(async move {
        use futures::StreamExt;
        while handler.next().await.is_some() {}
    });
    let page = browser
        .new_page("about:blank")
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("创建标签页失败: {e}")))?;
    // Pitfall F5: without an explicit target, downloads land in the
    // process CWD and dialogs block the navigation that raised them.
    if let Err(e) = set_download_dir(&page).await {
        tracing::warn!(error = %e, "设置浏览器下载目录失败（下载行为保持默认）");
    }
    tracing::info!(ws = %launched.ws_url, session = %key, "automation browser started");
    guard.insert(
        key.to_string(),
        Session {
            key: key.to_string(),
            browser,
            page,
            _handler_task: handler_task,
            _launched: Some(launched),
            attached_to_panel: false,
            refs: HashMap::new(),
            snapshots: 0,
            last_used: std::time::Instant::now(),
        },
    );
    Ok(guard)
}

/// Pitfall F5: with no explicit target, downloads land in the process CWD and
/// a `beforeunload` dialog blocks the very navigation that raised it. The
/// page-level command is the spelling that survives a new tab.
async fn set_download_dir(page: &Page) -> Result<(), String> {
    use chromiumoxide::cdp::browser_protocol::browser::{
        SetDownloadBehaviorBehavior, SetDownloadBehaviorParams,
    };

    let dir = hermix_config::paths::hermix_home().join("downloads").join("browser");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    page.execute(SetDownloadBehaviorParams {
        behavior: SetDownloadBehaviorBehavior::Allow,
        download_path: Some(dir.to_string_lossy().to_string()),
        browser_context_id: None,
        events_enabled: None,
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// CDP helpers
// ---------------------------------------------------------------------------

/// Turn a backend node id into a JS object handle.
async fn resolve_node(page: &Page, id: BackendNodeId) -> Result<String, ToolError> {
    let resolved = page
        .execute(ResolveNodeParams {
            node_id: None,
            backend_node_id: Some(id),
            object_group: None,
            execution_context_id: None,
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("解析元素节点失败: {e}")))?;
    resolved
        .result
        .object
        .object_id
        .map(|id| id.inner().clone())
        .ok_or_else(|| {
            ToolError::InvalidParams("该元素没有可执行的 JS 句柄（可能已被页面移除）".into())
        })
}

/// Run `body` with the node as `this`.
///
/// `user_gesture` is on deliberately: without it Chrome treats scripted clicks
/// as untrusted and a surprising number of buttons simply do nothing.
async fn call_on(
    page: &Page,
    id: BackendNodeId,
    body: &str,
) -> Result<Option<String>, ToolError> {
    let object_id = resolve_node(page, id).await?;
    // Every field is listed: the generated param struct has no `Default`, and a
    // missing field would silently mean "not set" rather than "compile error".
    let result = page
        .execute(CallFunctionOnParams {
            function_declaration: format!("function(){{ {body} }}"),
            object_id: Some(object_id.into()),
            arguments: None,
            silent: None,
            return_by_value: Some(true),
            generate_preview: None,
            user_gesture: Some(true),
            await_promise: None,
            execution_context_id: None,
            object_group: None,
            throw_on_side_effect: None,
            unique_context_id: None,
            serialization_options: None,
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("执行元素操作失败: {e}")))?;
    Ok(result
        .result
        .result
        .value
        .as_ref()
        .and_then(|v| v.as_str().map(str::to_string)))
}

async fn click_node(page: &Page, id: BackendNodeId) -> Result<(), ToolError> {
    call_on(page, id, "this.click();").await?;
    Ok(())
}

async fn focus_node(page: &Page, id: BackendNodeId) -> Result<(), ToolError> {
    call_on(page, id, "this.focus();").await?;
    Ok(())
}

async fn type_into_ref(page: &Page, id: BackendNodeId, text: &str) -> Result<(), ToolError> {
    focus_node(page, id).await?;
    // `Input.insertText` inserts one composed string, so CJK and emoji land in
    // one edit instead of one synthetic key event per UTF-16 unit.
    use chromiumoxide::cdp::browser_protocol::input::InsertTextParams;
    page.execute(InsertTextParams { text: text.to_string() })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("输入文本失败: {e}")))?;
    Ok(())
}

async fn press_enter(page: &Page) -> Result<(), ToolError> {
    use chromiumoxide::cdp::browser_protocol::input::{
        DispatchKeyEventParams, DispatchKeyEventType,
    };
    for ty in [DispatchKeyEventType::KeyDown, DispatchKeyEventType::KeyUp] {
        page.execute(DispatchKeyEventParams {
            r#type: ty,
            modifiers: None,
            timestamp: None,
            text: None,
            unmodified_text: None,
            key_identifier: None,
            code: Some("Enter".to_string()),
            key: Some("Enter".to_string()),
            windows_virtual_key_code: Some(13),
            native_virtual_key_code: Some(13),
            auto_repeat: None,
            is_keypad: None,
            is_system_key: None,
            location: None,
            commands: None,
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("发送回车失败: {e}")))?;
    }
    Ok(())
}

/// Ref lookup with the staleness rule: a ref from a previous snapshot is
/// refused rather than silently pointing at whatever now occupies that slot.
///
/// Takes the table rather than the whole session so the rule is testable
/// without a live browser.
fn resolve_ref<'a>(
    refs: &'a HashMap<String, BackendNodeId>,
    reference: &str,
) -> Result<&'a BackendNodeId, ToolError> {
    refs.get(reference).ok_or_else(|| {
        ToolError::InvalidParams(format!("{STALE_REF}（无效 ref `{reference}`）"))
    })
}

fn exactly_one(params: &Value, a: &str, b: &str) -> Result<(), ToolError> {
    let has_a = params.get(a).and_then(Value::as_str).is_some();
    let has_b = params.get(b).and_then(Value::as_str).is_some();
    match (has_a, has_b) {
        (true, true) => Err(ToolError::InvalidParams(format!(
            "`{a}` 与 `{b}` 只能给一个：ref 来自最近的 browser_snapshot，更可靠"
        ))),
        (false, false) => Err(ToolError::InvalidParams(format!(
            "缺少 `{a}` 或 `{b}`：先用 browser_snapshot 拿到 ref，或直接给 CSS selector"
        ))),
        _ => Ok(()),
    }
}

fn validate_url(raw: &str) -> Result<&str, ToolError> {
    let trimmed = raw.trim();
    let lower = trimmed.to_ascii_lowercase();
    if !ALLOWED_PREFIXES.iter().any(|p| lower.starts_with(p)) {
        return Err(ToolError::InvalidParams(format!(
            "不支持的 URL scheme：只允许 {}，收到 `{}`",
            ALLOWED_PREFIXES.join(" / "),
            trimmed.chars().take(60).collect::<String>()
        )));
    }
    Ok(trimmed)
}

fn opt_str(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(Value::as_str).map(str::to_string)
}

fn opt_usize(params: &Value, key: &str, default: usize, cap: usize) -> usize {
    params
        .get(key)
        .and_then(Value::as_u64)
        // Clamped both ways: 0 would come back looking like an empty page.
        .map(|v| v.clamp(1, cap as u64) as usize)
        .unwrap_or(default)
}

fn bool_param(params: &Value, key: &str, default: bool) -> bool {
    params.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn screenshot_path() -> PathBuf {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S-%f");
    hermix_config::paths::hermix_home()
        .join("screenshots")
        .join("browser")
        .join(format!("shot-{stamp}.png"))
}

/// Current vertical scroll position and remaining scrollable height,
/// reported back after a scroll so the model knows whether the page has
/// more below the fold. Best-effort: failures read as `(0, 0)`.
async fn scroll_position(page: &Page) -> (i64, i64) {
    let expr = "JSON.stringify({y: window.scrollY, \
                total: document.documentElement.scrollHeight - window.innerHeight})";
    match page.evaluate(expr).await {
        Ok(res) => {
            let v: Value = res.into_value().unwrap_or(Value::Null);
            let y = v.get("y").and_then(Value::as_i64).unwrap_or(0);
            let total = v.get("total").and_then(Value::as_i64).unwrap_or(0);
            (y, total.max(y))
        }
        Err(_) => (0, 0),
    }
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

struct BrowserHandler {
    op: Op,
}

#[async_trait]
impl ToolHandler for BrowserHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let session_key = session_key(&params);
        let out: Result<String, ToolError> = match self.op {
            Op::Navigate => {
                let raw = params
                    .get("url")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ToolError::InvalidParams("缺少必填参数 `url`".into()))?;
                let url = validate_url(raw)?.to_string();
                let timeout_ms = opt_usize(&params, "timeout_ms", 30_000, 120_000) as u64;

                let mut guard = ensure(&session_key).await?;
                let session = guard.get_mut(&session_key).expect("ensure() leaves a session");
                tokio::time::timeout(
                    Duration::from_millis(timeout_ms),
                    session.page.goto(url.clone()),
                )
                .await
                .map_err(|_| ToolError::Timeout(format!("打开 {url} 超时（{timeout_ms}ms）")))?
                .map_err(|e| ToolError::ExecutionFailed(format!("导航失败: {e}")))?;

                let final_url = session
                    .page
                    .url()
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| url.clone());
                let title = session.page.get_title().await.ok().flatten().unwrap_or_default();
                Ok(format!(
                    "已打开 {final_url}\n标题：{}\n下一步：调用 browser_snapshot 查看页面元素（ref 只在本次快照内有效）。",
                    if title.is_empty() { "(无标题)" } else { &title }
                ))
            }
            Op::Snapshot => {
                let max_depth = opt_usize(&params, "max_depth", 12, MAX_DEPTH);
                let max_nodes = opt_usize(&params, "max_nodes", 800, MAX_NODES);
                let max_chars = opt_usize(&params, "max_chars", RESULT_CAP, RESULT_CAP);

                let mut guard = ensure(&session_key).await?;
                let session = guard.get_mut(&session_key).expect("ensure() leaves a session");
                let (tree, refs) =
                    a11y::snapshot_with_refs(&session.page, max_depth, max_nodes)
                        .await
                        .map_err(ToolError::ExecutionFailed)?;
                // Replaced wholesale: any ref from before this line is dead.
                session.refs = refs;
                session.snapshots += 1;

                let header = if session.snapshots > 1 {
                    "[本快照已替换上一次快照，旧的 ref 全部失效]\n"
                } else {
                    ""
                };
                Ok(format!("{header}{}", truncate_chars(&tree, max_chars)))
            }
            Op::Click => {
                exactly_one(&params, "ref", "selector")?;
                let mut guard = ensure(&session_key).await?;
                let session = guard.get_mut(&session_key).expect("ensure() leaves a session");
                if let Some(reference) = opt_str(&params, "ref") {
                    let id = *resolve_ref(&session.refs, &reference)?;
                    click_node(&session.page, id).await?;
                    Ok(format!("已点击 {reference}"))
                } else {
                    let selector = opt_str(&params, "selector").expect("exactly_one");
                    let element = session
                        .page
                        .find_element(selector.clone())
                        .await
                        .map_err(|e| ToolError::InvalidParams(format!("找不到 `{selector}`: {e}")))?;
                    element
                        .click()
                        .await
                        .map_err(|e| ToolError::ExecutionFailed(format!("点击 `{selector}` 失败: {e}")))?;
                    Ok(format!("已点击 `{selector}`"))
                }
            }
            Op::Type => {
                exactly_one(&params, "ref", "selector")?;
                let text = params
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ToolError::InvalidParams("缺少必填参数 `text`".into()))?
                    .to_string();
                let submit = bool_param(&params, "submit", false);
                if text.chars().count() > 4_000 {
                    return Err(ToolError::InvalidParams(
                        "单次输入超过 4000 字符，请分段输入".into(),
                    ));
                }

                let mut guard = ensure(&session_key).await?;
                let session = guard.get_mut(&session_key).expect("ensure() leaves a session");
                let target = opt_str(&params, "ref")
                    .map(|r| format!("`{r}`"))
                    .unwrap_or_else(|| {
                        format!("`{}`", opt_str(&params, "selector").unwrap_or_default())
                    });
                if let Some(reference) = opt_str(&params, "ref") {
                    let id = *resolve_ref(&session.refs, &reference)?;
                    type_into_ref(&session.page, id, &text).await?;
                } else {
                    let selector = opt_str(&params, "selector").expect("exactly_one");
                    let element = session
                        .page
                        .find_element(selector.clone())
                        .await
                        .map_err(|e| ToolError::InvalidParams(format!("找不到 `{selector}`: {e}")))?;
                    element
                        .type_str(&text)
                        .await
                        .map_err(|e| ToolError::ExecutionFailed(format!("输入到 `{selector}` 失败: {e}")))?;
                }
                if submit {
                    press_enter(&session.page).await?;
                }
                Ok(format!(
                    "已向 {target} 输入 {} 个字符{}",
                    text.chars().count(),
                    if submit { "并回车" } else { "" }
                ))
            }
            Op::Screenshot => {
                let full_page = bool_param(&params, "full_page", false);
                let mut guard = ensure(&session_key).await?;
                let session = guard.get_mut(&session_key).expect("ensure() leaves a session");
                let png = capture(&session.page, full_page).await?;
                let path = screenshot_path();
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| ToolError::ExecutionFailed(format!("创建截图目录失败: {e}")))?;
                }
                std::fs::write(&path, &png)
                    .map_err(|e| ToolError::ExecutionFailed(format!("保存截图失败: {e}")))?;
                Ok(format!(
                    "已保存截图（{}，{} KB）：\n{}",
                    if full_page { "整页" } else { "视口" },
                    png.len() / 1024,
                    path.display()
                ))
            }
            Op::Close => {
                // Taking THIS conversation's session out and dropping it closes
                // the job handle, which is what actually terminates Edge.
                // 其它对话的浏览器实例不受影响（键控多会话）。
                let taken = sessions().lock().await.remove(&session_key);
                match taken {
                    Some(_) => Ok("已关闭本对话的自动化浏览器并清理其临时 profile。".to_string()),
                    None => Ok("本对话没有正在运行的自动化浏览器，无需关闭。".to_string()),
                }
            }
            Op::Wait => {
                let selector = params
                    .get("selector")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ToolError::InvalidParams("缺少必填参数 `selector`".into()))?
                    .to_string();
                let timeout_ms = opt_usize(&params, "timeout_ms", 5_000, 30_000) as u64;
                let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);

                let mut guard = ensure(&session_key).await?;
                let session = guard.get_mut(&session_key).expect("ensure() leaves a session");
                loop {
                    if session.page.find_element(&selector).await.is_ok() {
                        return Ok(format!("`{selector}` 已出现，可以继续操作。"));
                    }
                    if std::time::Instant::now() >= deadline {
                        return Err(ToolError::Timeout(format!(
                            "等待 `{selector}` 出现超时（{timeout_ms}ms）— 页面可能渲染失败或选择器有误"
                        )));
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
            Op::Scroll => {
                let provided = [
                    params.get("by").is_some(),
                    params.get("target").is_some(),
                    params.get("ref").is_some() || params.get("selector").is_some(),
                ];
                if provided.iter().filter(|p| **p).count() != 1 {
                    return Err(ToolError::InvalidParams(
                        "browser_scroll 需要且只需要 `by` / `target` / (`ref` 或 `selector`) 之一".into(),
                    ));
                }
                let js: String = if let Some(n) = params.get("by").and_then(Value::as_i64) {
                    format!("window.scrollBy(0, {n}); 'ok'")
                } else if let Some(t) = opt_str(&params, "target") {
                    match t.as_str() {
                        "top" => "window.scrollTo(0, 0); 'ok'".to_string(),
                        "bottom" => "window.scrollTo(0, document.body.scrollHeight); 'ok'".to_string(),
                        other => {
                            return Err(ToolError::InvalidParams(format!(
                                "`target` 只接受 top/bottom，收到 `{other}`"
                            )));
                        }
                    }
                } else {
                    // scrollIntoView — element refs go through the same
                    // ResolveNode path the click/type helpers use.
                    String::new() // filled below
                };

                let mut guard = ensure(&session_key).await?;
                let session = guard.get_mut(&session_key).expect("ensure() leaves a session");
                if !js.is_empty() {
                    session
                        .page
                        .evaluate(js)
                        .await
                        .map_err(|e| ToolError::ExecutionFailed(format!("滚动失败: {e}")))?;
                } else if let Some(reference) = opt_str(&params, "ref") {
                    let id = *resolve_ref(&session.refs, &reference)?;
                    call_on(&session.page, id, "this.scrollIntoView({block: 'center'});").await?;
                } else {
                    let selector = opt_str(&params, "selector").expect("validated above");
                    // JSON-encode the selector so it is a safe JS string
                    // literal regardless of quotes it contains.
                    let lit = serde_json::to_string(&selector)
                        .map_err(|e| ToolError::ExecutionFailed(format!("选择器编码失败: {e}")))?;
                    let js = format!(
                        "(() => {{ const el = document.querySelector({lit}); \
                         if (!el) return 'missing'; \
                         el.scrollIntoView({{block: 'center'}}); return 'ok'; }})()"
                    );
                    let result = session
                        .page
                        .evaluate(js)
                        .await
                        .map_err(|e| ToolError::ExecutionFailed(format!("滚动失败: {e}")))?;
                    let v: Value = result.into_value().unwrap_or(Value::Null);
                    if v.as_str() == Some("missing") {
                        return Err(ToolError::InvalidParams(format!(
                            "找不到 `{selector}`（元素未渲染或选择器有误）"
                        )));
                    }
                }
                let (y, total) = scroll_position(&session.page).await;
                Ok(format!(
                    "已滚动。当前纵向位置 {y} / {total} 像素。下方内容已进入视口后可重新 browser_snapshot。"
                ))
            }
        };
        Ok(truncate_chars(&out?, RESULT_CAP))
    }

    /// Screenshot attaches the PNG to the result so the desktop renders it
    /// inline; the bytes never enter the prompt as base64, which is exactly
    /// why the structured path exists.
    async fn execute_structured(&self, params: Value) -> Result<ToolOutput, ToolError> {
        let text = self.execute(params).await?;
        if self.op != Op::Screenshot {
            return Ok(ToolOutput::text(text));
        }
        let Some(path) = path_from_message(&text) else {
            return Ok(ToolOutput::text(text));
        };
        match std::fs::read(&path) {
            Ok(bytes) => Ok(ToolOutput::text(text).with_attachment(AttachmentKind::Image, "image/png", bytes)),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "读取截图文件失败，仅返回路径");
                Ok(ToolOutput::text(text))
            }
        }
    }

    fn schema(&self) -> ToolSchema {
        let reference = |what: &str| {
            json!({
                "type": "string",
                "description": format!("`ref=` handle from the latest browser_snapshot ({what})")
            })
        };
        let (name, desc, mut props, required): (&str, &str, IndexMap<String, Value>, Vec<String>) =
            match self.op {
                Op::Navigate => (
                    "browser_navigate",
                    TOOLS[0].3,
                    IndexMap::from([
                        ("url".to_string(), json!({"type": "string", "description": "Absolute URL. Allowed schemes: http, https, about, file, data."})),
                        ("timeout_ms".to_string(), json!({"type": "integer", "minimum": 1000, "maximum": 120000, "default": 30000})),
                    ]),
                    vec!["url".into()],
                ),
                Op::Snapshot => (
                    "browser_snapshot",
                    TOOLS[1].3,
                    IndexMap::from([
                        ("max_depth".to_string(), json!({"type": "integer", "minimum": 1, "maximum": MAX_DEPTH, "default": 12})),
                        ("max_nodes".to_string(), json!({"type": "integer", "minimum": 1, "maximum": MAX_NODES, "default": 800})),
                        ("max_chars".to_string(), json!({"type": "integer", "minimum": 1000, "maximum": RESULT_CAP, "default": RESULT_CAP})),
                    ]),
                    vec![],
                ),
                Op::Click => (
                    "browser_click",
                    TOOLS[2].3,
                    IndexMap::from([
                        ("ref".to_string(), reference("preferred")),
                        ("selector".to_string(), json!({"type": "string", "description": "CSS selector fallback; give exactly one of ref/selector."})),
                    ]),
                    vec![],
                ),
                Op::Type => (
                    "browser_type",
                    TOOLS[3].3,
                    IndexMap::from([
                        ("ref".to_string(), reference("preferred")),
                        ("selector".to_string(), json!({"type": "string", "description": "CSS selector fallback; give exactly one of ref/selector."})),
                        ("text".to_string(), json!({"type": "string", "description": "Text to type. CJK and emoji are supported."})),
                        ("submit".to_string(), json!({"type": "boolean", "default": false, "description": "Press Enter after typing."})),
                    ]),
                    vec!["text".into()],
                ),
                Op::Screenshot => (
                    "browser_screenshot",
                    TOOLS[4].3,
                    IndexMap::from([(
                        "full_page".to_string(),
                        json!({"type": "boolean", "default": false, "description": "Capture beyond the viewport instead of just what is visible."}),
                    )]),
                    vec![],
                ),
                Op::Close => ("browser_close", TOOLS[5].3, IndexMap::new(), vec![]),
            Op::Wait => (
                "browser_wait",
                TOOLS[6].3,
                IndexMap::from([
                    ("selector".to_string(), json!({"type": "string", "description": "CSS selector to wait for."})),
                    ("timeout_ms".to_string(), json!({"type": "integer", "minimum": 500, "maximum": 30000, "default": 5000})),
                ]),
                vec!["selector".into()],
            ),
            Op::Scroll => (
                "browser_scroll",
                TOOLS[7].3,
                IndexMap::from([
                    ("by".to_string(), json!({"type": "integer", "description": "Pixels to scroll vertically; negative scrolls up."})),
                    ("target".to_string(), json!({"type": "string", "enum": ["top", "bottom"], "description": "Scroll to page top or bottom."})),
                    ("ref".to_string(), reference("preferred")),
                    ("selector".to_string(), json!({"type": "string", "description": "CSS selector fallback; give exactly one of by/target/ref/selector."})),
                ]),
                vec![],
            ),
            };
        // 持久 profile（P5）：所有浏览器工具统一接受可选 `profile`，
        // 同名复用同一会话与登录态；缺省一次性 profile。
        props.insert(
            "profile".to_string(),
            json!({"type": "string", "description": "持久 profile 名（小写字母/数字/-/_）。同名调用复用同一浏览器会话与登录态（~/.hermix/browser/profiles/<名>）；缺省用一次性 profile。需要网站登录态时才用。"}),
        );
        tool_schema(name, desc, JsonSchema::object(props, required))
    }
}

async fn capture(page: &Page, full_page: bool) -> Result<Vec<u8>, ToolError> {
    use chromiumoxide::cdp::browser_protocol::page::{
        CaptureScreenshotFormat, CaptureScreenshotParams,
    };
    let shot = page
        .execute(CaptureScreenshotParams {
            format: Some(CaptureScreenshotFormat::Png),
            capture_beyond_viewport: Some(full_page),
            optimize_for_speed: Some(false),
            ..Default::default()
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("截图失败: {e}")))?;
    decode_cdp_binary(&shot.result.data)
}

/// CDP hands binary payloads back as base64 **text**.
///
/// `chromiumoxide_types::Binary` looks like it decodes — it implements
/// `AsRef<[u8]>` — but that impl is `self.0.as_bytes()`, i.e. the ASCII of the
/// base64 string. Trusting it writes a text file named `.png` and attaches it
/// as `image/png`, which no renderer will display.
fn decode_cdp_binary(data: &chromiumoxide::types::Binary) -> Result<Vec<u8>, ToolError> {
    use base64::Engine as _;
    // Annotated because `Binary` implements both `AsRef<str>` and
    // `AsRef<[u8]>`; only the str form is the base64 payload.
    let encoded: &str = data.as_ref();
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| ToolError::ExecutionFailed(format!("CDP base64 解码失败: {e}")))
}

/// The saved path is the last non-empty line, which is what makes this parse
/// independent of the prose around it. Only ever called on a screenshot
/// message, which is the one shape that ends in a bare path line.
fn path_from_message(text: &str) -> Option<PathBuf> {
    let last = text.lines().filter(|l| !l.trim().is_empty()).next_back()?;
    Some(PathBuf::from(last.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_documented_schemes_are_navigable() {
        assert!(validate_url("https://example.com").is_ok());
        assert!(validate_url("http://localhost:3000").is_ok());
        assert!(validate_url("about:blank").is_ok());
        assert!(validate_url("file:///C:/tmp/a.html").is_ok());
        // The dangerous one: script in the session's own origin.
        assert!(validate_url("javascript:alert(1)").is_err());
        assert!(validate_url("vbscript:x").is_err());
        assert!(validate_url("data:text/html,<script>x</script>").is_ok());
        assert!(validate_url("  HTTPS://Example.com  ").is_ok());
    }

    #[test]
    fn ref_and_selector_are_mutually_exclusive() {
        let both = json!({"ref": "e1", "selector": "button"});
        assert!(exactly_one(&both, "ref", "selector").is_err());
        assert!(exactly_one(&json!({"ref": "e1"}), "ref", "selector").is_ok());
        assert!(exactly_one(&json!({"selector": "#a"}), "ref", "selector").is_ok());
        let neither = json!({});
        let err = exactly_one(&neither, "ref", "selector").unwrap_err().to_string();
        assert!(err.contains("browser_snapshot"), "{err}");
    }

    #[test]
    fn a_ref_from_an_earlier_snapshot_is_refused() {
        let mut refs = HashMap::new();
        refs.insert("e1".to_string(), BackendNodeId::new(7));
        assert!(resolve_ref(&refs, "e1").is_ok());
        // A snapshot replaces the table wholesale, so the ref the model still
        // remembers is gone — and says so instead of hitting the wrong element.
        let err = resolve_ref(&HashMap::new(), "e1").unwrap_err().to_string();
        assert!(err.contains("browser_snapshot"), "{err}");
        assert!(resolve_ref(&refs, "e99").is_err());
    }

    #[test]
    fn limits_are_clamped_at_both_ends() {
        let p = json!({"max_nodes": 999_999, "max_depth": 0});
        assert_eq!(opt_usize(&p, "max_nodes", 800, MAX_NODES), MAX_NODES);
        assert_eq!(opt_usize(&p, "max_depth", 12, MAX_DEPTH), 1);
        assert_eq!(opt_usize(&json!({}), "max_nodes", 800, MAX_NODES), 800);
    }

    #[test]
    fn result_cap_never_splits_a_multibyte_char() {
        let capped = truncate_chars(&"字".repeat(40_000), RESULT_CAP);
        assert_eq!(capped.chars().count(), RESULT_CAP);
    }

    #[test]
    fn reading_tools_are_safe_and_acting_tools_are_caution() {
        for (name, _, risk, _) in TOOLS {
            let expected = match name {
                "browser_snapshot" | "browser_screenshot" | "browser_close" | "browser_wait"
                | "browser_scroll" => RiskLevel::Safe,
                _ => RiskLevel::Caution,
            };
            assert_eq!(risk, expected, "{name}");
        }
    }

    #[test]
    fn every_tool_carries_a_description_for_the_model() {
        for (name, _, _, desc) in TOOLS {
            assert!(!desc.trim().is_empty(), "{name}");
            assert!(!name.is_empty());
        }
    }

    #[test]
    fn cdp_binary_is_base64_text_and_needs_decoding() {
        // 1x1 transparent PNG. Regression guard: `Binary::as_ref::<[u8]>()`
        // yields these ASCII bytes, not the image, and writing *those* to a
        // `.png` produces a file nothing can render.
        const PNG_B64: &str =
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
        let binary = chromiumoxide::types::Binary::from(PNG_B64.to_string());
        let raw: &[u8] = binary.as_ref();
        assert_eq!(
            raw,
            PNG_B64.as_bytes(),
            "AsRef<[u8]> must stay understood as \"the base64 text\", not the image"
        );
        let png = decode_cdp_binary(&binary).expect("decodes");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "magic bytes after decode");
    }

    #[test]
    fn screenshot_message_round_trips_its_path() {
        let path = PathBuf::from(r"C:\hermix\shots\a.png");
        let msg = format!("已保存截图（视口，12 KB）：\n{}", path.display());
        assert_eq!(path_from_message(&msg).as_deref(), Some(path.as_path()));
        // The trailing line is the contract, so the prose above it can change
        // freely without breaking the round trip.
        let reworded = format!("截图已存好：\n\n{}", path.display());
        assert_eq!(path_from_message(&reworded).as_deref(), Some(path.as_path()));
        assert_eq!(path_from_message(""), None);
    }
}