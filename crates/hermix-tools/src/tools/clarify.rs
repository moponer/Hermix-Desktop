//! Clarify tool: ask user questions with choices

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema, traits::RiskLevel};

use crate::ToolRegistry;
use crate::ToolCallContext;
use crate::context::ContextAware;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register this tool with the registry (P1.2 migration).
///
/// `clarify` is a `ContextAware` tool: when dispatched through
/// `ToolRegistry::dispatch_with_context` (the path the desktop agent
/// loop uses via `bridge_tool_registry`), the per-session
/// `ToolCallContext.session_id` is threaded straight into the backend's
/// `ask` call. That session id is exactly what the server's WebSocket
/// layer uses to route the `ClarifyRequest` event to the correct client
/// pane — without it the event is dropped by the per-session filter and
/// the user never sees the question card.
///
/// Passing the session id as an explicit argument (rather than a shared
/// mutable "active session" field) keeps the dispatch race-free even
/// when several sessions await a `clarify` answer concurrently.
pub fn register(registry: &ToolRegistry, backend: Arc<dyn ClarifyBackend>) {
    let handler = Arc::new(ClarifyHandler::new(backend));
    let schema = handler.schema();
    registry.register_with_risk(
        "clarify",
        "clarify",
        schema,
        handler.clone(),
        Arc::new(|| true),
        vec![],
        true,
        "Ask the user a clarification question. Optionally provide choices for the user to select from.",
        "❓",
        None,
        RiskLevel::Safe,
        false,
    );
    registry.set_context_aware("clarify", handler);
}

// ---------------------------------------------------------------------------
// ClarifyBackend trait
// ---------------------------------------------------------------------------

/// Backend for presenting clarification questions to the user.
#[async_trait]
pub trait ClarifyBackend: Send + Sync {
    /// Ask the user a question and return their answer.
    ///
    /// `session_id` is the originating session (best-effort, supplied by
    /// the `ContextAware` dispatch path on the desktop). Backends should
    /// stamp it onto the outgoing request so the transport layer can
    /// route the question to the right client.
    async fn ask(
        &self,
        question: &str,
        choices: Option<&[String]>,
        session_id: Option<String>,
    ) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// ClarifyHandler
// ---------------------------------------------------------------------------

/// Tool for asking the user clarification questions.
pub struct ClarifyHandler {
    backend: Arc<dyn ClarifyBackend>,
}

impl ClarifyHandler {
    pub fn new(backend: Arc<dyn ClarifyBackend>) -> Self {
        Self { backend }
    }

    /// Validate + extract the tool arguments. Shared by both the plain
    /// `execute` path and the `ContextAware` `execute_with_context` path
    /// so the two never drift.
    fn parse(params: &Value) -> Result<(String, Option<Vec<String>>), ToolError> {
        let question = params
            .get("question")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'question' parameter".into()))?
            .to_string();

        let choices: Option<Vec<String>> =
            params.get("choices").and_then(|v| v.as_array()).map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            });

        Ok((question, choices))
    }
}

#[async_trait]
impl ToolHandler for ClarifyHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let (question, choices) = Self::parse(&params)?;
        // No session context available on the plain path; the desktop
        // always dispatches clarify through `dispatch_with_context`, so
        // this `None` is only hit by scripted / test callers.
        self.backend.ask(&question, choices.as_deref(), None).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "question".into(),
            json!({
                "type": "string",
                "description": "The question to ask the user"
            }),
        );
        props.insert(
            "choices".into(),
            json!({
                "type": "array",
                "description": "Optional list of choices for the user to select from",
                "items": { "type": "string" }
            }),
        );

        tool_schema(
            "clarify",
            "Ask the user a clarification question. Optionally provide choices for the user to select from.",
            JsonSchema::object(props, vec!["question".into()]),
        )
    }
}

#[async_trait]
impl ContextAware for ClarifyHandler {
    async fn execute_with_context(
        &self,
        params: Value,
        ctx: &ToolCallContext,
    ) -> Result<String, ToolError> {
        let (question, choices) = Self::parse(&params)?;
        self.backend
            .ask(&question, choices.as_deref(), Some(ctx.session_id.clone()))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockClarifyBackend;
    #[async_trait]
    impl ClarifyBackend for MockClarifyBackend {
        async fn ask(
            &self,
            question: &str,
            _choices: Option<&[String]>,
            _session_id: Option<String>,
        ) -> Result<String, ToolError> {
            Ok(format!("User answered: (question was: {})", question))
        }
    }

    #[tokio::test]
    async fn test_clarify_schema() {
        let handler = ClarifyHandler::new(Arc::new(MockClarifyBackend));
        assert_eq!(handler.schema().name, "clarify");
    }

    #[tokio::test]
    async fn test_clarify_execute() {
        let handler = ClarifyHandler::new(Arc::new(MockClarifyBackend));
        let result = handler
            .execute(json!({"question": "Which option?", "choices": ["A", "B"]}))
            .await
            .unwrap();
        assert!(result.contains("Which option?"));
    }
}
