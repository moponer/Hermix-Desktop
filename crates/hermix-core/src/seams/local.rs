//! 接缝的默认 provider（本地实现）。保证内核开箱可用：不装任何插件、
//! 不配置远程沙箱时，Fs/Shell 落在本机磁盘与本进程子进程上。
//!
//! 换 provider = 换这里的对象，上层无感。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::RwLock;

use super::{
    DirEntry, Envelope, ExecOutput, ExecWorld, Fs, FsError, Op, Sandbox, Shell, ShellError,
    ShellErrorKind, Transport, TransportError, Verdict,
};

// ---------------------------------------------------------------------------
// LocalFs
// ---------------------------------------------------------------------------

/// 本机磁盘。所有路径按传入原样使用，不做虚拟根映射。
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalFs;

impl LocalFs {
    fn map_io(path: &Path, e: std::io::Error) -> FsError {
        match e.kind() {
            std::io::ErrorKind::NotFound => FsError::NotFound(path.to_path_buf()),
            std::io::ErrorKind::PermissionDenied => FsError::Denied(path.to_path_buf()),
            _ => FsError::Io(e.to_string()),
        }
    }
}

#[async_trait]
impl Fs for LocalFs {
    async fn read(&self, path: &Path) -> Result<Vec<u8>, FsError> {
        tokio::fs::read(path).await.map_err(|e| Self::map_io(path, e))
    }

    async fn write(&self, path: &Path, contents: &[u8]) -> Result<(), FsError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| Self::map_io(parent, e))?;
            }
        }
        tokio::fs::write(path, contents).await.map_err(|e| Self::map_io(path, e))
    }

    async fn list(&self, path: &Path) -> Result<Vec<DirEntry>, FsError> {
        let mut rd = tokio::fs::read_dir(path).await.map_err(|e| Self::map_io(path, e))?;
        let mut out = Vec::new();
        while let Some(entry) = rd.next_entry().await.map_err(|e| Self::map_io(path, e))? {
            let meta = entry.metadata().await.map_err(|e| Self::map_io(&entry.path(), e))?;
            out.push(DirEntry { path: entry.path(), is_dir: meta.is_dir(), size: meta.len() });
        }
        Ok(out)
    }

    async fn remove(&self, path: &Path) -> Result<(), FsError> {
        let meta = tokio::fs::metadata(path).await.map_err(|e| Self::map_io(path, e))?;
        let r = if meta.is_dir() {
            tokio::fs::remove_dir_all(path).await
        } else {
            tokio::fs::remove_file(path).await
        };
        r.map_err(|e| Self::map_io(path, e))
    }
}

// ---------------------------------------------------------------------------
// LocalShell
// ---------------------------------------------------------------------------

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// 本机子进程。Windows 走 `cmd /C`，其它平台走 `sh -c`。
pub struct LocalShell {
    pub timeout: Duration,
}

impl Default for LocalShell {
    fn default() -> Self {
        Self { timeout: DEFAULT_TIMEOUT }
    }
}

#[async_trait]
impl Shell for LocalShell {
    async fn exec(&self, cmd: &str, cwd: &Path) -> Result<ExecOutput, ShellError> {
        let mut command = {
            #[cfg(windows)]
            {
                let mut c = tokio::process::Command::new("cmd");
                c.args(["/C", cmd]);
                c
            }
            #[cfg(not(windows))]
            {
                let mut c = tokio::process::Command::new("sh");
                c.args(["-c", cmd]);
                c
            }
        };
        command.current_dir(cwd).stdin(std::process::Stdio::null());
        // 管道而非继承，否则子进程写满管道会互相阻塞
        command.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        // CREATE_NO_WINDOW：spawn 控制台子进程时避免弹出黑窗
        #[cfg(windows)]
        command.creation_flags(0x08000000);

        let child = command.spawn().map_err(|e| ShellError::Spawn(e.to_string()))?;
        match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
            Ok(Ok(out)) => Ok(ExecOutput {
                code: out.status.code().unwrap_or(-1),
                // 控制台输出在 Windows 上是 CP936/OEM，直接 from_utf8_lossy 会
                // 把中文命令报错变成 U+FFFD，导致错误分类退化成"编码不匹配"
                stdout: crate::console_decode::decode_console_output_owned(out.stdout),
                stderr: crate::console_decode::decode_console_output_owned(out.stderr),
                timed_out: false,
            }),
            Ok(Err(e)) => Err(ShellError::Spawn(e.to_string())),
            Err(_) => Ok(ExecOutput {
                code: -1,
                stdout: String::new(),
                stderr: String::new(),
                timed_out: true,
            }),
        }
    }

    fn classify(&self, out: &ExecOutput) -> Option<ShellErrorKind> {
        if out.timed_out {
            return Some(ShellErrorKind::Timeout);
        }
        if out.code == 0 {
            return None;
        }
        Some(classify_text(&out.stderr, &out.stdout))
    }
}

/// 从 stderr（优先）/ stdout 文本判定失败类别。Windows 同一类失败有多种文案，
/// 中英双语都要覆盖，否则中文系统上会退化成 Unknown 让模型盲试。
fn classify_text(stderr: &str, stdout: &str) -> ShellErrorKind {
    let haystack = if stderr.trim().is_empty() { stdout } else { stderr };
    // U+FFFD 说明控制台编码与 UTF-8 不一致，文本已经不可信
    if haystack.contains('\u{FFFD}') {
        return ShellErrorKind::EncodingMismatch;
    }
    let lower = haystack.to_lowercase();
    let has = |pats: &[&str]| pats.iter().any(|p| lower.contains(p));

    if has(&["不是内部或外部命令", "is not recognized", "command not found", "no such file or directory"]) {
        ShellErrorKind::CommandNotFound
    } else if has(&["access is denied", "拒绝访问", "permission denied", "eacces", "eperm"]) {
        ShellErrorKind::AccessDenied
    } else if has(&["another program", "being used by another process", "正在使用", "file locked"]) {
        ShellErrorKind::FileLocked
    } else if has(&["语法不正确", "syntax error", "unexpected token", "命令语法"]) {
        ShellErrorKind::ShellSyntax
    } else {
        ShellErrorKind::Unknown
    }
}

// ---------------------------------------------------------------------------
// 执行世界（S2：Fs + Shell 由同一个 provider 给出）
// ---------------------------------------------------------------------------

/// 默认执行世界：本机磁盘 + 本机子进程。
pub struct LocalExecWorld {
    fs: LocalFs,
    shell: LocalShell,
}

impl Default for LocalExecWorld {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalExecWorld {
    pub fn new() -> Self {
        Self { fs: LocalFs, shell: LocalShell::default() }
    }
}

impl ExecWorld for LocalExecWorld {
    fn fs(&self) -> &dyn Fs {
        &self.fs
    }
    fn shell(&self) -> &dyn Shell {
        &self.shell
    }
}

/// 由调用方拼装的执行世界（测试 / 远程 provider 用）。
pub struct FixedExecWorld<F: Fs, S: Shell> {
    fs: F,
    shell: S,
}

impl<F: Fs, S: Shell> FixedExecWorld<F, S> {
    pub fn new(fs: F, shell: S) -> Self {
        Self { fs, shell }
    }
}

impl<F: Fs, S: Shell> ExecWorld for FixedExecWorld<F, S> {
    fn fs(&self) -> &dyn Fs {
        &self.fs
    }
    fn shell(&self) -> &dyn Shell {
        &self.shell
    }
}

// ---------------------------------------------------------------------------
// InMemoryFs / NullShell：换 provider 的验证用实现
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct InMemoryFs {
    files: RwLock<HashMap<PathBuf, Vec<u8>>>,
}

#[async_trait]
impl Fs for InMemoryFs {
    async fn read(&self, path: &Path) -> Result<Vec<u8>, FsError> {
        self.files.read().await.get(path).cloned().ok_or_else(|| FsError::NotFound(path.to_path_buf()))
    }

    async fn write(&self, path: &Path, contents: &[u8]) -> Result<(), FsError> {
        self.files.write().await.insert(path.to_path_buf(), contents.to_vec());
        Ok(())
    }

    async fn list(&self, path: &Path) -> Result<Vec<DirEntry>, FsError> {
        Ok(self
            .files
            .read()
            .await
            .iter()
            .filter(|(p, _)| p.parent() == Some(path))
            .map(|(p, c)| DirEntry { path: p.clone(), is_dir: false, size: c.len() as u64 })
            .collect())
    }

    async fn remove(&self, path: &Path) -> Result<(), FsError> {
        self.files.write().await.remove(path).map(|_| ()).ok_or_else(|| FsError::NotFound(path.to_path_buf()))
    }
}

/// 不执行任何命令的 Shell。用于验证上层在 provider 被替换后仍可工作。
pub struct NullShell;

#[async_trait]
impl Shell for NullShell {
    async fn exec(&self, _cmd: &str, _cwd: &Path) -> Result<ExecOutput, ShellError> {
        Ok(ExecOutput::default())
    }

    fn classify(&self, out: &ExecOutput) -> Option<ShellErrorKind> {
        if out.timed_out {
            Some(ShellErrorKind::Timeout)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// LocalSandbox / NoopTransport
// ---------------------------------------------------------------------------

/// 本机默认边界：读写与命令放行，**删除类交给人确认**。
/// 与 PI 一致——默认没有沙箱，授权靠启动时的边界划分，不是逐操作弹窗。
#[derive(Debug)]
pub struct LocalSandbox {
    /// 删除是否必须确认。关掉即全自动，风险自负。
    pub escalate_removal: bool,
}

impl Default for LocalSandbox {
    fn default() -> Self {
        Self { escalate_removal: true }
    }
}

impl Sandbox for LocalSandbox {
    fn allows(&self, op: &Op) -> Verdict {
        match op {
            Op::FsRemove { .. } if self.escalate_removal => Verdict::Escalate,
            _ => Verdict::Allow,
        }
    }
}

/// 默认不对外暴露（决策 D）。`publish` 一律返回 `Disabled`，
/// 与"网络故障"区分开：这是配置状态，不是故障。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopTransport;

#[async_trait]
impl Transport for NoopTransport {
    async fn publish(&self, _env: &Envelope) -> Result<(), TransportError> {
        Err(TransportError::Disabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_shell_runs_command() {
        let dir = tempfile::tempdir().unwrap();
        let shell = LocalShell::default();
        let out = shell
            .exec(if cfg!(windows) { "echo hi" } else { "echo hi" }, dir.path())
            .await
            .unwrap();
        assert_eq!(out.code, 0);
        assert!(out.stdout.contains("hi"));
        assert_eq!(shell.classify(&out), None);
    }

    #[tokio::test]
    async fn local_shell_classifies_missing_command() {
        let dir = tempfile::tempdir().unwrap();
        let shell = LocalShell::default();
        let out = shell
            .exec("definitely-not-a-real-command-xyz", dir.path())
            .await
            .unwrap();
        assert_ne!(out.code, 0);
        assert_eq!(shell.classify(&out), Some(ShellErrorKind::CommandNotFound));
    }

    #[test]
    fn classify_covers_chinese_and_english_text() {
        assert_eq!(
            classify_text("'foo' 不是内部或外部命令", ""),
            ShellErrorKind::CommandNotFound
        );
        assert_eq!(classify_text("Access is denied.", ""), ShellErrorKind::AccessDenied);
        assert_eq!(classify_text("拒绝访问。", ""), ShellErrorKind::AccessDenied);
        assert_eq!(classify_text("命令语法不正确。", ""), ShellErrorKind::ShellSyntax);
        assert_eq!(
            classify_text("The process cannot access the file because it is being used by another process.", ""),
            ShellErrorKind::FileLocked
        );
        assert_eq!(classify_text("something else", ""), ShellErrorKind::Unknown);
    }

    #[test]
    fn mojibake_is_reported_as_encoding_mismatch() {
        assert_eq!(classify_text("\u{FFFD}\u{FFFD}", ""), ShellErrorKind::EncodingMismatch);
    }

    #[tokio::test]
    async fn in_memory_fs_is_a_valid_provider() {
        let fs = InMemoryFs::default();
        let p = Path::new("/v/a.txt");
        fs.write(p, b"abc").await.unwrap();
        assert_eq!(fs.read(p).await.unwrap(), b"abc".to_vec());
        assert_eq!(fs.list(Path::new("/v")).await.unwrap().len(), 1);
        fs.remove(p).await.unwrap();
        assert!(matches!(fs.read(p).await, Err(FsError::NotFound(_))));
    }

    #[test]
    fn local_shell_default_timeout_is_bounded() {
        assert!(LocalShell::default().timeout < Duration::from_secs(600));
    }
}
