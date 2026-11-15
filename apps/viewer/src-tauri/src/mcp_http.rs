//! MCP 服务的本地 HTTP 端点：让**任意** MCP 客户端连上来。
//!
//! 已有的是 stdio 版（`hermix-viewer --mcp`）：适合「客户端自己拉起进程」
//! 的配置（command + args）。但很多客户端只支持填 URL，而且「已经开着
//! 的查看器」也希望能被连上——所以这里再给一个 HTTP 端点：
//!
//! - `POST /mcp`：JSON-RPC 2.0 一次一帧（与 stdio 版共用同一套处理函数）。
//! - 只监听**回环地址**（127.0.0.1）。开放到局域网要显式改设置——
//!   这套工具能列目录、读文件、写文本、操作桌面 GUI，开到网上等于把
//!   机器交出去。
//!
//! 为什么自己写而不是引 axum：只需要「一个 POST + 一个 JSON 响应」，
//! 几十行 std::net 就够；引框架会带来一大棵依赖树（与「速度性能一流」
//! 的要求相悖）。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;
use std::sync::OnceLock;

use serde_json::Value;

use crate::mcp_server::handle_request;

/// 当前服务状态（None = 没起）。
#[derive(Clone, Debug)]
pub struct McpHttpState {
    pub endpoint: String,
    pub port: u16,
}

static STATE: OnceLock<Mutex<Option<McpHttpState>>> = OnceLock::new();

fn state() -> &'static Mutex<Option<McpHttpState>> {
    STATE.get_or_init(|| Mutex::new(None))
}

/// 起一个本地 MCP HTTP 端点。返回对外可写的地址。
pub fn start(port: u16, allow_remote: bool) -> Result<McpHttpState, String> {
    if let Some(cur) = state().lock().unwrap().clone() {
        // 已经在跑：端口变了就重启，端口没变就直接返回现状。
        if cur.port == port {
            return Ok(cur);
        }
        stop();
    }
    let host = if allow_remote { "0.0.0.0" } else { "127.0.0.1" };
    let listener = TcpListener::bind((host, port))
        .map_err(|e| format!("MCP 端口 {port} 绑定失败: {e}"))?;
    let bound = listener
        .local_addr()
        .map_err(|e| format!("读取监听地址失败: {e}"))?;
    let actual_port = bound.port();
    let public_host = if allow_remote { "127.0.0.1" } else { "127.0.0.1" };
    let endpoint = format!("http://{public_host}:{actual_port}/mcp");

    std::thread::Builder::new()
        .name("viewer-mcp-http".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                std::thread::spawn(move || {
                    let _ = handle_conn(stream);
                });
            }
        })
        .map_err(|e| format!("MCP 线程启动失败: {e}"))?;

    let st = McpHttpState {
        endpoint: endpoint.clone(),
        port: actual_port,
    };
    *state().lock().unwrap() = Some(st.clone());
    if allow_remote {
        eprintln!(
            "[viewer-mcp] 警告：MCP 端点监听在 0.0.0.0，局域网内可访问——\
             这套工具能读写文件并操作桌面 GUI，确认网络环境可控再开。"
        );
    }
    Ok(st)
}

/// 停掉本地 MCP 端点（监听器随线程结束自然关闭；这里只清状态）。
pub fn stop() {
    *state().lock().unwrap() = None;
}

/// 当前状态（None = 没起）。
pub fn status() -> Option<McpHttpState> {
    state().lock().unwrap().clone()
}

/// 处理一条 HTTP 连接：读请求 → 若是 POST /mcp 就跑 JSON-RPC → 回 JSON。
fn handle_conn(mut stream: TcpStream) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    // 读头部（到 \r\n\r\n 为止）。
    let head_end = loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(()); // 客户端没发完就走了
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_sub(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > 64 * 1024 {
            return respond(&mut stream, 413, "{\"error\":\"请求头过大\"}");
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default().to_string();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();

    // Content-Length 决定还要读多少字节。
    let mut content_length = 0usize;
    for line in lines {
        let Some((k, v)) = line.split_once(':') else { continue };
        if k.trim().eq_ignore_ascii_case("content-length") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = buf[head_end..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }

    if method != "POST" || !path.starts_with("/mcp") {
        return respond(&mut stream, 404, "{\"error\":\"只有 POST /mcp 可用\"}");
    }
    if content_length > 4 * 1024 * 1024 {
        return respond(&mut stream, 413, "{\"error\":\"请求体过大\"}");
    }
    let text = String::from_utf8_lossy(&body).to_string();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return respond(&mut stream, 400, "{\"error\":\"空请求体\"}");
    }
    let request: Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(e) => {
            return respond(
                &mut stream,
                400,
                &format!("{{\"error\":\"不是合法 JSON: {e}\"}}"),
            )
        }
    };
    // 通知（无 id）没有响应体：回 202 Accepted。
    let response = handle_request(&request);
    match response {
        Some(v) => respond(&mut stream, 200, &v.to_string()),
        None => respond(&mut stream, 202, ""),
    }
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        404 => "Not Found",
        413 => "Payload Too Large",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    if !body.is_empty() {
        stream.write_all(body.as_bytes())?;
    }
    stream.flush()
}

fn find_sub(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_header_terminator() {
        assert_eq!(find_sub(b"a\r\n\r\nb", b"\r\n\r\n"), Some(1));
        assert!(find_sub(b"abc", b"\r\n\r\n").is_none());
    }
}
