//! Vision analysis tool

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema, traits::RiskLevel};

use crate::ToolRegistry;
use std::sync::Arc;

/// Register this tool with the registry (P1.2 per-tool migration).
pub fn register(registry: &ToolRegistry) {
    let backend = crate::backends::vision::OpenAiVisionBackend::from_env().unwrap_or_else(|_| {
        crate::backends::vision::OpenAiVisionBackend::new(
            String::new(),
            "https://api.openai.com/v1".into(),
            "gpt-4o".into(),
        )
    });
    let handler = Arc::new(VisionAnalyzeHandler::new(Arc::new(backend)));
    let schema = handler.schema();
    registry.register_with_risk(
        "vision",
        "vision",
        schema,
        handler,
        Arc::new(|| true),
        vec!["OPENAI_API_KEY".into()],
        false,
        "Analyze an image using vision AI. Provide an image URL and an optional question about the image.",
        "👁️",
        None,
        RiskLevel::Safe,
        false,
    );
}

// ---------------------------------------------------------------------------
// VisionBackend trait
// ---------------------------------------------------------------------------

/// Backend for vision analysis operations.
#[async_trait]
pub trait VisionBackend: Send + Sync {
    /// Analyze an image at the given URL with an optional question.
    async fn analyze(&self, image_url: &str, question: &str) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// VisionAnalyzeHandler
// ---------------------------------------------------------------------------

/// Tool for analyzing images using vision models.
pub struct VisionAnalyzeHandler {
    backend: Arc<dyn VisionBackend>,
}

impl VisionAnalyzeHandler {
    pub fn new(backend: Arc<dyn VisionBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for VisionAnalyzeHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let image_url = params
            .get("image_url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'image_url' parameter".into()))?;

        let question = params
            .get("question")
            .and_then(|v| v.as_str())
            .unwrap_or("Describe this image.");

        self.backend.analyze(image_url, question).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "image_url".into(),
            json!({
                "type": "string",
                "description": "URL of the image to analyze"
            }),
        );
        props.insert(
            "question".into(),
            json!({
                "type": "string",
                "description": "Question to ask about the image (default: 'Describe this image.')"
            }),
        );

        tool_schema(
            "vision_analyze",
            "Analyze an image using vision AI. Provide an image URL and an optional question about the image.",
            JsonSchema::object(props, vec!["image_url".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockVisionBackend;
    #[async_trait]
    impl VisionBackend for MockVisionBackend {
        async fn analyze(&self, url: &str, question: &str) -> Result<String, ToolError> {
            Ok(format!("Analysis of {} for: {}", url, question))
        }
    }

    #[tokio::test]
    async fn test_vision_analyze_schema() {
        let handler = VisionAnalyzeHandler::new(Arc::new(MockVisionBackend));
        assert_eq!(handler.schema().name, "vision_analyze");
    }

    #[tokio::test]
    async fn test_vision_analyze_execute() {
        let handler = VisionAnalyzeHandler::new(Arc::new(MockVisionBackend));
        let result = handler
            .execute(
                json!({"image_url": "https://example.com/img.png", "question": "What is this?"}),
            )
            .await
            .unwrap();
        assert!(result.contains("example.com"));
    }
}
