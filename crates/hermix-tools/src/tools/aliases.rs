//! Alias tools and introspection meta-tools.
//!
//! These tools exist to close the gap between the names the LLM has
//! been trained on (`list_files`, `edit_file`) and the canonical
//! tool names used internally by the registry (`search_files`,
//! `patch`). They are registered alongside the canonical handlers
//! and share their backend — calling `list_files` is identical to
//! calling `search_files` with `target="files"` defaulted.
//!
//! Also included: [`ListToolsHandler`], an introspection tool that
//! returns the full schema of every registered tool. This lets the
//! LLM recover from "I don't have that tool" failures by querying
//! what's actually wired up, instead of stalling on a hallucinated
//! answer.
//!
//! ## Why a separate module
//!
//! The handlers here are deliberately tiny — they each wrap a
//! pre-existing canonical handler. Putting them in `file.rs` would
//! muddy the file-vs-alias boundary; putting them inline in
//! `register_builtins.rs` would scatter alias definitions across
//! the registration pass. A dedicated module keeps the alias
//! surface auditable in one read.

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_core::tool_schema::JsonSchema;
use hermix_core::traits::RiskLevel;
use hermix_core::{ToolError, ToolHandler, ToolSchema};

use crate::tools::file::{PatchBackend, SearchBackend};
use crate::ToolRegistry;

// ---------------------------------------------------------------------------
// ListFilesHandler — alias of SearchFilesHandler with target="files"
// ---------------------------------------------------------------------------

/// Alias tool: `list_files(pattern, path?)`.
///
/// Resolves to `search_files(pattern, path, target="files")`. The LLM
/// is far more likely to reach for "list_files" than the canonical
/// `search_files`, and "Unknown tool 'list_files'" wastes a turn.
pub struct ListFilesHandler {
    backend: Arc<dyn SearchBackend>,
}

impl ListFilesHandler {
    pub fn new(backend: Arc<dyn SearchBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for ListFilesHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let pattern = params
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'pattern' parameter".into()))?;
        let path = params.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        // Hard-coded to file-name search; the `content` variant lives
        // under the canonical `search_files` tool.
        self.backend.search_files(pattern, path).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "pattern".into(),
            json!({
                "type": "string",
                "description": "Glob pattern to match file names (e.g. `*.rs`, `**/*.toml`). Use `*` to list every file in the directory."
            }),
        );
        props.insert(
            "path".into(),
            json!({
                "type": "string",
                "description": "Directory to search in. Defaults to the current workspace."
            }),
        );
        // No `target` field: this alias is *always* a filename search.
        // If you want regex content search, call `search_files` directly.
        hermix_core::tool_schema(
            "list_files",
            "List files in a directory matching a glob pattern (alias of search_files with target='files').\n\
             \n\
             ## When to reach for this tool\n\
             `list_files` is the right tool whenever the user wants to **see what's in a folder** (\"看下 D:\\qcc 下有啥\" / \"list the project\" / \"show me the directory\"). \
             It is cheaper, more precise, and copy-pasteable compared to `computer_use.screenshot` of File Explorer. \
             Pass `*` as the pattern to list every file. For content search, call `search_files`.",
            JsonSchema::object(props, vec!["pattern".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// EditFileHandler — alias of PatchHandler (rename only, no behaviour change)
// ---------------------------------------------------------------------------

/// Alias tool: `edit_file(path, old_string, new_string?, replace_all?)`.
///
/// Resolves to `patch` with identical arguments. Most LLM corpora and
/// human users reach for "edit_file" naturally; previously the call
/// would fail with "Unknown tool 'edit_file'".
pub struct EditFileHandler {
    backend: Arc<dyn PatchBackend>,
}

impl EditFileHandler {
    pub fn new(backend: Arc<dyn PatchBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for EditFileHandler {
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
                "description": "The file path to edit"
            }),
        );
        props.insert(
            "old_string".into(),
            json!({
                "type": "string",
                "description": "The exact text to find in the file (whitespace-tolerant fuzzy matching)"
            }),
        );
        props.insert(
            "new_string".into(),
            json!({
                "type": "string",
                "description": "The replacement text. Use an empty string to delete the matched text."
            }),
        );
        props.insert(
            "replace_all".into(),
            json!({
                "type": "boolean",
                "description": "Replace every occurrence instead of requiring a unique match (default: false).",
                "default": false
            }),
        );
        hermix_core::tool_schema(
            "edit_file",
            "Apply targeted find-and-replace edits to a file (alias of `patch`). \
             The same parameters are accepted: `path`, `old_string`, `new_string`, `replace_all`. \
             Use either `edit_file` or `patch` — they share the same implementation.",
            JsonSchema::object(props, vec!["path".into(), "old_string".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// ListToolsHandler — introspection: dump every registered schema
// ---------------------------------------------------------------------------

/// Introspection tool: returns a human-readable dump of every tool
/// currently registered with the agent, including aliases.
///
/// Why this exists:
/// - The system prompt injects a `<tools>` block but truncates long
///   descriptions. The model can call `list_tools` to retrieve the
///   full schema, including required parameter names and types.
/// - When the LLM hallucinates a tool name, the dispatcher returns
///   "Unknown tool 'X'. Available tools: [...]". The model can now
///   cross-reference by calling `list_tools` itself instead of
///   trying to remember names.
pub struct ListToolsHandler {
    registry: Arc<ToolRegistry>,
}

impl ListToolsHandler {
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl ToolHandler for ListToolsHandler {
    async fn execute(&self, _params: Value) -> Result<String, ToolError> {
        let defs = self.registry.get_definitions();
        let mut out = String::new();
        out.push_str(&format!(
            "# Currently registered tools ({} total)\n\n",
            defs.len()
        ));
        for d in defs {
            out.push_str(&format!("## {}\n", d.name));
            out.push_str(&format!("{}\n", d.description.trim()));
            if let Some(required) = d.parameters.required.as_ref() {
                if !required.is_empty() {
                    out.push_str(&format!("\nRequired parameters: `{}`\n", required.join("`, `")));
                }
            }
            if let Some(props) = d.parameters.properties.as_ref() {
                if !props.is_empty() {
                    out.push_str("\nParameters:\n");
                    for (k, v) in props {
                        let ty = v.get("type").and_then(|x| x.as_str()).unwrap_or("any");
                        let desc = v.get("description").and_then(|x| x.as_str()).unwrap_or("");
                        out.push_str(&format!("- `{k}` ({ty}): {desc}\n"));
                    }
                }
            }
            out.push('\n');
        }
        out.push_str(
            "\n## Aliases\n\
             - `list_files` ≡ `search_files` with `target=\"files\"`\n\
             - `edit_file` ≡ `patch` (preferred name; both work)\n",
        );
        Ok(out)
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "filter".into(),
            json!({
                "type": "string",
                "description": "Optional substring to filter tool names (case-insensitive). Omit to list every tool."
            }),
        );
        hermix_core::tool_schema(
            "list_tools",
            "List every tool currently registered with the agent, including aliases like `list_files` and `edit_file`. \
             Use this whenever you are unsure of an exact tool name, when a tool call returns \"Unknown tool\", \
             or when you want to see the full parameter schema for a tool. The output is plain text and safe to read \
             back into your next turn.",
            JsonSchema::object(props, vec![]),
        )
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the three aliases with the registry.
///
/// All three use local, stateless backends:
/// - `list_files` -> `LocalSearchBackend` (alias of `search_files`, target="files")
/// - `edit_file`  -> `LocalPatchBackend`  (alias of `patch`)
/// - `list_tools` -> the registry itself   (always-on introspection)
///
/// P1.3 migration: previously inline in
/// `register_builtins.rs::register_builtin_tools`.
pub fn register(registry: &ToolRegistry) {
    // list_files — alias of search_files (target="files")
    {
        let h = Arc::new(ListFilesHandler::new(Arc::new(
            crate::backends::file::LocalSearchBackend::new(),
        )));
        let schema = h.schema();
        registry.register_with_risk(
            "list_files",
            "file",
            schema,
            h,
            Arc::new(|| true),
            vec![],
            false,
            "List files in a directory matching a glob pattern (alias of search_files with target='files'). \
             Use this when you want to enumerate files in a folder; for non-file targets use search_files directly.",
            "📂",
            None,
            RiskLevel::Caution,
            false,
        );
    }
    // edit_file — alias of patch
    {
        let h = Arc::new(EditFileHandler::new(Arc::new(
            crate::backends::file::LocalPatchBackend::new(),
        )));
        let schema = h.schema();
        registry.register_with_risk(
            "edit_file",
            "file",
            schema,
            h,
            Arc::new(|| true),
            vec![],
            false,
            "Apply targeted find-and-replace edits to a file (alias of `patch`). \
             Use this for surgical edits; use write_file when rewriting a whole file.",
            "✏️",
            None,
            RiskLevel::Caution,
            false,
        );
    }
    // list_tools — introspection
    {
        let h = Arc::new(ListToolsHandler::new(Arc::new(registry.clone())));
        let schema = h.schema();
        registry.register_with_risk(
            "list_tools",
            "core",
            schema,
            h,
            Arc::new(|| true),
            vec![],
            false,
            "List every tool currently registered with the agent, including aliases. \
             Use when 'Unknown tool' is returned or to discover the full schema.",
            "🧰",
            None,
            RiskLevel::Safe,
            false,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::file::LocalSearchBackend;

    #[tokio::test]
    async fn test_list_files_alias_schema_name() {
        let h = ListFilesHandler::new(Arc::new(LocalSearchBackend::new()));
        assert_eq!(h.schema().name, "list_files");
        // target must NOT be in properties — this is an alias, target is fixed.
        assert!(!h.schema().parameters.properties.as_ref().unwrap().contains_key("target"));
    }

    #[tokio::test]
    async fn test_list_files_alias_requires_pattern() {
        let h = ListFilesHandler::new(Arc::new(LocalSearchBackend::new()));
        let res = h.execute(json!({})).await;
        assert!(res.is_err());
        match res.unwrap_err() {
            ToolError::InvalidParams(msg) => assert!(msg.contains("pattern")),
            other => panic!("expected InvalidParams, got {other:?}"),
        }
    }
}