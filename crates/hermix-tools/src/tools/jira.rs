//! Jira integration tool — create and search issues.
//!
//! Reads `JIRA_URL`, `JIRA_EMAIL`, `JIRA_API_TOKEN` from the environment.
//! Uses reqwest's built-in basic auth against the Jira REST API v3.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_core::{tool_schema, traits::RiskLevel, ToolError, ToolHandler, ToolSchema};

use std::sync::Arc;

use crate::ToolRegistry;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

const JIRA_URL_ENV: &str = "JIRA_URL";
const JIRA_EMAIL_ENV: &str = "JIRA_EMAIL";
const JIRA_TOKEN_ENV: &str = "JIRA_API_TOKEN";

fn jira_config() -> Result<(String, String, String), ToolError> {
    let url = std::env::var(JIRA_URL_ENV)
        .map_err(|_| ToolError::InvalidParams("Jira 未配置。请前往「设置 → API Keys」添加 Jira URL。".into()))?;
    let email = std::env::var(JIRA_EMAIL_ENV)
        .map_err(|_| ToolError::InvalidParams("Jira 未配置。请前往「设置 → API Keys」添加 Jira 邮箱。".into()))?;
    let token = std::env::var(JIRA_TOKEN_ENV)
        .map_err(|_| ToolError::InvalidParams("Jira 未配置。请前往「设置 → API Keys」添加 Jira API Token。".into()))?;
    Ok((url, email, token))
}

// ---------------------------------------------------------------------------
// Register
// ---------------------------------------------------------------------------

pub fn register(registry: &ToolRegistry) {
    let handler = Arc::new(JiraHandler);
    let schema = handler.schema();
    registry.register_with_risk(
        "jira",
        "project_management",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        true,
        "Create and search Jira issues",
        "🎯",
        None,
        RiskLevel::Safe,
        false,
    );
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

struct JiraHandler;

#[async_trait]
impl ToolHandler for JiraHandler {
    fn schema(&self) -> ToolSchema {
        let mut props: IndexMap<String, Value> = IndexMap::new();
        props.insert("action".into(), json!({
            "type": "string", "description": "Action: create (new issue), search (JQL), get (details by key)"
        }));
        props.insert("project".into(), json!({
            "type": "string", "description": "Project key (e.g. PROJ). Required for create."
        }));
        props.insert("summary".into(), json!({
            "type": "string", "description": "Issue summary/title. Required for create."
        }));
        props.insert("issuetype".into(), json!({
            "type": "string", "description": "Issue type (Task, Bug, Story). Default: Task.", "default": "Task"
        }));
        props.insert("description".into(), json!({
            "type": "string", "description": "Issue description."
        }));
        props.insert("jql".into(), json!({
            "type": "string", "description": "JQL query for search action."
        }));
        props.insert("issue_key".into(), json!({
            "type": "string", "description": "Issue key (e.g. PROJ-123). Required for get."
        }));
        props.insert("max_results".into(), json!({
            "type": "integer", "description": "Max results (default: 10, max: 50).", "default": 10
        }));
        tool_schema(
            "jira",
            "Interact with Jira: create issues, search issues, get issue details.",
            hermix_core::JsonSchema::object(props, vec!["action".into()]),
        )
    }

    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let (base_url, email, token) = jira_config()?;
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| ToolError::ExecutionFailed(format!("HTTP client: {e}")))?;

        match action {
            "create" => {
                let project = params.get("project").and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("project is required".into()))?;
                let summary = params.get("summary").and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("summary is required".into()))?;
                let issuetype = params.get("issuetype").and_then(|v| v.as_str()).unwrap_or("Task");
                let description = params.get("description").and_then(|v| v.as_str());

                let mut fields = json!({
                    "project": { "key": project },
                    "summary": summary,
                    "issuetype": { "name": issuetype },
                });
                if let Some(desc) = description {
                    if !desc.is_empty() {
                        fields["description"] = json!({
                            "type": "doc", "version": 1,
                            "content": [{"type": "paragraph", "content": [{"type": "text", "text": desc}]}]
                        });
                    }
                }

                let resp = client
                    .post(format!("{}/rest/api/3/issue", base_url.trim_end_matches('/')))
                    .basic_auth(&email, Some(&token))
                    .header("Accept", "application/json")
                    .json(&json!({ "fields": fields }))
                    .send().await
                    .map_err(|e| ToolError::ExecutionFailed(format!("Jira create: {e}")))?;
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                if status.is_success() || status.as_u16() == 201 {
                    if let Ok(parsed) = serde_json::from_str::<Value>(&body) {
                        return Ok(format!("✅ Created Jira issue {}", parsed["key"].as_str().unwrap_or("?")));
                    }
                    Ok("✅ Jira issue created".into())
                } else {
                    Err(ToolError::ExecutionFailed(format!("Jira returned {status}: {body}")))
                }
            }

            "search" => {
                let jql = params.get("jql").and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("jql is required".into()))?;
                let max = params.get("max_results").and_then(|v| v.as_u64()).unwrap_or(10).min(50);
                let enc: String = jql.chars().map(|c| match c {
                    'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
                    ' ' => "+".to_string(),
                    o => format!("%{:02X}", o as u8),
                }).collect();

                let resp = client
                    .get(format!("{}/rest/api/3/search?jql={enc}&maxResults={max}", base_url.trim_end_matches('/')))
                    .basic_auth(&email, Some(&token))
                    .header("Accept", "application/json")
                    .send().await
                    .map_err(|e| ToolError::ExecutionFailed(format!("Jira search: {e}")))?;
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                if status.is_success() {
                    if let Ok(parsed) = serde_json::from_str::<Value>(&body) {
                        let total = parsed["total"].as_u64().unwrap_or(0);
                        let list = parsed["issues"].as_array().map(|arr| arr.iter().map(|i|
                            format!("• {} [{}] — {}", i["key"].as_str().unwrap_or("?"), i["fields"]["status"]["name"].as_str().unwrap_or("?"), i["fields"]["summary"].as_str().unwrap_or("?"))
                        ).collect::<Vec<_>>().join("\n")).unwrap_or_default();
                        return Ok(format!("Found {total} issues:\n{list}"));
                    }
                    Ok("Search completed".into())
                } else {
                    Err(ToolError::ExecutionFailed(format!("Jira returned {status}: {body}")))
                }
            }

            "get" => {
                let key = params.get("issue_key").and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("issue_key is required".into()))?;
                let resp = client
                    .get(format!("{}/rest/api/3/issue/{key}", base_url.trim_end_matches('/')))
                    .basic_auth(&email, Some(&token))
                    .header("Accept", "application/json")
                    .send().await
                    .map_err(|e| ToolError::ExecutionFailed(format!("Jira get: {e}")))?;
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                if status.is_success() {
                    if let Ok(parsed) = serde_json::from_str::<Value>(&body) {
                        return Ok(format!("📋 {}: {}\nStatus: {} | Priority: {} | Assignee: {}",
                            parsed["key"].as_str().unwrap_or("?"),
                            parsed["fields"]["summary"].as_str().unwrap_or("?"),
                            parsed["fields"]["status"]["name"].as_str().unwrap_or("?"),
                            parsed["fields"]["priority"]["name"].as_str().unwrap_or("None"),
                            parsed["fields"]["assignee"]["displayName"].as_str().unwrap_or("Unassigned")));
                    }
                    Ok(format!("Got issue {key}"))
                } else {
                    Err(ToolError::ExecutionFailed(format!("Jira returned {status}: {body}")))
                }
            }

            _ => Err(ToolError::InvalidParams(format!("Unknown action '{action}'. Use: create, search, get"))),
        }
    }
}
