//! MarkItDown tool — convert local documents to LLM-friendly Markdown.
//!
//! Wraps the bundled `markitdown` sidecar binary (a pure-Rust port of
//! microsoft/markitdown) so the LLM can turn PDF / PPTX / DOCX / XLSX /
//! HTML / EPUB / CSV / JSON / XML / MSG / ZIP and common image files into
//! compact Markdown.
//!
//! ## Why this saves tokens
//!
//! Dumping a raw document (or even raw `read_file` text) into the
//! conversation context is wasteful: layouts, binary blobs, and
//! boilerplate inflate the prompt. `markitdown` extracts the *semantic*
//! content as Markdown — the minimal representation an LLM can reason
//! over — which is exactly the context-bloat prevention principle in
//! `AGENTS.md`. Use it as the default reader for any non-plaintext
//! document.
//!
//! ## Implementation notes
//!
//! - `resolve_binary()` mirrors the sidecar resolution: the binary is
//!   bundled next to the running executable (production), under a
//!   `binaries/` sibling, or under `../src-tauri/binaries/` (dev /
//!   unpackaged layouts).
//! - Shelling out goes through the shared `TerminalBackend` (same path
//!   as the other sidecars), which gives us a bounded timeout for free and
//!   keeps every subprocess launch in one consistent backend.
//! - Local-only: hermix must not reach out to the network (PR禁区:
//!   no external resource sensing), so URLs are rejected up front.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};
use std::sync::Arc;

use hermix_core::{
    tool_schema, CommandOutput, JsonSchema, TerminalBackend, ToolError, ToolHandler, ToolSchema,
    traits::RiskLevel,
};

use crate::ToolRegistry;

pub struct MarkitdownHandler {
    backend: std::sync::Arc<dyn TerminalBackend>,
}

impl MarkitdownHandler {
    pub fn new(backend: std::sync::Arc<dyn TerminalBackend>) -> Self {
        Self { backend }
    }

    /// Resolve the bundled `markitdown` sidecar binary.
    ///
    /// Mirrors the sidecar resolution: the binary
    /// is bundled next to the running executable (production), under a
    /// `binaries/` sibling, or under `../src-tauri/binaries/` (dev /
    /// unpackaged layouts).
    fn resolve_binary() -> Result<String, ToolError> {
        let candidates: Vec<std::path::PathBuf> = vec![
            std::path::PathBuf::from("markitdown-x86_64-pc-windows-msvc.exe"),
            {
                let exe = std::env::current_exe().unwrap_or_default();
                exe.parent()
                    .map(|p| {
                        p.join("binaries")
                            .join("markitdown-x86_64-pc-windows-msvc.exe")
                    })
                    .unwrap_or_default()
            },
            {
                let exe = std::env::current_exe().unwrap_or_default();
                exe.parent()
                    .map(|p| {
                        p.join("..")
                            .join("src-tauri")
                            .join("binaries")
                            .join("markitdown-x86_64-pc-windows-msvc.exe")
                    })
                    .unwrap_or_default()
            },
        ];
        for c in &candidates {
            if c.exists() {
                return Ok(c.to_string_lossy().to_string());
            }
        }
        Err(ToolError::ExecutionFailed(
            "markitdown binary not found. Please ensure it is bundled with the application.".to_string(),
        ))
    }
}

fn format_output(output: &CommandOutput) -> String {
    let mut parts = Vec::new();
    if !output.stdout.is_empty() {
        parts.push(output.stdout.clone());
    }
    if !output.stderr.is_empty() {
        parts.push(format!("[stderr] {}", output.stderr));
    }
    if output.exit_code != 0 {
        parts.push(format!("[exit code] {}", output.exit_code));
    }
    if parts.is_empty() {
        "(no output)".to_string()
    } else {
        parts.join("\n")
    }
}

#[async_trait]
impl ToolHandler for MarkitdownHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'path' parameter".into()))?;

        // Local-only: hermix must not fetch remote resources (PR禁区:
        // no external resource sensing). Reject URLs up front so the
        // agent never accidentally triggers a network call.
        if path.starts_with("http://")
            || path.starts_with("https://")
            || path.starts_with("ftp://")
        {
            return Err(ToolError::InvalidParams(
                "markitdown only accepts local file paths; URLs are not supported in this build.".into(),
            ));
        }

        let binary = Self::resolve_binary()?;

        // Build the argument list, then quote each arg the same way
        // the other sidecars do so Windows paths with spaces / backslashes
        // survive the shell the TerminalBackend ultimately invokes.
        // `markitdown` auto-detects the file type from its extension /
        // magic number, so no explicit type flag is needed here (the CLI
        // only honours `--ext` for stdin input anyway).
        let mut args = vec![path.to_string()];
        // v0.1.1+: `--max-chars` caps the output length for ALL formats —
        // the token-saving guard for huge documents.
        if let Some(max_chars) = params.get("max_chars").and_then(|v| v.as_u64()) {
            args.push("--max-chars".to_string());
            args.push(max_chars.to_string());
        }

        let command_str = format!(
            "{} {}",
            binary,
            args.iter()
                .map(|a| {
                    if a.contains(' ') || a.contains('"') || a.contains('\\') {
                        format!("\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\""))
                    } else {
                        a.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        );

        let timeout: u64 = params
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(60_000);

        match self
            .backend
            .execute_command(&command_str, Some(timeout), None, false, false, None)
            .await
        {
            Ok(output) => {
                if output.exit_code != 0 {
                    return Err(ToolError::ExecutionFailed(format_output(&output)));
                }
                let text = format_output(&output);
                if text.trim().is_empty() {
                    return Err(ToolError::ExecutionFailed(
                        "markitdown produced no output for this file (unsupported or empty format?).".into(),
                    ));
                }
                Ok(text)
            }
            Err(e) => Err(ToolError::ExecutionFailed(e.to_string())),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "path".into(),
            json!({
                "type": "string",
                "description": "Absolute path to a LOCAL document. Supports PDF, PPTX, DOCX, XLSX, HTML, EPUB, CSV, JSON, XML, MSG, ZIP, and common images. URLs are rejected."
            }),
        );
        props.insert(
            "timeout".into(),
            json!({
                "type": "integer",
                "description": "Conversion timeout in milliseconds (default: 60000)"
            }),
        );
        props.insert(
            "max_chars".into(),
            json!({
                "type": "integer",
                "description": "Optional output cap in characters (maps to CLI --max-chars, all formats). Protects context from huge documents; default: no cap."
            }),
        );

        tool_schema(
            "markitdown",
            "Convert a LOCAL document (PDF/PPTX/DOCX/XLSX/HTML/EPUB/CSV/JSON/XML/MSG/ZIP/image) into \
             compact LLM-friendly Markdown. Use this to read the *content* of non-plaintext files \
             cheaply instead of dumping raw bytes into context — it drastically cuts token usage. \
             Local files only; URLs are rejected.\n\n\
             ## When to use\n\
             - You need the text/content of a .docx/.pptx/.xlsx/.html/.epub/.csv/.pdf/etc. for reasoning.\n\
             - You want the token-cheapest representation of a document.\n\n\
             ## When NOT to use\n\
             - Plain text / code files → `read_file` (direct, keeps line numbers).\n\
             - Long documents or multi-doc comparison where you only need conclusions → `delegate_task` (keeps the main context lean); pass paths, not contents.\n\
             - You need a *modified* file, not the extracted text — this tool is read-only.\n\n\
             ## PDF 分工 (v0.1.1+)\n\
             - **文本型 PDF**：本工具直接转换，默认路径已带轻量结构化（编号列表 / 编号小节标题转为 Markdown），可传 `max_chars` 截断大文档。\n\
             - **扫描件 / 图片型 PDF**（转换结果为空或乱码）：本工具无 OCR，改用 `read_image` 走 LLM 视觉（本地 OCR 已从本构建移除）。\n\
             - 拿不准 PDF 类型时：先本工具，空/乱 → `read_image`。\n\n\
             ## Tips\n\
             - markitdown auto-detects the file type from its extension / magic number — just pass the path.\n\
             - Non-PDF formats all go through markitdown.",
            JsonSchema::object(props, vec!["path".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the `markitdown` tool with the registry (P1.8-style).
///
/// Tagged with the dedicated `"document"` toolset so
/// `essential::ESSENTIAL_TOOLSETS` lists it as a floor entry and can
/// re-assert it; without that, a user `enabled_toolsets.json` could
/// silently disable the token-saving document reader. Shares the
/// `terminal_backend` with `file` so every subprocess
/// launch goes through one consistent backend.
pub fn register(registry: &ToolRegistry, terminal_backend: Arc<dyn TerminalBackend>) {
    let handler = Arc::new(MarkitdownHandler::new(terminal_backend));
    let schema = handler.schema();
    registry.register_with_risk(
        "markitdown",
        "document",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        true,
        "Convert a local document into LLM-friendly Markdown (PDF/PPTX/DOCX/XLSX/HTML/EPUB/CSV/JSON/XML/MSG/ZIP/image). Token-saving file reader.",
        "📄",
        None,
        RiskLevel::Safe,
        false,
    );
}
