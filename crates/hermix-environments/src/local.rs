//! Local terminal backend – executes commands on the same host.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::process::Command as TokioCommand;

use hermix_core::console_decode::decode_console_output;
use hermix_core::{AgentError, CommandOutput, TerminalBackend};

pub use crate::jobs::{JobRegistry, JobStatus, JobSummary, LogStream};

// `decode_console_output` is re-exported by `hermix-core::console_decode`
// since the Windows-first hardening pass. The local
// backend used to own its own GBK-aware decoder; that's now centralized
// in `hermix-core` so the code-execution backend and Python agent
// glue can share the same logic.

/// Detect common image formats from the first few bytes of a file.
///
/// Returns the format name (e.g. `"JPEG"`) if the magic bytes match a
/// known image type, or `None` if the bytes don't look like an image.
/// Used by `read_file` to give the LLM a helpful error message when it
/// tries to read a binary image file as UTF-8 text (common when the
/// file extension is non-standard, e.g. `.jpgg` instead of `.jpg`).
fn detect_image_magic(header: &[u8]) -> Option<&'static str> {
    if header.len() >= 3 && header[0] == 0xFF && header[1] == 0xD8 && header[2] == 0xFF {
        return Some("JPEG");
    }
    if header.len() >= 8
        && header[0..8] == [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
    {
        return Some("PNG");
    }
    if header.len() >= 6
        && (&header[0..6] == b"GIF87a" || &header[0..6] == b"GIF89a")
    {
        return Some("GIF");
    }
    // WEBP: RIFF....WEBP
    if header.len() >= 12 && &header[0..4] == b"RIFF" && &header[8..12] == b"WEBP" {
        return Some("WEBP");
    }
    // BMP: BM
    if header.len() >= 2 && &header[0..2] == b"BM" {
        return Some("BMP");
    }
    None
}

/// Kill a process and all its descendants.
///
/// hermix launches commands via `cmd.exe /c <cmd>`, so the real
/// long-running process is a *grandchild* of the tokio `Child`.
/// `Child::kill_on_drop(true)` only terminates the direct child
/// (cmd.exe); the grandchild survives as an orphan and, on Windows,
/// keeps the inherited stdout/stderr pipe handles open — which hangs
/// `wait_with_output`'s read tasks indefinitely. `taskkill /T /F`
/// walks the process tree and kills every descendant, releasing the
/// pipes. Best-effort: failures are logged but never propagated.
fn kill_process_tree(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let out = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        match out {
            Ok(o) if o.status.success() => {
                tracing::info!(pid, "killed process tree after command timeout");
            }
            Ok(o) => {
                tracing::warn!(
                    pid,
                    stderr = %String::from_utf8_lossy(&o.stderr),
                    "taskkill /T /F did not succeed while cleaning up timed-out command"
                );
            }
            Err(e) => {
                tracing::warn!(pid, error = %e, "failed to invoke taskkill for timed-out command");
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
    }
}

/// A [`TerminalBackend`] that runs commands on the local machine.
pub struct LocalBackend {
    /// Default timeout in seconds for command execution.
    default_timeout: u64,
    /// Maximum output size in bytes before truncation.
    max_output_size: usize,
    /// Background processes tracked by PID for potential later interaction.
    background_processes: Arc<Mutex<Vec<u32>>>,
    /// Optional workspace root. When set, used as the default working
    /// directory for commands that don't specify their own `workdir`.
    pub workspace_root: Option<PathBuf>,
    /// Optional background-job registry. `None` until the caller
    /// wires one in (via `with_jobs_registry`). Tools like
    /// `jobs_start` etc. require this; the legacy
    /// `background_processes` PID list keeps working without it.
    pub jobs: Option<Arc<JobRegistry>>,
}

impl LocalBackend {
    /// Create a new local backend with the given defaults.
    pub fn new(default_timeout: u64, max_output_size: usize) -> Self {
        Self {
            default_timeout,
            max_output_size,
            background_processes: Arc::new(Mutex::new(Vec::new())),
            workspace_root: None,
            jobs: None,
        }
    }

    /// Set the workspace root for path boundary enforcement.
    pub fn with_workspace_root(mut self, root: PathBuf) -> Self {
        self.workspace_root = Some(root);
        self
    }

    /// Wire a background-job registry. Required for the four
    /// `jobs_*` tool methods below to be usable.
    pub fn with_jobs_registry(mut self, registry: Arc<JobRegistry>) -> Self {
        self.jobs = Some(registry);
        self
    }

    /// Spawn a command as a backgrounded, persistent job. Requires
    /// `with_jobs_registry` to have been called. The returned id is
    /// what `jobs_list` / `jobs_stop` / `jobs_log` accept.
    pub async fn jobs_start(
        &self,
        command: &str,
        cwd: &std::path::Path,
    ) -> Result<String, AgentError> {
        let reg = self.jobs.as_ref().ok_or_else(|| {
            AgentError::Io("jobs registry not configured on this backend".to_string())
        })?;
        reg.clone()
            .start(command, cwd)
            .await
            .map_err(|e: std::io::Error| AgentError::Io(format!("jobs_start: {}", e)))
    }

    pub async fn jobs_list(&self) -> Result<Vec<JobSummary>, AgentError> {
        let reg = self.jobs.as_ref().ok_or_else(|| {
            AgentError::Io("jobs registry not configured on this backend".to_string())
        })?;
        reg.list()
            .await
            .map_err(|e: std::io::Error| AgentError::Io(format!("jobs_list: {}", e)))
    }

    pub async fn jobs_stop(&self, id: &str) -> Result<JobSummary, AgentError> {
        let reg = self.jobs.as_ref().ok_or_else(|| {
            AgentError::Io("jobs registry not configured on this backend".to_string())
        })?;
        reg.stop(id)
            .await
            .map_err(|e: std::io::Error| AgentError::Io(format!("jobs_stop: {}", e)))
    }

    pub fn jobs_log_path(&self, id: &str, stream: LogStream) -> Result<PathBuf, AgentError> {
        let reg = self.jobs.as_ref().ok_or_else(|| {
            AgentError::Io("jobs registry not configured on this backend".to_string())
        })?;
        reg.log_path(id, stream)
            .map_err(|e| AgentError::Io(format!("jobs_log: {}", e)))
    }
}

impl Default for LocalBackend {
    fn default() -> Self {
        // 60s 对齐 tool_executor 的 per-call 超时, 避免双层 timeout 不一致;
        // 外部 API 慢时 60s 足够判定失败, 不必让用户等满 120s
        Self::new(60, 1_048_576)
    }
}

/// Split a leading `powershell`/`powershell.exe`/`pwsh`/`pwsh.exe`
/// token (case-insensitive, whitespace-delimited) off a Windows shell
/// command. Returns the concrete shell executable plus the remainder;
/// `None` when the command does not name a PowerShell shell and the
/// caller defaults to `powershell.exe`. The `.exe` variants are
/// matched explicitly — slicing by the bare name would leave a
/// `.exe` residue（真机探针钉死）.
#[cfg(windows)]
fn split_windows_shell_prefix(command: &str) -> Option<(&'static str, &str)> {
    let lower = command.to_lowercase();
    for (prefix, exe) in [
        ("powershell.exe ", "powershell.exe"),
        ("powershell ", "powershell.exe"),
        ("pwsh.exe ", "pwsh.exe"),
        ("pwsh ", "pwsh.exe"),
    ] {
        if lower.starts_with(prefix) {
            return Some((exe, command[prefix.len()..].trim_start()));
        }
    }
    None
}

/// Normalise Windows path separators in a command string:
/// forward slashes in **path-like** tokens become backslashes.
///
/// Rationale (deepseek-harness 对照): LLMs frequently emit paths in
/// POSIX form (`C:/x/y.py`, `~/notes.txt`, `server/dist/index.js`)
/// out of habit. cmd.exe accepts forward slashes, but downstream
/// tools are inconsistent (`python` accepts `C:/`, `FIND` / batch
/// labels / some CLI args do not). The harness pins everything to
/// the native Windows form before spawning; we do the same for the
/// cmd route (the only string-joined spawn path — PowerShell and
/// native-dispatch routes already pass argv, so they never mangle
/// separators).
///
/// Only separators in **path context** are flipped:
/// - `Drive:\` forms (`C:/`, `c:/`)
/// - tilde home (`~/`)
/// - a `/` that is *not* the start of a cmd switch (`/c`, `/d`,
///   `/I` …) and is not part of a URL scheme (`https://`)
///
/// A `/` immediately followed by more letters with no intervening
/// whitespace (e.g. `findstr /I hermix`, `dir /b /s`) is a **cmd
/// switch**, not a path separator — never flipped. A `/` between
/// two word-like segments (`server/dist`) *is* a path separator.
fn normalize_windows_path_slashes(command: &str) -> String {
    let chars: Vec<char> = command.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(n);

    // Pre-pass: mark the start index of each whitespace-delimited
    // token, and whether that token is a URL (contains `://` anywhere
    // in it). URLs are never Windows paths — a token like
    // `https://example.com/x` must NOT have its `com/x` slash
    // flipped even though it looks identical to a relative path.
    let mut token_url_start = std::collections::BTreeMap::new();
    let mut i = 0;
    while i < n {
        while i < n && chars[i].is_whitespace() {
            i += 1;
        }
        let token_start = i;
        while i < n && !chars[i].is_whitespace() {
            i += 1;
        }
        if i > token_start {
            let token: String = chars[token_start..i].iter().collect();
            if token.contains("://") {
                token_url_start.insert(token_start, true);
            }
        }
    }

    for i in 0..n {
        if chars[i] != '/' {
            out.push(chars[i]);
            continue;
        }
        let prev = chars.get(i - 1).copied();
        let next = chars.get(i + 1).copied();

        // Inside a URL token — never flip (a web URL in a shell
        // command is not a Windows path).
        let mut j = i;
        while j > 0 && !chars[j - 1].is_whitespace() {
            j -= 1;
        }
        if token_url_start.contains_key(&j) {
            out.push('/');
            continue;
        }

        // ── cmd switch slash: `/letter…` — NEVER flipped.
        // `findstr /I`, `dir /b /s`, `cmd /c`.
        let is_switch = next.map_or(false, |n| n.is_ascii_alphabetic())
            && prev.map_or(true, |p| {
                p.is_whitespace() || p == '"' || p == '\'' || p == '(' || p == ')'
            });

        // ── URL scheme `://` — two consecutive slashes after a
        // scheme letter. Handled by the token-level check above;
        // kept here as a second line of defense for a slash that
        // falls between two word tokens (rare in practice).
        let is_scheme = next == Some('/')
            && prev.map_or(false, |p| p.is_ascii_alphabetic());

        // ── Drive-letter sep: `X:/` where X is a drive letter (the
        // char right before `:` and before this slash is a lone letter
        // at a token start). `C:/x/y.py` → flip the drive slash AND
        // every following segment slash.
        let is_drive_sep = prev.map_or(false, |p| p == ':')
            && chars
                .get(i - 2)
                .map_or(false, |p2| p2.is_ascii_alphabetic())
            && next.is_some();

        // ── Tilde home: `~/` — prev is `~`.
        let is_tilde = prev == Some('~');

        // ── Relative path sep: `a/b` where prev is a path-ish char
        // (alphanumeric / dot / backslash) AND this is NOT a switch
        // or scheme. `server/dist`, `a/b/c.txt`.
        let is_path_sep = !is_switch
            && !is_scheme
            && prev.map_or(false, |p| {
                p.is_ascii_alphanumeric() || p == '.' || p == '\\'
            });

        let flip = is_drive_sep || is_tilde || is_path_sep;
        out.push(if flip { '\\' } else { '/' });
    }
    out
}

#[async_trait]
impl TerminalBackend for LocalBackend {
    async fn execute_command(
        &self,
        command: &str,
        timeout: Option<u64>,
        workdir: Option<&str>,
        background: bool,
        pty: bool,
    ) -> Result<CommandOutput, AgentError> {
        let timeout_secs = timeout.unwrap_or(self.default_timeout);

        if pty {
            // PTY mode: allocate a pseudo-terminal for interactive commands.
            // Uses `script` command (available on macOS/Linux) to wrap the
            // command in a PTY, which makes programs behave as if connected
            // to a real terminal (enables colors, line editing, etc.).
            #[cfg(unix)]
            {
                let mut pty_cmd = hermix_core::proc::cmd_tokio("script");
                pty_cmd
                    .arg("-q") // quiet mode
                    .arg("/dev/null") // discard typescript file
                    .arg("-c") // command to execute
                    .arg(command)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .stdin(Stdio::null());

                // Use explicit workdir, or fall back to workspace_root.
                // Strip \\?\ prefix on Windows — many programs (cmd.exe, FIND, etc.)
                // can't handle the Win32 device namespace path format.
                let effective_dir = workdir
                    .map(std::path::PathBuf::from)
                    .or_else(|| self.workspace_root.clone())
                    .map(|p| hermix_core::path_security::strip_win32_long_path_buf(&p));
                if let Some(dir) = effective_dir {
                    pty_cmd.current_dir(dir);
                }

                let result =
                    tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
                        let output = pty_cmd.output().await.map_err(|e| {
                            AgentError::Io(format!("Failed to spawn PTY command: {}", e))
                        })?;
                        let stdout = decode_console_output(&output.stdout).into_owned();
                        let stderr = decode_console_output(&output.stderr).into_owned();
                        Ok(CommandOutput {
                            exit_code: output.status.code().unwrap_or(-1),
                            stdout: if stdout.len() > self.max_output_size {
                                stdout[..self.max_output_size].to_string()
                            } else {
                                stdout
                            },
                            stderr: if stderr.len() > self.max_output_size {
                                stderr[..self.max_output_size].to_string()
                            } else {
                                stderr
                            },
                        })
                    })
                    .await;

                return match result {
                    Ok(Ok(output)) => Ok(output),
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err(AgentError::Timeout(format!(
                        "PTY command timed out after {} seconds",
                        timeout_secs
                    ))),
                };
            }

            #[cfg(not(unix))]
            {
                // Windows: PTY is not implemented. Surface this as
                // a hard `AgentError::Io` so the caller (the LLM,
                // via `TerminalHandler`) gets a clear "this is not
                // supported on Windows" message in its next turn,
                // rather than silently re-running through plain
                // pipes and producing subtly different output
                // (no color codes, no line-editing, etc.).
                // The dispatch path then continues with the
                // non-PTY branch below.
                return Err(AgentError::Io(
                    "PTY mode is not supported on Windows. \
                     Re-run the command with `pty: false` to use \
                     standard pipe-based execution. (Windows \
                     pseudo-console support via ConPTY is a future \
                     enhancement.)"
                        .to_string(),
                ));
            }
        }

        #[cfg(windows)]
        {
            // Windows ACL 沙箱路径（对齐 deepseek-harness fail-closed 语义）：
            // HERMIX_SANDBOX_MODE 非 danger-full-access 且有工作目录时，
            // 命令进受限 token spawn。provision/spawn 任一步失败 = 报错，
            // 绝不静默回退到不受限 spawn。danger-full-access 与无
            // workdir（全局探测类命令无边界可守）走下方常规路径，安全
            // 语义由审批层承接。
            use crate::sandbox::SandboxMode;
            let mode = SandboxMode::from_env();
            if !background && mode != SandboxMode::DangerFullAccess {
                if let Some(dir) = workdir
                    .map(std::path::PathBuf::from)
                    .or_else(|| self.workspace_root.clone())
                    .map(|p| hermix_core::path_security::strip_win32_long_path_buf(&p))
                {
                    return execute_command_sandboxed(
                        command,
                        timeout_secs,
                        &dir,
                        mode,
                        self.max_output_size,
                    )
                    .await;
                }
            }
        }

        #[cfg(unix)]
        let mut cmd = {
            let mut c = TokioCommand::new("sh");
            c.arg("-c").arg(command);
            c
        };
        #[cfg(windows)]
        let mut cmd = {
            let trimmed = command.trim_start();
            // Windows 统一 shell 层：PowerShell（对齐 pi 的 Windows 模型 —
            // 单一 shell 路由、argv 直传、不靠 cmd.exe 字符串拼接）。
            // PowerShell 自带 `ls`/`cat` 等 POSIX 别名，LLM 发 POSIX 名
            // 也 work。cmd.exe 不再作为默认路由（无 PowerShell 的
            // Windows 系统不存在，powershell.exe 系统自带）。
            //
            // `powershell`/`pwsh` 前缀的命令吃掉前缀走同一路由
            // （`.exe` 形态必须先判——否则按 "powershell" 长度切片
            // 残留 `.exe`）。剥前缀后还要吃掉 LLM 按旧习惯带的旗标
            // （`-Command X` / `-EncodedCommand b64` / `-File x.ps1`）
            // ——真机探针钉死：旗标残留在脚本文本里会被当命令名报
            // "无法将 -Command 项识别为 cmdlet"。
            let (shell_exe, bare) = match split_windows_shell_prefix(trimmed) {
                Some((exe, rest)) => (exe, rest),
                None => ("powershell.exe", trimmed),
            };
            let bare_lower = bare.to_lowercase();

            let mut c = TokioCommand::new(shell_exe);
            c.arg("-NoProfile").arg("-NonInteractive");
            if bare_lower.starts_with("-encodedcommand ") {
                // base64 token 可能含 `/`，斜杠归一化会破坏编码——
                // 作为真旗标直传，不经归一化。审批引擎 sees 原始命令
                // 串，`powershell-encoded` CONFIRM 规则照常命中。
                c.arg("-EncodedCommand")
                    .arg(bare["-encodedcommand ".len()..].trim_start());
            } else if bare_lower.starts_with("-file ") {
                let target = bare["-file ".len()..].trim_start();
                c.arg("-File").arg(normalize_windows_path_slashes(target));
            } else {
                // `-Command X` 的 X 才是脚本文本；裸命令整段进 -Command。
                let script = if bare_lower.starts_with("-command ") {
                    bare["-command ".len()..].trim_start()
                } else {
                    bare
                };
                let normalized = normalize_windows_path_slashes(script);
                c.arg("-Command").arg(normalized);
            }
            c
        };
        // Hide the console window on Windows — without CREATE_NO_WINDOW,
        // every shell tool call (dir /b, etc.) flashes a black cmd window
        // on the user's desktop. Same pattern as cdp_bootstrap.rs.
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        // Capture the spawned PID *outside* the timeout future so that
        // on timeout we can kill the entire process tree (see
        // `kill_process_tree`). Background mode returns immediately and
        // never reaches the timeout arm, so its process is left running
        // as intended.
        let pid_cell: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
        let pid_for_block = pid_cell.clone();

        // Use explicit workdir, or fall back to workspace_root.
        // Strip \\?\ prefix on Windows — many programs (cmd.exe, FIND, etc.)
        // can't handle the Win32 device namespace path format. The
        // helper lives in `hermix-core::path_security` so the same
        // normalization is applied consistently across backends.
        let effective_dir: Option<PathBuf> = workdir
            .map(std::path::PathBuf::from)
            .or_else(|| self.workspace_root.clone())
            .map(|p| hermix_core::path_security::strip_win32_long_path_buf(&p));
        if let Some(ref dir) = effective_dir {
            cmd.current_dir(dir);
        }

        if background {
            // In background mode, detach the process and return immediately.
            cmd.stdin(Stdio::null());
        }

        let result = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async move {
            let child = cmd
                .spawn()
                .map_err(|e| AgentError::Io(format!("Failed to spawn command: {}", e)))?;

            // Record the PID for the outer timeout handler (tree-kill
            // on timeout). Done for every mode; background mode also
            // pushes it to `background_processes` below.
            if let Some(id) = child.id() {
                *pid_for_block.lock().unwrap() = Some(id);
            }

            // In background mode, track the PID and return immediately.
            if background {
                if let Some(id) = child.id() {
                    self.background_processes
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(id);
                }
                return Ok(CommandOutput {
                    exit_code: 0,
                    stdout: format!(
                        "Process started in background (pid: {})",
                        child.id().unwrap_or(0)
                    ),
                    stderr: String::new(),
                });
            }

            let output = child
                .wait_with_output()
                .await
                .map_err(|e| AgentError::Io(format!("Failed to wait for command: {}", e)))?;

            // Decode console output — handle Windows OEM code pages (GBK).
            let stdout = decode_console_output(&output.stdout).into_owned();
            let stderr = decode_console_output(&output.stderr).into_owned();

            // Truncate output if it exceeds max_output_size
            let stdout = if stdout.len() > self.max_output_size {
                tracing::warn!(
                    "Command output exceeded max size ({} bytes), truncating",
                    stdout.len()
                );
                stdout[..self.max_output_size].to_string()
            } else {
                stdout
            };

            let stderr = if stderr.len() > self.max_output_size {
                tracing::warn!(
                    "Command stderr exceeded max size ({} bytes), truncating",
                    stderr.len()
                );
                stderr[..self.max_output_size].to_string()
            } else {
                stderr
            };

            Ok(CommandOutput {
                exit_code: output.status.code().unwrap_or(-1),
                stdout,
                stderr,
            })
        })
        .await;

        match result {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                // Timeout: kill the whole process tree so the
                // grandchild under `cmd.exe /c` doesn't outlive the
                // future and hold the stdout/stderr pipes open
                // (which would hang the runtime's read tasks).
                if let Some(pid) = *pid_cell.lock().unwrap() {
                    kill_process_tree(pid);
                }
                Err(AgentError::Timeout(format!(
                    "Command timed out after {} seconds",
                    timeout_secs
                )))
            }
        }
    }

    async fn read_file(
        &self,
        path: &str,
        offset: Option<u64>,
        limit: Option<u64>,
    ) -> Result<String, AgentError> {
        let content = match tokio::fs::read_to_string(path).await {
            Ok(content) => content,
            Err(e) => {
                // If the error is about UTF-8 validity, the file is likely
                // binary (e.g. a JPEG/PNG with a non-standard extension like
                // .jpgg). Read the first few bytes to check for known image
                // magic numbers and suggest `read_image` — the LLM frequently
                // picks read_file for image files when the extension doesn't
                // match the read_image tool's documented format list.
                let err_str = e.to_string();
                if err_str.contains("valid UTF-8")
                    || err_str.contains("invalid")
                    || err_str.contains("non-UTF")
                {
                    if let Ok(header) = tokio::fs::read(path).await {
                        if let Some(format_name) = detect_image_magic(&header) {
                            return Err(AgentError::Io(format!(
                                "Failed to read file '{}': the file is a binary {} image, \
                 not valid UTF-8 text. Use the `read_image` tool instead of `read_file` \
                 to read this file — read_image handles binary data correctly.",
                path,
                format_name
                            )));
                        }
                        return Err(AgentError::Io(format!(
                            "Failed to read file '{}': the file is binary (not valid UTF-8). \
                 If this is an image, use the `read_image` tool instead. \
                 Original error: {}",
                            path, e
                        )));
                    }
                }
                return Err(AgentError::Io(format!(
                    "Failed to read file '{}': {}",
                    path, e
                )));
            }
        };

        let lines: Vec<&str> = content.lines().collect();

        // Apply offset (0-indexed line number to start from)
        let start = offset.unwrap_or(0) as usize;
        let start = start.min(lines.len());

        // Apply limit (max number of lines to return)
        let end = if let Some(lim) = limit {
            (start + lim as usize).min(lines.len())
        } else {
            lines.len()
        };

        let selected_lines = &lines[start..end];
        Ok(selected_lines.join("\n"))
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<(), AgentError> {
        // Ensure parent directory exists
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await.map_err(|e| {
                    AgentError::Io(format!(
                        "Failed to create parent directory for '{}': {}",
                        path, e
                    ))
                })?;
            }
        }

        tokio::fs::write(path, content)
            .await
            .map_err(|e| AgentError::Io(format!("Failed to write file '{}': {}", path, e)))?;

        Ok(())
    }

    async fn file_exists(&self, path: &str) -> Result<bool, AgentError> {
        match tokio::fs::metadata(path).await {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(AgentError::Io(format!(
                "Failed to check file existence '{}': {}",
                path, e
            ))),
        }
    }
}

/// Windows ACL 沙箱执行路径：受限 token spawn + 超时强杀。同步 FFI
/// 走 `spawn_blocking`；stdout/stderr 并行读（先 wait 再读会因管道
/// 写满死锁）。fail-closed：provision/spawn 失败直接报错。
#[cfg(windows)]
async fn execute_command_sandboxed(
    command: &str,
    timeout_secs: u64,
    workdir: &std::path::Path,
    mode: crate::sandbox::SandboxMode,
    max_output_size: usize,
) -> Result<CommandOutput, AgentError> {
    use std::io::Read;

    let ws = workdir.to_path_buf();
    let wd = workdir.to_path_buf();
    let command = command.to_string();
    let blocking = tokio::task::spawn_blocking(move || -> Result<CommandOutput, AgentError> {
        let session = crate::sandbox::SandboxSession::provision(mode, &ws)?;
        // 统一 shell 层同款路由：整段命令进 PowerShell -Command。
        let mut child = session.spawn(
            "powershell.exe",
            &[
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-Command".to_string(),
                command,
            ],
            &wd,
        )?;
        child.spawn_kill_watchdog(timeout_secs);
        let (mut so, mut se) = child.try_clone_pipes()?;

        let out_thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = so.read_to_end(&mut buf);
            buf
        });
        let err_thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = se.read_to_end(&mut buf);
            buf
        });
        let exit_code = child.wait()?;
        let stdout = out_thread.join().unwrap_or_default();
        let stderr = err_thread.join().unwrap_or_default();

        let stdout = decode_console_output(&stdout).into_owned();
        let stderr = decode_console_output(&stderr).into_owned();
        Ok(CommandOutput {
            exit_code: exit_code as i32,
            stdout: if stdout.len() > max_output_size { stdout[..max_output_size].to_string() } else { stdout },
            stderr: if stderr.len() > max_output_size { stderr[..max_output_size].to_string() } else { stderr },
        })
    });
    match blocking.await {
        Ok(r) => r,
        Err(e) => Err(AgentError::Io(format!("sandbox join failed: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 沙箱路由测试共用锁：HERMIX_SANDBOX_MODE 是进程级 env，
    /// 并行测试互踩会导致路由分支漂移。
    #[cfg(windows)]
    static SANDBOX_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[cfg(windows)]
    fn set_sandbox_env(mode: &str) -> std::sync::MutexGuard<'static, ()> {
        let guard = SANDBOX_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("HERMIX_SANDBOX_MODE", mode);
        guard
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn sandbox_route_rejects_drive_root_fail_closed() {
        let _g = set_sandbox_env("workspace-write");
        let backend = LocalBackend::default();
        let result = backend
            .execute_command("echo hi", None, Some("C:\\"), false, false)
            .await;
        let err = result.err().expect("drive-root workdir must fail closed");
        assert!(
            err.to_string().contains("drive root"),
            "unexpected error: {err}"
        );
        std::env::remove_var("HERMIX_SANDBOX_MODE");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn sandbox_route_rejects_workspace_overlapping_temp() {
        let _g = set_sandbox_env("workspace-write");
        // workspace = 系统 temp 根：provision 的私有 temp 建在其下，
        // overlap 检查必须拒绝（否则沙箱 temp 对"工作区"自身可写）。
        let err = crate::sandbox::SandboxSession::provision(
            crate::sandbox::SandboxMode::WorkspaceWrite,
            &std::env::temp_dir(),
        )
        .err()
        .expect("temp-root workspace must fail closed");
        assert!(
            err.to_string().contains("overlaps"),
            "unexpected error: {err}"
        );
        std::env::remove_var("HERMIX_SANDBOX_MODE");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn sandbox_route_danger_full_access_bypasses_sandbox() {
        let _g = set_sandbox_env("danger-full-access");
        let tmp = tempfile::tempdir().unwrap();
        let backend = LocalBackend::default();
        let output = backend
            .execute_command(
                "echo hi",
                None,
                Some(tmp.path().to_str().unwrap()),
                false,
                false,
            )
            .await
            .expect("danger-full-access must take the regular path");
        assert_eq!(output.exit_code, 0);
        std::env::remove_var("HERMIX_SANDBOX_MODE");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn sandbox_route_runs_command_in_restricted_session() {
        let _g = set_sandbox_env("workspace-write");
        let tmp = tempfile::tempdir().unwrap();
        let backend = LocalBackend::default();
        let output = backend
            .execute_command(
                "echo hi",
                None,
                Some(tmp.path().to_str().unwrap()),
                false,
                false,
            )
            .await
            .expect("workspace-write sandbox must run the command end-to-end");
        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.trim().contains("hi"));
        std::env::remove_var("HERMIX_SANDBOX_MODE");
    }

    #[tokio::test]
    async fn test_execute_command_echo() {
        let backend = LocalBackend::default();
        let output = backend
            .execute_command("echo hello", None, None, false, false)
            .await
            .unwrap();
        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.trim().contains("hello"));
    }

    #[tokio::test]
    async fn test_execute_command_with_workdir() {
        // Use a real existing directory on the host instead of the
        // Unix-only `/tmp` literal that this test used to hardcode
        // (it silently passed on Linux but always failed on
        // Windows). The behavior under test is "the workdir param
        // is honored" — any existing dir is fine.
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path().to_str().unwrap();
        let backend = LocalBackend::default();
        let output = backend
            .execute_command(
                if cfg!(windows) { "cd" } else { "pwd" },
                None,
                Some(workdir),
                false,
                false,
            )
            .await
            .unwrap();
        assert_eq!(output.exit_code, 0);
        // On Windows `cd` doesn't print the path, so just assert
        // the command succeeded without checking the path output.
        if !cfg!(windows) {
            assert!(output.stdout.trim().contains(workdir.trim_end_matches('/')));
        }
    }

    #[tokio::test]
    async fn test_execute_command_timeout() {
        let backend = LocalBackend::new(1, 1_048_576);
        // Platform-appropriate long-running command. PowerShell has a
        // built-in `Start-Sleep` so no external binary is needed.
        let cmd = if cfg!(windows) {
            "Start-Sleep -Seconds 30"
        } else {
            "sleep 30"
        };
        let result = backend
            .execute_command(cmd, None, None, false, false)
            .await;
        assert!(result.is_err());
        match result {
            Err(AgentError::Timeout(_)) => {}
            _ => panic!("Expected timeout error"),
        }
    }

    #[tokio::test]
    async fn test_execute_command_failure() {
        let backend = LocalBackend::default();
        let output = backend
            .execute_command("exit 42", None, None, false, false)
            .await
            .unwrap();
        assert_eq!(output.exit_code, 42);
    }

    // ── Windows 统一 PowerShell 路由冒烟（真机语义钉死） ─────────
    // 探针结论：POSIX 别名（ls/cat）可用；未知命令 exit=1 且
    // stderr 非空；显式 exit code 原样传播（exit 42 由上面既有
    // 测试覆盖）。

    #[tokio::test]
    #[cfg(windows)]
    async fn powershell_route_posix_alias_lists_directory() {
        let dir = std::env::temp_dir().join("hermix_ps_route_alias");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"hello").unwrap();
        let backend = LocalBackend::default();
        let out = backend
            .execute_command(
                "ls a.txt",
                None,
                Some(dir.to_string_lossy().as_ref()),
                false,
                false,
            )
            .await
            .unwrap();
        assert_eq!(out.exit_code, 0);
        assert!(out.stdout.contains("a.txt"), "stdout: {}", out.stdout);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    #[cfg(windows)]
    async fn powershell_route_unknown_command_reports_nonzero_exit() {
        // 诚实失败语义：未知命令 exit != 0 且 stderr 带诊断（错误
        // 文案随系统语言变化，只断言非空）。
        let backend = LocalBackend::default();
        let out = backend
            .execute_command("definitely_not_a_command_xyz_9x7", None, None, false, false)
            .await
            .unwrap();
        assert_ne!(out.exit_code, 0);
        assert!(!out.stderr.is_empty());
    }

    #[tokio::test]
    #[cfg(windows)]
    async fn powershell_route_native_failure_propagates_nonzero_exit() {
        // native 子进程失败必须让 LLM 看到非零 exit code——否则
        // 构建失败会被当成成功。真机探针钉死：native 失败码经
        // PowerShell 折叠为 1（成功仍为 0），"成败判定"语义成立；
        // 精确失败码不保证透传（PS 级 `exit 42` 语句由上面既有
        // 测试覆盖）。`cmd /c exit 42` 本身不产生输出，钉"失败
        // 非零"这一条语义。
        let backend = LocalBackend::default();
        let out = backend
            .execute_command("cmd /c exit 42", None, None, false, false)
            .await
            .unwrap();
        assert_ne!(out.exit_code, 0, "native failure must not look like success");
    }

    #[test]
    #[cfg(windows)]
    fn windows_shell_prefix_split_variants() {
        assert_eq!(
            split_windows_shell_prefix("powershell -Command Get-Date"),
            Some(("powershell.exe", "-Command Get-Date"))
        );
        // `.exe` 形态必须显式匹配——按裸名长度切片会残留 `.exe`。
        assert_eq!(
            split_windows_shell_prefix("PowerShell.exe Get-Date"),
            Some(("powershell.exe", "Get-Date"))
        );
        assert_eq!(
            split_windows_shell_prefix("pwsh -File x.ps1"),
            Some(("pwsh.exe", "-File x.ps1"))
        );
        assert_eq!(split_windows_shell_prefix("Get-Date"), None);
        // `powershellfoo` 不是 shell 前缀。
        assert_eq!(split_windows_shell_prefix("powershellfoo -x"), None);
    }

    #[tokio::test]
    #[cfg(windows)]
    async fn powershell_route_strips_legacy_command_flag_prefix() {
        // LLM 按旧习惯发 `powershell -Command X`：剥前缀 + 剥旗标后
        // X 作为脚本文本执行，而不是把 "-Command" 当命令名报错
        // （真机探针钉死的回归）。
        let backend = LocalBackend::default();
        let out = backend
            .execute_command(
                "powershell -Command Write-Output hermix_flag_ok",
                None,
                None,
                false,
                false,
            )
            .await
            .unwrap();
        assert_eq!(out.exit_code, 0);
        assert!(
            out.stdout.contains("hermix_flag_ok"),
            "stdout: {}",
            out.stdout
        );
    }

    #[tokio::test]
    #[cfg(windows)]
    async fn powershell_route_exe_prefix_leaves_no_residue() {
        let backend = LocalBackend::default();
        let out = backend
            .execute_command(
                "powershell.exe Write-Output hermix_exe_ok",
                None,
                None,
                false,
                false,
            )
            .await
            .unwrap();
        assert_eq!(out.exit_code, 0);
        assert!(
            out.stdout.contains("hermix_exe_ok"),
            "stdout: {}",
            out.stdout
        );
    }

    #[tokio::test]
    #[cfg(windows)]
    async fn powershell_route_encoded_command_runs_as_flag() {
        // `-EncodedCommand` 是真旗标：base64 token 直传，不经斜杠
        // 归一化（归一化会破坏编码）。审批引擎看原始命令串，
        // `powershell-encoded` CONFIRM 规则照常命中。
        let backend = LocalBackend::default();
        let out = backend
            .execute_command(
                "powershell -EncodedCommand VwByAGkAdABlAC0ATwB1AHQAcAB1AHQAIABoAGUAcgBtAGkAeABfAGIANgA0AF8AbwBrAA==",
                None,
                None,
                false,
                false,
            )
            .await
            .unwrap();
        assert_eq!(out.exit_code, 0);
        let combined = format!("{}{}", out.stdout, out.stderr);
        assert!(
            combined.contains("hermix_b64_ok"),
            "stdout: {} stderr: {}",
            out.stdout,
            out.stderr
        );
    }

    #[tokio::test]
    async fn test_write_and_read_file() {
        let dir = std::env::temp_dir().join("hermes_test_write_read");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_file.txt");
        let path_str = path.to_string_lossy().to_string();

        let backend = LocalBackend::default();

        backend
            .write_file(&path_str, "hello\nworld\nfoo\nbar")
            .await
            .unwrap();
        let content = backend.read_file(&path_str, None, None).await.unwrap();
        assert_eq!(content, "hello\nworld\nfoo\nbar");

        // Test with offset
        let content = backend.read_file(&path_str, Some(1), None).await.unwrap();
        assert_eq!(content, "world\nfoo\nbar");

        // Test with offset and limit
        let content = backend
            .read_file(&path_str, Some(1), Some(2))
            .await
            .unwrap();
        assert_eq!(content, "world\nfoo");

        // Cleanup
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[tokio::test]
    async fn test_file_exists() {
        let backend = LocalBackend::default();

        // `std::env::temp_dir()` instead of a hardcoded "/tmp" — the
        // project is Windows-only, and "/tmp" does not exist there, so
        // this test failed on every run in CI.
        let tmp = std::env::temp_dir();
        let tmp_str = tmp.to_string_lossy().to_string();

        // A path that should exist
        assert!(backend.file_exists(&tmp_str).await.unwrap());

        // A path that should not exist
        assert!(!backend
            .file_exists(&tmp.join("hermes_nonexistent_test_file_xyz").to_string_lossy())
            .await
            .unwrap());
    }

    #[test]
    fn test_detect_image_magic_jpeg() {
        let header = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        assert_eq!(detect_image_magic(&header), Some("JPEG"));
    }

    #[test]
    fn test_detect_image_magic_png() {
        let header = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        assert_eq!(detect_image_magic(&header), Some("PNG"));
    }

    #[test]
    fn test_detect_image_magic_not_image() {
        let header = b"Hello, world! This is a text file.";
        assert_eq!(detect_image_magic(header), None);
    }

    // ── normalize_windows_path_slashes (deepseek-harness 对照) ──────
    // 斜杠归一化只作用于 cmd 路由的字符串拼接面。钉死语义：
    // 路径斜杠翻 backslash，cmd 开关斜杠不动。

    #[test]
    fn slash_norm_drive_path_flipped() {
        assert_eq!(
            normalize_windows_path_slashes("python C:/x/y.py"),
            "python C:\\x\\y.py"
        );
        // 小写 drive 也认
        assert_eq!(
            normalize_windows_path_slashes("node c:/proj/dist/index.js"),
            "node c:\\proj\\dist\\index.js"
        );
    }

    #[test]
    fn slash_norm_tilde_home_flipped() {
        assert_eq!(
            normalize_windows_path_slashes("cat ~/notes.txt"),
            "cat ~\\notes.txt"
        );
    }

    #[test]
    fn slash_norm_relative_path_flipped() {
        // 两段 word 之间的斜杠 = 路径分隔
        assert_eq!(
            normalize_windows_path_slashes("node server/dist/index.js"),
            "node server\\dist\\index.js"
        );
        // 带 . 的路径（dist/index.js）
        assert_eq!(
            normalize_windows_path_slashes("echo a/b/c.txt"),
            "echo a\\b\\c.txt"
        );
    }

    #[test]
    fn slash_norm_cmd_switch_not_flipped() {
        // 开关斜杠（斜杠紧跟字母、且斜杠前是空格/行首/引号）不动
        assert_eq!(
            normalize_windows_path_slashes("findstr /I hermix"),
            "findstr /I hermix"
        );
        assert_eq!(
            normalize_windows_path_slashes("dir /b /s /a"),
            "dir /b /s /a"
        );
        // /c 开关在 normalize_windows_cmd_prefix 之后仍保持斜杠
        assert_eq!(
            normalize_windows_path_slashes("cmd /c \"dir\""),
            "cmd /c \"dir\""
        );
    }

    #[test]
    fn slash_norm_mixed_path_and_switch() {
        // 同一命令里路径斜杠翻、开关斜杠留
        assert_eq!(
            normalize_windows_path_slashes("findstr /I text C:/logs/app.log"),
            "findstr /I text C:\\logs\\app.log"
        );
    }

    #[test]
    fn slash_norm_url_scheme_preserved() {
        // URL 的 `//`（scheme 后）不是路径分隔——前一个字符是
        // `:`，不在 path-ish 集合里，保持不动。
        assert_eq!(
            normalize_windows_path_slashes("start https://example.com/x"),
            "start https://example.com/x"
        );
    }

    #[test]
    fn slash_norm_pipe_and_semicolon_unaffected() {
        // shell 元字符不是斜杠，原样保留；相邻路径斜杠照翻
        assert_eq!(
            normalize_windows_path_slashes("dir C:/a; dir C:/b"),
            "dir C:\\a; dir C:\\b"
        );
    }

    #[test]
    fn slash_norm_idempotent_on_backslash() {
        // 已经是 backslash 的命令不被二次改写（无 / 可翻）
        assert_eq!(
            normalize_windows_path_slashes("dir C:\\Users\\x"),
            "dir C:\\Users\\x"
        );
    }

    #[tokio::test]
    async fn test_read_file_binary_image_suggests_read_image() {
        let dir = std::env::temp_dir().join("hermes_test_binary_read");
        let _ = std::fs::create_dir_all(&dir);
        // Create a fake JPEG file (magic bytes + some garbage).
        // The .jpgg extension is intentionally non-standard to
        // reproduce Bug ①: the LLM picked read_file because the
        // extension didn't match read_image's documented format list.
        let path = dir.join("test_image.jpgg");
        let path_str = path.to_string_lossy().to_string();
        std::fs::write(&path, [0xFFu8, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46])
            .unwrap();

        let backend = LocalBackend::default();
        let result = backend.read_file(&path_str, None, None).await;

        assert!(result.is_err(), "read_file should fail on binary JPEG");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("binary JPEG image"),
            "error should mention JPEG: got: {}",
            err_msg
        );
        assert!(
            err_msg.contains("read_image"),
            "error should suggest read_image: got: {}",
            err_msg
        );

        let _ = std::fs::remove_file(&path);
    }
}
