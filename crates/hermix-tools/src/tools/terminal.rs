//! Terminal tools: terminal command execution and process management

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};
use std::sync::Arc;

use hermix_core::{
    path_security::{forbidden_win32_prefix, detect_shell_metacharacters, strip_win32_long_path},
    tool_schema, ApprovalOutcome, Capability, CommandOutput, JsonSchema, Origin, Shell,
    ShellError, PtySessionOutput,
    TerminalBackend, ToolError, ToolHandler, ToolSchema, traits::RiskLevel,
};

use crate::approval::{ApprovalDecision, ApprovalManager, TerminalApprovalContext};
use crate::context::{ContextAware, ToolCallContext};
use crate::ToolRegistry;

/// A 修复: terminal 工具的硬超时上限 (秒)。
/// LLM 不传 `timeout` 时兜底到此值; 也防止 backend `default_timeout`
/// 被改大后, powershell/cmd 去无限等外部 API 而卡死整个对话。
/// 与 `LocalBackend::default()` 的 120s 对齐。
const TERMINAL_HARD_TIMEOUT_SECS: u64 = 120;

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the `terminal` tool with the registry (P1.2 migration).
/// Pass the same `terminal_workspace` handle used by other terminal-touching
/// tools (and the agent service) so per-turn workspace updates propagate
/// here too. Test-only callers without a registry workspace can pass
/// `Arc::new(RwLock::new(None))`.
/// Pass the registry's shared `terminal_approval_ctx` handle
/// (`registry.terminal_approval_ctx_handle()`) so `LocalAgentService`
/// can wire the WS approval hook per turn without re-registering the
/// tool. The handle starts as `default()` (both fields `None`); the
/// tool no longer falls back to any LLM-side confirmation bypass.
/// flow while it's still `None` (unit tests, headless callers).
pub fn register(
    registry: &ToolRegistry,
    backend: Arc<dyn TerminalBackend>,
    terminal_workspace: Arc<std::sync::RwLock<Option<std::path::PathBuf>>>,
    terminal_approval_ctx: Arc<std::sync::Mutex<TerminalApprovalContext>>,
    // 接缝 `Shell`：主进程装配注入。`None` 时 terminal 退回老路径
    // （format_command_output 裸 stderr + [exit code: N]）。
    shell: Option<Arc<dyn Shell>>,
) {
    let mut handler = TerminalHandler::new(
        backend,
        terminal_workspace,
        terminal_approval_ctx,
    );
    if let Some(s) = shell {
        handler = handler.with_shell(s);
    }
    let handler = Arc::new(handler);
    let schema = handler.schema();
    registry.register_with_risk(
        "terminal",
        "terminal",
        schema,
        handler.clone(),
        Arc::new(|| true),
        vec![],
        true,
        "Execute a shell command in the user's workspace (Windows, unified PowerShell layer; backslash paths, PowerShell cmdlets preferred).\n\n\
         ## When to use\n\
         Building, running tests, git operations, and anything the structured tools cannot do.\n\n\
         ## When NOT to use\n\
         Reading file contents (read_file / markitdown), searching (search_files), moving/copying (move_file), deleting (storage_clean) — purpose-built tools handle those with correct argument validation, risk grading, and Windows quoting. Falling back to the shell for these is how quoting and path bugs start.\n\n\
         ## Output discipline\n\
         Output is capped; unbounded dumps (dir /s /b, cat huge.log, find /) are blocked — pipe through Select-String / head / tail first. Per-command risk is decided by the internal ApprovalManager; a command needing user approval is presented as a confirmation card — you cannot approve it yourself, and no text marker bypasses the prompt.",
        "💻",
        None,
        RiskLevel::Caution,
        false,
    );
    //(session isolation): register as ContextAware so
    // dispatch_with_context routes to execute_with_context with
    // per-session ctx instead of reading process-global Arcs.
    let context_aware: Arc<dyn ContextAware> = handler as Arc<dyn ContextAware>;
    registry.set_context_aware("terminal", context_aware);
}

// ---------------------------------------------------------------------------
// TerminalHandler
// ---------------------------------------------------------------------------

/// Tool for executing terminal commands via an injected backend.
pub struct TerminalHandler {
    backend: std::sync::Arc<dyn TerminalBackend>,
    approval: ApprovalManager,
    /// Per-turn workspace root, updated by `LocalAgentService` before each
    /// `agent.run_*` call from `AgentOverrides::workspace`. When the LLM
    /// doesn't pass an explicit `workdir` param, this is used as the
    /// default `current_dir` for the spawned process — so terminal
    /// commands always run in the user's "工作区", not the server CWD.
    default_workspace: std::sync::Arc<std::sync::RwLock<Option<std::path::PathBuf>>>,
    /// Per-request approval hook . When populated,
    /// `RequiresConfirmation` is surfaced as a `request_approval` event
    /// on the active WS channel (so the desktop's `InlineApprovalCard`
    /// renders instead of bouncing the user through LLM text). When
    /// empty (default `TerminalApprovalContext`), the legacy
    /// the old LLM-side confirmation flow no longer exists.
    approval_ctx: Arc<std::sync::Mutex<TerminalApprovalContext>>,
    /// 接缝 `Shell`：C3 接缝用于把执行失败归类成结构化的
    /// `ShellErrorKind`，给模型可执行指令而不是裸 stderr。提供了就走
    /// 接缝，没提供继续把 `AgentError::to_string()` 透传——保留既有
    /// 8 个 terminal 测试的形状。
    shell: Option<Arc<dyn Shell>>,
}

impl TerminalHandler {
    /// Construct a handler. Pass a shared `terminal_workspace` handle from
    /// [`ToolRegistry::terminal_workspace_handle`] so the agent service can
    /// update the per-turn workspace without holding a direct reference to
    /// this handler. Tests that don't care about workspace semantics can
    /// pass `Arc::new(RwLock::new(None))`.
    /// Pass a shared `terminal_approval_ctx` handle from
    /// [`ToolRegistry::terminal_approval_ctx_handle`] so the agent
    /// service can swap the WS approval hook per request without
    /// re-registering the tool. Tests / headless callers can pass
    /// `Arc::new(Mutex::new(TerminalApprovalContext::default()))`.
    pub fn new(
        backend: std::sync::Arc<dyn TerminalBackend>,
        terminal_workspace: std::sync::Arc<std::sync::RwLock<Option<std::path::PathBuf>>>,
        terminal_approval_ctx: Arc<std::sync::Mutex<TerminalApprovalContext>>,
    ) -> Self {
        Self {
            backend,
            approval: ApprovalManager::new(),
            default_workspace: terminal_workspace,
            approval_ctx: terminal_approval_ctx,
            shell: None,
        }
    }

    /// 给后续迁移阶段使用：把 `Shell` seam 接进 terminal tool，
    /// 让执行失败的 stderr 输出走 `Shell::classify` + `instruction()`
    /// 而不是裸回灌。当前 `register()` 默认不传，等主进程装配接入。
    pub fn with_shell(mut self, shell: Arc<dyn Shell>) -> Self {
        self.shell = Some(shell);
        self
    }

    /// Update the default workspace for the next and subsequent tool calls.
    /// Cheap (RwLock write). Pass `None` to clear.
    pub fn set_default_workspace(&self, workspace: Option<std::path::PathBuf>) {
        if let Ok(mut guard) = self.default_workspace.write() {
            *guard = workspace;
        }
    }

    /// — When the agent service wired a `TerminalApprovalContext`
    /// for this turn (it captures the active WS `ApprovalRegistry` + the
    /// `on_event` channel), emit a `request_approval` event on the WS
    /// stream and `.await` the user's `approved` / `denied` click via
    /// the registry's oneshot receiver.
    /// Returns:
    /// - `Some(ApprovalOutcome::Approved)` — user approved; caller
    /// should fall through and execute. We also `approve_for_session`
    /// so the same command string is implicit-allow for the rest of
    /// the session (mirrors the user clicking "Allow for this
    /// session" in the InlineApprovalCard).
    /// - `Some(other)` — user denied / timed out / channel closed;
    /// caller should error out.
    /// - `None`        — no WS context wired (default state); caller
    /// has no LLM-side confirmation escape hatch, so
    /// headless / unit-test callers keep working.
    async fn request_ws_approval(
        &self,
        effective_cmd: &str,
        reason: Option<&str>,
        approval_ctx: &TerminalApprovalContext,
    ) -> Option<ApprovalOutcome> {
        //the context is now passed in directly — no mutex
        // lock needed. The caller (execute_inner) obtained it from
        // either the global Arc (execute path) or ctx (ContextAware
        // path), so it's already scoped to this call.
        if !approval_ctx.is_wired() {
            return None;
        }
        let session_id = approval_ctx.session_id.clone();
        let requester = approval_ctx.requester()?;
        let on_event = approval_ctx.on_event()?;

        // Build a unique request id. The terminal tool can fire many
        // approvals back-to-back in a single turn, so monotonic
        // counter is safer than `Uuid::new_v4()` for human-readable
        // WS logs (and it's zero-allocation).
        let request_id = format!(
            "terminal-{}",
            NEXT_REQUEST_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );

        // Register the pending decision BEFORE emitting so the
        // registry can't lose a race against a too-fast click.
        let rx = requester(session_id.clone(), request_id.clone());

        // Push the `request_approval` envelope. Shape matches
        // `decide_pre_tool_dispatch`'s dangerous-tool payload so the
        // frontend's existing `InlineApprovalCard` and
        // `useStreamChat.onApprovalRequest` handle it unchanged.
        on_event(json!({
            "event": "request_approval",
            "request_id": request_id,
            "tool": "terminal",
            "arguments": {
                "command": effective_cmd,
                "reason": reason.unwrap_or(""),
            },
            "risk": "caution",
        }));

        // Wait for the user's click. The dangerous-tool approval
        // path uses a 60s timeout that auto-denies; we don't enforce
        // it here because `ApprovalRegistry::register` already does
        // (it sets up its own 5-minute `DEFAULT_APPROVAL_TIMEOUT`).
        match rx.await {
            Ok(ApprovalOutcome::Approved) => {
                // Persist the approval to the in-session bypass so
                // a follow-up call with the same command string
                // skips the prompt (matches "Allow for this session"
                // semantics — `decide_pre_tool_dispatch` consults
                // `session_bypass` for the same shortcut).
                self.approval
                    .approve_for_session(effective_cmd);
                Some(ApprovalOutcome::Approved)
            }
            Ok(other) => {
                // Any non-approved outcome (user denied, timed out,
                // or channel closed) is reported back as a denial so
                // the caller's `!matches!(.., Approved)` check below
                // rejects the command.
                Some(other)
            }
            Err(_canceled) => {
                // The oneshot was dropped without sending — happens
                // when the agent service tore down the registry mid-
                // prompt (session ended, WS closed). Treat as a
                // channel-closed denial.
                tracing::warn!(
                    command = %effective_cmd,
                    request_id = %request_id,
                    "approval oneshot canceled before user response"
                );
                Some(ApprovalOutcome::ChannelClosed)
            }
        }
    }
}

/// Monotonic counter for terminal approval request ids (one per
/// process). Keeps log lines short and stable so a human reading
/// "terminal-42 approved" / "terminal-43 denied" can correlate
/// the WS envelope with the tool trace.
static NEXT_REQUEST_ID: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

#[async_trait]
impl ToolHandler for TerminalHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        //read from global Arcs (backward compat for
        // non-context dispatch path — tests, headless callers).
        let workspace = self
            .default_workspace
            .read()
            .ok()
            .and_then(|g| g.as_ref().cloned());
        let approval_ctx = self
            .approval_ctx
            .lock()
            .ok()
            .map(|g| g.clone())
            .unwrap_or_default();
        // 默认 Local：非 HTTP / 无 Origin 上下文路径（tests + headless）
        // 走老路径，零回归。
        execute_inner(self, params, workspace, &approval_ctx, &Origin::Local, None).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "command".into(),
            json!({
                "type": "string",
                "description": "The command to execute. \
                                Windows: the terminal tool runs commands through a PowerShell shell layer \
                                (single unified routing, argv passed through directly — no cmd.exe string joining). \
                                Built-in POSIX aliases ls→Get-ChildItem and cat→Get-Content work; \
                                grep has NO built-in alias — use Select-String for searching. \
                                Prefer native cmdlets (Get-ChildItem, Get-Content, \
                                Select-String, Get-Process) over cmd.exe one-liners (dir, type, findstr). \
                                Quote paths containing spaces with double quotes (Get-ChildItem \"C:\\Program Files\\x\"). \
                                Use native backslash path form (C:\\Users\\x\\file.txt); forward slashes are also \
                                accepted by PowerShell but backslash is the native form. \
                                Unix: standard shell syntax."
            }),
        );
        props.insert(
            "reason".into(),
            json!({
                "type": "string",
                "description": "One-sentence explanation of WHY this command is being run and what it does, \
                                for the user's approval card (e.g. \"安装项目依赖中的 requests 库\"). \
                                Required whenever the command has side effects — always fill it."
            }),
        );
        props.insert(
            "timeout".into(),
            json!({
                "type": "integer",
                "description": "Timeout in milliseconds (default: 30000)"
            }),
        );
        props.insert(
            "workdir".into(),
            json!({
                "type": "string",
                "description": "Working directory for the command"
            }),
        );
        props.insert(
            "background".into(),
            json!({
                "type": "boolean",
                "description": "Run command in background (default: false)",
                "default": false
            }),
        );
        props.insert(
            "pty".into(),
            json!({
                "type": "boolean",
                "description": "Run this ONE command in a pseudoconsole (interactive programs see a real terminal; colors/pagers work). Windows: ConPTY-backed. Default false. For state that must SURVIVE across calls (cd, venv, REPL), use persistent instead.",
                "default": false
            }),
        );
        props.insert(
            "persistent".into(),
            json!({
                "type": "boolean",
                "description": "Create a PERSISTENT PowerShell session whose state survives across calls (cd, activated venv, REPL, gdb). The response footer carries the session id; pass it as session_id in follow-up terminal calls to continue the same session. Readiness/completion detection is heuristic. Default false.",
                "default": false
            }),
        );
        props.insert(
            "session_id".into(),
            json!({
                "type": "string",
                "description": "Continue an existing persistent session (id from a previous persistent call's footer). The command is submitted to that session's shell and only newly produced output is returned. Use command=\"\" + submit=false to poll for pending output without submitting anything."
            }),
        );
        props.insert(
            "submit".into(),
            json!({
                "type": "boolean",
                "description": "Persistent sessions only: append Enter to the input (default true). Set false to send raw fragments WITHOUT pressing Enter (REPL continuation lines, multi-line paste) — finish with a final submit=true call.",
                "default": true
            }),
        );

        tool_schema(
            "terminal",
            "Execute a terminal command. Returns stdout, stderr, and exit code.\n\
             \n\
             ## When to reach for this tool\n\
             `terminal` is the right tool whenever the user wants to **see, list, count, search, or inspect** something on the system: directory listings (`dir` / `ls`), file contents (`type` / `cat` / `head`), running processes (`tasklist` / `ps`), system info, or any one-off shell pipeline. \
             It is also the right tool for **doing** things on the system: running a build, installing a package, invoking a CLI the user names. \
             Prefer this over `computer_use.screenshot` whenever a shell command can answer the question — `screenshot` is a 50× bigger payload for 1/50th the precision, and shell output is searchable, copy-pasteable, and diffable.\n\
             \n\
             ## Persistent sessions (REPL / venv / cd across calls)\n\
             One-off calls forget all state — `cd x && cmd` chaining and re-sourcing venv on every call is the smell. When a workflow needs state to survive (interactive `python`/`node` REPL, an activated venv, gdb, repeated commands in one directory), pass `persistent: true` once and then `session_id` on every follow-up. Full-screen TUI programs (vim/htop) are NOT supported in sessions — their redraw output is unreadable.",
            JsonSchema::object(props, vec!["command".into()]),
        )
    }
}

/// Core execution logic — free function to avoid trait-impl method
/// restrictions. Takes `&TerminalHandler` as first arg so both
/// `ToolHandler::execute` and `ContextAware::execute_with_context`
/// can call it with per-session workspace/approval_ctx.
async fn execute_inner(
    this: &TerminalHandler,
    params: Value,
    workspace: Option<std::path::PathBuf>,
    approval_ctx: &TerminalApprovalContext,
    origin: &Origin,
    device_scopes: Option<&std::collections::HashSet<Capability>>,
) -> Result<String, ToolError> {
        let command = params
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'command' parameter".into()))?;

        // Hard pre-flight gate on pathological path patterns. The
        // LLM is free to invent tool-call arguments and has been
        // observed to fabricate Windows-namespace-prefixed paths
        // like `ls \\?\D:\data` — `\\?\` is the Win32 device
        // namespace, used by low-level system / kernel code; it has
        // no business appearing in a user-facing shell command. We
        // also block:
        // - `\\.\` (Win32 device namespace, e.g. `\\.\COM1`,
        // `\\.\PhysicalDrive0`)
        // - `\\?\UNC\...` (Win32 UNC namespace) — when an agent
        // actually needs to reach a UNC share it should use the
        // normal `\\server\share\...` form; the `\\?\UNC\` form
        // is the long-path escape hatch and adds no value
        // over standard syntax for typical agent workloads.
        // These are *not* deny-by-policy decisions (which fire the
        // user-approval flow) — they are hard invalid-argument
        // errors. The LLM never asked the user whether to run
        // `\\?\D:\data`; the user shouldn't be asked to bless a
        // command the model invented out of whole cloth. We surface
        // a clear ToolError so the LLM gets feedback in its next
        // turn and can reformulate against a real path the user
        // actually mentioned.
        if let Some(bad_prefix) = forbidden_win32_prefix(command) {
            tracing::warn!(
                command,
                bad_prefix,
                "rejected terminal command with forbidden Win32 path prefix"
            );
            return Err(ToolError::InvalidParams(format!(
                "Command contains forbidden Win32 path prefix '{}'. \
                 Use standard paths (e.g. `D:\\qcc` or `./qcc`) or \
                 a normal UNC path (`\\\\server\\share`). The \
                 `\\\\?\\` Win32 device namespace is reserved for \
                 low-level system code and is not a valid argument \
                 in user-facing shell commands.",
                bad_prefix
            )));
        }

        // Binary-file guard: when the LLM tries to read binary file
        // CONTENT (images, PDFs) via terminal commands like `type`,
        // `cat`, `more`, `head`, `tail`, block it and redirect to
        // `delegate_task` + `read_image` / `read_pdf`. Terminal can
        // only inspect file metadata (dir, certutil, fc), not visual
        // content — the LLM must use a sub-agent for that.
        if let Some(ext) = detect_binary_content_read(command) {
            tracing::warn!(
                command,
                ext,
                "blocked terminal attempt to read binary file content"
            );
            let tool = if ext == "pdf" { "read_pdf" } else { "read_image" };
            return Err(ToolError::ExecutionFailed(format!(
                "Cannot read binary .{} file content via terminal. \
                 Terminal commands can only inspect file metadata \
                 (size, hash, structure), not visual content. \
                 \n\n\
                 To analyze this file's content:\n\
                 1. Call `delegate_task` with a task like \
                 \"Read and identify the content of this {}\"\n\
                 2. Pass the file path in the `context` parameter\n\
                 3. The sub-agent will call `{}` and return the \
                 result\n\
                 \n\
                 Example:\n\
                 delegate_task(task=\"Identify the person name and \
                 document type in this image\", context=\"Path: <the file path>\")",
                ext, ext, tool
            )));
        }

        // Binary-file nudge: for OTHER terminal commands that reference
        // binary file extensions (dir, certutil, fc, copy, etc.),
        // append a reminder to the output. This keeps the LLM aware
        // that terminal can't do content analysis, without blocking
        // useful metadata operations.
        let binary_nudge = detect_binary_file_reference(command);

        // -------------------------------------------------------------------
        // SINGLE approval gate. Metacharacter (pipe-to-shell injection) and
        // confirm-pattern checks are merged into ONE prompt. Previously they
        // were two independent `request_ws_approval` calls — a command that
        // is BOTH an injection vector AND a confirm-pattern hit popped the
        // approval card twice ("同一个审批项目连续两次跳出审批窗口"). Now a
        // command requires confirmation at most once, with both reasons shown.
        // Denied stays the highest priority (no pointless prompt first).
        // -------------------------------------------------------------------
        let metachar_desc = detect_shell_metacharacters(command);
        // 2026-10-06：不再剥 `HERMIX_CONFIRM=1 ` 前缀（审计批次 2 第 4 条）。
        //
        // **那个前缀是模型可以自己写的**。判定是
        // `RequiresConfirmation => stripped.is_none()`——只要命令文本以
        // 该前缀开头就跳过审批，于是模型重发一遍同样的命令、加上这行前缀
        // 就能自己批自己，用户根本没看到审批卡。它把"用户同意"降级成了
        // "模型声称用户同意"。
        //
        // 审计方案原文是把这个前缀**token 化**（registry 签发一次性 token、
        // 经审批上下文传递、terminal 校验）。这里选择**直接删除**，理由：
        // 要达成的安全结果（模型无法自签）两者完全相同，而删除不新增任何
        // 攻击面——token 机制自己就是新攻击面（怎么传？会不会进日志？能否
        // 重放？），21 处引用也不用动。
        //
        // 行为变化与方案预期一致：依赖该前缀的既有工作流退回审批卡。这正是
        // 目的，不是回归。
        let effective_cmd = command;

        let decision = this
            .approval
            .check_approval_for_origin_with_scopes(effective_cmd, origin, device_scopes);
        let needs_confirm = if metachar_desc.is_some() {
            // Pipe-to-shell always prompts.
            true
        } else {
            match decision {
                ApprovalDecision::Denied => {
                    return Err(ToolError::ExecutionFailed(format!(
                        "Command denied by security policy: {}",
                        effective_cmd
                    )));
                }
                ApprovalDecision::RequiresConfirmation => true,
                ApprovalDecision::Approved => false,
            }
        };

        if needs_confirm {
            let display = match metachar_desc {
                Some(d) => format!("{} (contains {})", effective_cmd, d),
                None => effective_cmd.to_string(),
            };
            // LLM-supplied rationale for the approval card (see schema).
            let reason = params
                .get("reason")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            if let Some(decision) = this
                .request_ws_approval(&display, reason, approval_ctx)
                .await
            {
                if !matches!(decision, ApprovalOutcome::Approved) {
                    let reason = match decision {
                        ApprovalOutcome::DeniedByUser => "denied by user",
                        ApprovalOutcome::DeniedWithFeedback { note } => {
                            return Err(ToolError::ExecutionFailed(format!(
                                "Command denied by user: {note}
{}",
                                effective_cmd
                            )))
                        }
                        ApprovalOutcome::TimedOut { timeout_secs } => {
                            return Err(ToolError::ExecutionFailed(format!(
                                "Command approval timed out after {timeout_secs}s: {}",
                                effective_cmd
                            )))
                        }
                        ApprovalOutcome::ChannelClosed => {
                            "connection closed before response"
                        }
                        ApprovalOutcome::Approved => {
                            // Guarded by !matches! above.
                            "approved (unexpected)"
                        }
                    };
                    return Err(ToolError::ExecutionFailed(format!(
                        "Command {reason}: {}",
                        effective_cmd
                    )));
                }
                // Persist the session approval so a follow-up call with
                // the same command / rule family skips the prompt.
                this.approval.approve_for_session(effective_cmd);
                tracing::info!(
                    command = %effective_cmd,
                    "command confirmed by user via WS approval card"
                );
            } else {
                // No WS context wired (headless test, CLI run, or unit test
                // that bypassed the agent service). There is no way to ask
                // the user, and **no way for the model to fake having asked**:
                // 2026-10-06 removed the `HERMIX_CONFIRM=1` escape hatch, so
                // the command simply fails. Do not print retry instructions
                // here — anything we tell the model becomes a workaround.
                return Err(ToolError::ExecutionFailed(format!(
                    "Command requires user confirmation but no approval channel is available: {}",
                    effective_cmd
                )));
            }
        }

        let timeout = params
            .get("timeout")
            .and_then(|v| v.as_u64())
            .or(Some(TERMINAL_HARD_TIMEOUT_SECS));

        // Resolve effective workdir:
        // 1. explicit `workdir` param from the LLM (highest priority)
        // 2. per-turn default_workspace (set by LocalAgentService)
        // 3. None (let the backend fall back to its own workspace_root or CWD)
        let explicit_workdir = params.get("workdir").and_then(|v| v.as_str());
        let workdir = explicit_workdir
            .map(|s| strip_win32_long_path(s))
            .or_else(|| {
                workspace
                    .as_ref()
                    .map(|p| strip_win32_long_path(&p.to_string_lossy()))
            });

        // Validate workdir for path traversal attempts
        if let Some(ref wd) = workdir {
            if hermix_core::path_security::has_traversal_component(wd) {
                tracing::warn!(
                    workdir = %wd,
                    "terminal workdir contains traversal component (..)"
                );
                return Err(ToolError::InvalidParams(format!(
                    "workdir '{}' contains path traversal (..). \
                     Use an absolute path or a relative path without '..' components.",
                    wd
                )));
            }
        }

        let background = crate::param_coercion::bool_param(&params, "background", false);

        let pty = crate::param_coercion::bool_param(&params, "pty", false);

        // -------------------------------------------------------------------
        // 持久终端会话分支（H3）。审批门已在上方按同一段契约跑过——
        // 每条 send 的命令文本照过 CONFIRM/DENY，不存在第二条审批路径。
        // -------------------------------------------------------------------
        let session_id_param = params
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let persistent = crate::param_coercion::bool_param(&params, "persistent", false);
        if session_id_param.is_some() || persistent {
            let submit = crate::param_coercion::bool_param(&params, "submit", true);
            if background {
                return Err(ToolError::InvalidParams(
                    "'background' is not supported with persistent terminal sessions; \
                     long-running work belongs inside the session itself"
                        .into(),
                ));
            }
            if pty {
                return Err(ToolError::InvalidParams(
                    "'pty' (one-shot pseudoconsole) and persistent sessions are \
                     mutually exclusive — drop 'pty' and use persistent/session_id"
                        .into(),
                ));
            }
            return execute_session_call(
                this,
                session_id_param,
                persistent,
                command,
                submit,
                timeout,
                workdir,
            )
            .await;
        }

        match this
            .backend
            .execute_command(command, timeout, workdir.as_deref(), background, pty)
            .await
        {
            Ok(output) => {
                // 接缝路径：exit_code != 0 时不再裸回灌 stderr，而是把
                // 输出喂给 `Shell::classify`，命中结构化 `ShellErrorKind`
                // 就输出 `ShellError::instruction()`（"换做法"），未命中
                // 退回 `format_command_output`（保留既有 8 个测试的形状）。
                //
                // SEAMS_DESIGN §3 决策 C 的实际落地：Windows 同一类失败
                // 有 N 种面，归一后才能阻止模型盲试循环。
                if output.exit_code != 0 {
                    if let Some(shell) = this.shell.as_ref() {
                        let exec = hermix_core::ExecOutput {
                            code: output.exit_code,
                            stdout: output.stdout.clone(),
                            stderr: output.stderr.clone(),
                            timed_out: false,
                        };
                        if let Some(kind) = shell.classify(&exec) {
                            let detail = if !output.stderr.is_empty() {
                                output.stderr.trim()
                            } else {
                                output.stdout.trim()
                            };
                            let err = ShellError::classified(kind, detail);
                            return Err(ToolError::from_message(format!(
                                "{}\n\n--- stderr ---\n{}",
                                err.instruction(),
                                output.stderr
                            )));
                        }
                    }
                }
                let mut result = format_command_output(&output);
                if let Some(nudge) = binary_nudge {
                    result.push_str(&format!(
                        "\n\n{}",
                        nudge
                    ));
                }
                Ok(result)
            }
            Err(e) => Err(ToolError::from_message(e.to_string())),
        }
}

impl TerminalHandler {
    /// 来源感知版入口（§5.5 第 5 层）。HTTP dispatcher 从
    /// `Extension<Origin>` 取出后调本方法；非 HTTP 路径继续走
    /// [`ToolHandler::execute`]（=Local）。
    pub async fn execute_for_origin(
        &self,
        params: Value,
        origin: &Origin,
    ) -> Result<String, ToolError> {
        let workspace = self
            .default_workspace
            .read()
            .ok()
            .and_then(|g| g.as_ref().cloned());
        let approval_ctx = self
            .approval_ctx
            .lock()
            .ok()
            .map(|g| g.clone())
            .unwrap_or_default();
        // execute_for_origin 是 HTTP dispatcher 的来源感知入口，
        // 能力位集维度走 execute_with_context（带 ToolCallContext）。
        // 此路径只传 origin，device_scopes = None（纯矩阵判定）。
        execute_inner(self, params, workspace, &approval_ctx, origin, None).await
    }

    // The `forbidden_path_prefix` private helper used to live here;
    // it was extracted to `hermix_core::path_security` during the
    // Windows-first hardening pass so memory / skills /
    // cron / credentials tools can all share the same check.
}

// ---------------------------------------------------------------------------
//ContextAware impl
// ---------------------------------------------------------------------------

/// directly reads from `ctx` — no global Arc swap/restore.
/// This eliminates the race window entirely: the workspace and
/// approval context come from the per-session `ToolCallContext`
/// captured in the bridge closure, not from process-global
/// `Arc<Mutex<…>>` fields that concurrent sessions can overwrite.
#[async_trait]
impl ContextAware for TerminalHandler {
    async fn execute_with_context(
        &self,
        params: Value,
        ctx: &ToolCallContext,
    ) -> Result<String, ToolError> {
        // 来源 + 设备能力位由调度方在 ToolCallContext 里带入（§5.5 第
        // 4/5 层）：
        // - 桌面 WS 路径（LocalAgentService）origin 固定 Local、
        //   device_scopes = None（无设备维度）；
        // - HTTP 路径由 request_guard → Extension → handler 覆盖填入；
        // - 测试 / 头less 走 ctx default = Local + None。
        execute_inner(
            self,
            params,
            ctx.workspace.clone(),
            &ctx.approval_ctx,
            &ctx.origin,
            ctx.device_scopes.as_ref(),
        )
        .await
    }
}

/// Detect if a terminal command is trying to READ the CONTENT of a
/// binary file (images, PDFs). Returns the file extension if so.
///
/// Only blocks content-reading commands (type, cat, more, head, tail, sort).
/// Metadata commands (dir, ls, certutil, fc, copy, move, del) are allowed.
fn detect_binary_content_read(command: &str) -> Option<&'static str> {
    let lower = command.to_lowercase();
    // Content-reading command prefixes (Windows + Unix).
    let read_cmds = ["type ", "cat ", "more ", "head ", "tail ", "sort "];
    let is_read_cmd = read_cmds.iter().any(|c| lower.starts_with(c)
        || lower.contains(&format!("| {}", c.trim()))
        || lower.contains(&format!("&&{}", c.trim())));
    if !is_read_cmd {
        return None;
    }
    // Check for binary file extensions in the command.
    let binary_exts = [".jpg", ".jpeg", ".png", ".gif", ".webp", ".bmp", ".pdf"];
    for ext in binary_exts {
        if lower.contains(ext) {
            // Return without the dot for the error message.
            return Some(match ext {
                ".jpg" | ".jpeg" => "jpg",
                ".png" => "png",
                ".gif" => "gif",
                ".webp" => "webp",
                ".bmp" => "bmp",
                ".pdf" => "pdf",
                _ => ext,
            });
        }
    }
    None
}

/// Detect if a terminal command references binary file extensions
/// (without being a content-read command). Returns a nudge string
/// to append to the command output.
fn detect_binary_file_reference(command: &str) -> Option<String> {
    let lower = command.to_lowercase();
    let binary_exts = [".jpg", ".jpeg", ".png", ".gif", ".webp", ".bmp", ".pdf"];
    let has_binary = binary_exts.iter().any(|e| lower.contains(e));
    if !has_binary {
        return None;
    }
    // Don't nudge if this is already a blocked content-read command
    // (those get a hard error, not a nudge).
    if detect_binary_content_read(command).is_some() {
        return None;
    }
    Some(
        "\n[Note: This command references a binary image/PDF file. \
         Terminal can only inspect metadata (size, hash, structure). \
         For CONTENT analysis (OCR, person/vehicle identification, \
         document type recognition), use delegate_task to spawn a \
         sub-agent that calls read_image or read_pdf.]".to_string()
    )
}

/// 持久会话调用：续用（session_id）或新建（persistent）。新建时把
/// workdir 交给 backend（ConPTY cwd），首条命令照常提交。
async fn execute_session_call(
    this: &TerminalHandler,
    session_id: Option<String>,
    persistent: bool,
    command: &str,
    submit: bool,
    timeout: Option<u64>,
    workdir: Option<String>,
) -> Result<String, ToolError> {
    let just_created = session_id.is_none();
    let output = match session_id {
        Some(sid) => {
            // 空 command + submit=false = 纯读取（轮询 pending 输出）。
            if command.is_empty() && !submit {
                this.backend
                    .pty_read_session(&sid, timeout)
                    .await
                    .map_err(|e| ToolError::from_message(e.to_string()))?
            } else {
                this.backend
                    .pty_send_session(&sid, command, submit, timeout)
                    .await
                    .map_err(|e| ToolError::from_message(e.to_string()))?
            }
        }
        None => {
            let sid = this
                .backend
                .pty_create_session(workdir.as_deref())
                .await
                .map_err(|e| ToolError::from_message(e.to_string()))?;
            // 空 command + submit=false：仅完成创建（会话已就绪才返回）。
            if command.is_empty() && !submit {
                this.backend
                    .pty_read_session(&sid, timeout)
                    .await
                    .map_err(|e| ToolError::from_message(e.to_string()))?
            } else {
                this.backend
                    .pty_send_session(&sid, command, submit, timeout)
                    .await
                    .map_err(|e| ToolError::from_message(e.to_string()))?
            }
        }
    };
    Ok(format_session_output(&output, just_created && persistent))
}

/// 会话输出格式化：正文 + 会话状态页脚（id / busy 轮询指引 / 退出提示）。
fn format_session_output(output: &PtySessionOutput, just_created: bool) -> String {
    const MAX_OUTPUT_CHARS: usize = 100_000;
    let mut result = output.text.clone();
    if result.chars().count() > MAX_OUTPUT_CHARS {
        let truncated: String = result.chars().take(MAX_OUTPUT_CHARS).collect();
        result = format!(
            "{}\n\n[... output truncated at {} chars, {} total ...]",
            truncated,
            MAX_OUTPUT_CHARS,
            result.chars().count()
        );
    }

    let mut footer = if just_created {
        format!("\n[persistent session created: id={}]", output.session_id)
    } else {
        format!("\n[session: {}]", output.session_id)
    };
    if output.busy {
        footer.push_str(&format!(
            " | still producing output — poll with session_id=\"{}\", command=\"\", submit=false",
            output.session_id
        ));
    }
    if !output.alive {
        footer.push_str(" | session has exited — create a new one with persistent: true");
    }
    if result.trim().is_empty() {
        result.push_str("no new output");
    }
    result.push_str(&footer);
    result
}

/// Format command output for display.
fn format_command_output(output: &CommandOutput) -> String {
    const MAX_OUTPUT_CHARS: usize = 100_000; // 100K chars safety limit
    let mut result = String::new();
    if !output.stdout.is_empty() {
        result.push_str(&output.stdout);
    }
    if !output.stderr.is_empty() {
        if !result.is_empty() {
            result.push_str("\n--- STDERR ---\n");
        }
        result.push_str(&output.stderr);
    }
    // Safety truncation to prevent unbounded memory allocation
    if result.chars().count() > MAX_OUTPUT_CHARS {
        let truncated: String = result.chars().take(MAX_OUTPUT_CHARS).collect();
        result = format!(
            "{}\n\n[... output truncated at {} chars, {} total ...]",
            truncated,
            MAX_OUTPUT_CHARS,
            result.chars().count()
        );
    }
    if output.exit_code != 0 {
        result.push_str(&format!("\n[exit code: {}]", output.exit_code));
    }
    if result.is_empty() {
        result = format!("[exit code: {}]", output.exit_code);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_core::AgentError;

    struct MockBackend {
        captured_workdir: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    }
    #[async_trait]
    impl TerminalBackend for MockBackend {
        async fn execute_command(
            &self,
            cmd: &str,
            _timeout: Option<u64>,
            workdir: Option<&str>,
            _bg: bool,
            _pty: bool,
        ) -> Result<CommandOutput, AgentError> {
            *self.captured_workdir.lock().unwrap() = workdir.map(str::to_string);
            Ok(CommandOutput {
                exit_code: 0,
                stdout: format!("output of: {}", cmd),
                stderr: String::new(),
            })
        }
        async fn read_file(
            &self,
            _path: &str,
            _offset: Option<u64>,
            _limit: Option<u64>,
        ) -> Result<String, AgentError> {
            Ok(String::new())
        }
        async fn write_file(&self, _path: &str, _content: &str) -> Result<(), AgentError> {
            Ok(())
        }
        async fn file_exists(&self, _path: &str) -> Result<bool, AgentError> {
            Ok(true)
        }
    }

    fn make_mock() -> (std::sync::Arc<MockBackend>, std::sync::Arc<std::sync::Mutex<Option<String>>>) {
        let cap = std::sync::Arc::new(std::sync::Mutex::new(None));
        let backend = std::sync::Arc::new(MockBackend {
            captured_workdir: cap.clone(),
        });
        (backend, cap)
    }

    #[tokio::test]
    async fn test_terminal_handler_schema() {
        let (backend, _cap) = make_mock();
        let handler = TerminalHandler::new(
            backend,
            std::sync::Arc::new(std::sync::RwLock::new(None)),
            std::sync::Arc::new(std::sync::Mutex::new(
                TerminalApprovalContext::default(),
            )),
        );
        let schema = handler.schema();
        assert_eq!(schema.name, "terminal");
    }

    #[tokio::test]
    async fn test_terminal_handler_execute() {
        let (backend, _cap) = make_mock();
        let handler = TerminalHandler::new(
            backend,
            std::sync::Arc::new(std::sync::RwLock::new(None)),
            std::sync::Arc::new(std::sync::Mutex::new(
                TerminalApprovalContext::default(),
            )),
        );
        let result = handler
            .execute(json!({"command": "echo hello"}))
            .await
            .unwrap();
        assert!(result.contains("echo hello"));
    }

    #[test]
    fn forbidden_prefix_detection() {
        // The LLM-fabricated case from the original bug report.
        // Raw JSON arg from the wire is `ls \\?\D:\data` (one
        // layer of backslashes per JSON escape). We accept both
        // the raw form and the post-shell-unescape form.
        assert_eq!(
            hermix_core::path_security::forbidden_win32_prefix("ls -la \\\\?\\D:\\qcc"),
            Some("\\\\?\\")
        );
        assert_eq!(
            hermix_core::path_security::forbidden_win32_prefix(r"ls -la \?\D:\data"),
            Some("\\\\?\\")
        );
        // Win32 UNC namespace (long-path escape hatch).
        assert_eq!(
            hermix_core::path_security::forbidden_win32_prefix(r"type \\?\UNC\server\share\file"),
            Some("\\\\?\\UNC\\")
        );
        // Win32 device namespace.
        assert_eq!(
            hermix_core::path_security::forbidden_win32_prefix(r"echo hello > \\.\COM1"),
            Some("\\\\.\\")
        );
        // Normal commands must pass through.
        assert_eq!(
            hermix_core::path_security::forbidden_win32_prefix("ls -la D:\\qcc"),
            None
        );
        assert_eq!(
            hermix_core::path_security::forbidden_win32_prefix(r"cat /etc/hosts"),
            None
        );
        assert_eq!(
            hermix_core::path_security::forbidden_win32_prefix("echo hello"),
            None
        );
    }

    #[tokio::test]
    async fn forbidden_prefix_rejected_without_running() {
        // Verify the pre-flight gate actually short-circuits
        // execution — `backend` is real (mock) so any
        // accidental pass-through would land in `execute_command`
        // and we would observe the captured command. We use a
        // backend whose `execute_command` panics to make
        // "got past the gate" observable.
        struct PanicBackend;
        #[async_trait]
        impl TerminalBackend for PanicBackend {
            async fn execute_command(
                &self,
                _cmd: &str,
                _t: Option<u64>,
                _w: Option<&str>,
                _bg: bool,
                _pty: bool,
            ) -> Result<CommandOutput, AgentError> {
                panic!("forbidden_prefix_rejected_without_running: backend was reached — gate failed");
            }
            async fn read_file(
                &self,
                _p: &str,
                _o: Option<u64>,
                _l: Option<u64>,
            ) -> Result<String, AgentError> {
                panic!("read_file should not be called in this test");
            }
            async fn write_file(
                &self,
                _p: &str,
                _c: &str,
            ) -> Result<(), AgentError> {
                panic!("write_file should not be called in this test");
            }
            async fn file_exists(&self, _p: &str) -> Result<bool, AgentError> {
                panic!("file_exists should not be called in this test");
            }
        }
        let handler = TerminalHandler::new(
            std::sync::Arc::new(PanicBackend),
            std::sync::Arc::new(std::sync::RwLock::new(None)),
            std::sync::Arc::new(std::sync::Mutex::new(
                TerminalApprovalContext::default(),
            )),
        );
        let err = handler
            .execute(json!({"command": "ls -la \\\\?\\D:\\qcc"}))
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("forbidden Win32 path prefix"),
            "expected explicit pre-flight error, got: {}",
            msg
        );
    }

    #[tokio::test]
    async fn default_workspace_applied_when_no_explicit_workdir() {
        let (backend, captured) = make_mock();
        let ws_handle: std::sync::Arc<std::sync::RwLock<Option<std::path::PathBuf>>> =
            std::sync::Arc::new(std::sync::RwLock::new(None));
        let handler = TerminalHandler::new(
            backend,
            ws_handle.clone(),
            std::sync::Arc::new(std::sync::Mutex::new(
                TerminalApprovalContext::default(),
            )),
        );

        // No workdir param, no default → backend sees None.
        handler
            .execute(json!({"command": "ls"}))
            .await
            .unwrap();
        assert_eq!(captured.lock().unwrap().clone(), None);

        // Set default workspace; next call should see it forwarded.
        handler.set_default_workspace(Some(std::path::PathBuf::from("/work/qcc")));
        handler
            .execute(json!({"command": "ls"}))
            .await
            .unwrap();
        assert_eq!(
            captured.lock().unwrap().clone().as_deref(),
            Some("/work/qcc")
        );

        // Explicit workdir param wins over default.
        handler
            .execute(json!({"command": "ls", "workdir": "/override"}))
            .await
            .unwrap();
        assert_eq!(
            captured.lock().unwrap().clone().as_deref(),
            Some("/override")
        );

        // Clearing the default reverts to None.
        handler.set_default_workspace(None);
        handler
            .execute(json!({"command": "ls"}))
            .await
            .unwrap();
        assert_eq!(captured.lock().unwrap().clone(), None);
    }

    //WS approval path (, "Caution tool needs UI prompts") ----------

    /// Helper: build a `TerminalApprovalContext` plus a registry of
    /// pending oneshot senders so the test can resolve the user's
    /// "click" from outside the handler. Returns `(ctx, sender_registry,
    /// captured_event)`. The test calls `pop_pending()` to fetch the
    /// sender and `tx.send(decision)` to settle the request.
    fn wire_approval_ctx() -> (
        std::sync::Arc<std::sync::Mutex<TerminalApprovalContext>>,
        std::sync::Arc<
            std::sync::Mutex<
                std::collections::HashMap<String, tokio::sync::oneshot::Sender<ApprovalOutcome>>,
            >,
        >,
        std::sync::Arc<std::sync::Mutex<Option<serde_json::Value>>>,
    ) {
        let reg: std::sync::Arc<
            std::sync::Mutex<
                std::collections::HashMap<String, tokio::sync::oneshot::Sender<ApprovalOutcome>>,
            >,
        > = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let reg_clone = reg.clone();

        let requester: std::sync::Arc<
            dyn Fn(String, String) -> tokio::sync::oneshot::Receiver<ApprovalOutcome>
                + Send
                + Sync,
        > = std::sync::Arc::new(move |_sid: String, rid: String| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            reg_clone.lock().unwrap().insert(rid, tx);
            rx
        });

        let captured_event: std::sync::Arc<std::sync::Mutex<Option<serde_json::Value>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_event_clone = captured_event.clone();
        let on_event: std::sync::Arc<dyn Fn(serde_json::Value) + Send + Sync> =
            std::sync::Arc::new(move |v: serde_json::Value| {
                *captured_event_clone.lock().unwrap() = Some(v);
            });

        let ctx = TerminalApprovalContext {
            session_id: "sess-test".to_string(),
            requester: Some(requester),
            on_event: Some(on_event),
        };

        (
            std::sync::Arc::new(std::sync::Mutex::new(ctx)),
            reg,
            captured_event,
        )
    }

    #[tokio::test]
    async fn ws_path_approved_falls_through_and_emits_event() {
        // Wire the WS context with a fake ApprovalRegistry-style
        // oneshot map so we can settle the request from outside.
        let (approval_ctx, reg, captured_event) = wire_approval_ctx();

        let (backend, _cap) = make_mock();
        let handler = TerminalHandler::new(
            backend,
            std::sync::Arc::new(std::sync::RwLock::new(None)),
            approval_ctx,
        );

        // Spawn the "user clicks Approve" task; it polls for the
        // pending request and resolves with `true`.
        let reg_for_task = reg.clone();
        let approver = tokio::spawn(async move {
            for _ in 0..100 {
                if let Some((_rid, tx)) =
                    reg_for_task.lock().unwrap().drain().next()
                {
                    let _ = tx.send(ApprovalOutcome::Approved);
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            panic!("approval request was never registered");
        });

        // `sudo` matches the `(?i)\bsudo\b` CONFIRM pattern.
        let result = handler
            .execute(json!({"command": "sudo apt install ripgrep"}))
            .await;
        approver.await.unwrap();

        // Fall-through: the mock backend returned success, so the
        // execute call must succeed (no error, no `HERMIX_CONFIRM=1`
        // bounce).
        result.expect("WS-approved command must execute, not error");

        // The on_event closure saw the request_approval envelope.
        let event = captured_event
            .lock()
            .unwrap()
            .clone()
            .expect("on_event must have fired");
        assert_eq!(event["event"], "request_approval");
        assert_eq!(event["tool"], "terminal");
        assert_eq!(event["risk"], "caution");
        assert_eq!(event["arguments"]["command"], "sudo apt install ripgrep");
        assert_eq!(event["risk"], "caution");
        let rid = event["request_id"]
            .as_str()
            .expect("request_id must be a string");
        assert!(
            rid.starts_with("terminal-"),
            "request_id should be prefixed with 'terminal-' (got {rid})"
        );
    }

    #[tokio::test]
    async fn ws_path_denied_returns_user_denial_error() {
        let (approval_ctx, reg, captured_event) = wire_approval_ctx();
        let (backend, _cap) = make_mock();
        let handler = TerminalHandler::new(
            backend,
            std::sync::Arc::new(std::sync::RwLock::new(None)),
            approval_ctx,
        );

        // Spawn the "user clicks Deny" task.
        let reg_for_task = reg.clone();
        let denier = tokio::spawn(async move {
            for _ in 0..100 {
                if let Some((_rid, tx)) =
                    reg_for_task.lock().unwrap().drain().next()
                {
                    let _ = tx.send(ApprovalOutcome::DeniedByUser);
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            panic!("approval request was never registered");
        });

        // Use `taskkill /f` which matches the Windows CONFIRM pattern
        // so we don't depend on Linux-only `sudo`.
        let result = handler
            .execute(json!({"command": "taskkill /f /im foo.exe"}))
            .await;
        denier.await.unwrap();

        let err = result.expect_err(
            "WS-denied command must error out, not execute",
        );
        let msg = format!("{err:?}").to_lowercase();
        assert!(
            msg.contains("denied by user"),
            "error must say 'denied by user', got: {msg}",
        );
        // The on_event still fired (the frontend needs to know the
        // card was rendered before the user denied it).
        assert!(
            captured_event.lock().unwrap().is_some(),
            "on_event must have fired even when user denied",
        );
    }

    #[tokio::test]
    async fn fallback_to_text_flow_when_ws_ctx_unwired() {
        // No approval_ctx wired: tool must fall back to the legacy
        // `HERMIX_CONFIRM=1` text-bounce flow. This is the
        // backward-compat path for headless callers / unit tests.
        let (backend, _cap) = make_mock();
        let handler = TerminalHandler::new(
            backend,
            std::sync::Arc::new(std::sync::RwLock::new(None)),
            std::sync::Arc::new(std::sync::Mutex::new(
                TerminalApprovalContext::default(),
            )),
        );

        let result = handler
            .execute(json!({"command": "sudo apt install ripgrep"}))
            .await;
        let err = result.expect_err(
            "without WS context, RequiresConfirmation must error",
        );
        let msg = format!("{err:?}");
        assert!(
            msg.contains("Command requires user confirmation"),
            "error must name the real blocker (got: {msg})",
        );
        // The old message taught the model to retry with the marker — i.e.
        // it handed out the bypass. The error must not mention it.
        assert!(
            !msg.contains("HERMIX_CONFIRM"),
            "error must NOT hand the model a bypass (got: {msg})",
        );
    }

    #[tokio::test]
    async fn hermix_confirm_marker_no_longer_bypasses_approval() {
        // 2026-10-06（审计批次 2 第 4 条）：`HERMIX_CONFIRM=1` 前缀曾是
        // **模型可自己写**的绕过开关——它把「用户同意」降级成「模型声称用户
        // 同意」。这个测试原来断言该前缀能跳过 WS 请求，现在断言它不能。
        //
        // **不 wire WS context**：wired 情况下 `wire_approval_ctx` 的
        // requester 只往注册表里塞一个 oneshot sender 就返回，没人回值，
        // `execute` 会一直等下去（原来的测试之所以秒回，恰恰是因为带前缀
        // 时压根不请求 WS）。无 context 时命令会立刻以"没有审批通道"失败，
        // 既不卡住，也同样证明了「前缀不再能自己批自己」。
        let (backend, _cap) = make_mock();
        let handler = TerminalHandler::new(
            backend,
            std::sync::Arc::new(std::sync::RwLock::new(None)),
            std::sync::Arc::new(std::sync::Mutex::new(
                TerminalApprovalContext::default(),
            )),
        );

        let err = handler
            .execute(json!({
                "command": "HERMIX_CONFIRM=1 sudo apt install ripgrep"
            }))
            .await
            .expect_err("the marker must not grant approval on its own");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("no approval channel is available"),
            "expected the no-approval-channel failure, got: {msg}"
        );
    }

    #[test]
    fn terminal_workspace_handle_shares_state_across_clones() {
        // Simulates the production setup where ToolRegistry owns the
        // canonical handle and `LocalAgentService` writes through it; the
        // `TerminalHandler` reads from a clone.
        let a: std::sync::Arc<std::sync::RwLock<Option<std::path::PathBuf>>> =
            std::sync::Arc::new(std::sync::RwLock::new(None));
        let b = a.clone();
        assert!(a.read().unwrap().is_none());

        *b.write().unwrap() = Some(std::path::PathBuf::from("/proj"));
        assert_eq!(
            a.read().unwrap().as_deref(),
            Some(std::path::Path::new("/proj"))
        );
    }

    #[test]
    fn test_format_command_output() {
        let output = CommandOutput {
            exit_code: 0,
            stdout: "hello".to_string(),
            stderr: String::new(),
        };
        assert_eq!(format_command_output(&output), "hello");

        let output_with_stderr = CommandOutput {
            exit_code: 1,
            stdout: "out".to_string(),
            stderr: "err".to_string(),
        };
        let formatted = format_command_output(&output_with_stderr);
        assert!(formatted.contains("out"));
        assert!(formatted.contains("err"));
        assert!(formatted.contains("exit code: 1"));
    }

    // -- 渐进迁移：终端失败输出接到 `Shell` seam（SEAMS_DESIGN §3 C）--
    // 验证 exit_code != 0 且提供 shell 时，stderr 输出走
    // `Shell::classify` + `ShellError::instruction()`，给模型可执行指令
    // 而不是裸 stderr + "原样重试"。同时也验证未提供 shell 时退化为
    // 老路径，老测试零回归。

    /// 失败输出可控的 MockBackend：根据预置列表返回 (exit_code, stdout, stderr)。
    struct FailingMockBackend {
        seq: std::sync::Arc<std::sync::Mutex<Vec<(i32, String, String)>>>,
    }

    #[async_trait]
    impl TerminalBackend for FailingMockBackend {
        async fn execute_command(
            &self,
            cmd: &str,
            _timeout: Option<u64>,
            _workdir: Option<&str>,
            _bg: bool,
            _pty: bool,
        ) -> Result<CommandOutput, AgentError> {
            let mut g = self.seq.lock().unwrap();
            let (code, out, err) = g.remove(0);
            // 把命令名塞 stdout 让测试能在分支里看到原始 cmd
            let out = if out.is_empty() {
                format!("out:{cmd}")
            } else {
                out
            };
            Ok(CommandOutput { exit_code: code, stdout: out, stderr: err })
        }
        async fn read_file(
            &self,
            _path: &str,
            _offset: Option<u64>,
            _limit: Option<u64>,
        ) -> Result<String, AgentError> {
            Ok(String::new())
        }
        async fn write_file(&self, _path: &str, _content: &str) -> Result<(), AgentError> {
            Ok(())
        }
        async fn file_exists(&self, _path: &str) -> Result<bool, AgentError> {
            Ok(true)
        }
    }

    fn make_failing(seq: Vec<(i32, String, String)>) -> Arc<FailingMockBackend> {
        Arc::new(FailingMockBackend {
            seq: Arc::new(std::sync::Mutex::new(seq)),
        })
    }

    fn make_handler(
        backend: Arc<dyn TerminalBackend>,
        shell: Option<Arc<dyn Shell>>,
    ) -> TerminalHandler {
        let h = TerminalHandler::new(
            backend,
            Arc::new(std::sync::RwLock::new(None)),
            Arc::new(std::sync::Mutex::new(TerminalApprovalContext::default())),
        );
        match shell {
            Some(s) => h.with_shell(s),
            None => h,
        }
    }

    #[tokio::test]
    async fn failing_command_surfaces_structured_instruction_via_seam() {
        // 中文 Windows 上"不是内部或外部命令"——分类必须是 CommandNotFound，
        // 输出必须是结构化指令（"改用…完整路径"），不能是"原样重试"。
        let backend = make_failing(vec![(
            1,
            String::new(),
            "'foo' 不是内部或外部命令，也不是可运行的程序".into(),
        )]);
        let shell: Arc<dyn Shell> = Arc::new(hermix_core::LocalShell::default());
        let handler = make_handler(backend, Some(shell));
        let err = handler
            .execute(json!({"command": "foo"}))
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("完整路径"),
            "expected structured instruction, got: {msg}"
        );
        assert!(
            !msg.contains("再试一次") && !msg.contains("retry"),
            "must NOT echo the legacy 'retry' pattern, got: {msg}"
        );
    }

    #[tokio::test]
    async fn access_denied_maps_to_actionable_hint() {
        // 用 `echo` 类无害命令绕过 approval 拦截——接缝 classify 看的是
        // backend 返回的 stderr，不是命令名本身。
        let backend = make_failing(vec![(
            5,
            String::new(),
            "Access is denied.".into(),
        )]);
        let shell: Arc<dyn Shell> = Arc::new(hermix_core::LocalShell::default());
        let handler = make_handler(backend, Some(shell));
        let err = handler.execute(json!({"command": "echo x"})).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("权限不足") || msg.contains("受保护目录"), "got: {msg}");
    }

    #[tokio::test]
    async fn file_locked_maps_to_actionable_hint() {
        let backend = make_failing(vec![(
            32,
            String::new(),
            "The process cannot access the file because it is being used by another process."
                .into(),
        )]);
        let shell: Arc<dyn Shell> = Arc::new(hermix_core::LocalShell::default());
        let handler = make_handler(backend, Some(shell));
        let err = handler.execute(json!({"command": "echo x"})).await.unwrap_err();
        assert!(err.to_string().contains("被占用") || err.to_string().contains("新路径"));
    }

    #[tokio::test]
    async fn unknown_failure_still_returns_some_instruction_not_bare_stderr() {
        // stderr 命中不了任何已知模式——分类为 Unknown，但 instruction
        // 仍然是结构化的（"先读 stderr 定位…不要原样重复"），不是
        // 把原始 stderr 整段回灌。
        let backend = make_failing(vec![(
            99,
            String::new(),
            "some weird internal-only error message".into(),
        )]);
        let shell: Arc<dyn Shell> = Arc::new(hermix_core::LocalShell::default());
        let handler = make_handler(backend, Some(shell));
        let err = handler.execute(json!({"command": "weird"})).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("不要原样重复"), "got: {msg}");
        // 原始 stderr 仍以 "--- stderr ---" 形式附带，便于调试，
        // 但 instruction 始终在前。
        assert!(msg.contains("--- stderr ---"), "stderr section missing, got: {msg}");
        assert!(msg.find("不要原样重复").unwrap() < msg.find("--- stderr ---").unwrap());
    }

    #[tokio::test]
    async fn exit_code_zero_never_goes_through_seam() {
        // exit_code = 0 即使有 shell seam 也不该触发指令输出。
        let backend = make_failing(vec![(0, "hello".into(), String::new())]);
        let shell: Arc<dyn Shell> = Arc::new(hermix_core::LocalShell::default());
        let handler = make_handler(backend, Some(shell));
        let result = handler.execute(json!({"command": "echo hi"})).await.unwrap();
        assert_eq!(result, "hello");
    }

    #[tokio::test]
    async fn no_seam_means_legacy_bare_stderr_path_preserved() {
        // 没传 shell seam 时，exit_code != 0 走老路径：format_command_output
        // 把 stderr 直接拼进结果。这条测试是"零回归"的钉子——主进程装配
        // 接入 shell seam 前，所有现有调用点行为不变。
        let backend = make_failing(vec![(
            1,
            String::new(),
            "'foo' 不是内部或外部命令".into(),
        )]);
        let handler = make_handler(backend, None);
        let result = handler.execute(json!({"command": "foo"})).await.unwrap();
        assert!(result.contains("不是内部或外部命令"));
        assert!(result.contains("[exit code: 1]"));
    }

    // ----- §5.5 第 5 层接线：来源感知审批 -----

    /// Local 下 confirm-pattern 命中的命令按原策略走 RequiresConfirmation。
    #[tokio::test]
    async fn local_origin_prompts_on_confirm_pattern() {
        let backend = Arc::new(MockBackend {
            captured_workdir: std::sync::Arc::new(std::sync::Mutex::new(None)),
        });
        let handler = make_handler(backend, None);
        // `del` 命中内置 CONFIRM_PATTERNS → Local 下必须 prompt
        let err = handler
            .execute_for_origin(json!({"command": "del x.txt"}), &Origin::Local)
            .await
            .expect_err("confirm 命中应触发 RequiresConfirmation");
        let msg = err.to_string();
        assert!(
            msg.contains("confirmation") || msg.contains("确认"),
            "got: {msg}"
        );
    }

    /// PairedDevice 下同一命令也必须 prompt——即使 Local 已被 session 批准过，
    /// 远程不能继承降级（对策 T5 远程越权）。
    #[tokio::test]
    async fn paired_device_cannot_inherit_local_session_approval() {
        let backend = Arc::new(MockBackend {
            captured_workdir: std::sync::Arc::new(std::sync::Mutex::new(None)),
        });
        let handler = make_handler(backend, None);
        // 先用 Local 批准 `del` 家族
        handler.approval.approve_for_session("del x.txt");

        // PairedDevice 下即使 Local 已批准过，也必须 prompt
        let err = handler
            .execute_for_origin(
                json!({"command": "del x.txt"}),
                &Origin::PairedDevice { device_id: "d1".into() },
            )
            .await
            .expect_err("PairedDevice 下必须拒绝 session 降级");
        let msg = err.to_string();
        assert!(
            msg.contains("confirmation") || msg.contains("确认"),
            "got: {msg}"
        );
    }

    /// PairedDevice 下只读命令放行——远程看日志没问题。
    #[tokio::test]
    async fn paired_device_readonly_command_runs() {
        let backend = Arc::new(MockBackend {
            captured_workdir: std::sync::Arc::new(std::sync::Mutex::new(None)),
        });
        let handler = make_handler(backend, None);
        // `Get-ComputerInfo` 在 READONLY_PATTERNS → 任何非 Unpaired 都放行
        let result = handler
            .execute_for_origin(
                json!({"command": "Get-ComputerInfo"}),
                &Origin::PairedDevice { device_id: "d1".into() },
            )
            .await
            .expect("只读命令 PairedDevice 下应放行");
        assert!(!result.is_empty());
    }

    /// Unpaired 纵深防御：灾难命令 Denied，只读也走 confirm（即被拒）。
    #[tokio::test]
    async fn unpaired_origin_denies_destructive_command() {
        let backend = Arc::new(MockBackend {
            captured_workdir: std::sync::Arc::new(std::sync::Mutex::new(None)),
        });
        let handler = make_handler(backend, None);
        let err = handler
            .execute_for_origin(
                json!({"command": "rm -rf /"}),
                &Origin::Unpaired,
            )
            .await
            .expect_err("灾难命令在 Unpaired 下必须 Denied");
        let msg = err.to_string();
        assert!(
            msg.contains("denied") || msg.contains("deny") || msg.contains("Denied"),
            "got: {msg}"
        );
    }

    // ----- §5.5 第 5 层接线：execute_with_context 路径服从 ctx.origin -----

    /// agent loop 路径（dispatch_with_context → execute_with_context）：
    /// PairedDevice ctx 触发 confirm，Local session 批准不能继承。
    #[tokio::test]
    async fn execute_with_context_paired_device_prompts() {
        let backend = Arc::new(MockBackend {
            captured_workdir: std::sync::Arc::new(std::sync::Mutex::new(None)),
        });
        let handler = make_handler(backend, None);
        // Local 已 session-批准过 `del` 家族
        handler.approval.approve_for_session("del x.txt");

        let ctx = ToolCallContext {
            origin: Origin::PairedDevice { device_id: "d1".into() },
            ..ToolCallContext::for_session("s1")
        };
        let err = handler
            .execute_with_context(json!({"command": "del x.txt"}), &ctx)
            .await
            .expect_err("PairedDevice ctx 下必须 prompt，不能继承 Local 批准");
        let msg = err.to_string();
        assert!(
            msg.contains("confirmation") || msg.contains("确认"),
            "got: {msg}"
        );
    }

    /// agent loop 路径：Unpaired ctx 一律 Denied，纵深防御。
    #[tokio::test]
    async fn execute_with_context_unpaired_denies_destructive() {
        let backend = Arc::new(MockBackend {
            captured_workdir: std::sync::Arc::new(std::sync::Mutex::new(None)),
        });
        let handler = make_handler(backend, None);
        let ctx = ToolCallContext {
            origin: Origin::Unpaired,
            ..ToolCallContext::for_session("s1")
        };
        let err = handler
            .execute_with_context(json!({"command": "rm -rf /"}), &ctx)
            .await
            .expect_err("Unpaired ctx 下灾难命令必须 Denied");
        let msg = err.to_string();
        assert!(
            msg.contains("denied") || msg.contains("Denied"),
            "got: {msg}"
        );
    }

    /// Local ctx 下只读命令放行——证明 ctx.origin 不影响默认桌面路径。
    #[tokio::test]
    async fn execute_with_context_local_runs_readonly() {
        let backend = Arc::new(MockBackend {
            captured_workdir: std::sync::Arc::new(std::sync::Mutex::new(None)),
        });
        let handler = make_handler(backend, None);
        let ctx = ToolCallContext {
            origin: Origin::Local,
            ..ToolCallContext::for_session("s1")
        };
        let result = handler
            .execute_with_context(json!({"command": "Get-ComputerInfo"}), &ctx)
            .await
            .expect("Local ctx 下只读命令应放行");
        assert!(!result.is_empty());
    }

    // -------------------------------------------------------------------
    // 持久终端会话（H3）：参数路由 / 页脚格式化 / 冲突校验。
    // 真实 ConPTY 行为在 hermix-environments 的集成测试里验证
    // （hermix-tools 不依赖 hermix-environments，依赖方向不允许 E2E）。
    // -------------------------------------------------------------------

    /// 记录会话调用的 mock backend。
    struct MockSessionBackend {
        calls: std::sync::Arc<std::sync::Mutex<Vec<(String, String, bool)>>>,
    }
    #[async_trait]
    impl TerminalBackend for MockSessionBackend {
        async fn execute_command(
            &self,
            _cmd: &str,
            _timeout: Option<u64>,
            _workdir: Option<&str>,
            _bg: bool,
            _pty: bool,
        ) -> Result<CommandOutput, AgentError> {
            unreachable!("session mode must not reach execute_command")
        }
        async fn read_file(
            &self,
            _: &str,
            _: Option<u64>,
            _: Option<u64>,
        ) -> Result<String, AgentError> {
            Ok(String::new())
        }
        async fn write_file(&self, _: &str, _: &str) -> Result<(), AgentError> {
            Ok(())
        }
        async fn file_exists(&self, _: &str) -> Result<bool, AgentError> {
            Ok(true)
        }
        async fn pty_create_session(
            &self,
            _workdir: Option<&str>,
        ) -> Result<String, AgentError> {
            Ok("pty-7".to_string())
        }
        async fn pty_send_session(
            &self,
            session_id: &str,
            input: &str,
            submit: bool,
            _budget: Option<u64>,
        ) -> Result<PtySessionOutput, AgentError> {
            self.calls
                .lock()
                .unwrap()
                .push((session_id.to_string(), input.to_string(), submit));
            Ok(PtySessionOutput {
                session_id: session_id.to_string(),
                text: format!("ran: {input}"),
                busy: false,
                alive: true,
            })
        }
        async fn pty_read_session(
            &self,
            session_id: &str,
            _budget: Option<u64>,
        ) -> Result<PtySessionOutput, AgentError> {
            Ok(PtySessionOutput {
                session_id: session_id.to_string(),
                text: String::new(),
                busy: false,
                alive: true,
            })
        }
    }

    fn make_session_handler(
    ) -> (
        TerminalHandler,
        std::sync::Arc<std::sync::Mutex<Vec<(String, String, bool)>>>,
    ) {
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let backend = std::sync::Arc::new(MockSessionBackend { calls: calls.clone() });
        let handler = TerminalHandler::new(
            backend,
            std::sync::Arc::new(std::sync::RwLock::new(None)),
            std::sync::Arc::new(std::sync::Mutex::new(
                TerminalApprovalContext::default(),
            )),
        );
        (handler, calls)
    }

    #[tokio::test]
    async fn test_persistent_create_returns_session_id_footer() {
        let (handler, _calls) = make_session_handler();
        let result = handler
            .execute(json!({"command": "echo hi", "persistent": true}))
            .await
            .expect("create should succeed");
        assert!(result.contains("ran: echo hi"));
        assert!(result.contains("[persistent session created: id=pty-7]"));
    }

    #[tokio::test]
    async fn test_session_id_continues_and_records_submit() {
        let (handler, calls) = make_session_handler();
        let result = handler
            .execute(json!({"command": "again", "session_id": "pty-7", "submit": false}))
            .await
            .expect("continue should succeed");
        assert!(result.contains("[session: pty-7]"));
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "pty-7");
        assert_eq!(calls[0].1, "again");
        assert!(!calls[0].2, "submit=false must reach the backend");
    }

    #[tokio::test]
    async fn test_session_poll_with_empty_command_reads_without_submit() {
        let (handler, calls) = make_session_handler();
        let result = handler
            .execute(json!({"command": "", "session_id": "pty-7", "submit": false}))
            .await
            .expect("poll should succeed");
        assert!(result.contains("no new output"), "got: {result}");
        assert!(result.contains("[session: pty-7]"));
        assert!(calls.lock().unwrap().is_empty(), "poll must not write input");
    }

    #[tokio::test]
    async fn test_session_conflicts_rejected() {
        let (handler, _calls) = make_session_handler();
        let err = handler
            .execute(json!({"command": "x", "persistent": true, "background": true}))
            .await
            .expect_err("background + persistent must be rejected");
        assert!(err.to_string().contains("background"));

        let err = handler
            .execute(json!({"command": "x", "session_id": "pty-7", "pty": true}))
            .await
            .expect_err("pty + session_id must be rejected");
        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[tokio::test]
    async fn test_session_footer_busy_and_exited_hints() {
        // busy=true → 轮询指引；alive=false → 退出提示。（直接测格式化函数）
        let out = PtySessionOutput {
            session_id: "pty-9".to_string(),
            text: "partial".to_string(),
            busy: true,
            alive: false,
        };
        let s = format_session_output(&out, false);
        assert!(s.contains("still producing output"));
        assert!(s.contains("session has exited"));
        assert!(s.contains("[session: pty-9]"));
    }

    #[tokio::test]
    async fn test_session_unsupported_backend_error_propagates() {
        // MockBackend（无会话能力，走 trait 默认实现）→ 明确报错而非 panic。
        let (backend, _cap) = make_mock();
        let handler = TerminalHandler::new(
            backend,
            std::sync::Arc::new(std::sync::RwLock::new(None)),
            std::sync::Arc::new(std::sync::Mutex::new(
                TerminalApprovalContext::default(),
            )),
        );
        let err = handler
            .execute(json!({"command": "x", "persistent": true}))
            .await
            .expect_err("default trait impl must error");
        assert!(err.to_string().contains("not supported"));
    }
}
