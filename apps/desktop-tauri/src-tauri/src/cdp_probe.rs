//! N0 技术验证探针：WebView2 进程内 CDP（批次 N，对标 ego-lite）。
//!
//! 回答一个二选一问题：做「内置智能体浏览器」是否必须定制 Chromium 内核？
//! ego-lite 定制内核；本探针验证 WebView2 的**进程内 CDP API**
//! （`CallDevToolsProtocolMethod` / `GetDevToolsProtocolEventReceiver`，
//! 微软官方文档 "Use the Chrome DevTools Protocol (CDP) in WebView2"）
//! 能否拿到同样的能力面 —— 若能，则无需 fork 内核、无需
//! remote-debugging-port（零端口暴露，比 ego 更安全）。
//!
//! 钉死四件事（与 pty_conpty_probe 同方法：真机探针固化事实）：
//! 1. Tauri `with_webview` → `ICoreWebView2Controller` → `ICoreWebView2`
//!    句柄链路可走通（webview2-com 0.38 绑定）
//! 2. `Runtime.evaluate` 页内求值返回预期值（= ego-browser `js()` 逃生舱）
//! 3. `DOMSnapshot.captureSnapshot` 返回结构化文档（= 语义快照原料）
//! 4. `Accessibility.getFullAXTree` 返回完整语义树（= ref 编号原料）
//!
//! 线程模型（编译器钉死的事实）：COM 接口非 Send，且 CDP 完成回调经
//! Windows 消息循环派发 —— 所以**全部 CDP 工作必须在 with_webview 闭包内
//! （= 主线程）完成**，`wait_for_async_operation` 内部的 `wait_with_pump`
//! 在等待时泵消息循环让回调得以触发；跨线程只传 String 结果。
#![cfg(windows)]

use webview2_com::CallDevToolsProtocolMethodCompletedHandler;
// `Microsoft` 是 webview2_com 对 webview2_com_sys 的重导出（lib.rs:2），
// 不直接依赖 sys crate。命名空间：Microsoft::Web::WebView2::Win32。
use webview2_com::Microsoft;
use windows_core::{PCWSTR, HSTRING};

/// `#[tauri::command]` 形态的真机探针入口：前端 `invoke("cdp_n0_probe")`
/// 触发，在主 webview 上跑三个 CDP 方法并返回 `(方法名, 摘要)` 列表。
#[tauri::command]
pub fn cdp_n0_probe(
    webview: tauri::Webview<tauri::Wry>,
) -> Result<Vec<(String, String)>, String> {
    probe_via_tauri(&webview, std::time::Duration::from_secs(30))
}

/// 在主线程（with_webview 闭包内）跑全部探针，把结果以
/// `(方法名, 摘要)` 列表传回。`timeout` 覆盖闭包排队 + 三次 CDP 调用。
pub fn probe_via_tauri(
    webview: &tauri::Webview<tauri::Wry>,
    timeout: std::time::Duration,
) -> Result<Vec<(String, String)>, String> {
    let (tx, rx) = std::sync::mpsc::channel::<Result<Vec<(String, String)>, String>>();

    webview
        .with_webview(move |platform: tauri::webview::PlatformWebview| {
            // tauri 2.11 的 controller() 已是类型化 ICoreWebView2Controller
            //（webview2_com::Microsoft::Web::WebView2::Win32），无需裸指针转换。
            let controller = platform.controller();
            let cv2 = match unsafe { controller.CoreWebView2() } {
                Ok(c) => c,
                Err(e) => {
                    tx.send(Err(format!("CoreWebView2() failed: {e}"))).ok();
                    return;
                }
            };
            tx.send(Ok(probe_results(&cv2))).ok();
        })
        .map_err(|e| format!("with_webview: {e}"))?;

    rx.recv_timeout(timeout)
        .map_err(|_| "探针超时：with_webview 闭包未执行（事件循环未泵？）".to_string())?
}

/// 同步调用一个 CDP 方法。宏生成的 `wait_for_async_operation` 两段式：
/// 发起闭包（拿 handler 发起调用，返回 `webview2_com::Result<()>`）+
/// 完成闭包（签名由宏定：`(Result<(), windows_core::Error>, String)`）。
/// 等待期间 `wait_with_pump` 泵消息循环 —— 必须在主线程调用。
fn call_cdp_sync(
    cv2: &Microsoft::Web::WebView2::Win32::ICoreWebView2,
    method: &str,
    params_json: &str,
) -> Result<String, String> {
    let method_h: HSTRING = HSTRING::from(method);
    let params_h: HSTRING = HSTRING::from(params_json);
    let cv2_for_call = cv2.clone();
    // 结果 JSON 经 channel 从完成闭包带回（与宏内部 wait_with_pump 同款
    // 模式：tx 移入 'static 闭包，rx 在等待返回后读取）。
    let (result_tx, result_rx) = std::sync::mpsc::channel::<String>();

    let outcome: Result<(), webview2_com::Error> =
        CallDevToolsProtocolMethodCompletedHandler::wait_for_async_operation(
            Box::new(move |handler| {
                // PCWSTR 从 HSTRING 裸指针构造（HSTRING 以 NUL 结尾，
                // 生命周期覆盖本次调用）。
                let m = PCWSTR::from_raw(method_h.as_ptr());
                let p = PCWSTR::from_raw(params_h.as_ptr());
                unsafe { cv2_for_call.CallDevToolsProtocolMethod(m, p, &handler) }
                    .map_err(webview2_com::Error::WindowsError)
            }),
            Box::new(
                move |error_code: Result<(), windows_core::Error>,
                      return_object_as_json: String| {
                    if error_code.is_ok() {
                        result_tx.send(return_object_as_json).ok();
                        Ok(())
                    } else {
                        Err(error_code.unwrap_err())
                    }
                },
            ),
        );

    outcome.map_err(|e| format!("CDP {method} 发起失败：{e}"))?;
    result_rx
        .recv()
        .map_err(|_| format!("CDP {method} 完成回调未带结果 JSON"))
}

/// 三个 CDP 探针：返回 (方法名, 摘要) 列表。必须在主线程调用。
pub fn probe_results(
    cv2: &Microsoft::Web::WebView2::Win32::ICoreWebView2,
) -> Vec<(String, String)> {
    let mut out = Vec::new();

    // ① Runtime.evaluate —— ego-browser `js()` 的对应物
    let r = call_cdp_sync(
        cv2,
        "Runtime.evaluate",
        r#"{"expression":"1+1","returnByValue":true}"#,
    );
    let summary = match &r {
        Ok(json) => {
            let v = serde_json::from_str::<serde_json::Value>(json)
                .ok()
                .and_then(|v| v["result"]["value"].as_i64());
            match v {
                Some(2) => "OK: 1+1==2（页内求值通）".to_string(),
                Some(other) => format!("UNEXPECTED: 1+1 == {other}"),
                None => format!("OK（形状异常）: {json}"),
            }
        }
        Err(e) => format!("FAIL: {e}"),
    };
    out.push(("Runtime.evaluate".into(), summary));

    // ② DOMSnapshot.captureSnapshot —— 语义快照原料（DOM+布局+样式全量）
    let r = call_cdp_sync(cv2, "DOMSnapshot.captureSnapshot", r#"{"computedStyles":false,"includePaintOrder":false,"includeDOMRects":false}"#);
    let summary = match &r {
        Ok(json) => {
            match serde_json::from_str::<serde_json::Value>(json) {
                Ok(v) => {
                    let docs = v["data"]["documents"]
                        .as_array()
                        .map(|a| a.len())
                        .unwrap_or(0);
                    let strings = v["data"]["strings"]
                        .as_array()
                        .map(|a| a.len())
                        .unwrap_or(0);
                    format!("OK: {docs} documents / {strings} strings")
                }
                Err(_) => "OK（形状待解析）".to_string(),
            }
        }
        Err(e) => format!("FAIL: {e}"),
    };
    out.push(("DOMSnapshot.captureSnapshot".into(), summary));

    // ③ Accessibility.getFullAXTree —— ref 编号的原料
    let r = call_cdp_sync(cv2, "Accessibility.getFullAXTree", "{}");
    let summary = match &r {
        Ok(json) => {
            let n = serde_json::from_str::<serde_json::Value>(json)
                .ok()
                .and_then(|v| v["nodes"].as_array().map(|a| a.len()))
                .unwrap_or(0);
            format!("OK: {n} AX nodes")
        }
        Err(e) => format!("FAIL: {e}"),
    };
    out.push(("Accessibility.getFullAXTree".into(), summary));

    out
}
