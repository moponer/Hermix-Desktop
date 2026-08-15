//! Tool Registry (Requirement 4)
//!
//! Central registry for all tool definitions and handlers. Supports:
//! - Dynamic registration/deregistration with availability checks
//! - Name-conflict detection with warning on overwrite
//! - Per-tool result size limits and global default
//! - Dispatch with error catching that always returns JSON

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use hermix_core::{traits::{RiskLevel, PermissionMode}, truncate_chars, ToolHandler, ToolSchema};
use serde_json::Value;
use tracing::instrument;

use crate::SessionStamper;
use crate::approval::TerminalApprovalContext;
use crate::context::{ContextAware, ToolCallContext};

// ---------------------------------------------------------------------------
// ToolEntry
// ---------------------------------------------------------------------------

/// A registered tool with its handler, metadata, and availability check.

pub mod entry;
pub use entry::{ToolEntry, ToolEntryInfo};

pub mod validation;
pub mod schema;


/// Run an async `ToolHandler::execute` call from a synchronous context
/// (e.g. the agent_loop `JoinSet` worker thread that just pulled a
/// `dyn Fn(Value) -> Result<String, ToolError>` handler off the
/// `agent_loop::ToolRegistry`).
/// **Why this helper exists.** `agent_loop` stores tool handlers as
/// synchronous closures (see `agent_loop::ToolEntry::handler`); inside
/// the closure they call into `ToolRegistry::dispatch` which has an
/// async `ToolHandler` underneath. A naïve
/// `Handle::current().block_on(async { ... })` panics on multi-thread
/// runtimes because it tries to drop a freshly-created blocking pool
/// from within a tokio worker — the exact "Cannot drop a runtime in
/// a context where blocking is not allowed" panic reported by the
/// user when they typed "你好" into the chat box.
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
    // Two paths, picked by the runtime flavor detected at call time:
    // 1. `MultiThread` / `MultiThreadAlt`: `block_in_place` is safe
    // and is the canonical fix for "Cannot drop a runtime in a
    // context where blocking is not allowed" — it converts the
    // current worker into a blocking thread so its inner
    // `block_on` can spin up its own scheduler.
    // 2. `CurrentThread` / `CurrentScheduler`: `block_in_place`
    // panics. Fall back to a plain `block_on`, which is safe
    // here because there is no worker pool to corrupt.
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

/// Same as [`run_async_in_blocking`] but for `ContextAware` tools that
/// need per-session `ToolCallContext` at dispatch time. The context is
/// cloned into the `block_on` future so the borrow outlives the
/// `block_in_place` scope.
fn run_async_in_blocking_with_context(
    ca: &dyn ContextAware,
    params: Value,
    ctx: &ToolCallContext,
) -> Result<String, hermix_core::ToolError> {
    use tokio::runtime::{Handle, RuntimeFlavor};
    let handle = Handle::current();
    let ctx_owned = ctx.clone();
    match handle.runtime_flavor() {
        RuntimeFlavor::MultiThread => tokio::task::block_in_place(|| {
            handle.block_on(async move { ca.execute_with_context(params, &ctx_owned).await })
        }),
        _ => handle.block_on(async move { ca.execute_with_context(params, &ctx_owned).await }),
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
    /// Shared handle to the terminal tool's per-request approval hook
    /// (, "Caution tool needs UI prompts"). The `TerminalHandler`
    /// clones this on construction; `LocalAgentService::send_message`
    /// writes a fresh `TerminalApprovalContext` (capturing the active WS
    /// `ApprovalRegistry` + `on_event`) on every turn so the terminal
    /// tool's `RequiresConfirmation` branch can emit a `request_approval`
    /// event instead of bouncing the user through text. `None` in both
    /// fields ⇒ fallback to the legacy `HERMIX_CONFIRM=1` text flow.
    pub terminal_approval_ctx: Arc<std::sync::Mutex<TerminalApprovalContext>>,
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
                terminal_approval_ctx: Arc::new(std::sync::Mutex::new(
                    TerminalApprovalContext::default(),
                )),
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
                terminal_approval_ctx: Arc::new(std::sync::Mutex::new(
                    TerminalApprovalContext::default(),
                )),
            })),
        }
    }

    /// Get a clone of the shared terminal-workspace handle. `register_builtin_tools`
    /// uses this to wire the `TerminalHandler` to the same `RwLock` the agent
    /// service writes to.
    pub fn terminal_workspace_handle(&self) -> Arc<std::sync::RwLock<Option<std::path::PathBuf>>> {
        self.inner.read().expect("registry rwlock not poisoned").terminal_workspace.clone()
    }

    /// Set the per-turn default workspace for the terminal (and any other
    /// workspace-aware) tool. Called by `LocalAgentService` before each
    /// `agent.run_*` from `AgentOverrides::workspace`. Cheap: single
    /// RwLock write.
    pub fn set_terminal_workspace(&self, workspace: Option<std::path::PathBuf>) {
        if let Ok(mut guard) = self.inner.read().expect("registry rwlock not poisoned").terminal_workspace.write() {
            *guard = workspace;
        } else {
            tracing::warn!("terminal_workspace lock poisoned; ignoring update");
        }
    }

    /// Get a clone of the shared terminal-approval-context handle.
    /// `register_builtin_tools` (and the per-tool `register` call)
    /// clone this into `TerminalHandler` so the agent service can
    /// swap the underlying context per request without re-registering
    /// the tool.
    pub fn terminal_approval_ctx_handle(&self) -> Arc<std::sync::Mutex<TerminalApprovalContext>> {
        self.inner
            .read()
            .expect("registry rwlock not poisoned")
            .terminal_approval_ctx
            .clone()
    }

    /// Set the per-request terminal approval hook. Called by
    /// `LocalAgentService::send_message` with the `ApprovalRegistry`
    /// closure + `on_event` from `effective_overrides`. Pass the same
    /// value back through [`Self::clear_terminal_approval_ctx`] after
    /// the turn completes so cross-session leaks are impossible
    /// (every send_message captures a fresh `ApprovalRegistry`
    /// anyway, but the explicit clear makes the lifecycle obvious in
    /// stack traces).
    pub fn set_terminal_approval_ctx(&self, ctx: TerminalApprovalContext) {
        match self
            .inner
            .read()
            .expect("registry rwlock not poisoned")
            .terminal_approval_ctx
            .lock()
        {
            Ok(mut guard) => *guard = ctx,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "terminal_approval_ctx lock poisoned; ignoring update"
                );
            }
        }
    }

    /// Reset the terminal approval context to `default()` (both fields
    /// `None`). Called at the end of `send_message` so a dropped
    /// session never leaves a stale `Arc<dyn Fn>` pointing at a dead
    /// WS handle.
    pub fn clear_terminal_approval_ctx(&self) {
        self.set_terminal_approval_ctx(TerminalApprovalContext::default());
    }

    /// Get a clone of the shared terminal-permission-profile handle.
    /// Register a new tool.
    /// If a tool with the same name already exists, the call is rejected
    /// (logged + no-op). Pass `override_existing: true` to force a
    /// replacement — used by tests and legitimate hot-swap paths. Plugin
    /// MCP / dynamic registrars should call the **non-overriding**
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
    /// `override_existing: false` (the safe default) makes registration
    /// **idempotent by name**: a second call for a name that's already
    /// in the registry logs an error and returns without touching the
    /// entry. This stops plugins / MCP servers from accidentally
    /// shadowing a built-in tool like `terminal` or `web_search` and
    /// silently disabling the original handler.
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

    /// Attach a [`ContextAware`] impl to an already-registered tool.
    /// Follows the same opt-in pattern as `set_session_stamper`:
    /// registration flows through `register_with_risk` which has no
    /// `context_aware` parameter (keeping the blast radius tiny — only
    /// terminal / todo / code_execution need one). After registering,
    /// the tool's `register` helper calls this with the same `Arc<T>`
    /// it handed to `register_with_risk`, so the registry keeps a
    /// typed `Arc<dyn ContextAware>` alongside the `Arc<dyn ToolHandler>`.
    pub fn set_context_aware(
        &self,
        name: &str,
        ca: Arc<dyn ContextAware>,
    ) {
        let mut inner = self.inner.write().expect("registry rwlock not poisoned");
        if let Some(entry) = inner.tools.get_mut(name) {
            entry.context_aware = Some(ca);
        } else {
            tracing::warn!(
                tool = %name,
                "set_context_aware called for unregistered tool; ignored"
            );
        }
    }

    /// Attach a [`SessionStamper`] to an already-registered tool.
    /// Registration flows through `register_with_risk` which has no
    /// `session_stamper` parameter (keeping the blast radius tiny — only
    /// the `todo` tool needs one). After registering, the tool's
    /// `register` helper calls this with the same `Arc<TodoHandler>` it
    /// handed to `register_with_risk`, so the registry keeps a second
    /// typed handle (`Arc<dyn SessionStamper>`) alongside the
    /// `Arc<dyn ToolHandler>`. The two point at the same concrete object.
    pub fn set_session_stamper(
        &self,
        name: &str,
        stamper: Arc<dyn SessionStamper>,
    ) {
        let mut inner = self.inner.write().expect("registry rwlock not poisoned");
        if let Some(entry) = inner.tools.get_mut(name) {
            entry.session_stamper = Some(stamper);
        } else {
            tracing::warn!(
                tool = %name,
                "set_session_stamper called for unregistered tool; ignored"
            );
        }
    }

    /// Mark a registered tool as interactive (sequential execution required).
    /// Interactive tools like `clarify` and `delegate_task` must not run
    /// in parallel with other tools. Returns true if the tool was found.
    pub fn mark_interactive(&self, name: &str) -> bool {
        let mut inner = self.inner.write().expect("registry rwlock not poisoned");
        if let Some(entry) = inner.tools.get_mut(name) {
            entry.is_interactive = true;
            true
        } else {
            false
        }
    }

    /// Replace the schema of an already-registered tool.
    /// Background (P1.2.4, Plan v2): a few tools have parameters
    /// that vary with the operator's configuration — e.g.
    /// `delegate_task` lists the available expert roles by name,
    /// and `model_providers` enumerates which providers are
    /// currently configured. When the configuration changes we
    /// want the next LLM turn to see the fresh schema without
    /// the operator having to restart the desktop app.
    /// This entry point mutates **only the schema** of an
    /// existing entry; the handler, check_fn, risk_level, and
    /// all other metadata stay. If no entry with `name` exists
    /// the call is a silent no-op (the operator probably hasn't
    /// registered that toolset yet).
    pub fn update_schema(&self, name: &str, new_schema: ToolSchema) {
        let mut inner = self.inner.write().expect("registry rwlock not poisoned");
        if let Some(entry) = inner.tools.get_mut(name) {
            entry.schema = new_schema;
        }
    }

    /// Dispatch a tool call by name, catching all errors.
    /// On success, returns the tool result string.
    /// On failure, returns a JSON error string: `{"error": "..."}`.
    pub fn dispatch(&self, name: &str, params: Value) -> String {
        let inner = self.inner.read().expect("registry rwlock not poisoned");
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
            let inner = self.inner.read().expect("registry rwlock not poisoned");
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
                // surface the error to the span so a `tracing`
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

    /// Dispatch a tool call by name with per-session context (SYNC).
    ///
    /// If the tool has a `ContextAware` impl, `execute_with_context`
    /// is called with `ctx`. Otherwise, the standard `execute` path
    /// runs (context is ignored). Both paths share the same
    /// truncation / error-wrapping logic.
    pub fn dispatch_with_context(
        &self,
        name: &str,
        params: Value,
        ctx: &ToolCallContext,
    ) -> String {
        let inner = self.inner.read().expect("registry rwlock not poisoned");
        let entry = match inner.tools.get(name) {
            Some(e) => e,
            None => return Self::tool_error(&format!("Tool not found: {}", name)),
        };
        let handler = Arc::clone(&entry.handler);
        let context_aware = entry.context_aware.clone();
        let max_chars = entry
            .max_result_size_chars
            .unwrap_or(inner.global_max_result_size_chars);
        let schema = entry.schema.clone();
        drop(inner);

        let params = Self::coerce_args(&schema, params);

        let result: Result<String, hermix_core::ToolError> = if let Some(ca) = context_aware {
            run_async_in_blocking_with_context(ca.as_ref(), params, ctx)
        } else {
            run_async_in_blocking(handler.as_ref(), params)
        };

        match result {
            Ok(output) => {
                if output.chars().count() > max_chars {
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

    /// Dispatch a tool call asynchronously with per-session context.
    ///
    /// If the tool has a `ContextAware` impl, `execute_with_context`
    /// is called with `ctx`. Otherwise, the standard `execute` path
    /// runs (context is ignored).
    pub async fn dispatch_async_with_context(
        &self,
        name: &str,
        params: Value,
        ctx: &ToolCallContext,
    ) -> String {
        let (handler, context_aware, max_chars, schema) = {
            let inner = self.inner.read().expect("registry rwlock not poisoned");
            match inner.tools.get(name) {
                Some(e) => (
                    Arc::clone(&e.handler),
                    e.context_aware.clone(),
                    e.max_result_size_chars
                        .unwrap_or(inner.global_max_result_size_chars),
                    e.schema.clone(),
                ),
                None => return Self::tool_error(&format!("Tool not found: {}", name)),
            }
        };

        let params = Self::coerce_args(&schema, params);

        // Await inside the match so the `Arc<dyn ContextAware>` (ca)
        // stays alive for the duration of the future. Extracting the
        // future into a `let exec_fut = …` before awaiting would drop
        // `ca` at the end of the `if let` block, leaving a dangling
        // borrow.
        let raw = match context_aware {
            Some(ca) => ca.execute_with_context(params, ctx).await,
            None => handler.execute(params).await,
        };

        match raw {
            Ok(output) => {
                if output.chars().count() > max_chars {
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

    /// Stamp the active session onto a tool whose handler impls
    /// `SessionStamper`. Returns `true` if the tool exists AND
    /// implements the trait; `false` otherwise. The trait lookup is
    /// an `Arc::clone` + `Any` downcast on the registered handler.
    /// Used by `agent_loop.execute_tool_calls` at the top of every
    /// tool-call batch to give the `todo` tool the originating
    /// session id, so the WS layer can route its todo events back
    /// only to the originating pane.
    /// Why not a default trait method on `ToolHandler` itself?
    /// `ToolHandler` is in `hermix-core` — adding a method there
    /// ripples through 18+ tool implementations for a feature only
    /// one tool needs. The opt-in `SessionStamper` marker keeps the
    /// blast radius tiny.
    pub fn set_active_session_for_tool(
        &self,
        name: &str,
        session_id: Option<String>,
    ) -> bool {
        // Grab a clone of the registered stamper handle (released
        // before the mutable downcall below, which is itself lock-free
        // — `set_active_session` only touches a `std::sync::Mutex` on
        // the concrete handler, never the registry's `RwLock`).
        let stamper = {
            let inner = self.inner.read().expect("registry rwlock not poisoned");
            match inner.tools.get(name) {
                Some(e) => match e.session_stamper.as_ref() {
                    Some(s) => Arc::clone(s),
                    None => return false,
                },
                None => return false,
            }
        };
        stamper.set_active_session(session_id);
        true
    }

    /// Stamp the active **assistant message id** onto a tool whose
    /// handler impls `SessionStamper`. Returns `true` if the tool
    /// exists AND opted in. See
    /// [`SessionStamper::set_active_message_id`] for the full
    /// rationale .
    pub fn set_active_message_id_for_tool(
        &self,
        name: &str,
        message_id: Option<String>,
    ) -> bool {
        let stamper = {
            let inner = self.inner.read().expect("registry rwlock not poisoned");
            match inner.tools.get(name) {
                Some(e) => match e.session_stamper.as_ref() {
                    Some(s) => Arc::clone(s),
                    None => return false,
                },
                None => return false,
            }
        };
        stamper.set_active_message_id(message_id);
        true
    }

    /// Build a per-session snapshot of the named tool's internal
    /// state via `SessionStamper::build_snapshot_json`. Returns
    /// `None` if the tool does not exist, did not opt in to
    /// `SessionStamper`, or did not override `build_snapshot_json`
    /// (the default returns `None`). Used by the WS layer to
    /// hydrate the front-end todo panel on connect and after every
    /// mutation.
    /// Commit B.
    pub async fn build_snapshot_json_for_tool(
        &self,
        name: &str,
        session_id: &str,
    ) -> Option<Vec<Value>> {
        let stamper = {
            let inner = self.inner.read().expect("registry rwlock not poisoned");
            match inner.tools.get(name) {
                Some(e) => match e.session_stamper.as_ref() {
                    Some(s) => Arc::clone(s),
                    None => return None,
                },
                None => return None,
            }
        };
        stamper.build_snapshot_json(session_id).await
    }

}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Simplified info about a tool entry (no handler, for inspection).


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
        use indexmap::IndexMap;
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
