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
use wry::http::{header, Method, Request, Response, StatusCode};
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
            // 追加模式: JS 可能连续发多条 (onerror + unhandledrejection + DOMContentLoaded)
            let path_ref = std::path::Path::new(&path);
            if let Some(parent) = path_ref.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            // 使用追加写入, 保留之前所有日志行
            let mut content = std::fs::read_to_string(path_ref).unwrap_or_default();
            content.push_str(&line);
            content.push('\n');
            if std::fs::write(path_ref, &content).is_err() {
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
    let is_smoke = std::env::var("HERMIX_SMOKE").as_deref() == Ok("1");

    // 冒烟模式: 注入 JS 代码捕获 console 错误和未处理异常, 回报给宿主
    let injection = if is_smoke {
        r#"<script>
(function(){
  // 捕获 window.onerror
  window.onerror = function(msg, src, line, col, err) {
    fetch('app://localhost/smoke-log', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({level:'error',message:msg+' (line:'+line+')'})
    }).catch(function(){});
  };
  // 捕获未处理的 Promise rejection
  window.addEventListener('unhandledrejection', function(e) {
    var msg = e.reason && e.reason.message ? e.reason.message : String(e.reason);
    fetch('app://localhost/smoke-log', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({level:'error',message:'Unhandled rejection: '+msg})
    }).catch(function(){});
  });
  // 页面加载完成后回报 ready
  window.addEventListener('DOMContentLoaded', function() {
    fetch('app://localhost/smoke-log', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({level:'ready',message:'DOMContentLoaded fired'})
    }).catch(function(){});
  });
})();
</script>"#
    } else {
        ""
    };

    // 将注入的脚本插入到 </head> 或 <html> 之前
    let final_html = if is_smoke && !injection.is_empty() {
        if let Some(pos) = html.rfind("</head>") {
            format!("{}{}{}", &html[..pos], injection, &html[pos..])
        } else if let Some(pos) = html.rfind("</html>") {
            format!("{}{}{}", &html[..pos], injection, &html[pos..])
        } else {
            format!("{}{}", html, injection)
        }
    } else {
        html
    };

    let webview = WebViewBuilder::new()
        .with_custom_protocol("app".to_string(), move |_id, req: Request<Vec<u8>>| {
            handle_request(&req, &final_html, &caps_for_handler, is_smoke)
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

    // 冒烟模式: WebView 构建成功 → 进入事件循环但设超时自动退出。
    // JS 注入的 onerror/unhandledrejection 会通过 /smoke-log 写日志。
    // 宿主在 20s 内轮询日志判断 ready/error。
    if is_smoke {
        smoke_log("info", &format!("app {} WebView 构建成功, 等待 JS 回报...", payload.manifest.name));
        // 给 WebView 时间运行 JS (DOMContentLoaded / onerror)
        // 用 setTimeout 模拟超时退出: 3 秒后如果没有 ready/error 就退出
        let start = std::time::Instant::now();
        event_loop.run(move |_event, _elwt, cf| {
            if start.elapsed() > std::time::Duration::from_secs(5) {
                *cf = tao::event_loop::ControlFlow::Exit;
            }
        });
    }

    event_loop.run(|_event, _elwt, _cf| {});
}

fn handle_request(
    req: &Request<Vec<u8>>,
    html: &str,
    caps: &CapabilitySet,
    is_smoke: bool,
) -> Response<Cow<'static, [u8]>> {
    let path = req.uri().path();
    match (path, req.method()) {
        ("/" | "/index.html", _) => html_response(html),
        ("/rpc", &Method::POST) => json_response(bridge::dispatch(req.body(), caps)),
        // 本地文件直通: 供 <audio>/<img>/<video> 等直接引用本地文件
        // 用法: app://localhost/file?path=C:/music/song.mp3
        ("/file", &Method::GET) => serve_file(req, caps),
        // 冒烟模式: JS 注入的错误/ready 回报端点
        ("/smoke-log", &Method::POST) if is_smoke => {
            if let Ok(body) = serde_json::from_slice::<serde_json::Value>(req.body()) {
                let level = body.get("level").and_then(|v| v.as_str()).unwrap_or("info");
                let message = body.get("message").and_then(|v| v.as_str()).unwrap_or("");
                smoke_log(level, message);
            }
            json_response(serde_json::json!({"ok":true}).to_string())
        }
        _ => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Cow::Borrowed(&b"not found"[..]))
            .unwrap(),
    }
}

/// 从请求的 query string 中提取参数值。
fn get_query_param(uri: &str, key: &str) -> Option<String> {
    let q = uri.split('?').nth(1)?;
    for pair in q.split('&') {
        let mut kv = pair.splitn(2, '=');
        if kv.next()? == key {
            let val = kv.next()?;
            return Some(percent_decode(val));
        }
    }
    None
}

/// 简单的 URL 解码。
fn percent_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""),
                16,
            ) {
                result.push(byte as char);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            result.push(' ');
        } else {
            result.push(bytes[i] as char);
        }
        i += 1;
    }
    result
}

/// 根据文件扩展名返回 MIME 类型。
fn mime_type(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        // 音频
        "mp3" => "audio/mpeg",
        "wav" | "wave" => "audio/wav",
        "ogg" => "audio/ogg",
        "flac" => "audio/flac",
        "m4a" | "aac" => "audio/aac",
        "wma" => "audio/x-ms-wma",
        // 视频
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        // 图片
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        // 文档
        "pdf" => "application/pdf",
        "html" | "htm" => "text/html",
        "txt" => "text/plain",
        "json" => "application/json",
        "xml" => "application/xml",
        "css" => "text/css",
        "js" => "application/javascript",
        // 默认
        _ => "application/octet-stream",
    }
}

/// 处理 /file 请求: 读取本地文件并返回, 支持 Range 请求。
/// 这是 <audio>/<video>/<img> 等元素直接播放本地文件的关键。
fn serve_file(req: &Request<Vec<u8>>, caps: &CapabilitySet) -> Response<Cow<'static, [u8]>> {
    // 权限检查: 需要 fs-read 权限
    if !caps.allows("fs_read") {
        return Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(Cow::Borrowed(&b"permission denied: fs-read required"[..]))
            .unwrap();
    }

    // 从 query string 解析 path 参数
    let uri = req.uri().to_string();
    let file_path = match get_query_param(&uri, "path") {
        Some(p) if !p.is_empty() => p,
        _ => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Cow::Borrowed(&b"missing 'path' query parameter"[..]))
                .unwrap();
        }
    };

    // 读取文件元数据
    let metadata = match std::fs::metadata(&file_path) {
        Ok(m) => m,
        Err(e) => {
            let status = if e.kind() == std::io::ErrorKind::NotFound {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            return Response::builder()
                .status(status)
                .body(Cow::Owned(format!("file error: {e}").into_bytes()))
                .unwrap();
        }
    };

    if metadata.is_dir() {
        return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Cow::Borrowed(&b"path is a directory"[..]))
            .unwrap();
    }

    let file_size = metadata.len();
    let content_type = mime_type(&file_path);

    // 检查 Range 请求头
    let range_header = req
        .headers()
        .get("range")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    if let Some(range_str) = range_header {
        // 解析 "bytes=start-end" 或 "bytes=start-"
        if let Some(range) = parse_range(&range_str, file_size) {
            let (start, end) = range;
            let length = end - start + 1;

            // 读取指定范围
            let data = match read_file_range(&file_path, start, length) {
                Ok(d) => d,
                Err(e) => {
                    return Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(Cow::Owned(format!("read error: {e}").into_bytes()))
                        .unwrap();
                }
            };

            Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_TYPE, content_type)
                .header(
                    header::CONTENT_RANGE,
                    format!("bytes {start}-{end}/{file_size}"),
                )
                .header(header::CONTENT_LENGTH, length.to_string())
                .header(header::ACCEPT_RANGES, "bytes")
                .header("Access-Control-Allow-Origin", "*")
                .body(Cow::Owned(data))
                .unwrap()
        } else {
            // Range 解析失败, 返回 416
            Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{file_size}"))
                .body(Cow::Borrowed(&b"invalid range"[..]))
                .unwrap()
        }
    } else {
        // 无 Range 头: 返回整个文件
        let data = match std::fs::read(&file_path) {
            Ok(d) => d,
            Err(e) => {
                return Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Cow::Owned(format!("read error: {e}").into_bytes()))
                    .unwrap();
            }
        };

        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CONTENT_LENGTH, file_size.to_string())
            .header(header::ACCEPT_RANGES, "bytes")
            .header("Access-Control-Allow-Origin", "*")
            .body(Cow::Owned(data))
            .unwrap()
    }
}

/// 解析 HTTP Range 头, 返回 (start, end) (含 end)。
fn parse_range(range_str: &str, file_size: u64) -> Option<(u64, u64)> {
    // 格式: "bytes=start-end" 或 "bytes=start-" 或 "bytes=-suffix"
    let s = range_str.strip_prefix("bytes=")?;
    let parts: Vec<&str> = s.splitn(2, '-').collect();
    if parts.len() != 2 {
        return None;
    }
    let start = if parts[0].is_empty() {
        // "bytes=-500" → 最后 500 字节
        let suffix: u64 = parts[1].parse().ok()?;
        if suffix > file_size {
            return Some((0, file_size - 1));
        }
        file_size - suffix
    } else {
        parts[0].parse().ok()?
    };
    let end = if parts[1].is_empty() {
        // "bytes=0-" → 从 start 到文件末尾
        file_size.saturating_sub(1)
    } else {
        let e: u64 = parts[1].parse().ok()?;
        e.min(file_size - 1)
    };
    if start > end || start >= file_size {
        return None;
    }
    Some((start, end))
}

/// 读取文件的指定范围 [start, start+length)。
fn read_file_range(path: &str, start: u64, length: u64) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut buf = vec![0u8; length as usize];
    file.read_exact(&mut buf)?;
    Ok(buf)
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
