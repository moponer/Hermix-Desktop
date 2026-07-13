use std::sync::Arc;

use hermix_core::{JsonSchema, ToolError, ToolHandler, ToolSchema};
use indexmap::IndexMap;
use serde_json::Value;

use crate::ToolRegistry;
use crate::tool_meta::{ToolMetaRegistry, DEFAULT_RESIDENT_TOOLS};

/// Tools that are ALWAYS visible to the LLM (never deferred).
/// Anything not in this list becomes a candidate for tool_search bridge.
///
/// P6-1: This constant is kept for backwards compatibility with
/// `classify_tools`. The canonical source is now
/// [`crate::tool_meta::DEFAULT_RESIDENT_TOOLS`] (same contents).
const CORE_TOOLS: &[&str] = DEFAULT_RESIDENT_TOOLS;

pub const BRIDGE_TOOL_NAMES: &[&str] = &["tool_search", "tool_describe", "tool_call"];

/// Threshold: when deferred tool count exceeds this, activate bridge.
/// This is a count-based threshold; token-based is future work.
pub(crate) const DEFERRED_THRESHOLD: usize = 25;

// ── Classification ──────────────────────────────────────────────────

fn is_core(name: &str) -> bool {
    CORE_TOOLS.iter().any(|c| *c == name)
}

pub fn is_bridge(name: &str) -> bool {
    BRIDGE_TOOL_NAMES.iter().any(|b| *b == name)
}

/// Classify tools into (visible, deferred) using the hard-coded
/// `CORE_TOOLS` list. Maintained for backwards compatibility.
///
/// Prefer [`classify_tools_with_meta`] when a [`ToolMetaRegistry`] is
/// available — it uses per-tool metadata instead of a hard-coded list.
pub fn classify_tools(schemas: &[ToolSchema]) -> (Vec<&ToolSchema>, Vec<&ToolSchema>) {
    let mut visible = Vec::new();
    let mut deferred = Vec::new();
    for s in schemas {
        if is_core(&s.name) || is_bridge(&s.name) {
            visible.push(s);
        } else {
            deferred.push(s);
        }
    }
    (visible, deferred)
}

/// Classify tools into (visible, deferred) using a [`ToolMetaRegistry`].
///
/// P6-1: This is the metadata-driven replacement for [`classify_tools`].
/// Tools registered as `LoadingStrategy::Deferred` in the metadata
/// registry are placed in the deferred bucket; all others (including
/// unregistered tools and bridge tools) are visible.
///
/// When the metadata registry is empty or a tool is not registered,
/// the function falls back to the `CORE_TOOLS` list so behaviour is
/// preserved for callers that haven't migrated yet.
pub fn classify_tools_with_meta<'a>(
    schemas: &'a [ToolSchema],
    meta: &'a ToolMetaRegistry,
) -> (Vec<&'a ToolSchema>, Vec<&'a ToolSchema>) {
    let mut visible = Vec::new();
    let mut deferred = Vec::new();
    for s in schemas {
        if is_bridge(&s.name) {
            // Bridge tools are always visible
            visible.push(s);
        } else if meta.get(&s.name).is_some() {
            // Tool is explicitly registered — honour the metadata.
            // Note: `meta.is_resident` returns true for unregistered tools
            // (defensive default), so we must gate the meta path on
            // `meta.get(...).is_some()` first to avoid silently promoting
            // every unknown tool to "resident".
            if meta.is_deferred(&s.name) {
                deferred.push(s);
            } else {
                visible.push(s);
            }
        } else if is_core(&s.name) {
            // Unregistered tool that happens to be in the hard-coded
            // CORE_TOOLS whitelist — keep it visible for backwards
            // compatibility with callers that haven't migrated.
            visible.push(s);
        } else {
            deferred.push(s);
        }
    }
    (visible, deferred)
}

// ── Assembly ────────────────────────────────────────────────────────

/// Result of progressive disclosure assembly.
pub struct AssemblyResult {
    pub tool_schemas: Vec<ToolSchema>,
    pub activated: bool,
    pub deferred_count: usize,
}

/// If deferred tools exceed threshold, replace them with 3 bridge tools.
/// Otherwise pass through all tools unchanged.
pub fn assemble_tool_schemas(all: Vec<ToolSchema>) -> AssemblyResult {
    // Strip any existing bridge tools (idempotent re-entry)
    let incoming: Vec<ToolSchema> = all
        .into_iter()
        .filter(|s| !is_bridge(&s.name))
        .collect();
    let (visible, deferred) = classify_tools(&incoming);
    if deferred.len() < DEFERRED_THRESHOLD {
        return AssemblyResult {
            tool_schemas: incoming,
            activated: false,
            deferred_count: 0,
        };
    }
    let mut result: Vec<ToolSchema> = visible.into_iter().cloned().collect();
    let count = deferred.len();
    result.extend(bridge_tool_schemas(count));
    AssemblyResult {
        tool_schemas: result,
        activated: true,
        deferred_count: count,
    }
}

pub(crate) fn bridge_tool_schemas(deferred_count: usize) -> Vec<ToolSchema> {
    vec![
        ToolSchema::new(
            "tool_search",
            &format!(
                "Search {deferred_count} additional tools loaded on demand. \
                 Returns up to `limit` matches with name and description. \
                 Follow with `tool_describe` to load a tool's full parameter schema, \
                 then `tool_call` to invoke it. Tools listed in the system prompt \
                 are already available and do not need searching."
            ),
            JsonSchema::object(
                {
                    let mut m = IndexMap::new();
                    m.insert(
                        "query".into(),
                        serde_json::json!({
                            "type": "string",
                            "description": "Keywords to search for (e.g. 'create github issue')"
                        }),
                    );
                    m.insert(
                        "limit".into(),
                        serde_json::json!({
                            "type": "integer",
                            "description": "Max results. Default 5."
                        }),
                    );
                    m
                },
                vec!["query".into()],
            ),
        ),
        ToolSchema::new(
            "tool_describe",
            "Load the full JSON schema for a tool returned by tool_search. \
             Required before tool_call if the tool's parameters are unknown.",
            JsonSchema::object(
                {
                    let mut m = IndexMap::new();
                    m.insert(
                        "name".into(),
                        serde_json::json!({
                            "type": "string",
                            "description": "Exact tool name (from tool_search results)"
                        }),
                    );
                    m
                },
                vec!["name".into()],
            ),
        ),
        ToolSchema::new(
            "tool_call",
            "Invoke a deferred tool by name with arguments. Argument shape \
             matches the tool's schema (see tool_describe). All hooks, approvals, \
             and guardrails run exactly as for directly-listed tools.",
            JsonSchema::object(
                {
                    let mut m = IndexMap::new();
                    m.insert(
                        "name".into(),
                        serde_json::json!({
                            "type": "string",
                            "description": "Exact tool name to invoke"
                        }),
                    );
                    m.insert(
                        "arguments".into(),
                        serde_json::json!({
                            "type": "object",
                            "description": "Arguments matching the tool's schema"
                        }),
                    );
                    m
                },
                vec!["name".into(), "arguments".into()],
            ),
        ),
    ]
}

// ── ToolSearchHandler ───────────────────────────────────────────────

pub struct ToolSearchHandler {
    registry: Arc<ToolRegistry>,
}

impl ToolSearchHandler {
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait::async_trait]
impl ToolHandler for ToolSearchHandler {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "tool_search",
            "Search additional tools loaded on demand.",
            JsonSchema::object(
                {
                    let mut m = IndexMap::new();
                    m.insert(
                        "query".into(),
                        serde_json::json!({
                            "type": "string",
                            "description": "Keywords to search for"
                        }),
                    );
                    m.insert(
                        "limit".into(),
                        serde_json::json!({
                            "type": "integer",
                            "description": "Max results. Default 5."
                        }),
                    );
                    m
                },
                vec!["query".into()],
            ),
        )
    }

    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if query.is_empty() {
            return Err(ToolError::InvalidParams(
                "query is required".into(),
            ));
        }
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;

        let all_defs = self.registry.get_definitions();
        let (_, deferred) = classify_tools(&all_defs);

        let query_lower = query.to_lowercase();
        let query_tokens: Vec<&str> = query_lower.split_whitespace().collect();

        let mut scored: Vec<(&ToolSchema, f32)> = deferred
            .iter()
            .map(|s| {
                let name_lower = s.name.to_lowercase();
                let desc_lower = s.description.to_lowercase();
                let combined = format!("{name_lower} {desc_lower}");
                let score = query_tokens
                    .iter()
                    .map(|t| {
                        if combined.contains(t) {
                            1.0f32
                        } else {
                            0.0
                        }
                    })
                    .sum::<f32>()
                    / (query_tokens.len().max(1) as f32);
                (*s, score)
            })
            .filter(|(_, score)| *score > 0.0)
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let hits: Vec<Value> = scored
            .into_iter()
            .take(limit)
            .map(|(s, score)| {
                let short_desc = if s.description.len() > 200 {
                    format!("{}...", &s.description[..197])
                } else {
                    s.description.clone()
                };
                serde_json::json!({
                    "name": s.name,
                    "description": short_desc,
                    "relevance": score,
                })
            })
            .collect();

        Ok(serde_json::json!({
            "query": query,
            "total_deferred": deferred.len(),
            "matches": hits,
        })
        .to_string())
    }
}

// ── ToolDescribeHandler ─────────────────────────────────────────────

pub struct ToolDescribeHandler {
    registry: Arc<ToolRegistry>,
}

impl ToolDescribeHandler {
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait::async_trait]
impl ToolHandler for ToolDescribeHandler {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "tool_describe",
            "Load the full JSON schema for a tool returned by tool_search.",
            JsonSchema::object(
                {
                    let mut m = IndexMap::new();
                    m.insert(
                        "name".into(),
                        serde_json::json!({
                            "type": "string",
                            "description": "Exact tool name (from tool_search results)"
                        }),
                    );
                    m
                },
                vec!["name".into()],
            ),
        )
    }

    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if name.is_empty() {
            return Err(ToolError::InvalidParams("name is required".into()));
        }
        if is_core(name) {
            return Err(ToolError::InvalidParams(format!(
                "'{name}' is a core tool — call it directly, no need for tool_describe."
            )));
        }
        let all_defs = self.registry.get_definitions();
        for s in &all_defs {
            if s.name == name {
                return Ok(serde_json::json!({
                    "name": s.name,
                    "description": s.description,
                    "parameters": s.parameters,
                })
                .to_string());
            }
        }
        Err(ToolError::ExecutionFailed(format!(
            "'{name}' not found. Use tool_search to find available tools."
        )))
    }
}

// ── ToolCallHandler ─────────────────────────────────────────────────

pub struct ToolCallHandler {
    registry: Arc<ToolRegistry>,
}

impl ToolCallHandler {
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait::async_trait]
impl ToolHandler for ToolCallHandler {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "tool_call",
            "Invoke a deferred tool by name with arguments.",
            JsonSchema::object(
                {
                    let mut m = IndexMap::new();
                    m.insert(
                        "name".into(),
                        serde_json::json!({
                            "type": "string",
                            "description": "Exact tool name to invoke"
                        }),
                    );
                    m.insert(
                        "arguments".into(),
                        serde_json::json!({
                            "type": "object",
                            "description": "Arguments matching the tool's schema"
                        }),
                    );
                    m
                },
                vec!["name".into(), "arguments".into()],
            ),
        )
    }

    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if name.is_empty() {
            return Err(ToolError::InvalidParams("name is required".into()));
        }

        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::json!({}));

        if is_core(name) {
            return Err(ToolError::InvalidParams(format!(
                "'{name}' is a core tool — call it directly."
            )));
        }

        // Verify the tool exists and is deferrable
        let all_defs = self.registry.get_definitions();
        let found = all_defs.iter().find(|s| s.name == name);
        if found.is_none() {
            return Err(ToolError::ExecutionFailed(format!(
                "'{name}' not found. Use tool_search to find available tools."
            )));
        }

        // Dispatch to the real tool via registry
        let result = self.registry.dispatch_async(name, arguments).await;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_separates_core_from_deferred() {
        let schemas = vec![
            ToolSchema::new("read_file", "Read", JsonSchema::new("object")),
            ToolSchema::new("some_plugin_tool", "Plugin", JsonSchema::new("object")),
            ToolSchema::new("tool_search", "Search", JsonSchema::new("object")),
        ];
        let (visible, deferred) = classify_tools(&schemas);
        assert_eq!(visible.len(), 2);
        assert_eq!(deferred.len(), 1);
        assert_eq!(deferred[0].name, "some_plugin_tool");
    }

    #[test]
    fn assembly_passthrough_when_few_deferred() {
        let schemas = vec![
            ToolSchema::new("read_file", "Read", JsonSchema::new("object")),
            ToolSchema::new("plugin_a", "A", JsonSchema::new("object")),
            ToolSchema::new("plugin_b", "B", JsonSchema::new("object")),
        ];
        let result = assemble_tool_schemas(schemas);
        assert!(!result.activated);
        assert_eq!(result.deferred_count, 0);
    }

    #[test]
    fn bridge_schemas_have_correct_names() {
        let schemas = bridge_tool_schemas(30);
        assert_eq!(schemas.len(), 3);
        assert_eq!(schemas[0].name, "tool_search");
        assert_eq!(schemas[1].name, "tool_describe");
        assert_eq!(schemas[2].name, "tool_call");
    }

    #[test]
    fn assembly_activates_when_threshold_exceeded() {
        // 26 deferred + 3 core = 29 tools, threshold is 25
        let mut schemas: Vec<ToolSchema> = vec![
            ToolSchema::new("read_file", "Read file", JsonSchema::new("object")),
            ToolSchema::new("terminal", "Run terminal", JsonSchema::new("object")),
            ToolSchema::new("memory", "Memory", JsonSchema::new("object")),
        ];
        for i in 0..26 {
            schemas.push(ToolSchema::new(
                format!("plugin_tool_{i}"),
                format!("Plugin tool {i} description"),
                JsonSchema::new("object"),
            ));
        }
        let result = assemble_tool_schemas(schemas);
        assert!(result.activated);
        assert_eq!(result.deferred_count, 26);
        // 3 core + 3 bridge = 6
        assert_eq!(result.tool_schemas.len(), 6);
        let names: Vec<&str> = result.tool_schemas.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"tool_search"));
        assert!(names.contains(&"tool_describe"));
        assert!(names.contains(&"tool_call"));
        // deferred tools should NOT be in the result
        assert!(!names.contains(&"plugin_tool_0"));
    }

    #[test]
    fn assembly_preserves_existing_bridges_idempotent() {
        // If bridge tools already in the list, strip them first
        let mut schemas = vec![
            ToolSchema::new("read_file", "Read", JsonSchema::new("object")),
            ToolSchema::new("tool_search", "Bridge", JsonSchema::new("object")),
        ];
        for i in 0..30 {
            schemas.push(ToolSchema::new(
                format!("deferred_{i}"),
                format!("Deferred {i}"),
                JsonSchema::new("object"),
            ));
        }
        let result = assemble_tool_schemas(schemas);
        assert!(result.activated);
        let names: Vec<&str> = result.tool_schemas.iter().map(|s| s.name.as_str()).collect();
        // Only 3 bridge tools, not 4 (duplicate tool_search stripped)
        let bridge_count = names.iter().filter(|n| BRIDGE_TOOL_NAMES.contains(n)).count();
        assert_eq!(bridge_count, 3);
    }

    #[tokio::test]
    async fn tool_search_returns_matching_results() {
        let registry = Arc::new(ToolRegistry::new());
        // Register some tools
        let dummy_handler: Arc<dyn hermix_core::ToolHandler> =
            Arc::new(DummyHandler);
        registry.register(
            "github_create_issue",
            "github",
            ToolSchema::new(
                "github_create_issue",
                "Create a GitHub issue in a repository",
                JsonSchema::new("object"),
            ),
            dummy_handler.clone(),
            Arc::new(|| true),
            vec![],
            false,
            "Create GitHub issue",
            "🐙",
            None,
        );
        registry.register(
            "github_list_prs",
            "github",
            ToolSchema::new(
                "github_list_prs",
                "List pull requests in a repository",
                JsonSchema::new("object"),
            ),
            dummy_handler.clone(),
            Arc::new(|| true),
            vec![],
            false,
            "List PRs",
            "📋",
            None,
        );
        registry.register(
            "read_file",
            "file",
            ToolSchema::new("read_file", "Read a file", JsonSchema::new("object")),
            dummy_handler.clone(),
            Arc::new(|| true),
            vec![],
            false,
            "Read file",
            "📖",
            None,
        );

        let handler = ToolSearchHandler::new(registry);
        let result = handler
            .execute(serde_json::json!({"query": "github issue", "limit": 5}))
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["total_deferred"], 2);
        let matches = parsed["matches"].as_array().unwrap();
        assert!(!matches.is_empty());
        assert_eq!(matches[0]["name"], "github_create_issue");
    }

    #[tokio::test]
    async fn tool_search_empty_query_errors() {
        let registry = Arc::new(ToolRegistry::new());
        let handler = ToolSearchHandler::new(registry);
        let err = handler
            .execute(serde_json::json!({"query": ""}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn tool_describe_returns_full_schema() {
        let registry = Arc::new(ToolRegistry::new());
        let dummy_handler: Arc<dyn hermix_core::ToolHandler> =
            Arc::new(DummyHandler);
        registry.register(
            "some_deferred_tool",
            "plugin",
            ToolSchema::new(
                "some_deferred_tool",
                "A deferred plugin tool with parameters",
                JsonSchema::object(
                    {
                        let mut m = IndexMap::new();
                        m.insert(
                            "input".into(),
                            serde_json::json!({"type": "string"}),
                        );
                        m
                    },
                    vec!["input".into()],
                ),
            ),
            dummy_handler,
            Arc::new(|| true),
            vec![],
            false,
            "Deferred tool",
            "🔧",
            None,
        );

        let handler = ToolDescribeHandler::new(registry);
        let result = handler
            .execute(serde_json::json!({"name": "some_deferred_tool"}))
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["name"], "some_deferred_tool");
        assert_eq!(parsed["description"], "A deferred plugin tool with parameters");
        assert!(parsed["parameters"]["properties"]["input"].is_object());
    }

    #[tokio::test]
    async fn tool_describe_rejects_core_tool() {
        let registry = Arc::new(ToolRegistry::new());
        let handler = ToolDescribeHandler::new(registry);
        let err = handler
            .execute(serde_json::json!({"name": "read_file"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn tool_describe_unknown_tool_errors() {
        let registry = Arc::new(ToolRegistry::new());
        let handler = ToolDescribeHandler::new(registry);
        let err = handler
            .execute(serde_json::json!({"name": "nonexistent_tool"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed(_)));
    }

    #[tokio::test]
    async fn tool_call_dispatches_to_real_tool() {
        let registry = Arc::new(ToolRegistry::new());
        let handler: Arc<dyn hermix_core::ToolHandler> =
            Arc::new(DummyHandler);
        registry.register(
            "echo_tool",
            "test",
            ToolSchema::new("echo_tool", "Echoes input", JsonSchema::new("object")),
            handler,
            Arc::new(|| true),
            vec![],
            false,
            "Echo tool",
            "🔊",
            None,
        );

        let call_handler = ToolCallHandler::new(registry);
        let result = call_handler
            .execute(serde_json::json!({"name": "echo_tool", "arguments": {"msg": "hello"}}))
            .await
            .unwrap();
        // dispatch_async wraps in {"result": ...}
        assert!(result.contains("echo ok"));
    }

    #[tokio::test]
    async fn tool_call_rejects_core_tool() {
        let registry = Arc::new(ToolRegistry::new());
        let handler = ToolCallHandler::new(registry);
        let err = handler
            .execute(serde_json::json!({"name": "terminal", "arguments": {}}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn tool_call_unknown_tool_errors() {
        let registry = Arc::new(ToolRegistry::new());
        let handler = ToolCallHandler::new(registry);
        let err = handler
            .execute(serde_json::json!({"name": "ghost_tool", "arguments": {}}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed(_)));
    }

    #[test]
    fn is_bridge_identifies_bridge_tools() {
        assert!(is_bridge("tool_search"));
        assert!(is_bridge("tool_describe"));
        assert!(is_bridge("tool_call"));
        assert!(!is_bridge("read_file"));
        assert!(!is_bridge("browser_navigate"));
    }

    #[test]
    fn classify_preserves_bridge_in_visible() {
        let schemas = vec![
            ToolSchema::new("read_file", "Read", JsonSchema::new("object")),
            ToolSchema::new("tool_search", "Search", JsonSchema::new("object")),
            ToolSchema::new("plugin_x", "X", JsonSchema::new("object")),
        ];
        let (visible, deferred) = classify_tools(&schemas);
        assert_eq!(visible.len(), 2);
        assert_eq!(deferred.len(), 1);
    }

    // ── P6-1: classify_tools_with_meta tests ──────────────────────

    #[test]
    fn classify_with_meta_uses_registry() {
        let meta = ToolMetaRegistry::new();
        meta.register(crate::tool_meta::ToolMeta::resident("read_file", "Read", "file"));
        meta.register(crate::tool_meta::ToolMeta::deferred("image_gen", "Generate", "ai"));

        let schemas = vec![
            ToolSchema::new("read_file", "Read", JsonSchema::new("object")),
            ToolSchema::new("image_gen", "Generate", JsonSchema::new("object")),
            ToolSchema::new("tool_search", "Search", JsonSchema::new("object")),
            ToolSchema::new("unknown_plugin", "Unknown", JsonSchema::new("object")),
        ];

        let (visible, deferred) = classify_tools_with_meta(&schemas, &meta);
        assert_eq!(visible.len(), 2); // read_file + tool_search
        assert_eq!(deferred.len(), 2); // image_gen + unknown_plugin
        let vis_names: Vec<&str> = visible.iter().map(|s| s.name.as_str()).collect();
        assert!(vis_names.contains(&"read_file"));
        assert!(vis_names.contains(&"tool_search"));
        let def_names: Vec<&str> = deferred.iter().map(|s| s.name.as_str()).collect();
        assert!(def_names.contains(&"image_gen"));
        assert!(def_names.contains(&"unknown_plugin"));
    }

    #[test]
    fn classify_with_meta_falls_back_when_empty() {
        // Empty registry → behaves like classify_tools (uses CORE_TOOLS)
        let meta = ToolMetaRegistry::new();
        let schemas = vec![
            ToolSchema::new("read_file", "Read", JsonSchema::new("object")),
            ToolSchema::new("some_plugin", "Plugin", JsonSchema::new("object")),
        ];
        let (visible, deferred) = classify_tools_with_meta(&schemas, &meta);
        assert_eq!(visible.len(), 1);
        assert_eq!(deferred.len(), 1);
    }

    #[test]
    fn classify_with_meta_can_override_core_tool_to_deferred() {
        // The metadata registry can mark a core tool as deferred
        let meta = ToolMetaRegistry::new();
        meta.mark_deferred("terminal");

        let schemas = vec![
            ToolSchema::new("terminal", "Terminal", JsonSchema::new("object")),
            ToolSchema::new("read_file", "Read", JsonSchema::new("object")),
        ];

        let (visible, deferred) = classify_tools_with_meta(&schemas, &meta);
        // terminal should be deferred, read_file should be visible
        let def_names: Vec<&str> = deferred.iter().map(|s| s.name.as_str()).collect();
        assert!(def_names.contains(&"terminal"));
        let vis_names: Vec<&str> = visible.iter().map(|s| s.name.as_str()).collect();
        assert!(vis_names.contains(&"read_file"));
    }
}

// Dummy handler for tests
struct DummyHandler;

#[async_trait::async_trait]
impl hermix_core::ToolHandler for DummyHandler {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new("dummy", "Dummy", JsonSchema::new("object"))
    }
    async fn execute(&self, _params: Value) -> Result<String, ToolError> {
        Ok("echo ok".into())
    }
}
