//! Real code execution backend: run scripts via tokio::process::Command.
//!
//! # Python runtime resolution (`find_python_bin`)
//!
//! Resolves a usable Python interpreter in three tiers, **highest priority first**:
//!
//! 1. **Explicit override** — caller-supplied path (Settings UI override
//!    or `HERMIX_PYTHON` env var). Stored on disk at
//!    `<hermix_home>/python_override.txt`. Used to fix a broken system
//!    install without touching `PATH`.
//! 2. **Sibling of the sidecar binary** — `<dir_of_hermix>/python/python.exe`
//!    on Windows / `<dir_of_hermix>/python/bin/python3` on Unix.
//!    This is the slot reserved for an opt-in bundled Python
//!    (`python-build-standalone`-style). Layout is fixed so
//!    `build-tauri.bat` can drop files in with a single `tar -x`.
//!    If the file isn't present we fall through silently — no error.
//! 3. **PATH probe** — `which::which(candidates)` in platform-correct
//!    order. On Windows: `python → py → python3`. On Unix: `python3 → python`.
//!
//! We deliberately do NOT auto-bundle Python by default (the bundled
//! interpreter is ~30MB and most users have Python already). The
//! sibling slot is reserved so the installer can opt into bundling
//! later via `tauri.conf.json::bundle.resources` + the `BUNDLED_PYTHON`
//! flag in `build-tauri.bat`.

use async_trait::async_trait;
use serde_json::json;
use std::path::{Path, PathBuf};
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

// ---------------------------------------------------------------------------
// Python interpreter discovery (shared with pdf.rs)
// ---------------------------------------------------------------------------

/// One candidate Python interpreter path, with where we heard about it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PythonSearchEntry {
    /// Human-readable source label (`"override"`, `"sibling-slot"`, `"PATH:python3"`, ...).
    pub source: &'static str,
    /// Absolute path that would be invoked.
    pub path: PathBuf,
    /// Whether the path resolves to an existing, executable file.
    pub exists: bool,
}

/// Resolve a usable Python interpreter at `python_override` if provided,
/// else probe the sibling slot + PATH.
///
/// Returns `Some(PathBuf)` if any candidate exists, else `None`. The
/// same path is reported by [`python_search_paths`] — use that for a
/// diagnostic listing.
pub fn find_python_bin(python_override: Option<&Path>) -> Option<PathBuf> {
    python_search_paths(python_override)
        .into_iter()
        .find(|e| e.exists)
        .map(|e| e.path)
}

/// Return every Python interpreter candidate tried, in priority order.
///
/// Does **not** fall back to `None` on errors — every tier is reported
/// even if missing, so the Settings UI can show "tried these paths".
/// The first entry whose `exists == true` is the one that would be used.
pub fn python_search_paths(python_override: Option<&Path>) -> Vec<PythonSearchEntry> {
    let mut out: Vec<PythonSearchEntry> = Vec::new();

    // 1. Caller-supplied override (highest priority).
    if let Some(p) = python_override {
        let exists = p.is_file();
        // We can't return &str on demand; source label is decided at the
        // call site via a Vec<&'static str> map. Keep it consistent:
        // this single entry always reports source as the override tag.
        out.push(PythonSearchEntry {
            source: "override",
            path: p.to_path_buf(),
            exists,
        });
    }

    // 2. HERMIX_PYTHON env var (orthogonal to caller override — both checked).
    if let Ok(env_p) = std::env::var("HERMIX_PYTHON") {
        if !env_p.is_empty() {
            let p = PathBuf::from(&env_p);
            // Skip duplicates if it happens to match the override.
            if !out.iter().any(|e| e.path == p) {
                out.push(PythonSearchEntry {
                    source: "env:HERMIX_PYTHON",
                    path: p,
                    exists: Path::new(&env_p).is_file(),
                });
            }
        }
    }

    // 3. Sibling of the running hermix binary. Reserved slot for the
    //    bundled-Python feature (gated on `bundle.resources`). If the
    //    file isn't there we silently skip — no error.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            #[cfg(windows)]
            {
                let sibling = dir.join("python").join("python.exe");
                if !out.iter().any(|e| e.path == sibling) {
                    out.push(PythonSearchEntry {
                        source: "sibling-slot",
                        path: sibling.clone(),
                        exists: sibling.is_file(),
                    });
                }
            }
            #[cfg(not(windows))]
            {
                let sibling = dir.join("python").join("bin").join("python3");
                if !out.iter().any(|e| e.path == sibling) {
                    out.push(PythonSearchEntry {
                        source: "sibling-slot",
                        path: sibling.clone(),
                        exists: sibling.is_file(),
                    });
                }
            }
        }
    }

    // 4. PATH probe — platform-correct candidate order.
    #[cfg(windows)]
    const PATH_CANDIDATES: &[&str] = &["python", "py", "python3", "python2"];
    #[cfg(not(windows))]
    const PATH_CANDIDATES: &[&str] = &["python3", "python", "python2"];

    for cand in PATH_CANDIDATES {
        // `which::which` returns the canonical full path; use *that* as
        // the entry path, not the bare `"python"` literal.
        let resolved = which::which(cand).ok();
        let path = resolved.clone().unwrap_or_else(|| PathBuf::from(cand));
        if out.iter().any(|e| e.path == path) {
            continue; // dedupe against sibling/env
        }
        out.push(PythonSearchEntry {
            // Source label is `PATH:<cand>` because the user might want to
            // see which PATH entry won (matters for debugging PATH conflicts).
            source: "PATH",
            // Leak-free: concat at format time below by stashing the candidate name.
            path,
            exists: resolved.is_some(),
        });
        let n = out.len() - 1;
        // Borrow the candidate name as a `&'static str` — `PATH_CANDIDATES`
        // is `&'static`, so this is safe.
        out[n].source = match out[n].source {
            "PATH" => leak_str(&format!("PATH:{cand}")),
            other => other,
        };
    }

    out
}

// `format!` returns `String`; `PythonSearchEntry.source` is `&'static str`.
// Leak the label once per call instead of changing the API to `String`.
// This call happens at most a handful of times per Settings refresh.
fn leak_str(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

/// Format the "no Python interpreter found" error message listing the
/// real search paths we tried.
///
/// The output is intended for two audiences:
/// - The LLM (sees it via tool error): emits a tighter message
///   telling it to switch to the `bash` tool with `python3 -c ...`
///   or to ask the user to configure the override.
/// - The Settings UI (renders the same listing): shows the user
///   exactly which paths were probed so they can fix the missing one.
pub fn format_missing_python_error(entries: &[PythonSearchEntry]) -> String {
    if entries.is_empty() {
        return "No Python interpreter configured. Set one in Settings (Backend tab) or install python3 and add it to PATH.".into();
    }
    let mut lines: Vec<String> = Vec::with_capacity(entries.len() + 1);
    lines.push("No Python interpreter found. Searched (in order):".to_string());
    for (i, e) in entries.iter().enumerate() {
        let status = if e.exists { "OK" } else { "MISSING" };
        lines.push(format!(
            "  {}. [{}] source={} path={}",
            i + 1,
            status,
            e.source,
            e.path.display()
        ));
    }
    lines.push(
        "Hint: install python3, set HERMIX_PYTHON, or pick an existing interpreter in Settings (Backend tab).".into(),
    );
    lines.join("\n")
}

#[async_trait]
impl CodeExecutionBackend for LocalCodeExecutionBackend {
    /// Which languages can we run *right now* (i.e. on this machine, with
    /// the current PATH)?
    ///
    /// Used by [`crate::tools::code_execution::ExecuteCodeHandler::schema`]
    /// to expose `language: enum: [actual, runtime, list]` to the LLM,
    /// so it doesn't keep calling `language="typescript"` on a host
    /// without `npx` and getting an error every turn.
    fn available_languages(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        out.push("python".into());
        if which::which("node").is_ok() {
            out.push("javascript".into());
        }
        if which::which("npx").is_ok() {
            out.push("typescript".into());
        }
        if which::which("bash").is_ok() {
            out.push("bash".into());
        }
        out
    }

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
        let interpreter = match lang {
            "python" | "python3" => match find_python_bin(None) {
                Some(p) => p,
                None => {
                    // The detailed error lists every path we tried so
                    // the LLM and the user can fix it without guessing.
                    let entries = python_search_paths(None);
                    return Err(ToolError::ExecutionFailed(format_missing_python_error(
                        &entries,
                    )));
                }
            },
            "javascript" | "js" | "node" => match which::which("node") {
                Ok(p) => p,
                Err(_) => {
                    return Err(ToolError::ExecutionFailed(
                        "Node.js not found on PATH. Install Node.js to run JavaScript."
                            .to_string(),
                    ));
                }
            },
            "typescript" | "ts" => match which::which("npx") {
                Ok(p) => p,
                Err(_) => {
                    return Err(ToolError::ExecutionFailed(
                        "npx not found on PATH. Install Node.js (which ships npx)."
                            .to_string(),
                    ));
                }
            },
            "bash" | "sh" => match which::which("bash") {
                Ok(p) => p,
                Err(_) => {
                    return Err(ToolError::ExecutionFailed(
                        "bash not found on PATH. Install Git for Windows, WSL, or run as a Unix shell."
                            .to_string(),
                    ));
                }
            },
            other => {
                return Err(ToolError::InvalidParams(format!(
                    "Unsupported language: {}",
                    other
                )))
            }
        };

        // For Python the flag is `-c`; for JS it's `-e`; for bash it's
        // `-c`. TypeScript wraps in npx + a temp file because `ts-node`
        // doesn't read from stdin.
        let flag: &str = match lang {
            "python" | "python3" | "bash" | "sh" => "-c",
            "javascript" | "js" | "node" => "-e",
            _ => "",
        };

        let mut cmd = TokioCommand::new(&interpreter);
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

        // JSON output reports the resolved interpreter path so the LLM
        // can correlate tool errors against "I called python on PATH and
        // got X".
        let interpreter_label = interpreter.display().to_string();

        let result = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
            let child = cmd.spawn().map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to spawn {interpreter_label}: {e}"))
            })?;
            let output = child.wait_with_output().await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to wait for process: {e}"))
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
                    "interpreter": interpreter_label,
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The search-paths helper is always >= 1 tier (PATH probes count
    /// for several entries on most systems). Callers can rely on
    /// "the list is non-empty" without `unwrap_or`/`?` ceremony.
    #[test]
    fn python_search_paths_is_non_empty() {
        let entries = python_search_paths(None);
        assert!(!entries.is_empty(), "PATH probe alone gives several entries");
    }

    /// If an override is supplied and points at a real file, it's
    /// returned by `find_python_bin` and appears as `source="override"`.
    #[test]
    fn find_python_bin_prefers_override_when_valid() {
        // `cargo test` always has a real `target/` parent and a real
        // Linux/Windows binary we can point the override at. We don't
        // care if the file is *executable*, only that `is_file()` is true.
        let tmp = tempfile::tempdir().unwrap();
        let fake_python = tmp.path().join("python.exe");
        std::fs::write(&fake_python, b"fake").unwrap();
        let resolved = find_python_bin(Some(&fake_python));
        assert_eq!(resolved.as_deref(), Some(fake_python.as_path()));
        let entries = python_search_paths(Some(&fake_python));
        assert_eq!(entries[0].source, "override");
        assert!(entries[0].exists);
    }

    /// The error formatter lists every tried path; this is what the
    /// LLM sees and what the Settings UI renders.
    #[test]
    fn format_missing_error_lists_paths() {
        let entries = vec![PythonSearchEntry {
            source: "PATH:python3",
            path: PathBuf::from("/usr/bin/python3"),
            exists: false,
        }];
        let s = format_missing_python_error(&entries);
        assert!(s.contains("Searched"));
        assert!(s.contains("PATH:python3"));
        assert!(s.contains("/usr/bin/python3"));
        assert!(s.contains("MISSING"));
    }

    /// `available_languages` always reports `python` (it's our floor;
    /// every backend can attempt it; the call fails gracefully if no
    /// interpreter is around). The rest are opportunistic.
    #[test]
    fn available_languages_includes_python() {
        let backend = LocalCodeExecutionBackend::default();
        let langs = backend.available_languages();
        assert!(langs.contains(&"python".to_string()), "python must always be advertised; got: {:?}", langs);
    }
}
