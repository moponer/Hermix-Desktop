//! Approval token system — token-based approval for tool execution.
//!
//! Inspired by claw-code's approval_tokens design. Provides a mechanism
//! for users to approve tool calls via tokens rather than interactive prompts.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema, traits::{RiskLevel, PermissionMode}};

use crate::ToolRegistry;

/// Register this tool with the registry.
pub fn register(registry: &ToolRegistry) {
    let handler = Arc::new(ApprovalTokenHandler::new());
    let schema = handler.schema();
    registry.register_with_permission(
        "approval_token",
        "approval",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        false,
        "Pre-authorization tokens that let matching tool calls auto-pass WITHOUT a popup. IMPORTANT: this is NOT how approval cards are triggered — approval cards are sent AUTOMATICALLY by the server when a high-risk tool (e.g. terminal) is invoked. Only use this to grant/revoke/list auto-approve tokens.",
        "🔐",
        None,
        RiskLevel::Safe,
        PermissionMode::ReadOnly,
        false,
    );
}

// ---------------------------------------------------------------------------
// ApprovalToken
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalToken {
    pub id: String,
    pub tool_name: String,
    pub pattern: String,
    pub granted_at: String,
    pub expires_at: Option<String>,
    pub granted_by: String,
}

// ---------------------------------------------------------------------------
// ApprovalTokenHandler
// ---------------------------------------------------------------------------

pub struct ApprovalTokenHandler {
    tokens: Mutex<Vec<ApprovalToken>>,
}

impl ApprovalTokenHandler {
    pub fn new() -> Self {
        Self {
            tokens: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ToolHandler for ApprovalTokenHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'action' parameter".into()))?;

        match action {
            "grant" => {
                let tool_name = params
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'tool_name' parameter".into()))?;
                let pattern = params
                    .get("pattern")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'pattern' parameter".into()))?;
                let expires_in_secs = params.get("expires_in_secs").and_then(|v| v.as_u64());
                
                let mut tokens = self.tokens.lock().map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                let id = uuid::Uuid::new_v4().to_string();
                let now = chrono::Utc::now();
                let expires_at = expires_in_secs.map(|secs| {
                    (now + chrono::Duration::seconds(secs as i64)).to_rfc3339()
                });
                
                let token = ApprovalToken {
                    id: id.clone(),
                    tool_name: tool_name.to_string(),
                    pattern: pattern.to_string(),
                    granted_at: now.to_rfc3339(),
                    expires_at: expires_at.clone(),
                    granted_by: "user".to_string(),
                };
                tokens.push(token);
                
                Ok(json!({
                    "status": "granted",
                    "token_id": id,
                    "tool_name": tool_name,
                    "pattern": pattern,
                    "expires_at": expires_at
                }).to_string())
            }
            "revoke" => {
                let token_id = params
                    .get("token_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'token_id' parameter".into()))?;
                
                let mut tokens = self.tokens.lock().map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                let initial_len = tokens.len();
                tokens.retain(|t| t.id != token_id);
                
                if tokens.len() < initial_len {
                    Ok(json!({"status": "revoked", "token_id": token_id}).to_string())
                } else {
                    Err(ToolError::NotFound(format!("Token '{}' not found", token_id)))
                }
            }
            "check" => {
                let tool_name = params
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'tool_name' parameter".into()))?;
                let command = params.get("command").and_then(|v| v.as_str());
                
                let tokens = self.tokens.lock().map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                let now = chrono::Utc::now();
                
                // Find matching tokens
                let matching: Vec<&ApprovalToken> = tokens.iter().filter(|t| {
                    if t.tool_name != tool_name {
                        return false;
                    }
                    // Check expiration
                    if let Some(ref expires) = t.expires_at {
                        if let Ok(exp) = chrono::DateTime::parse_from_rfc3339(expires) {
                            if now > exp {
                                return false;
                            }
                        }
                    }
                    // Check pattern match
                    if let Some(cmd) = command {
                        t.pattern == "*" || cmd.contains(&t.pattern)
                    } else {
                        true
                    }
                }).collect();
                
                Ok(json!({
                    "approved": !matching.is_empty(),
                    "matching_tokens": matching.len(),
                    "tokens": matching.iter().map(|t| json!({
                        "id": t.id,
                        "pattern": t.pattern,
                        "expires_at": t.expires_at
                    })).collect::<Vec<_>>()
                }).to_string())
            }
            "list" => {
                let tokens = self.tokens.lock().map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                let tool_filter = params.get("tool_name").and_then(|v| v.as_str());
                
                let filtered: Vec<&ApprovalToken> = match tool_filter {
                    Some(t) => tokens.iter().filter(|tk| tk.tool_name == t).collect(),
                    None => tokens.iter().collect(),
                };
                
                Ok(json!({
                    "tokens": filtered,
                    "total": filtered.len()
                }).to_string())
            }
            other => Err(ToolError::InvalidParams(format!(
                "Unknown action: '{}'. Use: grant, revoke, check, list.",
                other
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "description": "Action to perform",
                "enum": ["grant", "revoke", "check", "list"]
            }),
        );
        props.insert(
            "token_id".into(),
            json!({
                "type": "string",
                "description": "Token ID (for revoke)"
            }),
        );
        props.insert(
            "tool_name".into(),
            json!({
                "type": "string",
                "description": "Tool name (for grant, check, list)"
            }),
        );
        props.insert(
            "pattern".into(),
            json!({
                "type": "string",
                "description": "Pattern to match (for grant). Use '*' for all commands."
            }),
        );
        props.insert(
            "command".into(),
            json!({
                "type": "string",
                "description": "Command to check against patterns (for check)"
            }),
        );
        props.insert(
            "expires_in_secs".into(),
            json!({
                "type": "integer",
                "description": "Token expiration in seconds (for grant). None = never expires."
            }),
        );

        tool_schema(
            "approval_token",
            "Pre-authorization tokens for tool execution. Grant a token to make matching tool calls (e.g. a command pattern) auto-approved WITHOUT an interactive approval card. This is NOT a trigger for approval cards — those are sent AUTOMATICALLY by the server when a high-risk tool call occurs. Actions: grant, revoke, check, list.",
            JsonSchema::object(props, vec!["action".into()]),
        )
    }
}
