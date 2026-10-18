//! MCP Transport Layer
//!
//! Provides transport abstraction for MCP communication:
//! - **StdioTransport**: Communicates via stdin/stdout with a child process (JSON-RPC)
//!
//! The transport layer handles:
//! - Message framing with Content-Length header + JSON body
//! - Connection lifecycle (start, send, receive, close)
//! - Error handling and connection lifecycle

use std::collections::HashMap;
use std::process::Stdio;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tracing::{error, info, trace};

use crate::McpError;

#[cfg(target_os = "windows")]
use hermix_core::runtime::resolve_binary;

fn classify_http_status_error(status: reqwest::StatusCode, body: &str) -> McpError {
    let msg = format!("HTTP error {}: {}", status, body);
    match status.as_u16() {
        401 => McpError::Auth(msg),
        403 => McpError::Forbidden(msg),
        404 => McpError::ResourceNotFound(msg),
        400 | 422 => McpError::InvalidParams(msg),
        501 | 503 => McpError::NotConfigured(msg),
        _ => McpError::ConnectionError(msg),
    }
}

// ---------------------------------------------------------------------------
// McpTransport trait
// ---------------------------------------------------------------------------

/// Transport trait for MCP communication.
/// Implementations handle the low-level details of sending and receiving
/// JSON-RPC messages over different transport mechanisms.
#[async_trait]
pub trait McpTransport: Send + Sync {
    /// Start the transport connection.
    async fn start(&mut self) -> Result<(), McpError>;

    /// Send a JSON-RPC message.
    async fn send(&mut self, message: Value) -> Result<(), McpError>;

    /// Receive a JSON-RPC message.
    async fn receive(&mut self) -> Result<Value, McpError>;

    /// Close the transport connection.
    async fn close(&mut self) -> Result<(), McpError>;
}

// ---------------------------------------------------------------------------
// StdioTransport
// ---------------------------------------------------------------------------

/// Transport that communicates via stdin/stdout with a child process.
/// Uses the MCP message framing protocol:
/// - Each message is prefixed with a `Content-Length` header
/// - The body is a UTF-8 encoded JSON string
/// - Messages are separated by `\r\n\r\n` between header and body
/// This is the standard transport for local MCP server processes.
pub struct StdioTransport {
    /// The command to execute.
    command: String,
    /// Arguments for the command.
    args: Vec<String>,
    /// Environment variables for the child process.
    env: HashMap<String, String>,
    /// The child process.
    child: Option<Child>,
    /// Whether the transport has been started.
    started: bool,
}

impl StdioTransport {
    /// Create a new stdio transport for the given command.
    pub fn new(command: impl Into<String>, args: &[String], env: &HashMap<String, String>) -> Self {
        Self {
            command: command.into(),
            args: args.to_vec(),
            env: env.clone(),
            child: None,
            started: false,
        }
    }
}

#[async_trait]
impl McpTransport for StdioTransport {
    async fn start(&mut self) -> Result<(), McpError> {
        if self.started {
            return Err(McpError::ConnectionError(
                "Transport already started".to_string(),
            ));
        }

        info!(
            "Starting MCP stdio transport: {} {:?}",
            self.command, self.args
        );

        // On Windows, npm-family toolchain entries ship only as `.cmd`
        // / `.bat` shims and `Command::new` cannot resolve a bare
        // name. `resolve_binary` only touches the npm whitelist
        // (npx/npm/pnpm/yarn/…), so explicit user-configured
        // commands like `uv` or `python` still pass through.
        #[cfg(target_os = "windows")]
        let resolved_command = resolve_binary(&self.command);
        #[cfg(not(target_os = "windows"))]
        let resolved_command = self.command.clone();

        let mut cmd = Command::new(&resolved_command);
        cmd.args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Windows：MCP server 是**后台服务**（stdio 管道通信，用户根本不看
        // 它的窗口）。不挡的话每次自动拉起 server 都会闪一个黑窗 ——
        // 而且 npm 系（`npm`/`npx`/`node`）在 Windows 上只有 `.cmd` 批处理
        // shim，Windows 借 `cmd.exe` 执行，父进程无控制台时它就自己开一扇
        // 窗（就是启动时那个 `C:\WINDOWS\system32\cmd.exe`）。
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

        // Set environment variables
        for (key, value) in &self.env {
            cmd.env(key, value);
        }

        let child = cmd.spawn().map_err(|e| {
            // The underlying OS error from Windows for "missing .cmd"
            // is `The system cannot find the file specified.` (os
            // error 2). That message is opaque — turn it into
            // actionable guidance for the user.
            let hint = if e.raw_os_error() == Some(2) {
                let mut h = String::from(
                    " (hint: on Windows the command must include the \
                     extension, e.g. `npx.cmd` for the npm toolchain; \
                     MCP server configs with bare `npx`/`npm` need the \
                     `.cmd` suffix unless you use the bundled helper)"
                );
                #[cfg(target_os = "windows")]
                {
                    if !self.command.ends_with(".cmd")
                        && !self.command.ends_with(".bat")
                        && !self.command.ends_with(".exe")
                    {
                        h.push_str(&format!(
                            " — tried '{}' but CreateProcess requires the extension",
                            resolved_command
                        ));
                    }
                }
                h
            } else {
                String::new()
            };
            McpError::ConnectionError(format!(
                "Failed to spawn process '{}': {}{}",
                self.command, e, hint
            ))
        })?;

        self.child = Some(child);
        self.started = true;

        Ok(())
    }

    async fn send(&mut self, message: Value) -> Result<(), McpError> {
        let child = self
            .child
            .as_mut()
            .ok_or(McpError::ConnectionError("Process not started".to_string()))?;

        let stdin = child
            .stdin
            .as_mut()
            .ok_or(McpError::ConnectionError("stdin not available".to_string()))?;

        let body =
            serde_json::to_string(&message).map_err(|e| McpError::Serialization(e.to_string()))?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());

        trace!("Sending MCP message: {} bytes", body.len());

        stdin
            .write_all(header.as_bytes())
            .await
            .map_err(|e| McpError::ConnectionError(format!("Write header failed: {}", e)))?;
        stdin
            .write_all(body.as_bytes())
            .await
            .map_err(|e| McpError::ConnectionError(format!("Write body failed: {}", e)))?;
        stdin
            .flush()
            .await
            .map_err(|e| McpError::ConnectionError(format!("Flush failed: {}", e)))?;

        Ok(())
    }

    async fn receive(&mut self) -> Result<Value, McpError> {
        let child = self
            .child
            .as_mut()
            .ok_or(McpError::ConnectionError("Process not started".to_string()))?;

        let stdout = child.stdout.as_mut().ok_or(McpError::ConnectionError(
            "stdout not available".to_string(),
        ))?;

        // Read Content-Length header
        let mut reader = tokio::io::BufReader::new(stdout);

        // Read header lines until we get Content-Length or empty line
        let mut content_length: usize = 0;
        loop {
            let mut line = String::new();
            reader
                .read_line(&mut line)
                .await
                .map_err(|e| McpError::ConnectionError(format!("Read header failed: {}", e)))?;

            if line.is_empty() {
                return Err(McpError::ConnectionClosed);
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                // End of headers
                break;
            }

            if let Some(len_str) = trimmed.strip_prefix("Content-Length:") {
                content_length =
                    len_str
                        .trim()
                        .parse::<usize>()
                        .map_err(|e| McpError::Protocol {
                            code: -32700,
                            message: format!("Invalid Content-Length: {}", e),
                        })?;
            }
        }

        if content_length == 0 {
            return Err(McpError::Protocol {
                code: -32700,
                message: "Missing Content-Length header".to_string(),
            });
        }

        // Read the JSON body
        let mut body_buf = vec![0u8; content_length];
        reader
            .read_exact(&mut body_buf)
            .await
            .map_err(|e| McpError::ConnectionError(format!("Read body failed: {}", e)))?;

        let body_str = String::from_utf8(body_buf).map_err(|e| McpError::Protocol {
            code: -32700,
            message: format!("Invalid UTF-8 in body: {}", e),
        })?;

        trace!("Received MCP message: {} bytes", body_str.len());

        let value: Value = serde_json::from_str(&body_str).map_err(|e| McpError::Protocol {
            code: -32700,
            message: format!("JSON parse error: {}", e),
        })?;

        Ok(value)
    }

    async fn close(&mut self) -> Result<(), McpError> {
        if let Some(mut child) = self.child.take() {
            info!("Shutting down MCP stdio process");
            // Try graceful shutdown first
            match child.kill().await {
                Ok(()) => {
                    // Wait for the process to exit
                    let _ = child.wait().await;
                }
                Err(e) => {
                    error!("Failed to kill MCP process: {}", e);
                }
            }
        }
        self.started = false;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ServerStdioTransport — server-side stdio (reads own stdin, writes own stdout)
// ---------------------------------------------------------------------------

/// Server-side stdio transport that reads from the process's own stdin
/// and writes to stdout. Used when hermes itself acts as an MCP server
/// (e.g. `hermes mcp serve`).
pub struct ServerStdioTransport {
    started: bool,
}

impl Default for ServerStdioTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerStdioTransport {
    pub fn new() -> Self {
        Self { started: false }
    }
}

#[async_trait]
impl McpTransport for ServerStdioTransport {
    async fn start(&mut self) -> Result<(), McpError> {
        if self.started {
            return Err(McpError::ConnectionError(
                "Transport already started".to_string(),
            ));
        }
        self.started = true;
        Ok(())
    }

    async fn send(&mut self, message: Value) -> Result<(), McpError> {
        let body =
            serde_json::to_string(&message).map_err(|e| McpError::Serialization(e.to_string()))?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());

        let mut stdout = tokio::io::stdout();
        stdout
            .write_all(header.as_bytes())
            .await
            .map_err(|e| McpError::ConnectionError(format!("Write header failed: {}", e)))?;
        stdout
            .write_all(body.as_bytes())
            .await
            .map_err(|e| McpError::ConnectionError(format!("Write body failed: {}", e)))?;
        stdout
            .flush()
            .await
            .map_err(|e| McpError::ConnectionError(format!("Flush failed: {}", e)))?;
        Ok(())
    }

    async fn receive(&mut self) -> Result<Value, McpError> {
        let stdin = tokio::io::stdin();
        let mut reader = tokio::io::BufReader::new(stdin);

        let mut content_length: usize = 0;
        loop {
            let mut line = String::new();
            let n = reader
                .read_line(&mut line)
                .await
                .map_err(|e| McpError::ConnectionError(format!("Read header failed: {}", e)))?;
            if n == 0 {
                return Err(McpError::ConnectionClosed);
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(len_str) = trimmed.strip_prefix("Content-Length:") {
                content_length =
                    len_str
                        .trim()
                        .parse::<usize>()
                        .map_err(|e| McpError::Protocol {
                            code: -32700,
                            message: format!("Invalid Content-Length: {}", e),
                        })?;
            }
        }

        if content_length == 0 {
            return Err(McpError::Protocol {
                code: -32700,
                message: "Missing Content-Length header".to_string(),
            });
        }

        let mut body_buf = vec![0u8; content_length];
        reader
            .read_exact(&mut body_buf)
            .await
            .map_err(|e| McpError::ConnectionError(format!("Read body failed: {}", e)))?;

        let body_str = String::from_utf8(body_buf).map_err(|e| McpError::Protocol {
            code: -32700,
            message: format!("Invalid UTF-8 in body: {}", e),
        })?;

        serde_json::from_str(&body_str).map_err(|e| McpError::Protocol {
            code: -32700,
            message: format!("JSON parse error: {}", e),
        })
    }

    async fn close(&mut self) -> Result<(), McpError> {
        self.started = false;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// StreamableHttpTransport
// ---------------------------------------------------------------------------

/// Streamable HTTP transport (MCP spec 2025-03-26 and later): every JSON-RPC
/// message POSTs to the one endpoint and the response comes back inline
/// (`application/json`) or as an SSE stream on the same POST. Notifications
/// are acknowledged with 202 and no body.
///
/// Replaces the previous `HttpSseTransport`, which could not talk to any
/// conformant server: its `start()` read the SSE response body to completion
/// (an SSE stream never ends → hung forever), parsed the `endpoint` event as
/// JSON where the spec sends a bare path, and failed on empty 202 bodies.
/// Stateless servers need no session id; we still echo `Mcp-Session-Id` when
/// the server issues one so stateful servers work too.
pub struct StreamableHttpTransport {
    /// The MCP endpoint URL (e.g. `https://host/ops/mcp`).
    url: String,
    /// Optional bearer token for authentication.
    auth_token: Option<String>,
    /// HTTP client with bounded connect/total timeouts — without a total
    /// timeout a hung server stalls the whole agent loop.
    client: reqwest::Client,
    /// Session id echoed back from the server, if any.
    session_id: Option<String>,
    /// Pending response from the last POST.
    pending_response: Option<Value>,
}

impl StreamableHttpTransport {
    /// Create a new streamable HTTP transport for the given endpoint URL.
    pub fn new(url: &str, auth_token: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            url: url.to_string(),
            auth_token,
            client,
            session_id: None,
            pending_response: None,
        }
    }

    /// Extract the JSON payload from a response body: the raw body when the
    /// server answered inline JSON, otherwise the last `data:` line of the
    /// SSE stream.
    fn extract_json(body: &str, content_type: &str) -> Result<Value, McpError> {
        let json_body = if content_type.contains("text/event-stream") || body.contains("data: ") {
            let mut last: Option<&str> = None;
            for line in body.lines() {
                if let Some(data) = line.strip_prefix("data: ") {
                    last = Some(data);
                }
            }
            let data = last.ok_or_else(|| McpError::Protocol {
                code: -32700,
                message: "SSE response carried no data line".to_string(),
            })?;
            data.to_string()
        } else {
            body.to_string()
        };
        if json_body.trim().is_empty() {
            // 202-style empty body (notification ack).
            return Ok(Value::Null);
        }
        serde_json::from_str(json_body.trim()).map_err(|e| McpError::Protocol {
            code: -32700,
            message: format!("Invalid JSON response: {}", e),
        })
    }
}

#[async_trait]
impl McpTransport for StreamableHttpTransport {
    async fn start(&mut self) -> Result<(), McpError> {
        // Streamable HTTP has no out-of-band handshake — the JSON-RPC
        // `initialize` (the first send) is the reachability check.
        info!("MCP streamable HTTP transport ready for: {}", self.url);
        Ok(())
    }

    async fn send(&mut self, message: Value) -> Result<(), McpError> {
        let mut builder = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/json")
            // Both accepts: servers choose inline JSON or an SSE stream.
            .header("Accept", "application/json, text/event-stream");
        if let Some(ref token) = self.auth_token {
            builder = builder.header("Authorization", format!("Bearer {}", token));
        }
        if let Some(ref sid) = self.session_id {
            builder = builder.header("Mcp-Session-Id", sid);
        }
        let response = builder
            .json(&message)
            .send()
            .await
            .map_err(|e| McpError::ConnectionError(format!("HTTP send failed: {}", e)))?;

        if let Some(sid) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            self.session_id = Some(sid.to_string());
        }

        let status = response.status();
        if status.as_u16() == 202 {
            // Notification accepted; no response body follows.
            self.pending_response = Some(Value::Null);
            return Ok(());
        }
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown error".to_string());
            return Err(classify_http_status_error(status, &body));
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = response
            .text()
            .await
            .map_err(|e| McpError::ConnectionError(format!("Failed to read response: {}", e)))?;

        self.pending_response = Some(Self::extract_json(&body, &content_type)?);
        Ok(())
    }

    async fn receive(&mut self) -> Result<Value, McpError> {
        match self.pending_response.take() {
            Some(value) => Ok(value),
            None => Err(McpError::ConnectionError(
                "No pending response. Call send() first.".to_string(),
            )),
        }
    }

    async fn close(&mut self) -> Result<(), McpError> {
        info!("Closing MCP streamable HTTP transport");
        self.session_id = None;
        self.pending_response = None;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stdio_transport_new() {
        let env = HashMap::new();
        let transport = StdioTransport::new("echo", &[], &env);
        assert_eq!(transport.command, "echo");
        assert!(transport.args.is_empty());
        assert!(!transport.started);
    }

    #[test]
    fn test_classify_http_status_error_forbidden() {
        let e = classify_http_status_error(reqwest::StatusCode::FORBIDDEN, "no capability");
        assert!(matches!(e, McpError::Forbidden(_)));
    }

    #[test]
    fn test_classify_http_status_error_not_configured() {
        let e =
            classify_http_status_error(reqwest::StatusCode::SERVICE_UNAVAILABLE, "not configured");
        assert!(matches!(e, McpError::NotConfigured(_)));
    }

    #[test]
    fn test_extract_json_inline() {
        let v = StreamableHttpTransport::extract_json(
            r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#,
            "application/json",
        )
        .unwrap();
        assert_eq!(v["result"]["ok"], true);
    }

    #[test]
    fn test_extract_json_sse_frame() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"x\":1}}\n\n";
        let v = StreamableHttpTransport::extract_json(body, "text/event-stream").unwrap();
        assert_eq!(v["result"]["x"], 1);
    }

    #[test]
    fn test_extract_json_empty_202() {
        let v = StreamableHttpTransport::extract_json("", "application/json").unwrap();
        assert!(v.is_null());
    }

    /// Minimal one-request HTTP server: echoes whether the Authorization
    /// header carried the expected bearer token, as inline JSON.
    async fn spawn_echo_server(expected_auth: &'static str) -> std::net::SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 65536];
                    let mut got = vec![];
                    // Read until the full request (headers + content-length
                    // body) has arrived.
                    let (headers_end, _content_length) = loop {
                        let n = sock.read(&mut buf).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        got.extend_from_slice(&buf[..n]);
                        if let Some(pos) = got
                            .windows(4)
                            .position(|w| w == b"\r\n\r\n")
                        {
                            let head = String::from_utf8_lossy(&got[..pos]).to_lowercase();
                            let cl = head
                                .lines()
                                .find_map(|l| l.strip_prefix("content-length:"))
                                .and_then(|v| v.trim().parse::<usize>().ok())
                                .unwrap_or(0);
                            if got.len() >= pos + 4 + cl {
                                break (pos, cl);
                            }
                        }
                    };
                    let head = String::from_utf8_lossy(&got[..headers_end]).to_string();
                    let auth_ok = head.contains(expected_auth);
                    let body = format!(
                        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"auth\":{}}}}}",
                        auth_ok
                    );
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    sock.write_all(resp.as_bytes()).await.unwrap();
                });
            }
        });
        addr
    }

    #[tokio::test]
    async fn test_streamable_http_roundtrip_sends_bearer() {
        let addr = spawn_echo_server("Bearer homk_secret").await;
        let mut transport =
            StreamableHttpTransport::new(&format!("http://{addr}/mcp"), Some("homk_secret".into()));
        transport.start().await.unwrap();
        transport
            .send(serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"protocolVersion": "2025-06-18"}
            }))
            .await
            .unwrap();
        let response = transport.receive().await.unwrap();
        assert_eq!(response["result"]["auth"], true, "bearer header must reach the server");
    }

    #[tokio::test]
    async fn test_streamable_http_sends_no_auth_header_without_token() {
        let addr = spawn_echo_server("Bearer homk_secret").await;
        let mut transport = StreamableHttpTransport::new(&format!("http://{addr}/mcp"), None);
        transport.start().await.unwrap();
        transport
            .send(serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize"}))
            .await
            .unwrap();
        let response = transport.receive().await.unwrap();
        assert_eq!(response["result"]["auth"], false);
    }
}
