//! Tool metadata registry — `ToolMeta` + `LoadingStrategy`.
//!
//! (Phase 6 — P6-1)
//!
//! The metadata registry sits *alongside* the existing [`ToolRegistry`].
//! It records per-tool loading strategy metadata so the orchestration core
//! can decide which tools are always visible to the LLM (resident) and
//! which are deferred behind the `tool_search` / `tool_describe` /
//! `tool_call` bridge.
//!
//! ## Design
//!
//! The existing `tool_search.rs` module used a hard-coded `CORE_TOOLS`
//! constant list to classify tools. P6-1 replaces that with a proper
//! metadata layer:
//!
//! - **`LoadingStrategy::Resident`** — the tool schema is always included
//!   in the system prompt sent to the LLM. These are lightweight,
//!   frequently-used tools (read_file, terminal, memory, etc.).
//! - **`LoadingStrategy::Deferred`** — the tool schema is *not* included
//!   in the system prompt. The LLM discovers it via `tool_search` and
//!   invokes it via `tool_call`. These are heavier or less-frequently
//!   used tools (image_gen, video_gen, homeassistant, etc.).
//!
//! The registry is a thin wrapper around a `HashMap<String, ToolMeta>`,
//! protected by an `RwLock`. It does not hold handler references — the
//! `ToolRegistry` already owns those. The metadata registry is purely
//! advisory: the `ToolRegistry`'s `get_definitions()` still returns all
//! enabled tools; the metadata registry tells the caller *which subset*
//! to expose directly vs. defer.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use hermix_core::ToolSchema;

// ---------------------------------------------------------------------------
// LoadingStrategy
// ---------------------------------------------------------------------------

/// Loading strategy for a tool.
/// /// Determines whether the tool's schema is included directly in the
/// system prompt (`Resident`) or discovered on demand via the bridge
/// tools (`Deferred`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LoadingStrategy {
    /// Always visible to the LLM. The tool schema is included in
    /// `get_definitions()` output directly. Use for lightweight,
    /// frequently-used tools.
    #[default]
    Resident,
    /// Not included in the system prompt. The LLM discovers the tool
    /// via `tool_search` → `tool_describe` → `tool_call`. Use for
    /// heavier tools that would bloat the context if always present.
    Deferred,
}

impl LoadingStrategy {
    pub fn is_resident(&self) -> bool {
        matches!(self, Self::Resident)
    }

    pub fn is_deferred(&self) -> bool {
        matches!(self, Self::Deferred)
    }
}

// ---------------------------------------------------------------------------
// ToolMeta
// ---------------------------------------------------------------------------

/// Metadata for a single tool.
/// /// Stored in [`ToolMetaRegistry`] alongside the [`ToolRegistry`].
/// The `loading_strategy` field drives the deferred-loading mechanism.
#[derive(Debug, Clone)]
pub struct ToolMeta {
    /// Tool name (matches `ToolSchema.name`).
    pub name: String,
    /// One-line description.
    pub description: String,
    /// Toolset tag (e.g. "file", "web", "browser").
    pub toolset: String,
    /// Loading strategy for this tool.
    pub loading_strategy: LoadingStrategy,
    /// Optional category for grouping (e.g. "io", "search", "ai").
    pub category: Option<String>,
}

impl ToolMeta {
    /// Create a new resident tool metadata.
    pub fn resident(name: impl Into<String>, description: impl Into<String>, toolset: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            toolset: toolset.into(),
            loading_strategy: LoadingStrategy::Resident,
            category: None,
        }
    }

    /// Create a new deferred tool metadata.
    pub fn deferred(name: impl Into<String>, description: impl Into<String>, toolset: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            toolset: toolset.into(),
            loading_strategy: LoadingStrategy::Deferred,
            category: None,
        }
    }

    /// Set the category.
    pub fn with_category(mut self, category: impl Into<String>) -> Self {
        self.category = Some(category.into());
        self
    }
}

// ---------------------------------------------------------------------------
// ToolMetaRegistry
// ---------------------------------------------------------------------------

/// Registry of tool metadata, keyed by tool name.
/// /// Thread-safe (`RwLock<HashMap>`). Cheap to clone — the inner map is
/// behind an `Arc`.
#[derive(Clone)]
pub struct ToolMetaRegistry {
    inner: Arc<RwLock<HashMap<String, ToolMeta>>>,
}

impl ToolMetaRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register metadata for a tool. If metadata for the same name
    /// already exists, it is overwritten.
    pub fn register(&self, meta: ToolMeta) {
        let mut inner = self.inner.write().expect("tool_meta rwlock not poisoned");
        inner.insert(meta.name.clone(), meta);
    }

    /// Register multiple tool metadatas at once.
    pub fn register_all(&self, metas: Vec<ToolMeta>) {
        let mut inner = self.inner.write().expect("tool_meta rwlock not poisoned");
        for meta in metas {
            inner.insert(meta.name.clone(), meta);
        }
    }

    /// Set the loading strategy for a tool. If the tool isn't registered,
    /// a minimal entry is created with the given name.
    pub fn set_loading_strategy(&self, name: &str, strategy: LoadingStrategy) {
        let mut inner = self.inner.write().expect("tool_meta rwlock not poisoned");
        inner
            .entry(name.to_string())
            .and_modify(|m| m.loading_strategy = strategy)
            .or_insert(ToolMeta {
                name: name.to_string(),
                description: String::new(),
                toolset: String::new(),
                loading_strategy: strategy,
                category: None,
            });
    }

    /// Mark a tool as resident (always visible).
    pub fn mark_resident(&self, name: &str) {
        self.set_loading_strategy(name, LoadingStrategy::Resident);
    }

    /// Mark a tool as deferred (search-then-call).
    pub fn mark_deferred(&self, name: &str) {
        self.set_loading_strategy(name, LoadingStrategy::Deferred);
    }

    /// Get the metadata for a tool.
    pub fn get(&self, name: &str) -> Option<ToolMeta> {
        let inner = self.inner.read().expect("tool_meta rwlock not poisoned");
        inner.get(name).cloned()
    }

    /// Get the loading strategy for a tool. Returns `Resident` if the
    /// tool is not registered (safe default — unknown tools are treated
    /// as resident so they don't get hidden behind the bridge).
    pub fn loading_strategy(&self, name: &str) -> LoadingStrategy {
        let inner = self.inner.read().expect("tool_meta rwlock not poisoned");
        inner
            .get(name)
            .map(|m| m.loading_strategy)
            .unwrap_or(LoadingStrategy::Resident)
    }

    /// Check if a tool is resident.
    pub fn is_resident(&self, name: &str) -> bool {
        self.loading_strategy(name).is_resident()
    }

    /// Check if a tool is deferred.
    pub fn is_deferred(&self, name: &str) -> bool {
        self.loading_strategy(name).is_deferred()
    }

    /// Get all resident tool names.
    pub fn resident_names(&self) -> HashSet<String> {
        let inner = self.inner.read().expect("tool_meta rwlock not poisoned");
        inner
            .values()
            .filter(|m| m.loading_strategy.is_resident())
            .map(|m| m.name.clone())
            .collect()
    }

    /// Get all deferred tool names.
    pub fn deferred_names(&self) -> HashSet<String> {
        let inner = self.inner.read().expect("tool_meta rwlock not poisoned");
        inner
            .values()
            .filter(|m| m.loading_strategy.is_deferred())
            .map(|m| m.name.clone())
            .collect()
    }

    /// Partition a list of tool schemas into (resident, deferred) based
    /// on the registered metadata. Tools not in the registry default
    /// to `Resident`.
    /// /// This replaces the old `classify_tools` function in `tool_search.rs`
    /// which used a hard-coded `CORE_TOOLS` list.
    pub fn partition(&self, schemas: &[ToolSchema]) -> (Vec<ToolSchema>, Vec<ToolSchema>) {
        let mut resident = Vec::new();
        let mut deferred = Vec::new();
        for s in schemas {
            if self.is_deferred(&s.name) {
                deferred.push(s.clone());
            } else {
                resident.push(s.clone());
            }
        }
        (resident, deferred)
    }

    /// Get all registered metadata entries.
    pub fn list(&self) -> Vec<ToolMeta> {
        let inner = self.inner.read().expect("tool_meta rwlock not poisoned");
        inner.values().cloned().collect()
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        let inner = self.inner.read().expect("tool_meta rwlock not poisoned");
        inner.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Build a default metadata registry from a list of tool schemas.
    /// /// Tools whose names appear in the `DEFAULT_RESIDENT_TOOLS` list are
    /// marked as `Resident`; all others are marked as `Deferred`.
    /// This is the bridge between the old hard-coded list and the new
    /// metadata-driven approach.
    pub fn from_schemas_with_defaults(schemas: &[ToolSchema]) -> Self {
        let registry = Self::new();
        for s in schemas {
            let strategy = if DEFAULT_RESIDENT_TOOLS.contains(&s.name.as_str()) {
                LoadingStrategy::Resident
            } else {
                LoadingStrategy::Deferred
            };
            registry.register(ToolMeta {
                name: s.name.clone(),
                description: s.description.clone(),
                toolset: String::new(), // not available from schema alone
                loading_strategy: strategy,
                category: None,
            });
        }
        registry
    }
}

impl Default for ToolMetaRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Default resident tools
// ---------------------------------------------------------------------------

/// Tools that are resident by default. This list mirrors the old
/// `CORE_TOOLS` constant in `tool_search.rs` and serves as the seed
/// for `from_schemas_with_defaults`.
/// /// Tools not in this list are deferred by default — they're discoverable
/// via `tool_search` and invokable via `tool_call`.
pub const DEFAULT_RESIDENT_TOOLS: &[&str] = &[
    // File I/O
    "read_file",
    "write_file",
    "patch_file",
    "search_files",
    "list_files",
    "edit_file",
    // Binary / multimodal file readers — MUST be resident so the LLM
    // can see them without going through tool_search bridge. Without
    // this, the KeywordRouter + AlwaysInclude pipeline correctly
    // forwards them, but assemble_tool_schemas's classify_tools
    // buckets them as "deferred" (not in DEFAULT_RESIDENT_TOOLS),
    // hiding them behind tool_search/tool_call bridges.
    "read_pdf",
    "read_image",
    // File operations — move/rename must be resident so the LLM
    // can rename files without resorting to terminal mv (quoting issues).
    "move_file",
    // Sub-agent delegation — MUST be resident so the LLM can spawn
    // sub-agents for image/binary analysis without discovering the
    // tool through tool_search first.
    "delegate_task",
    "get_subagent_result",
    // Terminal
    "terminal",
    "process",
    // Memory
    "memory",
    "memory_search",
    "memory_store",
    // Skills
    "skills_list",
    "skill_view",
    "skill_manage",
    // Clarify
    "clarify",
    // Todo / Task tracker
    "todo",
    "task_create",
    "task_update",
    "task_list",
    "task_get",
    // Session
    "session_search",
    // Browser (core set)
    "browser_navigate",
    "browser_snapshot",
    "browser_click",
    "browser_type",
    "browser_scroll",
    "browser_back",
    "browser_press",
    "browser_get_images",
    "browser_vision",
    "browser_console",
    // Bridge tools (always resident)
    "tool_search",
    "tool_describe",
    "tool_call",
    // Security
    "url_safety",
    "security_guidance",
    // Web
    "web_search",
    "web_extract",
    // Introspection
    "model_providers",
    "platforms",
    "list_tools",
];

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
    fn test_loading_strategy_defaults_to_resident() {
        let registry = ToolMetaRegistry::new();
        // Unregistered tools default to Resident (safe default)
        assert_eq!(registry.loading_strategy("unknown"), LoadingStrategy::Resident);
        assert!(registry.is_resident("unknown"));
        assert!(!registry.is_deferred("unknown"));
    }

    #[test]
    fn test_register_and_get() {
        let registry = ToolMetaRegistry::new();
        registry.register(ToolMeta::deferred("image_generate", "Generate images", "ai"));
        registry.register(ToolMeta::resident("read_file", "Read a file", "file"));

        assert_eq!(
            registry.loading_strategy("image_generate"),
            LoadingStrategy::Deferred
        );
        assert_eq!(
            registry.loading_strategy("read_file"),
            LoadingStrategy::Resident
        );

        let meta = registry.get("image_generate").unwrap();
        assert_eq!(meta.name, "image_generate");
        assert_eq!(meta.description, "Generate images");
        assert_eq!(meta.toolset, "ai");
        assert!(meta.loading_strategy.is_deferred());
    }

    #[test]
    fn test_mark_resident_and_deferred() {
        let registry = ToolMetaRegistry::new();
        registry.mark_deferred("some_heavy_tool");
        assert!(registry.is_deferred("some_heavy_tool"));

        registry.mark_resident("some_heavy_tool");
        assert!(registry.is_resident("some_heavy_tool"));
    }

    #[test]
    fn test_resident_and_deferred_names() {
        let registry = ToolMetaRegistry::new();
        registry.register(ToolMeta::resident("a", "A", "test"));
        registry.register(ToolMeta::resident("b", "B", "test"));
        registry.register(ToolMeta::deferred("c", "C", "test"));
        registry.register(ToolMeta::deferred("d", "D", "test"));

        let resident = registry.resident_names();
        assert_eq!(resident.len(), 2);
        assert!(resident.contains("a"));
        assert!(resident.contains("b"));

        let deferred = registry.deferred_names();
        assert_eq!(deferred.len(), 2);
        assert!(deferred.contains("c"));
        assert!(deferred.contains("d"));
    }

    #[test]
    fn test_partition_schemas() {
        let registry = ToolMetaRegistry::new();
        registry.register(ToolMeta::resident("read_file", "Read", "file"));
        registry.register(ToolMeta::deferred("image_generate", "Gen", "ai"));

        let schemas = vec![
            dummy_schema("read_file"),
            dummy_schema("image_generate"),
            dummy_schema("unknown_tool"), // defaults to Resident
        ];

        let (resident, deferred) = registry.partition(&schemas);
        assert_eq!(resident.len(), 2);
        assert_eq!(deferred.len(), 1);
        assert_eq!(deferred[0].name, "image_generate");
    }

    #[test]
    fn test_from_schemas_with_defaults() {
        let schemas = vec![
            dummy_schema("read_file"),
            dummy_schema("terminal"),
            dummy_schema("image_generate"),
            dummy_schema("video_generate"),
        ];

        let registry = ToolMetaRegistry::from_schemas_with_defaults(&schemas);
        assert!(registry.is_resident("read_file"));
        assert!(registry.is_resident("terminal"));
        assert!(registry.is_deferred("image_generate"));
        assert!(registry.is_deferred("video_generate"));
    }

    #[test]
    fn test_default_resident_tools_nonempty() {
        assert!(!DEFAULT_RESIDENT_TOOLS.is_empty());
        // Bridge tools must always be resident
        assert!(DEFAULT_RESIDENT_TOOLS.contains(&"tool_search"));
        assert!(DEFAULT_RESIDENT_TOOLS.contains(&"tool_describe"));
        assert!(DEFAULT_RESIDENT_TOOLS.contains(&"tool_call"));
        // Core I/O must be resident
        assert!(DEFAULT_RESIDENT_TOOLS.contains(&"read_file"));
        assert!(DEFAULT_RESIDENT_TOOLS.contains(&"terminal"));
    }

    #[test]
    fn test_with_category() {
        let meta = ToolMeta::resident("test", "Test", "test")
            .with_category("io");
        assert_eq!(meta.category.as_deref(), Some("io"));
    }

    #[test]
    fn test_register_all() {
        let registry = ToolMetaRegistry::new();
        registry.register_all(vec![
            ToolMeta::resident("a", "A", "test"),
            ToolMeta::deferred("b", "B", "test"),
        ]);
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn test_clone_shares_state() {
        let registry = ToolMetaRegistry::new();
        let clone = registry.clone();
        clone.register(ToolMeta::resident("shared", "Shared", "test"));
        // Both registries share the same inner map
        assert!(registry.get("shared").is_some());
    }
}
