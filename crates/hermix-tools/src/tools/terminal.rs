//! Terminal tools: terminal command execution and process management

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_core::{
    path_security::{forbidden_win32_prefix, strip_win32_long_path},
    tool_schema, CommandOutput, JsonSchema, TerminalBackend, ToolError, ToolHandler, ToolSchema,
};

use crate::approval::{ApprovalDecision, ApprovalManager};

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
}

impl TerminalHandler {
    /// Construct a handler. Pass a shared `terminal_workspace` handle from
    /// [`ToolRegistry::terminal_workspace_handle`] so the agent service can
    /// update the per-turn workspace without holding a direct reference to
    /// this handler. Tests that don't care about workspace semantics can
    /// pass `Arc::new(RwLock::new(None))`.
    pub fn new(
        backend: std::sync::Arc<dyn TerminalBackend>,
        terminal_workspace: std::sync::Arc<std::sync::RwLock<Option<std::path::PathBuf>>>,
    ) -> Self {
        Self {
            backend,
            approval: ApprovalManager::new(),
            default_workspace: terminal_workspace,
        }
    }

    /// Update the default workspace for the next and subsequent tool calls.
    /// Cheap (RwLock write). Pass `None` to clear.
    pub fn set_default_workspace(&self, workspace: Option<std::path::PathBuf>) {
        if let Ok(mut guard) = self.default_workspace.write() {
            *guard = workspace;
        }
    }
}

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
                } else {
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
            Err(e) => Err(ToolError::ExecutionFailed(e.to_string())),
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
        let handler = TerminalHandler::new(backend, ws_handle.clone());

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
