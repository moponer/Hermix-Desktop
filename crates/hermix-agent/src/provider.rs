//! LLM provider implementations.
//!
//! Provides concrete implementations of the `LlmProvider` trait for
//! OpenAI, Anthropic, and OpenRouter APIs.

use async_trait::async_trait;
use hermix_config::env_compat;
use futures::stream::BoxStream;
use futures::StreamExt;
use reqwest::Client;
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};
use parking_lot::{Mutex as PLMutex, RwLock};

use hermix_core::{
    AgentError, CacheControl, FunctionCall, FunctionCallDelta, LlmProvider, LlmResponse, Message,
    MessageRole, StreamChunk, StreamDelta, ToolCall, ToolCallDelta, ToolSchema, UsageStats,
};

use crate::credential_pool::CredentialPool;
use crate::rate_limit::RateLimitTracker;

// ---------------------------------------------------------------------------
//HTTP client builder with mandatory timeouts ( / )
// ---------------------------------------------------------------------------

/// Build a `reqwest::Client` with mandatory connect + request timeouts.
///
/// Previously all LLM providers used `Client::new` which has
/// NO connect timeout — a half-open TCP connection (server accept() but
/// never responds) could hang up to 75s (Linux default) or indefinitely
/// (Windows). The `stream_first_chunk` 30s watchdog eventually fires,
/// but the user sees 30s of nothing. `connect_timeout(10s)` ensures the
/// TCP handshake phase fails fast; `timeout(180s)` aligns with the
/// existing `stream_overall` 180s cap so a stuck response body can't
/// outlive the stream watchdog.
fn build_llm_http_client() -> Client {
    Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(180))
        .build()
        .unwrap_or_else(|_| Client::new())
}

// ---------------------------------------------------------------------------
// GenericProvider — a flexible, config-driven provider
// ---------------------------------------------------------------------------

/// A generic LLM provider that can be configured for any OpenAI-compatible API.
/// This is the primary provider used by the agent loop. It supports
/// OpenAI-compatible APIs via configuration.
#[derive(Debug, Clone)]
pub struct GenericProvider {
    /// Base URL for the API endpoint.
    pub base_url: String,
    /// API key for authentication.
    pub api_key: String,
    /// Default model identifier.
    pub model: String,
    /// HTTP client. Uses RwLock so concurrent reads (the hot path —
    /// every LLM API call) proceed without contention; only the rare
    /// `refresh_client` takes a write lock.
    client: Arc<RwLock<Client>>,
    /// Last time we rebuilt the client transport.
    client_refreshed_at: Arc<PLMutex<Instant>>,
    /// Optional custom headers to send with every request.
    pub extra_headers: Vec<(String, String)>,
    /// Optional rate limit tracker.
    pub rate_limiter: Option<Arc<RateLimitTracker>>,
    /// Optional credential pool for key rotation.
    pub credential_pool: Option<Arc<CredentialPool>>,
}

impl GenericProvider {
    fn openrouter_tool_limit(extra_body: Option<&Value>) -> usize {
        if let Some(limit) = extra_body
            .and_then(|v| v.get("openrouter_max_tools"))
            .and_then(|v| v.as_u64())
        {
            return limit as usize;
        }
        if let Some(raw) = env_compat("HERMIX_OPENROUTER_MAX_TOOLS", "HERMES_OPENROUTER_MAX_TOOLS") {
            if let Ok(parsed) = raw.parse::<usize>() {
                return parsed;
            }
        }
        24
    }

    fn tool_priority(name: &str) -> usize {
        // Prefer filesystem/search primitives first when we must trim toolset size.
        // These are the most common "remote mode" requests and preserve core utility.
        if name.starts_with("read_")
            || name.starts_with("write_")
            || name.starts_with("list_")
            || name.starts_with("glob")
            || name.starts_with("search")
            || name.starts_with("grep")
            || name.contains("file")
            || name.contains("dir")
            || name.contains("path")
        {
            return 0;
        }
        if name.starts_with("exec") || name.contains("terminal") || name.contains("shell") {
            return 1;
        }
        if name.starts_with("browser_") {
            return 2;
        }
        3
    }

    fn normalize_tools_payload(&self, tools: &[ToolSchema], extra_body: Option<&Value>) -> Value {
        let mut normalized: Vec<Value> = tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    }
                })
            })
            .collect();

        if self.base_url.contains("openrouter.ai") {
            // OpenRouter may route to stricter downstream providers. Trim and prioritize
            // high-signal tools to avoid provider-specific payload validation failures.
            let mut ranked: Vec<(usize, Value)> = normalized
                .into_iter()
                .map(|v| {
                    let name = v
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or_default();
                    (Self::tool_priority(name), v)
                })
                .collect();
            ranked.sort_by_key(|(priority, _)| *priority);
            let limit = Self::openrouter_tool_limit(extra_body);
            normalized = ranked.into_iter().take(limit).map(|(_, v)| v).collect();
        }

        Value::Array(normalized)
    }

    /// Create a new generic provider.
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
            client: Arc::new(RwLock::new(build_llm_http_client())),
            client_refreshed_at: Arc::new(PLMutex::new(Instant::now())),
            extra_headers: Vec::new(),
            rate_limiter: None,
            credential_pool: None,
        }
    }

    /// Add a custom header to be sent with every request.
    pub fn with_header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_headers.push((key.into(), value.into()));
        self
    }

    /// Set a custom base URL.
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Set the default model.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Attach a rate limit tracker.
    pub fn with_rate_limiter(mut self, tracker: Arc<RateLimitTracker>) -> Self {
        self.rate_limiter = Some(tracker);
        self
    }

    /// Attach a credential pool for API key rotation.
    pub fn with_credential_pool(mut self, pool: Arc<CredentialPool>) -> Self {
        self.credential_pool = Some(pool);
        self
    }

    /// Get the effective API key, using the credential pool if available.
    fn effective_api_key(&self) -> String {
        if let Some(ref pool) = self.credential_pool {
            pool.get_key()
        } else {
            self.api_key.clone()
        }
    }

    /// Check rate limits before making a request. Waits if needed.
    async fn check_rate_limit(&self) {
        if let Some(ref tracker) = self.rate_limiter {
            if let Some(wait_duration) = tracker.should_wait() {
                tracing::info!(
                    "Rate limited, waiting {:?} before next request",
                    wait_duration
                );
                tokio::time::sleep(wait_duration).await;
            }
        }
    }

    /// Update rate limit state from response headers.
    fn update_rate_limit(&self, headers: &reqwest::header::HeaderMap) {
        if let Some(ref tracker) = self.rate_limiter {
            tracker.update_from_headers(headers);
        }
    }

    /// Inject optional runtime hints: reasoning effort, vision preprocessing,
    /// and service tier.
    fn apply_runtime_hints(
        &self,
        body: &mut Value,
        _messages: &[Message],
        extra_body: Option<&Value>,
    ) {
        // Reasoning effort passthrough (`low|medium|high`) using extra_body.reasoning_effort.
        if let Some(eb) = extra_body
            .and_then(|v| v.get("reasoning_effort"))
            .and_then(|v| v.as_str())
        {
            body["reasoning_effort"] = serde_json::json!(eb);
        }

        // OpenAI service tier passthrough.
        if let Some(st) = extra_body
            .and_then(|v| v.get("service_tier"))
            .and_then(|v| v.as_str())
        {
            body["service_tier"] = serde_json::json!(st);
        }
    }

    /// Force-close helper for future explicit TCP cleanup hooks.
    pub fn force_close_tcp_sockets(&self) {
        // reqwest handles connection pooling internally; dropping clones and relying
        // on idle timeout is currently sufficient for our runtime.
    }

    fn current_client(&self) -> Client {
        self.client.read().clone()
    }

    fn refresh_client(&self, reason: &str) {
        tracing::warn!("rebuilding primary HTTP client: {}", reason);
        *self.client.write() = build_llm_http_client();
        *self.client_refreshed_at.lock() = Instant::now();
    }

    async fn maybe_refresh_stale_client(&self, probe_url: &str) {
        const STALE_CLIENT_REFRESH_SECS: u64 = 300;
        let stale_after = Duration::from_secs(STALE_CLIENT_REFRESH_SECS);
        let should_refresh = self.client_refreshed_at.lock().elapsed() >= stale_after;
        if !should_refresh {
            return;
        }
        let probe_client = self.current_client();
        match probe_client
            .get(probe_url)
            .timeout(Duration::from_secs(3))
            .send()
            .await
        {
            Ok(_) => {
                *self.client_refreshed_at.lock() = Instant::now();
            }
            Err(e) => {
                if Self::is_connection_recoverable(&e) {
                    self.refresh_client(&format!("stale connection probe failed: {e}"));
                } else {
                    *self.client_refreshed_at.lock() = Instant::now();
                }
            }
        }
    }

    fn is_connection_recoverable(err: &reqwest::Error) -> bool {
        if err.is_connect() || err.is_timeout() || err.is_request() {
            return true;
        }
        let msg = err.to_string().to_lowercase();
        msg.contains("connection reset")
            || msg.contains("connection closed")
            || msg.contains("broken pipe")
            || msg.contains("pool")
            || msg.contains("eof")
    }

    fn should_sanitize_tool_calls(extra_body: Option<&Value>) -> bool {
        extra_body
            .and_then(|v| {
                v.get("strict_tool_calls")
                    .or_else(|| v.get("strict_api"))
                    .or_else(|| v.get("provider_strict"))
            })
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// Coerce a tool-call `arguments` value into a *valid JSON string*.
    ///
    /// OpenAI-compatible providers (incl. MiniMax) reject the entire
    /// request with HTTP 400 `invalid function arguments json string` if
    /// any tool call's `arguments` is not parseable JSON. Some models
    /// (notably MiniMax-M3) occasionally emit malformed arguments; we
    /// repair what we can and fall back to `{}` rather than letting every
    /// replayed turn 400 and kill the session.
    fn coerce_arguments_to_json_string(args: &Value) -> String {
        match args {
            Value::String(s) => {
                if serde_json::from_str::<Value>(s).is_ok() {
                    return s.clone();
                }
                // Repair attempt: drop trailing commas before `}` / `]`
                // (a common MiniMax glitch: `{"a":1,}` or `[1,2,]`).
                let repaired = Self::strip_json_trailing_commas(s);
                if serde_json::from_str::<Value>(&repaired).is_ok() {
                    return repaired;
                }
                tracing::warn!(
                    arguments = %s,
                    "tool call arguments are not valid JSON; falling back to empty object"
                );
                "{}".to_string()
            }
            // Already a structured value (object / number / …) — serialize it.
            other => serde_json::to_string(other).unwrap_or_else(|_| "{}".to_string()),
        }
    }

    /// Normalize a stored `function` object so its `arguments` is always a
    /// valid JSON string before it is sent to the provider.
    fn normalize_tool_function(mut function: Value) -> Value {
        if let Some(obj) = function.as_object_mut() {
            let fixed = match obj.get("arguments").cloned() {
                Some(a) => Self::coerce_arguments_to_json_string(&a),
                None => "{}".to_string(),
            };
            obj.insert("arguments".to_string(), Value::String(fixed));
        }
        function
    }

    /// Remove trailing commas immediately before `}` or `]` so that
    /// `{"a":1,}` / `[1,2,]` become valid JSON.
    fn strip_json_trailing_commas(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == ',' {
                // peek at the next non-whitespace char
                let mut peek = chars.clone();
                let mut next = None;
                while let Some(&nc) = peek.peek() {
                    if nc.is_whitespace() {
                        peek.next();
                    } else {
                        next = Some(nc);
                        break;
                    }
                }
                if next == Some('}') || next == Some(']') {
                    continue; // drop the trailing comma
                }
            }
            out.push(c);
        }
        out
    }

    /// Strict-pairing pass for tool messages: walks the messages in order,
    /// tracks the most recent assistant message that emitted tool_calls,
    /// and drops any `tool` role message whose `tool_call_id` is NOT in
    /// that assistant's call set, OR whose ID was already consumed by
    /// an earlier sibling tool message in the same assistant pairing.
    ///
    /// Unlike `drop_orphan_tool_results` (which only checks global ID
    /// membership), this enforces the structural rule that strict
    /// providers like MiniMax-M3 require:
    ///   assistant(tool_use=[A, B]) → tool(A) → tool(B)
    /// is valid, but
    ///   assistant(tool_use=[A, B]) → user("...") → tool(A)
    /// is rejected with `(2013) tool call result does not follow tool
    /// call` even though `A` exists in the surviving set — because the
    /// assistant that emitted `A` no longer immediately precedes the
    /// tool result that pairs with it. Compressed sessions that splice
    /// a summary block in between are the typical culprit.
    ///
    /// Also drops tool messages with `tool_call_id: None` or empty string,
    /// because the strict provider rejects anonymous tool results outright
    /// — we'd rather drop them than 400 the entire request.
    fn enforce_strict_tool_pairing(messages: Vec<Message>) -> Vec<Message> {
        // Track the set of `tool_call.id`s emitted by the most recent
        // assistant that produced tool_calls. Tool messages that follow
        // must reference one of these IDs. A non-tool, non-assistant
        // message (user follow-up, system reminder, compressed summary)
        // sitting between assistant(tool_use) and tool(result) breaks
        // the pairing per the strict provider's contract, so any
        // subsequent tool message before a fresh assistant(tool_calls)
        // is dropped.
        let mut pending_call_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut consumed: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut pairing_broken = false; // set when non-tool/assistant appears between pairs
        let before = messages.len();
        let mut dropped_count = 0usize;
        let mut stripped_call_count = 0usize;
        // Index into `kept` of the most recent assistant message that
        // emitted tool_calls — used to strip *unanswered* tool_uses
        // (see `strip_unanswered_tool_calls` below).
        let mut last_assistant_idx: Option<usize> = None;
        let mut kept: Vec<Message> = Vec::with_capacity(messages.len());

        for msg in messages {
            match msg.role {
                MessageRole::Assistant => {
                    // Before resetting the pairing window, strip tool_uses
                    // from the previous assistant that never received a
                    // tool result. Strict providers (MiniMax-M3) 400 with
                    // `(2013) tool call result does not follow tool call`
                    // not only on stray tool results but also on assistant
                    // tool_uses left unanswered (a stream that died
                    // mid-tool-call persists a half-assistant, and the next
                    // request replays it).
                    stripped_call_count += Self::strip_unanswered_tool_calls(
                        &mut kept,
                        last_assistant_idx,
                        &pending_call_ids,
                        &consumed,
                    );
                    // A new assistant message (with or without tool_calls)
                    // resets the pairing window. Its own tool_calls repopulate
                    // the pending set; any tools from prior cycles that
                    // haven't been consumed are considered abandoned.
                    pending_call_ids.clear();
                    consumed.clear();
                    pairing_broken = false;
                    if let Some(tcs) = msg.tool_calls.as_ref() {
                        for tc in tcs {
                            if !tc.id.is_empty() {
                                pending_call_ids.insert(tc.id.clone());
                            }
                        }
                    }
                    kept.push(msg);
                    last_assistant_idx = Some(kept.len() - 1);
                }
                MessageRole::Tool => {
                    let cid_opt = msg
                        .tool_call_id
                        .as_ref()
                        .filter(|s| !s.is_empty())
                        .cloned();
                    let drop_reason: Option<&'static str> = match cid_opt {
                        None => Some("missing tool_call_id"),
                        Some(ref cid)
                            if pending_call_ids.is_empty()
                                || !pending_call_ids.contains(cid) =>
                        {
                            Some("tool_call_id not in pending assistant tool_calls")
                        }
                        Some(ref cid) if consumed.contains(cid) => {
                            Some("duplicate tool_call_id within same assistant pairing")
                        }
                        Some(_) if pairing_broken => {
                            Some("pairing broken by intervening non-tool/assistant message")
                        }
                        _ => None,
                    };
                    if let Some(reason) = drop_reason {
                        dropped_count += 1;
                        tracing::debug!(
                            tool_call_id = ?msg.tool_call_id,
                            reason = reason,
                            "dropping tool message — fails strict pairing for MiniMax-style providers"
                        );
                        continue;
                    }
                    if let Some(cid) = cid_opt {
                        consumed.insert(cid);
                    }
                    kept.push(msg);
                }
                _ => {
                    // user / system / any other role. By itself it does NOT
                    // clear pending — but it DOES mark the prior pairing as
                    // broken, so any subsequent tool message that hasn't
                    // already been consumed is dropped. The pairing is
                    // restored only when a fresh assistant(tool_calls)
                    // arrives.
                    pairing_broken = true;
                    kept.push(msg);
                }
            }
        }

        // Array end: strip tool_uses from the final assistant that never
        // got their results (same reasoning as the per-assistant pass above).
        stripped_call_count += Self::strip_unanswered_tool_calls(
            &mut kept,
            last_assistant_idx,
            &pending_call_ids,
            &consumed,
        );

        if dropped_count > 0 || stripped_call_count > 0 {
            tracing::warn!(
                dropped = dropped_count,
                stripped = stripped_call_count,
                total = before,
                kept = kept.len(),
                "strict-pairing pass: dropped tool messages whose tool_call_id \
                 doesn't immediately follow a matching assistant tool_use, and \
                 stripped assistant tool_uses that never received a result \
                 (prevents 400 'tool call result does not follow tool call (2013)' \
                 on MiniMax-M3 and similar strict providers)"
            );
        }
        kept
    }

    /// Remove `tool_call`s from the most recent assistant message whose IDs
    /// are still pending but were never consumed by a `tool` result message.
    ///
    /// Strict providers (MiniMax-M3) reject *unanswered* tool_uses the same
    /// way they reject stray tool results — the pairing must be complete in
    /// both directions. Half-assistant messages (a stream interrupted while
    /// the model was emitting `tool_calls`, persisted before the tools ran)
    /// replay on the next request and trip `(2013) tool call result does not
    /// follow tool call`. Stripping the unanswered calls keeps the request
    /// valid: the assistant keeps whatever prose it produced, the dangling
    /// call vanishes, and the next turn re-invokes the tool if needed.
    ///
    /// Returns the number of stripped calls (0 when there is nothing to do).
    fn strip_unanswered_tool_calls(
        kept: &mut [Message],
        last_assistant_idx: Option<usize>,
        pending_call_ids: &std::collections::HashSet<String>,
        consumed: &std::collections::HashSet<String>,
    ) -> usize {
        let Some(ai) = last_assistant_idx else {
            return 0;
        };
        let unanswered: Vec<String> = pending_call_ids
            .iter()
            .filter(|id| !consumed.contains(*id))
            .cloned()
            .collect();
        if unanswered.is_empty() {
            return 0;
        }
        let Some(tcs) = kept[ai].tool_calls.as_mut() else {
            return 0;
        };
        let before = tcs.len();
        tcs.retain(|tc| !unanswered.contains(&tc.id));
        let stripped = before - tcs.len();
        if tcs.is_empty() {
            kept[ai].tool_calls = None;
        }
        if stripped > 0 {
            tracing::debug!(
                assistant_idx = ai,
                stripped = stripped,
                "stripping unanswered assistant tool_calls (no tool result ever arrived)"
            );
        }
        stripped
    }

    /// Merge consecutive `assistant` messages into one.
    ///
    /// Pass 1 (`enforce_strict_tool_pairing`) may drop tool messages whose
    /// pairing was broken, which can leave two assistant messages adjacent
    /// in the final array (assistant(tool_use) whose results were dropped,
    /// directly followed by the next assistant). Some strict providers
    /// (e.g. Bedrock — see `prompt_assembly::messages_for_api_call`, which
    /// already merges consecutive *user* messages) reject consecutive
    /// same-role messages outright. Merging here is the mirror treatment
    /// for the assistant side and is safe: two adjacent assistants have no
    /// tool results between them by definition, so concatenating content
    /// and tool_calls preserves semantics.
    fn merge_adjacent_assistant_messages(messages: Vec<Message>) -> Vec<Message> {
        let mut merged: Vec<Message> = Vec::with_capacity(messages.len());
        let mut merged_count = 0usize;
        for msg in messages {
            if msg.role == MessageRole::Assistant {
                if let Some(last) = merged.last_mut() {
                    if last.role == MessageRole::Assistant {
                        // Concatenate content (if the tail one has any).
                        if let Some(ref c) = msg.content {
                            if !c.is_empty() {
                                if let Some(ref mut lc) = last.content {
                                    lc.push('\n');
                                    lc.push_str(c);
                                } else {
                                    last.content = Some(c.clone());
                                }
                            }
                        }
                        // Append tail tool_calls to the head one.
                        if let Some(ref tcs) = msg.tool_calls {
                            if let Some(ref mut ltc) = last.tool_calls {
                                ltc.extend(tcs.clone());
                            } else {
                                last.tool_calls = Some(tcs.clone());
                            }
                        }
                        // Carry content_parts if the head has none yet.
                        if last.content_parts.is_none() {
                            last.content_parts = msg.content_parts.clone();
                        }
                        merged_count += 1;
                        continue;
                    }
                }
            }
            merged.push(msg);
        }
        if merged_count > 0 {
            tracing::warn!(
                merged = merged_count,
                "strict-api pass: merged adjacent assistant messages \
                 (prevents consecutive same-role rejection)"
            );
        }
        merged
    }

    /// Drop `tool` role messages whose `tool_call_id` doesn't match any
    /// surviving assistant `tool_call.id`. See Pass 0 inside
    /// `sanitize_messages_for_strict_api` for the full rationale (TL;DR:
    /// strict providers like MiniMax-M3 400 with `(2013) tool call result
    /// does not follow tool call` when an orphan tool_result slips into
    /// the request body).
    fn drop_orphan_tool_results(messages: &[Message]) -> Vec<Message> {
        let mut surviving_call_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for msg in messages {
            if msg.role == MessageRole::Assistant {
                if let Some(tcs) = msg.tool_calls.as_ref() {
                    for tc in tcs {
                        if !tc.id.is_empty() {
                            surviving_call_ids.insert(tc.id.clone());
                        }
                    }
                }
            }
        }

        let before = messages.len();
        let kept: Vec<Message> = messages
            .iter()
            .filter(|m| {
                !(m.role == MessageRole::Tool
                    && m.tool_call_id
                        .as_ref()
                        .map(|cid| !surviving_call_ids.contains(cid))
                        .unwrap_or(false))
            })
            .cloned()
            .collect();
        let dropped = before - kept.len();
        if dropped > 0 {
            tracing::warn!(
                dropped = dropped,
                "dropping orphan tool_results before LLM request — no matching assistant \
                 tool_call (prevents 400 'tool call result does not follow tool call (2013)' \
                 on strict providers)"
            );
        }
        kept
    }

    fn sanitize_messages_for_strict_api(messages: &[Message], enabled: bool) -> Value {
        // Pass 0: drop orphan tool_results BEFORE the per-message
        // normalization. A `tool` role message whose `tool_call_id`
        // doesn't match any surviving assistant `tool_call.id` will be
        // rejected by strict providers (e.g. MiniMax-M3) with HTTP 400
        // `invalid params, tool call result does not follow tool call
        // (2013)` — the provider can't pair the tool_result with any
        // preceding tool_use. This happens when a prior request cycle
        // dropped a malformed tool_call (e.g. via `normalize_tool_function`'s
        // `function.is_none()` branch on line 456) but left the
        // corresponding tool_result dangling in the history. Mirrors
        // `compression::sanitize_tool_pairs` but runs on every request
        // so the orphan is fixed before going out, not only on the next
        // compression cycle.
        let messages = Self::drop_orphan_tool_results(messages);

        // Pass 1: enforce strict tool_use ↔ tool_result *adjacency*. Strict
        // providers (MiniMax-M3) reject bodies whose `tool` role messages
        // don't immediately follow the matching assistant `tool_use`,
        // independently of whether the IDs exist anywhere in history. Pass
        // 0 only filters globally-orphaned IDs; this pass also drops tool
        // messages whose pairing was broken by a non-tool, non-assistant
        // message (e.g. a `user` follow-up, or a compressed summary)
        // sitting between the assistant that emitted the tool_use and
        // the tool that returned its result. The strict provider's error
        // is `tool call result does not follow tool call (2013)`, even
        // when the IDs do match — pairing is the only thing that matters.
        let messages = Self::enforce_strict_tool_pairing(messages);

        // Pass 2: merge any adjacent assistant messages left behind by
        // Pass 1's tool-result drops. Strict providers reject consecutive
        // same-role messages (Bedrock rejects consecutive users; the
        // assistant side is the mirror case).
        let messages = Self::merge_adjacent_assistant_messages(messages);

        let mut out = Vec::with_capacity(messages.len());
        for msg in &messages {
            let mut api_msg = serde_json::to_value(msg).unwrap_or_else(|_| serde_json::json!({}));

            // Guarantee every message carries a non-null `content` field.
            // Strict OpenAI-compatible gateways (Agnes, MiniMax-M3) reject
            // ANY message whose `content` is missing or null — not just
            // assistant messages that also carry `tool_calls`. A persisted
            // history can hold a message with no text and no tool_calls
            // (e.g. a partial assistant turn left behind after the previous
            // provider's stream was aborted at its quota/limit), which the
            // `skip_serializing_if` on `Message::content` drops entirely
            // from the JSON. Default it to "" so the request validates on
            // every provider. Messages that legitimately use a multimodal
            // `content_parts` array are left untouched — the block below
            // rewrites those into `content`.
            if let Some(obj) = api_msg.as_object_mut() {
                let has_content_parts = obj
                    .get("content_parts")
                    .and_then(|v| v.as_array())
                    .map(|a| !a.is_empty())
                    .unwrap_or(false);
                let content_is_string = obj
                    .get("content")
                    .map(|v| v.is_string())
                    .unwrap_or(false);
                if !has_content_parts && !content_is_string {
                    obj.insert("content".to_string(), Value::String(String::new()));
                }
            }

            // Vision support: if content_parts is present, use it as the
            // `content` field (OpenAI/Claude/MiniMax accept array here).
            // Remove the `content_parts` key from the JSON since it's not
            // a standard API field — only `content` (string or array) is.
            let content_parts_clone = api_msg.get("content_parts").cloned();
            if let Some(ref parts) = content_parts_clone {
                if let Some(arr) = parts.as_array() {
                    if !arr.is_empty() {
                        if let Some(obj) = api_msg.as_object_mut() {
                            obj.insert("content".to_string(), parts.clone());
                            obj.remove("content_parts");
                        }
                    }
                }
            }

            if let Some(tool_calls) = api_msg.get_mut("tool_calls").and_then(|v| v.as_array_mut()) {
                let mut rebuilt_tool_calls: Vec<Value> = Vec::with_capacity(tool_calls.len());
                for tc in tool_calls.iter() {
                    if let Some(obj) = tc.as_object() {
                            let id = obj.get("id").cloned();
                            let function = obj
                                .get("function")
                                .cloned()
                                .map(Self::normalize_tool_function)
                                .or_else(|| {
                                let name = obj.get("name").and_then(|v| v.as_str())?;
                                let arguments = obj
                                    .get("arguments")
                                    .map(Self::coerce_arguments_to_json_string)
                                    .unwrap_or_else(|| "{}".to_string());
                                Some(serde_json::json!({
                                    "name": name,
                                    "arguments": arguments,
                                }))
                            });
                        // Defensive: drop malformed legacy tool_call items that cannot be
                        // converted into OpenAI-compatible {"function": {...}} shape.
                        if function.is_none() {
                            continue;
                        }
                        // Also drop tool_calls whose function name is empty/missing —
                        // stream_collect filters these on live streams, but replayed
                        // persisted messages may still carry them, and a strict provider
                        // 400s on a nameless tool_use.
                        let fn_name = function
                            .as_ref()
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("");
                        if fn_name.is_empty() {
                            tracing::debug!(
                                tool_call_id = ?id,
                                "dropping tool_call with empty function name (legacy persisted message)"
                            );
                            continue;
                        }
                        let mut stripped = serde_json::Map::new();
                        if let Some(v) = id {
                            stripped.insert("id".to_string(), v);
                        }
                        stripped.insert(
                            "type".to_string(),
                            obj.get("type")
                                .cloned()
                                .unwrap_or_else(|| Value::String("function".to_string())),
                        );
                        if let Some(v) = function {
                            stripped.insert("function".to_string(), v);
                        }
                        if enabled {
                            rebuilt_tool_calls.push(Value::Object(stripped));
                        } else {
                            // Always keep an OpenAI-compatible representation to avoid provider
                            // deserialization errors when replaying tool call messages.
                            let mut merged = obj.clone();
                            if let Some(v) = stripped.get("function").cloned() {
                                merged.insert("function".to_string(), v);
                            }
                            merged.insert(
                                "type".to_string(),
                                stripped
                                    .get("type")
                                    .cloned()
                                    .unwrap_or_else(|| Value::String("function".to_string())),
                            );
                            rebuilt_tool_calls.push(Value::Object(merged));
                        }
                    }
                }
                if let Some(api_obj) = api_msg.as_object_mut() {
                    if rebuilt_tool_calls.is_empty() {
                        api_obj.remove("tool_calls");
                    } else {
                        api_obj.insert("tool_calls".to_string(), Value::Array(rebuilt_tool_calls));
                    }
                }
            }
            out.push(api_msg);
        }
        Value::Array(out)
    }

    fn build_request(
        &self,
        client: &Client,
        url: &str,
        api_key: &str,
        body: &Value,
    ) -> reqwest::RequestBuilder {
        let mut req = client
            .post(url)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(body);
        for (key, value) in &self.extra_headers {
            req = req.header(key.as_str(), value.as_str());
        }
        req
    }

    async fn send_with_dead_connection_recovery(
        &self,
        url: &str,
        api_key: &str,
        body: &Value,
    ) -> Result<reqwest::Response, AgentError> {
        self.maybe_refresh_stale_client(url).await;
        let client = self.current_client();
        match self.build_request(&client, url, api_key, body).send().await {
            Ok(resp) => Ok(resp),
            Err(e) => {
                if !Self::is_connection_recoverable(&e) {
                    return Err(AgentError::LlmApi(format!("HTTP request failed: {e}")));
                }
                self.refresh_client(&format!("recoverable transport error: {e}"));
                let retry_client = self.current_client();
                self.build_request(&retry_client, url, api_key, body)
                    .send()
                    .await
                    .map_err(|e2| {
                        AgentError::LlmApi(format!(
                            "HTTP request failed after reconnect retry: {e2}"
                        ))
                    })
            }
        }
    }
}

#[async_trait]
impl LlmProvider for GenericProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        self.check_rate_limit().await;

        let effective_model = model.unwrap_or(&self.model);
        let api_key = self.effective_api_key();
        let strict_tool_sanitize = Self::should_sanitize_tool_calls(extra_body);
        let api_messages = Self::sanitize_messages_for_strict_api(messages, strict_tool_sanitize);

        let mut body = serde_json::json!({
            "model": effective_model,
            "messages": api_messages,
        });

        if let Some(mt) = max_tokens {
            body["max_tokens"] = serde_json::json!(mt);
        }
        if let Some(temp) = temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if !tools.is_empty() {
            body["tools"] = self.normalize_tools_payload(tools, extra_body);
        }
        if let Some(eb) = extra_body {
            if let Value::Object(map) = eb {
                for (k, v) in map {
                    body[k] = v.clone();
                }
            }
        }
        self.apply_runtime_hints(&mut body, messages, extra_body);

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let resp = self
            .send_with_dead_connection_recovery(&url, &api_key, &body)
            .await?;

        self.update_rate_limit(resp.headers());

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp
                .text()
                .await
                .unwrap_or_else(|_| "<no body>".to_string());
            return Err(AgentError::LlmApi(format!(
                "API error {status}: {body_text}"
            )));
        }

        let resp_json: Value = resp
            .json()
            .await
            .map_err(|e| AgentError::LlmApi(format!("Failed to parse response: {e}")))?;

        parse_openai_response(&resp_json)
    }

    fn chat_completion_stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
        let provider = self.clone();
        let messages = messages.to_vec();
        let tools = tools.to_vec();
        let model = model.map(|s| s.to_string());
        let extra_body = extra_body.cloned();

        async_stream::stream! {
            provider.check_rate_limit().await;

            let effective_model = model.as_deref().unwrap_or(&provider.model);
            let api_key = provider.effective_api_key();
            let strict_tool_sanitize = GenericProvider::should_sanitize_tool_calls(extra_body.as_ref());
            let api_messages =
                GenericProvider::sanitize_messages_for_strict_api(&messages, strict_tool_sanitize);

            let mut body = serde_json::json!({
                "model": effective_model,
                "messages": api_messages,
                "stream": true,
            });

            if let Some(mt) = max_tokens {
                body["max_tokens"] = serde_json::json!(mt);
            }
            if let Some(temp) = temperature {
                body["temperature"] = serde_json::json!(temp);
            }
            if !tools.is_empty() {
                body["tools"] = provider.normalize_tools_payload(&tools, extra_body.as_ref());
            }
            if let Some(ref eb) = extra_body {
                if let Value::Object(map) = eb {
                    for (k, v) in map {
                        body[k] = v.clone();
                    }
                }
            }
            provider.apply_runtime_hints(&mut body, &messages, extra_body.as_ref());
            // Request usage in the final streaming chunk
            body["stream_options"] = serde_json::json!({"include_usage": true});

            let url = format!("{}/chat/completions", provider.base_url.trim_end_matches('/'));

            let resp = match provider
                .send_with_dead_connection_recovery(&url, &api_key, &body)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };

            provider.update_rate_limit(resp.headers());

            if !resp.status().is_success() {
                let status = resp.status();
                let body_text = resp.text().await.unwrap_or_else(|_| "<no body>".to_string());
                yield Err(AgentError::LlmApi(format!("API error {status}: {body_text}")));
                return;
            }

            // Read the SSE byte stream line by line
            let mut byte_stream = resp.bytes_stream();
            let mut buffer = String::new();

            while let Some(chunk_result) = byte_stream.next().await {
                let chunk_bytes = match chunk_result {
                    Ok(b) => b,
                    Err(e) => {
                        yield Err(AgentError::LlmApi(format!("Stream read error: {e}")));
                        return;
                    }
                };

                buffer.push_str(&String::from_utf8_lossy(&chunk_bytes).replace("\r\n", "\n"));

                // Process complete SSE events (separated by double newlines)
                while let Some(event_end) = buffer.find("\n\n") {
                    let event_block = buffer[..event_end].to_string();
                    buffer = buffer[event_end + 2..].to_string();

                    for line in event_block.lines() {
                        let line = line.trim();
                        if line.is_empty() || line.starts_with(':') {
                            continue;
                        }
                        if let Some(data) = line.strip_prefix("data: ") {
                            let data = data.trim();
                            if data == "[DONE]" {
                                // Stream finished
                                return;
                            }
                            match serde_json::from_str::<Value>(data) {
                                Ok(json) => {
                                    if let Some(chunk) = parse_sse_chunk(&json) {
                                        for c in split_chunk_for_char_stream(chunk) {
                                            yield Ok(c);
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("Failed to parse SSE data: {e}");
                                }
                            }
                        }
                    }
                }
            }

            // Process any remaining data in the buffer
            if !buffer.trim().is_empty() {
                for line in buffer.lines() {
                    let line = line.trim();
                    if let Some(data) = line.strip_prefix("data: ") {
                        let data = data.trim();
                        if data == "[DONE]" {
                            return;
                        }
                        if let Ok(json) = serde_json::from_str::<Value>(data) {
                            if let Some(chunk) = parse_sse_chunk(&json) {
                                for c in split_chunk_for_char_stream(chunk) {
                                    yield Ok(c);
                                }
                            }
                        }
                    }
                }
            }
        }
        .boxed()
    }
}

// ---------------------------------------------------------------------------
// OpenAiProvider
// ---------------------------------------------------------------------------

/// OpenAI API provider.
#[derive(Debug, Clone)]
pub struct OpenAiProvider {
    inner: GenericProvider,
}

impl OpenAiProvider {
    /// Create a new OpenAI provider with the given API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            inner: GenericProvider::new("https://api.openai.com/v1", api_key, "gpt-4o"),
        }
    }

    /// Use a custom base URL (e.g., for Azure OpenAI).
    pub fn with_base_url(self, base_url: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_base_url(base_url),
        }
    }

    /// Set the default model.
    pub fn with_model(self, model: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_model(model),
        }
    }

    /// Attach a credential pool for API key rotation.
    pub fn with_credential_pool(self, pool: Arc<CredentialPool>) -> Self {
        Self {
            inner: self.inner.with_credential_pool(pool),
        }
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        self.inner
            .chat_completion(messages, tools, max_tokens, temperature, model, extra_body)
            .await
    }

    fn chat_completion_stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
        self.inner.chat_completion_stream(
            messages,
            tools,
            max_tokens,
            temperature,
            model,
            extra_body,
        )
    }
}

// ---------------------------------------------------------------------------
// AnthropicProvider
// ---------------------------------------------------------------------------

/// Anthropic API provider with native Messages API support.
/// Uses Anthropic's own message format rather than OpenAI-compatible format:
/// - System message goes in `system` parameter, not in messages array
/// - Uses `x-api-key` header instead of `Authorization: Bearer`
/// - Content blocks use array format with typed blocks
/// - Tool use returns `type: "tool_use"` content blocks
#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    /// Base URL for the Anthropic API.
    pub base_url: String,
    /// API key for authentication.
    pub api_key: String,
    /// Default model identifier.
    pub model: String,
    /// HTTP client. Uses RwLock for concurrent reads (same as GenericProvider).
    client: Arc<RwLock<Client>>,
    /// Last time we rebuilt the client transport.
    client_refreshed_at: Arc<PLMutex<Instant>>,
    /// Anthropic API version header.
    pub api_version: String,
    /// Optional rate limit tracker.
    pub rate_limiter: Option<Arc<RateLimitTracker>>,
    /// Optional credential pool.
    pub credential_pool: Option<Arc<CredentialPool>>,
}

impl AnthropicProvider {
    /// Create a new Anthropic provider with the given API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            base_url: "https://api.anthropic.com".to_string(),
            api_key: api_key.into(),
            model: "claude-3-5-sonnet-20241022".to_string(),
            client: Arc::new(RwLock::new(build_llm_http_client())),
            client_refreshed_at: Arc::new(PLMutex::new(Instant::now())),
            api_version: "2023-06-01".to_string(),
            rate_limiter: None,
            credential_pool: None,
        }
    }

    /// Set the default model.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Set a custom base URL.
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Attach a rate limit tracker.
    pub fn with_rate_limiter(mut self, tracker: Arc<RateLimitTracker>) -> Self {
        self.rate_limiter = Some(tracker);
        self
    }

    /// Attach a credential pool.
    pub fn with_credential_pool(mut self, pool: Arc<CredentialPool>) -> Self {
        self.credential_pool = Some(pool);
        self
    }

    fn effective_api_key(&self) -> String {
        if let Some(ref pool) = self.credential_pool {
            pool.get_key()
        } else {
            self.api_key.clone()
        }
    }

    async fn check_rate_limit(&self) {
        if let Some(ref tracker) = self.rate_limiter {
            if let Some(wait_duration) = tracker.should_wait() {
                tracing::info!("Rate limited, waiting {:?}", wait_duration);
                tokio::time::sleep(wait_duration).await;
            }
        }
    }

    fn update_rate_limit(&self, headers: &reqwest::header::HeaderMap) {
        if let Some(ref tracker) = self.rate_limiter {
            tracker.update_from_headers(headers);
        }
    }

    fn current_client(&self) -> Client {
        self.client.read().clone()
    }

    fn refresh_client(&self, reason: &str) {
        tracing::warn!("rebuilding anthropic HTTP client: {}", reason);
        *self.client.write() = build_llm_http_client();
        *self.client_refreshed_at.lock() = Instant::now();
    }

    async fn maybe_refresh_stale_client(&self, probe_url: &str) {
        const STALE_CLIENT_REFRESH_SECS: u64 = 300;
        let stale_after = Duration::from_secs(STALE_CLIENT_REFRESH_SECS);
        let should_refresh = self.client_refreshed_at.lock().elapsed() >= stale_after;
        if !should_refresh {
            return;
        }
        let probe_client = self.current_client();
        match probe_client
            .get(probe_url)
            .timeout(Duration::from_secs(3))
            .send()
            .await
        {
            Ok(_) => {
                *self.client_refreshed_at.lock() = Instant::now();
            }
            Err(e) => {
                if GenericProvider::is_connection_recoverable(&e) {
                    self.refresh_client(&format!("stale connection probe failed: {e}"));
                } else {
                    *self.client_refreshed_at.lock() = Instant::now();
                }
            }
        }
    }

    fn build_request(
        &self,
        client: &Client,
        url: &str,
        api_key: &str,
        body: &Value,
    ) -> reqwest::RequestBuilder {
        client
            .post(url)
            .header("x-api-key", api_key)
            .header("anthropic-version", &self.api_version)
            .header("Content-Type", "application/json")
            .json(body)
    }

    async fn send_with_dead_connection_recovery(
        &self,
        url: &str,
        api_key: &str,
        body: &Value,
    ) -> Result<reqwest::Response, AgentError> {
        self.maybe_refresh_stale_client(url).await;
        let client = self.current_client();
        match self.build_request(&client, url, api_key, body).send().await {
            Ok(resp) => Ok(resp),
            Err(e) => {
                if !GenericProvider::is_connection_recoverable(&e) {
                    return Err(AgentError::LlmApi(format!("HTTP request failed: {e}")));
                }
                self.refresh_client(&format!("recoverable transport error: {e}"));
                let retry_client = self.current_client();
                self.build_request(&retry_client, url, api_key, body)
                    .send()
                    .await
                    .map_err(|e2| {
                        AgentError::LlmApi(format!(
                            "HTTP request failed after reconnect retry: {e2}"
                        ))
                    })
            }
        }
    }

    /// Convert internal messages to Anthropic format, extracting system message.
    /// The returned `(system_value, anthropic_messages)` tuple's `system_value`
    /// is a `serde_json::Value` rather than `String` — when the aggregated
    /// system content carries `cache_control`, we emit it as an array of text
    /// blocks (Anthropic API requirement) with the marker on the first block;
    /// otherwise we fall back to the legacy plain string.
    fn convert_messages(messages: &[Message]) -> (Option<Value>, Vec<Value>) {
        let mut system_text: Option<String> = None;
        let mut system_cache_control: Option<CacheControl> = None;
        let mut anthropic_messages: Vec<Value> = Vec::new();

        for msg in messages {
            match msg.role {
                MessageRole::System => {
                    // Anthropic: system goes in a separate `system` parameter
                    let content = msg.content.as_deref().unwrap_or("");
                    system_text = Some(match system_text {
                        Some(existing) => format!("{existing}\n\n{content}"),
                        None => content.to_string(),
                    });
                    // Capture cache_control from the first system message that
                    // sets one — multiple system messages get concatenated but
                    // only the marker on the first chunk matters for the
                    // provider's prefix-cache break point.
                    if system_cache_control.is_none() {
                        system_cache_control = msg.cache_control.clone();
                    }
                }
                MessageRole::User => {
                    let mut content_blocks = Vec::new();
                    if let Some(ref text) = msg.content {
                        let mut block = serde_json::json!({"type": "text", "text": text});
                        if let Some(ref cc) = msg.cache_control {
                            block["cache_control"] = serde_json::json!({"type": format!("{:?}", cc.cache_type).to_lowercase()});
                        }
                        content_blocks.push(block);
                    }
                    anthropic_messages.push(serde_json::json!({
                        "role": "user",
                        "content": content_blocks,
                    }));
                }
                MessageRole::Assistant => {
                    let mut content_blocks = Vec::new();
                    if let Some(ref text) = msg.content {
                        if !text.is_empty() {
                            content_blocks.push(serde_json::json!({"type": "text", "text": text}));
                        }
                    }
                    // Convert tool_calls to Anthropic tool_use blocks
                    if let Some(ref tool_calls) = msg.tool_calls {
                        for tc in tool_calls {
                            let input: Value = serde_json::from_str(&tc.function.arguments)
                                .unwrap_or(serde_json::json!({}));
                            content_blocks.push(serde_json::json!({
                                "type": "tool_use",
                                "id": tc.id,
                                "name": tc.function.name,
                                "input": input,
                            }));
                        }
                    }
                    if !content_blocks.is_empty() {
                        anthropic_messages.push(serde_json::json!({
                            "role": "assistant",
                            "content": content_blocks,
                        }));
                    }
                }
                MessageRole::Tool => {
                    // Anthropic: tool results go as user messages with tool_result content blocks
                    let content_blocks = vec![serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": msg.tool_call_id.as_deref().unwrap_or(""),
                        "content": msg.content.as_deref().unwrap_or(""),
                    })];
                    anthropic_messages.push(serde_json::json!({
                        "role": "user",
                        "content": content_blocks,
                    }));
                }
            }
        }

        let system_value = system_text.map(|text| match system_cache_control {
            Some(cc) => {
                let mut block = serde_json::json!({"type": "text", "text": text});
                block["cache_control"] = serde_json::json!({
                    "type": format!("{:?}", cc.cache_type).to_lowercase()
                });
                Value::Array(vec![block])
            }
            None => Value::String(text),
        });

        (system_value, anthropic_messages)
    }

    /// Convert tool schemas to Anthropic tool format.
    fn convert_tools(tools: &[ToolSchema]) -> Vec<Value> {
        tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            })
            .collect()
    }

    /// Parse an Anthropic Messages API response into LlmResponse.
    fn parse_response(json: &Value) -> Result<LlmResponse, AgentError> {
        let mut content_text = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();

        if let Some(content_arr) = json.get("content").and_then(|c| c.as_array()) {
            for block in content_arr {
                let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match block_type {
                    "text" => {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            if !content_text.is_empty() {
                                content_text.push('\n');
                            }
                            content_text.push_str(text);
                        }
                    }
                    "tool_use" => {
                        let id = block
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = block
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string();
                        let input = block.get("input").cloned().unwrap_or(serde_json::json!({}));
                        let arguments =
                            serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
                        tool_calls.push(ToolCall {
                            id,
                            function: FunctionCall { name, arguments },
                        });
                    }
                    _ => {}
                }
            }
        }

        let usage = json.get("usage").map(UsageStats::from_json);

        let stop_reason = json
            .get("stop_reason")
            .and_then(|s| s.as_str())
            .map(|s| match s {
                "end_turn" => "stop".to_string(),
                "tool_use" => "tool_calls".to_string(),
                other => other.to_string(),
            });

        let model = json
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown")
            .to_string();

        let message = Message {
            role: MessageRole::Assistant,
            content: Some(content_text),
            content_parts: None,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
            execution_timeline: None,
            ephemeral: false,
        };

        Ok(LlmResponse {
            message,
            usage,
            model,
            finish_reason: stop_reason,
        })
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        self.check_rate_limit().await;

        let effective_model = model.unwrap_or(&self.model);
        let api_key = self.effective_api_key();
        let (system_text, anthropic_messages) = Self::convert_messages(messages);

        let mut body = serde_json::json!({
            "model": effective_model,
            "messages": anthropic_messages,
            "max_tokens": max_tokens.unwrap_or(4096),
        });

        if let Some(sys) = system_text {
            body["system"] = sys;
        }
        if let Some(temp) = temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if !tools.is_empty() {
            body["tools"] = serde_json::json!(Self::convert_tools(tools));
        }
        if let Some(eb) = extra_body {
            if let Value::Object(map) = eb {
                for (k, v) in map {
                    body[k] = v.clone();
                }
            }
        }

        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let resp = self
            .send_with_dead_connection_recovery(&url, &api_key, &body)
            .await?;

        self.update_rate_limit(resp.headers());

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp
                .text()
                .await
                .unwrap_or_else(|_| "<no body>".to_string());
            return Err(AgentError::LlmApi(format!(
                "API error {status}: {body_text}"
            )));
        }

        let resp_json: Value = resp
            .json()
            .await
            .map_err(|e| AgentError::LlmApi(format!("Failed to parse response: {e}")))?;

        Self::parse_response(&resp_json)
    }

    fn chat_completion_stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
        let provider = self.clone();
        let messages = messages.to_vec();
        let tools = tools.to_vec();
        let model = model.map(|s| s.to_string());
        let extra_body = extra_body.cloned();

        async_stream::stream! {
            provider.check_rate_limit().await;

            let effective_model = model.as_deref().unwrap_or(&provider.model);
            let api_key = provider.effective_api_key();
            let (system_text, anthropic_messages) = AnthropicProvider::convert_messages(&messages);

            let mut body = serde_json::json!({
                "model": effective_model,
                "messages": anthropic_messages,
                "max_tokens": max_tokens.unwrap_or(4096),
                "stream": true,
            });

            if let Some(sys) = system_text {
                body["system"] = sys;
            }
            if let Some(temp) = temperature {
                body["temperature"] = serde_json::json!(temp);
            }
            if !tools.is_empty() {
                body["tools"] = serde_json::json!(AnthropicProvider::convert_tools(&tools));
            }
            if let Some(ref eb) = extra_body {
                if let Value::Object(map) = eb {
                    for (k, v) in map {
                        body[k] = v.clone();
                    }
                }
            }

            let url = format!("{}/v1/messages", provider.base_url.trim_end_matches('/'));

            let resp = match provider.send_with_dead_connection_recovery(&url, &api_key, &body).await {
                Ok(r) => r,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };

            provider.update_rate_limit(resp.headers());

            if !resp.status().is_success() {
                let status = resp.status();
                let body_text = resp.text().await.unwrap_or_else(|_| "<no body>".to_string());
                yield Err(AgentError::LlmApi(format!("API error {status}: {body_text}")));
                return;
            }

            let mut byte_stream = resp.bytes_stream();
            let mut buffer = String::new();
            // Track current tool_use block index for delta accumulation
            let mut current_tool_index: u32 = 0;

            while let Some(chunk_result) = byte_stream.next().await {
                let chunk_bytes = match chunk_result {
                    Ok(b) => b,
                    Err(e) => {
                        yield Err(AgentError::LlmApi(format!("Stream read error: {e}")));
                        return;
                    }
                };

                buffer.push_str(&String::from_utf8_lossy(&chunk_bytes));

                while let Some(event_end) = buffer.find("\n\n") {
                    let event_block = buffer[..event_end].to_string();
                    buffer = buffer[event_end + 2..].to_string();

                    let mut event_type = String::new();
                    let mut event_data = String::new();

                    for line in event_block.lines() {
                        let line = line.trim();
                        if let Some(et) = line.strip_prefix("event: ") {
                            event_type = et.trim().to_string();
                        } else if let Some(d) = line.strip_prefix("data: ") {
                            event_data = d.trim().to_string();
                        }
                    }

                    if event_data.is_empty() {
                        continue;
                    }

                    let json: Value = match serde_json::from_str(&event_data) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };

                    match event_type.as_str() {
                        "content_block_start" => {
                            let block = json.get("content_block").unwrap_or(&json);
                            let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            if block_type == "tool_use" {
                                let id = block.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                                let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                                let idx = json.get("index").and_then(|i| i.as_u64()).unwrap_or(current_tool_index as u64) as u32;
                                current_tool_index = idx;
                                yield Ok(StreamChunk {
                                    delta: Some(StreamDelta {
                                        content: None,
                                        tool_calls: Some(vec![ToolCallDelta {
                                            index: idx,
                                            id: Some(id),
                                            function: Some(FunctionCallDelta {
                                                name: Some(name),
                                                arguments: None,
                                            }),
                                        }]),
                                        extra: None,
                                    }),
                                    finish_reason: None,
                                    usage: None,
                                });
                            }
                        }
                        "content_block_delta" => {
                            let delta = json.get("delta").unwrap_or(&json);
                            let delta_type = delta.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            match delta_type {
                                "text_delta" => {
                                    let text = delta.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
                                    yield Ok(StreamChunk {
                                        delta: Some(StreamDelta {
                                            content: Some(text),
                                            tool_calls: None,
                                            extra: None,
                                        }),
                                        finish_reason: None,
                                        usage: None,
                                    });
                                }
                                "input_json_delta" => {
                                    let partial = delta.get("partial_json").and_then(|p| p.as_str()).unwrap_or("").to_string();
                                    yield Ok(StreamChunk {
                                        delta: Some(StreamDelta {
                                            content: None,
                                            tool_calls: Some(vec![ToolCallDelta {
                                                index: current_tool_index,
                                                id: None,
                                                function: Some(FunctionCallDelta {
                                                    name: None,
                                                    arguments: Some(partial),
                                                }),
                                            }]),
                                            extra: None,
                                        }),
                                        finish_reason: None,
                                        usage: None,
                                    });
                                }
                                _ => {}
                            }
                        }
                        "message_delta" => {
                            let stop_reason = json
                                .get("delta")
                                .and_then(|d| d.get("stop_reason"))
                                .and_then(|s| s.as_str())
                                .map(|s| match s {
                                    "end_turn" => "stop".to_string(),
                                    "tool_use" => "tool_calls".to_string(),
                                    other => other.to_string(),
                                });
                            let usage = json.get("usage").map(|u| {
                                let output = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                                UsageStats {
                                    prompt_tokens: 0,
                                    completion_tokens: output,
                                    total_tokens: output,
                                    estimated_cost: None,
                                    cache_read_tokens: u.get("cache_read_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                                    cache_creation_tokens: u.get("cache_creation_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                                    reasoning_tokens: 0,
                                }
                            });
                            yield Ok(StreamChunk {
                                delta: None,
                                finish_reason: stop_reason,
                                usage,
                            });
                        }
                        "message_start" => {
                            // Extract usage from the initial message
                            let usage = json.get("message").and_then(|m| m.get("usage")).map(|u| {
                                let input = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                                UsageStats {
                                    prompt_tokens: input,
                                    completion_tokens: 0,
                                    total_tokens: input,
                                    estimated_cost: None,
                                    cache_read_tokens: u.get("cache_read_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                                    cache_creation_tokens: u.get("cache_creation_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                                    reasoning_tokens: 0,
                                }
                            });
                            if let Some(u) = usage {
                                yield Ok(StreamChunk {
                                    delta: None,
                                    finish_reason: None,
                                    usage: Some(u),
                                });
                            }
                        }
                        "message_stop" => {
                            return;
                        }
                        _ => {}
                    }
                }
            }
        }
        .boxed()
    }
}

// ---------------------------------------------------------------------------
// OpenRouterProvider
// ---------------------------------------------------------------------------

/// OpenRouter API provider with support for OpenRouter-specific parameters.
/// Adds:
/// - `HTTP-Referer` and `X-Title` headers (required by OpenRouter)
/// - Support for `transforms`, `provider` preferences, `route` in extra_body
/// - Parsing of `reasoning_details` array from responses
/// - `reasoning_content` extraction
#[derive(Debug, Clone)]
pub struct OpenRouterProvider {
    inner: GenericProvider,
    /// HTTP-Referer header value (required by OpenRouter).
    pub http_referer: Option<String>,
    /// X-Title header value (required by OpenRouter).
    pub x_title: Option<String>,
    /// Preferred provider order (e.g. ["DeepInfra", "Together"]).
    pub provider_order: Option<Vec<String>>,
}

impl OpenRouterProvider {
    /// Create a new OpenRouter provider with the given API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            inner: GenericProvider::new("https://openrouter.ai/api/v1", api_key, "openai/gpt-4o"),
            http_referer: None,
            x_title: None,
            provider_order: None,
        }
    }

    /// Set the default model.
    pub fn with_model(self, model: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_model(model),
            ..self
        }
    }

    /// Set a custom base URL (OpenAI-compatible OpenRouter proxy).
    pub fn with_base_url(self, base_url: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_base_url(base_url),
            ..self
        }
    }

    /// Set the HTTP-Referer header (required by OpenRouter).
    pub fn with_http_referer(mut self, referer: impl Into<String>) -> Self {
        self.http_referer = Some(referer.into());
        self
    }

    /// Set the X-Title header (required by OpenRouter).
    pub fn with_x_title(mut self, title: impl Into<String>) -> Self {
        self.x_title = Some(title.into());
        self
    }

    /// Set preferred provider order for OpenRouter routing.
    /// e.g. `vec!["DeepInfra", "Together"]`
    pub fn with_provider_order(mut self, order: Vec<String>) -> Self {
        self.provider_order = Some(order);
        self
    }

    /// Attach a credential pool for API key rotation.
    pub fn with_credential_pool(self, pool: Arc<CredentialPool>) -> Self {
        Self {
            inner: self.inner.with_credential_pool(pool),
            ..self
        }
    }

    /// Build the extra headers including OpenRouter-specific ones.
    fn build_headers(&self) -> Vec<(String, String)> {
        let mut headers = self.inner.extra_headers.clone();
        if let Some(ref referer) = self.http_referer {
            headers.push(("HTTP-Referer".to_string(), referer.clone()));
        }
        if let Some(ref title) = self.x_title {
            headers.push(("X-Title".to_string(), title.clone()));
        }
        headers
    }

    /// Merge OpenRouter-specific parameters into extra_body.
    fn merge_extra_body(extra_body: Option<&Value>) -> Option<Value> {
        let mut merged = extra_body.cloned()?;
        // Internal compatibility tuning flag; don't forward unknown key upstream.
        if let Some(obj) = merged.as_object_mut() {
            obj.remove("openrouter_max_tools");
        }
        Some(merged)
    }

    /// Parse an OpenRouter response, extracting reasoning_details if present.
    fn parse_openrouter_response(json: &Value) -> Result<LlmResponse, AgentError> {
        let mut response = parse_openai_response(json)?;

        // Extract reasoning_content from various locations
        if let Some(reasoning) = crate::reasoning::parse_reasoning(json) {
            response.message.reasoning_content = Some(reasoning);
        }

        Ok(response)
    }
}

#[async_trait]
impl LlmProvider for OpenRouterProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        // Build a provider clone with OpenRouter headers
        let mut provider = self.inner.clone();
        provider.extra_headers = self.build_headers();

        let merged_extra = Self::merge_extra_body(extra_body);

        // Use GenericProvider for the actual request
        let effective_model = model.unwrap_or(&self.inner.model);
        provider.check_rate_limit().await;

        let api_key = provider.effective_api_key();
        let strict_tool_sanitize =
            GenericProvider::should_sanitize_tool_calls(merged_extra.as_ref());
        let api_messages =
            GenericProvider::sanitize_messages_for_strict_api(messages, strict_tool_sanitize);

        let mut body = serde_json::json!({
            "model": effective_model,
            "messages": api_messages,
        });

        if let Some(mt) = max_tokens {
            body["max_tokens"] = serde_json::json!(mt);
        }
        if let Some(temp) = temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if !tools.is_empty() {
            body["tools"] = provider.normalize_tools_payload(tools, merged_extra.as_ref());
        }
        if let Some(ref eb) = merged_extra {
            if let Value::Object(map) = eb {
                for (k, v) in map {
                    body[k] = v.clone();
                }
            }
        }

        // Inject OpenRouter provider preferences
        if let Some(ref order) = self.provider_order {
            if !order.is_empty() {
                body["provider"] = serde_json::json!({
                    "order": order,
                    "allow_fallbacks": false,
                });
            }
        }

        let url = format!(
            "{}/chat/completions",
            provider.base_url.trim_end_matches('/')
        );

        let resp = provider
            .send_with_dead_connection_recovery(&url, &api_key, &body)
            .await?;

        provider.update_rate_limit(resp.headers());

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp
                .text()
                .await
                .unwrap_or_else(|_| "<no body>".to_string());
            return Err(AgentError::LlmApi(format!(
                "API error {status}: {body_text}"
            )));
        }

        let resp_json: Value = resp
            .json()
            .await
            .map_err(|e| AgentError::LlmApi(format!("Failed to parse response: {e}")))?;

        Self::parse_openrouter_response(&resp_json)
    }

    fn chat_completion_stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
        // Use GenericProvider's streaming with OpenRouter headers
        let mut provider = self.inner.clone();
        provider.extra_headers = self.build_headers();
        let merged_extra = Self::merge_extra_body(extra_body);

        // Inject provider order into extra_body for streaming
        let final_extra = if let Some(ref order) = self.provider_order {
            if !order.is_empty() {
                let mut eb = merged_extra.unwrap_or_else(|| serde_json::json!({}));
                eb["provider"] = serde_json::json!({
                    "order": order,
                    "allow_fallbacks": false,
                });
                Some(eb)
            } else {
                merged_extra
            }
        } else {
            merged_extra
        };

        provider.chat_completion_stream(
            messages,
            tools,
            max_tokens,
            temperature,
            model,
            final_extra.as_ref(),
        )
    }
}

// ---------------------------------------------------------------------------
// SSE chunk parsing helpers
// ---------------------------------------------------------------------------

/// Parse a single SSE data JSON object into a StreamChunk (OpenAI format).
fn parse_sse_chunk(json: &Value) -> Option<StreamChunk> {
    // Usage may appear in the final chunk as a top-level field, even when
    // `choices` is empty (OpenAI's `stream_options.include_usage` format).
    // Extract it FIRST so we never discard usage from a choices=[] chunk.
    let usage = json.get("usage").map(|u| {
        let prompt = u
            .get("prompt_tokens")
            .and_then(|v| v.as_u64())
            .or_else(|| u.get("input_tokens").and_then(|v| v.as_u64()))
            .unwrap_or(0);
        let completion = u
            .get("completion_tokens")
            .and_then(|v| v.as_u64())
            .or_else(|| u.get("output_tokens").and_then(|v| v.as_u64()))
            .unwrap_or(0);
        UsageStats {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: u
                .get("total_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(prompt + completion),
            estimated_cost: None,
            cache_read_tokens: u
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_u64())
                .or_else(|| {
                    u.get("prompt_tokens_details")
                        .and_then(|d| d.get("cached_tokens"))
                        .and_then(|v| v.as_u64())
                })
                .unwrap_or(0),
            cache_creation_tokens: u
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            reasoning_tokens: u
                .get("reasoning_tokens")
                .and_then(|v| v.as_u64())
                .or_else(|| {
                    u.get("completion_tokens_details")
                        .and_then(|d| d.get("reasoning_tokens"))
                        .and_then(|v| v.as_u64())
                })
                .unwrap_or(0),
        }
    });

    let choices = json.get("choices").and_then(|c| c.as_array())?;

    // Empty choices with usage = usage-only final chunk (include_usage).
    // Return a StreamChunk carrying only the usage data.
    if choices.is_empty() && usage.is_some() {
        return Some(StreamChunk {
            delta: None,
            finish_reason: None,
            usage,
        });
    }

    let choice = choices.first()?;

    let delta_obj = choice.get("delta")?;

    let content = delta_obj
        .get("content")
        .and_then(|c| c.as_str())
        .map(|s| s.to_string());
    let reasoning = delta_obj
        .get("reasoning_content")
        .or_else(|| delta_obj.get("reasoning"))
        .and_then(|r| r.as_str())
        .map(|s| s.to_string());

    let tool_calls = delta_obj
        .get("tool_calls")
        .and_then(|tc| tc.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|tc| {
                    let index = tc.get("index").and_then(|i| i.as_u64())? as u32;
                    let id = tc.get("id").and_then(|i| i.as_str()).map(|s| s.to_string());
                    let function = tc.get("function").map(|f| FunctionCallDelta {
                        name: f
                            .get("name")
                            .and_then(|n| n.as_str())
                            .map(|s| s.to_string()),
                        arguments: f
                            .get("arguments")
                            .and_then(|a| a.as_str())
                            .map(|s| s.to_string()),
                    });
                    Some(ToolCallDelta {
                        index,
                        id,
                        function,
                    })
                })
                .collect::<Vec<_>>()
        });

    let delta = if content.is_some() || tool_calls.is_some() || reasoning.is_some() {
        Some(StreamDelta {
            content,
            tool_calls,
            extra: reasoning.map(|t| serde_json::json!({ "thinking": t })),
        })
    } else {
        None
    };

    let finish_reason = choice
        .get("finish_reason")
        .and_then(|f| f.as_str())
        .map(|s| s.to_string());

    Some(StreamChunk {
        delta,
        finish_reason,
        usage,
    })
}

// ---------------------------------------------------------------------------
// Per-character streaming splitter
// ---------------------------------------------------------------------------
// Some providers (notably MiniMax M2.7) emit OpenAI streaming deltas with
// **multi-character** content payloads (e.g. one `data:` line carrying
// 50+ characters at once). When the model is also reasoning inside
// `<think>...</think>`, the user experiences a "wait...wait...all at once"
// pattern: the API sends a handful of fat chunks and the UI can't render
// a smooth character stream.
// To restore the true "逐字流" feel, we split each chunk's `content` payload
// into individual characters and yield a separate `StreamChunk` per char.
// The frontend then renders the reply as a typewriter effect. The split
// is **opt-in** via the env var `HERMES_CHAR_STREAM` (default off) so the
// default behavior is unchanged for users who prefer fewer frames.

/// Whether per-character content splitting is enabled.
/// Set `HERMES_CHAR_STREAM=1` (or `true`) to enable; default is off.
pub fn is_char_stream_enabled() -> bool {
    match env_compat("HERMIX_CHAR_STREAM", "HERMES_CHAR_STREAM") {
        Some(v) => {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        }
        None => false,
    }
}

/// Split a single `StreamChunk` into multiple chunks, one per content char,
/// so that downstream consumers see true character-level deltas.
/// Only the visible `content` field is split. `tool_calls` deltas must remain
/// intact (they are partial JSON argument fragments that depend on
/// accumulation). `extra.thinking` deltas are also kept whole.
pub fn split_chunk_for_char_stream(chunk: StreamChunk) -> Vec<StreamChunk> {
    if !is_char_stream_enabled() {
        return vec![chunk];
    }
    let Some(delta) = chunk.delta.clone() else {
        return vec![chunk];
    };
    let Some(text) = delta.content.as_ref() else {
        return vec![chunk];
    };
    if text.chars().count() <= 1 {
        return vec![chunk];
    }
    // Build one chunk per character, preserving all other fields from the source.
    let mut out = Vec::with_capacity(text.chars().count());
    for c in text.chars() {
        let mut new_delta = delta.clone();
        new_delta.content = Some(c.to_string());
        out.push(StreamChunk {
            delta: Some(new_delta),
            finish_reason: chunk.finish_reason.clone(),
            usage: chunk.usage.clone(),
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Response parsing helpers
// ---------------------------------------------------------------------------

/// Parse an OpenAI-style chat completion response.
fn parse_openai_response(json: &Value) -> Result<LlmResponse, AgentError> {
    let choices = json
        .get("choices")
        .and_then(|c| c.as_array())
        .ok_or_else(|| AgentError::LlmApi("No choices in response".to_string()))?;

    let choice = choices
        .first()
        .ok_or_else(|| AgentError::LlmApi("Empty choices array".to_string()))?;

    let message_obj = choice
        .get("message")
        .ok_or_else(|| AgentError::LlmApi("No message in choice".to_string()))?;

    // Parse content
    let content = message_obj
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();

    // Parse tool calls
    let tool_calls = message_obj
        .get("tool_calls")
        .and_then(|tc| tc.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|tc| {
                    let id = tc.get("id")?.as_str()?.to_string();
                    let function = tc.get("function")?;
                    let name = function.get("name")?.as_str()?.to_string();
                    let arguments = function
                        .get("arguments")
                        .and_then(|a| a.as_str())
                        .unwrap_or("{}")
                        .to_string();

                    Some(hermix_core::ToolCall {
                        id,
                        function: hermix_core::FunctionCall { name, arguments },
                    })
                })
                .collect::<Vec<_>>()
        });

    // Parse usage — tolerant of both OpenAI-style (prompt_tokens/completion_tokens)
    // and Anthropic-style (input_tokens/output_tokens) field names.
    let usage = json.get("usage").map(UsageStats::from_json);

    let role = message_obj
        .get("role")
        .and_then(|r| r.as_str())
        .unwrap_or("assistant");

    // Extract reasoning content
    let reasoning_content = message_obj
        .get("reasoning_content")
        .and_then(|r| r.as_str())
        .map(|s| s.to_string());

    let message = Message {
        role: match role {
            "user" => MessageRole::User,
            "system" => MessageRole::System,
            "tool" => MessageRole::Tool,
            _ => MessageRole::Assistant,
        },
        content: Some(content),
        content_parts: None,
        tool_calls,
        tool_call_id: None,
        name: None,
        reasoning_content,
        cache_control: None,
        execution_timeline: None,
        ephemeral: false,
    };

    let finish_reason = choice
        .get("finish_reason")
        .and_then(|f| f.as_str())
        .map(|s| s.to_string());

    let model = json
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown")
        .to_string();

    Ok(LlmResponse {
        message,
        usage,
        model,
        finish_reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_openai_response_basic() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Hello!"
                },
                "finish_reason": "stop"
            }],
            "model": "gpt-4o",
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        });
        let resp = parse_openai_response(&json).unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("Hello!"));
        assert_eq!(resp.finish_reason.as_deref(), Some("stop"));
        assert_eq!(resp.usage.as_ref().unwrap().total_tokens, 15);
    }

    #[test]
    fn test_parse_openai_response_with_tool_calls() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_123",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\": \"test.txt\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "model": "gpt-4o"
        });
        let resp = parse_openai_response(&json).unwrap();
        let tc = resp.message.tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].function.name, "read_file");
    }

    #[test]
    fn test_parse_sse_chunk_content() {
        let json = serde_json::json!({
            "choices": [{
                "delta": {
                    "content": "Hello"
                },
                "finish_reason": null
            }]
        });
        let chunk = parse_sse_chunk(&json).unwrap();
        assert_eq!(
            chunk.delta.as_ref().unwrap().content.as_deref(),
            Some("Hello")
        );
        assert!(chunk.finish_reason.is_none());
    }

    #[test]
    fn test_parse_sse_chunk_tool_call() {
        let json = serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_abc",
                        "function": {
                            "name": "search",
                            "arguments": ""
                        }
                    }]
                },
                "finish_reason": null
            }]
        });
        let chunk = parse_sse_chunk(&json).unwrap();
        let tc = chunk.delta.as_ref().unwrap().tool_calls.as_ref().unwrap();
        assert_eq!(tc[0].index, 0);
        assert_eq!(tc[0].id.as_deref(), Some("call_abc"));
    }

    #[test]
    fn test_parse_sse_chunk_finish() {
        let json = serde_json::json!({
            "choices": [{
                "delta": {},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 50,
                "total_tokens": 150
            }
        });
        let chunk = parse_sse_chunk(&json).unwrap();
        assert_eq!(chunk.finish_reason.as_deref(), Some("stop"));
        assert_eq!(chunk.usage.as_ref().unwrap().total_tokens, 150);
    }

    #[test]
    fn test_parse_sse_chunk_usage_only_no_choices() {
        // OpenAI's `stream_options.include_usage` sends a final chunk with
        // `choices: []` and usage at top level. Before the fix, this chunk
        // was silently dropped because `choices.first()` returned None.
        let json = serde_json::json!({
            "choices": [],
            "usage": {
                "prompt_tokens": 200,
                "completion_tokens": 80,
                "total_tokens": 280
            }
        });
        let chunk = parse_sse_chunk(&json).unwrap();
        assert!(chunk.delta.is_none());
        assert!(chunk.finish_reason.is_none());
        let usage = chunk.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 200);
        assert_eq!(usage.completion_tokens, 80);
        assert_eq!(usage.total_tokens, 280);
    }

    #[test]
    fn test_anthropic_convert_messages() {
        let messages = vec![
            Message::system("You are helpful"),
            Message::user("Hello"),
            Message::assistant("Hi there!"),
        ];
        let (system, msgs) = AnthropicProvider::convert_messages(&messages);
        assert_eq!(system.as_ref().and_then(|v| v.as_str()), Some("You are helpful"));
        assert_eq!(msgs.len(), 2); // user + assistant, system extracted
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[1]["role"], "assistant");
    }

    #[test]
    fn test_anthropic_convert_messages_emits_cache_control_on_system_block() {
        use hermix_core::types::{CacheControl, CacheType};
        let mut sys = Message::system("Stable soul + directions");
        sys.cache_control = Some(CacheControl {
            cache_type: CacheType::Ephemeral,
        });
        let messages = vec![sys, Message::user("Hello")];
        let (system, msgs) = AnthropicProvider::convert_messages(&messages);
        // System should now be an array (not a string) so Anthropic accepts
        // the cache_control breakpoint on the prefix.
        let blocks = system
            .as_ref()
            .and_then(|v| v.as_array())
            .expect("system should be an array when cache_control is set");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "Stable soul + directions");
        assert_eq!(blocks[0]["cache_control"]["type"], "ephemeral");
        // User message follows normally.
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
    }

    #[test]
    fn test_anthropic_convert_messages_with_tool_result() {
        let messages = vec![
            Message::system("System"),
            Message::user("Do something"),
            Message {
                role: MessageRole::Assistant,
                content: None,
                content_parts: None,
                tool_calls: Some(vec![ToolCall {
                    id: "tc_1".to_string(),
                    function: FunctionCall {
                        name: "read_file".to_string(),
                        arguments: r#"{"path":"test.txt"}"#.to_string(),
                    },
                }]),
                tool_call_id: None,
                name: None,
                reasoning_content: None,
                cache_control: None,
                execution_timeline: None,
                ephemeral: false,
            },
            Message::tool_result("tc_1", "file contents here", Some("read_file")),
        ];
        let (system, msgs) = AnthropicProvider::convert_messages(&messages);
        assert_eq!(system.as_ref().and_then(|v| v.as_str()), Some("System"));
        assert_eq!(msgs.len(), 3); // user, assistant with tool_use, user with tool_result
                                   // Assistant message should have tool_use block
        let assistant_content = msgs[1]["content"].as_array().unwrap();
        assert_eq!(assistant_content[0]["type"], "tool_use");
        assert_eq!(assistant_content[0]["name"], "read_file");
        // Tool result should be a user message with tool_result block
        let tool_content = msgs[2]["content"].as_array().unwrap();
        assert_eq!(tool_content[0]["type"], "tool_result");
        assert_eq!(tool_content[0]["tool_use_id"], "tc_1");
    }

    #[test]
    fn test_anthropic_parse_response() {
        let json = serde_json::json!({
            "content": [
                {"type": "text", "text": "Here is the answer."}
            ],
            "model": "claude-3-5-sonnet-20241022",
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50
            }
        });
        let resp = AnthropicProvider::parse_response(&json).unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("Here is the answer."));
        assert_eq!(resp.finish_reason.as_deref(), Some("stop"));
        assert_eq!(resp.usage.as_ref().unwrap().prompt_tokens, 100);
        assert_eq!(resp.usage.as_ref().unwrap().completion_tokens, 50);
    }

    #[test]
    fn test_anthropic_parse_response_with_tool_use() {
        let json = serde_json::json!({
            "content": [
                {"type": "text", "text": "Let me read that file."},
                {
                    "type": "tool_use",
                    "id": "toolu_123",
                    "name": "read_file",
                    "input": {"path": "test.txt"}
                }
            ],
            "model": "claude-3-5-sonnet-20241022",
            "stop_reason": "tool_use",
            "usage": {
                "input_tokens": 200,
                "output_tokens": 80
            }
        });
        let resp = AnthropicProvider::parse_response(&json).unwrap();
        assert_eq!(resp.finish_reason.as_deref(), Some("tool_calls"));
        let tc = resp.message.tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].id, "toolu_123");
        assert_eq!(tc[0].function.name, "read_file");
    }

    #[test]
    fn test_openrouter_parse_response_with_reasoning() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "The answer is 42.",
                    "reasoning_content": "Let me think step by step..."
                },
                "finish_reason": "stop"
            }],
            "model": "deepseek/deepseek-r1",
            "usage": {
                "prompt_tokens": 50,
                "completion_tokens": 30,
                "total_tokens": 80
            }
        });
        let resp = OpenRouterProvider::parse_openrouter_response(&json).unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("The answer is 42."));
        assert_eq!(
            resp.message.reasoning_content.as_deref(),
            Some("Let me think step by step...")
        );
    }

    #[test]
    fn test_openrouter_parse_response_with_reasoning_details() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Final answer.",
                    "reasoning_details": [
                        {"type": "text", "text": "Step 1"},
                        {"type": "text", "text": "Step 2"}
                    ]
                },
                "finish_reason": "stop"
            }],
            "model": "openai/o1-preview"
        });
        let resp = OpenRouterProvider::parse_openrouter_response(&json).unwrap();
        let reasoning = resp.message.reasoning_content.as_deref().unwrap();
        assert!(reasoning.contains("Step 1"));
        assert!(reasoning.contains("Step 2"));
    }

    #[test]
    fn test_openrouter_build_headers() {
        let provider = OpenRouterProvider::new("key")
            .with_http_referer("https://example.com")
            .with_x_title("My App");
        let headers = provider.build_headers();
        assert!(headers
            .iter()
            .any(|(k, v)| k == "HTTP-Referer" && v == "https://example.com"));
        assert!(headers.iter().any(|(k, v)| k == "X-Title" && v == "My App"));
    }

    #[test]
    fn test_anthropic_convert_tools() {
        let tools = vec![ToolSchema::new(
            "read_file",
            "Read a file",
            hermix_core::JsonSchema::new("object"),
        )];
        let converted = AnthropicProvider::convert_tools(&tools);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0]["name"], "read_file");
        assert_eq!(converted[0]["description"], "Read a file");
        assert!(converted[0].get("input_schema").is_some());
    }

    // ---------------------------------------------------------------
    // Strict-pairing tests (Pass 1 of sanitize_messages_for_strict_api).
    // The MiniMax-M3 provider returns 400 "tool call result does not
    // follow tool call (2013)" whenever a `tool` role message does not
    // immediately follow the matching assistant tool_use. Pass 1 walks
    // history and drops tool messages that violate adjacency.
    // ---------------------------------------------------------------

    fn assistant_with_calls(calls: &[(&str, &str)]) -> Message {
        Message {
            role: MessageRole::Assistant,
            content: None,
            content_parts: None,
            tool_calls: Some(
                calls
                    .iter()
                    .map(|(id, name)| ToolCall {
                        id: (*id).to_string(),
                        function: hermix_core::FunctionCall {
                            name: (*name).to_string(),
                            arguments: "{}".into(),
                        },
                    })
                    .collect(),
            ),
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
            execution_timeline: None,
            ephemeral: false,
        }
    }

    fn tool_result(id: &str, content: &str) -> Message {
        Message {
            role: MessageRole::Tool,
            content: Some(content.into()),
            content_parts: None,
            tool_calls: None,
            tool_call_id: Some(id.into()),
            name: None,
            reasoning_content: None,
            cache_control: None,
            execution_timeline: None,
            ephemeral: false,
        }
    }

    fn user_msg(text: &str) -> Message {
        Message {
            role: MessageRole::User,
            content: Some(text.into()),
            content_parts: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
            execution_timeline: None,
            ephemeral: false,
        }
    }

    #[test]
    fn strict_pairing_keeps_well_paired_tool_results() {
        // assistant(A, B) → tool(A) → tool(B)
        let messages = vec![
            user_msg("read file"),
            assistant_with_calls(&[("call_a", "read_file"), ("call_b", "read_file")]),
            tool_result("call_a", "file-a"),
            tool_result("call_b", "file-b"),
        ];
        let out = GenericProvider::enforce_strict_tool_pairing(messages);
        assert_eq!(out.len(), 4, "all 4 messages should survive");
        assert_eq!(out[2].tool_call_id.as_deref(), Some("call_a"));
        assert_eq!(out[3].tool_call_id.as_deref(), Some("call_b"));
    }

    #[test]
    fn strict_pairing_drops_tool_after_intervening_user_message() {
        // assistant(A) → user("thanks") → tool(A)
        // Strict providers reject — user between assistant.tool_use and tool
        // results breaks the pairing.
        let messages = vec![
            user_msg("read file"),
            assistant_with_calls(&[("call_a", "read_file")]),
            user_msg("thanks"),
            tool_result("call_a", "file-a"),
        ];
        let out = GenericProvider::enforce_strict_tool_pairing(messages);
        assert_eq!(out.len(), 3, "tool result must be dropped");
        assert!(out
            .iter()
            .all(|m| m.role != MessageRole::Tool || m.tool_call_id.is_none()));
    }

    #[test]
    fn strict_pairing_resets_on_new_assistant_message() {
        // assistant(A) → tool(A) → assistant(B) → tool(B) is the cleanest
        // case: each pairing resets cleanly.
        let messages = vec![
            user_msg("first request"),
            assistant_with_calls(&[("call_a", "read_file")]),
            tool_result("call_a", "file-a"),
            user_msg("second request"),
            assistant_with_calls(&[("call_b", "read_file")]),
            tool_result("call_b", "file-b"),
        ];
        let out = GenericProvider::enforce_strict_tool_pairing(messages);
        assert_eq!(out.len(), 6, "both pairings intact");
    }

    #[test]
    fn strict_pairing_drops_orphan_id_not_in_any_assistant() {
        // assistant(A) → tool(A) → tool("orphan_id") — orphan_id is not in
        // pending set, must be dropped even though it might look paired.
        let messages = vec![
            assistant_with_calls(&[("call_a", "read_file")]),
            tool_result("call_a", "file-a"),
            tool_result("orphan_id", "from nowhere"),
        ];
        let out = GenericProvider::enforce_strict_tool_pairing(messages);
        assert_eq!(out.len(), 2, "orphan tool result must be dropped");
        assert_eq!(out[1].tool_call_id.as_deref(), Some("call_a"));
    }

    #[test]
    fn strict_pairing_drops_tool_with_missing_id() {
        // Some persisted sessions have `tool` role messages with no
        // tool_call_id. Strict providers reject them outright.
        let messages = vec![
            assistant_with_calls(&[("call_a", "read_file")]),
            tool_result("call_a", "file-a"),
            // tool message with no tool_call_id
            {
                let mut m = tool_result("ignored_placeholder", "orphan");
                m.tool_call_id = None;
                m
            },
        ];
        let out = GenericProvider::enforce_strict_tool_pairing(messages);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].tool_call_id.as_deref(), Some("call_a"));
    }

    #[test]
    fn strict_pairing_drops_duplicate_id_in_same_pairing() {
        // assistant(A) → tool(A) → tool(A) — second tool(A) is a duplicate
        // (e.g. retry storm from a partial stream persistence).
        let messages = vec![
            assistant_with_calls(&[("call_a", "read_file")]),
            tool_result("call_a", "first"),
            tool_result("call_a", "second"),
        ];
        let out = GenericProvider::enforce_strict_tool_pairing(messages);
        assert_eq!(out.len(), 2, "second tool(A) must be dropped");
    }

    #[test]
    fn strict_pairing_preserves_empty_assistant_between_calls() {
        // assistant(A) → tool(A) → assistant (no tool_calls, content only)
        // → user("...") → assistant(B) → tool(B)
        // The empty-content assistant should clear the pending window.
        let mut empty_assistant = Message {
            role: MessageRole::Assistant,
            content: Some("intermediate reply".into()),
            content_parts: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
            execution_timeline: None,
            ephemeral: false,
        };
        empty_assistant.tool_calls = None;
        let messages = vec![
            assistant_with_calls(&[("call_a", "read_file")]),
            tool_result("call_a", "file-a"),
            empty_assistant,
            user_msg("now do x"),
            assistant_with_calls(&[("call_b", "write_file")]),
            tool_result("call_b", "done"),
        ];
        let out = GenericProvider::enforce_strict_tool_pairing(messages);
        assert_eq!(out.len(), 6, "everything survives (clean pairings)");
    }

    #[test]
    fn strict_pairing_strips_unanswered_calls_when_next_assistant_arrives() {
        // assistant(A) → [no tool(A) result] → assistant(B) → tool(B).
        // The half assistant's tool_call A never received a result; it must
        // be stripped so the strict provider does not 400 (2013) on an
        // unanswered tool_use.
        let messages = vec![
            user_msg("do it"),
            assistant_with_calls(&[("call_a", "read_file")]),
            // no tool_result("call_a") — stream died after the call was emitted
            user_msg("continue"),
            assistant_with_calls(&[("call_b", "write_file")]),
            tool_result("call_b", "done"),
        ];
        let out = GenericProvider::enforce_strict_tool_pairing(messages);
        assert_eq!(out.len(), 5, "all 5 messages survive");
        // The first assistant must have its unanswered call_a stripped.
        let first_assistant = out.iter().find(|m| {
            m.role == MessageRole::Assistant
                && m.tool_calls
                    .as_ref()
                    .is_some_and(|tcs| tcs.iter().any(|tc| tc.id == "call_a"))
        });
        assert!(
            first_assistant.is_none(),
            "call_a tool_use must be stripped — it never received a result"
        );
        // call_b pairing intact.
        let last_tool = out.last().expect("last message is tool(B)");
        assert_eq!(last_tool.tool_call_id.as_deref(), Some("call_b"));
    }

    #[test]
    fn strict_pairing_strips_unanswered_calls_at_array_end() {
        // The final assistant carries tool_calls but the array ends before
        // any tool result arrives (interrupted turn, persisted before tools
        // ran). The dangling calls must be stripped on the next request.
        let messages = vec![
            user_msg("go"),
            assistant_with_calls(&[("call_a", "read_file"), ("call_b", "search_files")]),
            tool_result("call_a", "file-a"),
            // call_b result never arrived; array ends here.
        ];
        let out = GenericProvider::enforce_strict_tool_pairing(messages);
        assert_eq!(out.len(), 3);
        let assistant = &out[1];
        let tcs = assistant.tool_calls.as_ref().expect("tool_calls present");
        assert_eq!(tcs.len(), 1, "call_b stripped, call_a kept");
        assert_eq!(tcs[0].id, "call_a");
        assert_eq!(out[2].tool_call_id.as_deref(), Some("call_a"));
    }

    #[test]
    fn strict_pairing_strips_all_calls_when_none_answered() {
        // Half assistant whose tool_calls were emitted but the whole stream
        // died — no results at all. tool_calls must become None.
        let messages = vec![
            user_msg("go"),
            assistant_with_calls(&[("call_a", "read_file")]),
            // array ends, no tool result anywhere
        ];
        let out = GenericProvider::enforce_strict_tool_pairing(messages);
        assert_eq!(out.len(), 2);
        assert!(
            out[1].tool_calls.is_none(),
            "unanswered tool_calls must be removed entirely"
        );
    }

    // ---------------------------------------------------------------
    // Pass 2: merge_adjacent_assistant_messages
    // ---------------------------------------------------------------

    fn plain_assistant(text: &str) -> Message {
        Message {
            role: MessageRole::Assistant,
            content: Some(text.into()),
            content_parts: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
            execution_timeline: None,
            ephemeral: false,
        }
    }

    #[test]
    fn merge_adjacent_assistants_concatenates_content() {
        let messages = vec![
            plain_assistant("first"),
            plain_assistant("second"),
            user_msg("after"),
        ];
        let out = GenericProvider::merge_adjacent_assistant_messages(messages);
        assert_eq!(out.len(), 2, "two assistants collapse into one");
        assert_eq!(out[0].content.as_deref(), Some("first\nsecond"));
        assert_eq!(out[1].role, MessageRole::User);
    }

    #[test]
    fn merge_adjacent_assistants_merges_tool_calls() {
        let messages = vec![
            assistant_with_calls(&[("call_a", "read_file")]),
            assistant_with_calls(&[("call_b", "write_file")]),
        ];
        let out = GenericProvider::merge_adjacent_assistant_messages(messages);
        assert_eq!(out.len(), 1, "two tool-call assistants collapse into one");
        let tcs = out[0].tool_calls.as_ref().expect("tool_calls preserved");
        assert_eq!(tcs.len(), 2);
        assert_eq!(tcs[0].id, "call_a");
        assert_eq!(tcs[1].id, "call_b");
    }

    #[test]
    fn merge_adjacent_assistants_leaves_alternating_untouched() {
        let messages = vec![
            user_msg("u1"),
            plain_assistant("a1"),
            user_msg("u2"),
            plain_assistant("a2"),
        ];
        let out = GenericProvider::merge_adjacent_assistant_messages(messages);
        assert_eq!(out.len(), 4, "alternating roles never merge");
    }

    #[test]
    fn sanitize_full_pipeline_merges_assistants_after_pairing_drop() {
        // assistant(A) → user → assistant(B) → tool(A)[dropped by Pass 1]
        // → assistant(C). Pass 1 drops tool(A) (pairing broken by user),
        // leaving assistant(B) and assistant(C) adjacent; Pass 2 must
        // merge them into a single assistant message.
        let messages = vec![
            user_msg("go"),
            assistant_with_calls(&[("call_a", "read_file")]),
            user_msg("interrupt"),
            assistant_with_calls(&[("call_b", "write_file")]),
            tool_result("call_a", "file-a"),
            plain_assistant("final"),
        ];
        let out = GenericProvider::sanitize_messages_for_strict_api(&messages, true);
        let arr = out.as_array().expect("array of api messages");
        // Expect: user("go"), assistant(A), user("interrupt"),
        //         assistant(B + C merged), ... — and NO tool role message.
        let roles: Vec<_> = arr
            .iter()
            .filter_map(|m| m.get("role").and_then(|r| r.as_str()))
            .collect();
        let tool_count = roles.iter().filter(|r| **r == "tool").count();
        assert_eq!(tool_count, 0, "orphan tool result must be dropped");
        let assistant_idxs: Vec<usize> = roles
            .iter()
            .enumerate()
            .filter(|(_, r)| **r == "assistant")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(assistant_idxs.len(), 2, "two assistant messages remain");
        // The second assistant carries both call_b tool_call and merged
        // "final" content.
        let merged = &arr[assistant_idxs[1]];
        let tcs = merged
            .get("tool_calls")
            .and_then(|t| t.as_array())
            .expect("merged assistant keeps tool_calls");
        assert_eq!(tcs.len(), 1);
        assert_eq!(
            tcs[0].get("id").and_then(|v| v.as_str()),
            Some("call_b"),
            "only call_b survives (call_a's result was dropped)"
        );
        assert!(
            merged
                .get("content")
                .and_then(|c| c.as_str())
                .map(|c| c.contains("final"))
                .unwrap_or(false),
            "merged content includes tail assistant text"
        );
    }

    #[test]
    fn strict_api_guarantees_content_field_on_contentless_messages() {
        // Regression: a persisted history can hold a message with no text
        // and no tool_calls (e.g. a partial assistant turn left behind after
        // the previous provider's stream was aborted at its limit). Because
        // `Message::content` uses skip_serializing_if, such a message would
        // be sent without a `content` field and strict OpenAI-compatible
        // gateways (Agnes, MiniMax-M3) reject it with HTTP 400
        // `messages[N]: missing field content`. sanitize must inject "".
        let empty_assistant = Message {
            role: MessageRole::Assistant,
            content: None,
            ..Default::default()
        };
        let empty_user = Message {
            role: MessageRole::User,
            content: None,
            ..Default::default()
        };
        let populated = plain_assistant("real content");

        let out = GenericProvider::sanitize_messages_for_strict_api(
            &[empty_assistant, empty_user, populated],
            true,
        );
        let arr = out.as_array().expect("array of api messages");
        assert_eq!(arr.len(), 3);

        // empty assistant → content must be a string "" (not null/missing)
        let a_content = arr[0]
            .get("content")
            .expect("assistant content present");
        assert!(a_content.is_string(), "assistant content must be a string");
        assert_eq!(a_content.as_str(), Some(""));
        assert!(arr[0].get("content_parts").is_none(), "no content_parts key");

        // empty user → same guarantee
        let u_content = arr[1].get("content").expect("user content present");
        assert!(u_content.is_string());
        assert_eq!(u_content.as_str(), Some(""));

        // populated message → content preserved verbatim
        assert_eq!(
            arr[2].get("content").and_then(|c| c.as_str()),
            Some("real content")
        );
    }

    #[test]
    fn strict_api_preserves_multimodal_content_parts() {
        // A vision message carries `content_parts` (array); sanitize must
        // rewrite it into `content` and drop the `content_parts` key — it
        // must NOT be forced to an empty string.
        let vision = Message {
            role: MessageRole::User,
            content: None,
            content_parts: Some(vec![hermix_core::ContentPart::Text {
                text: "describe".into(),
            }]),
            ..Default::default()
        };
        let out = GenericProvider::sanitize_messages_for_strict_api(&[vision], true);
        let arr = out.as_array().expect("array of api messages");
        let c = arr[0].get("content").expect("content present");
        assert!(c.is_array(), "content_parts must become a content array");
        assert_eq!(arr[0].get("content_parts"), None, "content_parts key removed");
    }
}
