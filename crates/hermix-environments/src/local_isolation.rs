//! Local process isolation backend .
//!
//! A [`TerminalBackend`] implementation that runs commands in an
//! isolated temporary directory using `std::process::Command` (via
//! Tokio's async wrapper). Unlike `DockerBackend`, this requires
//! no external runtime — it's a lightweight sandbox for desktop
//! users who don't have Docker installed.
//!
//! Isolation guarantees:
//! - Each instance gets its own temporary working directory
//! - Child process CWD is set to the temp dir
//! - Environment variables are whitelisted (not inherited by default)
//! - Temp dir is cleaned up when the backend is dropped
//!
//! **Not a security sandbox**: A determined process can break out
//! of the temp dir. For real sandboxing, use `DockerBackend`.

use async_trait::async_trait;
use std::path::{Path, PathBuf};
use tokio::process::Command as TokioCommand;

use hermix_core::console_decode::decode_console_output;
use hermix_core::{AgentError, CommandOutput, TerminalBackend};

// ---------------------------------------------------------------------------
// LocalIsolationBackend
// ---------------------------------------------------------------------------

/// A [`TerminalBackend`] that runs commands in an isolated temporary
/// directory. The working directory is a `tempfile::TempDir` that is
/// automatically cleaned up when the backend is dropped.
pub struct LocalIsolationBackend {
    /// Isolated working directory (auto-cleaned on drop).
    work_dir: tempfile::TempDir,
    /// Environment variables to pass through to child processes.
    env_whitelist: Vec<String>,
    /// Default timeout in seconds for command execution.
    default_timeout: u64,
    /// Maximum output size in bytes before truncation.
    max_output_size: usize,
}

impl LocalIsolationBackend {
    /// Create a new local isolation backend.
    pub fn new(
        env_whitelist: Vec<String>,
        default_timeout: u64,
        max_output_size: usize,
    ) -> Result<Self, AgentError> {
        let work_dir = tempfile::TempDir::new()
            .map_err(|e| AgentError::Config(format!("Failed to create temp dir: {}", e)))?;

        Ok(Self {
            work_dir,
            env_whitelist,
            default_timeout,
            max_output_size,
        })
    }

    /// Create with the system PATH inherited (convenience).
    pub fn with_path_inherited(default_timeout: u64, max_output_size: usize) -> Result<Self, AgentError> {
        Self::new(
            vec!["PATH".to_string(), "HOME".to_string(), "TEMP".to_string()],
            default_timeout,
            max_output_size,
        )
    }

    /// Get the path to the isolated working directory.
    pub fn work_dir_path(&self) -> &Path {
        self.work_dir.path()
    }

    /// Build environment variables for a child process.
    fn build_env(&self) -> Vec<(String, String)> {
        self.env_whitelist
            .iter()
            .filter_map(|key| {
                std::env::var(key).ok().map(|val| (key.clone(), val))
            })
            .collect()
    }

    /// Read a file from within the isolated directory.
    fn resolve_path(&self, path: &str) -> PathBuf {
        let p = Path::new(path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.work_dir.path().join(path)
        }
    }

    /// Convert raw output into a `CommandOutput`, truncating at
    /// `max_output_size` bytes (same pattern as `DockerBackend`).
    fn make_output(&self, exit_code: i32, stdout: Vec<u8>, stderr: Vec<u8>) -> CommandOutput {
        let cap = self.max_output_size;
        let out = crate::output::truncate_bytes_to_boundary(stdout, cap);
        let err = crate::output::truncate_bytes_to_boundary(stderr, cap);
        CommandOutput {
            exit_code,
            stdout: decode_console_output(&out).to_string(),
            stderr: decode_console_output(&err).to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// TerminalBackend implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl TerminalBackend for LocalIsolationBackend {
    async fn execute_command(
        &self,
        command: &str,
        timeout: Option<u64>,
        workdir: Option<&str>,
        background: bool,
        _pty: bool,
    ) -> Result<CommandOutput, AgentError> {
        let effective_timeout = timeout.unwrap_or(self.default_timeout);
        let cwd = match workdir {
            Some(dir) => self.resolve_path(dir),
            None => self.work_dir.path().to_path_buf(),
        };

        // On Windows, use cmd.exe /C to execute the command.
        let mut cmd = TokioCommand::new("cmd");
        cmd.arg("/C").arg(command);
        cmd.current_dir(&cwd);
        #[cfg(target_family = "windows")]
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW on Windows

        // Set whitelisted environment variables.
        cmd.env_clear();
        for (key, val) in self.build_env() {
            cmd.env(key, val);
        }

        // Capture stdout/stderr.
        if background {
            cmd.stdout(std::process::Stdio::null());
            cmd.stderr(std::process::Stdio::null());
            cmd.stdin(std::process::Stdio::null());
        } else {
            cmd.stdout(std::process::Stdio::piped());
            cmd.stderr(std::process::Stdio::piped());
            cmd.stdin(std::process::Stdio::null());
        }

        let result = if background {
            // Spawn and detach.
            match cmd.spawn() {
                Ok(_) => CommandOutput {
                    exit_code: 0,
                    stdout: "[background process started]".to_string(),
                    stderr: String::new(),
                },
                Err(e) => CommandOutput {
                    exit_code: 1,
                    stdout: String::new(),
                    stderr: format!("Failed to start background process: {}", e),
                },
            }
        } else {
            // Execute with timeout.
            match tokio::time::timeout(
                std::time::Duration::from_secs(effective_timeout),
                cmd.output(),
            )
            .await
            {
                Ok(Ok(output)) => {
                    let exit_code = output.status.code().unwrap_or(1);
                    self.make_output(exit_code, output.stdout, output.stderr)
                }
                Ok(Err(e)) => CommandOutput {
                    exit_code: 1,
                    stdout: String::new(),
                    stderr: format!("Command execution failed: {}", e),
                },
                Err(_) => CommandOutput {
                    exit_code: 124, // timeout exit code convention
                    stdout: String::new(),
                    stderr: format!("Command timed out after {}s", effective_timeout),
                },
            }
        };

        Ok(result)
    }

    async fn read_file(
        &self,
        path: &str,
        offset: Option<u64>,
        limit: Option<u64>,
    ) -> Result<String, AgentError> {
        let full_path = self.resolve_path(path);
        let content = tokio::fs::read_to_string(&full_path)
            .await
            .map_err(|e| AgentError::Config(format!("Failed to read {}: {}", path, e)))?;

        let lines: Vec<&str> = content.lines().collect();
        let start = offset.unwrap_or(0) as usize;
        let end = limit.map(|l| (start + l as usize).min(lines.len()))
            .unwrap_or(lines.len());

        if start >= lines.len() {
            return Ok(String::new());
        }

        Ok(lines[start..end].join("\n"))
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<(), AgentError> {
        let full_path = self.resolve_path(path);

        // Ensure parent directory exists.
        if let Some(parent) = full_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| AgentError::Config(format!("Failed to create directory: {}", e)))?;
        }

        tokio::fs::write(&full_path, content)
            .await
            .map_err(|e| AgentError::Config(format!("Failed to write {}: {}", path, e)))?;

        Ok(())
    }

    async fn file_exists(&self, path: &str) -> Result<bool, AgentError> {
        let full_path = self.resolve_path(path);
        Ok(full_path.exists())
    }
}

// ---------------------------------------------------------------------------
// HostedAgentPool
// ---------------------------------------------------------------------------

/// A pool of pre-warmed local isolation instances.
/// Maintains a small pool of ready-to-use `LocalIsolationBackend`
/// instances so that agent delegation doesn't pay the temp-dir
/// creation cost on the hot path.
pub struct HostedAgentPool {
    /// Pre-warmed backends waiting for allocation.
    pool: Vec<LocalIsolationBackend>,
    /// Maximum pool size.
    max_size: usize,
    /// Configuration for creating new backends.
    env_whitelist: Vec<String>,
    default_timeout: u64,
    max_output_size: usize,
}

impl HostedAgentPool {
    /// Create a new pool with the given configuration.
    pub fn new(
        warm_pool_size: usize,
        max_size: usize,
        env_whitelist: Vec<String>,
        default_timeout: u64,
        max_output_size: usize,
    ) -> Result<Self, AgentError> {
        let mut pool = Vec::with_capacity(warm_pool_size);
        for _ in 0..warm_pool_size {
            pool.push(LocalIsolationBackend::new(
                env_whitelist.clone(),
                default_timeout,
                max_output_size,
            )?);
        }

        Ok(Self {
            pool,
            max_size,
            env_whitelist,
            default_timeout,
            max_output_size,
        })
    }

    /// Allocate an isolation backend from the pool.
    /// Creates a new one if the pool is empty and under max_size.
    pub fn allocate(&mut self) -> Result<LocalIsolationBackend, AgentError> {
        if let Some(backend) = self.pool.pop() {
            return Ok(backend);
        }

        if self.pool.len() < self.max_size {
            LocalIsolationBackend::new(
                self.env_whitelist.clone(),
                self.default_timeout,
                self.max_output_size,
            )
        } else {
            Err(AgentError::Config("Hosted agent pool exhausted".to_string()))
        }
    }

    /// Return a backend to the pool for reuse.
    pub fn release(&mut self, backend: LocalIsolationBackend) {
        if self.pool.len() < self.max_size {
            self.pool.push(backend);
        }
        // Otherwise, the backend is dropped and its temp dir cleaned up.
    }

    /// Current number of available backends in the pool.
    pub fn available(&self) -> usize {
        self.pool.len()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_isolation_execute_command() {
        let backend = LocalIsolationBackend::with_path_inherited(30, 65536).unwrap();

        let output = backend
            .execute_command("echo hello", None, None, false, false)
            .await
            .unwrap();

        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.contains("hello"), "got: {:?}", output.stdout);
    }

    #[tokio::test]
    async fn local_isolation_working_directory() {
        let backend = LocalIsolationBackend::with_path_inherited(30, 65536).unwrap();
        let work_dir = backend.work_dir_path().to_path_buf();

        let output = backend
            .execute_command("cd", None, None, false, false)
            .await
            .unwrap();

        // On Windows, `cd` outputs the current directory.
        let output_path = output.stdout.trim().to_string();
        assert!(
            output_path.eq_ignore_ascii_case(work_dir.to_str().unwrap()),
            "expected cwd {:?}, got {:?}",
            work_dir,
            output_path
        );
    }

    #[tokio::test]
    async fn local_isolation_file_operations() {
        let backend = LocalIsolationBackend::with_path_inherited(30, 65536).unwrap();

        // Write a file.
        backend
            .write_file("test.txt", "hello world")
            .await
            .unwrap();

        // Check it exists.
        assert!(backend.file_exists("test.txt").await.unwrap());

        // Read it back.
        let content = backend.read_file("test.txt", None, None).await.unwrap();
        assert_eq!(content, "hello world");
    }

    #[tokio::test]
    async fn local_isolation_env_whitelist() {
        // Create a backend that inherits no env vars.
        let backend = LocalIsolationBackend::new(vec![], 30, 65536).unwrap();

        let output = backend
            .execute_command("echo %PATH%", None, None, false, false)
            .await
            .unwrap();

        // Without PATH inherited, %PATH% should not be resolved.
        assert!(
            output.stdout.trim().eq_ignore_ascii_case("%PATH%"),
            "expected unresolved %PATH%, got: {:?}",
            output.stdout
        );
    }

    #[test]
    fn hosted_agent_pool() {
        let mut pool = HostedAgentPool::new(
            2,  // warm size
            5,  // max size
            vec!["PATH".to_string()],
            30,
            65536,
        )
        .unwrap();

        assert_eq!(pool.available(), 2);

        let b1 = pool.allocate().unwrap();
        assert_eq!(pool.available(), 1);

        pool.release(b1);
        assert_eq!(pool.available(), 2);
    }
}
