//! Model providers tool — list and configure LLM providers.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use hermix_core::{tool_schema, traits::RiskLevel, JsonSchema, ToolError, ToolHandler, ToolSchema};

use std::collections::HashMap;
use std::sync::Arc;

use crate::ToolRegistry;

/// Register the model_providers tool with the registry (P1.2 per-tool migration).
pub fn register(registry: &ToolRegistry) {
    let providers_path = hermix_config::paths::hermix_home().join("model_providers.json");
    let handler = Arc::new(ModelProvidersHandler::new(Arc::new(
        FileModelProvidersBackend::new(providers_path),
    )));
    let schema = handler.schema();
    registry.register_with_risk(
        "model_providers",
        "model_providers",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        true,
        "List and configure LLM providers",
        "🔧",
        None,
        RiskLevel::Safe,
        false,
    );
}

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub key_env_var: String,
    pub enabled: bool,
    pub models: Vec<String>,
}

// ---------------------------------------------------------------------------
// ModelProvidersBackend trait
// ---------------------------------------------------------------------------

#[async_trait]
pub trait ModelProvidersBackend: Send + Sync {
    async fn list_providers(&self) -> Result<String, ToolError>;
    async fn configure_provider(
        &self,
        provider_id: &str,
        enabled: Option<bool>,
    ) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// FileModelProvidersBackend — JSON-file backed
// ---------------------------------------------------------------------------

pub struct FileModelProvidersBackend {
    path: std::path::PathBuf,
}

impl FileModelProvidersBackend {
    pub fn new(path: std::path::PathBuf) -> Self {
        Self { path }
    }

    fn load(&self) -> HashMap<String, Provider> {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self, providers: &HashMap<String, Provider>) {
        let _ = std::fs::create_dir_all(self.path.parent().unwrap_or(std::path::Path::new(".")));
        if let Ok(json) = serde_json::to_string_pretty(providers) {
            let _ = std::fs::write(&self.path, json);
        }
    }
}

#[async_trait]
impl ModelProvidersBackend for FileModelProvidersBackend {
    async fn list_providers(&self) -> Result<String, ToolError> {
        let providers = self.load();
        let result: Vec<Value> = providers
            .values()
            .map(|p| {
                json!({
                    "id": p.id,
                    "name": p.name,
                    "base_url": p.base_url,
                    "key_env_var": p.key_env_var,
                    "enabled": p.enabled,
                    "models": p.models,
                })
            })
            .collect();
        serde_json::to_string_pretty(&result)
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))
    }

    async fn configure_provider(
        &self,
        provider_id: &str,
        enabled: Option<bool>,
    ) -> Result<String, ToolError> {
        let mut providers = self.load();
        let (name, new_enabled);
        {
            let provider = providers
                .get_mut(provider_id)
                .ok_or_else(|| {
                    ToolError::InvalidParams(format!("Provider '{}' not found", provider_id))
                })?;

            if let Some(en) = enabled {
                provider.enabled = en;
            }
            name = provider.name.clone();
            new_enabled = provider.enabled;
        }
        self.save(&providers);
        Ok(format!(
            "Provider '{}' configured (enabled: {})",
            name, new_enabled
        ))
    }
}

// ---------------------------------------------------------------------------
// ModelProvidersHandler
// ---------------------------------------------------------------------------

pub struct ModelProvidersHandler {
    backend: Arc<dyn ModelProvidersBackend>,
}

impl ModelProvidersHandler {
    pub fn new(backend: Arc<dyn ModelProvidersBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for ModelProvidersHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'action' parameter".into()))?;

        match action {
            "list" => self.backend.list_providers().await,
            "configure" => {
                let provider_id = params
                    .get("provider_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::InvalidParams("Missing 'provider_id' parameter".into())
                    })?;
                // See `cronjob`: a string-encoded boolean must not read
                // as "no change", or enabling/disabling a provider is
                // silently dropped.
                let enabled = crate::param_coercion::opt_bool_param(&params, "enabled");
                self.backend.configure_provider(provider_id, enabled).await
            }
            other => Err(ToolError::InvalidParams(format!(
                "Unknown action: '{}'. Use 'list' or 'configure'.",
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
                "description": "Action: list or configure",
                "enum": ["list", "configure"]
            }),
        );
        props.insert(
            "provider_id".into(),
            json!({
                "type": "string",
                "description": "Provider ID (for configure)"
            }),
        );
        props.insert(
            "enabled".into(),
            json!({
                "type": "boolean",
                "description": "Enable/disable provider (for configure)"
            }),
        );

        tool_schema(
            "model_providers",
            "List and configure LLM providers (OpenAI, Anthropic, MiniMax, etc.).",
            JsonSchema::object(props, vec!["action".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockModelProvidersBackend;
    #[async_trait]
    impl ModelProvidersBackend for MockModelProvidersBackend {
        async fn list_providers(&self) -> Result<String, ToolError> {
            Ok("[]".to_string())
        }
        async fn configure_provider(
            &self,
            _provider_id: &str,
            _enabled: Option<bool>,
        ) -> Result<String, ToolError> {
            Ok("Configured".to_string())
        }
    }

    #[tokio::test]
    async fn test_model_providers_list() {
        let handler = ModelProvidersHandler::new(Arc::new(MockModelProvidersBackend));
        let result = handler.execute(json!({"action": "list"})).await.unwrap();
        assert_eq!(result, "[]");
    }

    #[tokio::test]
    async fn test_model_providers_schema() {
        let handler = ModelProvidersHandler::new(Arc::new(MockModelProvidersBackend));
        assert_eq!(handler.schema().name, "model_providers");
    }
}
