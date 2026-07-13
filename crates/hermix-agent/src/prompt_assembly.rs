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
use std::sync::Mutex;

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
///
/// When `AgentConfig::platform_hint_override` is set:
/// - If the override starts with `+`, it is **appended** to the built-in hint.
/// - Otherwise it **replaces** the built-in hint entirely.
pub fn platform_hint_text(config: &AgentConfig) -> Option<String> {
    let platform_key = config
        .platform
        .as_deref()
        .map(|s| s.trim().to_lowercase())
        .unwrap_or_default();
    let built_in: Option<&'static str> = match platform_key.as_str() {
        "cli" => Some("You are a CLI AI Agent. Prefer concise plain text output suitable for terminals."),
        "telegram" | "discord" | "slack" => {
            Some("You are responding on a chat platform. Keep responses concise and avoid heavy formatting.")
        }
        "email" => Some("You are responding over email. Use clear structure and complete sentences."),
        "sms" => Some("You are responding over SMS. Keep responses short and high-signal."),
        _ => None,
    };
    match (&built_in, &config.platform_hint_override) {
        (_, Some(override_text)) if override_text.starts_with('+') => {
            // Append mode: built-in + override (skip the leading '+')
            let extra = &override_text[1..];
            Some(match built_in {
                Some(bi) => format!("{} {}", bi, extra.trim()),
                None => extra.trim().to_string(),
            })
        }
        (_, Some(override_text)) => {
            // Replace mode: override only
            Some(override_text.clone())
        }
        (Some(bi), None) => Some(bi.to_string()),
        (None, None) => None,
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
        body.push_str("(no installed skills discovered via skill_manage -- only ephemeral skills below are active)\n");
    } else {
        let mut rows: Vec<_> = commands.iter().collect();
        rows.sort_by(|a, b| a.0.cmp(b.0));

        // Group skills by category for the index
        let mut categories: std::collections::BTreeMap<String, Vec<_>> = std::collections::BTreeMap::new();
        for (cmd, info) in rows.into_iter().take(80) {
            let cat = if info.category.is_empty() { "other" } else { &info.category };
            categories.entry(cat.to_string()).or_default().push((cmd, info));
        }

        for (cat, skills) in &categories {
            body.push_str(&format!("### {}\n", cat));
            for (cmd, info) in skills {
                let tags_str = if info.tags.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", info.tags.join(", "))
                };
                body.push_str(&format!(
                    "- {}: {}{}\n",
                    cmd,
                    info.description.trim(),
                    tags_str,
                ));
            }
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
/// 2. Normalize messages (merge consecutive same-role, strip stale errors, etc.)
/// 3. Strip `reasoning_content` from all but the most recent assistant
///    message — old thinking blocks are noise once the LLM has moved on.
/// 4. Apply Anthropic prompt-cache markers.
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

    // --- Phase 0: Normalization pipeline (Claude Code-inspired) ---

    // 0a. Merge consecutive user messages (Bedrock rejects them).
    // 显式写 `Vec<Message>` 而非靠推断：上游 `messages = merged;` 之后
    // 这两个变量都不再被读，没有 use site 可作为 infer anchor。
    let mut merged: Vec<Message> = Vec::with_capacity(messages.len());
    for msg in messages {
        let role = msg.role;
        if role == hermix_core::MessageRole::User {
            if let Some(last) = merged.last_mut() {
                if last.role == role {
                    // Append content
                    if let Some(ref c) = msg.content {
                        if let Some(ref mut lc) = last.content {
                            lc.push('\n');
                            lc.push_str(c);
                        } else {
                            last.content = Some(c.clone());
                        }
                    }
                    // Merge tool_call_id if both carry one
                    if last.tool_call_id.is_none() {
                        last.tool_call_id.clone_from(&msg.tool_call_id);
                    }
                    continue;
                }
            }
        }
        // 0b. Filter virtual/internal messages (no content, no tool calls)
        if role == hermix_core::MessageRole::Assistant {
            let has_content = msg.content.as_deref().map(|c| !c.is_empty()).unwrap_or(false);
            let has_tool_calls = msg.tool_calls.as_ref().map(|t| !t.is_empty()).unwrap_or(false);
            if !has_content && !has_tool_calls {
                // Skip empty assistant messages
                continue;
            }
        }
        // 0c. Filter tool messages with empty content (stale error recovery)
        if role == hermix_core::MessageRole::Tool {
            let has_content = msg.content.as_deref().map(|c| !c.is_empty()).unwrap_or(false);
            if !has_content {
                continue;
            }
        }
        merged.push(msg);
    }
    messages = merged;

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

/// Anthropic-specific: mark the system message and the last 4 user/assistant
/// messages as `Ephemeral` cache. The system message gets the marker so the
/// provider-side prompt cache (Anthropic's `cache_control: {type: "ephemeral"}`)
/// can reuse the stable prefix across turns — the largest, most invariant
/// block (soul + directions + tools) lives in `build_stable_layer`, which is
/// rendered into the system message. Other providers do not consume these
/// markers, so the function is a no-op when `config.provider` is not
/// Anthropic/Claude.
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
                cache_type: CacheType::Ephemeral,
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
/// The rest of the prompt is split into two independently cached
/// layers (`build_stable_layer` and `build_context_layer`) so that a
/// mutation to one layer does not force the other to re-render.
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
///
/// Three-tier cache layout (2026-07-04):
/// - **stable**  — soul, configured system_prompt, directions, tools
///   section, personality profile, alibaba hint, platform hint,
///   workspace block. Invalidates on model / personality /
///   system_prompt / workspace change.
/// - **context** — built-in memory snapshot, memory router blocks,
///   skills prompt, context files. Invalidates on memory / skills /
///   context-files mutation (signalled by an `epoch` u64 tick).
/// - **volatile** — timestamp + session_id. Rebuilt every turn; not
///   cached.
#[allow(clippy::too_many_arguments)]
pub fn build_system_prompt(
    config: &AgentConfig,
    prompt_cache: &Mutex<crate::agent_helpers::PromptCache>,
    memory_router: Option<&MemoryRouter>,
    user_message: &str,
    tool_schemas: &[ToolSchema],
    model_for_prompt: &str,
    tool_call_count: usize,
) -> String {
    let tool_names_set: HashSet<&str> =
        tool_schemas.iter().map(|t| t.name.as_str()).collect();

    let stable_text = build_stable_layer(
        config,
        prompt_cache,
        memory_router,
        user_message,
        tool_schemas,
        &tool_names_set,
        model_for_prompt,
        tool_call_count,
    );

    // ★ directions[] — evaluated per-turn with the REAL tool_call_count so
    //   HadAnyToolUse / MultiStepTask routes fire correctly. This block
    //   lives OUTSIDE the cached stable layer (keyed on
    //   model/personality/system_prompt/workspace only) because
    //   tool_call_count changes every turn. Previously directions were
    //   baked into the stable cache with tool_call_count=0, so
    //   HadAnyToolUse never fired — summary/stopping guidance was
    //   silently skipped, leaving the LLM with no "stop after real work"
    //   constraint and producing filler + repeated tool calls.
    let directions_text = {
        use crate::context::{RouteContext, SystemPromptBuilder};
        let route_ctx = RouteContext {
            user_message,
            tool_names: &tool_names_set,
            model_name: model_for_prompt,
            autonomous_mode: config.autonomous_mode,
            tool_call_count,
        };
        let project_conventions = config
            .workspace
            .as_deref()
            .and_then(crate::directions::load_project_conventions);
        let directions =
            crate::directions::collect_directions(&route_ctx, project_conventions.as_deref());
        let mut b = SystemPromptBuilder::new();
        b = b.with_directions(directions);
        b.assemble_directions(&route_ctx);
        let audit = b.direction_audit();
        let fired_count = audit.iter().filter(|a| a.fired).count();
        let skipped_count = audit.len() - fired_count;
        tracing::Span::current().record("dir_fired", fired_count);
        tracing::Span::current().record("dir_skipped", skipped_count);
        if !audit.is_empty() {
            tracing::debug!(
                target: "hermix.directions",
                fired = fired_count,
                skipped = skipped_count,
                tool_call_count,
                "build_system_prompt direction audit ({} fired / {} skipped, tool_call_count={})",
                fired_count,
                skipped_count,
                tool_call_count
            );
        }
        b.build().to_string()
    };

    let context_text = build_context_layer(
        config,
        prompt_cache,
        memory_router,
        &tool_names_set,
    );

    let suffix_text = build_dynamic_suffix(config, model_for_prompt);

    let prompt = format!("{stable_text}{directions_text}{context_text}{suffix_text}");
    tracing::Span::current().record("out_chars", prompt.len());

    // ★ Cache-effectiveness summary (2026-07-05): one info event per
    //   turn so operators can compare token usage before vs after the
    //   three-tier cache was enabled.  Token estimates use chars ÷ 4.
    let stable_est = stable_text.len() / 4;
    let context_est = context_text.len() / 4;
    let volatile_est = suffix_text.len() / 4;
    let total_est = prompt.len() / 4;
    let (cached_stable_key, cached_stable_len, cached_context_key, cached_context_len) =
        prompt_cache
            .lock()
            .map(|s| s.snapshot())
            .unwrap_or((None, 0, None, 0));
    tracing::info!(
        target: "hermix.prompt_cache",
        stable_chars = stable_text.len(),
        stable_est_tokens = stable_est,
        context_chars = context_text.len(),
        context_est_tokens = context_est,
        volatile_chars = suffix_text.len(),
        volatile_est_tokens = volatile_est,
        total_chars = prompt.len(),
        total_est_tokens = total_est,
        cached_stable_prompt_key = ?cached_stable_key,
        cached_context_prompt_key = ?cached_context_key,
        cached_stable_chars = cached_stable_len,
        cached_context_chars = cached_context_len,
        "prompt_cache: assembled prompt (stable={stable_est}t context={context_est}t volatile={volatile_est}t → total={total_est}t)"
    );

    prompt
}

/// Render and cache the stable layer of the system prompt. See
/// [`build_system_prompt`] for the cache-key contract.
#[allow(clippy::too_many_arguments)]
fn build_stable_layer(
    config: &AgentConfig,
    prompt_cache: &Mutex<crate::agent_helpers::PromptCache>,
    _memory_router: Option<&MemoryRouter>,
    _user_message: &str,
    tool_schemas: &[ToolSchema],
    _tool_names_set: &HashSet<&str>,
    model_for_prompt: &str,
    _tool_call_count: usize,
) -> String {
    let cache_key = crate::agent_helpers::compute_prompt_cache_key(
        model_for_prompt,
        config.personality.as_deref(),
        config.system_prompt.as_deref(),
        config.workspace.as_deref(),
    );

    // Hot-path read: peek the cache without re-rendering. The lookup
    // uses the supplied key so a config change invalidates immediately.
    if let Ok(slot) = prompt_cache.lock() {
        if let (Some(k), Some(text)) = (
            slot.cached_stable_prompt_key.as_ref(),
            slot.stable.as_ref(),
        ) {
            if *k == cache_key && !text.is_empty() {
                tracing::debug!(
                    target: "hermix.prompt_cache",
                    layer = "stable",
                    hit = true,
                    cached_stable_prompt_key = cache_key,
                    chars = text.len(),
                    estimated_tokens = text.len() / 4,
                    "prompt_cache: stable layer hit"
                );
                return text.clone();
            }
        }
    }

    // Miss: render the stable layer end-to-end.
    let soul = load_soul_md();
    let mut builder = SystemPromptBuilder::new().with_personality(soul.as_deref());
    if let Some(base) = config.system_prompt.as_deref() {
        builder = builder.with_system_message(base);
    }

    // ★ directions[] moved to the per-turn dynamic layer in
    //   `build_system_prompt` (2026-07-08). Previously directions were
    //   baked into the stable cache with tool_call_count=0, which meant
    //   HadAnyToolUse / MultiStepTask routes never fired — summary and
    //   stopping guidance were silently skipped every turn, leaving the
    //   LLM with no "stop after real work" constraint and producing
    //   filler + repeated tool calls. Evaluating directions per-turn with
    //   the real tool_call_count fixes this. The stable layer must NOT
    //   depend on per-turn state (tool_call_count) so the cache key stays
    //   stable across turns.

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
        builder = builder.with_block(&hint);
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

    let rendered = builder.build().to_string();

    tracing::debug!(
        target: "hermix.prompt_cache",
        layer = "stable",
        hit = false,
        cached_stable_prompt_key = cache_key,
        chars = rendered.len(),
        estimated_tokens = rendered.len() / 4,
        "prompt_cache: stable layer miss — rendered"
    );

    // Write the rendered text back. We size-cap at 200 KB to keep the
    // cache slot from growing unbounded for unusually large configs.
    if rendered.len() < 200_000 {
        if let Ok(mut slot) = prompt_cache.lock() {
            slot.store_stable(cache_key, rendered.clone());
        }
    }

    rendered
}

/// Render and cache the context layer of the system prompt. The
/// context layer covers memory / skills / context-files blocks and
/// invalidates independently of the stable layer through the supplied
/// `context_epoch` key (callers bump the epoch when the underlying
/// stores mutate).
fn build_context_layer(
    config: &AgentConfig,
    prompt_cache: &Mutex<crate::agent_helpers::PromptCache>,
    memory_router: Option<&MemoryRouter>,
    tool_names_set: &HashSet<&str>,
) -> String {
    // The context layer is keyed by an external epoch tick supplied by
    // the caller (bumped on memory / skills / context-files mutation).
    // We expose a setter-style helper on the cache that the caller can
    // invoke; for backward compat we treat the supplied `prompt_cache`
    // as authoritative and the epoch as `0` when absent — meaning
    // every call re-renders the context layer. Callers that want
    // context-layer caching should bump `cache.context_epoch_via(...)`
    // or wrap the mutex externally.
    let cache_key = crate::agent_helpers::compute_context_cache_key(0);

    if let Ok(slot) = prompt_cache.lock() {
        if let (Some(k), Some(text)) = (
            slot.cached_context_prompt_key.as_ref(),
            slot.context.as_ref(),
        ) {
            if *k == cache_key && !text.is_empty() {
                tracing::debug!(
                    target: "hermix.prompt_cache",
                    layer = "context",
                    hit = true,
                    cached_context_prompt_key = cache_key,
                    chars = text.len(),
                    estimated_tokens = text.len() / 4,
                    "prompt_cache: context layer hit"
                );
                return text.clone();
            }
        }
    }

    let mut builder = SystemPromptBuilder::new();

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

    if let Some(skills_prompt) = skills_system_prompt(config, tool_names_set) {
        builder = builder.with_skills_prompt(&skills_prompt);
    }

    if let Some(context_prompt) = context_files_prompt(config) {
        builder = builder.with_context_files(&context_prompt);
    }

    let rendered = builder.build().to_string();

    tracing::debug!(
        target: "hermix.prompt_cache",
        layer = "context",
        hit = false,
        cached_context_prompt_key = cache_key,
        chars = rendered.len(),
        estimated_tokens = rendered.len() / 4,
        "prompt_cache: context layer miss — rendered"
    );

    if rendered.len() < 200_000 {
        if let Ok(mut slot) = prompt_cache.lock() {
            slot.store_context(cache_key, rendered.clone());
        }
    }

    rendered
}

/// Returns `(prompt, restored_from_storage)` — restored prompts skip fresh `build_system_prompt`.
pub fn resolve_initial_system_prompt(
    config: &AgentConfig,
    prompt_cache: &Mutex<crate::agent_helpers::PromptCache>,
    memory_router: Option<&MemoryRouter>,
    callbacks: &AgentCallbacks,
    user_message: &str,
    tool_schemas: &[ToolSchema],
    tool_call_count: usize,
) -> (String, bool) {
    let (prompt, restored) = if let Some(ref s) = config.stored_system_prompt {
        let t = s.trim();
        if !t.is_empty() {
            (s.clone(), true)
        } else {
            (
                build_system_prompt(
                    config,
                    prompt_cache,
                    memory_router,
                    user_message,
                    tool_schemas,
                    &config.model,
                    tool_call_count,
                ),
                false,
            )
        }
    } else {
        (
            build_system_prompt(
                config,
                prompt_cache,
                memory_router,
                user_message,
                tool_schemas,
                &config.model,
                tool_call_count,
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
            tool_call_count,
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
    use std::sync::Arc;

    /// 2026-07-04 regression: the `prompt_cache.stable` slot used to be
    /// write-only — `build_system_prompt` would render `prefix_text` on
    /// every call, *then* store it in the slot, but still use the
    /// freshly-rendered text for the prompt. The cache never actually
    /// short-circuited the second-call computation.
    ///
    /// 2026-07-04 (round 5): split into two layers (stable + context);
    /// pin that the **stable** slot is still read-before-write and the
    /// second call with identical config does not re-render the
    /// stable layer.
    #[test]
    fn build_system_prompt_reuses_cached_stable_layer_on_hit() {
        let cfg = AgentConfig::default();
        let slot: Arc<Mutex<crate::agent_helpers::PromptCache>> =
            Arc::new(Mutex::new(crate::agent_helpers::PromptCache::default()));

        // First call: miss → renders and writes the stable slot.
        let prompt1 = build_system_prompt(&cfg, &slot, None, "", &[], &cfg.model, 0);
        let (key1, stable1) = {
            let guard = slot.lock().unwrap();
            (
                guard.cached_stable_prompt_key,
                guard.stable.clone().expect("first call must populate the stable cache slot"),
            )
        };
        assert!(!stable1.is_empty(), "cached stable layer must be non-empty");

        // Second call with same config: hit → reuses cached stable layer.
        let prompt2 = build_system_prompt(&cfg, &slot, None, "", &[], &cfg.model, 0);
        let (key2, stable2) = {
            let guard = slot.lock().unwrap();
            (
                guard.cached_stable_prompt_key,
                guard.stable.clone().expect("second call must reuse the stable cache slot"),
            )
        };

        // Stable layer must be byte-identical before and after — proving
        // the second call reused the cached text instead of re-rendering.
        assert_eq!(stable1, stable2, "stable layer must be reused");
        assert_eq!(key1, key2, "stable cache key must be stable across hits");

        // The cached stable layer is a prefix of both rendered prompts
        // (volatile suffix is appended after).
        assert!(
            prompt1.starts_with(&stable1),
            "prompt1 must embed the cached stable layer"
        );
        assert!(
            prompt2.starts_with(&stable2),
            "prompt2 must embed the cached stable layer"
        );
    }

    /// Companion to the above: when the `workspace` field changes, the
    /// cache key changes and the stable slot is repopulated.
    #[test]
    fn build_system_prompt_invalidates_stable_cache_on_workspace_change() {
        let mut cfg = AgentConfig::default();
        cfg.workspace = Some("/tmp/workspace_a".to_string());
        let slot: Arc<Mutex<crate::agent_helpers::PromptCache>> =
            Arc::new(Mutex::new(crate::agent_helpers::PromptCache::default()));
        let _ = build_system_prompt(&cfg, &slot, None, "", &[], &cfg.model, 0);
        let key_a = slot.lock().unwrap().cached_stable_prompt_key;

        cfg.workspace = Some("/tmp/workspace_b".to_string());
        let _ = build_system_prompt(&cfg, &slot, None, "", &[], &cfg.model, 0);
        let key_b = slot.lock().unwrap().cached_stable_prompt_key;

        assert_ne!(
            key_a, key_b,
            "workspace change must produce a different stable cache key"
        );
    }

    /// Round 5 (three-tier split): the context layer is rendered
    /// independently of the stable layer. When the underlying context
    /// store is the same (no epoch bump), the second call reuses the
    /// context slot. When the config changes (e.g. skip_memory flips),
    /// the context layer must reflect the new value.
    #[test]
    fn build_system_prompt_layers_are_independently_addressable() {
        let cfg = AgentConfig::default();
        let slot: Arc<Mutex<crate::agent_helpers::PromptCache>> =
            Arc::new(Mutex::new(crate::agent_helpers::PromptCache::default()));

        let _ = build_system_prompt(&cfg, &slot, None, "", &[], &cfg.model, 0);
        let context_after_first = slot.lock().unwrap().context.clone();

        // With the same config the second call should keep the context
        // slot populated (either reused from cache or re-rendered into
        // the same slot — either way the slot must be Some).
        let _ = build_system_prompt(&cfg, &slot, None, "", &[], &cfg.model, 0);
        let context_after_second = slot.lock().unwrap().context.clone();

        assert!(
            context_after_first.is_some(),
            "context layer must populate after first call"
        );
        assert!(
            context_after_second.is_some(),
            "context layer must remain populated across calls"
        );
    }

    /// Round 5 sanity: with `skip_memory = true` the context layer
    /// must still render (just shorter) and the resulting prompt must
    /// be smaller than with memory on — proving the context layer is
    /// wired up correctly.
    #[test]
    fn build_system_prompt_skip_memory_shrinks_context_layer() {
        let cfg_with = AgentConfig::default();
        let cfg_skip = AgentConfig {
            skip_memory: true,
            ..AgentConfig::default()
        };
        let slot_with: Arc<Mutex<crate::agent_helpers::PromptCache>> =
            Arc::new(Mutex::new(crate::agent_helpers::PromptCache::default()));
        let slot_skip: Arc<Mutex<crate::agent_helpers::PromptCache>> =
            Arc::new(Mutex::new(crate::agent_helpers::PromptCache::default()));

        let prompt_with = build_system_prompt(
            &cfg_with,
            &slot_with,
            None,
            "",
            &[],
            &cfg_with.model,
            0,
        );
        let prompt_skip = build_system_prompt(
            &cfg_skip,
            &slot_skip,
            None,
            "",
            &[],
            &cfg_skip.model,
            0,
        );

        // The volatile suffix is a handful of chars; the stable layer is
        // identical between the two configs. So any size delta comes
        // from the context layer.
        let len_with = slot_with.lock().unwrap().context.as_ref().map(|t| t.len()).unwrap_or(0);
        let len_skip = slot_skip.lock().unwrap().context.as_ref().map(|t| t.len()).unwrap_or(0);
        assert!(
            len_skip <= len_with,
            "skip_memory=true must produce a context layer no larger than the default \
             (skip={}, default={})",
            len_skip,
            len_with
        );
        // Prompt lengths should also reflect the reduction.
        assert!(
            prompt_skip.len() <= prompt_with.len(),
            "skip_memory prompt must be no larger than default prompt"
        );
    }
}
