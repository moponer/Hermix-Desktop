//! File tools: read, write, patch, and search

use async_trait::async_trait;
use base64::engine::general_purpose;
use base64::Engine;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_core::{
    tool_schema, traits::RiskLevel, JsonSchema, TerminalBackend, ToolError, ToolHandler, ToolSchema,
};

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::credential_guard::CredentialGuard;
use crate::ToolRegistry;

// ---------------------------------------------------------------------------
// Registration (P1.3 migration: file tools move out of register_builtins.rs)
// ---------------------------------------------------------------------------

/// Register the four file tools: `read_file`, `write_file`, `patch`,
/// `search_files`. The same `terminal_backend` shared with the terminal
/// tool is reused so that read/write go through the same path the
/// shell uses; patch/search get their own local backends.
///
/// The handler registration mirrors the toolset tag + risk levels that
/// `register_builtins.rs` had before the migration:
/// - `read_file` → Safe (read-only, no env deps)
/// - `write_file` → Caution (writes to disk)
/// - `patch` → Caution (mutating edits)
/// - `search_files` → Caution (no destructive power, but the file-
///   toolset is uniformly Caution for consistency)
pub fn register(registry: &ToolRegistry, terminal_backend: Arc<dyn TerminalBackend>) {
    use crate::backends::file::{LocalPatchBackend, LocalSearchBackend};

    registry.register_with_risk(
        "read_file",
        "file",
        ReadFileHandler::new(terminal_backend.clone()).schema(),
        Arc::new(ReadFileHandler::new(terminal_backend.clone())),
        Arc::new(|| true),
        vec![],
        true,
        "Read file contents with optional offset and line limit. Returns the file content as a string with line numbers.",
        "📖",
        None,
        RiskLevel::Safe,
        /* override_existing = */ false,
    );
    registry.register_with_risk(
        "write_file",
        "file",
        WriteFileHandler::new(terminal_backend.clone()).schema(),
        Arc::new(WriteFileHandler::new(terminal_backend)),
        Arc::new(|| true),
        vec![],
        true,
        "Write content to a file. Creates the file and parent directories if they don't exist. Overwrites existing content.",
        "✏️",
        None,
        RiskLevel::Caution,
        /* override_existing = */ false,
    );
    let patch_backend: Arc<dyn PatchBackend> = Arc::new(LocalPatchBackend::new());
    registry.register_with_risk(
        "patch",
        "file",
        PatchHandler::new(patch_backend.clone()).schema(),
        Arc::new(PatchHandler::new(patch_backend)),
        Arc::new(|| true),
        vec![],
        true,
        "Apply targeted find-and-replace edits to a file using fuzzy matching. Minor whitespace/indentation differences won't break matching.",
        "🩹",
        None,
        RiskLevel::Caution,
        /* override_existing = */ false,
    );
    let search_backend: Arc<dyn SearchBackend> = Arc::new(LocalSearchBackend::new());
    registry.register_with_risk(
        "search_files",
        "file",
        SearchFilesHandler::new(search_backend.clone()).schema(),
        Arc::new(SearchFilesHandler::new(search_backend)),
        Arc::new(|| true),
        vec![],
        true,
        "Search file contents (ripgrep-backed regex) or find files by glob pattern.",
        "🔎",
        None,
        RiskLevel::Caution,
        /* override_existing = */ false,
    );
    // read_image — free local multimodal image reader
    registry.register_with_risk(
        "read_image",
        "file",
        ReadImageHandler::new().schema(),
        Arc::new(ReadImageHandler::new()),
        Arc::new(|| true),
        vec![],
        true,
        "Read a local image file so the LLM can SEE it natively — free, no API key.",
        "🖼️",
        None,
        RiskLevel::Safe,
        /* override_existing = */ false,
    );
}

/// Resolve a path to an absolute string for display in LLM-visible tool
/// output. The rule is **literal pass-through**: the path the user typed or
/// configured is the path we report. We deliberately do NOT canonicalize.
///
/// Why no canonicalize: on Windows, `std::fs::canonicalize` follows
/// junctions / symlinks AND prepends the `\\?\` long-path prefix. So if the
/// user is in `D:\data` (a junction) and the LLM calls `read_file("D:\\qcc\\foo")`,
/// the OS opens the file fine, but `canonicalize` would surface
/// `\\?\F:\...` instead of `D:\data\foo` — the junction target, not what
/// the user typed. The LLM would then echo the wrong path back into
/// subsequent reasoning and tool calls, and `subdirectory_hints` /
/// `_files.txt` would all drift. The user's configured workspace root and
/// the path they typed are the source of truth.
///
/// For relative paths we still need a base directory to make the result
/// absolute, so we fall back to:
///
/// 1. `TERMINAL_CWD` env var (set by `LocalAgentService::send_message_stream`
///    from the per-turn `AgentOverrides::workspace`, i.e. the desktop
///    "工作区" picker).
/// 2. Server process current working directory.
///
/// The cwd itself is not canonicalized — same reason.
///
/// Trade-off: `..` segments are not resolved in the display string. This is
/// fine; the OS resolves `..` on actual file open, and the LLM-visible
/// header doesn't need to be "lexically resolved".
pub fn to_absolute_path(path: &str) -> String {
    let p = Path::new(path);
    if p.is_absolute() {
        return p.to_string_lossy().to_string();
    }
    let cwd = std::env::var("TERMINAL_CWD")
        .ok()
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    cwd.join(p).to_string_lossy().to_string()
}

// ---------------------------------------------------------------------------
// ReadFileHandler
// ---------------------------------------------------------------------------

/// Tool for reading file contents via the terminal backend.
pub struct ReadFileHandler {
    backend: Arc<dyn TerminalBackend>,
}

impl ReadFileHandler {
    pub fn new(backend: Arc<dyn TerminalBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for ReadFileHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'path' parameter".into()))?;

        CredentialGuard::new().check_read_access(Path::new(path))?;

        let offset = params.get("offset").and_then(|v| v.as_u64());

        let limit = params.get("limit").and_then(|v| v.as_u64());

        self.backend
            .read_file(path, offset, limit)
            .await
            .map_err(|e| ToolError::from_message(e.to_string()))
            .map(|content| {
                // Prefix the content with an absolute-path header so the LLM
                // (and the front-end artifact extractor) can unambiguously
                // reference the file later. Format mirrors write_file.
                let abs = to_absolute_path(path);
                format!("[file: {abs}]\n{content}")
            })
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "path".into(),
            json!({
                "type": "string",
                "description": "The file path to read"
            }),
        );
        props.insert(
            "offset".into(),
            json!({
                "type": "integer",
                "description": "Line number to start reading from (1-indexed)"
            }),
        );
        props.insert(
            "limit".into(),
            json!({
                "type": "integer",
                "description": "Maximum number of lines to read"
            }),
        );

        tool_schema(
            "read_file",
            "Read file contents with optional offset and line limit. Returns the file content as a string with line numbers.",
            JsonSchema::object(props, vec!["path".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// WriteFileHandler
// ---------------------------------------------------------------------------

/// Tool for writing content to files via the terminal backend.
pub struct WriteFileHandler {
    backend: Arc<dyn TerminalBackend>,
}

impl WriteFileHandler {
    pub fn new(backend: Arc<dyn TerminalBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for WriteFileHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'path' parameter".into()))?;

        let content = params
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'content' parameter".into()))?;

        CredentialGuard::new().check_write_access(Path::new(path), content)?;

        self.backend
            .write_file(path, content)
            .await
            .map_err(|e| ToolError::from_message(e.to_string()))?;

        // Always report the absolute path so the LLM and front-end can
        // refer back to it without re-deriving. `to_absolute_path` falls back
        // to a non-canonicalized form if the file is on a path that can't be
        // canonicalized (rare, but happens with bind-mounts / junctions).
        let abs = to_absolute_path(path);
        Ok(format!(
            "Successfully wrote {} bytes to {}",
            content.len(),
            abs
        ))
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "path".into(),
            json!({
                "type": "string",
                "description": "The file path to write to"
            }),
        );
        props.insert(
            "content".into(),
            json!({
                "type": "string",
                "description": "The content to write to the file"
            }),
        );

        tool_schema(
            "write_file",
            "Write content to a file. Creates the file and parent directories if they don't exist. Overwrites existing content.",
            JsonSchema::object(props, vec!["path".into(), "content".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// PatchHandler
// ---------------------------------------------------------------------------

/// Backend trait for file patching operations.
#[async_trait]
pub trait PatchBackend: Send + Sync {
    /// Apply a patch to a file using fuzzy matching.
    async fn patch_file(
        &self,
        path: &str,
        old_string: &str,
        new_string: &str,
        replace_all: bool,
    ) -> Result<String, ToolError>;
}

/// Tool for patching files with fuzzy matching (find-and-replace).
pub struct PatchHandler {
    backend: Arc<dyn PatchBackend>,
}

impl PatchHandler {
    pub fn new(backend: Arc<dyn PatchBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for PatchHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'path' parameter".into()))?;

        let old_string = params
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'old_string' parameter".into()))?;

        let new_string = params
            .get("new_string")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let replace_all = params
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        self.backend
            .patch_file(path, old_string, new_string, replace_all)
            .await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "path".into(),
            json!({
                "type": "string",
                "description": "The file path to patch"
            }),
        );
        props.insert(
            "old_string".into(),
            json!({
                "type": "string",
                "description": "The text to find in the file (fuzzy matching supported)"
            }),
        );
        props.insert(
            "new_string".into(),
            json!({
                "type": "string",
                "description": "The replacement text (use empty string to delete)"
            }),
        );
        props.insert("replace_all".into(), json!({
            "type": "boolean",
            "description": "Replace all occurrences instead of requiring a unique match (default: false)",
            "default": false
        }));

        tool_schema(
            "patch",
            "Apply targeted find-and-replace edits to a file using fuzzy matching. Minor whitespace/indentation differences won't break matching.",
            JsonSchema::object(props, vec!["path".into(), "old_string".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// SearchFilesHandler
// ---------------------------------------------------------------------------

/// Backend trait for file search operations.
#[async_trait]
pub trait SearchBackend: Send + Sync {
    /// Search file contents by regex pattern.
    async fn search_content(
        &self,
        pattern: &str,
        path: &str,
        file_glob: Option<&str>,
        max_results: Option<usize>,
    ) -> Result<String, ToolError>;

    /// Search files by name (glob pattern).
    async fn search_files(&self, pattern: &str, path: &str) -> Result<String, ToolError>;
}

/// Tool for searching files by content or filename.
pub struct SearchFilesHandler {
    backend: Arc<dyn SearchBackend>,
}

impl SearchFilesHandler {
    pub fn new(backend: Arc<dyn SearchBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for SearchFilesHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let pattern = params
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'pattern' parameter".into()))?;

        let path = params.get("path").and_then(|v| v.as_str()).unwrap_or(".");

        let target = params
            .get("target")
            .and_then(|v| v.as_str())
            .unwrap_or("content");

        let file_glob = params.get("file_glob").and_then(|v| v.as_str());

        let max_results = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);

        match target {
            "content" => {
                self.backend
                    .search_content(pattern, path, file_glob, max_results)
                    .await
            }
            "files" => self.backend.search_files(pattern, path).await,
            other => Err(ToolError::InvalidParams(format!(
                "Unknown target: '{}'. Use 'content' or 'files'.",
                other
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "pattern".into(),
            json!({
                "type": "string",
                "description": "Regex pattern to search for (content) or glob pattern (files)"
            }),
        );
        props.insert(
            "path".into(),
            json!({
                "type": "string",
                "description": "Directory or file to search in (default: '.')"
            }),
        );
        props.insert("target".into(), json!({
            "type": "string",
            "description": "Search target: 'content' for file contents or 'files' for filenames",
            "enum": ["content", "files"],
            "default": "content"
        }));
        props.insert(
            "file_glob".into(),
            json!({
                "type": "string",
                "description": "Filter files by glob pattern when searching content (e.g. '*.py')"
            }),
        );
        props.insert(
            "limit".into(),
            json!({
                "type": "integer",
                "description": "Maximum number of results to return"
            }),
        );

        tool_schema(
            "search_files",
            "Search file contents (ripgrep-backed regex) or find files by glob pattern.\n\
             \n\
             ## When to reach for this tool\n\
             - **Find files by name** (e.g. \"where is the config?\" / \"show me the .toml files\") → use `target: \"files\"` or the `list_files` alias.\n\
             - **Search inside files** (e.g. \"where is the `effective_model` resolved?\" / \"grep TODO\") → use `target: \"content\"` (the default).\n\
             Much cheaper and more precise than `computer_use.screenshot` of an editor with Find-open.",
            JsonSchema::object(props, vec!["pattern".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// read_image — read a local image file and return base64 data
// for LLM multimodal injection (free, local, no API key).
// ---------------------------------------------------------------------------

/// Detect MIME type from file extension.
fn mime_from_ext(path: &str) -> &'static str {
    let p = path.to_lowercase();
    if p.ends_with(".png") { "image/png" }
    else if p.ends_with(".jpg") || p.ends_with(".jpeg") { "image/jpeg" }
    else if p.ends_with(".gif") { "image/gif" }
    else if p.ends_with(".webp") { "image/webp" }
    else if p.ends_with(".bmp") { "image/bmp" }
    else { "application/octet-stream" }
}

pub struct ReadImageHandler;

impl ReadImageHandler {
    pub fn new() -> Self { Self }
}

impl Default for ReadImageHandler {
    fn default() -> Self { Self::new() }
}

#[async_trait]
impl ToolHandler for ReadImageHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams(
                "missing 'path' — pass a local image file path (.png/.jpg/.gif/.webp)".into(),
            ))?;
        if path.is_empty() {
            return Err(ToolError::InvalidParams("'path' must be non-empty".into()));
        }

        let bytes = std::fs::read(path).map_err(|e| {
            ToolError::from_message(format!("cannot read image file '{path}': {e}"))
        })?;

        // Reject obviously non-image files (> 50 MB) to avoid base64-ing
        // huge binaries by mistake.
        if bytes.len() > 50 * 1024 * 1024 {
            return Err(ToolError::ExecutionFailed(format!(
                "file '{path}' is {} bytes — too large for image reading \
                 (max 50 MB). Use read_file for large files.",
                bytes.len()
            )));
        }

        let mime = mime_from_ext(path);
        let b64 = general_purpose::STANDARD.encode(&bytes);

        Ok(json!({
            "tool": "read_image",
            "ok": true,
            "path": path,
            "mime_type": mime,
            "size_bytes": bytes.len(),
            "_image_data": b64,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "path".into(),
            json!({
                "type": "string",
                "description": "Local image file path (PNG/JPEG/GIF/WEBP/BMP). Reads the file so the LLM can SEE it via native multimodal vision — free, offline, no API key."
            }),
        );
        tool_schema(
            "read_image",
            "Read a local image file so the LLM can SEE it using its native multimodal vision ability.\n\
             \n\
             Pass a `path` to any image file on disk. The tool reads the file, encodes it, \
             and injects it into the conversation so the LLM sees the actual pixels — just like \
             when you paste/drop an image into chat.\n\
             \n\
             ## What this does\n\
             - Reads the image file from disk\n\
             - Sends it to the LLM as multimodal content (the LLM 'sees' the picture)\n\
             - Free, local, no API key needed — uses the same multimodal pipeline as pasted images\n\
             \n\
             ## Supported formats\n\
             PNG, JPEG, GIF, WEBP, BMP (auto-detected from extension)\n\
             \n\
             ## Use cases\n\
             - Licence plates / ID cards / documents in photos\n\
             - Screenshots, diagrams, charts\n\
             - Any visual content you want the LLM to analyse\n\
             \n\
             ## Cost: FREE — no external API calls",
            JsonSchema::object(props, vec!["path".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_core::{AgentError, CommandOutput};

    #[test]
    fn read_image_returns_base64_for_png() {
        use serde_json::{json, Value};
        // Minimal valid 1x1 PNG (red pixel) — decodes to 67 bytes.
        let png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
        let bytes = base64::engine::general_purpose::STANDARD.decode(png_b64).unwrap();
        let p = std::env::temp_dir().join("read_image_test_1x1.png");
        std::fs::write(&p, &bytes).unwrap();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let out: Value = rt
            .block_on(async {
                let s = ReadImageHandler::new()
                    .execute(json!({ "path": p.to_str().unwrap() }))
                    .await
                    .unwrap();
                serde_json::from_str(&s).unwrap()
            });

        assert_eq!(out["ok"], true);
        assert_eq!(out["mime_type"], "image/png");
        assert!(out["_image_data"].as_str().unwrap().len() > 0);
        // The base64 we get back must round-trip the original file bytes.
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(out["_image_data"].as_str().unwrap())
            .unwrap();
        assert_eq!(decoded, bytes);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    #[ignore]
    fn read_image_reads_real_car_plate_image() {
        use serde_json::{json, Value};
        let p = std::env::temp_dir().join("test_car_plate.png");
        assert!(p.exists(), "generate the test image first (see audit notes)");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let out: Value = rt
            .block_on(async {
                let s = ReadImageHandler::new()
                    .execute(json!({ "path": p.to_str().unwrap() }))
                    .await
                    .unwrap();
                serde_json::from_str(&s).unwrap()
            });
        assert_eq!(out["ok"], true);
        assert_eq!(out["mime_type"], "image/png");
        let b64 = out["_image_data"].as_str().unwrap();
        assert!(b64.len() > 1000, "real image should produce a large base64 blob");
        println!("CAR_PLATE_IMAGE_BASE64_LEN={}", b64.len());
    }

    struct MockBackend;
    #[async_trait]
    impl TerminalBackend for MockBackend {
        async fn execute_command(
            &self,
            _cmd: &str,
            _timeout: Option<u64>,
            _workdir: Option<&str>,
            _bg: bool,
            _pty: bool,
        ) -> Result<CommandOutput, AgentError> {
            Ok(CommandOutput {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
        async fn read_file(
            &self,
            path: &str,
            _offset: Option<u64>,
            _limit: Option<u64>,
        ) -> Result<String, AgentError> {
            Ok(format!("contents of {}", path))
        }
        async fn write_file(&self, _path: &str, _content: &str) -> Result<(), AgentError> {
            Ok(())
        }
        async fn file_exists(&self, _path: &str) -> Result<bool, AgentError> {
            Ok(true)
        }
    }

    #[tokio::test]
    async fn test_read_file_handler() {
        let handler = ReadFileHandler::new(Arc::new(MockBackend));
        // Use the cross-platform temp dir instead of hardcoding
        // `/tmp/test.txt` — that path is Unix-only and was the
        // reason the test silently passed on Linux but always
        // errored on Windows. The MockBackend ignores the path
        // argument, so the only thing under test is "the
        // handler formats the success response correctly".
        let path = std::env::temp_dir().join("hermix_test_read.txt");
        let path_str = path.to_string_lossy().to_string();
        let result = handler
            .execute(json!({"path": path_str}))
            .await
            .unwrap();
        assert!(result.contains("contents of"));
    }

    #[tokio::test]
    async fn test_write_file_handler() {
        let handler = WriteFileHandler::new(Arc::new(MockBackend));
        let path = std::env::temp_dir().join("hermix_test_write.txt");
        let path_str = path.to_string_lossy().to_string();
        let result = handler
            .execute(json!({"path": path_str, "content": "hello"}))
            .await
            .unwrap();
        assert!(result.contains("Successfully wrote"));
    }

    #[tokio::test]
    async fn test_read_file_schema() {
        let handler = ReadFileHandler::new(Arc::new(MockBackend));
        let schema = handler.schema();
        assert_eq!(schema.name, "read_file");
    }

    #[tokio::test]
    async fn test_write_file_schema() {
        let handler = WriteFileHandler::new(Arc::new(MockBackend));
        let schema = handler.schema();
        assert_eq!(schema.name, "write_file");
    }

    // -- to_absolute_path: literal pass-through contract -----------------
    //
    // The path the user typed (or configured as workspace root) is the
    // path that must appear in LLM-visible tool output. No junction
    // resolution, no `\\?\` prefix, no `..` resolution.

    #[test]
    fn to_absolute_path_keeps_absolute_path_literal() {
        // Even when the path doesn't exist on disk (so canonicalize would
        // fail and the old code would have fallen through to is_absolute,
        // which is the behavior we preserve), the literal input is
        // returned as-is.
        let out = to_absolute_path("D:\\qcc\\never_existed\\foo.txt");
        assert_eq!(out, "D:\\qcc\\never_existed\\foo.txt");
    }

    #[test]
    fn to_absolute_path_does_not_canonicalize_junction() {
        // D:\data is a junction on this machine. The old code would call
        // canonicalize() and surface the junction target. The new code
        // must keep the user's literal path.
        let out = to_absolute_path("D:\\qcc\\Cargo.toml");
        assert_eq!(out, "D:\\qcc\\Cargo.toml");
        // The junction target prefix must NOT appear.
        assert!(!out.contains("F:\\spiker\\hemix-desktop"));
        assert!(!out.starts_with("\\\\?\\"));
    }

    #[test]
    fn to_absolute_path_relative_uses_terminal_cwd() {
        // Pin TERMINAL_CWD so the test is hermetic. This is the only
        // place we read the env var in the function, so cleanup is local.
        let prev = std::env::var("TERMINAL_CWD").ok();
        // SAFETY: tests in this module run on a single thread (cargo
        // test's default for `--test-threads=1` is opt-in; we rely on
        // the function reading TERMINAL_CWD on each call, not caching).
        std::env::set_var("TERMINAL_CWD", "D:\\qcc");
        let out = to_absolute_path("src\\main.rs");
        assert_eq!(out, "D:\\qcc\\src\\main.rs");
        match prev {
            Some(v) => std::env::set_var("TERMINAL_CWD", v),
            None => std::env::remove_var("TERMINAL_CWD"),
        }
    }
}
