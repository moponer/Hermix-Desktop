//! Session search tool: search past conversations using FTS5

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_core::{tool_schema, traits::RiskLevel, JsonSchema, ToolError, ToolHandler, ToolSchema};

use std::sync::Arc;

use crate::ToolRegistry;

/// Register the session_search tool with the registry (P1.2 per-tool migration).
pub fn register(registry: &ToolRegistry) {
    let db_path = hermix_config::paths::sessions_db_path();
    match crate::backends::session_search::SqliteSessionSearchBackend::new(
        &db_path.to_string_lossy(),
    ) {
        Ok(backend) => {
            let handler = Arc::new(SessionSearchHandler::new(Arc::new(backend)));
            let schema = handler.schema();
            registry.register_with_risk(
                "session_search",
                "session_search",
                schema,
                handler,
                Arc::new(|| true),
                vec![],
                true,
                "Search past conversation sessions using FTS5",
                "🔍",
                None,
                RiskLevel::Safe,
                false,
            );
        }
        Err(e) => {
            tracing::warn!("Failed to initialise session search DB: {e}; skipping session_search tool");
        }
    }
}

// ---------------------------------------------------------------------------
// SessionSearchBackend trait
// ---------------------------------------------------------------------------

/// Backend for searching past conversation sessions.
#[async_trait]
pub trait SessionSearchBackend: Send + Sync {
    /// Search past conversations using FTS5 full-text search.
    async fn search(
        &self,
        query: Option<&str>,
        role_filter: Option<&str>,
        limit: usize,
        current_session_id: Option<&str>,
    ) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// SessionSearchHandler
// ---------------------------------------------------------------------------

/// Tool for searching past conversation sessions using FTS5.
pub struct SessionSearchHandler {
    backend: Arc<dyn SessionSearchBackend>,
}

impl SessionSearchHandler {
    pub fn new(backend: Arc<dyn SessionSearchBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for SessionSearchHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let query = params.get("query").and_then(|v| v.as_str());
        let role_filter = params.get("role_filter").and_then(|v| v.as_str());
        let current_session_id = params.get("current_session_id").and_then(|v| v.as_str());
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(3) as usize;
        let capped_limit = limit.min(5);
        self.backend
            .search(query, role_filter, capped_limit, current_session_id)
            .await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "query".into(),
            json!({
                "type": "string",
                "description": "Search query for finding past conversations. Omit to list recent sessions."
            }),
        );
        props.insert(
            "role_filter".into(),
            json!({
                "type": "string",
                "description": "Optional comma-separated roles to include, e.g. 'user,assistant'."
            }),
        );
        props.insert(
            "limit".into(),
            json!({
                "type": "integer",
                "description": "Max sessions to return/summarize (default: 3, max: 5).",
                "default": 3
            }),
        );
        props.insert(
            "current_session_id".into(),
            json!({
                "type": "string",
                "description": "Optional active session id used to exclude current lineage from search results."
            }),
        );

        tool_schema(
            "session_search",
            "Search long-term memory of past conversations, or browse recent sessions when query is omitted.\n\n\
             BOUNDARY (three memory layers, pick one): `session_search` = READ-ONLY recall of \
             what was said in earlier sessions (\"we discussed X last week\") — it never writes; \
             `memory` = durable facts you want injected into every turn; `daily_memory` = this \
             task's working log. Use this when the user references the past, not to store new facts.",
            JsonSchema::object(props, vec![]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockSessionSearchBackend;
    #[async_trait]
    impl SessionSearchBackend for MockSessionSearchBackend {
        async fn search(
            &self,
            query: Option<&str>,
            role_filter: Option<&str>,
            limit: usize,
            current_session_id: Option<&str>,
        ) -> Result<String, ToolError> {
            Ok(format!(
                "Found {} results for '{:?}' with role_filter={:?}, current_session_id={:?} (limit: {})",
                0, query, role_filter, current_session_id, limit
            ))
        }
    }

    #[tokio::test]
    async fn test_session_search_schema() {
        let handler = SessionSearchHandler::new(Arc::new(MockSessionSearchBackend));
        assert_eq!(handler.schema().name, "session_search");
    }

    #[tokio::test]
    async fn test_session_search_execute() {
        let handler = SessionSearchHandler::new(Arc::new(MockSessionSearchBackend));
        let result = handler
            .execute(json!({"query": "rust async"}))
            .await
            .unwrap();
        assert!(result.contains("rust async"));
    }
}
