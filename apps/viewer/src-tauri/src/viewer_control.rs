//! 查看器的 **AI 自控面**：让 agent 能读到自己看到什么、并**直接执行**查看器的
//! 动作。
//!
//! 之前 agent 操作查看器只有一条路：`desktop_*` 那套 Win32 盲点工具。而查看器
//! 自己的界面是一个 WebView2 —— 对 Win32 来说它只是**一个** HWND，里面是 DOM，
//! **没有控件树** → 盲点既慢又脆（点不到 DOM 里的元素）。
//!
//! 这里给查看器加两个一等公民的工具：
//!
//! - `viewer_ui_state`：AI 读**当前界面状态**（当前目录 / 路径栏 / 搜索词 /
//!   标签列表 / 活动标签 / 打开了哪些面板 / 当前模式）。前端把状态快照推给
//!   Rust 缓存（单向推送，不需要往返）。
//! - `viewer_action`：AI **直接执行**查看器的动作（开文件、开网页、切标签、
//!   跳目录、搜索、跑菜单命令、开面板…）。Rust 发事件给前端 → 前端执行 →
//!   用请求 id 回结果（配对，不串台）。
//!
//! 设计取舍：**动作走「语义命令」而不是「点坐标」**——语义命令稳定（元素
//! 改版了也还在），坐标点击脆（布局一动就点歪）。真要点某个具体控件时，
//! agent 仍然可以用 `webview_click` / `desktop_click` 兜底。

use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

/// 前端推来的界面状态快照（最后一次）。
static STATE: OnceLock<Mutex<Option<Value>>> = OnceLock::new();

fn state_store() -> &'static Mutex<Option<Value>> {
    STATE.get_or_init(|| Mutex::new(None))
}

/// 等待中的动作请求：`id → 回信口`。
static PENDING: OnceLock<Mutex<HashMap<String, Sender<(bool, Value)>>>> = OnceLock::new();

fn pending_store() -> &'static Mutex<HashMap<String, Sender<(bool, Value)>>> {
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 请求 id 计数器。
fn next_id() -> u64 {
    static S: OnceLock<std::sync::atomic::AtomicU64> = OnceLock::new();
    let counter = S.get_or_init(|| std::sync::atomic::AtomicU64::new(1));
    counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// 应用句柄（`setup` 里登记）。
///
/// MCP 的 HTTP 端点跑在 GUI 进程里 → 有窗口；**stdio 模式没有窗口** →
/// 动作类工具报一句人话，而不是莫名失败。
static APP: OnceLock<AppHandle> = OnceLock::new();

/// 登记应用句柄。
pub fn set_app(app: AppHandle) {
    let _ = APP.set(app);
}

/// 取应用句柄。
pub fn app() -> Result<AppHandle, String> {
    APP.get().cloned().ok_or_else(|| {
        "查看器窗口功能需要查看器以窗口模式运行（当前是 MCP stdio 模式，没有窗口）".to_string()
    })
}

/// 前端把界面状态推上来（Tauri 命令）。
#[tauri::command]
pub fn viewer_state_report(state: Value) {
    *state_store().lock().unwrap() = Some(state);
}

/// **让前端打开一个文件**（任何模式都能用）。
///
/// 走 `viewer-open-file` 事件 → 前端已有的监听器会开一个预览标签。
/// 为什么不用路径栏：路径栏**只在重量模式存在**，而浏览器自动化任务默认是
/// **纯净模式** → 靠路径栏开文件会让 agent 在纯净模式下失去文件能力。
/// 这条通道与模式无关 ✓。
#[tauri::command]
pub fn viewer_open_path(app: AppHandle, path: String) {
    // agent 让开文件 = 真实使用：预热实例不再降级退出。
    crate::webview_host::mark_used();
    let _ = app.emit("viewer-open-file", path);
}

/// **让前端打开一个网址**（任何模式都能用）：走内嵌网页视图。
#[tauri::command]
pub fn viewer_open_url(_app: AppHandle, url: String) -> Result<(), String> {
    crate::webview_host::open_at_last_bounds(&url)
}

/// 前端回执（Tauri 命令）。
#[tauri::command]
pub fn viewer_action_reply(id: String, ok: bool, data: Value) {
    let sender = pending_store().lock().unwrap().remove(&id);
    if let Some(tx) = sender {
        let _ = tx.send((ok, data));
    }
}

/// 当前界面状态（AI 读）。
pub fn ui_state() -> Value {
    match state_store().lock().unwrap().clone() {
        Some(state) => state,
        None => json!({
            "ok": false,
            "message": "查看器还没有上报界面状态（它可能没在运行，或刚启动还没来得及上报）",
        }),
    }
}

/// 执行一个查看器动作，等前端回执。
pub fn run_action(app: &AppHandle, action: &str, args: Value) -> Result<Value, String> {
    let id = next_id().to_string();
    let (tx, rx) = mpsc::channel();
    pending_store().lock().unwrap().insert(id.clone(), tx);
    if let Err(e) = app.emit_to(
        "main",
        "viewer-action",
        json!({"id": id, "action": action, "args": args}),
    ) {
        pending_store().lock().unwrap().remove(&id);
        return Err(format!("下发动作失败：{e}"));
    }
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok((true, data)) => Ok(data),
        Ok((false, message)) => Err(message.to_string()),
        Err(_) => {
            pending_store().lock().unwrap().remove(&id);
            Err("查看器没有响应这个动作（界面正忙，或查看器不在运行）".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_is_absent_until_the_frontend_reports() {
        // 换一个独立键：单测之间共享全局状态会互相污染。
        let snapshot = json!({"ok": true, "currentPath": "C:/x"});
        viewer_state_report(snapshot.clone());
        let got = ui_state();
        assert_eq!(got.get("currentPath").and_then(|v| v.as_str()), Some("C:/x"));
    }
}
