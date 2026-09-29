//! File tools: read, write, patch, and search

use async_trait::async_trait;
use base64::engine::general_purpose;
use base64::Engine;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_core::{
    tool_schema, traits::RiskLevel, AttachmentKind, Fs, FsError, JsonSchema, TerminalBackend,
    ToolError, ToolHandler, ToolOutput, ToolSchema,
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
/// The handler registration mirrors the toolset tag + risk levels that
/// `register_builtins.rs` had before the migration:
/// - `read_file` → Safe (read-only, no env deps)
/// - `write_file` → Caution (writes to disk)
/// - `patch` → Caution (mutating edits)
/// - `search_files` → Caution (no destructive power, but the file-
/// toolset is uniformly Caution for consistency)
pub fn register(
    registry: &ToolRegistry,
    terminal_backend: Arc<dyn TerminalBackend>,
    // 接缝 `Fs`：主进程装配注入。`None` 时 ReadFileHandler/WriteFileHandler
    // 走 TerminalBackend（带 offset/limit 的旧路径）；提供了就走 Fs seam
    // （不带 offset/limit 的简单路径）。tests + 没装配的调用方传 None。
    fs: Option<Arc<dyn Fs>>,
) {
    use crate::backends::file::{LocalPatchBackend, LocalSearchBackend};

    registry.register_with_risk(
        "read_file",
        "file",
        ReadFileHandler::new(terminal_backend.clone(), fs.clone()).schema(),
        Arc::new(ReadFileHandler::new(terminal_backend.clone(), fs.clone())),
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
        WriteFileHandler::new(terminal_backend.clone(), fs.clone()).schema(),
        Arc::new(WriteFileHandler::new(terminal_backend, fs)),
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
        "Read a local image file so the LLM can SEE it natively — free, no API key. \
         ⚠️ MUST be called INSIDE a sub-agent via `delegate_task`: calling it in the MAIN agent context injects a 2,000–8,000 token base64 blob paid on every subsequent turn, bloating context and making the LLM drift off-task. \
         Delegate instead: `delegate_task(task=\"...\", context=\"Image path: ...\")`.",
        "🖼️",
        None,
        RiskLevel::Safe,
        /* override_existing = */ false,
    );
    // move_file — structured file move/copy/rename (no shell involvement)
    registry.register_with_risk(
        "move_file",
        "file",
        MoveFileHandler.schema(),
        Arc::new(MoveFileHandler),
        Arc::new(|| true),
        vec![],
        true,
        "Move, copy, or rename a file/directory using native OS APIs (no shell). \
         Eliminates shell quote/path-format issues that plague `terminal`-based copy/move.",
        "📦",
        None,
        RiskLevel::Caution,
        /* override_existing = */ false,
    );
}

/// Resolve a path to an absolute string for display in LLM-visible tool
/// output. The rule is **literal pass-through**: the path the user typed or
/// configured is the path we report. We deliberately do NOT canonicalize.
/// Why no canonicalize: on Windows, `std::fs::canonicalize` follows
/// junctions / symlinks AND prepends the `\\?\` long-path prefix. So if the
/// user is in `D:\data` (a junction) and the LLM calls `read_file("D:\\qcc\\foo")`,
/// the OS opens the file fine, but `canonicalize` would surface
/// `\\?\F:\...` instead of `D:\data\foo` — the junction target, not what
/// the user typed. The LLM would then echo the wrong path back into
/// subsequent reasoning and tool calls, and `subdirectory_hints` /
/// `_files.txt` would all drift. The user's configured workspace root and
/// the path they typed are the source of truth.
/// For relative paths we still need a base directory to make the result
/// absolute, so we fall back to:
/// 1. `TERMINAL_CWD` env var (set by `LocalAgentService::send_message_stream`
/// from the per-turn `AgentOverrides::workspace`, i.e. the desktop
/// "工作区" picker).
/// 2. Server process current working directory.
/// The cwd itself is not canonicalized — same reason.
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
    /// 接缝 `Fs`：提供了就走接缝（不带 offset/limit 的简单路径）。
    /// `None` 时继续走 `backend`，保留既有的 offset/limit 切片语义。
    /// 渐进迁移期两个都能用，不破坏既有调用点。
    fs: Option<Arc<dyn Fs>>,
}

impl ReadFileHandler {
    pub fn new(backend: Arc<dyn TerminalBackend>, fs: Option<Arc<dyn Fs>>) -> Self {
        Self { backend, fs }
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
        //: advisory path traversal check — logs but does not block.
        crate::credential_guard::check_path_traversal_advisory(path);

        let offset = params.get("offset").and_then(|v| v.as_u64());
        let limit = params.get("limit").and_then(|v| v.as_u64());

        // 接缝路径：只接受整文件读取；offset/limit 留给上层切片。
        // 用户配 Fs 但又传 offset/limit 时报错——避免行为分裂。
        let content = if let Some(fs) = &self.fs {
            if offset.is_some() || limit.is_some() {
                return Err(ToolError::InvalidParams(
                    "read_file on the Fs seam does not accept offset/limit; \
                     the caller must slice the result."
                        .into(),
                ));
            }
            match fs.read(Path::new(path)).await {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(FsError::NotFound(p)) => {
                    return Err(ToolError::from_message(format!("file not found: {}", p.display())));
                }
                Err(FsError::Denied(p)) => {
                    return Err(ToolError::from_message(format!("permission denied: {}", p.display())));
                }
                Err(FsError::Io(e)) => {
                    return Err(ToolError::from_message(e));
                }
            }
        } else {
            self.backend
                .read_file(path, offset, limit)
                .await
                .map_err(|e| ToolError::from_message(e.to_string()))?
        };

        // Character-level output cap. The backend's `limit` is line-based
        // and optional — a huge file (e.g. a multi-MB log) or a file with
        // very long single lines (minified JS) can otherwise produce a
        // multi-MB tool_result that blows up the next LLM request's
        // context and 400s. Cap here so the LLM always gets a bounded
        // body plus a hint to page through with offset/limit.
        const MAX_OUTPUT_CHARS: usize = 300_000;
        let mut body = String::with_capacity(MAX_OUTPUT_CHARS.min(content.len()));
        let mut chars_read = 0usize;
        let mut overflow = false;
        for ch in content.chars() {
            if chars_read >= MAX_OUTPUT_CHARS {
                overflow = true;
                break;
            }
            body.push(ch);
            chars_read += 1;
        }
        if overflow {
            body.push_str(&format!(
                "\n\n[truncated: file exceeds {MAX_OUTPUT_CHARS} chars, showing the first {MAX_OUTPUT_CHARS}. Use offset/limit to read further chunks]"
            ));
        }

        // Prefix the content with an absolute-path header so the LLM
        // (and the front-end artifact extractor) can unambiguously
        // reference the file later. Format mirrors write_file.
        let abs = to_absolute_path(path);
        Ok(format!("[file: {abs}]\n{body}"))
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
    /// 接缝 `Fs`：提供了就走接缝（不含任何 offset/limit 的纯写入）。
    /// `None` 时继续走 `backend`，保留既有调用点与测试。
    fs: Option<Arc<dyn Fs>>,
}

impl WriteFileHandler {
    pub fn new(backend: Arc<dyn TerminalBackend>, fs: Option<Arc<dyn Fs>>) -> Self {
        Self { backend, fs }
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
        //: advisory path traversal check — logs but does not block.
        crate::credential_guard::check_path_traversal_advisory(path);

        // 安全中心：覆盖前把旧版本存进备份目录（best-effort，失败不影响写）。
        let original = std::fs::read(path).unwrap_or_default();
        crate::backup::try_backup(Path::new(path), &original);

        if let Some(fs) = &self.fs {
            fs.write(Path::new(path), content.as_bytes()).await.map_err(|e| match e {
                FsError::Denied(p) => ToolError::from_message(format!("permission denied: {}", p.display())),
                FsError::NotFound(p) => ToolError::from_message(format!("path missing: {}", p.display())),
                FsError::Io(msg) => ToolError::from_message(msg),
            })?;
        } else {
            self.backend
                .write_file(path, content)
                .await
                .map_err(|e| ToolError::from_message(e.to_string()))?;
        }

        // Always report the absolute path so the LLM and front-end can
        // refer back to it without re-deriving. `to_absolute_path` falls back
        // to a non-canonicalized form if the file is on a path that can't be
        // canonicalized (rare, but happens with bind-mounts / junctions).
        let abs = to_absolute_path(path);
        Ok(format!("Successfully wrote {} bytes to {}", content.len(), abs))
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
        props.insert(
            "reason".into(),
            json!({
                "type": "string",
                "description": "One-sentence explanation of WHY this file is being written and what it does, \
                                for the user's approval card (e.g. \"为新建的 sub-app 创建 manifest\"). \
                                Required whenever the write has side effects — always fill it."
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
    /// Apply a single patch to a file using fuzzy matching.
    async fn patch_file(
        &self,
        path: &str,
        old_string: &str,
        new_string: &str,
        replace_all: bool,
    ) -> Result<String, ToolError>;

    /// Apply multiple patches to the same file atomically (read once,
    /// apply all edits in memory, write once). If any edit fails, the
    /// file is NOT modified — the entire operation aborts and returns
    /// the first error.
    ///
    /// Default implementation calls `patch_file` sequentially. Real
    /// backends should override this with an in-memory implementation
    /// to avoid N disk reads/writes.
    async fn patch_file_multi(
        &self,
        path: &str,
        edits: &[(String, String, bool)],
    ) -> Result<String, ToolError> {
        let mut results = Vec::new();
        for (i, (old, new, replace_all)) in edits.iter().enumerate() {
            let r = self.patch_file(path, old, new, *replace_all).await?;
            results.push(json!({"edit": i + 1, "result": r}));
        }
        Ok(json!({"status": "ok", "edits_applied": edits.len(), "details": results}).to_string())
    }
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

        // MultiEdit path: when `edits` array is present, apply multiple
        // find-and-replace operations to the same file in one call.
        // This reduces N tool calls (and N result entries in context) to
        // a single call + single result — critical for zero-context-bloat.
        if let Some(edits_arr) = params.get("edits").and_then(|v| v.as_array()) {
            // `edits` and `old_string` are mutually exclusive — if both
            // are present, the call is ambiguous and we reject it.
            if params.get("old_string").is_some() {
                return Err(ToolError::InvalidParams(
                    "Cannot specify both 'edits' and 'old_string'. Use 'edits' for multi-edit, or 'old_string'/'new_string' for single edit.".into(),
                ));
            }
            if edits_arr.is_empty() {
                return Err(ToolError::InvalidParams(
                    "'edits' array must not be empty.".into(),
                ));
            }
            // Cap at 50 edits to prevent LLM from flooding a single call.
            if edits_arr.len() > 50 {
                return Err(ToolError::InvalidParams(format!(
                    "'edits' array has {} entries — maximum is 50. Split into multiple patch calls.",
                    edits_arr.len()
                )));
            }

            // Parse each edit entry into (old_string, new_string, replace_all).
            let mut edits: Vec<(String, String, bool)> = Vec::with_capacity(edits_arr.len());
            for (i, entry) in edits_arr.iter().enumerate() {
                let old = entry
                    .get("old_string")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::InvalidParams(format!(
                            "edits[{i}] missing 'old_string'"
                        ))
                    })?;
                let new = entry
                    .get("new_string")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let ra = entry
                    .get("replace_all")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                edits.push((old.to_string(), new.to_string(), ra));
            }

            return self.backend.patch_file_multi(path, &edits).await;
        }

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
        props.insert("edits".into(), json!({
            "type": "array",
            "description": "Multiple find-and-replace edits to apply atomically to the same file. Each entry: {old_string, new_string, replace_all?}. Edits are applied sequentially — the result of edit N is the input to edit N+1. If any edit fails to match, the entire operation aborts and the file is NOT modified. Use this instead of multiple separate patch calls to avoid context bloat (N calls = N results in ctx vs 1 call = 1 result). Mutually exclusive with 'old_string'. Max 50 entries.",
            "items": json!({
                "type": "object",
                "properties": {
                    "old_string": json!({"type": "string", "description": "Text to find (fuzzy matching supported)"}),
                    "new_string": json!({"type": "string", "description": "Replacement text (empty string to delete)"}),
                    "replace_all": json!({"type": "boolean", "description": "Replace all occurrences (default: false)", "default": false})
                },
                "required": ["old_string"]
            })
        }));

        tool_schema(
            "patch",
            "Apply targeted find-and-replace edits to a file using fuzzy matching. Minor whitespace/indentation differences won't break matching. For multiple edits to the same file, pass an 'edits' array instead of separate old_string/new_string calls — this applies all edits atomically (read once, write once) and keeps context clean.",
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

/// Read an image file from disk and return `(mime_type, base64_data)`.
///
/// This is the shared helper used by both the `read_image` tool and
/// `sub_agent_orchestrator::attach_images` so the resize/encode logic
/// lives in exactly one place.
///
/// Images larger than 1 MiB are automatically decoded, resized to max
/// 1920 px on the longest side, and re-encoded as JPEG before base64.
/// This is critical because an 8.3 MB JPEG produces ~11 MB base64 —
/// most LLM vision APIs silently truncate payloads > 5 MB, causing
/// the LLM to hallucinate image content (wrong names, wrong text).
/// Resized images are typically 100–300 KB base64.
pub fn read_image_for_llm(
    path: &str,
    _detail: &str,
) -> Result<(String, String), String> {
    let bytes = std::fs::read(path)
        .map_err(|e| format!("cannot read image file '{path}': {e}"))?;

    if bytes.len() > 50 * 1024 * 1024 {
        return Err(format!(
            "file '{path}' is {} bytes — too large for image reading (max 50 MB)",
            bytes.len()
        ));
    }

    const MAX_RAW_SIZE_BYTES: usize = 1 * 1024 * 1024; // 1 MiB
    const MAX_DIMENSION_PX: u32 = 1920;

    let mime = mime_from_ext(path).to_string();

    // Resize JPEG/PNG images > 1 MiB to reduce base64 payload.
    // GIF/WEBP/BMP are harder to re-encode reliably; leave as-is.
    let can_resize = mime == "image/jpeg" || mime == "image/png";
    let needs_resize = bytes.len() > MAX_RAW_SIZE_BYTES;

    if can_resize && needs_resize {
        match image::load_from_memory(&bytes) {
            Ok(img) => {
                let (w, h) = (img.width(), img.height());
                let mut resized = img;
                if w.max(h) > MAX_DIMENSION_PX {
                    let ratio = MAX_DIMENSION_PX as f64 / w.max(h) as f64;
                    let new_w = (w as f64 * ratio).round() as u32;
                    let new_h = (h as f64 * ratio).round() as u32;
                    resized = resized.resize(
                        new_w,
                        new_h,
                        image::imageops::FilterType::Lanczos3,
                    );
                }
                let mut jpeg_buf = Vec::with_capacity(256 * 1024);
                match resized.write_to(
                    &mut std::io::Cursor::new(&mut jpeg_buf),
                    image::ImageFormat::Jpeg,
                ) {
                    Ok(()) => {
                        let b64 = general_purpose::STANDARD.encode(&jpeg_buf);
                        tracing::info!(
                            path,
                            orig_w = w, orig_h = h,
                            orig_kb = bytes.len() / 1024,
                            new_kb = jpeg_buf.len() / 1024,
                            "read_image_for_llm: resized large image for LLM vision"
                        );
                        return Ok(("image/jpeg".to_string(), b64));
                    }
                    Err(e) => {
                        tracing::warn!(
                            path, error = %e,
                            "read_image_for_llm: JPEG re-encode failed, falling back to raw bytes"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    path, error = %e,
                    "read_image_for_llm: image decode failed, falling back to raw bytes"
                );
            }
        }
    }

    let b64 = general_purpose::STANDARD.encode(&bytes);
    Ok((mime, b64))
}

pub struct ReadImageHandler;

impl ReadImageHandler {
    pub fn new() -> Self { Self }
}

impl Default for ReadImageHandler {
    fn default() -> Self { Self::new() }
}

/// 共享读取逻辑：`execute` 与 `execute_structured` 都从这里拿 bytes。
/// `execute_structured`（S4 接缝）拿原始 bytes 包成 `AttachmentKind::Image`
/// attachment；`execute` 拿 bytes 编码 base64 后塞 JSON 字符串。
/// 两条路径一致——只是 attachment 表达形式不同。
fn read_image_bytes(path: &str) -> Result<(Vec<u8>, String), ToolError> {
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
    let mime = mime_from_ext(path).to_string();
    Ok((bytes, mime))
}

#[async_trait]
impl ToolHandler for ReadImageHandler {
    /// 老的字符串返回路径：base64 JSON 字符串。**保留向后兼容**——目前
    /// 还有不少调用方按 `"_image_data"` 字段解析。迁移到
    /// `execute_structured` 后这条路径才能砍。
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

        let ocr = params
            .get("ocr")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if ocr {
            return Err(ToolError::ExecutionFailed(
                "Local OCR has been removed from this build. The image is still returned via \
                 `_image_data` — let the LLM read it with vision, or delegate to a sub-agent \
                 `read_image`."
                    .into(),
            ));
        }

        let (bytes, mime) = read_image_bytes(path)?;
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

    /// S4 接缝兑现：图片作为结构化 attachment，文本只放 metadata。
    /// 前端拿到 ToolOutput 后能从 attachment 直接渲染图片，不用解析 base64
    /// 字符串。LLM 这边看到的 text 部分是「path + mime + size」摘要，
    /// 实际像素走 multimodal 通道。
    async fn execute_structured(&self, params: Value) -> Result<ToolOutput, ToolError> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams(
                "missing 'path' — pass a local image file path (.png/.jpg/.gif/.webp)".into(),
            ))?;
        if path.is_empty() {
            return Err(ToolError::InvalidParams("'path' must be non-empty".into()));
        }

        let (bytes, mime) = read_image_bytes(path)?;
        let text = format!(
            "Read image: path={path} mime={mime} size_bytes={}",
            bytes.len()
        );
        Ok(ToolOutput::text(text)
            .with_attachment(AttachmentKind::Image, &mime, bytes))
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
        props.insert(
            "ocr".into(),
            json!({
                "type": "boolean",
                "description": "Local OCR has been removed from this build. Passing true returns an error — the image is always returned in `_image_data` so the LLM can read it directly with native multimodal vision (free, offline, no API key). Default false."
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

// ---------------------------------------------------------------------------
// MoveFileHandler — structured file move/copy/rename (no shell)
// ---------------------------------------------------------------------------

/// Tool for moving, copying, or renaming files and directories using
/// native `std::fs` APIs. This bypasses the shell entirely, eliminating
/// the quote-stripping, path-format, and binary-not-found issues that
/// plague `terminal`-based file operations on Windows.
///
/// **Why this exists**: The LLM frequently tries to copy/move files via
/// `terminal` (`cp`, `copy`, `Copy-Item`, `robocopy`), and each variant
/// hits a different shell-compatibility wall — Git Bash strips quotes,
/// `cp` doesn't exist, `Copy-Item` needs different syntax, etc. A single
/// `std::fs` call succeeds on the first try regardless of shell.
pub struct MoveFileHandler;

impl Default for MoveFileHandler {
    fn default() -> Self {
        Self
    }
}

impl MoveFileHandler {
    /// Copy a file. Uses `std::fs::copy` which handles binary files
    /// correctly on all platforms.
    fn copy_file(src: &Path, dst: &Path) -> Result<u64, String> {
        std::fs::copy(src, dst).map_err(|e| format!("复制失败 '{}': {}", src.display(), e))
    }

    /// Recursively copy a directory and its contents.
    fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<u64, String> {
        std::fs::create_dir_all(dst)
            .map_err(|e| format!("无法创建目标目录 '{}': {}", dst.display(), e))?;
        let mut total_bytes: u64 = 0;
        for entry in std::fs::read_dir(src)
            .map_err(|e| format!("无法读取源目录 '{}': {}", src.display(), e))? {
            let entry = entry.map_err(|e| format!("读取目录条目失败: {}", e))?;
            let from = entry.path();
            let to = dst.join(entry.file_name());
            if from.is_dir() {
                total_bytes += Self::copy_dir_recursive(&from, &to)?;
            } else {
                total_bytes += Self::copy_file(&from, &to)?;
            }
        }
        Ok(total_bytes)
    }

    /// Move a file or directory. Tries `std::fs::rename` first (atomic,
    /// same-drive). If that fails with a cross-device error, falls back
    /// to copy + delete.
    fn move_entry(src: &Path, dst: &Path) -> Result<u64, String> {
        match std::fs::rename(src, dst) {
            Ok(()) => {
                // rename succeeded — we don't know the size without
                // reading metadata before the move, but that's fine;
                // the caller reports success, not byte count.
                Ok(0)
            }
            Err(e) if e.raw_os_error() == Some(17) /* EXDEV: cross-device */ => {
                // Cross-drive move: copy then delete source.
                let bytes = if src.is_dir() {
                    Self::copy_dir_recursive(src, dst)?
                } else {
                    Self::copy_file(src, dst)?
                };
                if src.is_dir() {
                    std::fs::remove_dir_all(src)
                        .map_err(|e| format!("移动后删除源目录失败 '{}': {}", src.display(), e))?;
                } else {
                    std::fs::remove_file(src)
                        .map_err(|e| format!("移动后删除源文件失败 '{}': {}", src.display(), e))?;
                }
                Ok(bytes)
            }
            Err(e) => Err(format!("移动失败 '{} → {}': {}", src.display(), dst.display(), e)),
        }
    }
}

#[async_trait]
impl ToolHandler for MoveFileHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let src = params
            .get("src")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'src' parameter".into()))?;
        let dst = params
            .get("dst")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'dst' parameter".into()))?;
        let copy_mode = params
            .get("copy")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let overwrite = params
            .get("overwrite")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let src_path = Path::new(src);
        let dst_path = Path::new(dst);

        // Credential guard — same as read_file/write_file
        CredentialGuard::new().check_read_access(src_path)?;
        CredentialGuard::new().check_read_access(dst_path)?;

        // Source must exist
        if !src_path.exists() {
            return Err(ToolError::ExecutionFailed(format!(
                "源路径不存在: '{}'", src
            )));
        }

        // Handle destination exists
        if dst_path.exists() {
            if !overwrite {
                return Err(ToolError::ExecutionFailed(format!(
                    "目标已存在: '{}'。如果需要覆盖，请设置 overwrite: true。",
                    dst
                )));
            }
            // Remove existing destination
            if dst_path.is_dir() {
                std::fs::remove_dir_all(dst_path)
                    .map_err(|e| ToolError::ExecutionFailed(format!(
                        "覆盖目标目录失败 '{}': {}", dst, e
                    )))?;
            } else {
                std::fs::remove_file(dst_path)
                    .map_err(|e| ToolError::ExecutionFailed(format!(
                        "覆盖目标文件失败 '{}': {}", dst, e
                    )))?;
            }
        }

        // Auto-create parent directories of dst
        if let Some(parent) = dst_path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| ToolError::ExecutionFailed(format!(
                        "无法创建目标父目录 '{}': {}", parent.display(), e
                    )))?;
            }
        }

        // Execute the operation in a blocking task (fs operations are blocking)
        let src_owned = src.to_string();
        let dst_owned = dst.to_string();
        let result = tokio::task::spawn_blocking(move || {
            let src_p = Path::new(&src_owned);
            let dst_p = Path::new(&dst_owned);
            if copy_mode {
                if src_p.is_dir() {
                    Self::copy_dir_recursive(src_p, dst_p)
                } else {
                    Self::copy_file(src_p, dst_p).map(|_| 0u64)
                }
            } else {
                Self::move_entry(src_p, dst_p)
            }
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("文件操作任务失败: {}", e)))?;

        let bytes = result.map_err(ToolError::ExecutionFailed)?;
        let abs_src = to_absolute_path(src);
        let abs_dst = to_absolute_path(dst);
        let op = if copy_mode { "copied" } else { "moved" };

        Ok(json!({
            "ok": true,
            "operation": op,
            "src": abs_src,
            "dst": abs_dst,
            "bytes": bytes,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "src".into(),
            json!({
                "type": "string",
                "description": "Source file or directory path. Must exist."
            }),
        );
        props.insert(
            "dst".into(),
            json!({
                "type": "string",
                "description": "Destination path. Parent directories are auto-created. If dst already exists, set overwrite: true to replace it."
            }),
        );
        props.insert(
            "copy".into(),
            json!({
                "type": "boolean",
                "default": false,
                "description": "If true, copy the file/directory (keep source). If false (default), move it (source is removed after copy for cross-drive, or renamed atomically for same-drive)."
            }),
        );
        props.insert(
            "overwrite".into(),
            json!({
                "type": "boolean",
                "default": false,
                "description": "If true, overwrite the destination if it already exists. If false (default), the tool errors when dst exists."
            }),
        );
        tool_schema(
            "move_file",
            "Move, copy, or rename a file or directory using native OS filesystem APIs — no shell involved. \
             This is the PREFERRED way to copy/move/rename files on Windows: it bypasses all shell \
             compatibility issues (quote stripping, path format, missing binaries). \
             Set copy: true to copy (keep source), omit or false to move. Set overwrite: true to replace existing destination.",
            JsonSchema::object(props, vec!["src".into(), "dst".into()]),
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

    // -- S4 接缝兑现：read_image 的 execute_structured 把图片作为
    // attachment 而不是塞 base64 字符串。验证：
    //   1) ToolOutput.attachments 有一项 Image
    //   2) attachment bytes 与磁盘文件一致
    //   3) text 部分只是 metadata 摘要，没有 base64 数据
    //   4) 老 execute() 路径仍然返回 base64 JSON 字符串（向后兼容）

    fn tiny_png_path(name: &str) -> std::path::PathBuf {
        // 1x1 red PNG
        let png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
        let bytes = base64::engine::general_purpose::STANDARD.decode(png_b64).unwrap();
        let p = std::env::temp_dir().join(name);
        std::fs::write(&p, &bytes).unwrap();
        p
    }

    #[tokio::test]
    async fn read_image_structured_returns_attachment_not_base64() {
        use hermix_core::traits::ToolHandler as _;
        let p = tiny_png_path("read_image_s4_1.png");
        let out = ReadImageHandler::new()
            .execute_structured(json!({ "path": p.to_str().unwrap() }))
            .await
            .unwrap();
        assert!(!out.is_error);
        assert_eq!(out.attachments.len(), 1, "expected one image attachment");
        let att = &out.attachments[0];
        assert_eq!(att.kind, AttachmentKind::Image);
        assert_eq!(att.mime, "image/png");
        // text 段是 metadata 摘要，绝不含 base64
        assert!(out.text.contains("image/png"), "got text: {}", out.text);
        assert!(!out.text.contains("base64"), "text should not mention base64");
        assert!(!out.text.contains("data:image"), "text should not embed base64");
        let _ = std::fs::remove_file(&p);
    }

    #[tokio::test]
    async fn read_image_structured_attachment_bytes_round_trip_file() {
        use hermix_core::traits::ToolHandler as _;
        let p = tiny_png_path("read_image_s4_2.png");
        let original = std::fs::read(&p).unwrap();
        let out = ReadImageHandler::new()
            .execute_structured(json!({ "path": p.to_str().unwrap() }))
            .await
            .unwrap();
        assert_eq!(out.attachments.len(), 1);
        assert_eq!(
            out.attachments[0].data, original,
            "attachment bytes must equal on-disk bytes (no resize path on tiny png)"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[tokio::test]
    async fn read_image_legacy_execute_still_returns_base64_string() {
        // 零回归钉子：老 execute() 调用方仍在解析 `_image_data` 字段。
        use serde_json::Value;
        let p = tiny_png_path("read_image_s4_3.png");
        let out = ReadImageHandler::new()
            .execute(json!({ "path": p.to_str().unwrap() }))
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["mime_type"], "image/png");
        assert!(v["_image_data"].as_str().unwrap().len() > 0);
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
        let handler = ReadFileHandler::new(Arc::new(MockBackend), None);
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
        let handler = WriteFileHandler::new(Arc::new(MockBackend), None);
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
        let handler = ReadFileHandler::new(Arc::new(MockBackend), None);
        let schema = handler.schema();
        assert_eq!(schema.name, "read_file");
    }

    #[tokio::test]
    async fn test_write_file_schema() {
        let handler = WriteFileHandler::new(Arc::new(MockBackend), None);
        let schema = handler.schema();
        assert_eq!(schema.name, "write_file");
    }

    // -- 渐进迁移：提供 `Fs` seam 后走接缝路径 -----------------------
    // 测试读写的真实数据落到了 InMemoryFs 而不是 MockBackend，验证：
    //   1) 工具拿到 `Option<Arc<dyn Fs>>` 后能正确分流；
    //   2) 接缝路径的错误映射（NotFound → 工具错误）正常工作；
    //   3) 同时传 offset/limit + Fs 时拒绝（避免行为分裂）。

    #[tokio::test]
    async fn read_file_uses_fs_seam_when_provided() {
        let fs: Arc<dyn Fs> = Arc::new(hermix_core::InMemoryFs::default());
        fs.write(Path::new("/virtual/foo.txt"), b"hello seam").await.unwrap();

        let handler = ReadFileHandler::new(Arc::new(MockBackend), Some(fs.clone()));
        let result = handler.execute(json!({"path": "/virtual/foo.txt"})).await.unwrap();
        assert!(result.contains("hello seam"), "got: {result}");
    }

    #[tokio::test]
    async fn read_file_seam_rejects_offset_limit() {
        let fs: Arc<dyn Fs> = Arc::new(hermix_core::InMemoryFs::default());
        let handler = ReadFileHandler::new(Arc::new(MockBackend), Some(fs));
        let err = handler.execute(json!({"path": "/x", "offset": 1})).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("does not accept offset/limit"), "got: {msg}");
    }

    #[tokio::test]
    async fn read_file_seam_surfaces_not_found() {
        let fs: Arc<dyn Fs> = Arc::new(hermix_core::InMemoryFs::default());
        let handler = ReadFileHandler::new(Arc::new(MockBackend), Some(fs));
        let err = handler.execute(json!({"path": "/nope.txt"})).await.unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn write_file_uses_fs_seam_when_provided() {
        let fs: Arc<dyn Fs> = Arc::new(hermix_core::InMemoryFs::default());
        let handler = WriteFileHandler::new(Arc::new(MockBackend), Some(fs.clone()));
        let result = handler
            .execute(json!({"path": "/virtual/out.txt", "content": "abc"}))
            .await
            .unwrap();
        assert!(result.contains("Successfully wrote 3 bytes"));
        // 数据落到了 InMemoryFs
        let got = fs.read(Path::new("/virtual/out.txt")).await.unwrap();
        assert_eq!(got, b"abc".to_vec());
    }

    #[tokio::test]
    async fn write_file_without_seam_still_uses_terminal_backend() {
        // 验证"没传 fs"时不退化到接缝，老路径不变。
        let handler = WriteFileHandler::new(Arc::new(MockBackend), None);
        let result = handler
            .execute(json!({"path": "/dev/null", "content": "x"}))
            .await
            .unwrap();
        assert!(result.contains("Successfully wrote"));
    }

    // -- to_absolute_path: literal pass-through contract -----------------
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
