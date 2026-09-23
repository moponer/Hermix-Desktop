//! Shared approval side-effect helpers, redaction, and wire-image conversion.




use super::*;
use hermix_tools::extract_trust_signature;

pub(crate) fn record_approval_side_effect(
    state: &HttpServerState,
    session_id: &str,
    tool: &str,
    params: &serde_json::Value,
    choice: hermix_transport::ApprovalChoice,
) {
    match choice {
        hermix_transport::ApprovalChoice::Session => {
            // Extract the per-tool signature and append a regex
            // match for it to the session's bypass list. Tool-
            // aware: a write_file bypass doesn't accidentally
            // match a terminal call (the pre-checker computes
            // the same signature per call).
            let Some(signature) = extract_trust_signature(tool, params) else {
                tracing::warn!(tool, "session-bypass: no extractable signature; side effect skipped");
                return;
            };
            // Session scope grants the same thing `always` does, minus
            // restart survival: hit ⇔ identical trust signature. The
            // signature is itself a fully-anchored regex fragment
            // (`^sudo\b.*`, ...), so it must be stored as an **escaped
            // literal** — compiling it as a raw pattern would try to
            // match the anchor/meta characters in the rebuilt text and
            // never hit anything.
            let pattern = format!("^{}$", regex::escape(&signature));
            match Regex::new(&pattern) {
                Ok(re) => {
                    if let Ok(mut map) = state.session_bypass.lock() {
                        map.entry(session_id.to_string())
                            .or_default()
                            .push(re);
                        tracing::info!(
                            session = %session_id,
                            tool = %tool,
                            signature = %signature,
                            "session-bypass: added"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        tool = %tool,
                        signature = %signature,
                        error = %e,
                        "session-bypass: failed to compile pattern; side effect skipped"
                    );
                }
            }
        }
        hermix_transport::ApprovalChoice::Always => {
            let Some(signature) = extract_trust_signature(tool, params) else {
                tracing::warn!(tool, "always-allow: no extractable signature; side effect skipped");
                return;
            };
            // Pick a human-readable hint for the entry (command →
            // path → url → tool). The hint shows up in the
            // allowlist list output; matching is against the
            // compiled pattern, not the hint.
            let hint = params
                .get("command")
                .or_else(|| params.get("cmd"))
                .or_else(|| params.get("path"))
                .or_else(|| params.get("url"))
                .and_then(|v| v.as_str())
                .unwrap_or(tool)
                .to_string();
            if let Ok(mut guard) = state.user_allowlist.lock() {
                if guard.is_none() {
                    *guard = Some(UserAllowlist::new());
                }
                if let Some(al) = guard.as_mut() {
                    if let Err(e) = al.add_literal_for(tool, &signature, &hint) {
                        tracing::warn!(
                            tool = %tool,
                            signature = %signature,
                            error = %e,
                            "always-allow: failed to add entry; side effect skipped"
                        );
                        return;
                    }
                } else {
                    return;
                }
            } else {
                return;
            }
            // Persist the updated allowlist. Failure here is
            // logged but not propagated to the user — the
            // in-memory state is already correct and a failed
            // save just means the next restart loses this one
            // entry (the user can re-pick `always` next time).
            let path = hermix_config::paths::approval_allowlist_path();
            if let Ok(guard) = state.user_allowlist.lock() {
                if let Some(al) = guard.as_ref() {
                    if let Err(e) = al.save(&path) {
                        tracing::warn!(
                            error = %e,
                            path = %path.display(),
                            "always-allow: failed to persist allowlist; \
                             in-memory state is correct but the change \
                             won't survive a restart"
                        );
                    } else {
                        tracing::info!(
                            tool = %tool,
                            path = %path.display(),
                            "always-allow: persisted to disk"
                        );
                    }
                }
            }
        }
        _ => {
            // `Once` / `Deny` don't need a side effect — the
            // registry's resolve is the whole story for them.
        }
    }
}

/// P6 : resource-scope side effect. Used when the
/// approval request carries `risk: "resource"` (P6 direction 1),
/// meaning the user is authorising access to a specific path or
/// command prefix the agent requested. `Always` persists the
/// grant into `GatewayConfig.permission_profile` and writes it
/// back to `config.yaml`; `Session` keeps the grant in-memory only
/// (the existing `session_bypass` mechanism is for terminal
/// commands and doesn't carry paths). `Once` / `Deny` are no-ops.
pub(crate) async fn record_resource_approval_side_effect(
    state: &HttpServerState,
    session_id: &str,
    request_id: &str,
    choice: hermix_transport::ApprovalChoice,
) {
    let (path, command) = match ws::lookup_pending_resource(state, session_id, request_id).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(
                request_id = %request_id,
                error = %e,
                "resource-approval: failed to look up pending row; side effect skipped"
            );
            return;
        }
    };
    if path.is_none() && command.is_none() {
        // Non-resource approval (e.g. the existing Dangerous-tier
        // terminal approval) — fall through to the regular
        // command-based side effect path.
        return;
    }
    match choice {
        hermix_transport::ApprovalChoice::Always => {
            // 1. Pull a fresh GatewayConfig snapshot (the user may
            // have edited it via Settings since startup).
            let mut cfg: hermix_config::GatewayConfig = state
                .agent_service
                .current_config_value()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_else(|| state.config.as_ref().clone());
            // 2. Apply grants. Mirrors the
            // `PendingGrant::from_tool_call` heuristic in
            // hermix-core so server- and client-side agree on
            // what "grant this resource" means.
            if let Some(p) = &path {
                cfg.permission_profile.grant_path(p);
            }
            if let Some(c) = &command {
                cfg.permission_profile.grant_command(c);
            }
            // 3. Persist to config.yaml so the grant survives
            // server restart. Failure is logged but does not
            // abort the request — the in-memory state is
            // already pushed to the agent service below.
            let config_path = state.hermix_home.join("config.yaml");
            if let Err(e) = hermix_config::save_config_yaml(&config_path, &cfg) {
                tracing::warn!(
                    error = %e,
                    path = %config_path.display(),
                    "permission_profile: failed to persist config.yaml; \
                     in-memory state is correct but the change won't survive a restart"
                );
            } else {
                tracing::info!(
                    path = ?path,
                    command = ?command,
                    config = %config_path.display(),
                    "permission_profile: persisted Always-grant to config.yaml"
                );
            }
            // 4. Push the updated config to the live agent service
            // so the next chat's `build_agent_config` picks up
            // the new profile without a server restart.
            match serde_json::to_value(&cfg) {
                Ok(value) => state.agent_service.set_config(value),
                Err(e) => tracing::warn!(
                    error = %e,
                    "permission_profile: failed to serialise GatewayConfig for set_config"
                ),
            }
        }
        hermix_transport::ApprovalChoice::Session => {
            // In-memory only: live-push to the agent service.
            // Persisting session-scoped grants to disk would muddy
            // the "session" semantics — restart should clear them.
            let mut cfg: hermix_config::GatewayConfig = state
                .agent_service
                .current_config_value()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_else(|| state.config.as_ref().clone());
            if let Some(p) = &path {
                cfg.permission_profile.grant_path(p);
            }
            if let Some(c) = &command {
                cfg.permission_profile.grant_command(c);
            }
            if let Ok(value) = serde_json::to_value(&cfg) {
                state.agent_service.set_config(value);
            }
            tracing::info!(
                session_id = %session_id,
                path = ?path,
                command = ?command,
                "permission_profile: in-memory Session-grant applied (no persistence)"
            );
        }
        _ => {
            // `Once` / `Deny` are no-ops at the profile layer; the
            // agent already has the immediate grant via the
            // on_pre_tool_approved hook for this turn only.
        }
    }
}

pub(crate) fn redact_sensitive(input: &str) -> String {
    let mut out = input.to_string();
    let patterns = [
        ("sk-", "sk-***"),
        ("ghp_", "ghp_***"),
        ("xoxb-", "xoxb-***"),
    ];
    for (needle, replacement) in patterns {
        if out.contains(needle) {
            out = out.replace(needle, replacement);
        }
    }
    out
}

/// Side-effects for one resolved approval — `resolve_approval_message`
/// in the DB plus, for Session/Always choices, the resource-scope
/// `permission_profile` push and the durable allowlist entry. The
/// registry resolve itself is the caller's responsibility (so the
/// batch path can iterate the registry's `resolve_batch` result
/// without re-resolving already-resolved members).
pub(crate) async fn record_approval_resolution(
    state: &HttpServerState,
    session_id: &str,
    request_id: &str,
    choice: hermix_transport::ApprovalChoice,
) {
    let decision_str = match choice {
        hermix_transport::ApprovalChoice::Once => "once",
        hermix_transport::ApprovalChoice::Session => "session",
        hermix_transport::ApprovalChoice::Always => "always",
        hermix_transport::ApprovalChoice::Deny => "deny",
    };
    let _ = state
        .session_persistence
        .resolve_approval_message(session_id, request_id, decision_str);
    if matches!(
        choice,
        hermix_transport::ApprovalChoice::Session | hermix_transport::ApprovalChoice::Always
    ) {
        record_resource_approval_side_effect(state, session_id, request_id, choice).await;
        if let Ok(Some((tool, params, _expires_at))) =
            ws::lookup_pending_approval(state, session_id, request_id).await
        {
            record_approval_side_effect(state, session_id, &tool, &params, choice);
        }
    }
}

/// turn a raw `AgentError` from the agent loop into a
/// user-facing `(code, message)` pair.
/// Each classified category prepends a short actionable Chinese prefix
/// (e.g. "AI 服务过载:") followed by the **original provider error
/// message**, so the user can see what actually happened (which model,
/// which provider, what the API returned) instead of a generic
/// one-size-fits-all banner. Everything else falls back to
/// `e.to_string()` so we never lose diagnostic detail.
pub(crate) fn user_facing_agent_error(e: &str) -> (String, String) {
    let s = e.to_string();
    let lower = s.to_lowercase();

    /// Extract the human-readable message from a raw error string.
    /// Provider responses are often nested JSON like:
    /// `API error 529 <unknown status code>: {"type":"error","error":{"message":"负载较高..."}}`
    /// This function strips the `API error NNN` prefix and extracts
    /// the inner `message` field if present, falling back to the
    /// stripped string otherwise.
    fn extract_provider_message(raw: &str) -> String {
        // Strip repeated "LLM API error: API error:" wrappers
        // (replace is global — one pass removes every occurrence;
        // a second identical replace would be a no-op)
        let stripped = raw
            .replace("LLM API error: ", "")
            .replace("API error ", "");
        // Try to extract the provider's own message from JSON
        // e.g. `529 <unknown status code>: {"type":"error","error":{"message":"负载较高..."}}`
        if let Some(json_start) = stripped.find('{') {
            let json_part = &stripped[json_start..];
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_part) {
                if let Some(msg) = parsed
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                {
                    return msg.to_string();
                }
                if let Some(msg) = parsed.get("message").and_then(|m| m.as_str()) {
                    return msg.to_string();
                }
            }
        }
        // Not JSON or no message field — return the stripped version
        stripped.trim().to_string()
    }

    let provider_msg = extract_provider_message(&s);

    if lower.contains("429")
        || lower.contains("too many requests")
        || lower.contains("rate_limit")
        || lower.contains("rate limit")
        || lower.contains("用量上限")
        || lower.contains("token plan")
        || lower.contains("quota")
    {
        (
            "quota_exhausted".to_string(),
            format!("用量已达上限: {provider_msg}"),
        )
    } else if lower.contains("529")
        || lower.contains("overloaded")
        || lower.contains("负载较高")
        || lower.contains("server is overloaded")
        || lower.contains("service unavailable")
        || lower.contains("503")
    {
        (
            "provider_overloaded".to_string(),
            format!("AI 服务过载: {provider_msg}"),
        )
    } else if lower.contains("401")
        || lower.contains("unauthorized")
        || lower.contains("authentication")
        || lower.contains("invalid api key")
    {
        (
            "auth_error".to_string(),
            format!("API 密钥认证失败: {provider_msg}"),
        )
    } else {
        ("agent_error".to_string(), s)
    }
}

// ---------------------------------------------------------------------------
// Visible-text sanitizer for raw tool-call markers in LLM output.
// The redactor itself lives in `hermix_core::tool_call_sanitizer`
// (see `hermix_core::ToolCallSanitizer`). It is a *stateful*
// state-machine: opening and closing XML tags may arrive in different
// streaming chunks, and a per-chunk regex pass (the previous
// implementation) would leak any block whose opener and closer were
// split across a chunk boundary. The new implementation holds the
// in-between chunks until the matching close arrives.
// ## Why this lives in two places
// The agent loop (`hermix_agent::agent_loop::apply_stream_collect_chunk`)
// runs one copy so the *persisted* assistant message is clean on
// reload. The WS layer (`hermix_server::lib` on_chunk) runs a second
// copy so the *live* stream never shows leaked XML. The two passes
// are idempotent (the second pass is a no-op once the text is
// already clean), so the doubling is intentional belt-and-suspenders
// and not a bug.
// ## What it does NOT do
// It does NOT try to *execute* the embedded tool call. That would
// require either (a) telling the agent loop to use a different
// text-based parser as the authoritative source, or (b) pre-parsing
// the streamed text and synthesising `tool_start` / `tool_complete`
// events out-of-band. Both are bigger surface-area changes; the
// narrow "don't show the XML to the user" fix is enough to stop the
// visible leak today.
// The original regression tests for this leak have been moved to
// `hermix_core::tool_call_sanitizer::tests`.

/// Map the `permission` field of [`SendMessageRequest`] (a free-form string
/// the frontend hands us) to the strongly-typed [`PermissionLevel`]. Unknown
/// values fall back to `Default` so a malformed payload never escalates
/// privileges silently.
pub(crate) fn parse_permission_level(value: Option<&str>) -> Option<PermissionLevel> {
    match value.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("default") | None => Some(PermissionLevel::Default),
        Some("full") => Some(PermissionLevel::Full),
        Some(other) => {
            tracing::warn!(
                permission = %other,
                "unknown permission level in SendMessageRequest; falling back to Default"
            );
            Some(PermissionLevel::Default)
        }
    }
}

/// Convert wire-format images (raw bytes) to agent-format images (base64).
/// 多图支持 — client sends `Vec<ImageAttachment>` with binary
/// `bytes`; the LLM API needs `data:<mime>;base64,<...>` URLs, so we
/// encode here.
pub(crate) fn convert_wire_images(
    images: &Option<Vec<hermix_transport::ImageAttachment>>,
) -> Option<Vec<hermix_core::traits::AgentImage>> {
    images.as_ref().map(|imgs| {
        imgs.iter()
            .map(|img| hermix_core::traits::AgentImage {
                base64: base64::engine::general_purpose::STANDARD.encode(&img.bytes),
                mime_type: img.mime_type.clone(),
            })
            .collect()
    })
}

// ---------------------------------------------------------------------------
// L3: Voice bridge — HTTP endpoints for STT + TTS
// ---------------------------------------------------------------------------

/// `POST /v1/voice/transcribe` — accept base64-encoded 16-bit PCM
/// and return transcribed text. Body shape matches
/// `hermix_voice::TranscribeRequest`; response matches
/// `hermix_voice::TranscribeResponse`.
pub(crate) fn extract_llm_probe_url(config: &GatewayConfig) -> Option<(String, Option<String>)> {
    // 1) Try the active model string ("provider:model")
    if let Some(ref model_str) = config.model {
        if let Some((prov, _)) = model_str.split_once(':') {
            if let Some(p) = config.llm_providers.get(prov) {
                if let Some(ref url) = p.base_url {
                    return Some((prov.to_string(), Some(url.clone())));
                }
                // No base_url set, but we can still return provider name so
                // the runtime can infer a default URL from it.
                if p.api_key.as_deref().map(|s| !s.is_empty()).unwrap_or(false)
                    || std::env::var(os_env_var(prov)).ok().map(|s| !s.is_empty()).unwrap_or(false)
                {
                    return Some((prov.to_string(), None));
                }
            }
        }
    }
    // 2) Fall back to the first provider that is "configurable" (api_key OR env
    // var OR explicit base_url). Without an api_key/env, the runtime can't
    // actually authenticate, but returning the provider name + base_url lets
    // the caller at least surface a useful default (and probe it).
    for (name, p) in &config.llm_providers {
        let has_key = p.api_key.as_deref().map(|s| !s.is_empty()).unwrap_or(false)
            || std::env::var(os_env_var(name)).ok().map(|s| !s.is_empty()).unwrap_or(false);
        let has_url = p.base_url.as_deref().map(|s| !s.is_empty()).unwrap_or(false);
        if has_key || has_url {
            return Some((name.clone(), p.base_url.clone()));
        }
    }
    None
}

/// Map a provider name to its canonical env var name for key detection.
pub(crate) fn os_env_var(provider: &str) -> &'static str {
    match provider {
        "openai" => "OPENAI_API_KEY",
        "anthropic" => "ANTHROPIC_API_KEY",
        "openrouter" => "OPENROUTER_API_KEY",
        "qwen" | "dashscope" => "DASHSCOPE_API_KEY",
        "deepseek" => "DEEPSEEK_API_KEY",
        "kimi" | "moonshot" => "MOONSHOT_API_KEY",
        "minimax" => "MINIMAX_API_KEY",
        "nous" => "NOUS_API_KEY",
        "copilot" => "GITHUB_COPILOT_TOKEN",
        "siliconflow" => "SILICONFLOW_API_KEY",
        "zhipu" => "ZHIPU_API_KEY",
        "volcengine" => "ARK_API_KEY",
        _ => "UNKNOWN_API_KEY",
    }
}

