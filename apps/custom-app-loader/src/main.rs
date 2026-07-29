// hermix-app-loader — 通用自定义应用加载器 (wry 直接驱动, 无 Tauri)
//
// 职责:
//   1. 读自身 exe 尾部 payload (html + manifest)
//   2. 用 app:// 自定义协议 serve 页面 + 收 JSON-RPC (同 origin 免 CORS)
//   3. 单通道桥接 (capability 默认 deny 门控): fs / exec / fetch / dialog / path / env / temp
//   4. 不背 Tauri plugin 系统, 二进制更小, 不依赖 __TAURI_INTERNALS__

#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

use std::borrow::Cow;

use serde_json::json;
use tao::dpi::LogicalSize;
use tao::event_loop::EventLoop;
use tao::window::WindowBuilder;
use wry::http::{Method, Request, Response, StatusCode};
use wry::WebViewBuilder;

use hermix_app_loader::bridge;
use hermix_app_loader::capability::{self, CapabilitySet};
use hermix_app_loader::payload;

/// Write a smoke-test log line (JSON) to `HERMIX_SMOKE_LOG` (or stdout when
/// that env is unset). Used by the host's `smoke` action / `custom_app_smoke_test`.
fn smoke_log(level: &str, message: &str) {
    let line = json!({ "level": level, "message": message }).to_string();
    match std::env::var("HERMIX_SMOKE_LOG") {
        Ok(path) if !path.is_empty() => {
            // Defensive: a malformed/unwritable path (e.g. a Unix-style
            // `/tmp/...` path passed by the host on Windows) must NOT silently
            // drop the smoke signal — the host blocks waiting for "ready"/"error"
            // and would otherwise hang. Create the parent dir, and on any write
            // error fall back to stdout so the signal is never lost.
            let path_ref = std::path::Path::new(&path);
            if let Some(parent) = path_ref.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if std::fs::write(path_ref, format!("{line}\n")).is_err() {
                println!("{line}");
            }
        }
        _ => println!("{line}"),
    }
}

fn main() {
    let payload = match payload::read_from_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[hermix-app-loader] 读取内嵌应用失败: {e}");
            std::process::exit(1);
        }
    };
    let caps: CapabilitySet = capability::from_manifest(&payload.manifest);
    eprintln!(
        "[hermix-app-loader] 启动 app=`{}` 已授权方法={:?}",
        payload.manifest.name,
        caps.debug_list()
    );

    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title(payload.manifest.name.clone())
        .with_inner_size(LogicalSize::new(
            payload.manifest.window_config.width,
            payload.manifest.window_config.height,
        ))
        .with_resizable(payload.manifest.window_config.resizable)
        .build(&event_loop)
        .expect("创建窗口失败");

    let html = payload.html.clone();
    let caps_for_handler = caps.clone();

    let webview = WebViewBuilder::new()
        .with_custom_protocol("app".to_string(), move |_id, req: Request<Vec<u8>>| {
            handle_request(&req, &html, &caps_for_handler)
        })
        .with_url("app://localhost/index.html")
        .build(&window);

    let _webview = match webview {
        Ok(w) => w,
        Err(e) => {
            if std::env::var("HERMIX_SMOKE").as_deref() == Ok("1") {
                smoke_log("error", &format!("WebView 构建失败: {e}"));
            }
            eprintln!("[hermix-app-loader] WebView 构建失败: {e}");
            std::process::exit(1);
        }
    };

    // 冒烟模式: 构建成功即视为已挂载, 回报 ready 并退出 (不进入事件循环)。
    // 无显示器环境下也能验证 payload 解析 + WebView 构建链路。
    if std::env::var("HERMIX_SMOKE").as_deref() == Ok("1") {
        smoke_log("ready", &format!("app {} 已挂载 (payload 解析 + WebView 构建成功)", payload.manifest.name));
        return;
    }

    event_loop.run(|_event, _elwt, _cf| {});
}

fn handle_request(
    req: &Request<Vec<u8>>,
    html: &str,
    caps: &CapabilitySet,
) -> Response<Cow<'static, [u8]>> {
    let path = req.uri().path();
    match (path, req.method()) {
        ("/" | "/index.html", _) => html_response(html),
        ("/rpc", &Method::POST) => json_response(bridge::dispatch(req.body(), caps)),
        _ => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Cow::Borrowed(&b"not found"[..]))
            .unwrap(),
    }
}

fn html_response(html: &str) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .header("Content-Type", "text/html; charset=utf-8")
        .header("Access-Control-Allow-Origin", "*")
        .body(Cow::Owned(html.as_bytes().to_vec()))
        .unwrap()
}

fn json_response(s: String) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(Cow::Owned(s.into_bytes()))
        .unwrap()
}
