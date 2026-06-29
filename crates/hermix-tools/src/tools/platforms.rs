//! Platforms tool — list and switch terminal backends.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use std::collections::HashMap;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Platform {
    pub id: String,
    pub name: String,
    pub description: String,
    pub active: bool,
}

// ---------------------------------------------------------------------------
// PlatformsBackend trait
// ---------------------------------------------------------------------------

#[async_trait]
pub trait PlatformsBackend: Send + Sync {
    async fn list_platforms(&self) -> Result<String, ToolError>;
    async fn switch_platform(&self, platform_id: &str) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// FilePlatformsBackend — JSON-file backed
// ---------------------------------------------------------------------------

pub struct FilePlatformsBackend {
    path: std::path::PathBuf,
}

impl FilePlatformsBackend {
    pub fn new(path: std::path::PathBuf) -> Self {
        Self { path }
    }

    fn load(&self) -> HashMap<String, Platform> {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self, platforms: &HashMap<String, Platform>) {
        let _ = std::fs::create_dir_all(self.path.parent().unwrap_or(std::path::Path::new(".")));
        if let Ok(json) = serde_json::to_string_pretty(platforms) {
            let _ = std::fs::write(&self.path, json);
        }
    }
}

#[async_trait]
impl PlatformsBackend for FilePlatformsBackend {
    async fn list_platforms(&self) -> Result<String, ToolError> {
        let platforms = self.load();
        let result: Vec<Value> = platforms
            .values()
            .map(|p| {
                json!({
                    "id": p.id,
                    "name": p.name,
                    "description": p.description,
                    "active": p.active,
                })
            })
            .collect();
        serde_json::to_string_pretty(&result)
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))
    }

    async fn switch_platform(&self, platform_id: &str) -> Result<String, ToolError> {
        let mut platforms = self.load();
        if !platforms.contains_key(platform_id) {
            return Err(ToolError::InvalidParams(format!(
                "Platform '{}' not found",
                platform_id
            )));
        }

        // Deactivate all, activate the selected one
        for (id, p) in platforms.iter_mut() {
            p.active = id == platform_id;
        }
        self.save(&platforms);
        Ok(format!("Switched to platform '{}'", platform_id))
    }
}

// ---------------------------------------------------------------------------
// PlatformsHandler
// ---------------------------------------------------------------------------

pub struct PlatformsHandler {
    backend: Arc<dyn PlatformsBackend>,
}

impl PlatformsHandler {
    pub fn new(backend: Arc<dyn PlatformsBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for PlatformsHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'action' parameter".into()))?;

        match action {
            "list" => self.backend.list_platforms().await,
            "switch" => {
                let platform_id = params
                    .get("platform_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::InvalidParams("Missing 'platform_id' parameter".into())
                    })?;
                self.backend.switch_platform(platform_id).await
            }
            other => Err(ToolError::InvalidParams(format!(
                "Unknown action: '{}'. Use 'list' or 'switch'.",
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
                "description": "Action: list or switch",
                "enum": ["list", "switch"]
            }),
        );
        props.insert(
            "platform_id".into(),
            json!({
                "type": "string",
                "description": "Platform ID (for switch)"
            }),
        );

        tool_schema(
            "platforms",
            "List and switch terminal backends (local, Docker, SSH, etc.).",
            JsonSchema::object(props, vec!["action".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockPlatformsBackend;
    #[async_trait]
    impl PlatformsBackend for MockPlatformsBackend {
        async fn list_platforms(&self) -> Result<String, ToolError> {
            Ok("[]".to_string())
        }
        async fn switch_platform(&self, _platform_id: &str) -> Result<String, ToolError> {
            Ok("Switched".to_string())
        }
    }

    #[tokio::test]
    async fn test_platforms_list() {
        let handler = PlatformsHandler::new(Arc::new(MockPlatformsBackend));
        let result = handler.execute(json!({"action": "list"})).await.unwrap();
        assert_eq!(result, "[]");
    }

    #[tokio::test]
    async fn test_platforms_schema() {
        let handler = PlatformsHandler::new(Arc::new(MockPlatformsBackend));
        assert_eq!(handler.schema().name, "platforms");
    }
}
