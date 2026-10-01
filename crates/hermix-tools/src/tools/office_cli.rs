//! OfficeCLI tool — creates, reads, edits Word/Excel/PPT files.
//!
//! Wraps the bundled `officecli` binary so the LLM can invoke Office
//! operations directly as a function call instead of going through
//! `terminal` + skill text. The skill file (`SKILL.md`) still exists
//! as reference documentation, but this tool gives the model a
//! discoverable schema it can call without memorising CLI syntax.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};
use std::sync::Arc;

use hermix_core::{
    tool_schema, CommandOutput, JsonSchema, TerminalBackend, ToolError, ToolHandler, ToolSchema,
    traits::RiskLevel,
};

use crate::ToolRegistry;

pub struct OfficeCliHandler {
    backend: std::sync::Arc<dyn TerminalBackend>,
    default_workspace: std::sync::Arc<std::sync::RwLock<Option<std::path::PathBuf>>>,
}

impl OfficeCliHandler {
    pub fn new(
        backend: std::sync::Arc<dyn TerminalBackend>,
        default_workspace: std::sync::Arc<std::sync::RwLock<Option<std::path::PathBuf>>>,
    ) -> Self {
        Self { backend, default_workspace }
    }

    fn resolve_binary() -> Result<String, ToolError> {
        let candidates: Vec<std::path::PathBuf> = vec![
            std::path::PathBuf::from("officecli-x86_64-pc-windows-msvc.exe"),
            {
                let exe = std::env::current_exe().unwrap_or_default();
                exe.parent()
                    .map(|p| p.join("binaries").join("officecli-x86_64-pc-windows-msvc.exe"))
                    .unwrap_or_default()
            },
            {
                let exe = std::env::current_exe().unwrap_or_default();
                exe.parent()
                    .map(|p| p.join("..").join("src-tauri").join("binaries").join("officecli-x86_64-pc-windows-msvc.exe"))
                    .unwrap_or_default()
            },
        ];
        for c in &candidates {
            if c.exists() {
                return Ok(c.to_string_lossy().to_string());
            }
        }
        Err(ToolError::ExecutionFailed(
            "officecli binary not found. Please ensure it is bundled with the application.".to_string(),
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
impl ToolHandler for OfficeCliHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'action' parameter".into()))?;

        let file_path = params
            .get("file")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'file' parameter".into()))?;

        let binary = Self::resolve_binary()?;

        let mut args = vec![action.to_string(), file_path.to_string()];

        match action {
            "create" => {}
            "view" => {
                let mode = params.get("mode").and_then(|v| v.as_str()).unwrap_or("outline");
                args.push(mode.to_string());
            }
            "get" => {
                let path = params.get("path").and_then(|v| v.as_str()).unwrap_or("/");
                args.push(path.to_string());
                if let Some(depth) = params.get("depth").and_then(|v| v.as_u64()) {
                    args.push("--depth".to_string());
                    args.push(depth.to_string());
                }
                if crate::param_coercion::bool_param(&params, "json", false) {
                    args.push("--json".to_string());
                }
            }
            "set" => {
                let path = params.get("path").and_then(|v| v.as_str()).unwrap_or("/");
                args.push(path.to_string());
                if let Some(props) = params.get("props").and_then(|v| v.as_array()) {
                    for p in props {
                        if let Some(s) = p.as_str() {
                            args.push("--prop".to_string());
                            args.push(s.to_string());
                        }
                    }
                }
            }
            "add" => {
                let parent = params.get("parent").and_then(|v| v.as_str()).unwrap_or("/");
                args.push(parent.to_string());
                if let Some(element_type) = params.get("type").and_then(|v| v.as_str()) {
                    args.push("--type".to_string());
                    args.push(element_type.to_string());
                }
                if let Some(props) = params.get("props").and_then(|v| v.as_array()) {
                    for p in props {
                        if let Some(s) = p.as_str() {
                            args.push("--prop".to_string());
                            args.push(s.to_string());
                        }
                    }
                }
                if let Some(after) = params.get("after").and_then(|v| v.as_str()) {
                    args.push("--after".to_string());
                    args.push(after.to_string());
                }
                if let Some(before) = params.get("before").and_then(|v| v.as_str()) {
                    args.push("--before".to_string());
                    args.push(before.to_string());
                }
                if let Some(from) = params.get("from").and_then(|v| v.as_str()) {
                    args.push("--from".to_string());
                    args.push(from.to_string());
                }
            }
            "batch" => {
                if let Some(input) = params.get("input").and_then(|v| v.as_str()) {
                    args.push("--input".to_string());
                    args.push(input.to_string());
                }
                if crate::param_coercion::bool_param(&params, "force", false) {
                    args.push("--force".to_string());
                }
                if crate::param_coercion::bool_param(&params, "json", false) {
                    args.push("--json".to_string());
                }
            }
            "raw" => {
                let part = params.get("part").and_then(|v| v.as_str()).unwrap_or("/");
                args.push(part.to_string());
            }
            "validate" => {}
            "help" => {
                if let Some(topic) = params.get("topic").and_then(|v| v.as_str()) {
                    args.push(topic.to_string());
                }
            }
            "open" => {}
            "close" => {}
            _ => {
                return Err(ToolError::InvalidParams(format!(
                    "Unknown action '{}'. Supported: create, view, get, set, add, batch, raw, validate, help, open, close",
                    action
                )));
            }
        }

        if crate::param_coercion::bool_param(&params, "json", false)
            && !args.contains(&"--json".to_string())
        {
            args.push("--json".to_string());
        }

        let command_str = format!("{} {}", binary, args.iter().map(|a| {
            if a.contains(' ') || a.contains('"') || a.contains('\\') {
                format!("\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\""))
            } else {
                a.clone()
            }
        }).collect::<Vec<_>>().join(" "));

        let workdir = self
            .default_workspace
            .read()
            .ok()
            .and_then(|g| g.as_ref().map(|p| p.to_string_lossy().to_string()));

        let timeout: u64 = params
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(30000);

        match self
            .backend
            .execute_command(&command_str, Some(timeout), workdir.as_deref(), false, false)
            .await
        {
            Ok(output) => Ok(format_output(&output)),
            Err(e) => Err(ToolError::ExecutionFailed(e.to_string())),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "enum": ["create", "view", "get", "set", "add", "batch", "raw", "validate", "help", "open", "close"],
                "description": "The officecli action to perform"
            }),
        );
        props.insert(
            "file".into(),
            json!({
                "type": "string",
                "description": "Path to the .docx/.xlsx/.pptx file"
            }),
        );
        props.insert(
            "mode".into(),
            json!({
                "type": "string",
                "enum": ["outline", "stats", "issues", "text", "annotated", "html", "screenshot"],
                "description": "View mode (for action=view). Default: outline"
            }),
        );
        props.insert(
            "path".into(),
            json!({
                "type": "string",
                "description": "DOM path inside the document (e.g. '/body', '/slide[1]', '/Sheet1/A1'). 1-based indices."
            }),
        );
        props.insert(
            "parent".into(),
            json!({
                "type": "string",
                "description": "Parent path where a new element is added (for action=add)"
            }),
        );
        props.insert(
            "type".into(),
            json!({
                "type": "string",
                "description": "Element type to add (e.g. slide, shape, paragraph, run, table, row, cell, sheet, image, chart)"
            }),
        );
        props.insert(
            "props".into(),
            json!({
                "type": "array",
                "items": { "type": "string" },
                "description": "Key=value property pairs (e.g. [\"text=Hello\", \"font=Arial\", \"size=24\", \"color=FF0000\"])"
            }),
        );
        props.insert(
            "depth".into(),
            json!({
                "type": "integer",
                "description": "Depth for get action (how many levels of children to include)"
            }),
        );
        props.insert(
            "after".into(),
            json!({
                "type": "string",
                "description": "Insert after this path (for action=add)"
            }),
        );
        props.insert(
            "before".into(),
            json!({
                "type": "string",
                "description": "Insert before this path (for action=add)"
            }),
        );
        props.insert(
            "from".into(),
            json!({
                "type": "string",
                "description": "Clone from this existing path (for action=add)"
            }),
        );
        props.insert(
            "input".into(),
            json!({
                "type": "string",
                "description": "JSON file path for batch operations"
            }),
        );
        props.insert(
            "part".into(),
            json!({
                "type": "string",
                "description": "XML part name (for action=raw)"
            }),
        );
        props.insert(
            "topic".into(),
            json!({
                "type": "string",
                "description": "Help topic (e.g. 'docx', 'pptx', 'docx paragraph') for action=help"
            }),
        );
        props.insert(
            "force".into(),
            json!({
                "type": "boolean",
                "description": "Force overwrite for batch operations (default: false)",
                "default": false
            }),
        );
        props.insert(
            "json".into(),
            json!({
                "type": "boolean",
                "description": "Output as JSON (default: false)",
                "default": false
            }),
        );
        props.insert(
            "timeout".into(),
            json!({
                "type": "integer",
                "description": "Timeout in milliseconds (default: 30000)"
            }),
        );

        tool_schema(
            "office_cli",
            "Create, read, edit, and inspect Office documents (.docx, .xlsx, .pptx) using the bundled officecli binary. \
             No Microsoft Office installation needed.\n\n\
             ## Quick reference\n\
             - **Create**: `office_cli(action=\"create\", file=\"slides.pptx\")` — create blank file\n\
             - **View**: `office_cli(action=\"view\", file=\"report.docx\", mode=\"outline\")` — inspect structure\n\
             - **Add element**: `office_cli(action=\"add\", file=\"slides.pptx\", parent=\"/\", type=\"slide\", props=[\"title=Q4 Report\"])` — add slide/paragraph/shape\n\
             - **Set property**: `office_cli(action=\"set\", file=\"data.xlsx\", path=\"/Sheet1/A1\", props=[\"value=Revenue\", \"bold=true\"])` — modify properties\n\
             - **Get node**: `office_cli(action=\"get\", file=\"slides.pptx\", path=\"/slide[1]\", depth=2)` — inspect a node\n\
             - **Help**: `office_cli(action=\"help\", topic=\"pptx\")` — get detailed docs for an element type\n\
             - **Validate**: `office_cli(action=\"validate\", file=\"report.docx\")` — check for issues\n\n\
             ## Value formats\n\
             - Colors: hex (`FF0000`), named (`red`)\n\
             - Dimensions: `2cm`, `1in`, `72pt`, `96px`, or raw EMU (`914400`)\n\
             - Spacing: `12pt`, `0.5cm`, `1.5x`, `150%`\n\n\
             ## Tips\n\
             - Paths are **1-based**: `/body/p[3]` = third paragraph\n\
             - When unsure about property names, call `action=\"help\"` with the topic instead of guessing\n\
             - After modifications, validate with `action=\"validate\"`",
            JsonSchema::object(props, vec!["action".into(), "file".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the `office_cli` tool with the registry (P1.8 migration).
/// P1.8 : this `register` function was previously missing —
/// the `OfficeCliHandler` was implemented but never wired into the
/// runtime. The `essential::ESSENTIAL_TOOLSETS` constant already lists
/// `"office"` as a floor entry; without a registration the floor's
/// `set_toolset_enabled("office", true)` was a silent no-op and
/// `list_tools()` never exposed the tool to the LLM. The handler is
/// tagged with the dedicated `"office"` toolset so the floor and the
/// `set_enabled_toolsets` UI toggle both find it.
/// The same `terminal_backend` and `terminal_workspace` handles are
/// shared with the `terminal` and `file` tools so any code path that
/// shells out uses one consistent backend.
pub fn register(
    registry: &ToolRegistry,
    terminal_backend: Arc<dyn TerminalBackend>,
    terminal_workspace: Arc<std::sync::RwLock<Option<std::path::PathBuf>>>,
) {
    let handler = Arc::new(OfficeCliHandler::new(terminal_backend, terminal_workspace));
    let schema = handler.schema();
    registry.register_with_risk(
        "office_cli",
        "office",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        true,
        "Create, read, edit, and inspect Office documents (.docx, .xlsx, .pptx) using the bundled officecli binary. No Microsoft Office installation needed.",
        "📊",
        None,
        RiskLevel::Caution,
        false,
    );
}
