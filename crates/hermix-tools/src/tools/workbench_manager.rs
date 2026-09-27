//! Workbench manager — LLM tool bridging `hermix-plugin::workbench` to the
//! agent tool table. Without this bridge the `workbench` SKILL.md teaches
//! operations (write_code_file with lint gate, module_index, set_deps) the
//! tool table doesn't expose — the agent then invents tool names
//! (`workspace_build`), fails, and degrades to guessing.
//!
//! Thin by design: every action forwards to an already-tested free function
//! in `hermix_plugin::workbench` (validation / lint gate / undo backup live
//! THERE, not here).

use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_core::{traits::RiskLevel, ToolError, ToolHandler, ToolSchema};
use hermix_plugin::workbench as wb;

use crate::ToolRegistry;

/// Register the `workbench_manager` tool.
pub fn register(registry: &ToolRegistry) {
    let handler = Arc::new(WorkbenchManagerHandler);
    let schema = handler.schema();
    registry.register_with_risk(
        "workbench_manager",
        "workbench",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        true,
        "Operate on an EXISTING workbench's code files. Actions: list (installed \
         workbenches), files (code file paths), read (file content), write \
         (create/replace a code file — locked files rejected, line limits \
         enforced, pre-write lint gate returns file:line + rule ID + fix), lint \
         (full workbench lint report), index (module → exports, one line per \
         module), deps (set importmap deps; uninstalled libs are rejected — \
         request them, never use CDN), undo (revert last code write). Note: \
         creating a NEW workbench goes through custom_app_manager \
         (propose → approve → create), not this tool.",
        "🛠️",
        None,
        RiskLevel::Caution,
        /* override_existing = */ false,
    );
}

fn params_str(params: &Value, key: &str) -> Result<String, ToolError> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| ToolError::InvalidParams(format!("缺少 '{key}' 参数")))
}

#[derive(Default)]
pub struct WorkbenchManagerHandler;

impl WorkbenchManagerHandler {
    fn dispatch(&self, params: &Value) -> Result<String, ToolError> {
        let action = params_str(params, "action")?;
        match action.as_str() {
            "list" => {
                let manifests = wb::list_workbenches().map_err(ToolError::ExecutionFailed)?;
                let rows: Vec<Value> = manifests
                    .iter()
                    .map(|m| {
                        json!({
                            "id": m.id,
                            "name": m.name,
                            "primary": m.is_primary,
                            "deps": m.deps,
                        })
                    })
                    .collect();
                Ok(serde_json::to_string_pretty(&rows).unwrap_or_default())
            }
            "files" => {
                let id = params_str(params, "id")?;
                let files = wb::list_code_files(&id).map_err(ToolError::ExecutionFailed)?;
                Ok(files.join("\n"))
            }
            "read" => {
                let id = params_str(params, "id")?;
                let rel = params_str(params, "rel")?;
                wb::read_code_file(&id, &rel).map_err(ToolError::ExecutionFailed)
            }
            "write" => {
                let id = params_str(params, "id")?;
                let rel = params_str(params, "rel")?;
                let content = params_str(params, "content")?;
                wb::write_code_file(&id, &rel, &content).map_err(ToolError::ExecutionFailed)?;
                Ok(format!("written: {rel} (lint gate passed, backup recorded)"))
            }
            "lint" => {
                let id = params_str(params, "id")?;
                let violations =
                    wb::lint_workbench(&id, None).map_err(ToolError::ExecutionFailed)?;
                Ok(wb::format_violations(&violations))
            }
            "index" => {
                let id = params_str(params, "id")?;
                let entries = wb::module_index(&id).map_err(ToolError::ExecutionFailed)?;
                Ok(wb::format_module_index(&entries))
            }
            "deps" => {
                let id = params_str(params, "id")?;
                let deps: Vec<String> = params
                    .get("deps")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|d| d.as_str().map(str::to_string))
                            .collect()
                    })
                    .ok_or_else(|| ToolError::InvalidParams("缺少 'deps' 数组参数".into()))?;
                let applied = wb::set_deps(&id, &deps).map_err(ToolError::ExecutionFailed)?;
                Ok(format!("deps set: {}", applied.join(", ")))
            }
            "undo" => {
                let id = params_str(params, "id")?;
                match wb::undo_last(&id).map_err(ToolError::ExecutionFailed)? {
                    Some(rel) => Ok(format!("reverted: {rel}")),
                    None => Ok("nothing to undo".into()),
                }
            }
            other => Err(ToolError::InvalidParams(format!(
                "未知 action '{other}'（可用: list/files/read/write/lint/index/deps/undo）"
            ))),
        }
    }
}

#[async_trait::async_trait]
impl ToolHandler for WorkbenchManagerHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        self.dispatch(&params)
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "enum": ["list", "files", "read", "write", "lint", "index", "deps", "undo"],
                "description": "Operation to perform",
            }),
        );
        props.insert(
            "id".into(),
            json!({
                "type": "string",
                "description": "Workbench id (from `list`)",
            }),
        );
        props.insert(
            "rel".into(),
            json!({
                "type": "string",
                "description": "Code-relative path, e.g. views/overview.js (read/write)",
            }),
        );
        props.insert(
            "content".into(),
            json!({
                "type": "string",
                "description": "Full file content (write)",
            }),
        );
        props.insert(
            "deps".into(),
            json!({
                "type": "array",
                "items": {"type": "string"},
                "description": "Importmap dep names (deps)",
            }),
        );
        ToolSchema {
            name: "workbench_manager".into(),
            description: "Read/write/lint workbench code files with the lint gate; \
                          module index and dependency management."
                .into(),
            parameters: hermix_core::JsonSchema {
                schema_type: Some("object".into()),
                properties: Some(props),
                required: Some(vec!["action".into(), "id".into()]),
                additional_properties: None,
            },
        }
    }
}
