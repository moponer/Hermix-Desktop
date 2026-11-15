//! Registry validation: entry registration (duplicate detection) and
//! argument coercion against declared JSON-schema types.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use hermix_core::{traits::{PermissionMode, RiskLevel}, ToolHandler, ToolSchema};
use indexmap::IndexMap;
use serde_json::Value;
use tracing::debug;

use super::ToolRegistry;
use super::entry::ToolEntry;

// ---------------------------------------------------------------------------
// Registration validation (duplicate detection)
// ---------------------------------------------------------------------------

impl ToolRegistry {
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
        let mut inner = self.inner.write().expect("registry rwlock not poisoned");
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
            // `override_existing = true` 是调用方的**明确意图**（典型用途：
            // `register_all` 先用 Signal 占位实现，宿主随后用真实后端覆盖）。
            // 这属于预期路径，不该按 WARN 记 —— 实机每次启动固定刷 2 条，
            // 反而让真正的意外覆盖无迹可寻。
            debug!("Tool '{}' already registered; overwriting (override=true)", name);
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
                session_stamper: None,
                context_aware: None,
            },
        );
    }

    /// Enable or disable every tool whose `toolset` field matches
    /// `toolset`. Returns the number of entries flipped. Used by:
    /// 1. `register_builtin_tools` at startup to apply the
    /// `enabled_toolsets` startup filter (now re-implemented as
    /// "register all, then disable some" so toggling back on
    /// works without a re-register).
    /// 2. The Settings UI's `PUT /api/tools/toolsets/enabled`
    /// handler to flip user choices at runtime.
    /// Lock-then-iterate under the same inner mutex that
    /// `get_definitions` / `is_available` hold, so dispatch
    /// observers see a consistent snapshot (either all-flip or
    /// none-flip, no half-applied state). The actual atomic
    /// stores use `Release` ordering; readers use `Acquire`.
    pub fn set_toolset_enabled(&self, toolset: &str, enabled: bool) -> usize {
        let inner = self.inner.read().expect("registry rwlock not poisoned");
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
        let inner = self.inner.read().expect("registry rwlock not poisoned");
        inner.tools.get(name).map(|e| e.risk_level)
    }

    /// Deregister a tool by name.
    /// Returns `true` if the tool was present and removed.
    pub fn deregister(&self, name: &str) -> bool {
        let mut inner = self.inner.write().expect("registry rwlock not poisoned");
        inner.tools.remove(name).is_some()
    }

    /// Check whether a tool is available (exists, `check_fn` passes,
    /// **and** `is_enabled` is set). See [`Self::get_definitions`]
    /// for the rationale on the double gate.
    pub fn is_available(&self, name: &str) -> bool {
        let inner = self.inner.read().expect("registry rwlock not poisoned");
        match inner.tools.get(name) {
            Some(e) => (e.check_fn)() && e.is_enabled.load(Ordering::Acquire),
            None => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Argument coercion (P1.2.2, Plan v2)
// ---------------------------------------------------------------------------

impl ToolRegistry {
    /// Coerce `params` into the JSON types declared by `schema`.
    /// Background (P1.2.2, Plan v2): LLM providers sometimes hand us
    /// a `String` like `"42"` for a field whose schema is `integer`,
    /// or a `"true"` string for a boolean. The naïve fix would be to
    /// have every handler validate + cast, but that leaks the LLM's
    /// permissive JSON typing into every tool. Instead we do it once
    /// here, on the dispatch path, before the handler is called.
    /// Scope of coercions (intentionally small — keep it boring):
    /// - `string` ↔ `number` / `integer` (when the string parses cleanly)
    /// - `string` → `boolean` (`"true"` / `"false"`, case-insensitive)
    /// - `string` → `array` (when the string is valid JSON array)
    /// - `array` ↔ `object` is **not** auto-coerced — that's a
    /// semantic mismatch the LLM should be told about.
    /// - Missing fields declared as `required` in the schema are
    /// **not** filled in here (that's the caller's job — coercing
    /// is a narrow type fixup, not a defaults injector).
    /// Returns the coerced `Value`. The original is consumed.
    pub fn coerce_args(schema: &ToolSchema, params: Value) -> Value {
        let mut out = params;
        if let Some(properties) = schema.parameters.properties.as_ref() {
            Self::coerce_object(properties, &mut out);
        }
        out
    }

    /// Stamp the active session into `params["_session_id"]`.
    ///
    /// Why: `ToolHandler::execute` only receives `params: Value`, and the
    /// LLM provider never puts a session id in the params. Tools that must
    /// know their originating conversation (e.g. `cronjob` stamping the
    /// automation's anchor session) had no way to learn it — `cua.rs`'s
    /// `extract_session_id` was dead code because nothing ever injected
    /// this key.
    ///
    /// Semantics:
    /// - Underscore prefix: never in any tool's JSON schema, so it can
    ///   never collide with a model-generated argument and never reaches
    ///   the model on the way back.
    /// - Does **not** overwrite an existing `_session_id`: internal
    ///   callers (tests, nested dispatch) may have set a more specific
    ///   value than the ambient session.
    /// - Empty `session_id` (headless/test dispatch) injects nothing —
    ///   an empty string would read as "there is a session named \"\"".
    /// - Non-object params (bare string/number commands) pass through
    ///   untouched.
    /// - Caller must invoke this **after** [`Self::coerce_args`] so the
    ///   injected key is not subject to schema coercion.
    pub fn inject_session_id(params: Value, session_id: &str) -> Value {
        if session_id.is_empty() {
            return params;
        }
        match params {
            Value::Object(mut map) => {
                map.entry("_session_id".to_string())
                    .or_insert_with(|| Value::String(session_id.to_string()));
                Value::Object(map)
            }
            other => other,
        }
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
        // {"type": "string"}                     → simple
        // {"type": ["string", "null"]}           → nullable union
        // {"$ref": "..."}                        → we don't resolve refs
        // {}                                     → permissive
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
}
