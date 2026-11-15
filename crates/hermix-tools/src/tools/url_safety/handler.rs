//! `UrlSafetyHandler` — the LLM-invokable tool entrypoint for URL safety checks.
use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};
use super::policy::WebsitePolicy;


/// Tool handler for URL safety checks. Uses the `WebsitePolicy` engine.
pub struct UrlSafetyHandler {
    policy: Arc<WebsitePolicy>,
}

impl UrlSafetyHandler {
    pub fn new(policy: Arc<WebsitePolicy>) -> Self {
        Self { policy }
    }
}

impl Default for UrlSafetyHandler {
    fn default() -> Self {
        // Try to load from default path. Delegate to the
        // canonical resolver so HERMIX_HOME and the portable
        // mode work here too.
        let policy_path = hermix_config::paths::hermix_home().join("website_policy.yaml");

        let policy = if policy_path.exists() {
            WebsitePolicy::from_file(policy_path)
        } else {
            WebsitePolicy::new()
        };

        Self {
            policy: Arc::new(policy),
        }
    }
}

#[async_trait]
impl ToolHandler for UrlSafetyHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let url = params.get("url").and_then(|v| v.as_str()).unwrap_or("");
        if url.is_empty() {
            return Err(ToolError::InvalidParams("Missing 'url'".into()));
        }

        let result = self.policy.check_url(url);

        Ok(json!({
            "url": result.url,
            "safe": result.safe,
            "action": format!("{:?}", result.action).to_lowercase(),
            "reason": result.reason,
            "matched_rule": result.matched_rule,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "url".into(),
            json!({
                "type": "string",
                "description": "URL to check against the website safety policy"
            }),
        );
        tool_schema(
            "url_safety",
            "Check whether a URL is safe to access based on the website policy engine. \
             Evaluates domain whitelist/blacklist, path patterns, and regex rules. \
             Policy is loaded from ~/.hermix/website_policy.yaml with hot-reload support.",
            JsonSchema::object(props, vec!["url".into()]),
        )
    }
}

