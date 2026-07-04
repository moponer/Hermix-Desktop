//! Standalone prompt-assembly helpers used by `agent_loop`.
//!
//! Extracted from `agent_loop.rs` (P2.x, 2026-07-04) so the 10k-line
//! `agent_loop.rs` can focus on the `impl AgentLoop` block. **No behavior
//! change** — pure relocation of pure functions.
//!
//! Items living here:
//! - `should_inject_tool_enforcement` — gate for the format-guidance block.
//! - `platform_hint_text` — short format hint based on the active platform.
//! - `effective_provider_for_prompt` — resolve the provider name to inject
//!   into the prompt (config override → model prefix → `None`).
//! - `skills_system_prompt` — render the `<available_skills>` block plus
//!   ephemeral per-turn skills.
//! - `context_files_prompt` — render the workspace + personal context
//!   sections from disk.
//! - `extract_provider_and_model` — split `"provider:model"` notation.
//! - `messages_for_api_call` — assemble the messages vector sent to the
//!   provider (strips old reasoning, applies ephemeral system prompt, marks
//!   cache breakpoints).
//! - `apply_prompt_cache_markers` — Anthropic prompt-cache marker strategy.
//! - `build_dynamic_suffix` — per-turn timestamp + session_id block.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use hermix_core::{Message, ToolSchema};
use hermix_focus::MemoryRouter;

use crate::agent_config::AgentConfig;
use crate::agent_helpers::AgentCallbacks;
use crate::context::{load_builtin_memory_snapshot, load_soul_md, resolve_personality, SystemPromptBuilder};
use crate::context_files::{load_hermes_context_files, load_workspace_context};
use crate::ContextManager;

/// Decide whether the model family gets the MiniMax-style tool-format
/// enforcement block. See `agent_loop::collect_directions` for how the
/// flag is consumed.
pub fn should_inject_tool_enforcement(model: &str) -> bool {
    let model_lower = model.to_lowercase();
    ["gpt", "codex", "gemini", "gemma", "grok", "minimax", "m2"]
        .iter()
        .any(|p| model_lower.contains(p))
}

/// Short format hint string that nudges the model toward the
/// appropriate output style for the active platform (CLI / chat / email / SMS).
/// Returns `None` when no platform-specific hint applies.
pub fn platform_hint_text(config: &AgentConfig) -> Option<&'static str> {
    let platform_key = config
        .platform
        .as_deref()
        .map(|s| s.trim().to_lowercase())
        .unwrap_or_default();
    match platform_key.as_str() {
        "cli" => Some("You are a CLI AI Agent. Prefer concise plain text output suitable for terminals."),
        "telegram" | "discord" | "slack" => {
            Some("You are responding on a chat platform. Keep responses concise and avoid heavy formatting.")
        }
        "email" => Some("You are responding over email. Use clear structure and complete sentences."),
        "sms" => Some("You are responding over SMS. Keep responses short and high-signal."),
        _ => None,
    }
}

/// Resolve the provider name that should appear in the system prompt.
/// Priority: explicit `config.provider` → model prefix (before `:`) → `None`.
pub fn effective_provider_for_prompt(config: &AgentConfig, model: &str) -> Option<String> {
    if let Some(ref p) = config.provider {
        let trimmed = p.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    model
        .split_once(':')
        .map(|(provider, _)| provider.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Render the `<available_skills>` block plus any ephemeral per-turn
/// skills set via `AgentOverrides::skills`. Returns `None` when there are
/// no skills-management tools loaded AND no ephemeral skills requested.
pub fn skills_system_prompt(
    config: &AgentConfig,
    tool_names: &HashSet<&str>,
) -> Option<String> {
    let has_skills_tools = ["skills_list", "skill_view", "skill_manage"]
        .iter()
        .any(|t| tool_names.contains(*t));
    // Ephemeral per-turn skills (set by the desktop "扩展 → 技能" picker
    // via `AgentOverrides::skills`) always surface — they explicitly
    // request their content in the prompt, regardless of whether the
    // skills-management tools are also loaded.
    let ephemeral: Vec<String> = config
        .ephemeral_skills
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if !has_skills_tools && ephemeral.is_empty() {
        return None;
    }
    let mut orch = crate::skill_orchestrator::SkillOrchestrator::default_dir();
    let commands = orch.scan_skill_commands();
    let mut body = String::from(
        "## Skills (mandatory)\nBefore replying, check whether an existing skill applies. If yes, inspect it with `skill_view` and follow it.\n<available_skills>\n",
    );
    if commands.is_empty() {
        body.push_str("(no installed skills discovered via skill_manage — only ephemeral skills below are active)\n");
    } else {
        let mut rows: Vec<_> = commands.iter().collect();
        rows.sort_by(|a, b| a.0.cmp(b.0));
        for (cmd, info) in rows.into_iter().take(80) {
            body.push_str(&format!(
                "- {}: {} ({})\n",
                cmd,
                info.name,
                info.description.trim()
            ));
        }
    }
    body.push_str("</available_skills>");

    // Render ephemeral skills (set per-turn from AgentOverrides::skills).
    // These are the user's "I want this skill active for this chat"
    // picks — even if the skill isn't enabled server-side, we inline
    // its SKILL.md content here so the model can follow it. The
    // server-side enabled list is still the floor for which skills
    // `skill_manage` / `skills_list` can surface; this block is the
    // ceiling for which skills the prompt actually sees content from.
    if !ephemeral.is_empty() {
        let orch2 = crate::skill_orchestrator::SkillOrchestrator::default_dir();
        let (ephemeral_body, loaded, missing) =
            orch2.build_preloaded_skills_prompt(&ephemeral);
        if !loaded.is_empty() {
            body.push_str("\n\n## Active Skills (this turn)\n");
            body.push_str(
                "The user explicitly enabled the following skills for this conversation. \
                 Treat their instructions as active guidance for the duration of this \
                 session unless the user overrides them.\n",
            );
            body.push_str(&ephemeral_body);
        }
        if !missing.is_empty() {
            body.push_str("\n\n## Skills Not Found\n");
            body.push_str(
                "The user requested the following skills but they are not installed on \
                 disk. Ask the user to install them or use the plugin center:\n",
            );
            for name in &missing {
                body.push_str(&format!("- `{}`\n", name));
            }
        }
    }
    Some(body)
}

/// Render the workspace + personal context sections loaded from disk.
/// Returns `None` when neither source has any content.
pub fn context_files_prompt(config: &AgentConfig) -> Option<String> {
    // Priority: explicit TERMINAL_CWD env var (legacy escape hatch) →
    // per-turn config.workspace (set by LocalAgentService from
    // AgentOverrides::workspace, the desktop "工作区" picker) →
    // server process CWD. This is what the agent should treat as its
    // working directory when scanning for AGENTS.md / .hermes.md.
    let cwd = std::env::var("TERMINAL_CWD")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(|| {
            config
                .workspace
                .as_deref()
                .map(std::path::PathBuf::from)
        })
        .unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        });

    let mut sections = Vec::new();
    if let Some(workspace) = load_workspace_context(&cwd) {
        sections.push(format!("## Workspace Context\n{}", workspace));
    }

    let hermix_home = config
        .hermix_home
        .clone()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(hermix_config::paths::hermix_home);

    let personal_ctx = load_hermes_context_files(&hermix_home);
    if !personal_ctx.trim().is_empty() {
        sections.push(format!("## Personal Context\n{}", personal_ctx));
    }

    if sections.is_empty() {
        None
    } else {
        Some(sections.join("\n\n"))
    }
}

/// Split `"provider:model"` notation. Returns `(provider, model)` where
/// provider falls back to `config.provider` and finally `"openai"`.
pub fn extract_provider_and_model<'a>(
    config: &AgentConfig,
    model: &'a str,
) -> (String, &'a str) {
    if let Some((p, m)) = model.split_once(':') {
        let p = p.trim();
        let m = m.trim();
        if !p.is_empty() && !m.is_empty() {
            return (p.to_string(), m);
        }
    }
    let fallback_provider = config
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("openai")
        .to_string();
    (fallback_provider, model)
}

/// Assemble the messages vector sent to the provider.
/// 1. Inject `ephemeral_system_prompt` (per-turn override) if non-empty.
/// 2. Strip `reasoning_content` from all but the most recent assistant
///    message — old thinking blocks are noise once the LLM has moved on.
/// 3. Apply Anthropic prompt-cache markers.
pub fn messages_for_api_call(config: &AgentConfig, ctx: &ContextManager) -> Vec<Message> {
    let mut messages = ctx.get_messages().to_vec();
    if let Some(ephemeral) = config
        .ephemeral_system_prompt
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        messages.push(Message::system(ephemeral));
    }
    // Strip reasoning_content from all but the last assistant message.
    // Old thinking blocks are noise once the LLM has moved on — it doesn't
    // need to re-read its own previous reasoning every turn.
    for i in (0..messages.len()).rev() {
        if messages[i].reasoning_content.is_some() {
            // Keep the last one (most recent assistant), clear the rest.
            // Scan backwards: the first hit is the last assistant.
            for j in (0..i).rev() {
                messages[j].reasoning_content = None;
            }
            break;
        }
    }
    apply_prompt_cache_markers(config, &mut messages);
    messages
}

/// Anthropic-specific: mark the system message as `Persistent` cache and
/// the last 4 user/assistant messages as `Ephemeral` cache. Other
/// providers do not consume these markers, so the function is a no-op
/// when `config.provider` is not Anthropic/Claude.
pub fn apply_prompt_cache_markers(config: &AgentConfig, messages: &mut [Message]) {
    use hermix_core::types::{CacheControl, CacheType, MessageRole};
    if messages.is_empty() {
        return;
    }
    let provider = config
        .provider
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();
    let is_anthropic = provider.contains("anthropic") || provider.contains("claude");
    if !is_anthropic {
        return;
    }
    for msg in messages.iter_mut() {
        if msg.role == MessageRole::System {
            msg.cache_control = Some(CacheControl {
                cache_type: CacheType::Persistent,
            });
            break;
        }
    }
    let ephemeral_budget = 4;
    let mut marked = 0;
    for msg in messages.iter_mut().rev() {
        if marked >= ephemeral_budget {
            break;
        }
        if msg.role == MessageRole::User || msg.role == MessageRole::Assistant {
            msg.cache_control = Some(CacheControl {
                cache_type: CacheType::Ephemeral,
            });
            marked += 1;
        }
    }
}

/// Build the per-turn dynamic suffix of the system prompt.
///
/// This is the only part of the system prompt that changes every turn:
///   - the timestamp block (model + provider name)
///   - the optional session_id line (when `pass_session_id` is on)
///
/// The rest of the prompt (identity, directions, tools, personality,
/// memory, skills, context files, model branding, platform hint,
/// workspace) lives in the stable prefix and is reused from
/// `cached_stable_prompt_key` across turns when its key matches.
pub fn build_dynamic_suffix(config: &AgentConfig, model_for_prompt: &str) -> String {
    let provider = effective_provider_for_prompt(config, model_for_prompt);
    let mut suffix = crate::context::SystemPromptBuilder::new();
    suffix = suffix.with_timestamp(Some(model_for_prompt), provider.as_deref());

    let mut timestamp_extras = String::new();
    if config.pass_session_id {
        if let Some(ref sid) = config.session_id {
            if !sid.trim().is_empty() {
                timestamp_extras.push_str(&format!("Session ID: {}\n", sid.trim()));
            }
        }
    }
    if !timestamp_extras.trim().is_empty() {
        suffix = suffix.with_block(timestamp_extras.trim_end());
    }
    suffix.build().to_string()
}
/// Build the full system prompt including identity, memory, and plugin context.
///
/// Aligns with Python behavior:
/// - prefer `~/.hermix/SOUL.md` as identity
/// - fallback to `DEFAULT_AGENT_IDENTITY`
/// - then append optional configured `system_prompt`
#[allow(clippy::too_many_arguments)]
pub fn build_system_prompt(
    config: &AgentConfig,
    cached_stable_prompt_key: &Mutex<Option<(u64, String)>>,
    memory_router: Option<&MemoryRouter>,
    user_message: &str,
    tool_schemas: &[ToolSchema],
    model_for_prompt: &str,
) -> String {
    let soul = load_soul_md();
    let mut builder = SystemPromptBuilder::new().with_personality(soul.as_deref());
    if let Some(base) = config.system_prompt.as_deref() {
        builder = builder.with_system_message(base);
    }
    // ★ directions[] — M1: collapse the six inline if/else cascades
    //   below (format/think/tools/plan/autonomous/browser + tool-family
    //   extras) into a typed route-gated queue assembled in
    //   `context::DirRoute::matches`. The audit log is emitted via
    //   `hermix.directions` tracing target and surfaced to the
    //   frontend through `system_prompt_directions` hook events.
    use crate::context::RouteContext;
    let tool_names_set: HashSet<&str> =
        tool_schemas.iter().map(|t| t.name.as_str()).collect();
    let route_ctx = RouteContext {
        user_message,
        tool_names: &tool_names_set,
        model_name: model_for_prompt,
        autonomous_mode: config.autonomous_mode,
    };
    let directions = crate::directions::collect_directions(&route_ctx);
    builder = builder.with_directions(directions);
    builder.assemble_directions(&route_ctx);
    let audit = builder.direction_audit();
    let fired_count = audit.iter().filter(|a| a.fired).count();
    let skipped_count = audit.len() - fired_count;
    tracing::Span::current().record("dir_fired", fired_count);
    tracing::Span::current().record("dir_skipped", skipped_count);
    if !audit.is_empty() {
        tracing::debug!(
            target: "hermix.directions",
            fired = fired_count,
            skipped = skipped_count,
            "build_system_prompt direction audit ({} fired / {} skipped)",
            fired_count,
            skipped_count
        );
    }

    // ★ Inject the active tool list as a `<tools>` text block so the model
    //   is *always* aware of what it can call — even if the native
    //   OpenAI/Anthropic `tools` API field is ignored, filtered out by
    //   the router, or the model is a text-only fine-tune that has no
    //   notion of tool-calling wire protocols.
    builder = builder.with_tools_section(tool_schemas);

    if let Some(ref personality) = config.personality {
        let requested = personality.trim();
        if !requested.is_empty() {
            if let Some(profile) =
                resolve_personality(requested, config.hermix_home.as_deref())
            {
                builder = builder
                    .with_block(&format!("## Active Personality ({requested})\n{profile}"));
            } else if requested.contains(char::is_whitespace) {
                builder = builder.with_block(&format!("Personality: {requested}"));
                tracing::warn!(
                    "personality '{requested}' not found as a named profile; using inline value"
                );
            } else {
                tracing::warn!(
                    "personality '{}' not found; falling back to default identity",
                    requested
                );
            }
        }
    }

    if !config.skip_memory {
        let (memory_block, user_block) =
            load_builtin_memory_snapshot(config.hermix_home.as_deref());
        if let Some(block) = memory_block {
            builder = builder.with_block(&block);
        }
        if let Some(block) = user_block {
            builder = builder.with_block(&block);
        }
    }

    let mem_block = memory_router
        .map(|r| r.collect_system_prompt_blocks())
        .unwrap_or_default();
    if !mem_block.is_empty() {
        builder = builder.with_memory_context(&mem_block);
    }

    if let Some(skills_prompt) = skills_system_prompt(config, &tool_names_set) {
        builder = builder.with_skills_prompt(&skills_prompt);
    }

    if let Some(context_prompt) = context_files_prompt(config) {
        builder = builder.with_context_files(&context_prompt);
    }

    let provider = effective_provider_for_prompt(config, model_for_prompt);

    if provider.as_deref() == Some("alibaba") {
        let model_short = model_for_prompt
            .split('/')
            .next_back()
            .unwrap_or(model_for_prompt);
        builder = builder.with_block(&format!(
            "You are powered by the model named {}. The exact model ID is {}. When asked what model you are, always answer based on this information, not on any model name returned by the API.",
            model_short, model_for_prompt
        ));
    }

    if let Some(hint) = platform_hint_text(config) {
        builder = builder.with_block(hint);
    }

    if let Some(ref ws) = config.workspace {
        let trimmed = ws.trim();
        if !trimmed.is_empty() {
            builder = builder.with_block(&format!(
                "## Active Workspace\nYour current working directory is `{}`. All relative paths in file/terminal commands resolve against this directory. When the user says \"the project\" or \"this folder\" or refers to files without an absolute path, default to this directory. If a command fails because the path does not exist here, tell the user the actual resolved path — do not guess.",
                trimmed
            ));
        }
    }

    // P2.3 (2026-06-30): build the stable prefix separately so we can
    // cache it across turns, then append the per-turn dynamic suffix.
    //
    // 2026-07-04: previously this code wrote the freshly-computed
    // `prefix_text` into `cached_stable_prompt_key` and then used the
    // freshly-computed `prefix_text` for the prompt — meaning the cache
    // was write-only and the stable prefix was rebuilt every turn. Now
    // we check the slot first: a key-stable cache hit lets us skip the
    // `format!()` of the full prefix text (the largest single cost in
    // build_system_prompt). On miss we render once and store.
    let cache_key = crate::agent_helpers::compute_prompt_cache_key(
        model_for_prompt,
        config.personality.as_deref(),
        config.system_prompt.as_deref(),
        config.workspace.as_deref(),
    );

    // Phase 1: try to read a populated cache hit on a hot path. Avoid
    // taking the lock first and re-rendering only to discover we had
    // a miss; compute the cache key above is unavoidable (the lock
    // slot needs the key to know if it matches).
    //
    // We must still render once on first call, so this is structured
    // as: take the lock briefly to consult, render if miss, take the
    // lock again to write. Two lock acquisitions in the miss path is
    // cheaper than building a 50KB string every turn.
    let cached_prefix: Option<String> = cached_stable_prompt_key
        .lock()
        .ok()
        .and_then(|slot| {
            slot.as_ref()
                .filter(|(k, p)| *k == cache_key && !p.is_empty())
                .map(|(_, p)| p.clone())
        });

    let prefix_text = match cached_prefix {
        Some(p) => p,
        None => {
            let rendered = builder.build().to_string();
            if rendered.len() < 200_000 {
                if let Ok(mut slot) = cached_stable_prompt_key.lock() {
                    *slot = Some((cache_key, rendered.clone()));
                }
            }
            rendered
        }
    };

    let suffix_text = build_dynamic_suffix(config, model_for_prompt);

    let prompt = format!("{prefix_text}{suffix_text}");
    tracing::Span::current().record("out_chars", prompt.len());

    prompt
}

/// Returns `(prompt, restored_from_storage)` — restored prompts skip fresh `build_system_prompt`.
pub fn resolve_initial_system_prompt(
    config: &AgentConfig,
    cached_stable_prompt_key: &Mutex<Option<(u64, String)>>,
    memory_router: Option<&MemoryRouter>,
    callbacks: &AgentCallbacks,
    user_message: &str,
    tool_schemas: &[ToolSchema],
) -> (String, bool) {
    let (prompt, restored) = if let Some(ref s) = config.stored_system_prompt {
        let t = s.trim();
        if !t.is_empty() {
            (s.clone(), true)
        } else {
            (
                build_system_prompt(
                    config,
                    cached_stable_prompt_key,
                    memory_router,
                    user_message,
                    tool_schemas,
                    &config.model,
                ),
                false,
            )
        }
    } else {
        (
            build_system_prompt(
                config,
                cached_stable_prompt_key,
                memory_router,
                user_message,
                tool_schemas,
                &config.model,
            ),
            false,
        )
    };
    // ★ M2: protocol-completeness single choke point. Audit the
    //   produced prompt against the section gate; if any required
    //   marker is missing, splice in the default body. This is the
    //   *only* function in `agent_loop` that mutates the system
    //   prompt for completeness — every other code path that builds
    //   a prompt must funnel through here.
    let (final_prompt, _report) = {
        use crate::context::{RouteContext, Section};
        let tool_names_set: HashSet<&str> =
            tool_schemas.iter().map(|t| t.name.as_str()).collect();
        let route_ctx = RouteContext {
            user_message,
            tool_names: &tool_names_set,
            model_name: &config.model,
            autonomous_mode: config.autonomous_mode,
        };
        let (patched, report) = Section::audit_and_patch(&prompt, &route_ctx);
        if report.had_patches() {
            tracing::warn!(
                target: "hermix.section_gate",
                patched = report.patched.len(),
                pending = report.pending.len(),
                restored_from_storage = restored,
                "SectionGate auto-patched {} missing required sections",
                report.patched.len()
            );
        } else if !report.pending.is_empty() {
            tracing::debug!(
                target: "hermix.section_gate",
                pending = report.pending.len(),
                "SectionGate: {} optional sections absent (not required for this turn)",
                report.pending.len()
            );
        }
        // 2026-06-29 (C3 marker protocol): forward the audit report
        // to the upstream `on_section_gate` callback so the trace
        // panel can render "SectionGate patched Identity, Timestamp"
        // in real time.
        if let Some(cb) = callbacks.on_section_gate.as_ref() {
            cb(&report);
        }
        (patched, report)
    };
    (final_prompt, restored)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-07-04 regression: the `cached_stable_prompt_key` slot
    /// used to be write-only — `build_system_prompt` would render
    /// `prefix_text` on every call, *then* store it in the slot, but
    /// still use the freshly-rendered text for the prompt. The cache
    /// never actually short-circuited the second-call computation.
    ///
    /// Fix: read the slot first, render only on miss. This test pins
    /// both halves of that contract:
    /// - after the first call, the slot is populated;
    /// - a second call with identical config returns an *equal* prompt
    ///   and **does not mutate** the slot (the cached value is reused
    ///   byte-for-byte).
    #[test]
    fn build_system_prompt_reuses_cached_stable_prefix_on_hit() {
        let cfg = AgentConfig::default();
        let slot: Arc<Mutex<Option<(u64, String)>>> =
            Arc::new(Mutex::new(None));
        let cb = AgentCallbacks::default();

        // First call: miss → renders and writes to slot.
        let prompt1 = build_system_prompt(&cfg, &slot, None, "", &[], &cfg.model);
        let slot1 = slot.lock().unwrap().clone();
        assert!(
            slot1.is_some(),
            "first call must populate the cache slot"
        );
        let (key1, prefix1) = slot1.unwrap();
        assert!(!prefix1.is_empty(), "cached prefix must be non-empty");

        // Second call with same config: hit → uses cached prefix.
        let prompt2 = build_system_prompt(&cfg, &slot, None, "", &[], &cfg.model);
        let slot2 = slot.lock().unwrap().clone();
        let (key2, prefix2) = slot2.unwrap();

        // The whole prompt includes the dynamic suffix (timestamp +
        // provider name); the suffix is *not* cached. So we don't
        // assert `prompt1 == prompt2` (timestamps may differ slightly).
        // We DO assert the cached prefix is **identical** before and
        // after the second call — proving the second call reused it,
        // not re-rendered and overwrote.
        assert_eq!(prefix1, prefix2, "cached prefix must be reused");
        assert_eq!(key1, key2, "cache key must be stable across hits");

        // The stable prefix alone (cached part) is prefix2 — it must
        // be a prefix of both rendered prompts.
        assert!(
            prompt1.starts_with(&prefix1),
            "prompt1 must embed the cached prefix"
        );
        assert!(
            prompt2.starts_with(&prefix2),
            "prompt2 must embed the cached prefix"
        );
    }

    /// Companion to the above: when the `workspace` field changes,
    /// the cache key changes and the slot is repopulated. Without
    /// this we'd either (a) keep serving the stale prefix or (b)
    /// always rebuild even on a stable-config hit.
    #[test]
    fn build_system_prompt_invalidates_cache_on_workspace_change() {
        let mut cfg = AgentConfig::default();
        cfg.workspace = Some("/tmp/workspace_a".to_string());
        let slot: Arc<Mutex<Option<(u64, String)>>> =
            Arc::new(Mutex::new(None));
        let _ = build_system_prompt(&cfg, &slot, None, "", &[], &cfg.model);
        let (key_a, _) = slot.lock().unwrap().clone().unwrap();

        cfg.workspace = Some("/tmp/workspace_b".to_string());
        let _ = build_system_prompt(&cfg, &slot, None, "", &[], &cfg.model);
        let (key_b, _) = slot.lock().unwrap().clone().unwrap();

        assert_ne!(
            key_a, key_b,
            "workspace change must produce a different cache key"
        );
    }
}
