//! Code execution tool: run Python scripts with hermix_tools RPC
//!
//! Registered as `RiskLevel::Caution` (not Dangerous) — the dispatch-level
//! L3 gate lets it through without a WS popup. Instead, the tool has its
//! own internal [`CodeExecutionGuard`] that inspects the code string for
//! dangerous patterns before execution, exactly like how [`TerminalHandler`]
//! uses [`ApprovalManager`] for shell commands.
//!
//! This avoids unnecessary approval popups for simple calculations while
//! still blocking / flagging destructive patterns.

use async_trait::async_trait;
use indexmap::IndexMap;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::LazyLock;

use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema, traits::RiskLevel};

use crate::ToolRegistry;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register this tool with the registry (P1.2 migration).
pub fn register(registry: &ToolRegistry, backend: Arc<dyn CodeExecutionBackend>) {
    let handler = Arc::new(ExecuteCodeHandler::new(backend));
    let schema = handler.schema();
    registry.register_with_risk(
        "code_execution",
        "code_execution",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        true,
        "Execute code in a local interpreter (Python etc). Inspects code for dangerous patterns before execution.",
        "🖥️",
        None,
        RiskLevel::Caution,
        false,
    );
}

// ---------------------------------------------------------------------------
// CodeExecutionBackend trait
// ---------------------------------------------------------------------------

/// Backend for code execution operations.
#[async_trait]
pub trait CodeExecutionBackend: Send + Sync {
    /// Execute code and return the output.
    async fn execute(
        &self,
        code: &str,
        language: Option<&str>,
        timeout: Option<u64>,
    ) -> Result<String, ToolError>;

    /// Languages this backend can serve **right now** on this host.
    ///
    /// Used by [`ExecuteCodeHandler::schema`] to populate the `language`
    /// enum (and the description) with what's actually available, so
    /// the LLM doesn't keep calling `language="typescript"` on a host
    /// without `npx`. Synchronous because it's a cheap probe (PATH
    /// only); keeping it sync avoids forcing the trait into a
    /// second async layer.
    fn available_languages(&self) -> Vec<String> {
        // Default: trust the static catalog. Real backends
        // (e.g. `LocalCodeExecutionBackend`) override this with live
        // PATH probes so the LLM sees the truth.
        vec!["python".to_string()]
    }
}

// ---------------------------------------------------------------------------
// CodeExecutionGuard — internal security for code strings
// ---------------------------------------------------------------------------

/// Decision returned by [`CodeExecutionGuard::check`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeExecutionDecision {
    /// Code looks safe — proceed.
    Approved,
    /// Code contains a pattern that requires explicit confirmation.
    RequiresConfirmation,
    /// Code is outright denied (destructive patterns).
    Denied,
}

/// Patterns that are outright denied (never allowed, never overridable).
static DENIED_CODE_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // os.system/popen — direct shell execution from Python
        Regex::new(r"(?i)\bos\.system\s*\(").unwrap(),
        Regex::new(r"(?i)\bos\.popen\s*\(").unwrap(),
        Regex::new(r"(?i)\bos\.execl\b").unwrap(),
        Regex::new(r"(?i)\bos\.execv\b").unwrap(),
        // subprocess module — spawning arbitrary processes
        Regex::new(r"(?i)\bsubprocess\.(run|call|Popen|check_output|check_call)\s*\(").unwrap(),
        Regex::new(r"(?i)from\s+subprocess\s+import").unwrap(),
        // shutil.rmtree — recursive deletion
        Regex::new(r"(?i)\bshutil\.rmtree\s*\(").unwrap(),
        // os.remove / os.unlink — file deletion
        Regex::new(r"(?i)\bos\.(remove|unlink)\s*\(").unwrap(),
        // os.rmdir — directory removal
        Regex::new(r"(?i)\bos\.rmdir\s*\(").unwrap(),
        // os.kill / os.killpg — process signalling
        Regex::new(r"(?i)\bos\.(kill|killpg)\s*\(").unwrap(),
        // os.chmod / os.chown / os.setuid — permission changes
        Regex::new(r"(?i)\bos\.(chmod|chown|setuid|setgid)\s*\(").unwrap(),
        // Node.js child_process — the `(` is for `require('child_process')` syntax
        Regex::new(r#"(?i)(require|import)\s*\(?\s*['"]child_process['"]"#).unwrap(),
        Regex::new(r#"(?i)(require|import)\s*\(?\s*['"]fs['"]"#).unwrap(),
        // Python pathlib destructive methods — only catches obvious patterns
        Regex::new(r"(?i)\.(unlink|rmdir|symlink_to)\s*\(").unwrap(),
        // os.walk combined with os.remove is dangerous
        Regex::new(r"(?i)\bos\.walk\s*\(").unwrap(),
    ]
});

/// Patterns that require confirmation (may be legitimate but risky).
static CONFIRM_CODE_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // eval / exec — arbitrary code execution
        Regex::new(r"(?i)\beval\s*\(").unwrap(),
        Regex::new(r"(?i)\bexec\s*\(").unwrap(),
        Regex::new(r"(?i)\bcompile\s*\(").unwrap(),
        // __import__ — dynamic imports
        Regex::new(r"(?i)__import__\s*\(").unwrap(),
        // Writing to files (legitimate but should be noted)
        Regex::new(r#"(?i)open\s*\(.*['"](w|a)['"]"#).unwrap(),
        Regex::new(r"(?i)\.write_text\s*\(").unwrap(),
        Regex::new(r"(?i)\.write_bytes\s*\(").unwrap(),
        // Network access
        Regex::new(r"(?i)import\s+urllib").unwrap(),
        Regex::new(r"(?i)from\s+urllib\s+import").unwrap(),
        Regex::new(r"(?i)(import|from)\s+.*\brequests\b").unwrap(),
        Regex::new(r"(?i)(import|from)\s+.*\bsocket\b").unwrap(),
        Regex::new(r"(?i)import\s+paramiko").unwrap(),
        Regex::new(r"(?i)import\s+http\.(client|server)").unwrap(),
        // Database operations
        Regex::new(r"(?i)import\s+sqlite3").unwrap(),
        Regex::new(r"(?i)(import|from)\s+.*\bpsycopg2\b").unwrap(),
        Regex::new(r"(?i)(import|from)\s+.*\bpymongo\b").unwrap(),
        // Threading / multiprocessing (can be used for resource abuse)
        Regex::new(r"(?i)import\s+threading").unwrap(),
        Regex::new(r"(?i)import\s+multiprocessing").unwrap(),
        // Ctypes — can call arbitrary native code
        Regex::new(r"(?i)import\s+ctypes").unwrap(),
        Regex::new(r"(?i)from\s+ctypes\s+import").unwrap(),
        // os module operations
        Regex::new(r"(?i)\bos\.(makedirs|rename|replace|symlink|link)\s*\(").unwrap(),
        // shutil operations other than rmtree
        Regex::new(r"(?i)\bshutil\.(move|copy|copytree|make_archive)\s*\(").unwrap(),
    ]
});

/// Internal security guard for code execution. Inspects code strings for
/// dangerous patterns before dispatch, mirroring [`ApprovalManager`] for
/// the `terminal` tool.
pub struct CodeExecutionGuard;

impl CodeExecutionGuard {
    /// Check `code` against built-in denied and confirmation patterns.
    ///
    /// Returns [`CodeExecutionDecision::Denied`] if a destructive pattern
    /// is found, [`CodeExecutionDecision::RequiresConfirmation`] if a
    /// risky but possibly legitimate pattern is found, or
    /// [`CodeExecutionDecision::Approved`] if the code looks safe.
    pub fn check(code: &str) -> CodeExecutionDecision {
        // 1. Denied patterns — destructive operations that should
        //    never run outside an explicit user override.
        for re in DENIED_CODE_PATTERNS.iter() {
            if re.is_match(code) {
                return CodeExecutionDecision::Denied;
            }
        }

        // 2. Confirm patterns — flag for logging/warning but allow
        //    execution to proceed. The LLM receives the output
        //    regardless; this is informational (like the terminal
        //    tool's `RequiresConfirmation` which logs a warning and
        //    continues).
        for re in CONFIRM_CODE_PATTERNS.iter() {
            if re.is_match(code) {
                return CodeExecutionDecision::RequiresConfirmation;
            }
        }

        CodeExecutionDecision::Approved
    }
}

// ---------------------------------------------------------------------------
// ExecuteCodeHandler
// ---------------------------------------------------------------------------

/// Tool for executing code in a local interpreter.
pub struct ExecuteCodeHandler {
    backend: Arc<dyn CodeExecutionBackend>,
}

impl ExecuteCodeHandler {
    pub fn new(backend: Arc<dyn CodeExecutionBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for ExecuteCodeHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let code = params
            .get("code")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'code' parameter".into()))?;

        let language = params.get("language").and_then(|v| v.as_str());
        let timeout = params.get("timeout").and_then(|v| v.as_u64());

        // Internal security check — mirrors TerminalHandler's
        // ApprovalManager.check_approval().
        match CodeExecutionGuard::check(code) {
            CodeExecutionDecision::Denied => {
                return Err(ToolError::ExecutionFailed(
                    "Code denied by security policy: the code contains patterns that may \
                     perform destructive operations (file deletion, subprocess execution, \
                     permission changes). Try a different approach without these operations."
                        .into(),
                ));
            }
            CodeExecutionDecision::RequiresConfirmation => {
                tracing::warn!(
                    code_len = code.len(),
                    language = ?language,
                    "code execution with confirmation-required patterns — proceeding"
                );
            }
            CodeExecutionDecision::Approved => {}
        }

        self.backend.execute(code, language, timeout).await
    }

    fn schema(&self) -> ToolSchema {
        // Probe at schema-time so the LLM sees a *live* enum, not a
        // stale static one. The backend trait is `Arc<dyn ...>` so
        // this is one virtual call per `tools/list` request — cheap.
        let langs = self.backend.available_languages();
        let lang_labels = langs.clone();

        let mut props = IndexMap::new();
        props.insert(
            "code".into(),
            json!({
                "type": "string",
                "description": "The code to execute"
            }),
        );
        props.insert(
            "language".into(),
            json!({
                "type": "string",
                // Description is dynamic: tells the LLM exactly which
                // languages are usable right now, instead of a stale
                // list that may overlap with what's actually on PATH.
                "description": format!(
                    "Programming language (default: 'python'). Only pick from languages currently available: [{}].",
                    lang_labels.join(", ")
                ),
                "enum": langs,
                "default": "python"
            }),
        );
        props.insert(
            "timeout".into(),
            json!({
                "type": "integer",
                "description": "Execution timeout in seconds (default: 30)",
                "default": 30
            }),
        );

        tool_schema(
            "execute_code",
            &format!(
                "Execute code in a local interpreter. Available languages on this host: [{}]. \
                 The `language` enum reflects the live probe (sync), so picking an unavailable \
                 language at call-time will fail with an error listing the searched paths.",
                lang_labels.join(", ")
            ),
            JsonSchema::object(props, vec!["code".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockCodeBackend;
    #[async_trait]
    impl CodeExecutionBackend for MockCodeBackend {
        async fn execute(
            &self,
            code: &str,
            _language: Option<&str>,
            _timeout: Option<u64>,
        ) -> Result<String, ToolError> {
            Ok(format!("Output of: {}", code))
        }
    }

    // ── Guard tests ───────────────────────────────────────────────

    #[test]
    fn guard_allows_simple_calculation() {
        assert_eq!(
            CodeExecutionGuard::check("print(1 + 1)"),
            CodeExecutionDecision::Approved
        );
    }

    #[test]
    fn guard_allows_list_comprehension() {
        assert_eq!(
            CodeExecutionGuard::check("[x**2 for x in range(10)]"),
            CodeExecutionDecision::Approved
        );
    }

    #[test]
    fn guard_allows_math_and_string_ops() {
        let codes = [
            "import math; print(math.pi)",
            "sorted([3, 1, 2])",
            "\"\".join(['a', 'b'])",
            "sum(range(100))",
            "max(1, 2, 3)",
            "isinstance(42, int)",
            "dict(a=1, b=2).keys()",
        ];
        for code in codes {
            assert_eq!(
                CodeExecutionGuard::check(code),
                CodeExecutionDecision::Approved,
                "expected Approved for: {code}"
            );
        }
    }

    #[test]
    fn guard_denies_os_system() {
        assert_eq!(
            CodeExecutionGuard::check("import os; os.system('rm -rf /')"),
            CodeExecutionDecision::Denied
        );
    }

    #[test]
    fn guard_denies_os_popen() {
        assert_eq!(
            CodeExecutionGuard::check("import os; os.popen('id')"),
            CodeExecutionDecision::Denied
        );
    }

    #[test]
    fn guard_denies_subprocess_run() {
        assert_eq!(
            CodeExecutionGuard::check("import subprocess; subprocess.run(['ls'])"),
            CodeExecutionDecision::Denied
        );
    }

    #[test]
    fn guard_denies_shutil_rmtree() {
        assert_eq!(
            CodeExecutionGuard::check("import shutil; shutil.rmtree('/tmp/x')"),
            CodeExecutionDecision::Denied
        );
    }

    #[test]
    fn guard_denies_os_remove() {
        assert_eq!(
            CodeExecutionGuard::check("os.remove('/tmp/test.txt')"),
            CodeExecutionDecision::Denied
        );
    }

    #[test]
    fn guard_denies_os_unlink() {
        assert_eq!(
            CodeExecutionGuard::check("os.unlink('/tmp/test.txt')"),
            CodeExecutionDecision::Denied
        );
    }

    #[test]
    fn guard_denies_os_kill() {
        assert_eq!(
            CodeExecutionGuard::check("os.kill(1234, 15)"),
            CodeExecutionDecision::Denied
        );
    }

    #[test]
    fn guard_denies_os_chmod() {
        assert_eq!(
            CodeExecutionGuard::check("os.chmod('/tmp/x', 0o755)"),
            CodeExecutionDecision::Denied
        );
    }

    #[test]
    fn guard_denies_child_process_require() {
        assert_eq!(
            CodeExecutionGuard::check(r#"const { exec } = require('child_process')"#),
            CodeExecutionDecision::Denied
        );
    }

    #[test]
    fn guard_denies_os_walk() {
        assert_eq!(
            CodeExecutionGuard::check("for root, dirs, files in os.walk('/tmp'): print(root)"),
            CodeExecutionDecision::Denied
        );
    }

    #[test]
    fn guard_confirms_eval() {
        assert_eq!(
            CodeExecutionGuard::check("eval('1+1')"),
            CodeExecutionDecision::RequiresConfirmation
        );
    }

    #[test]
    fn guard_confirms_file_write() {
        assert_eq!(
            CodeExecutionGuard::check("open('/tmp/test.txt', 'w').write('hello')"),
            CodeExecutionDecision::RequiresConfirmation
        );
    }

    #[test]
    fn guard_confirms_network_import() {
        assert_eq!(
            CodeExecutionGuard::check("import requests; requests.get('http://example.com')"),
            CodeExecutionDecision::RequiresConfirmation
        );
    }

    #[test]
    fn guard_confirms_threading() {
        assert_eq!(
            CodeExecutionGuard::check("import threading; t = threading.Thread(target=fn)"),
            CodeExecutionDecision::RequiresConfirmation
        );
    }

    #[test]
    fn guard_confirms_ctypes() {
        assert_eq!(
            CodeExecutionGuard::check("import ctypes; ctypes.CDLL('libc.so.6')"),
            CodeExecutionDecision::RequiresConfirmation
        );
    }

    #[test]
    fn guard_confirms_shutil_move() {
        assert_eq!(
            CodeExecutionGuard::check("import shutil; shutil.move('/tmp/a', '/tmp/b')"),
            CodeExecutionDecision::RequiresConfirmation
        );
    }

    #[test]
    fn guard_denied_beats_confirm() {
        // Even if code has both denied and confirm patterns, denied wins.
        let code = "import os; os.system('rm -rf /'); open('/tmp/test.txt', 'w')";
        assert_eq!(
            CodeExecutionGuard::check(code),
            CodeExecutionDecision::Denied
        );
    }

    // ── Handler tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_execute_code_schema() {
        let handler = ExecuteCodeHandler::new(Arc::new(MockCodeBackend));
        assert_eq!(handler.schema().name, "execute_code");
    }

    #[tokio::test]
    async fn test_execute_code_allowed() {
        let handler = ExecuteCodeHandler::new(Arc::new(MockCodeBackend));
        let result = handler
            .execute(json!({"code": "print(1+1)"}))
            .await
            .unwrap();
        assert!(result.contains("print(1+1)"));
    }

    #[tokio::test]
    async fn test_execute_code_denied_by_guard() {
        let handler = ExecuteCodeHandler::new(Arc::new(MockCodeBackend));
        let result = handler
            .execute(json!({"code": "import os; os.system('ls')"}))
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            ToolError::ExecutionFailed(msg) => {
                assert!(msg.contains("denied by security policy"), "msg: {msg}");
            }
            other => panic!("expected ExecutionFailed, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_execute_code_confirmed_passes() {
        // RequiresConfirmation just logs a warning — the code still runs.
        let handler = ExecuteCodeHandler::new(Arc::new(MockCodeBackend));
        let result = handler
            .execute(json!({"code": "eval('1+1')"}))
            .await;
        assert!(result.is_ok(), "confirmed code should still execute: {result:?}");
    }
}
