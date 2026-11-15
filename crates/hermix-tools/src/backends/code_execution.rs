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

use crate::tools::code_execution::CodeExecutionBackend;
use hermix_core::console_decode::decode_console_output;
use hermix_core::ToolError;

/// 单流捕获上限：超出部分继续读但不存储（`print` 洪水不再把内存吃爆，
/// 继续排空也避免子进程因管道写满而挂死）。
const CAPTURE_CAP: usize = 32 * 1024 * 1024;

/// 有界排空一个子进程输出管道。
async fn drain_bounded<R: tokio::io::AsyncRead + Unpin>(pipe: Option<R>) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut pipe = match pipe {
        Some(p) => p,
        None => return Vec::new(),
    };
    let mut buf = Vec::new();
    let mut chunk = [0u8; 65_536];
    loop {
        match pipe.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if buf.len() < CAPTURE_CAP {
                    let take = n.min(CAPTURE_CAP - buf.len());
                    buf.extend_from_slice(&chunk[..take]);
                }
            }
        }
    }
    buf
}

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
    // bundled-Python feature (gated on `bundle.resources`). If the
    // file isn't there we silently skip — no error.
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
/// The output is intended for two audiences:
/// - The LLM (sees it via tool error): emits a tighter message
/// telling it to switch to the `bash` tool with `python3 -c ...`
/// or to ask the user to configure the override.
/// - The Settings UI (renders the same listing): shows the user
/// exactly which paths were probed so they can fix the missing one.
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

        let mut cmd = hermix_core::proc::cmd_tokio(&interpreter);
        if lang == "typescript" {
            // For TypeScript, write to temp file and run with ts-node.
            // Use `temp_dir()` (cross-platform).
            let tmp = std::env::temp_dir()
                .join(format!("hermes_exec_{}.ts", uuid::Uuid::new_v4()));
            tokio::fs::write(&tmp, code).await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to write temp file: {}", e))
            })?;
            // Win 上 `npx` 真名是 `npx.cmd`（npm 把 shim 写成 .cmd），
            // `Command::new` 不自动加扩展名 → 直接报 "program not found"。
            // `hermix_core::runtime::resolve_binary` 是项目级别的 npm
            // shim helper（白名单 npx/npm/pnpm/yarn/bun/...），把
            // "Win 上要不要加 .cmd" 这个判断集中在一处，避免每个调用点
            // 都重复 cfg 双分支。
            cmd = hermix_core::proc::cmd_tokio(hermix_core::runtime::resolve_binary("npx"));
            cmd.arg("ts-node").arg(tmp.to_str().unwrap_or(""));
        } else {
            cmd.arg(flag).arg(code);
        }

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        // 超时时 future 被 drop，必须连带杀掉解释器（kill_on_drop）——
        // 死循环脚本的孤儿此前会常驻。
        cmd.kill_on_drop(true);

        // JSON output reports the resolved interpreter path so the LLM
        // can correlate tool errors against "I called python on PATH and
        // got X".
        let interpreter_label = interpreter.display().to_string();

        //( / ) Plant a canary file in the
        // temp directory before execution. After execution, we verify
        // it's intact — if the executed code modified or deleted it,
        // that's a potential sandbox escape.
        let canary_dir = std::env::temp_dir();
        let canary = canary::plant(&canary_dir).ok();

        let result = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
            let mut child = cmd.spawn().map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to spawn {interpreter_label}: {e}"))
            })?;
            // Job 容器：drop 即 TerminateJobObject 杀整棵树（含脚本派生的
            // 孙进程）；附着失败降级为只杀顶层。正常结束时也顺带清残渣。
            #[cfg(windows)]
            let _job = hermix_core::win_job::JobGuard::attach(child.id().unwrap_or(0));
            // 有界捕获 + 排空（旧 wait_with_output 无上限，`print` 洪水会把
            // 内存吃爆）：只保留前 CAPTURE_CAP 字节，超出继续读但不存储。
            let out_task = tokio::spawn(drain_bounded(child.stdout.take()));
            let err_task = tokio::spawn(drain_bounded(child.stderr.take()));
            let status = child.wait().await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to wait for process: {e}"))
            })?;
            let stdout = out_task.await.unwrap_or_default();
            let stderr = err_task.await.unwrap_or_default();
            Ok::<_, ToolError>((status.code().unwrap_or(-1), stdout, stderr))
        })
        .await;

        //Verify canary integrity after execution.
        if let Some(ref canary_path) = canary {
            if let Err(msg) = canary::verify(canary_path) {
                tracing::warn!(
                    target: "hermix.tools.code_execution",
                    canary = %canary_path.display(),
                    "{msg}"
                );
            }
            canary::remove(canary_path);
        }

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

//sandbox canary detection ----------------------------------------

/// ( / ) Canary file for sandbox leak
/// detection. We write a unique canary file to a temp directory
/// before code execution. After execution, we check:
/// 1. The canary file still exists (not deleted).
/// 2. The canary content is unchanged (not modified).
/// If either check fails, the executed code may have escaped its
/// intended scope and wrote to the temp directory.
mod canary {
    use std::path::{Path, PathBuf};

    /// Magic string written into the canary file.
    const CANARY_MAGIC: &str = "HERMIX_CANARY_DO_NOT_TOUCH";

    /// Write a canary file into `dir` and return its path.
    /// The filename is unique per call to prevent collision.
    pub fn plant(dir: &Path) -> std::io::Result<PathBuf> {
        let canary_path = dir.join(format!(
            ".hermix_canary_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&canary_path, CANARY_MAGIC.as_bytes())?;
        Ok(canary_path)
    }

    /// Check that the canary file still exists and its content
    /// is unchanged. Returns `Ok(())` if intact, or `Err(msg)`
    /// describing the violation.
    pub fn verify(canary_path: &Path) -> Result<(), String> {
        if !canary_path.exists() {
            return Err(format!(
                "sandbox canary file was deleted: {} — code may have escaped its scope",
                canary_path.display()
            ));
        }
        match std::fs::read_to_string(canary_path) {
            Ok(content) if content == CANARY_MAGIC => Ok(()),
            Ok(content) => Err(format!(
                "sandbox canary file was modified (expected '{}', got '{}') — code may have escaped its scope",
                CANARY_MAGIC, content
            )),
            Err(e) => Err(format!(
                "sandbox canary file is unreadable: {e} — code may have escaped its scope"
            )),
        }
    }

    /// Remove the canary file (cleanup). Best-effort — ignores errors.
    pub fn remove(canary_path: &Path) {
        let _ = std::fs::remove_file(canary_path);
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

    //sandbox canary tests -------------------------------------------

    #[test]
    fn canary_plant_and_verify_intact() {
        let tmp = tempfile::tempdir().unwrap();
        let canary = canary::plant(tmp.path()).unwrap();
        assert!(canary.exists());
        assert!(canary::verify(&canary).is_ok(), "fresh canary should be intact");
        canary::remove(&canary);
        assert!(!canary.exists());
    }

    #[test]
    fn canary_detects_deletion() {
        let tmp = tempfile::tempdir().unwrap();
        let canary = canary::plant(tmp.path()).unwrap();
        std::fs::remove_file(&canary).unwrap();
        let result = canary::verify(&canary);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("deleted"));
    }

    #[test]
    fn canary_detects_modification() {
        let tmp = tempfile::tempdir().unwrap();
        let canary = canary::plant(tmp.path()).unwrap();
        std::fs::write(&canary, b"TAMPERED").unwrap();
        let result = canary::verify(&canary);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("modified"));
    }
}
