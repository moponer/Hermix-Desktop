use std::sync::Arc;

use hermix_core::{
    JsonSchema, ToolError, ToolHandler, ToolSchema,
    traits::RiskLevel,
};
use indexmap::IndexMap;
use serde_json::Value;

use crate::ToolRegistry;
use crate::tool_meta::{ToolMetaRegistry, DEFAULT_RESIDENT_TOOLS};

/// Tools that are ALWAYS visible to the LLM (never deferred).
/// Anything not in this list becomes a candidate for tool_search bridge.
/// This constant is kept for backwards compatibility with
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
/// This is the metadata-driven replacement for [`classify_tools`].
/// Tools registered as `LoadingStrategy::Deferred` in the metadata
/// registry are placed in the deferred bucket; all others (including
/// unregistered tools and bridge tools) are visible.
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
    // Split out any pre-existing bridge tools (idempotent re-entry) instead
    // of dropping them. The old shape filtered them out of `incoming` and
    // only re-added them on the activated branch, so a registry with fewer
    // than `DEFERRED_THRESHOLD` deferred tools lost `tool_search` /
    // `tool_describe` / `tool_call` from the schema list entirely — while
    // `directions.rs` went on advertising them. The model was told the
    // bridge existed, could not see it, and burned turns rediscovering it.
    // ("同一会话内先成功后 does not exist" — the first turn crossed the
    // threshold, the next did not.)
    let mut existing_bridges: Vec<ToolSchema> = Vec::new();
    let mut incoming: Vec<ToolSchema> = Vec::new();
    for s in all {
        if is_bridge(&s.name) {
            existing_bridges.push(s);
        } else {
            incoming.push(s);
        }
    }
    let (visible, deferred) = classify_tools(&incoming);
    if deferred.len() < DEFERRED_THRESHOLD {
        // Bridge NOT active → every deferred tool is directly visible, so the
        // bridge trio is pure overhead: it occupies slots in the tools array
        // (prompt tokens) and it *invites* the model to go on a discovery
        // detour. Measured 2026-10-22: 25% of a long-running task's calls
        // were spent hunting for tools through it.
        //
        // Dropping them here also keeps `directions.rs` honest: that guidance
        // is gated on the visible tool names, so with the trio out of the
        // array the model is never told the bridge exists.
        return AssemblyResult {
            tool_schemas: incoming,
            activated: false,
            deferred_count: 0,
        };
    }
    let mut result: Vec<ToolSchema> = visible.into_iter().cloned().collect();
    let count = deferred.len();
    // Activated: emit the canonical bridge trio (carrying the real deferred
    // count), not whatever happened to be in the incoming list.
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
                        "action".into(),
                        serde_json::json!({
                            "type": "string",
                            "enum": ["search", "list"],
                            "description": "`search` (default) matches keywords; `list` enumerates every tool in the registry — use it to see what the app can do before asking for keywords."
                        }),
                    );
                    m.insert(
                        "query".into(),
                        serde_json::json!({
                            "type": "string",
                            "description": "Keywords to search for (e.g. 'create github issue'). Required unless action=list."
                        }),
                    );
                    m.insert(
                        "limit".into(),
                        serde_json::json!({
                            "type": "integer",
                            "description": "Max results. Default 5 for search, 200 for list."
                        }),
                    );
                    m
                },
                vec![],
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

/// 截断描述到 200 字符（按 char 边界截，避免切坏 UTF-8 多字节）。
fn short_description(desc: &str) -> String {
    const MAX: usize = 200;
    if desc.chars().count() <= MAX {
        return desc.to_string();
    }
    let cut: String = desc.chars().take(MAX - 3).collect();
    format!("{cut}...")
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
                        "action".into(),
                        serde_json::json!({
                            "type": "string",
                            "enum": ["search", "list"],
                            "description": "`search` (default) or `list` (enumerate all tools)"
                        }),
                    );
                    m.insert(
                        "query".into(),
                        serde_json::json!({
                            "type": "string",
                            "description": "Keywords to search for. Required unless action=list."
                        }),
                    );
                    m.insert(
                        "limit".into(),
                        serde_json::json!({
                            "type": "integer",
                            "description": "Max results. Default 5 for search, 200 for list."
                        }),
                    );
                    m
                },
                vec![],
            ),
        )
    }

    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("search")
            .trim()
            .to_ascii_lowercase();

        // `list` 分支：全量枚举工具名 + 描述（含常驻工具，不限于 deferred 桶）。
        // 存在的理由：按关键词盲搜永远可能漏 —— 一个不知道自己有哪些工具
        // 的助理，谈不上"知道你能做到什么"。
        if action == "list" {
            let limit = params
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(200) as usize;
            let all_defs = self.registry.get_definitions();
            let (resident, deferred) = classify_tools(&all_defs);
            let tools: Vec<Value> = all_defs
                .iter()
                .take(limit)
                .map(|s| {
                    let deferred_bucket = deferred.iter().any(|d| d.name == s.name);
                    serde_json::json!({
                        "name": s.name,
                        "description": short_description(&s.description),
                        "bucket": if deferred_bucket { "deferred" } else { "resident" },
                    })
                })
                .collect();
            return Ok(serde_json::json!({
                "action": "list",
                "total": all_defs.len(),
                "resident": resident.len(),
                "deferred": deferred.len(),
                "returned": tools.len(),
                "tools": tools,
            })
            .to_string());
        }

        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if query.is_empty() {
            return Err(ToolError::InvalidParams(
                "query is required unless action=list".into(),
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
                serde_json::json!({
                    "name": s.name,
                    "description": short_description(&s.description),
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

/// Pull the target tool's arguments out of a `tool_call` params object.
///
/// The declared shape is `{"name": "...", "arguments": {...}}`, and that is
/// what `coerce_args` produces when the model complies. But models —
/// especially the ones that just ran `tool_describe` and saw the target
/// tool's schema inline — frequently flatten the call instead:
/// `{"name": "memory_store", "text": "cssso"}`, with the target's fields
/// sitting next to `name` at the top level. The old code took
/// `params["arguments"]` unconditionally and substituted `{}` when the key
/// was missing, so the target tool ran with an empty parameter object and
/// reported `missing 'text'` / `必须要有 confirm: true` — an error that
/// points at the target tool's schema when the actual fault is two levels
/// up in the bridge call.
///
/// The fallback only fires when `arguments` is absent or not an object, so
/// a compliant call is untouched: no `name` / `arguments` key ever leaks
/// into the target's parameters.
fn extract_arguments(params: &Value) -> Value {
    match params.get("arguments") {
        Some(Value::Object(m)) => Value::Object(m.clone()),
        // A JSON-encoded string is what several providers emit for
        // `arguments`; `coerce_args` normally fixes this upstream, but the
        // bridge is also reachable through `dispatch_bridge_tool`, which
        // bypasses coercion.
        Some(Value::String(s)) => serde_json::from_str::<Value>(s)
            .ok()
            .filter(|v| v.is_object())
            .unwrap_or_else(|| flatten_into_arguments(params)),
        _ => flatten_into_arguments(params),
    }
}

/// Collect every top-level key except the bridge's own (`name` and
/// `arguments`) as the target's arguments.
///
/// Excluding `arguments` matters on the fallback path: when the model sends
/// a JSON *string* that fails to parse, the old filter kept the raw
/// `arguments` key and handed a string to a tool that expects an object
/// there — one more shape the target had to defend against. The two bridge
/// keys are never valid target parameters, so both are dropped here.
fn flatten_into_arguments(params: &Value) -> Value {
    match params.as_object() {
        Some(m) => Value::Object(
            m.iter()
                .filter(|(k, _)| k.as_str() != "name" && k.as_str() != "arguments")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ),
        None => serde_json::json!({}),
    }
}

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

        let arguments = extract_arguments(&params);

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

// ── Bridge registration ─────────────────────────────────────────

/// Register the three bridge tools into `registry`.
///
/// Until this existed the handlers were fully written and never wired:
/// `tools::register_all` never called into this module, so none of
/// `tool_search` / `tool_describe` / `tool_call` ever reached
/// `get_definitions()`. Three independent layers then made the failure
/// invisible from the model's side:
///
///  1. `bridge_tool_registry` (hermix-agent) only bridges what
///     `get_definitions()` returns, so the agent's registry stayed empty
///     of bridge names.
///  2. `run_loop` validates every claimed tool call with
///     `tool_registry.get(name).is_some()` → false → `tool_call_validator`
///     rejects with `Tool 'tool_call' does not exist`.
///  3. `directions.rs` still advertises the bridge, so the model saw the
///     tools in its system prompt and kept calling them into the wall.
///
/// Net effect: the whole deferred-tool path was dead — `ToolCallHandler`
/// below was unreachable code, and the model burned tokens retrying.
///
/// `ToolRegistry` is a cheap handle over `Arc<RwLock<ToolRegistryInner>>`,
/// so `registry.clone()` + `Arc::new(..)` shares the *same* registry
/// rather than snapshotting it — which matters because `tool_call`
/// dispatches back into it.
pub fn register(registry: &ToolRegistry) {
    let shared = Arc::new(registry.clone());
    let entries: [(&str, Arc<dyn ToolHandler>, RiskLevel, &str, &str); 3] = [
        (
            "tool_search",
            Arc::new(ToolSearchHandler::new(shared.clone())),
            RiskLevel::Safe,
            "\u{1F50D}",
            "Find deferred tools by keyword. Returns name + one-line description for each match.",
        ),
        (
            "tool_describe",
            Arc::new(ToolDescribeHandler::new(shared.clone())),
            RiskLevel::Safe,
            "\u{1F4D6}",
            "Get the full parameter schema for a deferred tool, so tool_call receives correct arguments.",
        ),
        (
            "tool_call",
            Arc::new(ToolCallHandler::new(shared.clone())),
            // Not Safe: this one can dispatch ANY deferred tool, including
            // Dangerous ones. Caution keeps it out of parallel batches and
            // leaves per-tool approval decisions to the dispatched handler.
            RiskLevel::Caution,
            "\u{1F17F}",
            "Invoke a deferred tool by name. `arguments` MUST match that tool's schema exactly — run tool_describe first if unsure.",
        ),
    ];
    for (name, handler, risk, emoji, description) in entries {
        let schema = handler.schema();
        registry.register_with_risk(
            name,
            name,
            schema,
            handler,
            Arc::new(|| true),
            vec![],
            true,
            description,
            emoji,
            None,
            risk,
            true,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `tool_call` 的参数包装：合规形态（对象）必须原样透传。
    #[test]
    fn extract_arguments_passes_object_through() {
        let params = serde_json::json!({"name": "memory_store", "arguments": {"text": "cssso"}});
        assert_eq!(extract_arguments(&params), serde_json::json!({"text": "cssso"}));
    }

    /// 模型跑完 `tool_describe` 后常把目标字段平铺在顶层，这是报告里
    /// "缺少必需参数 text" 的成因：老代码无条件读 `arguments`，缺失就
    /// 填 `{}`，目标工具拿着空参数对象报错。
    #[test]
    fn extract_arguments_flattens_when_arguments_absent() {
        let params = serde_json::json!({"name": "memory_store", "text": "cssso"});
        assert_eq!(extract_arguments(&params), serde_json::json!({"text": "cssso"}));
    }

    /// 有些 provider 把 `arguments` 编成 JSON 字符串（`dispatch_bridge_tool`
    /// 绕过 `coerce_args`），要能还原。
    #[test]
    fn extract_arguments_parses_json_string() {
        let params =
            serde_json::json!({"name": "todo", "arguments": "{\"x\": 960, \"y\": 540}"});
        assert_eq!(extract_arguments(&params), serde_json::json!({"x": 960, "y": 540}));
    }

    /// 兜底不能把 `name` 漏进目标参数，也不能把 `arguments` 之外的杂音
    /// 混进去。
    #[test]
    fn extract_arguments_never_leaks_the_bridge_keys() {
        let params = serde_json::json!({"name": "t", "arguments": {"a": 1}});
        let got = extract_arguments(&params);
        let obj = got.as_object().expect("arguments must stay an object");
        assert!(!obj.contains_key("name"));
        assert!(!obj.contains_key("arguments"));
    }

    /// 非对象 / 坏字符串的兜底：宁可拿到平铺的顶层键，也不要 `{}`。
    #[test]
    fn extract_arguments_falls_back_to_flatten_for_garbage() {
        let params = serde_json::json!({"name": "t", "arguments": "not json", "text": "x"});
        assert_eq!(extract_arguments(&params), serde_json::json!({"text": "x"}));
    }

    /// Regression guard for the dead deferred-tool path.
    ///
    /// `register_all` never called into this module, so the three bridge
    /// tools never entered `get_definitions()`. hermix-agent's
    /// `bridge_tool_registry` bridges only that list, and `run_loop` gates
    /// every claimed tool call on `tool_registry.get(name).is_some()` — so
    /// `tool_call` was rejected with "does not exist" and
    /// `ToolCallHandler` was unreachable, while `directions.rs` kept
    /// advertising the bridge.
    ///
    /// The pre-existing `tool_call_dispatches_to_real_tool` test could not
    /// catch this: it drives the handler directly, bypassing registration.
    #[test]
    fn register_exposes_all_three_bridge_tools() {
        let reg = crate::ToolRegistry::new();
        register(&reg);
        let names: Vec<String> = reg.get_definitions().into_iter().map(|s| s.name).collect();
        for want in ["tool_search", "tool_describe", "tool_call"] {
            assert!(names.contains(&want.to_string()), "missing from registry: {want}");
        }
    }

    /// `action=list` 必须能看到「常驻桶」里的工具 —— 这正是 search 分支
    /// 的结构性盲区（它只扫 deferred 桶），也是「助理不知道自己会什么」的成因。
    #[tokio::test]
    async fn list_action_enumerates_resident_tools_too() {
        let reg = Arc::new(crate::ToolRegistry::new());
        register(&reg);
        let handler = ToolSearchHandler::new(reg);

        let out = handler
            .execute(serde_json::json!({"action": "list"}))
            .await
            .expect("list 不该失败");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();

        assert_eq!(v["action"], "list");
        let tools = v["tools"].as_array().expect("tools 必须是数组");
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        for want in ["tool_search", "tool_describe", "tool_call"] {
            assert!(names.contains(&want), "list 漏了常驻工具 {want}");
        }
        assert_eq!(v["returned"].as_u64().unwrap() as usize, tools.len());
        assert!(v["total"].as_u64().unwrap() >= tools.len() as u64);
        // 每个条目都必须带 bucket，调用方才分得清常驻/按需
        assert!(tools
            .iter()
            .all(|t| t["bucket"] == "resident" || t["bucket"] == "deferred"));
    }

    /// 去掉 required 之后，search 分支仍要自己拒空 query（不能静默返回空）。
    #[tokio::test]
    async fn search_without_query_is_rejected() {
        let reg = Arc::new(crate::ToolRegistry::new());
        register(&reg);
        let handler = ToolSearchHandler::new(reg);
        let err = handler.execute(serde_json::json!({})).await.unwrap_err();
        assert!(
            matches!(err, ToolError::InvalidParams(_)),
            "空 query 必须报 InvalidParams，实际: {err:?}"
        );
    }

    /// The shared handle must alias the caller's live registry, not a
    /// snapshot — `tool_call` dispatches back into it, so a copied registry
    /// would reject every deferred target as "not found".
    #[tokio::test]
    async fn registered_tool_call_dispatches_into_the_same_registry() {
        let reg = crate::ToolRegistry::new();

        struct Ping;
        #[async_trait::async_trait]
        impl ToolHandler for Ping {
            fn schema(&self) -> ToolSchema {
                ToolSchema::new(
                    "bridge_ping_probe",
                    "probe target",
                    JsonSchema::object(IndexMap::new(), vec![]),
                )
            }
            async fn execute(&self, _p: Value) -> Result<String, ToolError> {
                Ok("pong".into())
            }
        }
        reg.register_with_risk(
            "bridge_ping_probe",
            "bridge_ping_probe",
            Ping.schema(),
            Arc::new(Ping),
            Arc::new(|| true),
            vec![],
            true,
            "probe target",
            "p",
            None,
            RiskLevel::Safe,
            true,
        );
        register(&reg);

        // Goes through the registry's own `tool_call` entry point, exactly
        // as the agent loop would after `bridge_tool_registry` copies it.
        // The registry surfaces handler failures as an `{"error":…}` body
        // rather than an Err, so assert on the payload, not on `?`.
        let out = reg
            .dispatch_async(
                "tool_call",
                serde_json::json!({ "name": "bridge_ping_probe", "arguments": {} }),
            )
            .await;
        assert!(
            !out.contains("\"error\""),
            "tool_call failed to reach the live registry: {out}"
        );
        assert!(out.contains("pong"), "expected the probe's result, got: {out}");
    }

    /// Regression guard for the flattened-call shape.
    ///
    /// A real session produced four consecutive failures — `missing 'text'`,
    /// two × `缺少 steps 数组`, `必须要有 confirm: true` — all of them the
    /// target tool's own param validation, all of them reached *after* the
    /// user clicked 允许 on the approval card. The bridge read
    /// `params["arguments"]` unconditionally, so when the model put the
    /// target's fields next to `name` instead of nesting them, the target
    /// ran with `{}` and blamed its own schema.
    ///
    /// The probe target below echoes the params it received, so this fails
    /// if the argument object is empty, still nested, or carrying the
    /// bridge's own `name` key.
    #[tokio::test]
    async fn tool_call_accepts_flattened_arguments() {
        let reg = crate::ToolRegistry::new();

        struct Echo;
        #[async_trait::async_trait]
        impl ToolHandler for Echo {
            fn schema(&self) -> ToolSchema {
                ToolSchema::new(
                    "bridge_echo_probe",
                    "echo target",
                    JsonSchema::object(IndexMap::new(), vec!["text".into()]),
                )
            }
            async fn execute(&self, p: Value) -> Result<String, ToolError> {
                match p.get("text").and_then(|v| v.as_str()) {
                    Some(t) => Ok(format!("echo:{t}")),
                    // Report the shape actually seen — a bare "missing text"
                    // is what made the original report point at the wrong file.
                    None => Ok(format!("no_text:keys={:?}", p.as_object().map(|m|
                        m.keys().cloned().collect::<Vec<_>>()))),
                }
            }
        }
        reg.register_with_risk(
            "bridge_echo_probe",
            "bridge_echo_probe",
            Echo.schema(),
            Arc::new(Echo),
            Arc::new(|| true),
            vec![],
            true,
            "echo target",
            "e",
            None,
            RiskLevel::Safe,
            true,
        );
        register(&reg);

        // Flattened: `text` sits next to `name`, no `arguments` wrapper.
        let flat = reg
            .dispatch_async(
                "tool_call",
                serde_json::json!({ "name": "bridge_echo_probe", "text": "cssso" }),
            )
            .await;
        assert!(flat.contains("echo:cssso"), "flattened call lost its args: {flat}");

        // The compliant shape must keep working and must NOT gain a stray
        // `name` key from the fallback path.
        let nested = reg
            .dispatch_async(
                "tool_call",
                serde_json::json!({ "name": "bridge_echo_probe", "arguments": { "text": "ok" } }),
            )
            .await;
        assert!(nested.contains("echo:ok"), "nested call regressed: {nested}");
        assert!(
            !nested.contains("no_text"),
            "nested call must not fall through to the flatten path: {nested}"
        );
    }

    /// A JSON-encoded `arguments` string must be unwrapped, not handed to
    /// the target as a string. `coerce_args` handles this on the registry
    /// dispatch path, but `dispatch_bridge_tool` (dispatch.rs) constructs the
    /// handler directly and skips coercion.
    #[tokio::test]
    async fn tool_call_unwraps_stringified_arguments() {
        let reg = crate::ToolRegistry::new();
        struct Echo;
        #[async_trait::async_trait]
        impl ToolHandler for Echo {
            fn schema(&self) -> ToolSchema {
                ToolSchema::new(
                    "bridge_str_echo",
                    "echo target",
                    JsonSchema::object(IndexMap::new(), vec!["text".into()]),
                )
            }
            async fn execute(&self, p: Value) -> Result<String, ToolError> {
                Ok(format!(
                    "got:{}",
                    p.get("text").and_then(|v| v.as_str()).unwrap_or("<none>")
                ))
            }
        }
        reg.register_with_risk(
            "bridge_str_echo",
            "bridge_str_echo",
            Echo.schema(),
            Arc::new(Echo),
            Arc::new(|| true),
            vec![],
            true,
            "echo target",
            "e",
            None,
            RiskLevel::Safe,
            true,
        );
        register(&reg);

        // Bypass the registry's coercion by driving the handler the way
        // `dispatch_bridge_tool` does.
        let handler = ToolCallHandler::new(Arc::new(reg));
        let out = handler
            .execute(serde_json::json!({
                "name": "bridge_str_echo",
                "arguments": "{\"text\":\"from-string\"}",
            }))
            .await
            .expect("handler must not error");
        assert!(out.contains("got:from-string"), "stringified args not parsed: {out}");
    }

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

    /// Regression guard for the intermittent "tool_search / tool_describe
    /// does not exist" report.
    ///
    /// The old `assemble_tool_schemas` filtered bridge tools out of the
    /// incoming list and only re-added them on the *activated* branch. A
    /// registry under `DEFERRED_THRESHOLD` therefore shipped a schema list
    /// with no bridge at all — while `directions.rs` still advertised the
    /// bridge. Turn 1 crossed the threshold (bridge visible, call succeeds),
    /// turn 2 didn't (bridge gone, `tool_call_validator` rejects it). That
    /// is the "先成功后 does not exist" shape exactly.
    #[test]
    fn assembly_keeps_bridges_below_threshold() {
        let mut schemas = vec![
            ToolSchema::new("read_file", "Read", JsonSchema::new("object")),
            ToolSchema::new("tool_search", "Bridge", JsonSchema::new("object")),
            ToolSchema::new("tool_describe", "Bridge", JsonSchema::new("object")),
            ToolSchema::new("tool_call", "Bridge", JsonSchema::new("object")),
        ];
        // 3 deferred — below DEFERRED_THRESHOLD (25).
        for i in 0..3 {
            schemas.push(ToolSchema::new(
                format!("deferred_{i}"),
                format!("Deferred {i}"),
                JsonSchema::new("object"),
            ));
        }
        let result = assemble_tool_schemas(schemas);
        assert!(!result.activated, "below threshold must not activate");
        let names: Vec<&str> = result.tool_schemas.iter().map(|s| s.name.as_str()).collect();
        for want in BRIDGE_TOOL_NAMES {
            assert!(
                names.contains(want),
                "bridge '{want}' vanished below threshold — directions.rs still advertises it"
            );
        }
    }

    /// Same contract for the meta-driven variant, which had the identical
    /// filter-and-drop shape — asserted in `deferred_loader.rs`, where that
    /// function lives.
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
        assert!(!is_bridge("web_search"));
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

    //── : classify_tools_with_meta tests ──────────────────────

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
