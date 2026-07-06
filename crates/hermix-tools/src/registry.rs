//! Tool Registry (Requirement 4)
//!
//! Central registry for all tool definitions and handlers. Supports:
//! - Dynamic registration/deregistration with availability checks
//! - Name-conflict detection with warning on overwrite
//! - Per-tool result size limits and global default
//! - Dispatch with error catching that always returns JSON

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use hermix_core::{traits::{RiskLevel, PermissionMode}, truncate_chars, ToolHandler, ToolSchema};
use indexmap::IndexMap;
use serde_json::Value;
use tracing::{instrument, warn};

// ---------------------------------------------------------------------------
// ToolEntry
// ---------------------------------------------------------------------------

/// A registered tool with its handler, metadata, and availability check.
pub struct ToolEntry {
    /// Unique tool name (e.g. "web_search", "terminal").
    pub name: String,
    /// Which toolset this tool belongs to (e.g. "web", "terminal").
    pub toolset: String,
    /// OpenAI-format tool schema.
    pub schema: ToolSchema,
    /// Handler that executes the tool logic.
    pub handler: Arc<dyn ToolHandler>,
    /// Availability check — returns true if the tool can be used right now.
    pub check_fn: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Environment dependencies required for this tool (e.g. "EXA_API_KEY").
    pub env_deps: Vec<String>,
    /// Whether this tool performs async I/O.
    pub is_async: bool,
    /// Human-readable description.
    pub description: String,
    /// Emoji icon for display.
    pub emoji: String,
    /// Per-tool max result size override (characters).
    pub max_result_size_chars: Option<usize>,
    /// Risk classification used by the pre-execution approval hook.
    /// Defaults to [`RiskLevel::Caution`] for backwards compatibility —
    /// unknown / newly-added tools get a confirmation prompt rather than
    /// being silently allowed.
    pub risk_level: RiskLevel,
    /// Permission level for this tool (inspired by claw-code design).
    /// Determines what operations the tool is allowed to perform.
    pub permission_mode: PermissionMode,
    /// Whether this tool requires sequential execution (interactive tools
    /// like `clarify`, `delegate_task` must not run in parallel with others).
    /// Set via `ToolRegistry::register_with_risk` and consumed by
    /// `dispatch_tools` to partition calls into parallel vs sequential groups.
    pub is_interactive: bool,
    /// Runtime enable/disable flag, mutated by
    /// [`ToolRegistry::set_toolset_enabled`]. Distinct from
    /// [`Self::check_fn`]: `check_fn` reflects *capability* (env vars
    /// present, browser driver up, etc.), `is_enabled` reflects
    /// *user choice* (the operator toggled this toolset off in
    /// Settings). Both must be true for a tool to be visible to the
    /// LLM and dispatchable. Lock-free: a single `AtomicBool` per
    /// entry, mutated by the Settings UI on a separate task and
    /// read on every dispatch hot path.
    pub is_enabled: Arc<AtomicBool>,
}

/// Run an async `ToolHandler::execute` call from a synchronous context
/// (e.g. the agent_loop `JoinSet` worker thread that just pulled a
/// `dyn Fn(Value) -> Result<String, ToolError>` handler off the
/// `agent_loop::ToolRegistry`).
///
/// **Why this helper exists.** `agent_loop` stores tool handlers as
/// synchronous closures (see `agent_loop::ToolEntry::handler`); inside
/// the closure they call into `ToolRegistry::dispatch` which has an
/// async `ToolHandler` underneath. A naïve
/// `Handle::current().block_on(async { ... })` panics on multi-thread
/// runtimes because it tries to drop a freshly-created blocking pool
/// from within a tokio worker — the exact "Cannot drop a runtime in
/// a context where blocking is not allowed" panic reported by the
/// user when they typed "你好" into the chat box.
///
/// The canonical fix is `block_in_place(|| block_on(...))` (see
/// `tokio` docs, "Bridging with sync code"). `block_in_place`
/// converts the current worker into a dedicated blocking thread so
/// its inner `block_on` is free to spin up its own scheduler. On a
/// current-thread runtime `block_in_place` returns
/// `Err(BlockingOnCurrentThread)`; in that case we fall back to a
/// plain `block_on` because there is no worker pool to corrupt.
fn run_async_in_blocking(
    handler: &dyn ToolHandler,
    params: Value,
) -> Result<String, hermix_core::ToolError> {
    use tokio::runtime::{Handle, RuntimeFlavor};

    // `handler` is `&dyn ToolHandler` (borrowed) and `params` is owned.
    // We can't `await` a borrowed trait object across the future
    // boundary without a fresh borrow on each call, so the borrow
    // is scoped to the future body.
    //
    // Two paths, picked by the runtime flavor detected at call time:
    //
    // 1. `MultiThread` / `MultiThreadAlt`: `block_in_place` is safe
    //    and is the canonical fix for "Cannot drop a runtime in a
    //    context where blocking is not allowed" — it converts the
    //    current worker into a blocking thread so its inner
    //    `block_on` can spin up its own scheduler.
    //
    // 2. `CurrentThread` / `CurrentScheduler`: `block_in_place`
    //    panics. Fall back to a plain `block_on`, which is safe
    //    here because there is no worker pool to corrupt.
    //
    // Earlier versions used `block_in_place` unconditionally and
    // documented the current-thread panic as a "production never
    // hits this" assumption. Any future test or alternative runtime
    // path that picked current_thread would crash the entire tool
    // subsystem. Runtime flavor detection is one extra branch and
    // makes the helper safe on every supported flavor.
    let handle = Handle::current();
    let params = params;
    let h: &dyn ToolHandler = handler;
    match handle.runtime_flavor() {
        RuntimeFlavor::MultiThread => tokio::task::block_in_place(
            || handle.block_on(async move { h.execute(params).await }),
        ),
        _ => handle.block_on(async move { h.execute(params).await }),
    }
}


// ---------------------------------------------------------------------------
// ToolRegistry
// ---------------------------------------------------------------------------

/// Thread-safe registry of all available tools.
pub struct ToolRegistryInner {
    /// Registered tools keyed by name.
    tools: HashMap<String, ToolEntry>,
    /// Global default max result size in characters.
    pub global_max_result_size_chars: usize,
    /// Shared handle to the terminal tool's per-turn default workspace.
    /// `register_builtin_tools` clones this into the `TerminalHandler` it
    /// constructs, and `LocalAgentService::send_message_stream` writes the
    /// active workspace here before each turn. Cheap (RwLock + Arc), no
    /// extra coupling between the two crates beyond this single handle.
    pub terminal_workspace: Arc<std::sync::RwLock<Option<std::path::PathBuf>>>,
}

/// Thread-safe wrapper around `ToolRegistryInner`.
#[derive(Clone)]
pub struct ToolRegistry {
    inner: Arc<RwLock<ToolRegistryInner>>,
}

impl ToolRegistry {
    /// Create a new empty registry with default global max result size.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(ToolRegistryInner {
                tools: HashMap::new(),
                global_max_result_size_chars: 50_000,
                terminal_workspace: Arc::new(std::sync::RwLock::new(None)),
            })),
        }
    }

    /// Create a new registry with a custom global max result size.
    pub fn with_max_result_size(max: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(ToolRegistryInner {
                tools: HashMap::new(),
                global_max_result_size_chars: max,
                terminal_workspace: Arc::new(std::sync::RwLock::new(None)),
            })),
        }
    }

    /// Get a clone of the shared terminal-workspace handle. `register_builtin_tools`
    /// uses this to wire the `TerminalHandler` to the same `RwLock` the agent
    /// service writes to.
    pub fn terminal_workspace_handle(&self) -> Arc<std::sync::RwLock<Option<std::path::PathBuf>>> {
        self.inner.read().unwrap().terminal_workspace.clone()
    }

    /// Set the per-turn default workspace for the terminal (and any other
    /// workspace-aware) tool. Called by `LocalAgentService` before each
    /// `agent.run_*` from `AgentOverrides::workspace`. Cheap: single
    /// RwLock write.
    pub fn set_terminal_workspace(&self, workspace: Option<std::path::PathBuf>) {
        if let Ok(mut guard) = self.inner.read().unwrap().terminal_workspace.write() {
            *guard = workspace;
        } else {
            tracing::warn!("terminal_workspace lock poisoned; ignoring update");
        }
    }

    /// Register a new tool.
    ///
    /// If a tool with the same name already exists, the call is rejected
    /// (logged + no-op). Pass `override_existing: true` to force a
    /// replacement — used by tests and legitimate hot-swap paths. Plugin
    /// / MCP / dynamic registrars should call the **non-overriding**
    /// variant by default so a buggy plugin cannot silently clobber a
    /// built-in handler.
    pub fn register(
        &self,
        name: impl Into<String>,
        toolset: impl Into<String>,
        schema: ToolSchema,
        handler: Arc<dyn ToolHandler>,
        check_fn: Arc<dyn Fn() -> bool + Send + Sync>,
        env_deps: Vec<String>,
        is_async: bool,
        description: impl Into<String>,
        emoji: impl Into<String>,
        max_result_size_chars: Option<usize>,
    ) {
        self.register_with_permission(
            name,
            toolset,
            schema,
            handler,
            check_fn,
            env_deps,
            is_async,
            description,
            emoji,
            max_result_size_chars,
            RiskLevel::default(),
            PermissionMode::default(),
            /* override_existing = */ false,
        );
    }

    /// Same as [`Self::register`] but lets the caller attach a non-default
    /// [`RiskLevel`] **and** choose whether to permit overwriting a
    /// pre-existing entry with the same name.
    ///
    /// `override_existing: false` (the safe default) makes registration
    /// **idempotent by name**: a second call for a name that's already
    /// in the registry logs an error and returns without touching the
    /// entry. This stops plugins / MCP servers from accidentally
    /// shadowing a built-in tool like `terminal` or `web_search` and
    /// silently disabling the original handler.
    ///
    /// `override_existing: true` is the original "last writer wins"
    /// behaviour, kept for the two legitimate scenarios where it's
    /// needed: (a) the test suite re-creating a registry per case and
    /// (b) an admin-level hot-swap that intentionally wants to replace
    /// a built-in with a newer / patched handler.
    /// Default `is_interactive` to false; use `mark_interactive()` to override.
    /// Permission mode defaults to `PermissionMode::Default`.
    pub fn register_with_risk(
        &self,
        name: impl Into<String>,
        toolset: impl Into<String>,
        schema: ToolSchema,
        handler: Arc<dyn ToolHandler>,
        check_fn: Arc<dyn Fn() -> bool + Send + Sync>,
        env_deps: Vec<String>,
        is_async: bool,
        description: impl Into<String>,
        emoji: impl Into<String>,
        max_result_size_chars: Option<usize>,
        risk_level: RiskLevel,
        override_existing: bool,
    ) {
        self.register_with_permission(
            name,
            toolset,
            schema,
            handler,
            check_fn,
            env_deps,
            is_async,
            description,
            emoji,
            max_result_size_chars,
            risk_level,
            PermissionMode::default(),
            override_existing,
        );
    }

    /// Same as [`Self::register_with_risk`] but also accepts a [`PermissionMode`]
    /// for fine-grained access control (inspired by claw-code design).
    pub fn register_with_permission(
        &self,
        name: impl Into<String>,
        toolset: impl Into<String>,
        schema: ToolSchema,
        handler: Arc<dyn ToolHandler>,
        check_fn: Arc<dyn Fn() -> bool + Send + Sync>,
        env_deps: Vec<String>,
        is_async: bool,
        description: impl Into<String>,
        emoji: impl Into<String>,
        max_result_size_chars: Option<usize>,
        risk_level: RiskLevel,
        permission_mode: PermissionMode,
        override_existing: bool,
    ) {
        let name = name.into();
        let mut inner = self.inner.write().unwrap();
        if inner.tools.contains_key(&name) {
            if !override_existing {
                tracing::error!(
                    tool = %name,
                    "Tool '{}' already registered; refusing to overwrite without override flag. \
                     Built-in shadow registration rejected to prevent plugin handler \
                     collisions. If you really want to replace the existing entry, call \
                     register_with_risk(..., override_existing=true).",
                    name
                );
                return;
            }
            warn!("Tool '{}' already registered; overwriting (override=true)", name);
        }
        // New entries default to enabled. The Settings UI can flip
        // this later via `set_toolset_enabled` without re-registering.
        // We intentionally do NOT honour the caller's `check_fn` for
        // the runtime-disable dimension — `check_fn` stays as
        // capability (env vars, browser driver) and `is_enabled` is
        // its own axis. Both are AND-ed in `get_definitions` /
        // `is_available` below.
        let is_enabled = Arc::new(AtomicBool::new(true));
        inner.tools.insert(
            name.clone(),
            ToolEntry {
                name: name.clone(),
                toolset: toolset.into(),
                schema,
                handler,
                check_fn,
                env_deps,
                is_async,
                description: description.into(),
                emoji: emoji.into(),
                max_result_size_chars,
                risk_level,
                permission_mode,
                is_interactive: false,
                is_enabled,
            },
        );
    }

    /// Mark a registered tool as interactive (sequential execution required).
    /// Interactive tools like `clarify` and `delegate_task` must not run
    /// in parallel with other tools. Returns true if the tool was found.
    pub fn mark_interactive(&self, name: &str) -> bool {
        let mut inner = self.inner.write().unwrap();
        if let Some(entry) = inner.tools.get_mut(name) {
            entry.is_interactive = true;
            true
        } else {
            false
        }
    }

    /// Replace the schema of an already-registered tool.
    ///
    /// Background (P1.2.4, Plan v2): a few tools have parameters
    /// that vary with the operator's configuration — e.g.
    /// `delegate_task` lists the available expert roles by name,
    /// and `model_providers` enumerates which providers are
    /// currently configured. When the configuration changes we
    /// want the next LLM turn to see the fresh schema without
    /// the operator having to restart the desktop app.
    ///
    /// This entry point mutates **only the schema** of an
    /// existing entry; the handler, check_fn, risk_level, and
    /// all other metadata stay. If no entry with `name` exists
    /// the call is a silent no-op (the operator probably hasn't
    /// registered that toolset yet).
    pub fn update_schema(&self, name: &str, new_schema: ToolSchema) {
        let mut inner = self.inner.write().unwrap();
        if let Some(entry) = inner.tools.get_mut(name) {
            entry.schema = new_schema;
        }
    }

    /// Coerce `params` into the JSON types declared by `schema`.
    ///
    /// Background (P1.2.2, Plan v2): LLM providers sometimes hand us
    /// a `String` like `"42"` for a field whose schema is `integer`,
    /// or a `"true"` string for a boolean. The naïve fix would be to
    /// have every handler validate + cast, but that leaks the LLM's
    /// permissive JSON typing into every tool. Instead we do it once
    /// here, on the dispatch path, before the handler is called.
    ///
    /// Scope of coercions (intentionally small — keep it boring):
    /// - `string` ↔ `number` / `integer` (when the string parses cleanly)
    /// - `string` → `boolean` (`"true"` / `"false"`, case-insensitive)
    /// - `string` → `array` (when the string is valid JSON array)
    /// - `array` ↔ `object` is **not** auto-coerced — that's a
    ///   semantic mismatch the LLM should be told about.
    /// - Missing fields declared as `required` in the schema are
    ///   **not** filled in here (that's the caller's job — coercing
    ///   is a narrow type fixup, not a defaults injector).
    ///
    /// Returns the coerced `Value`. The original is consumed.
    pub fn coerce_args(schema: &ToolSchema, params: Value) -> Value {
        let mut out = params;
        if let Some(properties) = schema.parameters.properties.as_ref() {
            Self::coerce_object(properties, &mut out);
        }
        out
    }

    fn coerce_object(properties: &IndexMap<String, Value>, value: &mut Value) {
        // Only coerce inside an object; other top-level types (string
        // command, number, etc.) are tool-defined and we trust them.
        if let Value::Object(map) = value {
            for (key, prop_schema) in properties {
                if let Some(field) = map.get_mut(key) {
                    Self::coerce_field(prop_schema, field);
                }
            }
        }
    }

    fn coerce_field(prop_schema: &Value, field: &mut Value) {
        // A field schema may be:
        //   {"type": "string"}                     → simple
        //   {"type": ["string", "null"]}           → nullable union
        //   {"$ref": "..."}                        → we don't resolve refs
        //   {}                                     → permissive
        //
        // We extract the type (or types) and try the matching
        // coercion. On failure we leave the original in place —
        // letting the handler decide whether to fail.
        let declared = Self::declared_types(prop_schema);
        if declared.is_empty() {
            return;
        }

        for target in &declared {
            if Self::try_coerce(field, target) {
                return;
            }
        }
    }

    fn declared_types(prop_schema: &Value) -> Vec<String> {
        match prop_schema.get("type") {
            Some(Value::String(s)) => vec![s.clone()],
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Try to coerce `field` in place to `target` JSON type. Returns
    /// `true` on success (the value was modified), `false` if no
    /// coercion was possible.
    fn try_coerce(field: &mut Value, target: &str) -> bool {
        match (field.clone(), target) {
            // string → integer / number
            (Value::String(s), "integer") => {
                if let Ok(n) = s.parse::<i64>() {
                    *field = Value::Number(n.into());
                    return true;
                }
                false
            }
            (Value::String(s), "number") => {
                if let Ok(n) = s.parse::<f64>() {
                    if let Some(num) = serde_json::Number::from_f64(n) {
                        *field = Value::Number(num);
                        return true;
                    }
                }
                false
            }
            // string → boolean
            (Value::String(s), "boolean") => match s.trim().to_ascii_lowercase().as_str() {
                "true" => {
                    *field = Value::Bool(true);
                    true
                }
                "false" => {
                    *field = Value::Bool(false);
                    true
                }
                _ => false,
            },
            // string → array / object (when the string parses as JSON)
            (Value::String(s), "array") | (Value::String(s), "object") => {
                if let Ok(parsed) = serde_json::from_str::<Value>(&s) {
                    let parsed_is_object_or_array =
                        matches!(&parsed, Value::Array(_) | Value::Object(_));
                    let matches_target = matches!(
                        (target, &parsed),
                        ("array", Value::Array(_)) | ("object", Value::Object(_))
                    );
                    if parsed_is_object_or_array && matches_target {
                        *field = parsed;
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// Enable or disable every tool whose `toolset` field matches
    /// `toolset`. Returns the number of entries flipped. Used by:
    ///
    /// 1. `register_builtin_tools` at startup to apply the
    ///    `enabled_toolsets` startup filter (now re-implemented as
    ///    "register all, then disable some" so toggling back on
    ///    works without a re-register).
    /// 2. The Settings UI's `PUT /api/tools/toolsets/enabled`
    ///    handler to flip user choices at runtime.
    ///
    /// Lock-then-iterate under the same inner mutex that
    /// `get_definitions` / `is_available` hold, so dispatch
    /// observers see a consistent snapshot (either all-flip or
    /// none-flip, no half-applied state). The actual atomic
    /// stores use `Release` ordering; readers use `Acquire`.
    pub fn set_toolset_enabled(&self, toolset: &str, enabled: bool) -> usize {
        let inner = self.inner.read().unwrap();
        let mut count = 0;
        for entry in inner.tools.values() {
            if entry.toolset == toolset {
                entry.is_enabled.store(enabled, Ordering::Release);
                count += 1;
            }
        }
        count
    }

    /// Return the [`RiskLevel`] of a registered tool, or `None` if no such
    /// tool exists. The pre-execution approval hook uses this to decide
    /// whether to prompt the user.
    pub fn risk_level(&self, name: &str) -> Option<RiskLevel> {
        let inner = self.inner.read().unwrap();
        inner.tools.get(name).map(|e| e.risk_level)
    }

    /// Deregister a tool by name.
    ///
    /// Returns `true` if the tool was present and removed.
    pub fn deregister(&self, name: &str) -> bool {
        let mut inner = self.inner.write().unwrap();
        inner.tools.remove(name).is_some()
    }

    /// Get tool definitions for all tools whose `check_fn` returns true
    /// **and** whose `is_enabled` flag is set.
    ///
    /// Both gates are required: a tool that is capability-OK (env
    /// vars present) but user-disabled will be hidden from the LLM;
    /// a tool that is capability-broken (missing env) will also be
    /// hidden even if user-enabled. This is used to build the tool
    /// list sent to the LLM.
    pub fn get_definitions(&self) -> Vec<ToolSchema> {
        let inner = self.inner.read().unwrap();
        inner
            .tools
            .values()
            .filter(|entry| {
                (entry.check_fn)() && entry.is_enabled.load(Ordering::Acquire)
            })
            .map(|entry| entry.schema.clone())
            .collect()
    }

    /// Generate a markdown reference file listing all registered tools.
    /// The LLM can `read_file` it on demand instead of bloating the prompt.
    pub fn write_tools_reference(&self, path: &std::path::Path) -> Result<(), String> {
        let inner = self.inner.read().map_err(|e| format!("lock: {e}"))?;
        let mut md = String::from("# Tool Reference\n\n");
        md.push_str("Read this file when you need a tool's parameters.\n\n");

        let mut tools: Vec<&ToolEntry> = inner.tools.values().filter(|e| e.is_enabled.load(Ordering::Acquire)).collect();
        tools.sort_by(|a, b| a.schema.name.cmp(&b.schema.name));

        for entry in &tools {
            let s = &entry.schema;
            md.push_str(&format!("## {}\n", s.name));
            md.push_str(&format!("- Toolset: {} | Risk: {:?}\n", entry.toolset, entry.risk_level));
            let desc = s.description.trim();
            if !desc.is_empty() {
                md.push_str(&format!("- {}\n", desc));
            }
            if let Some(props) = &s.parameters.properties {
                let required = s.parameters.required.as_deref().unwrap_or_default();
                for (pname, pinfo) in props {
                    let ptype = pinfo.get("type").and_then(|v| v.as_str()).unwrap_or("any");
                    let pdesc = pinfo.get("description").and_then(|v| v.as_str()).unwrap_or("");
                    let req = if required.contains(pname) { " *" } else { "" };
                    if !pdesc.is_empty() {
                        md.push_str(&format!("  - `{}`: `{}`{} — {}\n", pname, ptype, req, pdesc));
                    } else {
                        md.push_str(&format!("  - `{}`: `{}`{}\n", pname, ptype, req));
                    }
                }
            }
            md.push('\n');
        }
        std::fs::write(path, &md).map_err(|e| format!("write failed: {e}"))?;
        Ok(())
    }

    /// Dispatch a tool call by name, catching all errors.
    ///
    /// On success, returns the tool result string.
    /// On failure, returns a JSON error string: `{"error": "..."}`.
    pub fn dispatch(&self, name: &str, params: Value) -> String {
        let inner = self.inner.read().unwrap();
        let entry = match inner.tools.get(name) {
            Some(e) => e,
            None => return Self::tool_error(&format!("Tool not found: {}", name)),
        };
        let handler = Arc::clone(&entry.handler);
        let max_chars = entry
            .max_result_size_chars
            .unwrap_or(inner.global_max_result_size_chars);
        let schema = entry.schema.clone();
        drop(inner); // release lock before async execute

        // P1.2.2: auto-coerce string args to declared JSON-schema
        // types before handing them to the handler. LLM providers
        // occasionally return `"42"` for an integer field; we
        // transparently fix that here so handlers can write
        // `as_u64()` without first validating.
        let params = Self::coerce_args(&schema, params);

        // All production callers of dispatch() run inside a tokio task
        // (e.g. agent_loop JoinSet::spawn). A bare `Handle::block_on`
        // is fatal here — it creates a *new* blocking pool on this
        // worker thread, and dropping that pool from inside the worker
        // panics with "Cannot drop a runtime in a context where blocking
        // is not allowed" (tokio-1.51.1
        // `runtime/blocking/shutdown.rs:51:21`).
        //
        // The correct bridge from a tokio worker into `async { ... }`
        // is `block_in_place` + `block_on`. `block_in_place` converts
        // the current worker into a blocking thread, which then is
        // free to host its own `Handle::block_on` future without
        // colliding with the outer runtime's pool. This is the
        // canonical pattern documented in the `tokio` crate's
        // "Bridging with sync code" section, and is safe in both
        // multi-thread and current-thread runtimes — `block_in_place`
        // returns an error on current-thread (in which case we fall
        // back to a plain `block_on` which still works on
        // current-thread because there is no worker pool to corrupt).
        let result: Result<String, hermix_core::ToolError> =
            run_async_in_blocking(handler.as_ref(), params);

        match result {
            Ok(output) => {
                if output.chars().count() > max_chars {
                    // UTF-8 safe: `truncate_chars` slices on a
                    // char boundary even when `max_chars` lands in
                    // the middle of a multi-byte code point. Earlier
                    // versions used `&output[..max_chars]` (byte
                    // index) which panicked on every CJK / emoji
                    // output over the limit.
                    let head = truncate_chars(&output, max_chars);
                    let removed = output.chars().count() - max_chars;
                    Self::tool_result(&format!(
                        "{}\n\n[... truncated {} characters ...]",
                        head, removed
                    ))
                } else {
                    Self::tool_result(&output)
                }
            }
            Err(e) => Self::tool_error(&e.to_string()),
        }
    }

    /// Dispatch a tool call asynchronously.
    #[instrument(
        name = "tool_dispatch",
        level = "info",
        skip_all,
        fields(
            tool = %name,
            params_bytes = params.to_string().len(),
            result_chars = tracing::field::Empty,
            result_kind = tracing::field::Empty,
            tool_ms = tracing::field::Empty,
            error = tracing::field::Empty,
        )
    )]
    pub async fn dispatch_async(&self, name: &str, params: Value) -> String {
        let started = std::time::Instant::now();
        let (handler, max_chars, schema) = {
            let inner = self.inner.read().unwrap();
            match inner.tools.get(name) {
                Some(e) => (
                    Arc::clone(&e.handler),
                    e.max_result_size_chars
                        .unwrap_or(inner.global_max_result_size_chars),
                    e.schema.clone(),
                ),
                None => return Self::tool_error(&format!("Tool not found: {}", name)),
            }
        };

        // P1.2.2: see `dispatch` for rationale.
        let params = Self::coerce_args(&schema, params);

        let result = match handler.execute(params).await {
            Ok(output) => {
                if output.chars().count() > max_chars {
                    let head = truncate_chars(&output, max_chars);
                    let removed = output.chars().count() - max_chars;
                    let truncated = Self::tool_result(&format!(
                        "{}\n\n[... truncated {} characters ...]",
                        head, removed
                    ));
                    (truncated, "truncated", output.chars().count())
                } else {
                    let len = output.chars().count();
                    (Self::tool_result(&output), "ok", len)
                }
            }
            Err(e) => {
                // N-06: surface the error to the span so a `tracing`
                // subscriber can correlate failures with the
                // originating tool call.
                tracing::Span::current().record("error", tracing::field::display(&e));
                (Self::tool_error(&e.to_string()), "error", 0)
            }
        };
        let elapsed = started.elapsed();
        tracing::Span::current().record("result_chars", result.2);
        tracing::Span::current().record("result_kind", result.1);
        tracing::Span::current().record("tool_ms", elapsed.as_millis() as u64);
        result.0
    }

    /// Get a reference to a tool entry by name.
    ///
    /// Note: This returns a cloned `Arc<ToolEntry>` because we cannot return
    /// a reference to data behind a Mutex lock. Returns `None` if not found.
    pub fn get_tool(&self, name: &str) -> Option<ToolEntryInfo> {
        let inner = self.inner.read().unwrap();
        inner.tools.get(name).map(|e| ToolEntryInfo {
            name: e.name.clone(),
            toolset: e.toolset.clone(),
            description: e.description.clone(),
            emoji: e.emoji.clone(),
            is_async: e.is_async,
            is_enabled: e.is_enabled.load(Ordering::Acquire),
            env_deps: e.env_deps.clone(),
            max_result_size_chars: e.max_result_size_chars,
        })
    }

    /// List all registered tool entries.
    pub fn list_tools(&self) -> Vec<ToolEntryInfo> {
        let inner = self.inner.read().unwrap();
        inner
            .tools
            .values()
            .map(|e| ToolEntryInfo {
                name: e.name.clone(),
                toolset: e.toolset.clone(),
                description: e.description.clone(),
                emoji: e.emoji.clone(),
                is_async: e.is_async,
                is_enabled: e.is_enabled.load(Ordering::Acquire),
                env_deps: e.env_deps.clone(),
                max_result_size_chars: e.max_result_size_chars,
            })
            .collect()
    }

    /// List distinct toolset names.
    pub fn list_toolsets(&self) -> Vec<String> {
        let inner = self.inner.read().unwrap();
        let mut sets: Vec<String> = inner.tools.values().map(|e| e.toolset.clone()).collect();
        sets.sort();
        sets.dedup();
        sets
    }

    /// Catalog — lightweight tool directory for agent startup.
    /// Returns only enabled+available tools, ~50 bytes per entry.
    pub fn catalog(&self) -> Vec<hermix_core::ToolCatalogEntry> {
        let inner = self.inner.read().unwrap();
        inner.tools.values()
            .filter(|e| e.is_enabled.load(Ordering::Acquire) && (e.check_fn)())
            .map(|e| hermix_core::ToolCatalogEntry {
                name: e.name.clone(),
                one_line: truncate_one_line(&e.description),
                risk_level: format!("{:?}", e.risk_level).to_lowercase(),
                tags: vec![e.toolset.clone()],
            })
            .collect()
    }

    /// Look up the full schema for a single tool, so the agent can
    /// call `get_tool_details("name")` after browsing the catalog.
    pub fn get_tool_schema(&self, name: &str) -> Option<ToolSchema> {
        let inner = self.inner.read().unwrap();
        inner.tools.get(name)
            .filter(|e| e.is_enabled.load(Ordering::Acquire) && (e.check_fn)())
            .map(|e| e.schema.clone())
    }

    /// Check whether a tool is available (exists, `check_fn` passes,
    /// **and** `is_enabled` is set). See [`Self::get_definitions`]
    /// for the rationale on the double gate.
    pub fn is_available(&self, name: &str) -> bool {
        let inner = self.inner.read().unwrap();
        match inner.tools.get(name) {
            Some(e) => (e.check_fn)() && e.is_enabled.load(Ordering::Acquire),
            None => false,
        }
    }

    /// Format an error as JSON: `{"error": "msg"}`.
    pub fn tool_error(msg: &str) -> String {
        serde_json::json!({ "error": msg }).to_string()
    }

    /// Format a tool result. If the result looks like JSON, pass it through;
    /// otherwise wrap it as `{"result": "..."}`.
    pub fn tool_result(data: &str) -> String {
        if data.starts_with('{') || data.starts_with('[') {
            // Assume already JSON-ish, pass through
            data.to_string()
        } else {
            serde_json::json!({ "result": data }).to_string()
        }
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Simplified info about a tool entry (no handler, for inspection).
#[derive(Debug, Clone)]
pub struct ToolEntryInfo {
    pub name: String,
    pub toolset: String,
    pub description: String,
    pub emoji: String,
    pub is_async: bool,
    pub is_enabled: bool,
    pub env_deps: Vec<String>,
    pub max_result_size_chars: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use hermix_core::{tool_schema, JsonSchema, ToolError, ToolSchema};
    use serde_json::Value;

    struct EchoHandler;

    #[async_trait]
    impl ToolHandler for EchoHandler {
        async fn execute(&self, params: Value) -> Result<String, ToolError> {
            Ok(params.to_string())
        }
        fn schema(&self) -> ToolSchema {
            tool_schema("echo", "Echo back input", JsonSchema::new("object"))
        }
    }

    #[test]
    fn test_register_and_dispatch() {
        let registry = ToolRegistry::new();
        let handler = Arc::new(EchoHandler);
        let schema = handler.schema();
        registry.register(
            "echo",
            "test",
            schema,
            handler,
            Arc::new(|| true),
            vec![],
            false,
            "Echo tool",
            "🔊",
            None,
        );

        let defs = registry.get_definitions();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "echo");
    }

    #[test]
    fn test_register_defaults_risk_level_to_caution() {
        let registry = ToolRegistry::new();
        let handler = Arc::new(EchoHandler);
        let schema = handler.schema();
        registry.register(
            "echo",
            "test",
            schema,
            handler,
            Arc::new(|| true),
            vec![],
            false,
            "Echo tool",
            "🔊",
            None,
        );
        assert_eq!(registry.risk_level("echo"), Some(RiskLevel::Caution));
    }

    #[test]
    fn test_register_with_risk_overrides_default() {
        let registry = ToolRegistry::new();
        let handler = Arc::new(EchoHandler);
        let schema = handler.schema();
        registry.register_with_risk(
            "terminal",
            "shell",
            schema,
            handler,
            Arc::new(|| true),
            vec![],
            false,
            "Terminal tool",
            "💻",
            None,
            RiskLevel::Dangerous,
            /* override_existing = */ false,
        );
        assert_eq!(registry.risk_level("terminal"), Some(RiskLevel::Dangerous));
    }

    #[test]
    fn test_register_rejects_duplicate_without_override() {
        // First registration succeeds and is visible.
        let registry = ToolRegistry::new();
        let handler1 = Arc::new(EchoHandler);
        let schema1 = handler1.schema();
        registry.register(
            "echo",
            "test",
            schema1,
            handler1,
            Arc::new(|| true),
            vec![],
            false,
            "First echo",
            "🔊",
            None,
        );
        assert_eq!(registry.get_definitions().len(), 1);

        // Second registration for the same name with the default
        // (override_existing = false) MUST be a no-op. The original
        // description must survive — this is the "plugin cannot shadow
        // a built-in" guarantee.
        let handler2 = Arc::new(EchoHandler);
        let schema2 = handler2.schema();
        registry.register(
            "echo",
            "test",
            schema2,
            handler2,
            Arc::new(|| true),
            vec![],
            false,
            "Shadowed echo (should be discarded)",
            "🚫",
            None,
        );

        let entries = registry.list_tools();
        let echo = entries.iter().find(|e| e.name == "echo").unwrap();
        assert_eq!(
            echo.description, "First echo",
            "duplicate registration must NOT overwrite the original entry"
        );
    }

    #[test]
    fn test_register_allows_explicit_override() {
        let registry = ToolRegistry::new();
        let handler1 = Arc::new(EchoHandler);
        let schema1 = handler1.schema();
        registry.register(
            "echo",
            "test",
            schema1,
            handler1,
            Arc::new(|| true),
            vec![],
            false,
            "Original",
            "🔊",
            None,
        );

        // Explicit override_existing = true replaces the entry.
        let handler2 = Arc::new(EchoHandler);
        let schema2 = handler2.schema();
        registry.register_with_risk(
            "echo",
            "test",
            schema2,
            handler2,
            Arc::new(|| true),
            vec![],
            false,
            "Replaced",
            "🚫",
            None,
            RiskLevel::Dangerous,
            /* override_existing = */ true,
        );

        let entries = registry.list_tools();
        let echo = entries.iter().find(|e| e.name == "echo").unwrap();
        assert_eq!(echo.description, "Replaced");
        assert_eq!(registry.risk_level("echo"), Some(RiskLevel::Dangerous));
    }

    #[test]
    fn test_risk_level_unknown_tool() {
        let registry = ToolRegistry::new();
        assert_eq!(registry.risk_level("nope"), None);
    }

    #[test]
    fn test_dispatch_not_found() {
        let registry = ToolRegistry::new();
        let result = registry.dispatch("nonexistent", Value::Null);
        assert!(result.contains("error"));
    }

    #[test]
    fn test_tool_error_format() {
        let msg = ToolRegistry::tool_error("something went wrong");
        let parsed: Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(parsed["error"], "something went wrong");
    }

    #[test]
    fn test_tool_result_format() {
        // Plain string wrapped as JSON
        let result = ToolRegistry::tool_result("hello");
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["result"], "hello");

        // Already JSON passes through
        let json_result = ToolRegistry::tool_result(r#"{"key": "val"}"#);
        let parsed: Value = serde_json::from_str(&json_result).unwrap();
        assert_eq!(parsed["key"], "val");
    }

    #[test]
    fn test_deregister() {
        let registry = ToolRegistry::new();
        let handler = Arc::new(EchoHandler);
        let schema = handler.schema();
        registry.register(
            "echo",
            "test",
            schema,
            handler,
            Arc::new(|| true),
            vec![],
            false,
            "Echo tool",
            "🔊",
            None,
        );
        assert!(registry.deregister("echo"));
        assert!(!registry.deregister("echo"));
        assert!(registry.get_definitions().is_empty());
    }

    #[test]
    fn test_check_fn_filtering() {
        let registry = ToolRegistry::new();
        let handler = Arc::new(EchoHandler);
        let schema = handler.schema();
        registry.register(
            "available",
            "test",
            schema.clone(),
            Arc::new(EchoHandler),
            Arc::new(|| true),
            vec![],
            false,
            "Available",
            "✅",
            None,
        );
        registry.register(
            "unavailable",
            "test",
            schema,
            Arc::new(EchoHandler),
            Arc::new(|| false),
            vec![],
            false,
            "Unavailable",
            "❌",
            None,
        );
        let defs = registry.get_definitions();
        assert_eq!(defs.len(), 1);
        // ToolSchema name comes from the handler's schema(), which is "echo" for EchoHandler.
        // The registry key ("available") and the schema name may differ.
        assert_eq!(defs[0].name, "echo");
    }

    // -----------------------------------------------------------------------
    // P1.2.2 — coerce_args tests (Plan v2)
    // -----------------------------------------------------------------------

    /// `String("42")` → `Number(42)` when schema says `integer`.
    #[test]
    fn coerce_string_to_integer() {
        use indexmap::IndexMap;
        let mut props = IndexMap::new();
        props.insert("count".to_string(), serde_json::json!({"type": "integer"}));
        let schema = ToolSchema::new("t", "d", JsonSchema::object(props, vec!["count".into()]));

        let mut params = serde_json::json!({"count": "42"});
        ToolRegistry::coerce_args(&schema, std::mem::take(&mut params));
        // re-derive from the modified binding — but we need a fresh one
        let mut params = serde_json::json!({"count": "42"});
        let coerced = ToolRegistry::coerce_args(&schema, std::mem::take(&mut params));
        assert_eq!(coeff_u64(&coerced, "count"), Some(42));
    }

    #[test]
    fn coerce_string_to_number() {
        use indexmap::IndexMap;
        let mut props = IndexMap::new();
        props.insert("ratio".to_string(), serde_json::json!({"type": "number"}));
        let schema = ToolSchema::new("t", "d", JsonSchema::object(props, vec![]));

        let params = serde_json::json!({"ratio": "3.14"});
        let coerced = ToolRegistry::coerce_args(&schema, params);
        assert!(
            coerced.get("ratio").and_then(|v| v.as_f64()).is_some(),
            "ratio should coerce to a number: got {}",
            coerced
        );
    }

    #[test]
    fn coerce_string_to_boolean() {
        use indexmap::IndexMap;
        let mut props = IndexMap::new();
        props.insert("enabled".to_string(), serde_json::json!({"type": "boolean"}));
        let schema = ToolSchema::new("t", "d", JsonSchema::object(props, vec![]));

        let params = serde_json::json!({"enabled": "true"});
        let coerced = ToolRegistry::coerce_args(&schema, params);
        assert_eq!(coerced.get("enabled"), Some(&serde_json::json!(true)));

        let params = serde_json::json!({"enabled": "False"});
        let coerced = ToolRegistry::coerce_args(&schema, params);
        assert_eq!(coerced.get("enabled"), Some(&serde_json::json!(false)));
    }

    #[test]
    fn coerce_string_to_array() {
        use indexmap::IndexMap;
        let mut props = IndexMap::new();
        props.insert("tags".to_string(), serde_json::json!({"type": "array"}));
        let schema = ToolSchema::new("t", "d", JsonSchema::object(props, vec![]));

        let params = serde_json::json!({"tags": "[\"a\", \"b\"]"});
        let coerced = ToolRegistry::coerce_args(&schema, params);
        let arr = coerced.get("tags").and_then(|v| v.as_array());
        assert!(arr.is_some(), "tags should be array, got {}", coerced);
        assert_eq!(arr.unwrap().len(), 2);
    }

    /// Unparseable strings pass through unchanged — handler decides.
    #[test]
    fn coerce_leaves_unparseable_string_unchanged() {
        use indexmap::IndexMap;
        let mut props = IndexMap::new();
        props.insert("count".to_string(), serde_json::json!({"type": "integer"}));
        let schema = ToolSchema::new("t", "d", JsonSchema::object(props, vec![]));

        let params = serde_json::json!({"count": "not-a-number"});
        let coerced = ToolRegistry::coerce_args(&schema, params);
        assert_eq!(
            coerced.get("count"),
            Some(&serde_json::json!("not-a-number")),
            "unparseable string must be left as-is for the handler to reject"
        );
    }

    /// Already-correct types are no-ops.
    #[test]
    fn coerce_passes_through_correct_types() {
        use indexmap::IndexMap;
        let mut props = IndexMap::new();
        props.insert("count".to_string(), serde_json::json!({"type": "integer"}));
        props.insert("enabled".to_string(), serde_json::json!({"type": "boolean"}));
        let schema = ToolSchema::new("t", "d", JsonSchema::object(props, vec![]));

        let params = serde_json::json!({"count": 7, "enabled": true});
        let coerced = ToolRegistry::coerce_args(&schema, params);
        assert_eq!(coerced.get("count"), Some(&serde_json::json!(7)));
        assert_eq!(coerced.get("enabled"), Some(&serde_json::json!(true)));
    }

    /// Nullable union `["string", "null"]` still tries the string→integer
    /// coercion when the value is a string.
    #[test]
    fn coerce_nullable_union_type() {
        use indexmap::IndexMap;
        let mut props = IndexMap::new();
        props.insert(
            "count".to_string(),
            serde_json::json!({"type": ["integer", "null"]}),
        );
        let schema = ToolSchema::new("t", "d", JsonSchema::object(props, vec![]));

        let params = serde_json::json!({"count": "5"});
        let coerced = ToolRegistry::coerce_args(&schema, params);
        assert_eq!(coerced.get("count"), Some(&serde_json::json!(5)));
    }

    /// Missing properties in the schema → no coercion attempted on
    /// anything, the input is returned as-is. (Coercion is a narrow
    /// type fixup, not a schema-driven defaults injector.)
    #[test]
    fn coerce_no_schema_returns_input_unchanged() {
        let schema = ToolSchema::new("t", "d", JsonSchema::new("object"));
        let params = serde_json::json!({"x": "42"});
        let coerced = ToolRegistry::coerce_args(&schema, params.clone());
        assert_eq!(coerced, params);
    }

    /// dispatch() auto-coerces args before the handler sees them.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatch_auto_coerces_args() {
        use indexmap::IndexMap;
        let mut props = IndexMap::new();
        props.insert("count".to_string(), serde_json::json!({"type": "integer"}));
        let schema = ToolSchema::new("count_tool", "Count", JsonSchema::object(props, vec![]));

        let registry = ToolRegistry::new();
        let handler = Arc::new(EchoHandler);
        registry.register(
            "count_tool",
            "test",
            schema,
            handler,
            Arc::new(|| true),
            vec![],
            false,
            "Counts things",
            "🔢",
            None,
        );

        let result = registry.dispatch("count_tool", serde_json::json!({"count": "7"}));
        // EchoHandler returns params.to_string(); an integer 7
        // serializes as `7`, not `"7"`.
        assert!(
            result.contains("\"count\":7"),
            "expected dispatch to coerce \"7\" → 7, got {}",
            result
        );
    }

    // -------------------------------------------------------------------
    // P1.2.4 — update_schema tests (Plan v2)
    // -------------------------------------------------------------------

    #[test]
    fn update_schema_replaces_existing_tool_schema() {
        let registry = ToolRegistry::new();
        let handler = Arc::new(EchoHandler);
        registry.register(
            "echo",
            "test",
            handler.schema(),
            handler,
            Arc::new(|| true),
            vec![],
            false,
            "Echo",
            "🔊",
            None,
        );

        let mut props = IndexMap::new();
        props.insert("msg".to_string(), serde_json::json!({"type": "string"}));
        let new_schema =
            ToolSchema::new("echo", "New description", JsonSchema::object(props, vec![]));

        registry.update_schema("echo", new_schema.clone());

        let defs = registry.get_definitions();
        assert_eq!(defs[0].description, "New description");
    }

    #[test]
    fn update_schema_on_unknown_tool_is_noop() {
        let registry = ToolRegistry::new();
        let schema = tool_schema("ghost", "n/a", JsonSchema::new("object"));
        // No panic, no insert — just silently does nothing.
        registry.update_schema("ghost", schema);
        assert_eq!(registry.get_definitions().len(), 0);
    }
}

/// Helper for coerce_args tests: extract a u64 field, returning None
/// if the field is missing or not a number.
fn coeff_u64(v: &serde_json::Value, key: &str) -> Option<u64> {
    v.get(key).and_then(|x| x.as_u64())
}

// -----------------------------------------------------------------------
// P1.2.4 — update_schema tests (Plan v2)
// -----------------------------------------------------------------------
// (Tests live inside the `tests` module above to share `EchoHandler`.)

// -----------------------------------------------------------------------
// P1.2.4 — update_schema tests (Plan v2)
// -----------------------------------------------------------------------
// (added inside `tests` module above to share `EchoHandler`)

/// Trim a description to ~50 chars for the one-line catalog entry.
fn truncate_one_line(desc: &str) -> String {
    let first_line = desc.lines().next().unwrap_or(desc);
    if first_line.len() <= 50 {
        first_line.to_string()
    } else {
        format!("{}…", &first_line[..47])
    }
}
