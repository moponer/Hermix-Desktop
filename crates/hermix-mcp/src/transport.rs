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

    /// 切换 stdio 帧格式（[`McpFraming`]）。HTTP transport 忽略此调用。
    fn set_framing(&mut self, _framing: McpFraming) {}

    /// 是否支持帧格式回退——只有 stdio transport 返回 true，
    /// `McpClient::connect` 据此决定握手失败时是否换帧重连。
    fn supports_framing_fallback(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// StdioTransport
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Windows 进程树回收（Job Object, KILL_ON_JOB_CLOSE）
// ---------------------------------------------------------------------------
//
// 泄漏事故复盘（2026-11-14，~/.hermix/logs/server.log 实测）：npx 型 stdio
// MCP 握手失败 → 重试循环每轮重新 spawn，但旧进程树（cmd → node(npx) →
// node(server)）从不被终止 —— 2.5 小时堆了 23 份 / 46 个 node。根因有两层：
//   1. connect 失败时 client 被直接 drop，`close()`（唯一杀进程处）没被调用
//      —— `std::process::Child::drop` 只关句柄、不杀进程；
//   2. 即使调了 `close()`，`child.kill()` 也只杀**直接子进程**，杀不到
//      npx 派生的 node 孙进程。
// 治本：spawn 后立刻把直接子进程放进一个 KILL_ON_JOB_CLOSE 的 Job Object。
// Job 句柄关闭时 Windows 终止**整个 job 内的进程树**（自动分配：子进程
// 新 spawn 的后代自动入 job）。Job 句柄随 StdioTransport 生命周期走：
//   - 连接成功 → job 随 transport 一起活，服务器正常运行；
//   - 正常 disconnect → close() 先优雅杀直接子进程，再关 job 收编剩余；
//   - connect 失败 → client drop → 本文件的 `impl Drop` 兜底杀子 + 关 job。
// 用裸 kernel32 extern 而非引入 windows-sys：这 5 个调用是稳定 ABI，
// 不给 MCP 核心 crate 增加依赖面。
#[cfg(windows)]
mod job_kill {
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Default)]
    struct JobBasicLimitInfo {
        per_process_user_time_limit: u32,
        per_job_user_time_limit: u32,
        // JOBOBJECT_BASIC_LIMIT_INFORMATION.LimitFlags 是 DWORD（偏移 0x08）。
        limit_flags: u32,
        process_memory_limit: i64,
        job_memory_limit: i64,
        peak_process_memory_used: u32,
        peak_job_memory_used: u32,
    }

    extern "system" {
        fn CreateJobObjectW(lp_job_attributes: *const c_void, lp_name: *const u16) -> *mut c_void;
        fn SetInformationJobObject(
            h_job: *mut c_void,
            job_object_information_class: u32,
            lp_job_object_information: *const c_void,
            length: u32,
        ) -> i32;
        fn AssignProcessToJobObject(
            h_job: *mut c_void,
            h_process: *mut c_void,
        ) -> i32;
        fn OpenProcess(dw_desired_access: u32, b_inherit_handle: i32, dw_process_id: u32) -> *mut c_void;
        fn CloseHandle(h_object: *mut c_void) -> i32;
    }

    const JOB_OBJECT_BASIC_LIMIT_INFORMATION: u32 = 0;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;
    const PROCESS_SET_QUOTA: u32 = 0x100;

    /// 裸句柄的 Send+Sync 包装：job 句柄只在创建线程 attach、在 drop 时
    /// 关闭，跨线程移动（transport 在 tokio task 间流转）是安全的。
    struct SafeHandle(*mut c_void);

    // SAFETY: Windows 句柄是内核对象引用，跨线程持有/关闭由内核同步；
    // 本句柄生命周期内不会有并发 CloseHandle（Drop 单次执行）。
    unsafe impl Send for SafeHandle {}
    unsafe impl Sync for SafeHandle {}

    /// 一个 KILL_ON_JOB_CLOSE 的 Job 句柄。drop 时关句柄 → 整棵进程树终止。
    pub struct KillOnCloseJob {
        handle: SafeHandle,
    }

    impl KillOnCloseJob {
        /// 把 `pid` 直接挂进新建 job。失败一律返回 None（降级为"只杀直接子
        /// 进程"，绝不让 job 问题阻塞 MCP 启动）。
        pub fn attach(pid: u32) -> Option<Self> {
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return None;
                }
                let mut info: JobBasicLimitInfo = Default::default();
                info.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    job,
                    JOB_OBJECT_BASIC_LIMIT_INFORMATION,
                    &info as *const JobBasicLimitInfo as *const c_void,
                    std::mem::size_of::<JobBasicLimitInfo>() as u32,
                );
                if ok == 0 {
                    CloseHandle(job);
                    return None;
                }
                let proc = OpenProcess(PROCESS_SET_QUOTA, 0, pid);
                if proc.is_null() {
                    CloseHandle(job);
                    return None;
                }
                let ok = AssignProcessToJobObject(job, proc);
                CloseHandle(proc);
                if ok == 0 {
                    CloseHandle(job);
                    return None;
                }
                Some(Self { handle: SafeHandle(job) })
            }
        }
    }

    impl Drop for KillOnCloseJob {
        fn drop(&mut self) {
            // 关句柄 = 终止 job 内全部进程（KILL_ON_JOB_CLOSE）。
            unsafe {
                CloseHandle(self.handle.0);
            }
        }
    }
}

/// stdio 帧格式。MCP 现行规范（2025-06-18 起）stdio 用换行分隔 JSON（JSONL）；
/// 更早的 SDK 服务器仍用 `Content-Length` 头 + JSON 体（LSP 风格）。
///
/// 2026-11-14 实测：i18n-mcp-server 与 bing-cn-mcp-enhanced（均 SDK 1.32.2）
/// 只认 JSONL——旧 hermix 客户端发 Content-Length 帧，服务器第一行
/// `JSON.parse("Content-Length: …")` 失败即 onerror→close，握手永远完不成
/// （这才是 "i18n Workflow connect failed" 的真正根因；npx 慢只是加重症状）。
/// 默认 JSONL（现行规范），握手无响应时由 `McpClient::connect` 回退
/// ContentLength 重连一次，覆盖新旧两代 SDK 服务器。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpFraming {
    /// 换行分隔 JSON —— 现行 MCP 规范（SDK ≥ 1.12 左右）。
    Jsonl,
    /// `Content-Length: N\r\n\r\n{body}` —— 旧 SDK 服务器。
    ContentLength,
}

/// Transport that communicates via stdin/stdout with a child process.
/// 帧格式由 [`McpFraming`] 决定（见其文档）。
pub struct StdioTransport {
    /// The command to execute.
    command: String,
    /// Arguments for the command.
    args: Vec<String>,
    /// Environment variables for the child process.
    env: HashMap<String, String>,
    /// The child process.
    child: Option<Child>,
    /// start() 后以 owned 方式持有的管道。reader 必须跨 receive() 复用：
    /// 每次调用都重新包 BufReader 的话，上一个 reader 预读进内部缓冲的
    /// 字节随其 drop 丢失（服务器一次性吐出多条消息时，第二条会永久失踪）。
    stdin: Option<tokio::process::ChildStdin>,
    stdout: Option<tokio::process::ChildStdout>,
    reader: Option<tokio::io::BufReader<tokio::process::ChildStdout>>,
    /// Windows：持有直接子进程所属的 KillOnCloseJob 句柄。transport 死亡
    /// （正常 close / 失败路径 drop）时关句柄 → 整棵进程树被终止，重试循环
    /// 不再累积进程树（2026-11-14 泄漏事故的治本点）。
    #[cfg(windows)]
    job: Option<job_kill::KillOnCloseJob>,
    /// Whether the transport has been started.
    started: bool,
    /// 当前帧格式（JSONL 为现行规范默认）。
    framing: McpFraming,
}

impl StdioTransport {
    /// Create a new stdio transport for the given command.
    pub fn new(command: impl Into<String>, args: &[String], env: &HashMap<String, String>) -> Self {
        Self {
            command: command.into(),
            args: args.to_vec(),
            env: env.clone(),
            child: None,
            stdin: None,
            stdout: None,
            reader: None,
            #[cfg(windows)]
            job: None,
            started: false,
            framing: McpFraming::Jsonl,
        }
    }

    /// 子进程 PID（若已 spawn）。诊断与防泄漏测试用。
    pub fn child_pid(&self) -> Option<u32> {
        self.child.as_ref().and_then(|c| c.id())
    }

    /// 当前帧格式。
    pub fn framing(&self) -> McpFraming {
        self.framing
    }
}

impl Drop for StdioTransport {
    /// 失败路径兜底：connect 失败时 `McpClient` 被直接 drop、`close()` 不会
    /// 被调用。没有这个兜底，子进程永远活着（node 在等 stdin），每轮重试
    /// 净增一棵进程树（2026-11-14 泄漏事故：2.5h 堆 23 份）。
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // 同步 kill：tokio 的 `Child::kill()` 返回 Future（在 Drop 里 await
            // 不可能，裸调又只是创建被丢弃的 future），`start_kill` 才是
            // Drop 里能用的同步版本。
            let _ = child.start_kill();
        }
        // Windows：job 字段随后 drop → 关句柄 → 杀剩余整树（cmd→node→node）。
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

        let mut child = cmd.spawn().map_err(|e| {
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

        // spawn 后立刻把直接子进程放进 kill job：cmd 派生的 node（npx）、
        // node 再派的 node（服务器本体）经自动分配全部入 job。attach 失败
        // 降级为只杀直接子进程，不阻塞 MCP 启动。
        #[cfg(windows)]
        {
            let job = child.id().and_then(job_kill::KillOnCloseJob::attach);
            if job.is_none() {
                tracing::warn!(
                    pid = ?child.id(),
                    "MCP stdio child not attached to kill job; falling back to direct-child-only kill"
                );
            }
            self.job = job;
        }
        // 先把管道抽出来以 owned 方式持有（reader 需跨 receive() 复用，
        // 见字段注释），再存 child（kill/wait 仍走 child）。
        self.stdin = child.stdin.take();
        self.stdout = child.stdout.take();
        self.child = Some(child);
        self.started = true;

        Ok(())
    }

    async fn send(&mut self, message: Value) -> Result<(), McpError> {
        let body =
            serde_json::to_string(&message).map_err(|e| McpError::Serialization(e.to_string()))?;

        match self.framing {
            McpFraming::Jsonl => {
                let mut out = body.into_bytes();
                out.push(b'\n');
                trace!("Sending MCP message (JSONL): {} bytes", out.len());
                let stdin = self.stdin.as_mut().ok_or_else(|| {
                    McpError::ConnectionError("stdin not available".to_string())
                })?;
                stdin
                    .write_all(&out)
                    .await
                    .map_err(|e| McpError::ConnectionError(format!("Write failed: {}", e)))?;
                stdin
                    .flush()
                    .await
                    .map_err(|e| McpError::ConnectionError(format!("Flush failed: {}", e)))?;
            }
            McpFraming::ContentLength => {
                let header = format!("Content-Length: {}\r\n\r\n", body.len());
                trace!("Sending MCP message (Content-Length): {} bytes", body.len());
                let stdin = self.stdin.as_mut().ok_or_else(|| {
                    McpError::ConnectionError("stdin not available".to_string())
                })?;
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
            }
        }

        Ok(())
    }

    async fn receive(&mut self) -> Result<Value, McpError> {
        // 持久化 reader（字段注释解释了为什么不能每次重建）。
        if self.reader.is_none() {
            self.reader = self.stdout.take().map(tokio::io::BufReader::new);
        }
        let reader = self
            .reader
            .as_mut()
            .ok_or_else(|| McpError::ConnectionError("stdout not available".to_string()))?;

        match self.framing {
            McpFraming::Jsonl => {
                // 换行分隔 JSON（MCP 2025-06-18 规范）：一行一条完整消息。
                // 跳过空行（个别服务器会发多余换行）。
                loop {
                    let mut line = String::new();
                    let n = reader
                        .read_line(&mut line)
                        .await
                        .map_err(|e| McpError::ConnectionError(format!("Read line failed: {}", e)))?;
                    if n == 0 {
                        return Err(McpError::ConnectionClosed);
                    }
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let value: Value =
                        serde_json::from_str(trimmed).map_err(|e| McpError::Protocol {
                            code: -32700,
                            message: format!("JSON parse error: {}", e),
                        })?;
                    trace!("Received MCP message (JSONL): {} bytes", n);
                    return Ok(value);
                }
            }
            McpFraming::ContentLength => {
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

                trace!("Received MCP message (Content-Length): {} bytes", body_str.len());

                let value: Value =
                    serde_json::from_str(&body_str).map_err(|e| McpError::Protocol {
                        code: -32700,
                        message: format!("JSON parse error: {}", e),
                    })?;

                Ok(value)
            }
        }
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
        // 关 kill job → 终止整棵进程树（直接子进程的后代：cmd→node→node）。
        // 没有这一步，`child.kill()` 杀完直接子进程后孙进程仍会存活。
        #[cfg(windows)]
        self.job.take();
        // 释放持久化 reader（持有 stdout 管道），避免挂到下一轮重连。
        self.reader.take();
        self.started = false;
        Ok(())
    }

    fn set_framing(&mut self, framing: McpFraming) {
        self.framing = framing;
    }

    fn supports_framing_fallback(&self) -> bool {
        true
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

// ---------------------------------------------------------------------------
// 防泄漏回归测试（2026-11-14 事故）：npx 型 stdio MCP 连接失败重试时，
// 旧进程树必须被整树终止，否则每轮重试净增一棵 cmd→node→node。
// ---------------------------------------------------------------------------
#[cfg(test)]
mod leak_tests {
    use super::StdioTransport;
    use super::McpTransport;
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    /// 跨平台"进程是否已死"检查。
    fn is_dead(pid: u32) -> bool {
        #[cfg(windows)]
        {
            mod win {
                use std::ffi::c_void;
                extern "system" {
                    fn OpenProcess(dw: u32, b: i32, pid: u32) -> *mut c_void;
                    fn CloseHandle(h: *mut c_void) -> i32;
                    fn GetExitCodeProcess(h: *mut c_void, code: *mut u32) -> i32;
                }
                pub const STILL_ACTIVE: u32 = 259;
                pub fn check(pid: u32) -> bool {
                    unsafe {
                        // PROCESS_QUERY_INFORMATION
                        let h = OpenProcess(0x400, 0, pid);
                        if h.is_null() {
                            return true; // 连句柄都拿不到 = 已死
                        }
                        let mut code = STILL_ACTIVE;
                        let exited = GetExitCodeProcess(h, &mut code) != 0 && code != STILL_ACTIVE;
                        CloseHandle(h);
                        exited
                    }
                }
            }
            win::check(pid)
        }
        #[cfg(not(windows))]
        {
            !std::path::Path::new(&format!("/proc/{pid}")).exists()
        }
    }

    fn wait_dead(pids: &[u32], timeout: Duration) -> Vec<u32> {
        let start = Instant::now();
        loop {
            let alive: Vec<u32> = pids.iter().copied().filter(|p| !is_dead(*p)).collect();
            if alive.is_empty() || start.elapsed() > timeout {
                return alive;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// 失败路径兜底：drop transport（不经过 close）后，直接子进程必须死。
    /// 这是 2026-11-14 泄漏事故的最小子复现面。
    #[tokio::test]
    async fn drop_without_close_kills_direct_child() {
        #[cfg(windows)]
        let (cmd, args): (&str, Vec<String>) = (
            "cmd",
            ["/c", "ping", "-n", "30", "127.0.0.1"].iter().map(|s| s.to_string()).collect(),
        );
        #[cfg(not(windows))]
        let (cmd, args): (&str, Vec<String>) =
            ("sh", ["-c", "sleep 30"].iter().map(|s| s.to_string()).collect());

        let mut t = StdioTransport::new(cmd, &args, &HashMap::new());
        t.start().await.expect("spawn ok");
        let pid = t.child_pid().expect("child pid");
        drop(t);
        let alive = wait_dead(&[pid], Duration::from_secs(5));
        assert!(alive.is_empty(), "direct child {pid} must die on drop, alive: {alive:?}");
    }

    /// Windows 整树回收：drop 后**孙进程**（cmd 派生的 ping）也必须死。
    /// 没有 Job Object（KILL_ON_JOB_CLOSE）时这条必挂 —— 只杀直接子进程
    /// 的实现会留下孤儿。
    #[cfg(windows)]
    #[tokio::test]
    async fn drop_kills_entire_process_tree_including_grandchildren() {
        mod proc {
            use std::ffi::c_void;
            extern "system" {
                fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> *mut c_void;
                fn Process32FirstW(h: *mut c_void, e: *mut Entry) -> i32;
                fn Process32NextW(h: *mut c_void, e: *mut Entry) -> i32;
                fn CloseHandle(h: *mut c_void) -> i32;
            }
            const TH32CS_SNAPPROCESS: u32 = 0x2;
            #[repr(C)]
            #[derive(Clone, Copy)]
            pub struct Entry {
                pub dw_size: u32,
                pub cnt_usage: u32,
                pub th32_process_id: u32,
                pub th32_default_heap_id: usize,
                pub th32_module_id: u32,
                pub cnt_threads: u32,
                pub th32_parent_process_id: u32,
                pub pc_priority_class: i32,
                pub dw_flags: u32,
                pub sz_exe_file: [u16; 260],
            }
            pub fn child_pids(parent: u32) -> Vec<u32> {
                snapshot_entries()
                    .into_iter()
                    .filter(|e| e.th32_parent_process_id == parent)
                    .map(|e| e.th32_process_id)
                    .collect()
            }
            /// 诊断：pid → (exe 名, 父 pid)。失败输出用。
            pub fn describe(pid: u32) -> String {
                match snapshot_entries().into_iter().find(|e| e.th32_process_id == pid) {
                    Some(e) => {
                        let name = wide_to_string(&e.sz_exe_file);
                        format!("{}(ppid={})", name, e.th32_parent_process_id)
                    }
                    None => "(已不存在)".to_string(),
                }
            }
            fn wide_to_string(buf: &[u16; 260]) -> String {
                let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                String::from_utf16_lossy(&buf[..end])
            }
            fn snapshot_entries() -> Vec<Entry> {
                let mut out = Vec::new();
                unsafe {
                    let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
                    if snap as isize == -1 {
                        return out;
                    }
                    let mut e = Entry {
                        dw_size: std::mem::size_of::<Entry>() as u32,
                        cnt_usage: 0,
                        th32_process_id: 0,
                        th32_default_heap_id: 0,
                        th32_module_id: 0,
                        cnt_threads: 0,
                        th32_parent_process_id: 0,
                        pc_priority_class: 0,
                        dw_flags: 0,
                        sz_exe_file: [0u16; 260],
                    };
                    if Process32FirstW(snap, &mut e) != 0 {
                        loop {
                            out.push(e);
                            if Process32NextW(snap, &mut e) == 0 {
                                break;
                            }
                        }
                    }
                    CloseHandle(snap);
                }
                out
            }
        }

        let args: Vec<String> = ["/c", "ping", "-n", "30", "127.0.0.1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut t = StdioTransport::new("cmd", &args, &HashMap::new());
        t.start().await.expect("spawn ok");
        let root = t.child_pid().expect("child pid");

        // 等 ping（孙进程）出现，凑齐整棵树。
        // 窗口 10s：这是前置条件（cmd→ping 的 spawn 完成），不是被测性质
        // （被测性质是 drop 后整树必死，窗口在下面）。并行测试时本机同时
        // 冷启多个 node/cmd，5s 窗口曾导致间歇性误判（2026-11-14）。
        let mut tree = vec![root];
        for _ in 0..100 {
            let kids = proc::child_pids(root);
            if !kids.is_empty() {
                tree = kids.into_iter().chain(std::iter::once(root)).collect();
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(tree.len() > 1, "grandchild (ping) should have spawned by now");

        drop(t);
        let alive = wait_dead(&tree, Duration::from_secs(8));
        assert!(
            alive.is_empty(),
            "entire process tree must die on drop; alive: {} (tree {tree:?})",
            alive
                .iter()
                .map(|p| format!("{p}={}", proc::describe(*p)))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    // ── 帧格式（2026-11-14 自适应帧修复的回归钉） ──────────────────────

    use super::McpFraming;

    /// 字节回显服务器：`node -e "process.stdin.pipe(process.stdout)"` ——
    /// 字节级精确透传，跨平台一致（`cmd /c type con` 在管道下不回显，
    /// 不可用；sh cat 在 CI unix 上可用，但为行为一致统一走 node）。
    /// 本机无 node 时返回 None，测试跳过。
    fn echo_command() -> Option<(String, Vec<String>)> {
        let has_node = std::process::Command::new("node")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !has_node {
            return None;
        }
        Some((
            "node".to_string(),
            vec!["-e".to_string(), "process.stdin.pipe(process.stdout)".to_string()],
        ))
    }

    /// 两种帧格式各自的单条消息往返：send 写出的字节必须被 receive 原样解回。
    async fn framing_roundtrip(framing: McpFraming) {
        let (cmd, args) =
            echo_command().unwrap_or_else(|| panic!("node required for framing roundtrip"));
        let mut t = StdioTransport::new(cmd, &args, &HashMap::new());
        t.set_framing(framing);
        t.start().await.expect("spawn echo server");
        let msg = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"});
        t.send(msg.clone()).await.expect("send");
        let got = t
            .receive()
            .await
            .unwrap_or_else(|e| panic!("receive failed under {framing:?}: {e}"));
        assert_eq!(got, msg, "roundtrip mismatch under {framing:?}");
        t.close().await.ok();
    }

    #[tokio::test]
    async fn jsonl_framing_roundtrip() {
        framing_roundtrip(McpFraming::Jsonl).await;
    }

    #[tokio::test]
    async fn content_length_framing_roundtrip() {
        framing_roundtrip(McpFraming::ContentLength).await;
    }

    /// 持久化 reader 回归：背靠背发两条消息（回显服务器会把它们连续推回），
    /// 两次 receive 必须各自命中。若 receive() 每次重建 BufReader，第一个
    /// reader 预读进内部缓冲的第二条消息会随其 drop 丢失，第二次 receive 挂死。
    #[tokio::test]
    async fn persistent_reader_survives_burst() {
        let (cmd, args) =
            echo_command().unwrap_or_else(|| panic!("node required for burst test"));
        let mut t = StdioTransport::new(cmd, &args, &HashMap::new());
        t.set_framing(McpFraming::Jsonl);
        t.start().await.expect("spawn echo server");
        t.send(serde_json::json!({"id": 1})).await.expect("send 1");
        t.send(serde_json::json!({"id": 2})).await.expect("send 2");
        let r1 = tokio::time::timeout(Duration::from_secs(5), t.receive())
            .await
            .expect("first message within 5s")
            .expect("parse 1");
        let r2 = tokio::time::timeout(Duration::from_secs(5), t.receive())
            .await
            .expect("second message must not hang (persistent reader)")
            .expect("parse 2");
        assert_eq!(r1["id"], 1);
        assert_eq!(r2["id"], 2);
        t.close().await.ok();
    }
}
