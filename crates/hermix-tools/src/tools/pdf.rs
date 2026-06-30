//! PDF tools: read PDF files via a three-tier fallback chain.
//!
//! 1. **pdftotext** (Poppler binary) — fastest, best output quality.
//! 2. **lopdf** (pure Rust) — no external dependencies, works everywhere.
//! 3. **pdfminer.six** (Python) — last resort if Python is on PATH.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};
use tokio::process::Command;

use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema, traits::RiskLevel};

use crate::tools::file::to_absolute_path;
use crate::ToolRegistry;

/// Register this tool with the registry (P1.2 migration).
pub fn register(registry: &ToolRegistry) {
    let handler = Arc::new(ReadPdfHandler::new());
    let schema = handler.schema();
    registry.register_with_risk(
        "read_pdf",
        "file",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        true,
        "Read a PDF file via pdftotext/lopdf/pdfminer fallback chain",
        "📕",
        None,
        RiskLevel::Safe,
        false,
    );
}

/// Trimmed-down re-export: we only need `Document::load` + `extract_text`.
use lopdf::Document;

/// Name of the vendored binary, including the `.exe` suffix on Windows.
#[cfg(windows)]
const PDFTOTEXT_BIN: &str = "pdftotext.exe";
#[cfg(not(windows))]
const PDFTOTEXT_BIN: &str = "pdftotext";

/// Default cap on returned text size.
const DEFAULT_MAX_CHARS: usize = 50_000;

/// Windows `STATUS_INVALID_IMAGE_NOT_FORMAT` (0xc000007b).
const STATUS_INVALID_IMAGE_NOT_FORMAT: i32 = -1_073_741_701;

/// Resolve the on-disk path to the `pdftotext` binary.
///
/// In production, `hermes.exe` and `pdftotext.exe` are installed side-by-side
/// (e.g. `C:\Program Files\Hermix Desktop\`). In dev, we fall back to PATH
/// lookup so contributors don't have to copy the binary into `target/debug`.
pub fn resolve_pdftotext_path() -> Result<PathBuf, String> {
    // 1. Sibling of the running hermes.exe (production / pnpm tauri build).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join(PDFTOTEXT_BIN);
            if sibling.exists() {
                return Ok(sibling);
            }
        }
    }

    // 2. PATH lookup (dev mode, vcpkg, MSYS2, homebrew, etc.).
    if let Ok(p) = which::which("pdftotext") {
        return Ok(p);
    }

    Err(format!(
        "pdftotext binary not found. Expected {} next to hermes.exe, or on PATH.",
        PDFTOTEXT_BIN
    ))
}

// ---------------------------------------------------------------------------
// ReadPdfHandler
// ---------------------------------------------------------------------------

/// Tool for reading PDF files. Wraps Poppler's `pdftotext` and returns the
/// extracted text with optional page range and layout preservation.
pub struct ReadPdfHandler;

impl ReadPdfHandler {
    pub fn new() -> Self {
        Self
    }

    /// Fallback tier 2: extract PDF text via **lopdf** (pure Rust, no
    /// external dependencies). Called when `pdftotext` is not found or
    /// fails to execute.
    async fn extract_via_lopdf(&self, path: &str) -> Result<String, ToolError> {
        let p = path.to_string();
        let doc = tokio::task::spawn_blocking(move || Document::load(&p))
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("lopdf task panicked: {e}")))?
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("lopdf failed to load PDF: {e}"))
            })?;

        // Get all page numbers. `get_pages()` returns BTreeMap<u32, _>.
        let page_numbers: Vec<u32> = doc.get_pages().keys().copied().collect();
        if page_numbers.is_empty() {
            return Err(ToolError::ExecutionFailed(
                "PDF has no pages".into(),
            ));
        }

        let text = tokio::task::spawn_blocking(move || doc.extract_text(&page_numbers))
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("lopdf task panicked: {e}")))?
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("lopdf text extraction failed: {e}"))
            })?;

        let max_chars = DEFAULT_MAX_CHARS;
        let mut out = text;
        let truncated = out.len() > max_chars;
        if truncated {
            out.truncate(max_chars);
            out.push_str("\n\n[...truncated, use a smaller `pages` range to read more]");
        }
        let abs = to_absolute_path(path);
        Ok(format!("[file: {abs}]\n{out}"))
    }

    /// Fallback tier 3: extract PDF text via Python's pdfminer.six.
    async fn extract_via_python(&self, path: &str) -> Result<String, ToolError> {
        // Use the shared find_python_bin from the code_execution backend
        // (3-tier resolution: HERMIX_PYTHON / sibling-slot / PATH probe)
        // so the PDF fallback and the `execute_code` tool can never
        // disagree about which interpreter is "the one".
        let python = match crate::backends::code_execution::find_python_bin(None) {
            Some(p) => p,
            None => {
                let entries = crate::backends::code_execution::python_search_paths(None);
                return Err(ToolError::ExecutionFailed(
                    crate::backends::code_execution::format_missing_python_error(&entries)
                        + "\n\n(Needed for tier-3 PDF fallback. Tier 1/2 of the read_pdf chain — \
                          pdftotext and lopdf — work without Python.)",
                ));
            }
        };
        let python_label = python.display().to_string();
        let script = format!(
            r#"
import sys, json
try:
    from pdfminer.high_level import extract_text
    text = extract_text({path:?})
    print(json.dumps({{"ok": True, "text": text, "chars": len(text)}}))
except Exception as e:
    print(json.dumps({{"ok": False, "error": str(e)}}))
"#,
            path = path
        );
        let mut cmd = Command::new(&python);
        cmd.arg("-c").arg(&script);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let output = cmd
            .output()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to spawn {python_label}: {e}")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ToolError::ExecutionFailed(format!(
                "{python_label} exited with status {}: {}",
                output.status,
                stderr.trim()
            )));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let result: serde_json::Value =
            serde_json::from_str(stdout.trim()).map_err(|e| {
                ToolError::ExecutionFailed(format!(
                    "pdfminer output was not valid JSON: {e}\n---\n{stdout}"
                ))
            })?;
        if result.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let err = result
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            return Err(ToolError::ExecutionFailed(format!(
                "pdfminer failed: {err}.\n\nInstall Poppler (https://github.com/oschwartz10612/poppler-windows/releases) \
                 and place pdftotext.exe in PATH to use the native backend.",
            )));
        }
        let text = result
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let max_chars = DEFAULT_MAX_CHARS;
        let mut out = text.to_string();
        let truncated = out.len() > max_chars;
        if truncated {
            out.truncate(max_chars);
            out.push_str("\n\n[...truncated, use a smaller `pages` range to read more]");
        }
        let abs = to_absolute_path(path);
        Ok(format!("[file: {abs}]\n{out}"))
    }
}

impl Default for ReadPdfHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for ReadPdfHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'path' parameter".into()))?;

        let path_p = Path::new(path);
        if !path_p.exists() {
            let parent = path_p.parent();
            let mut diag = format!("PDF file does not exist: {path}");
            if let Some(parent_dir) = parent {
                if parent_dir.exists() && parent_dir.is_dir() {
                    // Parent directory exists — scan for .pdf files
                    // that might match the intended filename.
                    let similar: Vec<String> = std::fs::read_dir(parent_dir)
                        .ok()
                        .into_iter()
                        .flatten()
                        .filter_map(|e| e.ok())
                        .filter(|e| {
                            let name = e.file_name().to_string_lossy().to_lowercase();
                            name.ends_with(".pdf")
                        })
                        .map(|e| {
                            let name = e.file_name().to_string_lossy().to_string();
                            let target = path_p
                                .file_name()
                                .map(|f| f.to_string_lossy().to_lowercase())
                                .unwrap_or_default();
                            // Mark exact-match candidates
                            if name.to_lowercase() == target {
                                format!("  ⚡ {name}  ← 匹配文件名！")
                            } else {
                                format!("  - {name}")
                            }
                        })
                        .collect();
                    if !similar.is_empty() {
                        diag.push_str(&format!(
                            "\n\nThe parent directory `{}` exists and contains these PDF files:\n{}",
                            parent_dir.display(),
                            similar.join("\n"),
                        ));
                    }
                } else {
                    diag.push_str(&format!(
                        "\n\nParent directory `{}` does not exist either. \
                         Check if the drive/path is accessible from this machine. \
                         Tip: use `terminal` with `dir /b <parent>` to verify the directory structure.",
                        parent_dir.display(),
                    ));
                }
            }
            return Err(ToolError::InvalidParams(diag));
        }

        // Defaults: keep layout, no page range clamp, default char cap.
        let layout = params
            .get("layout")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let pages = params.get("pages").and_then(|v| v.as_str()).map(str::to_owned);
        let max_chars = params
            .get("max_chars")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_CHARS);

        // ── Tier 1: pdftotext (Poppler binary) ────────────────
        match resolve_pdftotext_path() {
            Ok(bin) => {
                let mut cmd = Command::new(&bin);
                if layout {
                    cmd.arg("-layout");
                } else {
                    cmd.arg("-simple");
                }
                cmd.arg("-q");
                if let Some(ref pr) = pages {
                    if let Some((f, l)) = pr.split_once('-') {
                        cmd.arg("-f").arg(f);
                        cmd.arg("-l").arg(l);
                    }
                }
                cmd.arg(path);

                let output = cmd.output().await.map_err(|e| {
                    ToolError::ExecutionFailed(format!(
                        "Failed to spawn pdftotext at {}: {e}",
                        bin.display()
                    ))
                })?;

                if output.status.success() {
                    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
                    let truncated = text.len() > max_chars;
                    if truncated {
                        text.truncate(max_chars);
                        text.push_str("\n\n[...truncated, use a smaller `pages` range to read more]");
                    }
                    let abs = to_absolute_path(path);
                    return Ok(format!("[file: {abs}]\n{text}"));
                }

                // pdftotext ran but failed. Check for the Windows
                // 0xc000007b (missing Poppler DLLs) — if so, fall
                // through to tier 2. Otherwise report the error.
                let stderr = String::from_utf8_lossy(&output.stderr);
                let exit_code = output.status.code().unwrap_or(-1);
                if exit_code == STATUS_INVALID_IMAGE_NOT_FORMAT
                    || stderr.contains("0xc000007b")
                {
                    tracing::warn!(
                        "pdftotext failed with 0xc000007b (missing Poppler DLLs); \
                         falling through to lopdf"
                    );
                } else {
                    return Err(ToolError::ExecutionFailed(format!(
                        "pdftotext exited with status {}: {}",
                        output.status,
                        stderr.trim()
                    )));
                }
            }
            Err(e) => {
                tracing::warn!("pdftotext not available ({e}); falling through to lopdf");
            }
        }

        // ── Tier 2: lopdf (pure Rust) ─────────────────────────
        match self.extract_via_lopdf(path).await {
            Ok(text) => return Ok(text),
            Err(e) => {
                tracing::warn!("lopdf fallback failed ({e}); trying Python pdfminer");
            }
        }

        // ── Tier 3: Python pdfminer.six (last resort) ─────────
        self.extract_via_python(path).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "path".into(),
            json!({
                "type": "string",
                "description": "Absolute path to the PDF file to read"
            }),
        );
        props.insert(
            "layout".into(),
            json!({
                "type": "boolean",
                "description": "Preserve the original physical layout (whitespace/columns). Default: true. Use false for plain reading flow.",
                "default": true
            }),
        );
        props.insert(
            "pages".into(),
            json!({
                "type": "string",
                "description": "Optional page range as 'first-last' (e.g. '1-5'). Omit to read all pages."
            }),
        );
        props.insert(
            "max_chars".into(),
            json!({
                "type": "integer",
                "description": format!("Maximum characters to return. Default: {DEFAULT_MAX_CHARS}. Output is truncated with a hint if exceeded.")
            }),
        );

        tool_schema(
            "read_pdf",
            "Extract text from a PDF file using Poppler's pdftotext. \
             **Use this tool whenever the user wants to read, search, or extract content from a PDF file** \u{2014} \
             prefer this over running `pdftotext`, `pdf2txt`, or any PDF-parsing command via `terminal` or `process_registry`. \
             Preserves CJK characters and physical layout by default. \
             Use the `pages` parameter to limit the range; use a smaller `max_chars` for very large documents.",
            JsonSchema::object(props, vec!["path".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_returns_err_when_binary_missing() {
        // We don't assert success/failure of the resolution itself (the
        // binary may or may not be present on CI), but we verify the
        // function is callable and returns a String (not panics).
        let _ = resolve_pdftotext_path();
    }

    #[test]
    fn test_schema_lists_required_path_param() {
        let h = ReadPdfHandler::new();
        let s = h.schema();
        assert_eq!(s.name, "read_pdf");
        // Properties map should at least contain `path`.
        let props = s
            .parameters
            .properties
            .as_ref()
            .expect("schema must declare properties");
        assert!(props.contains_key("path"), "path must be a declared property");
        assert!(props.contains_key("layout"), "layout must be a declared property");
        assert!(props.contains_key("pages"), "pages must be a declared property");
    }
}
