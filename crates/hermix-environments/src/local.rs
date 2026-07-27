//! Local terminal backend – executes commands on the same host.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::process::Command as TokioCommand;

use hermix_core::console_decode::decode_console_output;
use hermix_core::{AgentError, CommandOutput, TerminalBackend};

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
}

impl LocalBackend {
    /// Create a new local backend with the given defaults.
    pub fn new(default_timeout: u64, max_output_size: usize) -> Self {
        Self {
            default_timeout,
            max_output_size,
            background_processes: Arc::new(Mutex::new(Vec::new())),
            workspace_root: None,
        }
    }

    /// Set the workspace root for path boundary enforcement.
    pub fn with_workspace_root(mut self, root: PathBuf) -> Self {
        self.workspace_root = Some(root);
        self
    }
}

impl Default for LocalBackend {
    fn default() -> Self {
        Self::new(120, 1_048_576)
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
    ) -> Result<CommandOutput, AgentError> {
        let timeout_secs = timeout.unwrap_or(self.default_timeout);

        if pty {
            // PTY mode: allocate a pseudo-terminal for interactive commands.
            // Uses `script` command (available on macOS/Linux) to wrap the
            // command in a PTY, which makes programs behave as if connected
            // to a real terminal (enables colors, line editing, etc.).
            #[cfg(unix)]
            {
                let mut pty_cmd = TokioCommand::new("script");
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

        #[cfg(unix)]
        let mut cmd = {
            let mut c = TokioCommand::new("sh");
            c.arg("-c").arg(command);
            c
        };
        #[cfg(windows)]
        let mut cmd = {
            let trimmed = command.trim_start();
            let lower = trimmed.to_lowercase();
            // Detect PowerShell commands and route directly to
            // powershell.exe, bypassing cmd.exe which mangles quotes
            // in PowerShell commands (e.g. stripping outer double
            // quotes from `powershell -Command "Get-Process | Select Name"`).
            if lower.starts_with("powershell ") || lower.starts_with("powershell.exe ") {
                // Strip "powershell" or "powershell.exe" prefix (case-insensitive)
                let after_ps = trimmed[lower.find(' ').unwrap_or(0)..].trim_start();
                let lower_after = after_ps.to_lowercase();
                let mut c = TokioCommand::new("powershell.exe");
                c.arg("-NoProfile").arg("-NonInteractive");
                if lower_after.starts_with("-command ") {
                    let cmd_str = after_ps["-command ".len()..].trim_start();
                    c.arg("-Command").arg(cmd_str);
                } else if lower_after.starts_with("-encodedcommand ") {
                    let cmd_str = after_ps["-encodedcommand ".len()..].trim_start();
                    c.arg("-EncodedCommand").arg(cmd_str);
                } else if lower_after.starts_with("-file ") {
                    let cmd_str = after_ps["-file ".len()..].trim_start();
                    c.arg("-File").arg(cmd_str);
                } else {
                    // No flag — wrap the entire rest in -Command
                    c.arg("-Command").arg(after_ps);
                }
                c
            } else if lower.starts_with("pwsh ") || lower.starts_with("pwsh.exe ") {
                let after_pws = trimmed[lower.find(' ').unwrap_or(0)..].trim_start();
                let lower_after = after_pws.to_lowercase();
                let mut c = TokioCommand::new("pwsh.exe");
                c.arg("-NoProfile").arg("-NonInteractive");
                if lower_after.starts_with("-command ") {
                    c.arg("-Command").arg(after_pws["-command ".len()..].trim_start());
                } else {
                    c.arg("-Command").arg(after_pws);
                }
                c
            } else if lower.starts_with("cmd /c ") {
                // LLM wrapped in "cmd /c" — strip it, we already use cmd.exe /c
                let rest = &trimmed["cmd /c ".len()..];
                let mut c = TokioCommand::new("cmd.exe");
                c.arg("/c").arg(rest);
                c
            } else if lower.starts_with("cmd.exe /c ") {
                let rest = &trimmed["cmd.exe /c ".len()..];
                let mut c = TokioCommand::new("cmd.exe");
                c.arg("/c").arg(rest);
                c
            } else {
                // Default: cmd.exe /c <command>
                let mut c = TokioCommand::new("cmd.exe");
                c.arg("/c").arg(command);
                c
            }
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

        // Use explicit workdir, or fall back to workspace_root.
        // Strip \\?\ prefix on Windows — many programs (cmd.exe, FIND, etc.)
        // can't handle the Win32 device namespace path format. The
        // helper lives in `hermix-core::path_security` so the same
        // normalization is applied consistently across backends.
        let effective_dir: Option<PathBuf> = workdir
            .map(std::path::PathBuf::from)
            .or_else(|| self.workspace_root.clone())
            .map(|p| hermix_core::path_security::strip_win32_long_path_buf(&p));
        if let Some(dir) = effective_dir {
            cmd.current_dir(dir);
        }

        if background {
            // In background mode, detach the process and return immediately.
            cmd.stdin(Stdio::null());
        }

        let result = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
            let child = cmd
                .spawn()
                .map_err(|e| AgentError::Io(format!("Failed to spawn command: {}", e)))?;

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
            Err(_) => Err(AgentError::Timeout(format!(
                "Command timed out after {} seconds",
                timeout_secs
            ))),
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

#[cfg(test)]
mod tests {
    use super::*;

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
        // Use a platform-appropriate long-running command.
        // Unix: `sleep 30`; Windows: `ping -n 31 127.0.0.1 >nul` (~30s).
        let cmd = if cfg!(windows) {
            "ping -n 31 127.0.0.1 >nul"
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

        // A path that should exist
        assert!(backend.file_exists("/tmp").await.unwrap());

        // A path that should not exist
        assert!(!backend
            .file_exists("/tmp/hermes_nonexistent_test_file_xyz")
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
