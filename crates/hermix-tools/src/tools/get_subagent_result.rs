//! get_subagent_result tool: fetch the output of a background sub-agent.
//!
//! `delegate_task` now dispatches the child loop on a detached tokio task and
//! returns a `sub_agent_id` immediately (see `SubAgentOrchestrator::execute_async`).
//! This tool is how the parent agent retrieves the child's result once it has
//! finished — it reads the lineage JSON written to
//! `<hermix_home>/subagents/{sub_agent_id}.json`.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_config::paths::hermix_home;
use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema, traits::RiskLevel};

use crate::ToolRegistry;
use std::sync::Arc;

pub struct GetSubAgentResultHandler;

#[async_trait]
impl ToolHandler for GetSubAgentResultHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let sub_agent_id = params
            .get("sub_agent_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'sub_agent_id' parameter".into()))?;

        if sub_agent_id.trim().is_empty()
            || sub_agent_id.contains("..")
            || sub_agent_id.contains('/')
            || sub_agent_id.contains('\\')
        {
            return Err(ToolError::InvalidParams(
                "'sub_agent_id' must be non-empty and must not contain path separators or '..'".into(),
            ));
        }

        let path = hermix_home().join("subagents").join(format!("{}.json", sub_agent_id));

        let body = match tokio::fs::read_to_string(&path).await {
            Ok(b) => b,
            Err(_) => {
                return Ok(json!({
                    "sub_agent_id": sub_agent_id,
                    "status": "not_found",
                    "error": "No sub-agent record found for this id. It may have been cleaned up, or the id is wrong.",
                })
                .to_string());
            }
        };

        let lineage: Value = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(e) => {
                return Ok(json!({
                    "sub_agent_id": sub_agent_id,
                    "status": "parse_error",
                    "error": format!("Failed to parse sub-agent record: {}", e),
                })
                .to_string());
            }
        };

        let status = lineage
            .get("status")
            .and_then(|s| s.as_str())
            .unwrap_or("unknown");

        if status == "spawned" || status == "started" {
            return Ok(json!({
                "sub_agent_id": sub_agent_id,
                "status": status,
                "note": "Sub-agent is still running in the background. Wait briefly, then call get_subagent_result again with the same sub_agent_id.",
            })
            .to_string());
        }

        Ok(json!({
            "sub_agent_id": sub_agent_id,
            "status": status,
            "result": lineage.get("result").cloned(),
            "error": lineage.get("error").cloned(),
            "total_turns": lineage.get("total_turns"),
            "estimated_cost_usd": lineage.get("estimated_cost_usd"),
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "sub_agent_id".into(),
            json!({
                "type": "string",
                "description": "The sub_agent_id returned by delegate_task (e.g. 'subagent-xxxx').",
            }),
        );
        tool_schema(
            "get_subagent_result",
            "Fetch the result of a BACKGROUND sub-agent spawned by delegate_task. \
             delegate_task returns immediately with a sub_agent_id and does NOT block — \
             the child runs in the background. Call this to retrieve its output. \
             If status is 'spawned' (or 'started'), the child is still running; wait a \
             moment and call again with the same sub_agent_id. Once status is 'completed' \
             the `result` field holds the child's text conclusion.",
            JsonSchema::object(props, vec!["sub_agent_id".into()]),
        )
    }
}

/// Register this tool with the registry.
pub fn register(registry: &ToolRegistry) {
    let handler = Arc::new(GetSubAgentResultHandler);
    let schema = handler.schema();
    registry.register_with_risk(
        "get_subagent_result",
        "get_subagent_result",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        true,
        "Fetch the result of a background sub-agent spawned by delegate_task.",
        "\u{1F4E8}",
        None,
        RiskLevel::Safe,
        false,
    );
}
