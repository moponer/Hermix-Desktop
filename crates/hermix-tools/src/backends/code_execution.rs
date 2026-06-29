//! Real code execution backend: run scripts via tokio::process::Command.

use async_trait::async_trait;
use serde_json::json;
use std::process::Stdio;
use tokio::process::Command as TokioCommand;

use crate::tools::code_execution::CodeExecutionBackend;
use hermix_core::console_decode::decode_console_output;
use hermix_core::ToolError;

/// Code execution backend using local interpreters.
pub struct LocalCodeExecutionBackend {
    default_timeout_secs: u64,
}

impl LocalCodeExecutionBackend {
    pub fn new(default_timeout_secs: u64) -> Self {
        Self {
            default_timeout_secs,
        }
    }
}

impl Default for LocalCodeExecutionBackend {
    fn default() -> Self {
        Self::new(30)
    }
}

/// Resolve a Python interpreter binary for the current platform.
///
/// On **Windows** the order is `python` → `py` (the official Windows
/// launcher; works with the Microsoft Store and python.org
/// installs) → `python3`. On **Unix** the order is `python3` →
/// `python`. Returns the first binary that exists on `PATH` (via
/// the `which` crate), or `None` if none of the candidates are
/// installed.
fn resolve_python() -> Option<&'static str> {
    #[cfg(windows)]
    const CANDIDATES: &[&str] = &["python", "py", "python3", "python2"];
    #[cfg(not(windows))]
    const CANDIDATES: &[&str] = &["python3", "python", "python2"];

    for cand in CANDIDATES {
        if which::which(cand).is_ok() {
            return Some(*cand);
        }
    }
    None
}

#[async_trait]
impl CodeExecutionBackend for LocalCodeExecutionBackend {
    async fn execute(
        &self,
        code: &str,
        language: Option<&str>,
        timeout: Option<u64>,
    ) -> Result<String, ToolError> {
        let lang = language.unwrap_or("python");
        let timeout_secs = timeout.unwrap_or(self.default_timeout_secs);

        // Resolve interpreter. For Python we probe PATH at call time
        // (not registration time) so the tool picks up a freshly
        // installed interpreter without restart.
        let (interpreter, flag) = match lang {
            "python" | "python3" => match resolve_python() {
                Some(p) => (p, "-c"),
                None => {
                    return Err(ToolError::ExecutionFailed(
                        "No Python interpreter found on PATH. \
                         Tried: python, py, python3 (Windows) / \
                         python3, python (Unix). Install Python or \
                         set the PATH."
                            .to_string(),
                    ));
                }
            },
            "javascript" | "js" | "node" => match which::which("node") {
                Ok(_) => ("node", "-e"),
                Err(_) => {
                    return Err(ToolError::ExecutionFailed(
                        "Node.js not found on PATH. Install Node.js to run JavaScript."
                            .to_string(),
                    ));
                }
            },
            "typescript" | "ts" => match which::which("npx") {
                Ok(_) => ("npx", ""),
                Err(_) => {
                    return Err(ToolError::ExecutionFailed(
                        "npx not found on PATH. Install Node.js (which ships npx)."
                            .to_string(),
                    ));
                }
            },
            "bash" | "sh" => {
                // On Windows there is no `bash` unless Git for
                // Windows / WSL is installed. We probe explicitly so
                // the error message names the missing binary.
                let bash = "bash";
                match which::which(bash) {
                    Ok(_) => (bash, "-c"),
                    Err(_) => {
                        return Err(ToolError::ExecutionFailed(
                            "bash not found on PATH. Install Git for Windows, WSL, or run as a Unix shell."
                                .to_string(),
                        ));
                    }
                }
            }
            other => {
                return Err(ToolError::InvalidParams(format!(
                    "Unsupported language: {}",
                    other
                )))
            }
        };

        let mut cmd = TokioCommand::new(interpreter);
        if lang == "typescript" {
            // For TypeScript, write to temp file and run with ts-node.
            // Use `temp_dir()` (cross-platform).
            let tmp = std::env::temp_dir()
                .join(format!("hermes_exec_{}.ts", uuid::Uuid::new_v4()));
            tokio::fs::write(&tmp, code).await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to write temp file: {}", e))
            })?;
            cmd = TokioCommand::new("npx");
            cmd.arg("ts-node").arg(tmp.to_str().unwrap_or(""));
        } else {
            cmd.arg(flag).arg(code);
        }

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let result = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
            let child = cmd.spawn().map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to spawn {}: {}", interpreter, e))
            })?;
            let output = child.wait_with_output().await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to wait for process: {}", e))
            })?;
            Ok::<_, ToolError>((
                output.status.code().unwrap_or(-1),
                output.stdout,
                output.stderr,
            ))
        })
        .await;

        match result {
            Ok(Ok((exit_code, stdout, stderr))) => {
                // Use the shared GBK-aware decoder from hermix-core
                // so Chinese / Japanese / Korean output from
                // cmd.exe and child Python doesn't mojibake.
                let stdout_str = decode_console_output(&stdout).into_owned();
                let stderr_str = decode_console_output(&stderr).into_owned();
                Ok(json!({
                    "exit_code": exit_code,
                    "stdout": stdout_str,
                    "stderr": stderr_str,
                    "language": lang,
                    "interpreter": interpreter,
                })
                .to_string())
            }
            Ok(Err(e)) => Err(e),
            Err(_) => Err(ToolError::Timeout(format!(
                "Code execution timed out after {}s",
                timeout_secs
            ))),
        }
    }
}
