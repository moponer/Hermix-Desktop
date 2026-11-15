//! 内核接缝（C3）。定义内核认识、业务不认识的**最小接口面**。
//!
//! 设计约束：**薄**。这里只放内核真正需要调度的能力边界，不搬业务实现，
//! 不为某个调用方预留参数。业务 crate 依赖这些 trait，而不是依赖彼此。
//!
//! # 不变量（S1）
//!
//! **流式 delta 是唯一允许的易失路径。** 逐 token 不落持久日志；一条消息
//! 生成完毕后作为单个 entry 提交；链路中断时该条消息必须标记为不完整，
//! 不得伪装成完整消息。此例外**只允许存在于流式输出一处**，不得以"性能"
//! 为由扩大到工具结果、记忆写入或任何其它路径。
//!
//! # 已有接缝（不在此重复定义）
//!
//! | 接缝 | 现有 trait | 位置 |
//! |---|---|---|
//! | Llm | `crate::traits::LlmProvider` | `traits.rs` |
//! | Memory | `crate::traits::MemoryProvider` | `traits.rs` |
//! | Skills | `crate::traits::SkillProvider` | `traits.rs` |
//! | Legacy 执行（字符串面） | `crate::traits::TerminalBackend` | `traits.rs` |
//!
//! `TerminalBackend` 是合并前的执行世界（命令 + 文件读写，全部走 `String`）。
//! 新代码一律用本模块的 [`Fs`] / [`Shell`]；`TerminalBackend` 仅在迁移期保留。
//!
//! # MCP 的位置（S5）
//!
//! MCP 不是独立接缝。它只是"工具来源"的一种实现：把远端工具适配成
//! `ToolHandler` 后挂进工具注册表。默认不编译、不激活，只有用户显式配置
//! 才存在。内核不知道 MCP 这个概念。

pub mod local;

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// S4：结构化工具输出
// ---------------------------------------------------------------------------

/// 工具产物分类。决定前端如何渲染（S4 之前只能塞进字符串里猜）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttachmentKind {
    /// 位图（截图、生成的图）
    Image,
    /// 结构化表格
    Table,
    /// 任意文件
    File,
}

/// 工具产出的非文本负载。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolAttachment {
    pub kind: AttachmentKind,
    pub mime: String,
    pub data: Vec<u8>,
}

/// 工具的结构化返回值。替代裸 `String`：文本与二进制产物分开走，
/// 上层不必再靠"字符串里看着像 base64"来猜。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutput {
    /// 给模型 / 用户看的文本部分。
    pub text: String,
    /// 业务判定为失败（区别于 `Err`：Err 是执行未启动，这里是执行了但没成）。
    pub is_error: bool,
    pub attachments: Vec<ToolAttachment>,
}

impl ToolOutput {
    pub fn text(text: impl Into<String>) -> Self {
        Self { text: text.into(), is_error: false, attachments: Vec::new() }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self { text: text.into(), is_error: true, attachments: Vec::new() }
    }

    pub fn with_attachment(mut self, kind: AttachmentKind, mime: &str, data: Vec<u8>) -> Self {
        self.attachments.push(ToolAttachment { kind, mime: mime.to_string(), data });
        self
    }
}

// ---------------------------------------------------------------------------
// Fs：文件世界
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum FsError {
    #[error("path not found: {0}")]
    NotFound(PathBuf),
    #[error("path denied: {0}")]
    Denied(PathBuf),
    #[error("io error: {0}")]
    Io(String),
}

/// 目录项。只带元数据，不带内容，避免 `list` 一次性灌爆上下文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
}

#[async_trait]
pub trait Fs: Send + Sync {
    async fn read(&self, path: &Path) -> Result<Vec<u8>, FsError>;
    async fn write(&self, path: &Path, contents: &[u8]) -> Result<(), FsError>;
    async fn list(&self, path: &Path) -> Result<Vec<DirEntry>, FsError>;
    async fn remove(&self, path: &Path) -> Result<(), FsError>;

    /// 读文件**前 `max_lines` 行**（`0` = 不限，等价 [`Self::read`]）。
    ///
    /// 存在的理由（P1-10）：`read_file` 带 `offset`/`limit` 时原本也要把整个
    /// 文件读进内存再切行——读第 1 页 GB 级日志会 OOM。默认实现全读后截断，
    /// 保证所有既有实现零改动即满足契约；大文件实现（`LocalFs`）覆盖它做
    /// 流式读，只读到所需行就停。
    ///
    /// 返回**原始字节**（不规范化行尾）：调用方仍按原样做 UTF-8 校验与
    /// `slice_lines` 切片，所以输出与全读路径逐字一致。
    async fn read_lines(&self, path: &Path, max_lines: u64) -> Result<Vec<u8>, FsError> {
        let all = self.read(path).await?;
        Ok(truncate_bytes_at_line(&all, max_lines))
    }
}

/// 截到第 `max_lines` 行末（含该行换行符）；`max_lines == 0` 原样返回。
/// 最后一行没有换行符时按「不足 N 行」处理，原样返回全部。
pub fn truncate_bytes_at_line(all: &[u8], max_lines: u64) -> Vec<u8> {
    if max_lines == 0 {
        return all.to_vec();
    }
    let mut seen = 0u64;
    for (i, b) in all.iter().enumerate() {
        if *b == b'\n' {
            seen += 1;
            if seen >= max_lines {
                return all[..=i].to_vec();
            }
        }
    }
    all.to_vec()
}

// ---------------------------------------------------------------------------
// Shell：命令执行世界（决策 C：错误必须结构化）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// 失败分类。每条都自带**给模型的可执行指令**，避免把裸 stderr 回灌
/// 让模型盲试（同一类失败在 Windows 上有 N 种 stderr 面）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellErrorKind {
    CommandNotFound,
    EncodingMismatch,
    AccessDenied,
    FileLocked,
    ShellSyntax,
    Timeout,
    Unknown,
}

impl ShellErrorKind {
    /// 模型拿到后能直接照做的下一步，而不是"再试一次"。
    pub fn hint(&self) -> &'static str {
        match self {
            Self::CommandNotFound => "命令不存在：改用已安装的可执行文件完整路径，或换用等价的内置工具完成目标；不要重复调用同一条命令。",
            Self::EncodingMismatch => "输出编码不匹配（控制台非 UTF-8）：改用把结果写入文件再读文件的方式获取内容，不要用管道直接读取。",
            Self::AccessDenied => "权限不足：确认目标不在系统受保护目录，改用用户目录下路径；不要以提升权限的方式重试。",
            Self::FileLocked => "文件被占用：先结束持有该文件的进程，或写到新路径；不要立即重试同一路径。",
            Self::ShellSyntax => "命令语法错误：修正引号与转义后重发；对含空格的路径统一加引号。",
            Self::Timeout => "执行超时：拆分为更小的步骤，或改用后台执行并轮询；不要用相同命令原样重试。",
            Self::Unknown => "命令失败：先读 stderr 定位具体原因，再选择不同的做法；不要原样重复。",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ShellError {
    #[error("spawn failed: {0}")]
    Spawn(String),
    #[error("timeout")]
    Timeout,
    #[error("{kind:?}: {detail} — {hint}")]
    Classified {
        kind: ShellErrorKind,
        detail: String,
        hint: &'static str,
    },
}

impl ShellError {
    pub fn classified(kind: ShellErrorKind, detail: impl Into<String>) -> Self {
        Self::Classified { kind, detail: detail.into(), hint: kind.hint() }
    }

    pub fn kind(&self) -> ShellErrorKind {
        match self {
            Self::Spawn(_) => ShellErrorKind::Unknown,
            Self::Timeout => ShellErrorKind::Timeout,
            Self::Classified { kind, .. } => *kind,
        }
    }

    /// 给模型看的完整指令。调用方应把这个而不是裸 stderr 送进上下文。
    pub fn instruction(&self) -> String {
        match self {
            Self::Spawn(e) => format!("命令无法启动（{e}）：检查可执行文件是否存在后改用完整路径。"),
            Self::Timeout => ShellErrorKind::Timeout.hint().to_string(),
            Self::Classified { detail, hint, .. } => format!("{detail}\n{hint}"),
        }
    }
}

#[async_trait]
pub trait Shell: Send + Sync {
    async fn exec(&self, cmd: &str, cwd: &Path) -> Result<ExecOutput, ShellError>;

    /// 把执行结果归类为结构化错误码。返回 `None` 表示成功，不产生错误。
    fn classify(&self, out: &ExecOutput) -> Option<ShellErrorKind>;
}

// ---------------------------------------------------------------------------
// S2：Fs 与 Shell 共享一个执行世界
// ---------------------------------------------------------------------------

/// 一个 provider 同时提供文件与命令能力（S2）。换 provider 指向远程沙箱时，
/// 两者整体跟着搬走，不必给每个工具单独写分支。
pub trait ExecWorld: Send + Sync {
    fn fs(&self) -> &dyn Fs;
    fn shell(&self) -> &dyn Shell;
}

// ---------------------------------------------------------------------------
// Sandbox：能力边界（内核强制，模型无法争辩）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    FsRead { path: PathBuf },
    FsWrite { path: PathBuf },
    FsRemove { path: PathBuf },
    Shell { cmd: String },
    Net { host: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny,
    /// 需要人确认（审批）。默认策略只对删除类产生这个值。
    Escalate,
}

pub trait Sandbox: Send + Sync {
    fn allows(&self, op: &Op) -> Verdict;
}

// ---------------------------------------------------------------------------
// Transport：对外暴露面（决策 D：默认关闭）
// ---------------------------------------------------------------------------

/// 传输信封。`payload` 是已提交条目的序列化结果——只有提交过的东西能发出去。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub session: String,
    pub kind: String,
    pub payload: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// 未启用对外暴露面。这是默认状态，不是故障。
    #[error("transport disabled: network exposure requires explicit opt-in + pairing")]
    Disabled,
    #[error("transport failed: {0}")]
    Failed(String),
}

#[async_trait]
pub trait Transport: Send + Sync {
    async fn publish(&self, env: &Envelope) -> Result<(), TransportError>;
}

#[cfg(test)]
mod tests {
    use super::local::{LocalExecWorld, LocalFs, LocalSandbox, NoopTransport};
    use super::*;
    use crate::errors::ToolError;
    use crate::tool_schema::{JsonSchema, ToolSchema};
    use crate::traits::ToolHandler as _;
    use serde_json::Value;

    #[test]
    fn truncate_bytes_at_line_counts_newlines() {
        assert_eq!(truncate_bytes_at_line(b"a\nb\nc\n", 2), b"a\nb\n");
        assert_eq!(truncate_bytes_at_line(b"a\nb\nc\n", 0), b"a\nb\nc\n");
        assert_eq!(truncate_bytes_at_line(b"a\nb\nc\n", 99), b"a\nb\nc\n");
        // 末行无换行符：按「不足 N 行」返回全部；只有 1 行时切在换行符上。
        assert_eq!(truncate_bytes_at_line(b"a\nb", 2), b"a\nb");
        assert_eq!(truncate_bytes_at_line(b"a\nb", 1), b"a\n");
        assert_eq!(truncate_bytes_at_line(b"", 3), b"");
    }

    // --- S4：未迁移的工具自动获得结构化输出 ---

    struct LegacyTool;

    #[async_trait]
    impl crate::traits::ToolHandler for LegacyTool {
        async fn execute(&self, _params: Value) -> Result<String, ToolError> {
            Ok("done".to_string())
        }
        fn schema(&self) -> ToolSchema {
            ToolSchema::new("legacy", "legacy tool", JsonSchema::new("object"))
        }
    }

    #[tokio::test]
    async fn legacy_tool_gets_structured_output_for_free() {
        let out = LegacyTool.execute_structured(Value::Null).await.unwrap();
        assert_eq!(out, ToolOutput::text("done"));
        assert!(!out.is_error);
        assert!(out.attachments.is_empty());
    }

    // --- 决策 C：错误结构化且带可执行指令 ---

    #[test]
    fn every_error_kind_carries_an_actionable_hint() {
        for kind in [
            ShellErrorKind::CommandNotFound,
            ShellErrorKind::EncodingMismatch,
            ShellErrorKind::AccessDenied,
            ShellErrorKind::FileLocked,
            ShellErrorKind::ShellSyntax,
            ShellErrorKind::Timeout,
            ShellErrorKind::Unknown,
        ] {
            assert!(!kind.hint().is_empty(), "{kind:?} has no hint");
            assert!(!kind.hint().contains("再试一次"));
        }
    }

    #[test]
    fn instruction_includes_both_detail_and_hint() {
        let e = ShellError::classified(ShellErrorKind::CommandNotFound, "foo 不是内部或外部命令");
        assert_eq!(e.kind(), ShellErrorKind::CommandNotFound);
        let msg = e.instruction();
        assert!(msg.contains("不是内部或外部命令"));
        assert!(msg.contains("不要重复调用"));
    }

    // --- 决策 D：Transport 默认关闭 ---

    #[tokio::test]
    async fn transport_is_disabled_by_default() {
        let env = Envelope { session: "s".into(), kind: "entry".into(), payload: vec![1, 2, 3] };
        let err = NoopTransport.publish(&env).await.unwrap_err();
        assert!(matches!(err, TransportError::Disabled));
    }

    // --- Sandbox：只有删除类需要人确认 ---

    #[test]
    fn sandbox_escalates_only_on_removal() {
        let sb = LocalSandbox::default();
        assert_eq!(sb.allows(&Op::FsRead { path: PathBuf::from("a.txt") }), Verdict::Allow);
        assert_eq!(sb.allows(&Op::FsWrite { path: PathBuf::from("a.txt") }), Verdict::Allow);
        assert_eq!(sb.allows(&Op::Shell { cmd: "cargo check".into() }), Verdict::Allow);
        assert_eq!(sb.allows(&Op::FsRemove { path: PathBuf::from("a.txt") }), Verdict::Escalate);
    }

    // --- S2 换 provider：上层只依赖 ExecWorld，文件系统换了也照样跑 ---

    /// 只依赖接缝的上层逻辑，完全不知道底下是磁盘还是内存。
    async fn count_entries(world: &dyn ExecWorld, dir: &Path) -> usize {
        world.fs().list(dir).await.unwrap().len()
    }

    #[tokio::test]
    async fn swapping_fs_provider_is_invisible_to_callers() {
        let dir = tempfile::tempdir().unwrap();
        let world = LocalExecWorld::new();
        let real = world.fs();
        real.write(&dir.path().join("a.txt"), b"x").await.unwrap();
        real.write(&dir.path().join("b.txt"), b"y").await.unwrap();
        assert_eq!(count_entries(&world, dir.path()).await, 2);

        let mem = local::InMemoryFs::default();
        mem.write(Path::new("/m/one"), b"1").await.unwrap();
        let mem_world = local::FixedExecWorld::new(mem, local::NullShell);
        assert_eq!(count_entries(&mem_world, Path::new("/m")).await, 1);
    }

    #[tokio::test]
    async fn local_fs_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let fs = LocalFs::default();
        let p = dir.path().join("f.bin");
        fs.write(&p, b"hello").await.unwrap();
        assert_eq!(fs.read(&p).await.unwrap(), b"hello".to_vec());
        let entries = fs.list(dir.path()).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].is_dir);
        fs.remove(&p).await.unwrap();
        assert!(matches!(fs.read(&p).await, Err(FsError::NotFound(_))));
    }
}
