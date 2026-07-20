//! Terminal tools: terminal command execution and process management

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};
use std::sync::Arc;

use hermix_core::{
    path_security::{forbidden_win32_prefix, detect_shell_metacharacters, strip_win32_long_path},
    tool_schema, ApprovalOutcome, CommandOutput, JsonSchema, TerminalBackend, ToolError,
    ToolHandler, ToolSchema, traits::RiskLevel,
};

use crate::approval::{ApprovalDecision, ApprovalManager, TerminalApprovalContext};
use crate::ToolRegistry;

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the `terminal` tool with the registry (P1.2 migration).
///
/// Pass the same `terminal_workspace` handle used by other terminal-touching
/// tools (and the agent service) so per-turn workspace updates propagate
/// here too. Test-only callers without a registry workspace can pass
/// `Arc::new(RwLock::new(None))`.
///
/// Pass the registry's shared `terminal_approval_ctx` handle
/// (`registry.terminal_approval_ctx_handle()`) so `LocalAgentService`
/// can wire the WS approval hook per turn without re-registering the
/// tool. The handle starts as `default()` (both fields `None`); the
/// tool gracefully falls back to the legacy `HERMIX_CONFIRM=1` text
/// flow while it's still `None` (unit tests, headless callers).
pub fn register(
    registry: &ToolRegistry,
    backend: Arc<dyn TerminalBackend>,
    terminal_workspace: Arc<std::sync::RwLock<Option<std::path::PathBuf>>>,
    terminal_approval_ctx: Arc<std::sync::Mutex<TerminalApprovalContext>>,
) {
    let handler = Arc::new(TerminalHandler::new(
        backend,
        terminal_workspace,
        terminal_approval_ctx,
    ));
    let schema = handler.schema();
    registry.register_with_risk(
        "terminal",
        "terminal",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        true,
        "Execute a shell command on the user's system. You have full access to the filesystem (including C:\\ and other drives). Per-command risk is decided by an internal ApprovalManager; this tool-level risk is Caution (commands may have side effects). If a command is blocked by security policy, the error message will tell you how to retry with HERMIX_CONFIRM=1 prefix.",
        "💻",
        None,
        RiskLevel::Caution,
        false,
    );
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
    /// Per-request approval hook (2026-07-17). When populated,
    /// `RequiresConfirmation` is surfaced as a `request_approval` event
    /// on the active WS channel (so the desktop's `InlineApprovalCard`
    /// renders instead of bouncing the user through LLM text). When
    /// empty (default `TerminalApprovalContext`), the legacy
    /// "reply `HERMIX_CONFIRM=1`" flow runs — backward-compatible.
    approval_ctx: Arc<std::sync::Mutex<TerminalApprovalContext>>,
}

impl TerminalHandler {
    /// Construct a handler. Pass a shared `terminal_workspace` handle from
    /// [`ToolRegistry::terminal_workspace_handle`] so the agent service can
    /// update the per-turn workspace without holding a direct reference to
    /// this handler. Tests that don't care about workspace semantics can
    /// pass `Arc::new(RwLock::new(None))`.
    ///
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
        }
    }

    /// Update the default workspace for the next and subsequent tool calls.
    /// Cheap (RwLock write). Pass `None` to clear.
    pub fn set_default_workspace(&self, workspace: Option<std::path::PathBuf>) {
        if let Ok(mut guard) = self.default_workspace.write() {
            *guard = workspace;
        }
    }

    /// 2026-07-17 — When the agent service wired a `TerminalApprovalContext`
    /// for this turn (it captures the active WS `ApprovalRegistry` + the
    /// `on_event` channel), emit a `request_approval` event on the WS
    /// stream and `.await` the user's `approved` / `denied` click via
    /// the registry's oneshot receiver.
    ///
    /// Returns:
    /// - `Some(ApprovalOutcome::Approved)` — user approved; caller
    ///   should fall through and execute. We also `approve_for_session`
    ///   so the same command string is implicit-allow for the rest of
    ///   the session (mirrors the user clicking "Allow for this
    ///   session" in the InlineApprovalCard).
    /// - `Some(other)` — user denied / timed out / channel closed;
    ///   caller should error out.
    /// - `None`        — no WS context wired (default state); caller
    ///   falls back to the legacy `HERMIX_CONFIRM=1` text flow so
    ///   headless / unit-test callers keep working.
    async fn request_ws_approval(&self, effective_cmd: &str) -> Option<ApprovalOutcome> {
        // Snapshot the context so we don't hold the mutex across
        // `.await` (the requester/on_event clones are cheap; the
        // Mutex guard would block any `set_terminal_approval_ctx`
        // call from the agent service during the await).
        let (session_id, requester, on_event) = {
            let ctx = match self.approval_ctx.lock() {
                Ok(g) => g,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "approval_ctx lock poisoned; falling back to text flow"
                    );
                    return None;
                }
            };
            if !ctx.is_wired() {
                return None;
            }
            (
                ctx.session_id.clone(),
                ctx.requester(),
                ctx.on_event(),
            )
        };
        let requester = requester?; // unreachable (is_wired checked) but makes the match arms tidy
        let on_event = on_event?;

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
        //
        //   - `\\.\` (Win32 device namespace, e.g. `\\.\COM1`,
        //     `\\.\PhysicalDrive0`)
        //   - `\\?\UNC\...` (Win32 UNC namespace) — when an agent
        //     actually needs to reach a UNC share it should use the
        //     normal `\\server\share\...` form; the `\\?\UNC\` form
        //     is the long-path escape hatch and adds no value
        //     over standard syntax for typical agent workloads.
        //
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

        // Defense-in-depth: detect shell metacharacters that could be
        // used for command chaining or injection. This catches generic
        // injection vectors that might not be in the approval pattern
        // list. Commands with metacharacters require explicit user
        // confirmation (cannot be bypassed with HERMIX_CONFIRM=1).
        if let Some(metachar_desc) = detect_shell_metacharacters(command) {
            tracing::warn!(
                command,
                metachar_desc,
                "terminal command contains shell metacharacters"
            );
            // Always require confirmation for metacharacter commands,
            // even with HERMIX_CONFIRM=1 marker. This prevents the
            // marker from being used to bypass this safety check.
            if let Some(decision) = self
                .request_ws_approval(&format!(
                    "{} (contains {})", command, metachar_desc
                ))
                .await
            {
                if !matches!(decision, ApprovalOutcome::Approved) {
                    let reason = match decision {
                        ApprovalOutcome::DeniedByUser => "denied by user",
                        ApprovalOutcome::TimedOut { timeout_secs } => {
                            &format!("approval timed out after {timeout_secs}s")
                        }
                        ApprovalOutcome::ChannelClosed => "connection closed before response",
                        ApprovalOutcome::Approved => unreachable!(),
                    };
                    return Err(ToolError::ExecutionFailed(format!(
                        "Command {reason}: {}",
                        command
                    )));
                }
            } else {
                // No WS context — require HERMIX_CONFIRM=1 for metachar commands
                if command.strip_prefix("HERMIX_CONFIRM=1 ").is_none() {
                    return Err(ToolError::ExecutionFailed(format!(
                        "Command contains shell metacharacters ({}): {}\n\n\
                         This command requires explicit user confirmation.\n\
                         Tell the user what this command does and ask for permission. \
                         If they approve, retry with:\n  HERMIX_CONFIRM=1 {}",
                        metachar_desc, command, command
                    )));
                }
                // HERMIX_CONFIRM=1 is present — proceed with metachar command
                tracing::info!(
                    command = %command,
                    metachar_desc,
                    "metachar command confirmed by user via HERMIX_CONFIRM marker"
                );
            }
        }

        // Check for the user-override marker. When the LLM confirms
        // with the user and retries with HERMIX_CONFIRM=1, we skip the
        // confirmation-pattern check to allow the command through.
        let stripped = command.strip_prefix("HERMIX_CONFIRM=1 ");
        let effective_cmd = stripped.unwrap_or(command);

        match self.approval.check_approval(effective_cmd) {
            ApprovalDecision::Denied => {
                return Err(ToolError::ExecutionFailed(format!(
                    "Command denied by security policy: {}",
                    effective_cmd
                )));
            }
            ApprovalDecision::RequiresConfirmation => {
                if stripped.is_some() {
                    // LLM already confirmed with the user and retried
                    // with the HERMIX_CONFIRM=1 marker — proceed.
                    tracing::info!(
                        command = %effective_cmd,
                        "command confirmed by user via HERMIX_CONFIRM marker"
                    );
                } else if let Some(decision) = self
                    .request_ws_approval(effective_cmd)
                    .await
                {
                    // 2026-07-18: WS path. The agent service wired
                    // `TerminalApprovalContext` for this turn; we emit
                    // a `request_approval` event and `.await` the
                    // user's verdict. `Approved` ⇒ fall through and
                    // execute. Any other outcome (user denied, timed
                    // out, channel closed) ⇒ error out below so the
                    // LLM can reformulate.
                    if !matches!(decision, ApprovalOutcome::Approved) {
                        let reason = match decision {
                            ApprovalOutcome::DeniedByUser => "denied by user",
                            ApprovalOutcome::TimedOut { timeout_secs } => {
                                &format!("approval timed out after {timeout_secs}s")
                            }
                            ApprovalOutcome::ChannelClosed => "connection closed before response",
                            ApprovalOutcome::Approved => unreachable!(),
                        };
                        return Err(ToolError::ExecutionFailed(format!(
                            "Command {reason}: {}",
                            effective_cmd
                        )));
                    }
                    tracing::info!(
                        command = %effective_cmd,
                        "command confirmed by user via WS approval card"
                    );
                } else {
                    // No WS context wired (headless test, CLI run,
                    // or unit test that bypassed the agent service):
                    // fall back to the legacy text flow so the LLM
                    // can still bounce the user through `HERMIX_CONFIRM=1`.
                    return Err(ToolError::ExecutionFailed(format!(
                        "Command requires user confirmation: {}\n\n\
                         Tell the user what this command does and ask for permission. \
                         If they approve, retry with:\n  HERMIX_CONFIRM=1 {}",
                        effective_cmd, effective_cmd
                    )));
                }
            }
            ApprovalDecision::Approved => {}
        }

        let timeout = params.get("timeout").and_then(|v| v.as_u64());

        // Resolve effective workdir:
        // 1. explicit `workdir` param from the LLM (highest priority)
        // 2. per-turn default_workspace (set by LocalAgentService)
        // 3. None (let the backend fall back to its own workspace_root or CWD)
        let explicit_workdir = params.get("workdir").and_then(|v| v.as_str());
        let workdir = explicit_workdir
            .map(|s| strip_win32_long_path(s))
            .or_else(|| {
                self.default_workspace
                    .read()
                    .ok()
                    .and_then(|g| g.as_ref().map(|p| strip_win32_long_path(&p.to_string_lossy())))
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

        let background = params
            .get("background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let pty = params.get("pty").and_then(|v| v.as_bool()).unwrap_or(false);

        match self
            .backend
            .execute_command(command, timeout, workdir.as_deref(), background, pty)
            .await
        {
            Ok(output) => Ok(format_command_output(&output)),
            Err(e) => Err(ToolError::from_message(e.to_string())),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "command".into(),
            json!({
                "type": "string",
                "description": "The command to execute"
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
                "description": "Run command in PTY mode for interactive programs (default: false)",
                "default": false
            }),
        );

        tool_schema(
            "terminal",
            "Execute a terminal command. Returns stdout, stderr, and exit code.\n\
             \n\
             ## When to reach for this tool\n\
             `terminal` is the right tool whenever the user wants to **see, list, count, search, or inspect** something on the system: directory listings (`dir` / `ls`), file contents (`type` / `cat` / `head`), running processes (`tasklist` / `ps`), system info, or any one-off shell pipeline. \
             It is also the right tool for **doing** things on the system: running a build, installing a package, invoking a CLI the user names. \
             Prefer this over `computer_use.screenshot` whenever a shell command can answer the question — `screenshot` is a 50× bigger payload for 1/50th the precision, and shell output is searchable, copy-pasteable, and diffable.",
            JsonSchema::object(props, vec!["command".into()]),
        )
    }
}

impl TerminalHandler {
    // The `forbidden_path_prefix` private helper used to live here;
    // it was extracted to `hermix_core::path_security` during the
    // 2026-06-07 Windows-first hardening pass so memory / skills /
    // cron / credentials tools can all share the same check.
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

// ---------------------------------------------------------------------------
// ProcessHandler
// ---------------------------------------------------------------------------

/// Backend trait for process management operations.
#[async_trait]
pub trait ProcessBackend: Send + Sync {
    /// List all background processes.
    async fn list_processes(&self) -> Result<String, ToolError>;
    /// Poll a process for output (non-blocking).
    async fn poll_process(&self, pid: &str) -> Result<String, ToolError>;
    /// Wait for a process to complete and return its full output.
    async fn wait_process(&self, pid: &str, timeout: Option<u64>) -> Result<String, ToolError>;
    /// Kill a background process.
    async fn kill_process(&self, pid: &str) -> Result<String, ToolError>;
    /// Write stdin to a running process.
    async fn write_stdin(&self, pid: &str, data: &str) -> Result<String, ToolError>;
    /// Submit input to a process and get output.
    async fn submit_process(&self, pid: &str, input: &str) -> Result<String, ToolError>;
    /// Close stdin of a process.
    async fn close_process(&self, pid: &str) -> Result<String, ToolError>;
}

/// Tool for managing background processes.
pub struct ProcessHandler {
    backend: std::sync::Arc<dyn ProcessBackend>,
}

impl ProcessHandler {
    pub fn new(backend: std::sync::Arc<dyn ProcessBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for ProcessHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'action' parameter".into()))?;

        match action {
            "list" => self.backend.list_processes().await,
            "poll" => {
                let pid = params
                    .get("pid")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'pid' parameter".into()))?;
                self.backend.poll_process(pid).await
            }
            "wait" => {
                let pid = params
                    .get("pid")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'pid' parameter".into()))?;
                let timeout = params.get("timeout").and_then(|v| v.as_u64());
                self.backend.wait_process(pid, timeout).await
            }
            "kill" => {
                let pid = params
                    .get("pid")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'pid' parameter".into()))?;
                self.backend.kill_process(pid).await
            }
            "write" => {
                let pid = params
                    .get("pid")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'pid' parameter".into()))?;
                let data = params
                    .get("data")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'data' parameter".into()))?;
                self.backend.write_stdin(pid, data).await
            }
            "submit" => {
                let pid = params
                    .get("pid")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'pid' parameter".into()))?;
                let input = params
                    .get("input")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'input' parameter".into()))?;
                self.backend.submit_process(pid, input).await
            }
            "close" => {
                let pid = params
                    .get("pid")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'pid' parameter".into()))?;
                self.backend.close_process(pid).await
            }
            other => Err(ToolError::InvalidParams(format!(
                "Unknown action: {}",
                other
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "description": "Action to perform: list, poll, wait, kill, write, submit, close",
                "enum": ["list", "poll", "wait", "kill", "write", "submit", "close"]
            }),
        );
        props.insert(
            "pid".into(),
            json!({
                "type": "string",
                "description": "Process ID (required for all actions except 'list')"
            }),
        );
        props.insert(
            "timeout".into(),
            json!({
                "type": "integer",
                "description": "Timeout in milliseconds for 'wait' action"
            }),
        );
        props.insert(
            "data".into(),
            json!({
                "type": "string",
                "description": "Data to write to process stdin (for 'write' action)"
            }),
        );
        props.insert(
            "input".into(),
            json!({
                "type": "string",
                "description": "Input to submit to the process (for 'submit' action)"
            }),
        );

        tool_schema(
            "process",
            "Manage background processes: list, poll output, wait for completion, kill, write to stdin, or close.",
            JsonSchema::object(props, vec!["action".into()]),
        )
    }
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

    // ---------- WS approval path (2026-07-17, "Caution tool needs UI prompts") ----------

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
            "fallback message must instruct LLM to ask user (got: {msg})",
        );
        assert!(
            msg.contains("HERMIX_CONFIRM=1"),
            "fallback message must mention HERMIX_CONFIRM=1 marker (got: {msg})",
        );
    }

    #[tokio::test]
    async fn hermix_confirm_marker_skips_ws_path() {
        // Even with WS wired, the `HERMIX_CONFIRM=1` prefix must
        // short-circuit straight to execution (LLM-bounced approval
        // path stays valid for backward compat).
        let (approval_ctx, reg, _captured_event) = wire_approval_ctx();
        let (backend, _cap) = make_mock();
        let handler = TerminalHandler::new(
            backend,
            std::sync::Arc::new(std::sync::RwLock::new(None)),
            approval_ctx,
        );

        let result = handler
            .execute(json!({
                "command": "HERMIX_CONFIRM=1 sudo apt install ripgrep"
            }))
            .await;
        result.expect(
            "HERMIX_CONFIRM=1 marker must let the command through without prompting",
        );
        // No pending approval should have been registered — the
        // marker means the user already said yes.
        assert!(
            reg.lock().unwrap().is_empty(),
            "HERMIX_CONFIRM=1 must skip the WS request entirely",
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
}
