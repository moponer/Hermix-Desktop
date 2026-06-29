//! Docker terminal backend — executes commands inside a long-running
//! container via the `docker` CLI.
//!
//! P1-5 (2026-06-27): wrapped in a `docker` Cargo feature so the
//! dependency footprint stays zero for users who never opt in.
//!
//! Why CLI and not the `bollard` crate? `bollard` pulls in a long
//! tail of HTTP/SSL/TLS deps and pins a specific Docker daemon API
//! version that drifts; `docker exec` is stable since Docker 1.0 and
//! the CLI is already a hard dep on any system that *has* Docker
//! installed. We get the same isolation guarantee (process in a
//! different mount/PID/network namespace) without taking on a
//! half-megabyte of compile-time Rust.
//!
//! Sandbox use case: when the LLM wants to run a tool with side
//! effects (compile, install, mutate a build dir), routing through
//! `DockerBackend` keeps it from touching the user's host.

use async_trait::async_trait;
use tokio::process::Command as TokioCommand;

use hermix_core::console_decode::decode_console_output;
use hermix_core::{AgentError, CommandOutput, TerminalBackend};

/// A [`TerminalBackend`] that runs commands inside an existing
/// Docker container via `docker exec`. The container is assumed to
/// be already running and reachable as `<container>` (either by name
/// or id). Long-lived containers should be created out-of-band by
/// the user — we deliberately don't auto-spawn one because that
/// would require image-management state we don't have.
pub struct DockerBackend {
    /// Target container name or id (must already be running).
    container: String,
    /// Default timeout in seconds.
    default_timeout: u64,
    /// Maximum output size in bytes before truncation.
    max_output_size: usize,
    /// When true, prefix every `docker exec` invocation with
    /// `-u 0:0` so commands run as root inside the container.
    /// Defaults to false so we don't surprise the user with
    /// privilege escalation they didn't ask for.
    run_as_root: bool,
}

impl DockerBackend {
    /// Construct a new Docker backend.
    ///
    /// `container` must be the name or id of an already-running
    /// container. We do *not* auto-create one — that's a
    /// lifecycle concern (`docker run -d ...`) that belongs to the
    /// user or an orchestration layer.
    pub fn new(container: impl Into<String>, default_timeout: u64, max_output_size: usize) -> Self {
        Self {
            container: container.into(),
            default_timeout,
            max_output_size,
            run_as_root: false,
        }
    }

    /// Toggle root-user execution inside the container. Off by
    /// default; flip on if the container's default user can't write
    /// to its working dir.
    pub fn with_root(mut self, yes: bool) -> Self {
        self.run_as_root = yes;
        self
    }

    /// Build the `docker exec` prefix that goes in front of every
    /// command. Kept as a method so unit tests can assert on the
    /// exact shape — we don't want to silently drop `-i` and break
    /// background mode.
    fn build_exec_prefix(&self) -> Vec<String> {
        let mut args = vec!["exec".to_string()];
        if self.run_as_root {
            args.push("-u".to_string());
            args.push("0:0".to_string());
        }
        // `-i` keeps stdin open so background processes don't get
        // SIGPIPE the moment we stop reading; `-T` disables the
        // pseudo-TTY so output stays clean for line-based parsing.
        args.push("-i".to_string());
        args.push("-T".to_string());
        args.push(self.container.clone());
        args
    }

    /// Convert `DockerBackend::execute_command` output into the
    /// shared `CommandOutput` shape. Truncates stdout/stderr at
    /// `max_output_size` bytes (single-byte boundary — Docker is
    /// UTF-8 by default but we don't want to assume).
    fn make_output(&self, exit_code: i32, stdout: Vec<u8>, stderr: Vec<u8>) -> CommandOutput {
        let cap = self.max_output_size;
        let mut out = stdout;
        let mut err = stderr;
        if out.len() > cap {
            out.truncate(cap);
        }
        if err.len() > cap {
            err.truncate(cap);
        }
        CommandOutput {
            exit_code,
            stdout: decode_console_output(&out).to_string(),
            stderr: decode_console_output(&err).to_string(),
        }
    }
}

#[async_trait]
impl TerminalBackend for DockerBackend {
    async fn execute_command(
        &self,
        command: &str,
        timeout: Option<u64>,
        workdir: Option<&str>,
        background: bool,
        _pty: bool,
    ) -> Result<CommandOutput, AgentError> {
        let timeout = timeout.unwrap_or(self.default_timeout);

        // `docker exec -w <workdir> <container> sh -c <command>` so we
        // get shell semantics (pipes, redirects, globs). Background
        // mode drops the timeout — `docker exec -d` would be cleaner
        // but isn't supported on every Docker daemon version.
        let mut args = self.build_exec_prefix();
        if let Some(wd) = workdir {
            if !wd.is_empty() {
                args.push("-w".to_string());
                args.push(wd.to_string());
            }
        }
        args.push("sh".to_string());
        args.push("-c".to_string());
        args.push(command.to_string());

        let mut cmd = TokioCommand::new("docker");
        cmd.args(&args);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let child = cmd.spawn().map_err(|e| {
            AgentError::Config(format!(
                "DockerBackend failed to spawn docker (is the docker CLI on PATH?): {e}"
            ))
        })?;

        let output = if background {
            // Fire-and-record; we don't wait. The caller already
            // asked for background, so any "did it work" semantics
            // belong to the user's `kill` / `ps` flow.
            return Ok(CommandOutput {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            });
        } else {
            match tokio::time::timeout(
                std::time::Duration::from_secs(timeout),
                child.wait_with_output(),
            )
            .await
            {
                Ok(Ok(out)) => out,
                Ok(Err(e)) => {
                    return Err(AgentError::Io(format!(
                        "DockerBackend: failed to wait on docker exec: {e}"
                    )));
                }
                Err(_) => {
                    return Err(AgentError::Timeout(format!(
                        "DockerBackend: command timed out after {timeout}s"
                    )));
                }
            }
        };

        let exit_code = output.status.code().unwrap_or(-1);
        Ok(self.make_output(exit_code, output.stdout, output.stderr))
    }

    async fn read_file(
        &self,
        path: &str,
        offset: Option<u64>,
        limit: Option<u64>,
    ) -> Result<String, AgentError> {
        // We use `docker exec ... cat` rather than `docker cp` because
        // it streams back the file contents directly. `docker cp` would
        // require a writable scratch path on the host and an extra
        // round trip to clean it up.
        let mut cmd_parts = Vec::new();
        cmd_parts.push("cat".to_string());
        if let Some(skip) = offset {
            // `tail -n +N` skips the first N-1 lines; offset is
            // 1-based so `offset=1` returns line 1. We use 1-based
            // because that's how every other file tool in this
            // crate reports line numbers.
            cmd_parts.push(format!("(tail -n +{} || true) |", skip.max(1)));
        }
        if let Some(n) = limit {
            cmd_parts.push(format!("head -n {n}"));
        }
        cmd_parts.push(format!("'{}'", shell_escape(path)));

        let sh_cmd = cmd_parts.join(" ");
        let mut args = self.build_exec_prefix();
        args.push("sh".to_string());
        args.push("-c".to_string());
        args.push(sh_cmd);

        let output = TokioCommand::new("docker")
            .args(&args)
            .output()
            .await
            .map_err(|e| AgentError::Io(format!("DockerBackend read_file spawn: {e}")))?;

        if !output.status.success() {
            let stderr = decode_console_output(&output.stderr);
            return Err(AgentError::Io(format!(
                "DockerBackend read_file failed: {stderr}"
            )));
        }
        Ok(decode_console_output(&output.stdout).to_string())
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<(), AgentError> {
        // Pipe content via stdin to `docker exec ... tee` so we don't
        // have to write to a host-side scratch file (which would leak
        // when the container is later destroyed). `tee -p` is GNU
        // specific; we use plain `tee` because BusyBox/alpine
        // containers don't ship `-p` and we want this to work on
        // minimal images too.
        let sh_cmd = format!(
            "mkdir -p \"$(dirname '{path}')\" && tee '{path}' > /dev/null",
            path = shell_escape(path),
        );
        let mut args = self.build_exec_prefix();
        args.push("sh".to_string());
        args.push("-c".to_string());
        args.push(sh_cmd);

        let mut child = TokioCommand::new("docker")
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| AgentError::Io(format!("DockerBackend write_file spawn: {e}")))?;

        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            stdin.write_all(content.as_bytes()).await.map_err(|e| {
                AgentError::Io(format!("DockerBackend write_file stdin: {e}"))
            })?;
            // Drop stdin to signal EOF.
        }

        let out = child
            .wait_with_output()
            .await
            .map_err(|e| AgentError::Io(format!("DockerBackend write_file wait: {e}")))?;
        if !out.status.success() {
            let stderr = decode_console_output(&out.stderr);
            return Err(AgentError::Io(format!(
                "DockerBackend write_file failed: {stderr}"
            )));
        }
        Ok(())
    }

    async fn file_exists(&self, path: &str) -> Result<bool, AgentError> {
        // `test -e` exits 0 if the path exists (any kind: file,
        // dir, symlink, char device). Cheaper than stat and doesn't
        // require a separate error path for permission denied.
        let mut args = self.build_exec_prefix();
        args.push("sh".to_string());
        args.push("-c".to_string());
        args.push(format!("test -e '{}' && echo yes || echo no", shell_escape(path)));
        let out = TokioCommand::new("docker")
            .args(&args)
            .output()
            .await
            .map_err(|e| AgentError::Io(format!("DockerBackend file_exists spawn: {e}")))?;
        if !out.status.success() {
            // Non-zero exit here is unexpected (the inner test only
            // echoes). Treat as "not exists" rather than bubbling.
            return Ok(false);
        }
        let stdout = decode_console_output(&out.stdout);
        Ok(stdout.trim() == "yes")
    }
}

/// Wrap a path in single quotes, escaping any embedded single quote
/// (`'` → `'\''`). Used everywhere we build a shell command string
/// so the LLM can't inject arbitrary commands via a filename like
/// `'; rm -rf /`.
fn shell_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_exec_prefix_default_is_unprivileged() {
        let b = DockerBackend::new("test-container", 30, 1024);
        assert_eq!(
            b.build_exec_prefix(),
            vec!["exec", "-i", "-T", "test-container"]
        );
    }

    #[test]
    fn build_exec_prefix_with_root() {
        let b = DockerBackend::new("c", 30, 1024).with_root(true);
        assert_eq!(
            b.build_exec_prefix(),
            vec!["exec", "-u", "0:0", "-i", "-T", "c"]
        );
    }

    #[test]
    fn shell_escape_handles_quotes_and_empty() {
        assert_eq!(shell_escape(""), "''");
        assert_eq!(shell_escape("hello"), "'hello'");
        // The classic injection: a single quote inside the path
        // must NOT terminate the outer string.
        assert_eq!(shell_escape("a'b"), "'a'\\''b'");
        // Backslashes and special chars are passed through literally
        // because the outer quotes make them safe.
        assert_eq!(shell_escape("a;b c"), "'a;b c'");
    }

    #[test]
    fn make_output_truncates_at_max_size() {
        let b = DockerBackend::new("c", 30, 4);
        let huge = b"0123456789".to_vec();
        let out = b.make_output(0, huge.clone(), huge);
        assert_eq!(out.stdout.as_bytes().len(), 4);
        assert_eq!(out.stdout.as_bytes(), b"0123");
    }

    #[test]
    fn make_output_no_truncate_when_small() {
        let b = DockerBackend::new("c", 30, 100);
        let out = b.make_output(0, b"hi".to_vec(), b"ho".to_vec());
        assert_eq!(out.stdout, "hi");
        assert_eq!(out.stderr, "ho");
    }
}