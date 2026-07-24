//! Deferred tool loader — coordinates the search → describe → call flow.
//!
//! (Phase 6 — P6-2)
//!
//! The orchestration core uses this module to decide which tool schemas
//! are included directly in the system prompt and which are deferred
//! behind the `tool_search` / `tool_describe` / `tool_call` bridge.
//!
//! ## Flow
//!
//! 1. **Assembly** — `assemble_tool_schemas_with_meta()` takes all
//!    registered tool schemas + a `ToolMetaRegistry` and produces an
//!    `AssemblyResult`. If deferred tools exceed the threshold, the
//!    result contains only resident tools + 3 bridge tool schemas.
//!    Otherwise all tools pass through unchanged.
//!
//! 2. **Search** — the LLM calls `tool_search` with a keyword query.
//!    The `ToolSearchHandler` (already in `tool_search.rs`) searches
//!    the deferred tool set and returns matching names + descriptions.
//!
//! 3. **Describe** — the LLM calls `tool_describe` with a tool name.
//!    The `ToolDescribeHandler` returns the full JSON schema.
//!
//! 4. **Call** — the LLM calls `tool_call` with the tool name + args.
//!    The `ToolCallHandler` dispatches to the real tool handler.
//!
//! This module provides the assembly entry point (`assemble_with_meta`)
//! and a `DeferredToolLoader` struct that can be injected into the
//! orchestration core to centralize the metadata-aware assembly logic.

use hermix_core::ToolSchema;

use crate::tool_meta::ToolMetaRegistry;
use crate::tools::tool_search::{
    bridge_tool_schemas, is_bridge, AssemblyResult,
    DEFERRED_THRESHOLD,
};

// ---------------------------------------------------------------------------
// Metadata-aware assembly
// ---------------------------------------------------------------------------

/// Assemble tool schemas using a `ToolMetaRegistry` to classify tools
/// as resident or deferred.
/// /// P6-2: This is the metadata-driven replacement for
/// [`assemble_tool_schemas`]. When the metadata registry is non-empty,
/// tools registered as `Deferred` are placed behind the bridge tools.
/// When the registry is empty, the function falls back to the
/// hard-coded `CORE_TOOLS` list (backwards compatibility).
/// /// Returns an [`AssemblyResult`] with:
/// - `tool_schemas` — the schemas to include in the system prompt
/// - `activated` — whether the bridge was activated
/// - `deferred_count` — number of deferred tools
pub fn assemble_tool_schemas_with_meta(
    all: Vec<ToolSchema>,
    meta: &ToolMetaRegistry,
) -> AssemblyResult {
    // Strip any existing bridge tools (idempotent re-entry)
    let incoming: Vec<ToolSchema> = all
        .into_iter()
        .filter(|s| !is_bridge(&s.name))
        .collect();

    // Classify using metadata registry
    let (visible, deferred) = if meta.len() > 0 {
        let mut vis = Vec::new();
        let mut def = Vec::new();
        for s in &incoming {
            if meta.is_deferred(&s.name) {
                def.push(s.clone());
            } else {
                vis.push(s.clone());
            }
        }
        (vis, def)
    } else {
        // Fall back to hard-coded classification
        let (vis_refs, def_refs) = crate::tools::tool_search::classify_tools(&incoming);
        let vis: Vec<ToolSchema> = vis_refs.into_iter().cloned().collect();
        let def: Vec<ToolSchema> = def_refs.into_iter().cloned().collect();
        (vis, def)
    };

    if deferred.len() < DEFERRED_THRESHOLD {
        return AssemblyResult {
            tool_schemas: incoming,
            activated: false,
            deferred_count: 0,
        };
    }

    let count = deferred.len();
    let mut result: Vec<ToolSchema> = visible;
    result.extend(bridge_tool_schemas(count));
    AssemblyResult {
        tool_schemas: result,
        activated: true,
        deferred_count: count,
    }
}

// ---------------------------------------------------------------------------
// DeferredToolLoader
// ---------------------------------------------------------------------------

/// Coordinates the deferred tool loading flow.
/// /// Holds a reference to the `ToolRegistry` (for dispatch) and the
/// `ToolMetaRegistry` (for classification). The orchestration core
/// calls `assemble()` at the start of each turn to get the tool
/// schemas to include in the system prompt.
/// /// The LLM then discovers deferred tools via `tool_search` and invokes
/// them via `tool_call` — the handlers for those bridge tools are
/// already registered in `tool_search.rs`.
#[derive(Clone)]
pub struct DeferredToolLoader {
    meta_registry: ToolMetaRegistry,
}

impl DeferredToolLoader {
    /// Create a new loader with an empty metadata registry.
    /// Tools will be classified using the hard-coded `CORE_TOOLS` list.
    pub fn new() -> Self {
        Self {
            meta_registry: ToolMetaRegistry::new(),
        }
    }

    /// Create a new loader with the given metadata registry.
    pub fn with_meta(meta_registry: ToolMetaRegistry) -> Self {
        Self { meta_registry }
    }

    /// Get a reference to the metadata registry.
    pub fn meta(&self) -> &ToolMetaRegistry {
        &self.meta_registry
    }

    /// Assemble tool schemas for the system prompt.
    /// /// This is the main entry point: call it with all registered tool
    /// schemas and get back the subset to include in the LLM's tool list.
    pub fn assemble(&self, all_schemas: Vec<ToolSchema>) -> AssemblyResult {
        assemble_tool_schemas_with_meta(all_schemas, &self.meta_registry)
    }

    /// Mark a tool as deferred (hidden behind the bridge).
    pub fn mark_deferred(&self, name: &str) {
        self.meta_registry.mark_deferred(name);
    }

    /// Mark a tool as resident (always visible).
    pub fn mark_resident(&self, name: &str) {
        self.meta_registry.mark_resident(name);
    }

    /// Register metadata for a tool.
    pub fn register_meta(&self, meta: crate::tool_meta::ToolMeta) {
        self.meta_registry.register(meta);
    }

    /// Check if a tool is deferred.
    pub fn is_deferred(&self, name: &str) -> bool {
        self.meta_registry.is_deferred(name)
    }

    /// Check if a tool is resident.
    pub fn is_resident(&self, name: &str) -> bool {
        self.meta_registry.is_resident(name)
    }
}

impl Default for DeferredToolLoader {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_core::JsonSchema;

    fn dummy_schema(name: &str) -> ToolSchema {
        ToolSchema::new(name, "test", JsonSchema::new("object"))
    }

    #[test]
    fn test_assemble_with_meta_passthrough_when_few_deferred() {
        let meta = ToolMetaRegistry::new();
        meta.register(crate::tool_meta::ToolMeta::deferred("plugin_a", "A", "test"));
        meta.register(crate::tool_meta::ToolMeta::deferred("plugin_b", "B", "test"));

        let schemas = vec![
            dummy_schema("read_file"),
            dummy_schema("plugin_a"),
            dummy_schema("plugin_b"),
        ];

        let result = assemble_tool_schemas_with_meta(schemas, &meta);
        assert!(!result.activated);
        assert_eq!(result.deferred_count, 0);
        // All 3 tools pass through (below threshold)
        assert_eq!(result.tool_schemas.len(), 3);
    }

    #[test]
    fn test_assemble_with_meta_activates_when_threshold_exceeded() {
        let meta = ToolMetaRegistry::new();
        // Mark 26 tools as deferred (threshold is 25)
        for i in 0..26 {
            meta.register(crate::tool_meta::ToolMeta::deferred(
                format!("plugin_{i}"),
                format!("Plugin {i}"),
                "test",
            ));
        }
        meta.register(crate::tool_meta::ToolMeta::resident("read_file", "Read", "file"));

        let mut schemas = vec![dummy_schema("read_file")];
        for i in 0..26 {
            schemas.push(dummy_schema(&format!("plugin_{i}")));
        }

        let result = assemble_tool_schemas_with_meta(schemas, &meta);
        assert!(result.activated);
        assert_eq!(result.deferred_count, 26);
        // 1 resident + 3 bridge = 4
        assert_eq!(result.tool_schemas.len(), 4);

        let names: Vec<&str> = result.tool_schemas.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"tool_search"));
        assert!(names.contains(&"tool_describe"));
        assert!(names.contains(&"tool_call"));
    }

    #[test]
    fn test_assemble_with_meta_falls_back_when_empty() {
        let meta = ToolMetaRegistry::new();
        let schemas = vec![
            dummy_schema("read_file"),
            dummy_schema("some_plugin"),
        ];

        let result = assemble_tool_schemas_with_meta(schemas, &meta);
        // Empty registry → falls back to CORE_TOOLS classification
        assert!(!result.activated);
        assert_eq!(result.tool_schemas.len(), 2);
    }

    #[test]
    fn test_deferred_tool_loader_assemble() {
        let loader = DeferredToolLoader::new();
        loader.mark_deferred("heavy_tool_1");
        loader.mark_deferred("heavy_tool_2");

        let schemas = vec![
            dummy_schema("read_file"),
            dummy_schema("heavy_tool_1"),
            dummy_schema("heavy_tool_2"),
        ];

        let result = loader.assemble(schemas);
        // Below threshold, so passthrough
        assert!(!result.activated);
        assert_eq!(result.tool_schemas.len(), 3);
    }

    #[test]
    fn test_deferred_tool_loader_with_meta_registry() {
        let meta = ToolMetaRegistry::new();
        for i in 0..30 {
            meta.register(crate::tool_meta::ToolMeta::deferred(
                format!("heavy_{i}"),
                format!("Heavy {i}"),
                "ai",
            ));
        }
        meta.register(crate::tool_meta::ToolMeta::resident("read_file", "Read", "file"));

        let loader = DeferredToolLoader::with_meta(meta);

        let mut schemas = vec![dummy_schema("read_file")];
        for i in 0..30 {
            schemas.push(dummy_schema(&format!("heavy_{i}")));
        }

        let result = loader.assemble(schemas);
        assert!(result.activated);
        assert_eq!(result.deferred_count, 30);
        // 1 resident + 3 bridge = 4
        assert_eq!(result.tool_schemas.len(), 4);
    }

    #[test]
    fn test_deferred_tool_loader_mark_and_check() {
        let loader = DeferredToolLoader::new();
        loader.mark_deferred("image_gen");
        loader.mark_resident("read_file");

        assert!(loader.is_deferred("image_gen"));
        assert!(loader.is_resident("read_file"));
        assert!(!loader.is_deferred("read_file"));
        assert!(!loader.is_resident("image_gen"));
    }

    #[test]
    fn test_deferred_tool_loader_clone_shares_state() {
        let loader = DeferredToolLoader::new();
        let clone = loader.clone();
        clone.mark_deferred("shared_tool");
        // Both share the same metadata registry
        assert!(loader.is_deferred("shared_tool"));
    }
}
