//! Computer-Use Automation (CUA) tool handlers.
//!
//! Thin `ToolHandler` wrappers around the `hermix-cua` crate.
//! Each handler:
//!
//! 1. Parses and validates the LLM-supplied parameters.
//! 2. Calls into `hermix-cua` (synchronous, blocking — wrapped
//!    in `spawn_blocking` so the agent loop stays responsive).
//! 3. Serialises the result to the canonical JSON string the
//!    dispatcher expects.
//!
//! ## Shared state
//!
//! A single `SafetyGuard` and a single `TraceRecorder` are kept
//! in `Lazy` statics — every handler reads the same guard
//! (so the rate limiter counts across handlers) and the same
//! recorder (so the trace file shows the cross-tool action
//! sequence). The recorder writes to
//! `<HERMIX_HOME>/cua-trace.jsonl` when the home is reachable;
//! otherwise it falls back to the null sink.
//!
//! ## Risk classification
//!
//! Two independent vocabularies meet here, and they answer
//! different questions:
//!
//! * [`CoreRisk`] (`Safe` / `Caution` / `Dangerous`) is attached at
//!   registration and is the *only* input to
//!   `decide_pre_tool_dispatch`, which prompts the user at
//!   `Dangerous` under `PermissionLevel::Default`.
//! * [`RiskLevel`] (`Low` / `Medium` / `High`) is the per-invocation
//!   tier handed to [`SafetyGuard::check`]. `Medium`/`High` are
//!   rate-limited through a shared sliding window; `Low` is exempt.
//!
//! They do not have to agree. `cua_screenshot` is `Caution` at the
//! registration layer (it reads the user's screen) but `Low` at the
//! invocation layer (it changes nothing).
//!
//! Every tool that reaches the OS input pipeline or mutates window /
//! UIA state must pass a `Medium`-or-higher `check` *before* the
//! side effect, not after.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose;
use base64::Engine;
use indexmap::IndexMap;
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema, traits::RiskLevel as CoreRisk};
use hermix_cua::{
    action::{self, MouseButton},
    capture::{self, CaptureConfig, MonitorInfo, ScreenshotResult},
    element::{WindowFilter, WindowInfo},
    safety::{RiskLevel, SafetyGuard},
    trace::{TraceEvent, TraceKind, TraceRecorder},
};

use crate::ToolRegistry;

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register all CUA (computer-use automation) tool handlers.
pub fn register(registry: &ToolRegistry) {
    // Tag every CUA handler with the *group* name
    // `"cua"`, not the per-tool name. `essential::ESSENTIAL_TOOLSETS`
    // references `"cua"` as a single toolset, and the
    // `set_enabled_toolsets` UI toggle keys on this tag. Tagging
    // each handler with its own name would have caused the toggle
    // to silently miss every CUA tool and the essential floor to
    // no-op on this group.
    let register_one = |name: &str, label: &str, risk: CoreRisk, handler: Arc<dyn ToolHandler>| {
        registry.register_with_risk(
            name,
            "cua",
            handler.schema(),
            handler,
            Arc::new(|| true),
            vec![],
            true,
            "Computer-Use Automation tool",
            label,
            None,
            risk,
            false,
        );
    };

    // Risk tiers. `Dangerous` = mutates the machine (click / type /
    // key / invoke / focus) and is the only tier that fires the
    // approval card in `PermissionLevel::Default`.
    // `Caution` = read-only, but it reads *the user's screen and
    // window titles*, which can surface credentials or private
    // document names in the transcript. Keeping these one tier
    // below `Safe` records that without making the agent prompt for
    // a permission slip on every `cua_screenshot`.
    // `Safe` = no observable side effect at all (currently only
    // `cua_move_cursor`).
    register_one("cua_screenshot", "📸", CoreRisk::Caution, Arc::new(CuaScreenshotHandler::new()));
    register_one("cua_screen_info", "🖥️", CoreRisk::Caution, Arc::new(CuaScreenInfoHandler::new()));
    register_one("cua_list_windows", "🪟", CoreRisk::Caution, Arc::new(CuaListWindowsHandler::new()));
    register_one("cua_window_at_point", "🎯", CoreRisk::Caution, Arc::new(CuaWindowAtPointHandler::new()));
    register_one("cua_ui_tree", "🌳", CoreRisk::Caution, Arc::new(CuaUiTreeHandler::new()));
    register_one("cua_ui_find", "🔎", CoreRisk::Caution, Arc::new(CuaUiFindHandler::new()));
    register_one("cua_left_click", "🖱️", CoreRisk::Dangerous, Arc::new(CuaLeftClickHandler::new()));
    register_one("cua_type_text", "⌨️", CoreRisk::Dangerous, Arc::new(CuaTypeTextHandler::new()));
    register_one("cua_press_key", "🔘", CoreRisk::Dangerous, Arc::new(CuaPressKeyHandler::new()));
    register_one("cua_click_window", "👆", CoreRisk::Dangerous, Arc::new(CuaClickWindowHandler::new()));
    register_one("cua_ui_click", "👆", CoreRisk::Dangerous, Arc::new(CuaUiClickHandler::new()));
    register_one("cua_ui_focus", "🎯", CoreRisk::Dangerous, Arc::new(CuaUiFocusHandler::new()));
    register_one("cua_ui_invoke", "⚙️", CoreRisk::Dangerous, Arc::new(CuaUiInvokeHandler::new()));
    //mouse handlers (audit fix): these three were
    // previously defined in `all_handlers()` factory but never wired into
    // `register()`, so the LLM could not actually invoke them despite
    // the 17/17 "CUA tests pass" green light (tests only verified the
    // struct, not the live registry entry).
    register_one("cua_scroll", "🔄", CoreRisk::Dangerous, Arc::new(CuaScrollHandler::new()));
    register_one("cua_double_click", "🖱️", CoreRisk::Dangerous, Arc::new(CuaDoubleClickHandler::new()));
    register_one("cua_move_cursor", "↔️", CoreRisk::Safe, Arc::new(CuaMoveCursorHandler::new()));
}

// ---------------------------------------------------------------------------
// Shared guard + recorder
// ---------------------------------------------------------------------------

/// Process-wide safety guard. Shared across all five CUA handlers
/// so the rate limiter sees the cross-tool action stream.
static SAFETY: Lazy<SafetyGuard> = Lazy::new(SafetyGuard::new);

/// Process-wide trace recorder. Writes to `<HERMIX_HOME>/cua-trace.jsonl`;
/// falls back to the null sink when the home directory is not
/// writable (e.g. very early in startup, or in tests).
static TRACE: Lazy<TraceRecorder> = Lazy::new(|| {
    let path = hermix_config::paths::hermix_home().join("cua-trace.jsonl");
    TraceRecorder::open(&path).unwrap_or_else(|_| TraceRecorder::null())
});

/// Default per-call timeout for any single desktop-automation
/// action. Mirrors the 10-second budget
/// `computer_use::runtime_blocking_timeout` already uses — any
/// longer and the agent loop starves the user's UI.
const ACTION_TIMEOUT: Duration = Duration::from_secs(10);

/// Run a synchronous `hermix-cua` closure on the blocking pool,
/// racing it against `ACTION_TIMEOUT`. The closure must be
/// infallible wrt the timeout — we cannot cancel the OS input
/// pipeline mid-call, but we can drop the `JoinHandle` and let
/// the work continue in the background while the agent loop
/// surfaces a `Timeout` error to the LLM.
async fn run_blocking<F, R>(work: F) -> Result<R, ToolError>
where
    F: FnOnce() -> Result<R, hermix_cua::CuaError> + Send + 'static,
    R: Send + 'static,
{
    match tokio::time::timeout(ACTION_TIMEOUT, tokio::task::spawn_blocking(work)).await {
        Ok(Ok(result)) => result.map_err(|e| map_cua_error(e)),
        Ok(Err(join_err)) => Err(ToolError::ExecutionFailed(format!(
            "cua handler panicked: {join_err}"
        ))),
        Err(_) => Err(ToolError::Timeout(format!(
            "cua action exceeded {:?}",
            ACTION_TIMEOUT
        ))),
    }
}

/// Translate a `hermix_cua::CuaError` into the canonical
/// `ToolError` the dispatcher expects. The `kind()` short string
/// is preserved in the message so the desktop UI can render a
/// stable error code.
/// `ToolError` does not have a dedicated `PermissionDenied`
/// variant — denied actions map to `ExecutionFailed` with a
/// `denied by safety:` prefix so the dispatcher-side error
/// filter can still match on it.
fn map_cua_error(err: hermix_cua::CuaError) -> ToolError {
    use hermix_cua::CuaError;
    match err {
        CuaError::Denied(msg) => {
            ToolError::ExecutionFailed(format!("denied by safety: {msg}"))
        }
        CuaError::Timeout(_) => ToolError::Timeout(format!("{:?}", ACTION_TIMEOUT)),
        CuaError::InvalidParams(msg) => ToolError::InvalidParams(msg),
        CuaError::NotFound(msg) => ToolError::NotFound(msg),
        other => ToolError::from_message(format!("cua::{}: {}", other.kind(), other)),
    }
}

// ---------------------------------------------------------------------------
// Session id helper — every trace line carries a session id so
// the audit log can be grouped per task. We accept it as a
// parameter rather than reaching into globals; the dispatcher
// already passes it via the `tool_call_id` field on the JSON
// payload it forwards to the handler.
// ---------------------------------------------------------------------------

fn extract_session_id(params: &Value) -> String {
    params
        .get("_session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// 解析可选的 `hwnd`。Windows 句柄恒为正，0 与负值要么是无意义的调用，
/// 要么会让 UIA 拿 NULL 句柄、回退成从桌面根遍历——那正是"传了 hwnd
/// 却仍拿到整棵桌面树"的现象。两种都显式报错，不留静默路径。
fn parse_optional_hwnd(params: &Value) -> Result<Option<isize>, ToolError> {
    match params.get("hwnd").and_then(|v| v.as_i64()) {
        None => Ok(None),
        Some(h) if h > 0 => Ok(Some(h as isize)),
        Some(h) => Err(ToolError::InvalidParams(format!(
            "hwnd must be a positive isize (got {h})"
        ))),
    }
}

/// 解析必需的 `hwnd`：没有它就无从下手的工具（按句柄点击窗口）用这条。
fn parse_required_hwnd(params: &Value) -> Result<isize, ToolError> {
    parse_optional_hwnd(params)?.ok_or_else(|| ToolError::InvalidParams("missing 'hwnd'".into()))
}

/// 把截图的 base64 载荷写成 PNG 文件，返回写入字节数。
///
/// 走这条路时像素不进上下文：1920×1080 的帧 base64 后是 1–2 MB，而每次
/// 工具结果都会留在对话历史里被后续每一轮重复计费——这是 cua 工具
/// token 成本的主要来源。给了 `save_to` 就只回路径和尺寸。
fn save_png_base64(png_base64: &str, path: &Path) -> Result<usize, ToolError> {
    let bytes = general_purpose::STANDARD
        .decode(png_base64)
        .map_err(|e| ToolError::ExecutionFailed(format!("screenshot base64 decode failed: {e}")))?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| {
                ToolError::ExecutionFailed(format!("create_dir_all({}): {e}", parent.display()))
            })?;
        }
    }
    std::fs::write(path, &bytes)
        .map_err(|e| ToolError::ExecutionFailed(format!("write({}): {e}", path.display())))?;
    Ok(bytes.len())
}

/// 读取可选的 `save_to`。空串按未指定处理，免得写出一个空名文件。
fn save_to_path(params: &Value) -> Option<PathBuf> {
    params
        .get("save_to")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// 进程内自增序号：保证同毫秒内连拍也不重名。
static CAPTURE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 未指定 `save_to` 时的默认落盘位：`<hermix_home>/captures/`。
/// 像素永远不进上下文——UI 经 asset 协议从 `saved_to` 渲染（该目录已在
/// assetProtocol scope 内），子 agent 由 processor 从盘上读回注入视觉
/// 块，主上下文零 base64。
fn default_capture_path(monitor: usize) -> PathBuf {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = CAPTURE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    hermix_config::paths::hermix_home()
        .join("captures")
        .join(format!("capture-{millis}-{seq}-m{monitor}.png"))
}

/// captures/ 的保留窗口与清理限频间隔。截图目录会无限增长，默认落盘
/// 时顺带把过期文件扫掉。
const CAPTURE_RETENTION: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);
const CAPTURE_PRUNE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3600);
/// 上次清理的 unix 秒；0 表示本次进程还没清理过。
static LAST_CAPTURE_PRUNE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 限频入口：默认落盘分支每次都调，但目录扫描至多每小时进行一次。
/// 失败一律静默——清理是顺手维护，不能反过来弄失败截图本身。
fn prune_stale_captures(dir: &Path) {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let last = LAST_CAPTURE_PRUNE.load(std::sync::atomic::Ordering::Relaxed);
    if last != 0 && now_secs.saturating_sub(last) < CAPTURE_PRUNE_INTERVAL.as_secs() {
        return;
    }
    LAST_CAPTURE_PRUNE.store(now_secs, std::sync::atomic::Ordering::Relaxed);
    let _ = sweep_stale_captures(dir, std::time::SystemTime::now());
}

/// 扫描 `dir`，删除 `capture-*.png` 命名（本工具自己的落盘模式）且
/// 修改时间早于保留窗口的文件，返回删除数。子目录与其他命名的文件
/// 一律不碰——目录里可能有用户手动放的东西，只清理自家产物。
fn sweep_stale_captures(dir: &Path, now: std::time::SystemTime) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !(name.starts_with("capture-") && name.ends_with(".png")) {
            continue;
        }
        let Ok(modified) = meta.modified() else { continue };
        let age = now.duration_since(modified).unwrap_or_default();
        if age >= CAPTURE_RETENTION && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

// ---------------------------------------------------------------------------
// 1) cua_screenshot
// ---------------------------------------------------------------------------

/// Capture the primary monitor and return a JSON envelope carrying the
/// saved PNG path and the captured frame's dimensions — **never** the
/// base64 pixels (they live on disk; the desktop UI renders `saved_to`
/// via the asset protocol, and the sub-agent vision path reads the file
/// back in `tool_result_processor`).
pub struct CuaScreenshotHandler;

impl CuaScreenshotHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaScreenshotHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaScreenshotHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        SAFETY
            .check(None, RiskLevel::Low)
            .map_err(map_cua_error)?;
        let monitor = params.get("monitor").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let config = CaptureConfig { monitor };
        let session_id = extract_session_id(&params);
        let started = std::time::Instant::now();
        let result: Result<ScreenshotResult, ToolError> =
            run_blocking(move || capture::screenshot(config)).await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let event = match &result {
            Ok(r) => TraceEvent::new(
                TraceKind::Screenshot,
                &session_id,
                format!("monitor={} {}x{}", r.monitor, r.width, r.height),
            )
            .with_elapsed_ms(elapsed_ms),
            Err(e) => TraceEvent::new(
                TraceKind::Screenshot,
                &session_id,
                format!("monitor={monitor}"),
            )
            .with_elapsed_ms(elapsed_ms)
            .with_error(&hermix_cua::CuaError::Gdi(e.to_string())),
        };
        let _ = TRACE.record(&event);
        let result = result?;
        // 落盘分支（无条件）：像素永不进上下文。显式 `save_to` 用调用方
        // 的路径，否则落到 <hermix_home>/captures/ 下；结果只带路径与
        // 尺寸——UI 经 asset 协议渲染 `saved_to`，子 agent 由 processor
        // 从盘上读回。1920×1080 的帧 base64 后 1–2 MB，留进对话历史会被
        // 后续每一轮重复计费，这是 cua 工具 token 成本的主要来源。
        let path = match save_to_path(&params) {
            Some(p) => p,
            None => {
                // 只清理默认目录；显式 save_to 的文件归调用方管，绝不碰。
                prune_stale_captures(&hermix_config::paths::hermix_home().join("captures"));
                default_capture_path(result.monitor)
            }
        };
        let bytes = save_png_base64(&result.png_base64, &path)?;
        Ok(json!({
            "tool": "cua_screenshot",
            "monitor": result.monitor,
            "monitor_name": result.monitor_name,
            "width": result.width,
            "height": result.height,
            "bytes": bytes,
            "saved_to": path.display().to_string(),
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "monitor".into(),
            json!({
                "type": "integer",
                "minimum": 0,
                "description": "Display index (0 = primary). Phase 1 only supports 0."
            }),
        );
        props.insert(
            "save_to".into(),
            json!({
                "type": "string",
                "description": "Optional explicit destination for the PNG. When omitted the \
                                frame is auto-saved under <hermix_home>/captures/ — either way \
                                the result carries only the path + dimensions, never pixels."
            }),
        );
        tool_schema(
            "cua_screenshot",
            "Capture the primary monitor. The PNG is written to disk (auto path under \
             <hermix_home>/captures/, or the explicit `save_to` path) and the result \
             carries only `saved_to` + frame dimensions — never base64 pixels, so the \
             conversation context stays small (a 1920x1080 frame would otherwise cost \
             1-2 MB of base64 that every later turn pays for again). The desktop UI \
             renders the saved file automatically. For model vision over the frame, \
             delegate to a sub-agent (the pixels are injected there) or read specific \
             regions with other tools.\n\
             \n\
             ## When to use this (vs. `computer_use`)\n\
             `cua_screenshot` is the lower-latency sibling of the existing `computer_use`\n\
             tool — it uses the same wire format but a different capture backend\n\
             (BitBlt vs. xcap). For tasks that only need a screenshot (not a full\n\
             `computer_use` session), prefer `cua_screenshot` because it does not\n\
             import the input-pipeline crates and is therefore ~2× faster on first\n\
             call.\n\
             \n\
             ## Coordinate system\n\
             All click / move / type coordinates returned by subsequent tools are\n\
             in the same pixel coordinate space as the screenshot.",
            JsonSchema::object(props, Vec::new()),
        )
    }
}

// ---------------------------------------------------------------------------
// 2) cua_screen_info
// ---------------------------------------------------------------------------

/// Return the list of connected displays .
pub struct CuaScreenInfoHandler;

impl CuaScreenInfoHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaScreenInfoHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaScreenInfoHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        SAFETY
            .check(None, RiskLevel::Low)
            .map_err(map_cua_error)?;
        let session_id = extract_session_id(&params);
        let started = std::time::Instant::now();
        let result: Result<Vec<MonitorInfo>, ToolError> =
            run_blocking(|| capture::screen_info::enumerate()).await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let event = match &result {
            Ok(list) => TraceEvent::new(
                TraceKind::ScreenInfo,
                &session_id,
                format!("monitors={}", list.len()),
            )
            .with_elapsed_ms(elapsed_ms),
            Err(e) => TraceEvent::new(TraceKind::ScreenInfo, &session_id, "")
                .with_elapsed_ms(elapsed_ms)
                .with_error(&hermix_cua::CuaError::Gdi(e.to_string())),
        };
        let _ = TRACE.record(&event);
        let result = result?;
        serde_json::to_string_pretty(&result).map_err(|e| {
            ToolError::ExecutionFailed(format!("screen_info JSON encode failed: {e}"))
        })
    }

    fn schema(&self) -> ToolSchema {
        let props = IndexMap::new();
        tool_schema(
            "cua_screen_info",
            "Return the list of connected displays.\n\
             \n\
             Phase 1 returns the primary monitor only (Phase 2 will use\n\
             `EnumDisplayMonitors` + `GetMonitorInfo` to enumerate every display\n\
             in the virtual desktop coordinate space).\n\
             \n\
             ## When to call\n\
             The LLM should call `cua_screen_info` once at the start of a\n\
             Computer-Use session to learn the primary monitor's pixel\n\
             dimensions. All subsequent `cua_left_click` / `cua_type_text` /\n\
             `cua_press_key` coordinates are interpreted in the same coordinate\n\
             space.",
            JsonSchema::object(props, Vec::new()),
        )
    }
}

// ---------------------------------------------------------------------------
// 3) cua_left_click
// ---------------------------------------------------------------------------

/// Mouse click at `(x, y)`. Defaults to `button="left"`; the
/// right and middle buttons are accepted for completeness.
pub struct CuaLeftClickHandler;

impl CuaLeftClickHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaLeftClickHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaLeftClickHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let x = params
            .get("x")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| ToolError::InvalidParams("missing 'x' coordinate".into()))?;
        let y = params
            .get("y")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| ToolError::InvalidParams("missing 'y' coordinate".into()))?;
        if x < 0 || y < 0 {
            return Err(ToolError::InvalidParams(format!(
                "coordinates must be non-negative (got x={x}, y={y})"
            )));
        }
        if x > i32::MAX as i64 || y > i32::MAX as i64 {
            return Err(ToolError::InvalidParams(format!(
                "coordinates overflow i32 (got x={x}, y={y})"
            )));
        }
        let button_str = params
            .get("button")
            .and_then(|v| v.as_str())
            .unwrap_or("left");
        let button = MouseButton::parse(button_str).map_err(|e| match e {
            hermix_cua::CuaError::InvalidParams(m) => ToolError::InvalidParams(m),
            other => ToolError::InvalidParams(other.to_string()),
        })?;
        let session_id = extract_session_id(&params);

        let safety = &*SAFETY;
        let trace = &*TRACE;
        run_blocking(move || action::click(safety, trace, &session_id, x as i32, y as i32, button))
            .await?;

        Ok(json!({
            "tool": "cua_left_click",
            "x": x, "y": y,
            "button": button_str,
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "x".into(),
            json!({"type": "integer", "description": "X coordinate (pixels from top-left of primary display)."}),
        );
        props.insert(
            "y".into(),
            json!({"type": "integer", "description": "Y coordinate (pixels from top-left of primary display)."}),
        );
        props.insert(
            "button".into(),
            json!({
                "type": "string",
                "enum": ["left", "right", "middle"],
                "description": "Mouse button. Default: left."
            }),
        );
        tool_schema(
            "cua_left_click",
            "Click at (x, y) using `SendInput`. Defaults to a left click; `right`\n\
             and `middle` are also accepted.\n\
             \n\
             ## Coordinate space\n\
             The (x, y) values are pixel coordinates relative to the primary\n\
             monitor's top-left. Call `cua_screen_info` first to learn the\n\
             monitor's pixel dimensions, and use coordinates from a previous\n\
             `cua_screenshot` to land on the right pixel.\n\
             \n\
             ## When to use vs. `computer_use`\n\
             `cua_left_click` is a Phase-1 standalone — it does not require the\n\
             `computer_use` tool session to be open and can be called from any\n\
             agent. Prefer `cua_left_click` for one-off clicks; reach for\n\
             `computer_use` only when you need the full action surface (double\n\
             click, scroll, clipboard, multi-display).",
            JsonSchema::object(props, vec!["x".into(), "y".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// 4) cua_type_text
// ---------------------------------------------------------------------------

/// Type a string of Unicode text into whatever window currently
/// has focus. CJK, emoji, and combining marks all work (the
/// injection uses `KEYEVENTF_UNICODE`).
pub struct CuaTypeTextHandler;

impl CuaTypeTextHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaTypeTextHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaTypeTextHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let text = params
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("missing 'text'".into()))?;
        let session_id = extract_session_id(&params);

        let safety = &*SAFETY;
        let trace = &*TRACE;
        let text_owned = text.to_string();
        let n_chars = text_owned.chars().count();
        run_blocking(move || action::type_text(safety, trace, &session_id, &text_owned)).await?;

        Ok(json!({
            "tool": "cua_type_text",
            "characters_typed": n_chars,
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "text".into(),
            json!({"type": "string", "description": "Text to type into the currently focused window. Unicode is supported (CJK, emoji, combining marks)."}),
        );
        tool_schema(
            "cua_type_text",
            "Type a string of Unicode text using `SendInput` with `KEYEVENTF_UNICODE`.\n\
             \n\
             ## Limits\n\
             - Empty strings are rejected.\n\
             - NUL bytes are rejected.\n\
             - Strings over 100,000 characters are rejected.\n\
             \n\
             ## Coordinate independence\n\
             `cua_type_text` does not take coordinates — it types into whatever\n\
             window currently has keyboard focus. The agent must arrange focus\n\
             (e.g. via `cua_left_click` on the target text field) before calling\n\
             this tool.",
            JsonSchema::object(props, vec!["text".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// 5) cua_press_key
// ---------------------------------------------------------------------------

/// Press a key chord (e.g. `"ctrl+shift+t"`, `"Return"`,
/// `"alt+F4"`). Blacklisted chords (`ctrl+alt+delete`, `win+l`,
/// `alt+f4`, etc.) are refused before any input is injected.
pub struct CuaPressKeyHandler;

impl CuaPressKeyHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaPressKeyHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaPressKeyHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let chord = params
            .get("chord")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("missing 'chord'".into()))?;
        let session_id = extract_session_id(&params);

        let safety = &*SAFETY;
        let trace = &*TRACE;
        let chord_owned = chord.to_string();
        run_blocking(move || action::press_key(safety, trace, &session_id, &chord_owned)).await?;

        Ok(json!({
            "tool": "cua_press_key",
            "chord": chord,
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "chord".into(),
            json!({
                "type": "string",
                "description": "Key chord like 'ctrl+shift+t', 'Return', 'alt+F4'. Single printable characters are accepted as 'a', '5', etc."
            }),
        );
        tool_schema(
            "cua_press_key",
            "Press a key chord using `SendInput`.\n\
             \n\
             ## Blacklist\n\
             The following chords are refused by the safety guard before any\n\
             OS-level injection happens:\n\
             - `ctrl+alt+delete`, `ctrl+alt+del` — secure attention sequence\n\
             - `win+l`, `meta+l`, `super+l` — lock workstation\n\
             - `alt+f4` — force-close foreground window\n\
             - `ctrl+alt+end`, `ctrl+win+delete` — shutdown / log-off / restart\n\
             - `win+x`, `win+d` — power-user menu, show-desktop\n\
             \n\
             Attempting any of these returns a `Denied` error and never reaches\n\
             the OS input pipeline.\n\
             \n\
             ## Modifier order\n\
             Modifiers are sorted before comparison, so `ctrl+shift+t` and\n\
             `shift+ctrl+t` both match the blacklist identically. The actual\n\
             injection always presses the modifier `DOWN` first, then the tap\n\
             key, then releases the modifiers in reverse order.",
            JsonSchema::object(props, vec!["chord".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// 6) cua_list_windows
// ---------------------------------------------------------------------------

/// Enumerate the desktop's top-level windows. Filters (all optional):
/// - `title` — case-insensitive substring match against the window
/// title. Empty / missing → match all titles.
/// - `visible_only` — default `true`; excludes hidden / owner-drawn
/// tool windows. Set `false` to enumerate everything.
/// - `max_results` — default `100`. The full desktop enumeration
/// rarely exceeds a few dozen visible windows; the cap is a safety
/// belt for the LLM context window, not a performance limit.
pub struct CuaListWindowsHandler;

impl CuaListWindowsHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaListWindowsHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaListWindowsHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        SAFETY
            .check(None, RiskLevel::Low)
            .map_err(map_cua_error)?;
        let title = params
            .get("title")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let visible_only = crate::param_coercion::bool_param(&params, "visible_only", true);
        let max_results = params
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(100) as usize;
        let session_id = extract_session_id(&params);

        let filter = WindowFilter {
            title: title.clone(),
            class_name: None,
            process_id: None,
            visible_only,
        };

        let started = std::time::Instant::now();
        let result: Result<Vec<WindowInfo>, ToolError> = run_blocking(
            move || hermix_cua::element::find_all_windows(&filter),
        )
        .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let mut windows = result?;
        if windows.len() > max_results {
            windows.truncate(max_results);
        }
        let n = windows.len();
        let event = TraceEvent::new(
            TraceKind::Other {
                subkind: "list_windows".into(),
            },
            &session_id,
            format!(
                "matched={n} title_filter={:?} visible_only={visible_only}",
                title
            ),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&event);
        Ok(json!({
            "tool": "cua_list_windows",
            "matched": n,
            "truncated": n >= max_results,
            "windows": windows,
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "title".into(),
            json!({
                "type": "string",
                "description": "Case-insensitive substring filter for the window title. Optional — omit to list all windows."
            }),
        );
        props.insert(
            "visible_only".into(),
            json!({
                "type": "boolean",
                "description": "When true (default), only visible windows are returned.",
                "default": true,
            }),
        );
        props.insert(
            "max_results".into(),
            json!({
                "type": "integer",
                "minimum": 1,
                "maximum": 1000,
                "description": "Cap on the number of returned windows. Default: 100.",
            }),
        );
        tool_schema(
            "cua_list_windows",
            "Enumerate top-level windows via Win32 `EnumWindows`.\n\
             \n\
             ## Returns\n\
             A JSON object with a `windows` array. Each entry has:\n\
             - `hwnd` — raw window handle (isize)\n\
             - `title` — `GetWindowTextW` result, may be empty\n\
             - `class_name` — `GetClassNameW` (e.g. `Notepad`, `#32770`, `Shell_TrayWnd`)\n\
             - `process_id` — owning PID\n\
             - `is_visible` — `IsWindowVisible`\n\
             - `rect` — `{x, y, width, height}` from `GetWindowRect`\n\
             \n\
             ## When to use\n\
             Call `cua_list_windows` to discover what apps are open\n\
             before clicking into one. Pair with `cua_window_at_point`\n\
             to figure out *which* window is under a screen coordinate\n\
             the user mentioned. Call `cua_click_window` to focus +\n\
             click into the centre of a window by its hwnd.\n\
             \n\
             ## Layer\n\
             This is the **Level-1 Win32 layer** (Phase 2-B). The full\n\
             UIA `IUIAutomation` tree (Phase 3) returns richer elements\n\
             (buttons, text fields, check boxes) with names and\n\
             automation IDs.",
            JsonSchema::object(props, Vec::new()),
        )
    }
}

// ---------------------------------------------------------------------------
// 7) cua_window_at_point
// ---------------------------------------------------------------------------

/// Return the top-level window under screen coordinate `(x, y)`.
/// Uses `WindowFromPoint` + a `IsWindow` validity check so a
/// stale / closed window cannot leak through.
/// Result is `null` when the point hits no window (e.g. outside
/// the virtual desktop).
pub struct CuaWindowAtPointHandler;

impl CuaWindowAtPointHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaWindowAtPointHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaWindowAtPointHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        SAFETY
            .check(None, RiskLevel::Low)
            .map_err(map_cua_error)?;
        let x = params
            .get("x")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| ToolError::InvalidParams("missing 'x' coordinate".into()))?;
        let y = params
            .get("y")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| ToolError::InvalidParams("missing 'y' coordinate".into()))?;
        if x < 0 || y < 0 {
            return Err(ToolError::InvalidParams(format!(
                "coordinates must be non-negative (got x={x}, y={y})"
            )));
        }
        if x > i32::MAX as i64 || y > i32::MAX as i64 {
            return Err(ToolError::InvalidParams(format!(
                "coordinates overflow i32 (got x={x}, y={y})"
            )));
        }
        let session_id = extract_session_id(&params);

        let started = std::time::Instant::now();
        let result: Result<Option<WindowInfo>, ToolError> = run_blocking(
            move || hermix_cua::element::window_at_point(x as i32, y as i32),
        )
        .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let window = result?;
        let event = TraceEvent::new(
            TraceKind::Other {
                subkind: "window_at_point".into(),
            },
            &session_id,
            format!("x={x} y={y} hit={}", window.is_some()),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&event);
        Ok(json!({
            "tool": "cua_window_at_point",
            "x": x,
            "y": y,
            "window": window,
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "x".into(),
            json!({"type": "integer", "description": "X coordinate in screen pixels."}),
        );
        props.insert(
            "y".into(),
            json!({"type": "integer", "description": "Y coordinate in screen pixels."}),
        );
        tool_schema(
            "cua_window_at_point",
            "Return the top-level window under screen coordinate `(x, y)`.\n\
             \n\
             Backed by `WindowFromPoint` + `IsWindow`. Use this to\n\
             figure out which application a pixel belongs to before\n\
             issuing a `cua_left_click` into the same spot — it is\n\
             the safer sibling of blind coordinate clicks because it\n\
             surfaces the target's `title` / `class_name` / `pid`\n\
             the LLM can sanity-check against its intent.\n\
             \n\
             The `window` field is `null` when the point is outside\n\
             the desktop (negative virtual-screen coords, or a\n\
             multi-monitor gap).",
            JsonSchema::object(props, vec!["x".into(), "y".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// 8) cua_click_window
// ---------------------------------------------------------------------------

/// Focus a window by its `hwnd` and click into its centre. The
/// two-step shape (focus → click) is the natural pattern for
/// "switch to app X and then interact with it" — the focus step
/// makes the target window the foreground so the click lands
/// somewhere meaningful instead of being routed to whatever app
/// the user happened to be in.
/// The click coordinate is **always** the centre of the window's
/// `GetWindowRect` — explicit `(x, y)` overrides would defeat the
/// "click into the window" abstraction and re-introduce blind
/// coordinate clicking. Use `cua_left_click` for coordinate-precise
/// clicks.
pub struct CuaClickWindowHandler;

impl CuaClickWindowHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaClickWindowHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaClickWindowHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let hwnd = parse_required_hwnd(&params)?;
        let button_str = params
            .get("button")
            .and_then(|v| v.as_str())
            .unwrap_or("left");
        let button = MouseButton::parse(button_str).map_err(|e| match e {
            hermix_cua::CuaError::InvalidParams(m) => ToolError::InvalidParams(m),
            other => ToolError::InvalidParams(other.to_string()),
        })?;
        let session_id = extract_session_id(&params);

        // Step 1 — look up the window. We need the centre of its
        // rect for the click target; doing this outside the
        // blocking closure keeps the click + focus on the same
        // thread that called `find_window` so the rect cannot
        // shift between read and click in a noticeable way.
        let started = std::time::Instant::now();
        let hwnd_isize = hwnd as isize;
        let safety = &*SAFETY;
        let trace = &*TRACE;
        let session = session_id.clone();
        let result: Result<(i32, i32), ToolError> = run_blocking(move || {
            // Charge the limiter up front: steps (a) and (b) already
            // mutate window state (restore + `SetForegroundWindow`),
            // so gating only at `action::click` would let the window
            // churn before any rate limit applies.
            safety.check(None, RiskLevel::Medium)?;
            // (a) Find the window to learn its rect.
            let win = hermix_cua::element::window_from_hwnd(hwnd_isize).ok_or_else(|| {
                hermix_cua::CuaError::NotFound(format!("window hwnd={hwnd_isize:#x} is not valid"))
            })?;
            // (b) Focus it (SetForegroundWindow + ShowWindow restore).
            hermix_cua::action::focus_window(hwnd_isize)?;
            // (c) Compute the click point — centre of the rect.
            let (cx, cy) = win.rect.center();
            if cx < 0 || cy < 0 {
                return Err(hermix_cua::CuaError::InvalidParams(format!(
                    "window hwnd={hwnd_isize:#x} has degenerate/offscreen rect ({},{})",
                    cx, cy
                )));
            }
            // (d) Issue the click through the normal action path so
            // the safety guard / trace recorder see it.
            hermix_cua::action::click(safety, trace, &session, cx, cy, button)?;
            Ok((cx, cy))
        })
        .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let (cx, cy) = result?;
        let event = TraceEvent::new(
            TraceKind::Other {
                subkind: "click_window".into(),
            },
            &session_id,
            format!("hwnd={hwnd:#x} clicked_at=({cx},{cy}) button={button_str}"),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&event);
        Ok(json!({
            "tool": "cua_click_window",
            "hwnd": hwnd,
            "clicked_at": { "x": cx, "y": cy },
            "button": button_str,
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "hwnd".into(),
            json!({
                "type": "integer",
                "description": "Window handle (isize) from a previous cua_list_windows or cua_window_at_point call."
            }),
        );
        props.insert(
            "button".into(),
            json!({
                "type": "string",
                "enum": ["left", "right", "middle"],
                "description": "Mouse button. Default: left."
            }),
        );
        tool_schema(
            "cua_click_window",
            "Focus a window by HWND and click into its centre.\n\
             \n\
             ## Steps performed\n\
             1. `WindowFromHwnd` — validate the HWND is still live\n\
             2. `SetForegroundWindow` + `ShowWindow(SW_RESTORE)` —\n\
                bring the window to the front (un-minimise if needed)\n\
             3. Compute the click target — centre of `GetWindowRect`\n\
             4. `SendInput` — left / right / middle click at that point\n\
             \n\
             ## When to use\n\
             Pair with `cua_list_windows`: the LLM first lists open\n\
             windows, picks the one matching its intent (e.g. by\n\
             title), then calls `cua_click_window` with that window's\n\
             `hwnd`. The focus step makes the target the foreground\n\
             app, so subsequent `cua_type_text` / `cua_press_key`\n\
             land in the right window.\n\
             \n\
             ## Coordinate target\n\
             Always the centre of the window — coordinate overrides\n\
             would defeat the abstraction. Use `cua_left_click` for\n\
             exact (x, y) clicks.",
            JsonSchema::object(props, vec!["hwnd".into()]),
        )
    }
}

// =========================================================================
//C: UIAutomation element-tree handlers
// =========================================================================

// Helper: run blocking COM calls with per-call CoInitializeEx.
// We construct a new UiaAutomation inside the blocking closure
// because COM apartment is per-thread and spawn_blocking gives
// us a fresh thread each time.
// Note: `UiaAutomation::new()` already calls CoInitializeEx, so
// it is safe to construct on a new thread.

macro_rules! uia_blocking {
    ($closure:expr) => {{
        tokio::task::spawn_blocking(move || -> Result<_, ToolError> {
            // Safety: spawn_blocking runs on a fresh thread pool
            // thread; COM apartment must be initialised per thread.
            let uia = unsafe {
                hermix_cua::uia::UiaAutomation::new()
                    .map_err(|e| ToolError::ExecutionFailed(format!("UIA init: {e}")))?
            };
            ($closure)(&uia)
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("blocking panic: {e}")))?
    }};
}

// -- CuaUiTreeHandler -----------------------------------------------------

/// Dump the control-view UIA element tree up to a configurable depth.
pub struct CuaUiTreeHandler;

impl CuaUiTreeHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaUiTreeHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaUiTreeHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let max_depth = params
            .get("max_depth")
            .and_then(|v| v.as_u64())
            .unwrap_or(3) as u32;
        // Absent → walk the whole desktop. Present → scope to that window;
        // a non-positive value is rejected rather than passed down, which
        // used to fall back to the desktop root silently and read as
        // "`hwnd` is ignored".
        let hwnd = parse_optional_hwnd(&params)?;
        let session_id = extract_session_id(&params);

        let started = std::time::Instant::now();
        let elements = uia_blocking!(|uia: &hermix_cua::uia::UiaAutomation| {
        // SAFETY: `uia` was constructed via `UiaAutomation::new()` which
            // initialises COM on this blocking thread.
            let tree = unsafe {
                match hwnd {
                    Some(h) => {
                        let root = uia
                            .raw_element_from_handle(h)
                            .map_err(|e| ToolError::ExecutionFailed(format!("ElementFromHandle({h}): {e}")))?;
                        uia.get_control_view_tree_from(&root, max_depth)
                    }
                    None => uia.get_control_view_tree(max_depth),
                }
                .map_err(|e| ToolError::ExecutionFailed(format!("UIA tree walk: {e}")))?
            };
            Ok::<_, ToolError>(tree)
        })?;
        let elapsed_ms = started.elapsed().as_millis() as u64;

        let count = elements.len();
        let event = TraceEvent::new(
            TraceKind::Other { subkind: "ui_tree".into() },
            &session_id,
            format!("max_depth={max_depth} elements={count}"),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&event);

        Ok(json!({
            "tool": "cua_ui_tree",
            "max_depth": max_depth,
            "element_count": count,
            "elements": elements,
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "max_depth".into(),
            json!({
                "type": "integer",
                "description": "Maximum tree depth to walk. 0 = root only, 1 = root + immediate children, etc. Default: 3."
            }),
        );
        props.insert(
            "hwnd".into(),
            json!({
                "type": "integer",
                "description": "Scope the walk to one window. Pass the handle from cua_list_windows. Omit to walk the whole desktop (returns the taskbar and every other window too — usually not what you want)."
            }),
        );
        tool_schema(
            "cua_ui_tree",
            "Get the UI Automation control-view element tree.\n\
             \n\
             Walks the desktop's UIA element tree depth-first up to\n\
             `max_depth` levels and returns every element with its\n\
             name, class name, control type, and key properties.\n\
             \n\
             ## When to use\n\
             Before clicking or typing into a specific UI control,\n\
             call this first to discover the tree structure. The\n\
             returned elements have `name`, `automation_id`,\n\
             `control_type` which you can use to identify buttons,\n\
             text boxes, lists, etc. that the quick Win32 window\n\
             enumeration (`cua_list_windows`) cannot see.\n\
             \n\
             Use `cua_ui_find` to search within a tree, or\n\
             `cua_ui_click` to click a named element.",
            JsonSchema::object(props, vec![]),
        )
    }
}

// -- CuaUiFindHandler -----------------------------------------------------

/// Find UIA elements by name, automation ID, class name, or control type.
pub struct CuaUiFindHandler;

impl CuaUiFindHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaUiFindHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaUiFindHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let name = params.get("name").and_then(|v| v.as_str()).map(String::from);
        let automation_id = params.get("automation_id").and_then(|v| v.as_str()).map(String::from);
        let class_name = params.get("class_name").and_then(|v| v.as_str()).map(String::from);
        let max_results = params.get("max_results").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
        let session_id = extract_session_id(&params);

        if name.is_none() && automation_id.is_none() && class_name.is_none() {
            return Err(ToolError::InvalidParams(
                "at least one of 'name', 'automation_id', or 'class_name' is required".into(),
            ));
        }

        // Clone before spawning the blocking closure (which moves).
        let n_name = name.clone();
        let n_automation_id = automation_id.clone();
        let n_class_name = class_name.clone();

        let started = std::time::Instant::now();
        let elements = uia_blocking!(|uia: &hermix_cua::uia::UiaAutomation| {
            use hermix_cua::uia::UiaCondition;

            // Build condition from available criteria
            let mut parts: Vec<UiaCondition> = Vec::new();
            if let Some(ref n) = n_name {
                parts.push(UiaCondition::Name(n.clone()));
            }
            if let Some(ref a) = n_automation_id {
                parts.push(UiaCondition::AutomationId(a.clone()));
            }
            if let Some(ref c) = n_class_name {
                parts.push(UiaCondition::ClassName(c.clone()));
            }
            let cond = if parts.len() == 1 {
                parts.into_iter().next().expect("checked len == 1, must have one element")
            } else {
                UiaCondition::And(parts)
            };

            // SAFETY: `uia` was constructed on this thread and owns a valid
            // COM apartment. `find_all` is a read-only query.
            let found = unsafe {
                uia.find_all(&cond)
                    .map_err(|e| ToolError::ExecutionFailed(format!("UIA find: {e}")))?
            };

            let truncated = found.len() > max_results;
            let slice: Vec<_> = found.into_iter().take(max_results).collect();
            Ok::<_, ToolError>((slice, truncated))
        })?;
        let elapsed_ms = started.elapsed().as_millis() as u64;

        let count = elements.0.len();
        let event = TraceEvent::new(
            TraceKind::Other { subkind: "ui_find".into() },
            &session_id,
            format!("name={name:?} auto_id={automation_id:?} class={class_name:?} found={count}"),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&event);

        Ok(json!({
            "tool": "cua_ui_find",
            "count": count,
            "truncated": elements.1,
            "elements": elements.0,
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "name".into(),
            json!({ "type": "string", "description": "Element name substring (from `UIA_NamePropertyId`)." }),
        );
        props.insert(
            "automation_id".into(),
            json!({ "type": "string", "description": "Automation ID (from `UIA_AutomationIdPropertyId`)." }),
        );
        props.insert(
            "class_name".into(),
            json!({ "type": "string", "description": "Class name (from `UIA_ClassNamePropertyId`)." }),
        );
        props.insert(
            "max_results".into(),
            json!({ "type": "integer", "description": "Maximum results. Default: 50." }),
        );
        tool_schema(
            "cua_ui_find",
            "Find UIA elements matching name / automation ID / class name.\n\
             \n\
             At least one search criterion is required. Multiple\n\
             criteria are combined with AND. Use `cua_ui_tree` first\n\
             to discover what element names / IDs are available,\n\
             then call this to get the exact element(s).\n\
             \n\
             ## Examples\n\
             - `{\"name\": \"OK\"}` — find all elements named \"OK\"\n\
             - `{\"class_name\": \"Button\", \"name\": \"Save\"}` — find\n\
               the button named \"Save\"",
            JsonSchema::object(props, vec![]),
        )
    }
}

// -- CuaUiClickHandler ----------------------------------------------------

/// Find a UIA element by name and click its clickable point.
pub struct CuaUiClickHandler;

impl CuaUiClickHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaUiClickHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaUiClickHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("missing 'name'".into()))?
            .to_string();
        let session_id = extract_session_id(&params);

        let started = std::time::Instant::now();
        let safety = &*SAFETY;
        let trace = &*TRACE;
        let session = session_id.clone();
        let closure_name = name.clone();
        let result: Result<(i32, i32), ToolError> = run_blocking(move || {
            // Charge the rate limiter *before* the UIA lookup, not
            // only inside `action::click` below: the name lookup
            // walks the automation tree via COM and costs far more
            // than the click itself, so gating it after the walk
            // would let a loop spend unbounded time in the UIA
            // provider.
            safety.check(None, RiskLevel::Medium)?;
            // Step 1 — init UIA on this thread (COM apartment).
            // SAFETY: COM apartment initialised by `UiaAutomation::new()`
            // on this blocking thread.
            let uia = unsafe { hermix_cua::uia::UiaAutomation::new()? };
            // SAFETY: same thread, same COM apartment.
            let (cx, cy) =
                unsafe { hermix_cua::uia::click_by_name(&uia, &closure_name)? };
            // Step 3 — issue the click through SendInput.
            hermix_cua::action::click(
                safety,
                trace,
                &session,
                cx,
                cy,
                hermix_cua::action::MouseButton::Left,
            )?;
            Ok((cx, cy))
        })
        .await;
        let (cx, cy) = result?;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let event = TraceEvent::new(
            TraceKind::Other {
                subkind: "ui_click".into(),
            },
            &session_id,
            format!("name={name} clicked_at=({cx},{cy})"),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&event);

        Ok(json!({
            "tool": "cua_ui_click",
            "name": name,
            "clicked_at": { "x": cx, "y": cy },
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "name".into(),
            json!({ "type": "string", "description": "Element name to search for (exact match on UIA Name property). First match wins." }),
        );
        tool_schema(
            "cua_ui_click",
            "Find a UIA element by name and click its clickable point.\n\
             \n\
             Looks up the element by its UIA `Name` property, retrieves\n\
             the clickable point via `IUIAutomationElement::GetClickablePoint`,\n\
             and issues a left mouse click at those coordinates.\n\
             \n\
             ## When to use\n\
             When you know the name of a button or control but not\n\
             its screen coordinates. Works for most standard Windows\n\
             controls (buttons, checkboxes, menu items, etc.).",
            JsonSchema::object(props, vec!["name".into()]),
        )
    }
}

// -- CuaUiFocusHandler ----------------------------------------------------

/// Find a UIA element by name and set keyboard focus to it.
pub struct CuaUiFocusHandler;

impl CuaUiFocusHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaUiFocusHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaUiFocusHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("missing 'name'".into()))?
            .to_string();
        let session_id = extract_session_id(&params);

        let started = std::time::Instant::now();
        let safety = &*SAFETY;
        let closure_name = name.clone();
        run_blocking(move || {
            // `SetFocus` + the name lookup both cross the COM boundary.
            // The UIA tree walk alone can cost 50–500 ms, so an
            // unbounded loop here starves the agent loop and floods
            // the UIA provider. `Medium` puts it under the same
            // sliding-window ceiling as `type_text`.
            safety.check(None, RiskLevel::Medium)?;
            // SAFETY: COM apartment initialised on this blocking thread.
            let uia = unsafe { hermix_cua::uia::UiaAutomation::new()? };
            // SAFETY: same thread, same COM apartment.
            unsafe { hermix_cua::uia::focus_by_name(&uia, &closure_name)? };
            Ok(())
        })
        .await?;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let event = TraceEvent::new(
            TraceKind::Other {
                subkind: "ui_focus".into(),
            },
            &session_id,
            format!("name={name}"),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&event);

        Ok(json!({
            "tool": "cua_ui_focus",
            "name": name,
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "name".into(),
            json!({ "type": "string", "description": "Element name to focus." }),
        );
        tool_schema(
            "cua_ui_focus",
            "Focus a UIA element by name.\n\
             Sets keyboard focus to the named element via\n\
             `IUIAutomationElement::SetFocus`.\n\
             \n\
             Use before typing into a text field or pressing a key\n\
             that should go to a specific control.",
            JsonSchema::object(props, vec!["name".into()]),
        )
    }
}

// -- CuaUiInvokeHandler ---------------------------------------------------

/// Find a UIA element by name and invoke its default action (e.g. click a button).
pub struct CuaUiInvokeHandler;

impl CuaUiInvokeHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CuaUiInvokeHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for CuaUiInvokeHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("missing 'name'".into()))?
            .to_string();
        let session_id = extract_session_id(&params);

        let started = std::time::Instant::now();
        let safety = &*SAFETY;
        let closure_name = name.clone();
        run_blocking(move || {
            // `InvokePattern.Invoke` is the UIA equivalent of
            // pressing the control's default button — the element
            // name is LLM-supplied and never checked against what
            // is actually on screen, so this is gated at `High`
            // (the same tier as `press_key`) rather than `Medium`.
            // Without this, a loop over `cua_ui_invoke` is an
            // unbounded confirm-button machine.
            safety.check(None, RiskLevel::High)?;
            // SAFETY: COM apartment initialised on this blocking thread.
            let uia = unsafe { hermix_cua::uia::UiaAutomation::new()? };
            // SAFETY: same thread, same COM apartment.
            unsafe { hermix_cua::uia::invoke_by_name(&uia, &closure_name)? };
            Ok(())
        })
        .await?;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let event = TraceEvent::new(
            TraceKind::Other {
                subkind: "ui_invoke".into(),
            },
            &session_id,
            format!("name={name}"),
        )
        .with_elapsed_ms(elapsed_ms);
        let _ = TRACE.record(&event);

        Ok(json!({
            "tool": "cua_ui_invoke",
            "name": name,
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "name".into(),
            json!({ "type": "string", "description": "Element name to invoke." }),
        );
        tool_schema(
            "cua_ui_invoke",
            "Invoke a UIA element's default action by name.\n\
             Uses the `IUIAutomationInvokePattern` to trigger the\n\
             element's default action (click a button, check a box,\n\
             select a menu item, follow a hyperlink).\n\
             \n\
             More reliable than `cua_ui_click` for buttons, but only\n\
             works on elements that implement the Invoke pattern.\n\
             Falls back to clicking the clickable point if the\n\
             element does not support Invoke_Pattern.",
            JsonSchema::object(props, vec!["name".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
//D: OCR handler — REMOVED
//
// `cua_ocr_recognize` was removed because the local Windows.Media.Ocr engine
// had <30% character accuracy for Chinese documents/IDs/certificates. The LLM
// kept calling it instead of `delegate_task` + sub-agent `read_image`, wasting
// turns and polluting the main context with unreliable OCR output. All image
// analysis now goes through `delegate_task` → sub-agent `read_image` (LLM
// vision), which has far higher accuracy.
// ---------------------------------------------------------------------------
// Re-exports — `register_builtins` imports the handler types
// directly. The convenience group below keeps the imports
// compact at the call site.
// ---------------------------------------------------------------------------

pub use self::{
    CuaClickWindowHandler as ClickWindow, CuaLeftClickHandler as LeftClick,
    CuaListWindowsHandler as ListWindows, CuaPressKeyHandler as PressKey,
    CuaScreenInfoHandler as ScreenInfo, CuaScreenshotHandler as Screenshot,
    CuaTypeTextHandler as TypeText, CuaWindowAtPointHandler as WindowAtPoint,
    //UIA element-tree handlers
    CuaUiClickHandler as UiClick, CuaUiFindHandler as UiFind,
    CuaUiFocusHandler as UiFocus, CuaUiInvokeHandler as UiInvoke,
    CuaUiTreeHandler as UiTree,
};

// Convenience factory: build all five handlers behind an
// `Arc<dyn ToolHandler>` slice. `register_builtins` calls this
// when wiring the toolset.
pub fn all_handlers() -> Vec<Arc<dyn ToolHandler>> {
    vec![
        Arc::new(CuaScreenshotHandler::new()),
        Arc::new(CuaScreenInfoHandler::new()),
        Arc::new(CuaLeftClickHandler::new()),
        Arc::new(CuaTypeTextHandler::new()),
        Arc::new(CuaPressKeyHandler::new()),
        Arc::new(CuaListWindowsHandler::new()),
        Arc::new(CuaWindowAtPointHandler::new()),
        Arc::new(CuaClickWindowHandler::new()),
        //C: UIA element-tree handlers
        Arc::new(CuaUiTreeHandler::new()),
        Arc::new(CuaUiFindHandler::new()),
        Arc::new(CuaUiClickHandler::new()),
        Arc::new(CuaUiFocusHandler::new()),
        Arc::new(CuaUiInvokeHandler::new()),
        //scroll, double-click, move-cursor
        Arc::new(CuaScrollHandler::new()),
        Arc::new(CuaDoubleClickHandler::new()),
        Arc::new(CuaMoveCursorHandler::new()),
    ]
}

// ---------------------------------------------------------------------------
//Mouse scroll
// ---------------------------------------------------------------------------

/// Scroll the mouse wheel at `(x, y)`.
pub struct CuaScrollHandler;

impl CuaScrollHandler {
    pub fn new() -> Self { Self }
}

impl Default for CuaScrollHandler {
    fn default() -> Self { Self::new() }
}

#[async_trait]
impl ToolHandler for CuaScrollHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let x = params
            .get("x")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| ToolError::InvalidParams("missing 'x' coordinate".into()))?;
        let y = params
            .get("y")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| ToolError::InvalidParams("missing 'y' coordinate".into()))?;
        if x < 0 || y < 0 {
            return Err(ToolError::InvalidParams(format!(
                "coordinates must be non-negative (got x={x}, y={y})"
            )));
        }
        if x > i32::MAX as i64 || y > i32::MAX as i64 {
            return Err(ToolError::InvalidParams(format!(
                "coordinates overflow i32 (got x={x}, y={y})"
            )));
        }
        let delta = params
            .get("delta")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| ToolError::InvalidParams("missing 'delta' parameter".into()))?;
        if delta == 0 {
            return Err(ToolError::InvalidParams("delta must not be zero".into()));
        }
        if delta < -100 || delta > 100 {
            return Err(ToolError::InvalidParams(format!(
                "delta must be between -100 and 100 (got {delta})"
            )));
        }
        let session_id = extract_session_id(&params);
        let safety = &*SAFETY;
        let trace = &*TRACE;
        run_blocking(move || action::scroll(safety, trace, &session_id, x as i32, y as i32, delta as i32))
            .await?;

        Ok(json!({
            "tool": "cua_scroll",
            "x": x, "y": y, "delta": delta,
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "x".into(),
            json!({"type": "integer", "description": "X coordinate (pixels from top-left of primary display)."}),
        );
        props.insert(
            "y".into(),
            json!({"type": "integer", "description": "Y coordinate (pixels from top-left of primary display)."}),
        );
        props.insert(
            "delta".into(),
            json!({
                "type": "integer",
                "description": "Scroll amount in clicks. Positive = scroll up, negative = scroll down. Each click is 120 units. Range: -100 to 100."
            }),
        );
        tool_schema(
            "cua_scroll",
            "Scroll the mouse wheel at (x, y). Positive delta scrolls up, negative scrolls down.",
            JsonSchema::object(props, vec!["x".into(), "y".into(), "delta".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
//Double click
// ---------------------------------------------------------------------------

/// Double-click at `(x, y)` with the left mouse button.
pub struct CuaDoubleClickHandler;

impl CuaDoubleClickHandler {
    pub fn new() -> Self { Self }
}

impl Default for CuaDoubleClickHandler {
    fn default() -> Self { Self::new() }
}

#[async_trait]
impl ToolHandler for CuaDoubleClickHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let x = params
            .get("x")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| ToolError::InvalidParams("missing 'x' coordinate".into()))?;
        let y = params
            .get("y")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| ToolError::InvalidParams("missing 'y' coordinate".into()))?;
        if x < 0 || y < 0 {
            return Err(ToolError::InvalidParams(format!(
                "coordinates must be non-negative (got x={x}, y={y})"
            )));
        }
        if x > i32::MAX as i64 || y > i32::MAX as i64 {
            return Err(ToolError::InvalidParams(format!(
                "coordinates overflow i32 (got x={x}, y={y})"
            )));
        }
        let session_id = extract_session_id(&params);
        let safety = &*SAFETY;
        let trace = &*TRACE;
        run_blocking(move || action::double_click(safety, trace, &session_id, x as i32, y as i32))
            .await?;

        Ok(json!({
            "tool": "cua_double_click",
            "x": x, "y": y,
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "x".into(),
            json!({"type": "integer", "description": "X coordinate (pixels from top-left of primary display)."}),
        );
        props.insert(
            "y".into(),
            json!({"type": "integer", "description": "Y coordinate (pixels from top-left of primary display)."}),
        );
        tool_schema(
            "cua_double_click",
            "Double-click at (x, y) with the left mouse button. Useful for opening files, selecting words, etc.",
            JsonSchema::object(props, vec!["x".into(), "y".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
//Move cursor
// ---------------------------------------------------------------------------

/// Move the mouse cursor to `(x, y)` without clicking.
pub struct CuaMoveCursorHandler;

impl CuaMoveCursorHandler {
    pub fn new() -> Self { Self }
}

impl Default for CuaMoveCursorHandler {
    fn default() -> Self { Self::new() }
}

#[async_trait]
impl ToolHandler for CuaMoveCursorHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let x = params
            .get("x")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| ToolError::InvalidParams("missing 'x' coordinate".into()))?;
        let y = params
            .get("y")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| ToolError::InvalidParams("missing 'y' coordinate".into()))?;
        if x < 0 || y < 0 {
            return Err(ToolError::InvalidParams(format!(
                "coordinates must be non-negative (got x={x}, y={y})"
            )));
        }
        if x > i32::MAX as i64 || y > i32::MAX as i64 {
            return Err(ToolError::InvalidParams(format!(
                "coordinates overflow i32 (got x={x}, y={y})"
            )));
        }
        let session_id = extract_session_id(&params);
        let safety = &*SAFETY;
        let trace = &*TRACE;
        run_blocking(move || action::move_cursor(safety, trace, &session_id, x as i32, y as i32))
            .await?;

        Ok(json!({
            "tool": "cua_move_cursor",
            "x": x, "y": y,
            "ok": true,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "x".into(),
            json!({"type": "integer", "description": "X coordinate (pixels from top-left of primary display)."}),
        );
        props.insert(
            "y".into(),
            json!({"type": "integer", "description": "Y coordinate (pixels from top-left of primary display)."}),
        );
        tool_schema(
            "cua_move_cursor",
            "Move the mouse cursor to (x, y) without clicking. Useful for hover effects and positioning.",
            JsonSchema::object(props, vec!["x".into(), "y".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// Tests — pure-Rust unit tests only; we do not exercise the
// Windows-only code paths here because those need a live desktop
// session. The schema tests run on every platform.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// `hwnd` 要么生效、要么报错，绝不静默回退到桌面根：0 与负值在 UIA
    /// 侧等同于 NULL 句柄，走出来的正是"传了 hwnd 却仍拿到整棵桌面树"。
    #[test]
    fn parse_optional_hwnd_accepts_absent_and_positive() {
        assert_eq!(parse_optional_hwnd(&json!({})).unwrap(), None);
        assert_eq!(
            parse_optional_hwnd(&json!({"hwnd": 22151398})).unwrap(),
            Some(22151398)
        );
    }

    #[test]
    fn parse_optional_hwnd_rejects_zero_and_negative() {
        let err = parse_optional_hwnd(&json!({"hwnd": 0})).unwrap_err();
        assert!(matches!(err, ToolError::InvalidParams(_)), "{err:?}");
        assert!(parse_optional_hwnd(&json!({"hwnd": -1})).is_err());
    }

    /// 按句柄点击窗口这类工具没有 hwnd 就无从下手，缺失必须单独报错。
    #[test]
    fn parse_required_hwnd_rejects_absent() {
        assert!(parse_required_hwnd(&json!({})).is_err());
    }

    /// `save_to` 落盘：写进去的必须是解出来的 PNG 字节，父目录要能创建。
    #[test]
    fn save_png_base64_writes_decoded_bytes() {
        let raw = b"\x89PNG\r\n\x1a\nrest-of-frame";
        let b64 = general_purpose::STANDARD.encode(raw);
        let dir = std::env::temp_dir().join(format!("hermix-cua-save-{}", std::process::id()));
        let path = dir.join("nested").join("shot.png");
        let written = save_png_base64(&b64, &path).expect("落盘应当成功");
        assert_eq!(written, raw.len());
        assert_eq!(std::fs::read(&path).expect("文件应可读"), raw.to_vec());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 坏 base64 必须报错，而不是写出一个空文件。
    #[test]
    fn save_png_base64_rejects_invalid_base64() {
        let path = std::env::temp_dir().join(format!("hermix-cua-never-{}.png", std::process::id()));
        assert!(save_png_base64("not base64!!", &path).is_err());
        assert!(!path.exists(), "失败时不应留下文件");
    }

    /// 空串按"未指定"处理，避免写出一个空名文件。
    #[test]
    fn save_to_path_treats_empty_as_unspecified() {
        assert!(save_to_path(&json!({})).is_none());
        assert!(save_to_path(&json!({"save_to": "  "})).is_none());
        assert_eq!(
            save_to_path(&json!({"save_to": "out/shot.png"})).map(|p| p.display().to_string()),
            Some("out/shot.png".to_string())
        );
    }

    /// 默认落盘位必须落在 assetProtocol scope（`$HOME/.hermix/**`）内的
    /// captures/ 目录、.png 后缀、带显示器号；连拍不重名靠序号兜底。
    #[test]
    fn default_capture_path_lands_in_hermix_captures() {
        let a = default_capture_path(0);
        let b = default_capture_path(0);
        assert!(a.starts_with(hermix_config::paths::hermix_home().join("captures")));
        assert_eq!(a.extension().and_then(|e| e.to_str()), Some("png"));
        assert!(a.to_string_lossy().ends_with("-m0.png"));
        assert_ne!(a, b, "同毫秒连拍文件名必须不同");
    }

    /// 把文件 mtime 拨到 `age` 之前，供过期清理用例构造"陈旧"文件。
    fn set_file_modified(path: &Path, age: std::time::Duration) {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("打开待拨 mtime 的文件");
        let past = std::time::SystemTime::now()
            .checked_sub(age)
            .expect("age 超出系统时间表示范围");
        f.set_modified(past).expect("set_modified");
    }

    #[test]
    fn sweep_stale_captures_removes_only_own_stale_png() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let stale = tmp.path().join("capture-1000-0-m0.png");
        let fresh = tmp.path().join("capture-2000-1-m0.png");
        let foreign = tmp.path().join("keep-me-1000.png");
        std::fs::write(&stale, b"old").expect("write stale");
        std::fs::write(&fresh, b"new").expect("write fresh");
        std::fs::write(&foreign, b"user file").expect("write foreign");
        let eight_days = std::time::Duration::from_secs(8 * 24 * 3600);
        set_file_modified(&stale, eight_days);
        set_file_modified(&foreign, eight_days);

        let removed = sweep_stale_captures(tmp.path(), std::time::SystemTime::now());

        assert_eq!(removed, 1, "只应删掉一个过期 capture png");
        assert!(!stale.exists(), "过期自家产物应被删除");
        assert!(fresh.exists(), "未过期文件必须保留");
        assert!(foreign.exists(), "非 capture-* 命名的文件绝不能碰");
    }

    #[cfg(windows)]
    #[test]
    fn sweep_skips_subdirectories_even_when_stale() {
        use std::os::windows::fs::OpenOptionsExt;
        let tmp = tempfile::tempdir().expect("tempdir");
        let sub = tmp.path().join("capture-3000-2-m0.png");
        std::fs::create_dir(&sub).expect("mkdir named like a capture");
        // Windows 拒绝普通 write 句柄打开目录；拨目录 mtime 必须带
        // FILE_FLAG_BACKUP_SEMANTICS。
        let f = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(0x0200_0000)
            .open(&sub)
            .expect("以 backup semantics 打开目录句柄");
        let past = std::time::SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(8 * 24 * 3600))
            .expect("age 超出系统时间表示范围");
        f.set_modified(past).expect("set dir mtime");

        let removed = sweep_stale_captures(tmp.path(), std::time::SystemTime::now());

        assert_eq!(removed, 0);
        assert!(sub.exists(), "目录条目不删，哪怕名字撞上 capture-*");
    }

    #[test]
    fn prune_stale_captures_runs_after_reset() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let stale = tmp.path().join("capture-4000-3-m0.png");
        std::fs::write(&stale, b"old").expect("write stale");
        set_file_modified(&stale, std::time::Duration::from_secs(8 * 24 * 3600));
        // 清零限频水位，让本次调用必定真正扫描（避免被其他用例留下的
        // 时间戳 short-circuit）。
        LAST_CAPTURE_PRUNE.store(0, std::sync::atomic::Ordering::Relaxed);

        prune_stale_captures(tmp.path());

        assert!(!stale.exists(), "重置水位后限频入口应完成一次真实清理");
    }

    #[test]
    fn schema_names_are_stable() {
        // Renaming a public tool name is a wire-protocol break —
        // pin the schema name here so an accidental rename fails
        // the test before it reaches the agent loop.
        assert_eq!(CuaScreenshotHandler::new().schema().name, "cua_screenshot");
        assert_eq!(CuaScreenInfoHandler::new().schema().name, "cua_screen_info");
        assert_eq!(CuaLeftClickHandler::new().schema().name, "cua_left_click");
        assert_eq!(CuaTypeTextHandler::new().schema().name, "cua_type_text");
        assert_eq!(CuaPressKeyHandler::new().schema().name, "cua_press_key");
        assert_eq!(CuaScrollHandler::new().schema().name, "cua_scroll");
        assert_eq!(CuaDoubleClickHandler::new().schema().name, "cua_double_click");
        assert_eq!(CuaMoveCursorHandler::new().schema().name, "cua_move_cursor");
    }

    #[test]
    fn schema_required_fields_present() {
        let s = CuaLeftClickHandler::new().schema();
        let reqs = s.parameters.required.as_ref().expect("required fields");
        assert!(reqs.contains(&"x".to_string()));
        assert!(reqs.contains(&"y".to_string()));
    }

    #[test]
    fn schema_text_required() {
        let s = CuaTypeTextHandler::new().schema();
        let reqs = s.parameters.required.as_ref().expect("required fields");
        assert!(reqs.contains(&"text".to_string()));
    }

    #[test]
    fn schema_chord_required() {
        let s = CuaPressKeyHandler::new().schema();
        let reqs = s.parameters.required.as_ref().expect("required fields");
        assert!(reqs.contains(&"chord".to_string()));
    }

    #[test]
    fn execute_rejects_missing_coords() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let h = CuaLeftClickHandler::new();
        let r = rt.block_on(h.execute(json!({"y": 100})));
        assert!(r.is_err());
        let r = rt.block_on(h.execute(json!({"x": 100})));
        assert!(r.is_err());
    }

    #[test]
    fn execute_rejects_negative_coords() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let h = CuaLeftClickHandler::new();
        let r = rt.block_on(h.execute(json!({"x": -1, "y": 100})));
        assert!(r.is_err());
    }

    #[test]
    fn execute_rejects_missing_text() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let h = CuaTypeTextHandler::new();
        let r = rt.block_on(h.execute(json!({})));
        assert!(r.is_err());
    }

    #[test]
    fn execute_rejects_missing_chord() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let h = CuaPressKeyHandler::new();
        let r = rt.block_on(h.execute(json!({})));
        assert!(r.is_err());
    }

    #[test]
    fn all_handlers_returns_correct_count() {
        assert_eq!(all_handlers().len(), 16);
    }

    #[test]
    fn schema_names_phase2b_are_stable() {
        //Pin the -B handler names so a typo fails the test
        // before it reaches the dispatcher.
        assert_eq!(CuaListWindowsHandler::new().schema().name, "cua_list_windows");
        assert_eq!(CuaWindowAtPointHandler::new().schema().name, "cua_window_at_point");
        assert_eq!(CuaClickWindowHandler::new().schema().name, "cua_click_window");
    }

    #[test]
    fn schema_window_at_point_required_fields() {
        let s = CuaWindowAtPointHandler::new().schema();
        let reqs = s.parameters.required.as_ref().expect("required fields");
        assert!(reqs.contains(&"x".to_string()));
        assert!(reqs.contains(&"y".to_string()));
    }

    #[test]
    fn schema_click_window_required_fields() {
        let s = CuaClickWindowHandler::new().schema();
        let reqs = s.parameters.required.as_ref().expect("required fields");
        assert!(reqs.contains(&"hwnd".to_string()));
    }

    #[test]
    fn execute_window_at_point_rejects_missing_coords() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let h = CuaWindowAtPointHandler::new();
        assert!(rt.block_on(h.execute(json!({"y": 100}))).is_err());
        assert!(rt.block_on(h.execute(json!({"x": 100}))).is_err());
        assert!(rt.block_on(h.execute(json!({}))).is_err());
    }

    #[test]
    fn execute_window_at_point_rejects_negative_coords() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let h = CuaWindowAtPointHandler::new();
        let r = rt.block_on(h.execute(json!({"x": -1, "y": 100})));
        assert!(r.is_err());
    }

    #[test]
    fn execute_click_window_rejects_missing_hwnd() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let h = CuaClickWindowHandler::new();
        let r = rt.block_on(h.execute(json!({})));
        assert!(r.is_err());
    }

    #[test]
    fn execute_click_window_rejects_zero_or_negative_hwnd() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let h = CuaClickWindowHandler::new();
        assert!(rt.block_on(h.execute(json!({"hwnd": 0}))).is_err());
        assert!(rt.block_on(h.execute(json!({"hwnd": -1}))).is_err());
    }

    /// Registration tier decides whether the user gets an approval
    /// card, so it is a security-relevant value, not a label. Pin the
    /// full map: read-only tools must stay below the prompt threshold,
    /// anything that mutates the machine must stay at `Dangerous`.
    #[test]
    fn registration_tiers_match_side_effects() {
        let registry = ToolRegistry::new();
        register(&registry);

        // Read-only, but they read the user's screen and window
        // titles — one tier below `Safe` on purpose.
        for read_only in [
            "cua_screenshot",
            "cua_screen_info",
            "cua_list_windows",
            "cua_window_at_point",
            "cua_ui_tree",
            "cua_ui_find",
        ] {
            assert_eq!(
                registry.risk_level(read_only),
                Some(CoreRisk::Caution),
                "{read_only} reads the screen but must not prompt"
            );
        }

        // Mutates the machine — these are the only ones that may
        // prompt, and dropping any of them to `Caution` would let a
        // write run unapproved under `PermissionLevel::Default`.
        for mutating in [
            "cua_left_click",
            "cua_type_text",
            "cua_press_key",
            "cua_scroll",
            "cua_double_click",
            "cua_click_window",
            "cua_ui_click",
            "cua_ui_focus",
            "cua_ui_invoke",
        ] {
            assert_eq!(
                registry.risk_level(mutating),
                Some(CoreRisk::Dangerous),
                "{mutating} mutates the machine and must stay Dangerous"
            );
        }

        assert_eq!(registry.risk_level("cua_move_cursor"), Some(CoreRisk::Safe));
    }

    /// `SafetyGuard::check` only enforces the sliding window for
    /// `Medium`/`High` (`safety.rs::check` matches on exactly those
    /// two). A mutating tool that checks at `Low` therefore gets no
    /// rate limiting at all — the "read-only" tier is not a safe
    /// default for anything that reaches the OS.
    #[test]
    fn low_risk_is_not_rate_limited() {
        // This is the property the handler tiers depend on: if a
        // future refactor makes `Low` rate-limited too, the tier
        // choices above become over-cautious but still safe. If it
        // ever goes the other way and `Low` starts bypassing the
        // window for mutating tools, this test is the canary.
        let guard = SafetyGuard::new();
        for _ in 0..200 {
            guard
                .check(None, RiskLevel::Low)
                .expect("low risk must never be refused");
        }
        guard
            .check(None, RiskLevel::Medium)
            .expect("low-risk traffic must not consume the window");
    }

    #[test]
    fn execute_list_windows_accepts_empty_params() {
        // No required fields — listing all visible windows is the
        // common case. We can't verify the *contents* (depends on
        // the live desktop), but we can verify the handler doesn't
        // reject the empty payload.
        // We assert via parsed JSON rather than substring contains
        // — `serde_json::json!` uses a BTreeMap-keyed Map so the
        // wire key order is alphabetical, which makes
        // `s.contains("\"ok\": true")` brittle (any whitespace or
        // trailing-comma difference breaks it).
        let rt = tokio::runtime::Runtime::new().unwrap();
        let h = CuaListWindowsHandler::new();
        let r = rt.block_on(h.execute(json!({})));
        // Will succeed on Windows with a non-empty desktop, fail on
        // non-Windows CI with the "requires Windows" stub. Either
        // way, the handler must not have errored on parsing.
        if let Ok(s) = r.as_ref() {
            let v: serde_json::Value =
                serde_json::from_str(s).expect("handler output must be valid JSON");
            assert_eq!(v.get("ok").and_then(|x| x.as_bool()), Some(true));
            assert_eq!(
                v.get("tool").and_then(|x| x.as_str()),
                Some("cua_list_windows")
            );
        }
    }
}
