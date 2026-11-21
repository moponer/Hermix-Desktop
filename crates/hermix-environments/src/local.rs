//! Local terminal backend – executes commands on the same host.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::process::Command as TokioCommand;

use hermix_core::console_decode::decode_console_output;
use hermix_core::{AgentError, CommandOutput, PtySessionInfo, PtySessionOutput, TerminalBackend};

pub use crate::jobs::{JobRegistry, JobStatus, JobSummary, LogStream};
pub use crate::pty_session::PtySessionRegistry;

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
    /// 持久终端会话表（ConPTY，H3）。挂在本 backend 上，经
    /// `TerminalBackend` 的 `pty_*` 会话方法暴露；一次性 `pty: true`
    /// 执行也复用它的 ConPTY 通道。
    pub pty_sessions: Arc<PtySessionRegistry>,
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
            pty_sessions: Arc::new(PtySessionRegistry::new()),
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
            // A literal URL (`https://…`) is obviously not a path. So is any
            // token containing a PowerShell interpolation (`"$base/"`): the
            // value is only known at runtime, long after this rewrite runs,
            // so the token *looks* like a relative path but resolves to
            // whatever the variable holds. Without this, `$base='https://x'`
            // followed by `Invoke-WebRequest -Uri "$base/"` shipped
            // `https://x\` and 400'd. The previous comment claimed the
            // PowerShell route "never mangles separators" — that was true of
            // argv passing but not of this normalization step, which runs on
            // the whole script.
            if token.contains("://") || token.contains('$') {
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

/// Windows 统一 shell 路由：PowerShell（对齐 pi 的 Windows 模型 —
/// 单一 shell 路由、argv 直传、不靠 cmd.exe 字符串拼接）。
/// PowerShell 自带 `ls`/`cat` 等 POSIX 别名，LLM 发 POSIX 名
/// 也 work。cmd.exe 不再作为默认路由（无 PowerShell 的
/// Windows 系统不存在，powershell.exe 系统自带）。
///
/// `powershell`/`pwsh` 前缀的命令吃掉前缀走同一路由
/// （`.exe` 形态必须先判——否则按 "powershell" 长度切片
/// 残留 `.exe`）。剥前缀后还要吃掉 LLM 按旧习惯带的旗标
/// （`-Command X` / `-EncodedCommand b64` / `-File x.ps1`）
/// ——真机探针钉死：旗标残留在脚本文本里会被当命令名报
/// "无法将 -Command 项识别为 cmdlet"。
///
/// 契约唯一入口：常规 spawn 路径与 `pty: true` ConPTY 一次性路径
/// 共用（两条路径不得各自实现一份，否则必然走散——H2 教训）。
#[cfg(windows)]
pub(crate) fn windows_shell_route(command: &str) -> (&'static str, Vec<String>) {
    let trimmed = command.trim_start();
    let (exe, bare) = match split_windows_shell_prefix(trimmed) {
        Some((exe, rest)) => (exe, rest),
        None => ("powershell.exe", trimmed),
    };
    let bare_lower = bare.to_lowercase();

    let mut args: Vec<String> = vec!["-NoProfile".to_string(), "-NonInteractive".to_string()];
    if bare_lower.starts_with("-encodedcommand ") {
        // base64 token 可能含 `/`，斜杠归一化会破坏编码——
        // 作为真旗标直传，不经归一化。审批引擎 sees 原始命令
        // 串，`powershell-encoded` CONFIRM 规则照常命中。
        args.push("-EncodedCommand".to_string());
        args.push(bare["-encodedcommand ".len()..].trim_start().to_string());
    } else if bare_lower.starts_with("-file ") {
        // 2026-11-19（实测碰壁清单 #17）：① 剥掉 LLM 写的引号（-File 解析器
        // 不剥，报 Illegal characters in path）；② `-ExecutionPolicy Bypass`
        // （进程级）——不带 Bypass 的脚本执行被 AuthorizationManager 静默拒绝。
        // 不扩大能力面：-Command 形态本就不受执行策略约束。
        args.push("-ExecutionPolicy".to_string());
        args.push("Bypass".to_string());
        args.push("-File".to_string());
        args.push(strip_surrounding_quotes(&normalize_windows_path_slashes(
            bare["-file ".len()..].trim_start(),
        )).to_string());
    } else {
        // `-Command X` 的 X 才是脚本文本；裸命令整段进 -Command。
        let script = if bare_lower.starts_with("-command ") {
            bare["-command ".len()..].trim_start()
        } else {
            bare
        };
        args.push("-Command".to_string());
        args.push(normalize_windows_path_slashes(script)
            + "; if ($LASTEXITCODE) { exit $LASTEXITCODE }");
    }
    (exe, args)
}

/// 剥掉首尾成对的双引号（LLM 常给 -File 路径手动加引号）。只剥一层、
/// 且只有成对时才剥——中间的引号是路径内容，不能动。
fn strip_surrounding_quotes(s: &str) -> &str {
    let t = s.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        &t[1..t.len() - 1]
    } else {
        t
    }
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
        on_output: Option<std::sync::Arc<dyn for<'r> Fn(&'r str) + Send + Sync + 'static>>,
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
                            stdout: crate::output::truncate_to_boundary(
                                stdout,
                                self.max_output_size,
                            ),
                            stderr: crate::output::truncate_to_boundary(
                                stderr,
                                self.max_output_size,
                            ),
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
                // Windows: ConPTY 一次性执行（H3 持久终端立项后落地）。
                // 真机探针钉死 ConPTY 可用性与握手/编码/噪音特性，见
                // `pty_session.rs` 头注释与 `tests/pty_conpty_probe.rs`。
                // ConPTY 缺失（Win10 1809 前）时 openpty 报错返回给模型。
                // 注意：此路径不走下方 ACL 沙箱（与 background 相同的
                // 既有豁免面），安全语义由审批层承接。
                let effective_dir = workdir
                    .map(std::path::PathBuf::from)
                    .or_else(|| self.workspace_root.clone())
                    .map(|p| hermix_core::path_security::strip_win32_long_path_buf(&p));
                return self
                    .pty_sessions
                    .execute_oneshot(
                        command,
                        timeout_secs,
                        effective_dir.as_deref(),
                        self.max_output_size,
                    )
                    .await;            }
        }

        // 降级说明（非沙箱路径时为 None）：沙箱建不起来时以非受限方式跑同一条
        // 命令，并把这件事写进 stderr —— 模型和用户都看得到，不静默。
        #[allow(unused_mut)]
        let mut sandbox_note: Option<String> = None;

        #[cfg(windows)]
        {
            // Windows ACL 沙箱路径：HERMIX_SANDBOX_MODE 非 danger-full-access
            // 且有工作目录时，命令进受限 token spawn。provision/spawn 的**沙箱
            // 基础设施**失败（见 sandbox_unavailable）= 降级为非受限执行 +
            // stderr 显式告知；**命令自身**失败原样上报。fail-closed 的旧语义
            // 在沙箱恒失败的机器上等于把终端工具判死（2026-11-18 用户实况：连
            // go build 都做不到）。danger-full-access 与无 workdir 走下方常规
            // 路径，安全语义由审批层承接。
            use crate::sandbox::SandboxMode;
            let mode = SandboxMode::from_env();
            if mode != SandboxMode::DangerFullAccess {
                if let Some(dir) = workdir
                    .map(std::path::PathBuf::from)
                    .or_else(|| self.workspace_root.clone())
                    .map(|p| hermix_core::path_security::strip_win32_long_path_buf(&p))
                {
                    // 2026-11-20（A2 后台一致性）：后台命令走账户沙箱的
                    // spawn_background 路径（detached + 日志重定向），与前台同受
                    // 沙箱约束；账户路也失败才降级为非受限后台执行。
                    if background {
                        match crate::account_sandbox::run_background_in_account_sandbox(command, &dir)
                        {
                            Ok(job_id) => {
                                return Ok(CommandOutput {
                                    exit_code: 0,
                                    stdout: job_id,
                                    stderr: String::new(),
                                });
                            }
                            Err(acct_err) => {
                                sandbox_note = Some(format!(
                                    "[sandbox] 受限与账户沙箱均不可用（{acct_err}），后台命令以非受限方式执行。"
                                ));
                            }
                        }
                    } else {
                        match execute_command_sandboxed(
                            command,
                            timeout_secs,
                            &dir,
                            mode,
                            self.max_output_size,
                            on_output,
                        )
                        .await
                        {
                            Ok(out) => return Ok(out),
                            Err(e) if sandbox_unavailable(&e) => {
                                sandbox_note = Some(format!(
                                    "[sandbox] 受限令牌不可用（{e}），以非受限方式执行同一条命令。"
                                ));
                            }
                            Err(e) => return Err(e),
                        }
                    }
                }
            }
        }

        // Windows 常规路径（danger-full-access 或无 workdir 才落到这里；
        // 默认沙盒路径已在上方处理）。这些命令不进沙盒，但仍要避免
        // CREATE_NO_WINDOW 隐藏执行被杀软拦截。非 background 命令改用 ConPTY
        // 一次性执行（伪终端不弹窗、不隐藏，杀软友好，与 pty 路径一致）；
        // background 必须 detach、ConPTY 不支持，下方保留 std::process（隐藏
        // 标志对该场景是 detach 功能必需，见其注释）。
        #[cfg(windows)]
        if !background {
            let effective_dir = workdir
                .map(std::path::PathBuf::from)
                .or_else(|| self.workspace_root.clone())
                .map(|p| hermix_core::path_security::strip_win32_long_path_buf(&p));
            let mut out = self
                .pty_sessions
                .execute_oneshot(
                    command,
                    timeout_secs,
                    effective_dir.as_deref(),
                    self.max_output_size,
                )
                .await?;
            // 沙箱降级说明：以非受限方式跑的命令必须在 stderr 里挑明，
            // 模型和用户都看得到，不静默。
            if let Some(note) = sandbox_note.take() {
                out.stderr = format!("{note}\n{}", out.stderr);
            }
            return Ok(out);
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
            let (shell_exe, args) = windows_shell_route(trimmed);
            let mut c = TokioCommand::new(shell_exe);
            for a in &args {
                c.arg(a);
            }
            c
        };
        // 仅 background（danger 模式 / 无 workdir）命令会走到这里并设置
        // CREATE_NO_WINDOW：background 必须 detach，ConPTY 是同步 wait 不支持，
        // 故保留无窗口以避免 GUI 应用后台进程永久挂一个黑窗。非 background
        // 已在上方的 ConPTY 分支返回，不会到此。这是 detach 功能必需的隐藏
        // 执行（非默认路径，用户已显式选 full access），不属于被杀软拦截的
        // 交互式终端命令场景。
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
                    // 沙箱降级说明：非受限后台执行的命令在 stderr 挑明。
                    stderr: sandbox_note
                        .map(|n| format!("{n}\n"))
                        .unwrap_or_default(),
                });
            }

            // 用户按停止时，外层 `tool_executor` 直接 `abort()` 这个任务 ——
            // future 被 drop，下面 `tokio::time::timeout` 的 Err 分支根本没有
            // 机会运行，于是 `kill_process_tree` 也不会跑，PowerShell / cmd
            // 进程会一直留在系统里（实测残留：12:16 / 13:38 起各一个 pwsh，
            // 而那两条命令早就没人等了）。
            //
            // Drop guard 兜底：future 被丢弃（abort）时清进程树；正常完成时
            // `disarm()`，不对已退出的 pid 做无谓 taskkill。background 分支在
            // 上面已经 return，构造不到这个 guard，所以不会被误杀。
            struct KillTreeOnDrop(Option<u32>);
            impl KillTreeOnDrop {
                fn disarm(&mut self) {
                    self.0 = None;
                }
            }
            impl Drop for KillTreeOnDrop {
                fn drop(&mut self) {
                    if let Some(pid) = self.0 {
                        tracing::warn!(
                            pid,
                            "command future dropped before completion (abort/stop) — killing process tree"
                        );
                        kill_process_tree(pid);
                    }
                }
            }
            let mut tree_guard = KillTreeOnDrop(child.id());

            let output = child
                .wait_with_output()
                .await
                .map_err(|e| AgentError::Io(format!("Failed to wait for command: {}", e)))?;
            tree_guard.disarm();

            // Decode console output — handle Windows OEM code pages (GBK).
            let stdout = decode_console_output(&output.stdout).into_owned();
            let stderr = decode_console_output(&output.stderr).into_owned();

            // Truncate output if it exceeds max_output_size. Cut at a char
            // boundary: `s[..n]` panics when n lands inside a multi-byte
            // character (CJK / emoji), which is routine in terminal output.
            if stdout.len() > self.max_output_size {
                tracing::warn!(
                    "Command output exceeded max size ({} bytes), truncating",
                    stdout.len()
                );
            }
            let stdout = crate::output::truncate_to_boundary(stdout, self.max_output_size);

            if stderr.len() > self.max_output_size {
                tracing::warn!(
                    "Command stderr exceeded max size ({} bytes), truncating",
                    stderr.len()
                );
            }
            let stderr = crate::output::truncate_to_boundary(stderr, self.max_output_size);

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

    // -----------------------------------------------------------------
    // 持久终端会话（ConPTY，H3）。委托给 pty_sessions 注册表。
    // -----------------------------------------------------------------

    async fn pty_create_session(&self, workdir: Option<&str>) -> Result<String, AgentError> {
        self.pty_sessions.create_session(workdir).await
    }

    async fn pty_send_session(
        &self,
        session_id: &str,
        input: &str,
        submit: bool,
        read_budget_ms: Option<u64>,
    ) -> Result<PtySessionOutput, AgentError> {
        self.pty_sessions
            .send_session(session_id, input, submit, read_budget_ms)
            .await
    }

    async fn pty_read_session(
        &self,
        session_id: &str,
        read_budget_ms: Option<u64>,
    ) -> Result<PtySessionOutput, AgentError> {
        self.pty_sessions.read_session(session_id, read_budget_ms).await
    }

    async fn pty_list_sessions(&self) -> Vec<PtySessionInfo> {
        self.pty_sessions.list_sessions()
    }

    async fn pty_close_session(&self, session_id: &str) -> Result<(), AgentError> {
        self.pty_sessions.close_session(session_id).await
    }
}

/// 有界收集子进程输出：两个读线程各 send 一次，超时即返回（调用方随后
/// 杀进程树迫使管道 EOF）。必须用通道而不是 `join` —— `join` 等的是管道
/// EOF（所有写端关闭），孙进程握着写端时它永远不返回。
#[cfg(windows)]
fn collect_reader_output(
    rx: &std::sync::mpsc::Receiver<(u8, Vec<u8>)>,
    budget: std::time::Duration,
    stdout: &mut Vec<u8>,
    stderr: &mut Vec<u8>,
    got: &mut u8,
) {
    let deadline = std::time::Instant::now() + budget;
    while *got != 3 {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        match rx.recv_timeout(remaining) {
            Ok((0, buf)) => {
                *stdout = buf;
                *got |= 1;
            }
            Ok((_, buf)) => {
                *stderr = buf;
                *got |= 2;
            }
            Err(_) => return,
        }
    }
}

/// 杀掉命令的整棵进程树。job 可用时优先 `TerminateJobObject`（一次杀整棵
/// 树，且不经过 PID——顶层进程退出后 PID 可能被系统复用，`taskkill` 会误杀
/// 无关进程）；附着失败才降级 taskkill。
#[cfg(windows)]
fn kill_command_tree(job: &Option<hermix_core::win_job::JobGuard>, pid: u32) {
    match job {
        Some(job) => job.terminate(),
        None => kill_process_tree(pid),
    }
}

/// 沙箱不可用错误的统一标记：记忆化条目与新建错误都带上它，
/// [`sandbox_unavailable`] 只认这个标记——保证记忆化的错误**必然**可降级，
/// 不会出现「记了个不可降级的错，之后每条命令都硬失败」。
const SANDBOX_UNAVAILABLE_TAG: &str = "[sandbox-unavailable] ";

/// 判定一个执行错误是否属于「沙箱基础设施失败」（命令从未起跑）。只有这类
/// 错误允许降级为非受限执行并在 stderr 写明；命令自身的失败原样上报。
#[cfg(windows)]
fn sandbox_unavailable(e: &AgentError) -> bool {
    let s = e.to_string();
    s.contains(SANDBOX_UNAVAILABLE_TAG)
        || s.contains("(Win32 error 5)")
        || s.contains("(Win32 error 1314)")
        || s.contains("(Win32 error 6)")
}

/// Windows ACL 沙箱执行路径：受限 token spawn + 有界等待 + 超时杀整棵树。
/// 同步 FFI 走 `spawn_blocking`；stdout/stderr 并行读（先 wait 再读会因
/// 管道写满死锁）。fail-closed：provision/spawn 失败直接报错。
#[cfg(windows)]
async fn execute_command_sandboxed(
    command: &str,
    timeout_secs: u64,
    workdir: &std::path::Path,
    mode: crate::sandbox::SandboxMode,
    max_output_size: usize,
    on_output: Option<std::sync::Arc<dyn for<'r> Fn(&'r str) + Send + Sync + 'static>>,
) -> Result<CommandOutput, AgentError> {
    use std::io::Read;

    // 2026-11-19：沙箱「确定不可用」的进程级记忆（实测碰壁清单 #10）。失败是
    // 确定性的 —— 第一次 5/1314 后记住，后续命令直接降级，不再每条白付一遍
    // provision 的 ACL 开销（大工作区上是分钟级）。**按工作区键控**：A 工作区
    // 沙箱坏 ≠ B 工作区沙箱坏。记忆只在进程内：重启应用 = 重新给机会。
    static SANDBOX_VERDICT: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<PathBuf, String>>> =
        std::sync::OnceLock::new();
    let verdict_key = workdir.to_path_buf();
    if let Some(cached) = SANDBOX_VERDICT
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&verdict_key)
        .cloned()
    {
        // 缓存条目一律带 SANDBOX_UNAVAILABLE_TAG —— 保证调用方的
        // sandbox_unavailable 判定必然命中 → 走降级而不是硬失败。
        return Err(AgentError::Io(format!("{SANDBOX_UNAVAILABLE_TAG}{cached}")));
    }

    let ws = workdir.to_path_buf();
    let wd = workdir.to_path_buf();
    let command = command.to_string();
    let blocking = tokio::task::spawn_blocking(move || -> Result<CommandOutput, AgentError> {
        // 账户沙箱兜底：受限 token spawn 的**任何**失败都视为基础设施失败
        // （命令从未起跑）—— 内置 Administrator（RID 500）机器上受限令牌恒
        // 失败（1314/5/6，2026-11-19 实测）→ 专用本地账户 +
        // CreateProcessWithLogonW（免特权）。账户路也失败（setup 被拒/工作区
        // 不可授权）才记忆化降级。
        let fallback_to_account = |orig: AgentError| -> Result<crate::sandbox::SandboxChild, AgentError> {
            let setup_ready = crate::account_sandbox::setup_is_complete()
                || (crate::account_sandbox::can_request_setup()
                    && crate::account_sandbox::request_setup().is_ok());
            let account_result = if setup_ready {
                crate::account_sandbox::run_command_in_account_sandbox(&command, &wd)
            } else {
                Err(AgentError::Io("sandbox account setup not available".into()))
            };
            match account_result {
                Ok(c) => Ok(c),
                Err(acct_err) => {
                    tracing::warn!(
                        workdir = %wd.display(),
                        account_error = %acct_err,
                        "account sandbox unavailable too — degrading to unrestricted execution",
                    );
                    SANDBOX_VERDICT
                        .get_or_init(Default::default)
                        .lock()
                        .unwrap_or_else(|x| x.into_inner())
                        .insert(
                            wd.clone(),
                            format!("{SANDBOX_UNAVAILABLE_TAG}{}", orig),
                        );
                    Err(orig)
                }
            }
        };
        // provision + spawn 合并成一个表达式：任何一步失败都汇入同一账户兜底，
        // 兜底产出 SandboxChild 后汇入同一条收集尾巴。
        let child_result = (|| -> Result<crate::sandbox::SandboxChild, AgentError> {
            let session = crate::sandbox::SandboxSession::provision(mode, &ws)?;
            // 统一 shell 层同款路由：整段命令进 PowerShell -Command。
            session.spawn(
                "powershell.exe",
                &[
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-Command".to_string(),
                    command.clone(),
                ],
                &wd,
            )
        })();
        let mut child = match child_result {
            Ok(c) => c,
            Err(e) => match fallback_to_account(e) {
                Ok(c) => c,
                Err(orig) => return Err(orig),
            },
        };
        let pid = child.pid;
        // 进程树容器：只 TerminateProcess 顶层时，孙进程（cargo/rustc/node）
        // 会继续握着 stdout 写端，读线程永不返回——"命令已超时但会话/UI
        // 永远卡住"的根因。附着失败时降级为 taskkill 杀树。
        #[cfg(windows)]
        let job = hermix_core::win_job::JobGuard::attach(pid);

        let (mut so, mut se) = child.try_clone_pipes()?;

        // 有界排空：只保留前 CAPTURE_CAP 字节，超出部分继续读但不再存储。
        // 既不让子进程因管道写满而挂死，也不让 `dir /s` 级输出把内存吃爆
        // （旧 read_to_end 无上限，截断发生在拿到完整字节之后，救不了峰值）。
        const CAPTURE_CAP: usize = 32 * 1024 * 1024;
        // 空闲超时（2026-11-20 用户要求「完成率高，不要总是出错」）：
        // timeout 参数从「硬总时长」升级为「**空闲窗口**」——每次收到输出都
        // 重置计时。持续产出（构建/安装/扫描）跑满硬上限也不被掐；真挂死
        // （120s 无任何输出）照杀。硬上限 600s 与 terminal 显式上限对齐。
        let last_output = std::sync::Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
        let (tx, rx) = std::sync::mpsc::channel::<(u8, Vec<u8>)>();
        let tx_out = tx.clone();
        let last_out_out = last_output.clone();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 65_536];
            // 流式（2026-11-20）：按行缓冲 stdout，每完成一行调一次回调。
            // UI 的运行中面板由此拿到实时输出（此前攒到退出才一次性返回）。
            let mut pending = String::new();
            loop {
                match so.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        if n > 0 {
                            *last_out_out.lock().unwrap() = std::time::Instant::now();
                            if let Some(cb) = &on_output {
                                pending.push_str(&String::from_utf8_lossy(&chunk[..n]));
                                while let Some(pos) = pending.find('\n') {
                                    let line: String = pending.drain(..=pos).collect();
                                    let line = line.trim_end_matches(['\n', '\r']);
                                    if !line.is_empty() {
                                        cb(line);
                                    }
                                }
                            }
                        }
                        if buf.len() < CAPTURE_CAP {
                            let take = n.min(CAPTURE_CAP - buf.len());
                            buf.extend_from_slice(&chunk[..take]);
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = tx_out.send((0, buf));
        });
        let tx_err = tx.clone();
        let last_out_err = last_output.clone();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 65_536];
            loop {
                match se.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        if n > 0 {
                            *last_out_err.lock().unwrap() = std::time::Instant::now();
                        }
                        if buf.len() < CAPTURE_CAP {
                            let take = n.min(CAPTURE_CAP - buf.len());
                            buf.extend_from_slice(&chunk[..take]);
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = tx_err.send((1, buf));
        });
        drop(tx);

        // 空闲续期等待：wait_timeout 按 200ms 切片轮询，每片之后检查
        // 「距上次输出是否已超过空闲窗口」——超过即视为挂死杀树；输出持续
        // 到来则一直续期（受总上限约束）。timeout 参数的语义因此从
        // 「硬总时长」变成「空闲窗口」。
        let idle_window = std::time::Duration::from_secs(timeout_secs);
        let total_cap = std::time::Duration::from_secs(600); // 与 terminal 显式上限对齐
        let started_at = std::time::Instant::now();
        let outcome = loop {
            match child.wait_timeout(std::time::Duration::from_millis(200)) {
                Ok(crate::sandbox::WaitOutcome::Exited(code)) => {
                    break crate::sandbox::WaitOutcome::Exited(code);
                }
                Ok(crate::sandbox::WaitOutcome::TimedOut) => {
                    let idle_exceeded = last_output.lock().unwrap().elapsed() >= idle_window;
                    let total_exceeded = started_at.elapsed() >= total_cap;
                    if idle_exceeded || total_exceeded {
                        break crate::sandbox::WaitOutcome::TimedOut;
                    }
                }
                Err(e) => {
                    child.terminate();
                    kill_command_tree(&job, pid);
                    return Err(e);
                }
            }
        };
        let timed_out = matches!(outcome, crate::sandbox::WaitOutcome::TimedOut);
        if timed_out {
            child.terminate();
            kill_command_tree(&job, pid);
        }

        let mut stdout_bytes = Vec::new();
        let mut stderr_bytes = Vec::new();
        let mut got = 0u8;
        collect_reader_output(
            &rx,
            std::time::Duration::from_secs(2),
            &mut stdout_bytes,
            &mut stderr_bytes,
            &mut got,
        );
        if got != 3 {
            // 正常退出也可能有孙进程握着写端（`start /b`、后台子进程）。
            // 收尾窗口用尽后杀树再收一次，绝不让读线程永久挂着。
            child.terminate();
            kill_command_tree(&job, pid);
            collect_reader_output(
                &rx,
                std::time::Duration::from_secs(2),
                &mut stdout_bytes,
                &mut stderr_bytes,
                &mut got,
            );
        }
        drop(rx);

        // A5（2026-11-20）：结局已定（成功或超时已显式杀过），撤销 Drop 的
        // 自动 terminate —— `start /b` 留下的后台子进程继续活着；
        // PID 复用误杀风险随之消失。
        if let Some(job) = &job {
            job.disarm();
        }

        if timed_out {
            return Err(AgentError::Timeout(format!(
                "Command timed out after {} seconds",
                timeout_secs
            )));
        }

        let exit_code = match outcome {
            crate::sandbox::WaitOutcome::Exited(code) => code as i32,
            crate::sandbox::WaitOutcome::TimedOut => -1,
        };
        let stdout = decode_console_output(&stdout_bytes).into_owned();
        let stderr = decode_console_output(&stderr_bytes).into_owned();
        if stdout.len() > max_output_size || stderr.len() > max_output_size {
            tracing::warn!(
                stdout_bytes = stdout.len(),
                stderr_bytes = stderr.len(),
                max_output_size,
                "sandboxed command output exceeded max size, truncating"
            );
        }
        Ok(CommandOutput {
            exit_code,
            // 按字节直切会在 CJK/emoji 中间 panic（非沙箱路径早已改，这里漏过）。
            stdout: crate::output::truncate_to_boundary(stdout, max_output_size),
            stderr: crate::output::truncate_to_boundary(stderr, max_output_size),
        })
    });
    // 兜底：即使进程树/管道出现病态行为，也不让一次命令永久占住一个
    // blocking 线程（线程池耗尽会让整个后端卡死）。
    // 2026-11-20：空闲续期后合法长命令可跑到 600s 总上限，兜底必须
    // ≥ 总上限 + 余量，否则续期中的命令会在 timeout+30s 被外层掐掉。
    match tokio::time::timeout(
        std::time::Duration::from_secs(600 + 30),
        blocking,
    )
    .await
    {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => Err(AgentError::Io(format!("sandbox join failed: {e}"))),
        Err(_) => Err(AgentError::Timeout(format!(
            "Command timed out after {} seconds (sandbox cleanup exceeded its budget)",
            timeout_secs
        ))),
    }
}

#[cfg(test)]
// The three sandbox tests below hold `SANDBOX_ENV_LOCK` across `.await`
// on purpose: `HERMIX_SANDBOX_MODE` is process-global, so dropping the
// guard before the command runs would let a parallel test overwrite it
// and make the routing assertions flap. The lint's deadlock argument (an
// owning thread parks and can never release) does not apply — these are
// `#[tokio::test]`s whose guard is released when the test ends. The lint
// fires at the *call sites*, so the allow has to live on the module, not
// on `set_sandbox_env`.
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;

    #[test]
    fn windows_shell_route_appends_exit_code_guard() {
        // powershell.exe -Command 会把尾部成功语句之后的退出码重置为 0；
        // 路由层必须在脚本尾显式透传 $LASTEXITCODE（2026-11-20 实测）。只对
        // -Command 形态生效——脚本以显式 exit 结尾时后缀不会执行。
        let (exe, args) = windows_shell_route("cargo build && echo done");
        assert_eq!(exe, "powershell.exe");
        let script = args.last().unwrap();
        assert!(
            script.ends_with("; if ($LASTEXITCODE) { exit $LASTEXITCODE }"),
            "missing exit guard: {script}"
        );
        assert!(script.starts_with("cargo build && echo done"));
    }

    #[test]
    fn windows_shell_route_file_branch_has_no_guard() {
        // -File 形态走的是 powershell 自己的脚本退出码语义，不能加 -Command
        // 的守卫后缀（否则脚本文件名后面跟了一段脚本体，直接语法错）。
        let (_exe, args) = windows_shell_route("-File build.ps1");
        assert_eq!(args[2], "-ExecutionPolicy");
        assert_eq!(args[3], "Bypass");
        assert_eq!(args[4], "-File");
        assert_eq!(args[5], "build.ps1");
        assert_eq!(args.len(), 6);
    }

    /// 沙箱路由测试共用锁：HERMIX_SANDBOX_MODE 是进程级 env，
    /// 并行测试互踩会导致路由分支漂移。
    static SANDBOX_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[cfg(windows)]
    fn set_sandbox_env(mode: &str) -> std::sync::MutexGuard<'static, ()> {
        let guard = SANDBOX_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("HERMIX_SANDBOX_MODE", mode);
        guard
    }

    /// 某些卷（外接盘、特殊 ACL 的临时目录）不允许给沙箱临时目录授 DACL
    /// （SetNamedSecurityInfoW → Win32 error 5）。这种环境跑不了沙箱，
    /// 相关测试直接跳过 —— 这是环境的属性，不是被测代码的行为。
    async fn probe_sandbox(backend: &LocalBackend) -> bool {
        match backend.execute_command("echo ok", None, None, false, false, None).await {
            Ok(o) => o.exit_code == 0,
            Err(e) => !e.to_string().contains("Win32 error 5"),
        }
    }

    /// 带 workdir 的探针：沙箱目录建在 workdir 之下，授 DACL 的失败只在
    /// 这种形态出现（无 workdir 的探测会误判为可用）。
    async fn probe_sandbox_in_workdir(backend: &LocalBackend) -> bool {
        let tmp = match tempfile::tempdir() {
            Ok(t) => t,
            Err(_) => return true,
        };
        match backend
            .execute_command(
                "echo ok",
                None,
                Some(tmp.path().to_str().unwrap()),
                false,
                false,
             None,)
            .await
        {
            Ok(o) => o.exit_code == 0,
            Err(e) => !e.to_string().contains("Win32 error 5"),
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn sandbox_route_rejects_drive_root_fail_closed() {
        let _g = set_sandbox_env("workspace-write");
        let backend = LocalBackend::default();
        let result = backend
            .execute_command("echo hi", None, Some("C:\\"), false, false, None)
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
    async fn sandbox_route_survives_unwritable_temp_root() {
        let _g = set_sandbox_env("workspace-write");
        // 本机实况（2026-11-10）：系统 TMP 指向 <TMPDIR>，整棵子树
        // 的 ACL 拒绝 WRITE_DAC（新建子目录也继承拒绝，tempfile 建的
        // 目录一样授不进去）→ 主 temp 根授权必失败。回归钉：provision
        // 必须回退到 ~/.hermix/tmp 成功建会话。测试 workspace 直接建
        // 在回退根内（确定可授），用完即清。
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .expect("test requires a home dir");
        let primary = std::path::Path::new(&home)
            .join(".hermix")
            .join("tmp")
            .join(format!("sbx-test-primary-{}", std::process::id()));
        std::fs::create_dir_all(&primary).expect("create test primary root");
        let session = crate::sandbox::SandboxSession::provision(
            crate::sandbox::SandboxMode::WorkspaceWrite,
            &primary,
        )
        .expect("fallback temp root should keep the sandbox usable");
        assert!(
            !session.temp_dir.starts_with(&primary),
            "fallback temp must live outside the test workspace"
        );
        let _ = std::fs::remove_dir_all(&primary);
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
             None,)
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
        let output = match backend
            .execute_command(
                "echo hi",
                None,
                Some(tmp.path().to_str().unwrap()),
                false,
                false,
             None,)
            .await
        {
            Ok(o) => o,
            Err(e) if e.to_string().contains("Win32 error 5") => {
                eprintln!("skip: sandbox ACL denied on this volume");
                std::env::remove_var("HERMIX_SANDBOX_MODE");
                return;
            }
            Err(e) => panic!("workspace-write sandbox must run the command end-to-end: {e}"),
        };
        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.trim().contains("hi"));
        std::env::remove_var("HERMIX_SANDBOX_MODE");
    }

    #[tokio::test]
    async fn test_execute_command_echo() {
        let _env_guard = SANDBOX_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let backend = LocalBackend::default();
        let output = backend
            .execute_command("echo hello", None, None, false, false, None)
            .await
            .unwrap();
        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.trim().contains("hello"));
    }

    #[tokio::test]
    async fn test_execute_command_with_workdir() {
        let _env_guard = SANDBOX_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Use a real existing directory on the host instead of the
        // Unix-only `/tmp` literal that this test used to hardcode
        // (it silently passed on Linux but always failed on
        // Windows). The behavior under test is "the workdir param
        // is honored" — any existing dir is fine.
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path().to_str().unwrap();
        let backend = LocalBackend::default();
        if !probe_sandbox_in_workdir(&backend).await {
            eprintln!("skip: sandbox ACL denied on this volume");
            return;
        }
        let output = backend
            .execute_command(
                if cfg!(windows) { "cd" } else { "pwd" },
                None,
                Some(workdir),
                false,
                false,
             None,)
            .await
            .unwrap();
        assert_eq!(output.exit_code, 0);
        // On Windows `cd` doesn't print the path, so just assert
        // the command succeeded without checking the path output.
        if !cfg!(windows) {
            assert!(output.stdout.trim().contains(workdir.trim_end_matches('/')));
        }
    }

    /// 输出收集阶段必须有界：一个读线程永不回报时也不能卡住调用方
    /// （旧实现是 `join()`，等的是管道 EOF，孙进程握着写端就永不返回）。
    #[cfg(windows)]
    #[test]
    fn collect_reader_output_gives_up_after_budget() {
        let (tx, rx) = std::sync::mpsc::channel::<(u8, Vec<u8>)>();
        tx.send((0u8, b"hello".to_vec())).unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut got = 0u8;
        let started = std::time::Instant::now();
        collect_reader_output(
            &rx,
            std::time::Duration::from_millis(150),
            &mut stdout,
            &mut stderr,
            &mut got,
        );
        let elapsed = started.elapsed();
        assert_eq!(got, 1, "只收到 stdout");
        assert_eq!(stdout, b"hello");
        assert!(
            elapsed >= std::time::Duration::from_millis(140),
            "应等满预算再放弃，实际 {elapsed:?}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "不得无限等待，实际 {elapsed:?}"
        );
    }

    /// 回归：超时只杀顶层进程时，孙进程继续握着 stdout 写端，旧实现的
    /// `join()` 会一直等到孙进程自然退出（`ping -n 20` ≈ 19s），表现为
    /// "命令已超时报错但会话/UI 卡住"。新路径靠 job/taskkill 杀整棵树。
    #[cfg(windows)]
    #[tokio::test]
    async fn sandbox_timeout_kills_grandchild_and_does_not_hang() {
        // `set_sandbox_env` 自身会取 `SANDBOX_ENV_LOCK`，这里不能再手动锁
        // （std Mutex 不可重入，双重加锁 = 自死锁）。
        let _sandbox_env = set_sandbox_env("workspace-write");
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path().to_str().unwrap();
        let backend = LocalBackend::new(1, 1_048_576);
        if !probe_sandbox_in_workdir(&backend).await {
            eprintln!("skip: sandbox ACL denied on this volume");
            return;
        }
        let started = std::time::Instant::now();
        let result = backend
            .execute_command("ping -n 20 127.0.0.1", None, Some(workdir), false, false, None)
            .await;
        let elapsed = started.elapsed();
        assert!(
            matches!(result, Err(AgentError::Timeout(_))),
            "超时必须如实报 Timeout，实际 {result:?}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(12),
            "不得等孙进程自然退出（会卡 ~19s），实际 {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn test_execute_command_timeout() {
        let _env_guard = SANDBOX_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let backend = LocalBackend::new(1, 1_048_576);
        if !probe_sandbox(&backend).await {
            eprintln!("skip: sandbox ACL denied on this volume");
            return;
        }
        // Platform-appropriate long-running command. PowerShell has a
        // built-in `Start-Sleep` so no external binary is needed.
        let cmd = if cfg!(windows) {
            "Start-Sleep -Seconds 30"
        } else {
            "sleep 30"
        };
        let result = backend
            .execute_command(cmd, None, None, false, false, None)
            .await;
        assert!(result.is_err());
        match result {
            Err(AgentError::Timeout(_)) => {}
            _ => panic!("Expected timeout error"),
        }
    }

    #[tokio::test]
    async fn test_execute_command_failure() {
        let _env_guard = SANDBOX_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let backend = LocalBackend::default();
        let output = backend
            .execute_command("exit 42", None, None, false, false, None)
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
        let _env_guard = SANDBOX_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join("hermix_ps_route_alias");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"hello").unwrap();
        let backend = LocalBackend::default();
        let out = match backend
            .execute_command(
                "ls a.txt",
                None,
                Some(dir.to_string_lossy().as_ref()),
                false,
                false,
             None,)
            .await
        {
            Ok(o) => o,
            Err(e) if e.to_string().contains("Win32 error 5") => {
                eprintln!("skip: sandbox ACL denied on this volume");
                let _ = std::fs::remove_dir_all(&dir);
                return;
            }
            Err(e) => panic!("unexpected error: {e}"),
        };
        assert_eq!(out.exit_code, 0);
        assert!(out.stdout.contains("a.txt"), "stdout: {}", out.stdout);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    #[cfg(windows)]
    async fn powershell_route_unknown_command_reports_nonzero_exit() {
        let _env_guard = SANDBOX_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // 诚实失败语义：未知命令 exit != 0 且 stderr 带诊断（错误
        // 文案随系统语言变化，只断言非空）。
        let backend = LocalBackend::default();
        let out = match backend
            .execute_command("definitely_not_a_command_xyz_9x7", None, None, false, false, None)
            .await
        {
            Ok(o) => o,
            Err(e) if e.to_string().contains("Win32 error 5") => {
                eprintln!("skip: sandbox ACL denied on this volume");
                return;
            }
            Err(e) => panic!("unexpected error: {e}"),
        };
        if out.exit_code != 0 && out.stderr.is_empty() {
            // 沙箱在这台机器上建不起来时，命令根本没跑出去 —— 环境属性，跳过。
            eprintln!("skip: sandbox unavailable (nonzero exit with empty stderr)");
            return;
        }
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
            .execute_command("cmd /c exit 42", None, None, false, false, None)
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
             None,)
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
             None,)
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
             None,)
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

    #[test]
    fn slash_norm_dollar_interpolation_never_flipped() {
        // BUG-08 回归：变量插值的值运行时才可知。`"$base/system/..."`
        // 看着像相对路径，实际 $base 展开后是 URL——按路径翻斜杠会把
        // `https://x/system/a` 葬送成 `https://x\system\a`。含 `$` 的
        // token 一律原样保留。
        assert_eq!(
            normalize_windows_path_slashes(
                "Invoke-WebRequest -Uri \"$base/system/rights/companyinfo\""
            ),
            "Invoke-WebRequest -Uri \"$base/system/rights/companyinfo\""
        );
        // 变量赋值里的 URL 同理（$base = 'https://…'）。
        assert_eq!(
            normalize_windows_path_slashes("$base = 'https://sy.cssso.com'.Trim()"),
            "$base = 'https://sy.cssso.com'.Trim()"
        );
    }

    #[test]
    fn slash_norm_flips_real_paths_beside_interpolation() {
        // 同一条命令里真路径照翻、插值 token 不动——两者互不牵连。
        assert_eq!(
            normalize_windows_path_slashes(
                "Get-Content C:/logs/a.txt; Invoke-WebRequest -Uri \"$base/system/x\""
            ),
            "Get-Content C:\\logs\\a.txt; Invoke-WebRequest -Uri \"$base/system/x\""
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
