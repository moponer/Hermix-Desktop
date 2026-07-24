//! Slack integration tool — send messages via webhook.
//!
//! Reads `SLACK_WEBHOOK_URL` from the environment. When not configured,
//! the tool returns a clear "not configured" error.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_core::{tool_schema, traits::RiskLevel, ToolError, ToolHandler, ToolSchema};

use std::sync::Arc;

use crate::ToolRegistry;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

const SLACK_WEBHOOK_ENV: &str = "SLACK_WEBHOOK_URL";

fn webhook_url() -> Result<String, ToolError> {
    std::env::var(SLACK_WEBHOOK_ENV).map_err(|_| {
        ToolError::InvalidParams(
            "Slack 未配置。请前往「设置 → API Keys」添加 Slack Webhook URL。".into(),
        )
    })
}

// ---------------------------------------------------------------------------
// Register
// ---------------------------------------------------------------------------

pub fn register(registry: &ToolRegistry) {
    let handler = Arc::new(SlackHandler);
    let schema = handler.schema();
    registry.register_with_risk(
        "slack",
        "messaging",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        true,
        "Send messages to Slack via webhook",
        "💬",
        None,
        RiskLevel::Safe,
        false,
    );
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

struct SlackHandler;

#[async_trait]
impl ToolHandler for SlackHandler {
    fn schema(&self) -> ToolSchema {
        let mut props: IndexMap<String, Value> = IndexMap::new();
        props.insert(
            "channel".into(),
            json!({"type": "string", "description": "Channel name (e.g. #general). Optional."}),
        );
        props.insert(
            "message".into(),
            json!({"type": "string", "description": "Message text (supports Slack markdown)."}),
        );
        tool_schema(
            "slack",
            "Send a message to a Slack channel via webhook.",
            hermix_core::JsonSchema::object(props, vec!["message".into()]),
        )
    }

    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let url = webhook_url()?;

        let message = params
            .get("message")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::InvalidParams("message is required".into()))?;

        let mut payload = json!({ "text": message });
        if let Some(channel) = params.get("channel").and_then(|v| v.as_str()) {
            let ch = channel.trim();
            if !ch.is_empty() {
                payload["channel"] = json!(ch);
            }
        }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| ToolError::ExecutionFailed(format!("HTTP client: {e}")))?;

        let resp = client.post(&url).json(&payload).send().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Slack API request failed: {e}"))
        })?;

        let status = resp.status();
        let body = match resp.text().await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("Failed to read Slack API response body: {e}");
                String::from("<unreadable response body>")
            }
        };

        if status.is_success() {
            Ok(format!("✅ Message sent to Slack (status {status})"))
        } else {
            Err(ToolError::ExecutionFailed(format!(
                "Slack API returned {status}: {body}"
            )))
        }
    }
}
