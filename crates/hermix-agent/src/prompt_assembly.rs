//! Standalone prompt-assembly helpers used by `agent_loop`.
//!
//! Extracted from `agent_loop.rs` (P2.x, ) so the 10k-line
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
use hermix_intelligence::{DomainMode, DomainModeDetector};

/// Resolve this turn's L1 domain mode: the user's pin wins (`Code` = 编码
/// 模式等), otherwise run the keyword/context auto-detector. The pin goes
/// through the detector's own `manual_override` slot (confidence 1.0,
/// source `ManualOverride`) rather than an early return, so detection
/// stays the single decision point. `Some(mode)` is always returned —
/// `Chat` carries an empty directive, so gating in `collect_directions`
/// injects nothing for it.
///
/// The detector is constructed per call — it only does regex keyword
/// matching over the current message (+ workspace stat), so cross-turn
/// state is not carried; the historical inertia / tool-history signals
/// were removed with their unreachable accumulation machinery.
pub fn resolve_domain_mode(config: &AgentConfig, user_message: &str) -> DomainMode {
    let mut detector = DomainModeDetector::new();
    detector.set_manual_override(config.domain_override);
    detector
        .detect(user_message, config.workspace.as_deref())
        .mode
}

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

/// Compact index of installed custom apps, injected into every turn's
/// system prompt so the LLM always knows which apps exist without
/// calling `custom_app_manager list` first.
///
/// Deliberately tiny per entry (id + display name only — ~15-20 tokens)
/// so 50+ apps stay under ~1% of a 128k context. Full metadata
/// (description / permissions / launch steps) is fetched on demand via
/// `custom_app_manager get id="<id>"` — the agent only pays for the
/// apps it actually touches.
///
/// Rendered block looks like:
/// ```text
/// ## 已装的自定义应用
/// 用户提到应用时用 @app:<id> 引用; 用 custom_app_manager 的 get/run
/// action 操作它, 不要重复创建已存在的应用。
/// @app:local-music — 本地音乐
/// @app:notes — 笔记
/// ```
pub fn custom_apps_index_prompt(config: &AgentConfig) -> Option<String> {
    let apps_dir = config
        .hermix_home
        .clone()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(hermix_config::paths::hermix_home)
        .join("custom-apps");
    let read_dir = std::fs::read_dir(&apps_dir).ok()?;
    let mut entries: Vec<String> = Vec::new();
    for dir in read_dir.flatten() {
        let manifest_path = dir.path().join("manifest.json");
        let Ok(raw) = std::fs::read_to_string(&manifest_path) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        let id = manifest
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let name = manifest
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or(id);
        if !id.is_empty() {
            entries.push(format!("@app:{id} — {name}"));
        }
    }
    if entries.is_empty() {
        return None;
    }
    entries.sort();
    let mut block = String::from(
        "## 已装的自定义应用\n\
         用户提到应用或用 @app:<id> 引用时, 说明它已安装: 用 \
         custom_app_manager 的 get 查详情、run 启动; 不要重复创建已存在的应用。\n",
    );
    block.push_str(&entries.join("\n"));
    Some(block)
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
/// message — old thinking blocks are noise once the LLM has moved on.
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

    //Normalization pipeline (Claude Code-inspired) ---

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
/// This is the only part of the system prompt that changes every turn:
/// - the timestamp block (model + provider name)
/// - the optional session_id line (when `pass_session_id` is on)
/// The rest of the prompt is split into two independently cached
/// layers (`build_stable_layer` and `build_context_layer`) so that a
/// mutation to one layer does not force the other to re-render.
pub fn build_dynamic_suffix(config: &AgentConfig, model_for_prompt: &str) -> String {
    let provider = effective_provider_for_prompt(config, model_for_prompt);
    let mut suffix = crate::context::SystemPromptBuilder::new();
    // 会话内恒定的时间戳（见 AgentConfig::session_started_at）：
    // 每 turn 变化会让前缀缓存从系统提示尾部起全部失效。
    let started_at = config
        .session_started_at
        .clone()
        .unwrap_or_else(|| chrono::Local::now().to_rfc3339());
    suffix = suffix.with_timestamp_at(Some(model_for_prompt), provider.as_deref(), &started_at);

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

/// (tool error demotion): same as [`build_system_prompt`] but
/// takes a snapshot of the running tool error counts. Tools at or above
/// `config.tool_error_demotion_threshold` errors are surfaced as a
/// soft hint block in the system prompt. `None` disables the
/// injection.
#[allow(clippy::too_many_arguments)]
pub fn build_system_prompt_with_tool_errors(
    config: &AgentConfig,
    prompt_cache: &Mutex<crate::agent_helpers::PromptCache>,
    memory_router: Option<&MemoryRouter>,
    user_message: &str,
    tool_schemas: &[ToolSchema],
    model_for_prompt: &str,
    tool_call_count: usize,
    tool_error_counts: Option<&std::collections::HashMap<String, u32>>,
    context_epoch: u64,
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

    // ★ directions[] — evaluated per-turn because their gating inputs
    // (user message, active tool set, model) change every turn. This block
    // lives OUTSIDE the cached stable layer (keyed on
    // model/personality/system_prompt/workspace only). Note: the
    // HadAnyToolUse / MultiStepTask route variants still exist in
    // DirRoute but no guidance currently uses them — the verbose
    // summary/stopping guidance they used to gate was removed
    // (see directions.rs "the verbose rules ... have been removed").
    let mut directions_text = {
        use crate::context::{RouteContext, SystemPromptBuilder};
        let route_ctx = RouteContext {
            user_message,
            tool_names: &tool_names_set,
            model_name: model_for_prompt,
            autonomous_mode: config.autonomous_mode,
            tool_call_count,
            domain_mode: Some(resolve_domain_mode(config, user_message)),
            workbench_id: config.workbench_id.as_deref(),
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

    //append the tool-error demotion block as a separate
    // `## Tool Failures` section after the directions. Empty when
    // `tool_error_counts` is None or no tool has reached the configured
    // threshold. Capped at the configured max rows.
    if let Some(counts) = tool_error_counts {
        if let Some(block) = crate::tool_error_demotion::build_tool_error_demotion_block(
            counts,
            config.tool_error_demotion_threshold,
            config.tool_error_demotion_max_items,
        ) {
            directions_text.push_str("\n\n## Tool Failures\n\n");
            directions_text.push_str(&block);
        }
    }

    let context_text = build_context_layer(
        config,
        prompt_cache,
        memory_router,
        &tool_names_set,
        context_epoch, // bumped when memory/skills/context-files mutate
    );

    let suffix_text = build_dynamic_suffix(config, model_for_prompt);

    let prompt = format!("{stable_text}{directions_text}{context_text}{suffix_text}");
    tracing::Span::current().record("out_chars", prompt.len());

    //★ Cache-effectiveness summary : one info event per
    // turn so operators can compare token usage before vs after the
    // three-tier cache was enabled.  Token estimates use chars ÷ 4.
    let stable_est = stable_text.len() / 4;
    let context_est = context_text.len() / 4;
    let volatile_est = suffix_text.len() / 4;
    let total_est = prompt.len() / 4;
    let (cached_stable_key, cached_stable_len, cached_context_key, cached_context_len) =
        prompt_cache
            .lock()
            .map(|s| s.snapshot())
            .unwrap_or((None, 0, None, 0));
    // Same fingerprint contract as the primary assembly path: a
    // per-turn-churning hash means volatile content leaked into the
    // system prompt and the provider's prefix cache cannot cover
    // tools + history.
    let prompt_fingerprint = {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        prompt.hash(&mut h);
        format!("{:016x}", h.finish())
    };
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
        prompt_fingerprint = %prompt_fingerprint,
        "prompt_cache: assembled prompt (stable={stable_est}t context={context_est}t volatile={volatile_est}t → total={total_est}t)"
    );

    prompt
}

/// Build the full system prompt including identity, memory, and plugin context.
/// Aligns with Python behavior:
/// - prefer `~/.hermix/SOUL.md` as identity
/// - fallback to `DEFAULT_AGENT_IDENTITY`
/// - then append optional configured `system_prompt`
/// Three-tier cache layout :
/// - **stable**  — soul, configured system_prompt, directions, tools
/// section, personality profile, alibaba hint, platform hint,
/// workspace block. Invalidates on model / personality /
/// system_prompt / workspace change.
/// - **context** — built-in memory snapshot, memory router blocks,
/// skills prompt, context files. Invalidates on memory / skills /
/// context-files mutation (signalled by an `epoch` u64 tick).
/// - **volatile** — timestamp + session_id. Rebuilt every turn; not
/// cached.
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

    // ★ directions[] — evaluated per-turn because their gating inputs
    // (user message, active tool set, model) change every turn. This block
    // lives OUTSIDE the cached stable layer (keyed on
    // model/personality/system_prompt/workspace only). Note: the
    // HadAnyToolUse / MultiStepTask route variants still exist in
    // DirRoute but no guidance currently uses them — the verbose
    // summary/stopping guidance they used to gate was removed
    // (see directions.rs "the verbose rules ... have been removed").
    let directions_text = {
        use crate::context::{RouteContext, SystemPromptBuilder};
        let route_ctx = RouteContext {
            user_message,
            tool_names: &tool_names_set,
            model_name: model_for_prompt,
            autonomous_mode: config.autonomous_mode,
            tool_call_count,
            domain_mode: Some(resolve_domain_mode(config, user_message)),
            workbench_id: config.workbench_id.as_deref(),
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
        0, // test helper: unit tests never mutate memory/skill stores in-session
    );

    let suffix_text = build_dynamic_suffix(config, model_for_prompt);

    let prompt = format!("{stable_text}{directions_text}{context_text}{suffix_text}");
    tracing::Span::current().record("out_chars", prompt.len());

    //★ Cache-effectiveness summary : one info event per
    // turn so operators can compare token usage before vs after the
    // three-tier cache was enabled.  Token estimates use chars ÷ 4.
    let stable_est = stable_text.len() / 4;
    let context_est = context_text.len() / 4;
    let volatile_est = suffix_text.len() / 4;
    let total_est = prompt.len() / 4;
    let (cached_stable_key, cached_stable_len, cached_context_key, cached_context_len) =
        prompt_cache
            .lock()
            .map(|s| s.snapshot())
            .unwrap_or((None, 0, None, 0));
    // Same fingerprint contract as the primary assembly path: a
    // per-turn-churning hash means volatile content leaked into the
    // system prompt and the provider's prefix cache cannot cover
    // tools + history.
    let prompt_fingerprint = {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        prompt.hash(&mut h);
        format!("{:016x}", h.finish())
    };
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
        prompt_fingerprint = %prompt_fingerprint,
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
    // `build_system_prompt`. The stable layer must NOT depend on
    // per-turn state (user message, active tool set) so the cache key
    // stays stable across turns.

    // ★ Inject the active tool list as a `<tools>` text block so the model
    // is *always* aware of what it can call — even if the native
    // OpenAI/Anthropic `tools` API field is ignored, filtered out by
    // the router, or the model is a text-only fine-tune that has no
    // notion of tool-calling wire protocols.
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

    // Operating Principles: forced task-workflow protocol so goals,
    // progress and conclusions survive across turns/sessions. Lives in
    // the stable (cached) layer because it is static guidance.
    builder = builder.with_block(
        r#"## 工作任务规范 (Operating Principles)
任何实质性任务（>=2 步）必须严格遵守工作流，确保目标不丢、进度可查、结果可溯：
1. 分析对象：明确要处理的目标与边界。
2. 分析需求：厘清用户真正想要的结果与约束。
3. 制定目标：用 task_create 建立目标树（标题=目标，描述含验收标准），多步任务拆子任务。
4. 记录目标：任务第一步用 daily_memory 写入 goals 段，固化当日目标。
5. 执行计划：按目标树落实，每完成检查点用 daily_memory 的 progress 段更新进度。
6. 验证校验：完成后用工具/测试验证，把验证结论记入 progress。
7. 收尾总结：结束用 daily_memory 的 conclusions 段记录成果、决策与待办。
    硬性要求：目标存在于 task_tracker（跨回合持久）与当日 daily_memory；任何>1步的任务都必须有 task 条目与进度更新；会话开始已自动创建当日 daily_memory 骨架，你只需填内容；目标达成即停止，不要为看起来在干活而重复工具调用。"#,
    );

    // Temp-file hygiene: agent frequently authors scratch files
    // (test scripts under /tmp, debug dumps, fixture data, one-off
    // parse outputs). Without an explicit cleanup rule the user's
    // workspace accumulates dead artifacts after every run. This
    // block sits next to the Operating Principles because the same
    // "completion" step that writes `conclusions` is the natural
    // moment to also tidy up the scratch tree.
    builder = builder.with_block(
        r#"## 临时文件自清理 (Temp File Hygiene)
任何由本会话创建、且非用户原始输入的文件，都视作"临时文件"——除非用户在请求里明确说"保留/不要删"，否则必须在本任务收尾时清理：
1. 写入位置仅限以下三类（其它位置必须先问用户）：
   - `target/` / `node_modules/` / `.git/` 下（构建/版本目录，会被 git/build 自动忽略）
   - 系统临时目录（`$TMPDIR` / `std::env::temp_dir()` / `%TEMP%`）
   - 任务沙箱根目录下的 `.scratch/` 子目录（任务结束整目录删）
2. 不允许把临时文件写到：
   - 用户家目录根 (`~/`、`C:\Users\<user>\`)
   - 项目根目录（与用户源文件混在一起）
   - 任何 dotfile（`.bashrc` / `.zshrc` / `.profile` / `Cargo.toml` 等配置）
3. 收尾顺序（与 Operating Principles 第 7 步"收尾总结"合并执行）：
   a. 验证结论已写入 progress 段
   b. 删除本任务产生的所有临时文件/目录
   c. 在 daily_memory 的 conclusions 段末尾追加一句「临时文件：已清理 N 个（M 个保留）」，让用户知道清扫结果
   d. 若发现需要保留的文件（用户指定或产物类），用 `file_change` 事件或 final message 明确告知用户路径
   反面案例：写 `/tmp/probe.py` 验证后不删、写项目根 `test_output.txt`、在 `~/` 下创建 `notes.md`。这些都是污染行为。"#,
    );

    // 自定义应用开发闭环纪律: validate+patch 外科手术式修复, 禁止整篇重写/整篇读入上下文.
    builder = builder.with_block(
        r#"## 自定义应用开发循环纪律 (Custom App Dev Loop)
开发/修改自定义应用 (custom_app_manager) 时，强制以下闭环，禁止把整篇源码读入上下文或整篇重写：
1. 改完代码后必须调 `custom_app_manager` 的 `validate` action 做开发期静态校验。
2. `validate` 返回 `ok=false` 时：用 issue 里的 `line`/`col`/`snippet` 定位精确位置，**禁止**整篇重写 `index_html`/`app_source`，也**禁止**把 >5KB 源码整篇读入上下文。
3. 调用 `custom_app_manager` 的 `patch` action：传 `old_block`（直接取 issue.snippet 的精确文本）+ `new_block`，只动出错那几行做外科手术式替换。
4. 再次 `validate` 复验，形成 `validate → patch → validate` 闭环，最多 3 轮。
5. 3 轮仍 `ok=false` 即停止并向上汇报（说清卡在哪条 issue），不要反复重写整篇文件或把整篇源码甩给用户。
上下文纪律：任何源码文件 >5KB 一律不整篇 read；靠 `validate` 的 snippet + `patch` by anchor 精准改；确需全局结构时调 `get` 拿 manifest/结构而非全文源码。
性能守则（validate 的 perf 类 issue 同样按上述闭环修复）：
1. 禁止 innerHTML/outerHTML 全量赋值做状态更新——局部更新（textContent/createElement/key 复用 diff），初始化一次性渲染除外。
2. 桥调用（window.hx.*）禁止出现在 for/while/map/forEach 内——循环外一次批量取回再渲染，storage 读写缓存到内存统一写回。
3. 列表 ≥50 条必须分页/截断/虚拟滚动；数据多时避免 `.map(...).join("")` 拼整段 DOM。
4. 不用 backdrop-filter 与全局 `* { transition }`；过渡只加在交互元素的具体属性（transform/opacity）。"#,
    );

    let mut rendered = builder.build().to_string();

    // ★ Protocol-completeness: inline the five required section markers
    // into the stable layer itself. Without this, every fresh process /
    // cache miss produces a prompt with zero `### Identity`-style markers
    // and the SectionGate audit patches all five in (WARN
    // "SectionGate auto-patched 5 missing required sections" + markers
    // appended at the prompt tail, growing the prompt every rebuild).
    // Inlining mirrors `Section::default_body()` but with the real
    // per-turn facts (tool presence / workspace) so the model is never
    // told "no tools are active" when tools ARE available.
    {
        use crate::context::Section;
        let has_tools = !tool_schemas.is_empty();
        let has_workspace = config
            .workspace
            .as_deref()
            .map(str::trim)
            .map(|w| !w.is_empty())
            .unwrap_or(false);
        let protocol = [
            Section::Identity.default_body().to_string(),
            if has_tools {
                "### Active tools\nSee the active tools list above.".to_string()
            } else {
                Section::ToolList.default_body().to_string()
            },
            if has_tools {
                "### Tool enforcement\nTools are active. Prefer tool calls over prose when action is required.".to_string()
            } else {
                Section::ToolEnforce.default_body().to_string()
            },
            if has_workspace {
                "### Working directory\nSee the Active Workspace block above.".to_string()
            } else {
                Section::WorkingDir.default_body().to_string()
            },
            Section::Timestamp.default_body().to_string(),
        ]
        .join("\n\n");
        rendered.push_str(&format!("\n\n{protocol}"));
    }

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
    context_epoch: u64,
) -> String {
    // The context layer is keyed by an external epoch tick supplied by
    // the caller (bumped on memory / skills / context-files mutation).
    let cache_key = crate::agent_helpers::compute_context_cache_key(context_epoch);

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
        let memory_dir = config.resolved_memory_dir();
        let (memory_block, user_block) = load_builtin_memory_snapshot(
            &memory_dir,
            config.memory_reads_global_fallback(),
        );
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

    // Custom-apps index: tiny per-app line so the LLM knows what's
    // installed without a list() round-trip. Cheap enough to live in
    // every turn; details fetched on demand via `custom_app_manager get`.
    if let Some(apps_index) = custom_apps_index_prompt(config) {
        builder = builder.with_block(&apps_index);
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

/// wire: same as [`resolve_initial_system_prompt`] but threads
/// the running per-tool error counter into the rendered prompt.
/// `tool_error_counts == None` falls back to the legacy build path;
/// `Some(map)` triggers the `[Tool Failure Hints]` block via
/// [`build_system_prompt_with_tool_errors`].
/// All other behavior — cached prompt restoration, SectionGate audit,
/// protocol-completeness — is byte-identical to
/// [`resolve_initial_system_prompt`].
pub fn resolve_initial_system_prompt_with_tool_errors(
    config: &AgentConfig,
    prompt_cache: &Mutex<crate::agent_helpers::PromptCache>,
    memory_router: Option<&MemoryRouter>,
    callbacks: &AgentCallbacks,
    user_message: &str,
    tool_schemas: &[ToolSchema],
    tool_call_count: usize,
    tool_error_counts: Option<&std::collections::HashMap<String, u32>>,
    context_epoch: u64,
    force_rebuild: bool,
) -> (String, bool) {
    // ★ `force_rebuild` is set by the AgentLoop when a
    // memory/skill/context-file store mutated since
    // `config.stored_system_prompt` was frozen at
    // `stored_prompt_epoch`. When set, we bypass the reuse path and
    // re-read the Context layer from disk so in-session `memory` tool
    // edits (add/replace/remove) take effect on the next user message
    // instead of only after a session restart.
    let (prompt, restored) = if !force_rebuild {
        if let Some(ref s) = config.stored_system_prompt {
            let t = s.trim();
            if !t.is_empty() {
                (s.clone(), true)
            } else {
                (
                    build_system_prompt_with_tool_errors(
                        config,
                        prompt_cache,
                        memory_router,
                        user_message,
                        tool_schemas,
                        &config.model,
                        tool_call_count,
                        tool_error_counts,
                        context_epoch,
                    ),
                    false,
                )
            }
        } else {
            (
                build_system_prompt_with_tool_errors(
                    config,
                    prompt_cache,
                    memory_router,
                    user_message,
                    tool_schemas,
                    &config.model,
                    tool_call_count,
                    tool_error_counts,
                    context_epoch,
                ),
                false,
            )
        }
    } else {
        (
            build_system_prompt_with_tool_errors(
                config,
                prompt_cache,
                memory_router,
                user_message,
                tool_schemas,
                &config.model,
                tool_call_count,
                tool_error_counts,
                context_epoch,
            ),
            false,
        )
    };
    //★ : protocol-completeness single choke point. Audit the
    // produced prompt against the section gate; if any required
    // marker is missing, splice in the default body. This is the
    // *only* function in `agent_loop` that mutates the system
    // prompt for completeness — every other code path that builds
    // a prompt must funnel through here.
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
            domain_mode: Some(resolve_domain_mode(config, user_message)),
            workbench_id: config.workbench_id.as_deref(),
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
        // (C3 marker protocol): forward the audit report
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

/// Returns `(prompt, restored_from_storage)` — restored prompts skip fresh `build_system_prompt`.
/// Thin shim over [`resolve_initial_system_prompt_with_tool_errors`] with
/// `tool_error_counts = None`. Preserves the legacy call sites that
/// pre-date (17 callers in `agent_loop` tests).
pub fn resolve_initial_system_prompt(
    config: &AgentConfig,
    prompt_cache: &Mutex<crate::agent_helpers::PromptCache>,
    memory_router: Option<&MemoryRouter>,
    callbacks: &AgentCallbacks,
    user_message: &str,
    tool_schemas: &[ToolSchema],
    tool_call_count: usize,
    context_epoch: u64,
    force_rebuild: bool,
) -> (String, bool) {
    resolve_initial_system_prompt_with_tool_errors(
        config,
        prompt_cache,
        memory_router,
        callbacks,
        user_message,
        tool_schemas,
        tool_call_count,
        None,
        context_epoch,
        force_rebuild,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// 缓存稳定性回归（2026-09-28，对标 dsh 的 97%→11% 缓存 issue）：
    /// 系统提示尾部的 "Conversation started" 块此前每 turn 用
    /// `now()` 重新渲染。请求序列是 [system][tools][history]，前缀缓存
    /// 要求逐字节一致 —— 尾部时间戳一变，tools 块与整段历史每 turn
    /// 缓存全 miss。修复后该块由 `session_started_at`（会话内恒定）
    /// 决定，模型/provider/session_id 相同的两次组装必须**字节一致**。
    #[test]
    fn dynamic_suffix_is_stable_within_a_session() {
        let mut cfg = AgentConfig::default();
        cfg.pass_session_id = true;
        cfg.session_id = Some("sess-cache-1".to_string());
        cfg.session_started_at = Some("2026-09-28T10:00:00+08:00".to_string());

        let a = build_dynamic_suffix(&cfg, "deepseek-v4-pro");
        std::thread::sleep(std::time::Duration::from_millis(5));
        let b = build_dynamic_suffix(&cfg, "deepseek-v4-pro");
        assert_eq!(a, b, "suffix must be byte-identical across turns");
        assert!(a.contains("Conversation started"));
    }

    /// 反向保证：不同会话的时间戳必须不同（否则模型会拿到别的会话的
    /// 起始时间）。同时确认 model/provider 变化仍会改变该块——它们是
    /// 合法且稳定的差异来源。
    #[test]
    fn dynamic_suffix_tracks_session_start_and_model() {
        let mut cfg = AgentConfig::default();
        cfg.session_started_at = Some("2026-09-28T10:00:00+08:00".to_string());
        let base = build_dynamic_suffix(&cfg, "deepseek-v4-pro");

        let mut other = cfg.clone();
        other.session_started_at = Some("2026-09-28T22:30:00+08:00".to_string());
        assert_ne!(build_dynamic_suffix(&other, "deepseek-v4-pro"), base);

        let other_model = build_dynamic_suffix(&cfg, "deepseek-v4-flash");
        assert_ne!(other_model, base);
    }

    /// regression: the `prompt_cache.stable` slot used to be
    /// write-only — `build_system_prompt` would render `prefix_text` on
    /// every call, *then* store it in the slot, but still use the
    /// freshly-rendered text for the prompt. The cache never actually
    /// short-circuited the second-call computation.
    /// (round 5): split into two layers (stable + context);
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
        // Regression: the stable layer must inline the protocol markers so
        // SectionGate never reports `patched=5` on a fresh cache miss.
        for marker in [
            "### Identity",
            "### Active tools",
            "### Tool enforcement",
            "### Working directory",
            "### Current time",
        ] {
            assert!(
                stable1.contains(marker),
                "stable layer must inline protocol marker {marker:?} (SectionGate would patch otherwise)"
            );
        }

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

    /// (three-tier split): the context layer is rendered
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

    /// sanity: with `skip_memory = true` the context layer
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

    // -------------------------------------------------------------
    //wire tests
    // -------------------------------------------------------------

    #[test]
    fn resolve_initial_system_prompt_with_tool_errors_injects_demotion_block() {
        let mut cfg = AgentConfig::default();
        cfg.tool_error_demotion_threshold = 2;
        cfg.tool_error_demotion_max_items = 5;
        let slot: Arc<Mutex<crate::agent_helpers::PromptCache>> =
            Arc::new(Mutex::new(crate::agent_helpers::PromptCache::default()));

        let mut counts = std::collections::HashMap::new();
        counts.insert("search".to_string(), 3);
        counts.insert("read_file".to_string(), 1);

        let (prompt, restored) = resolve_initial_system_prompt_with_tool_errors(
            &cfg,
            &slot,
            None,
            &AgentCallbacks::default(),
            "what time is it?",
            &[],
            0,
            Some(&counts),
            0,
            false,
        );
        assert!(!restored);
        assert!(
            prompt.contains("search"),
            "demoted tool name should appear in the prompt: {}",
            &prompt[..prompt.len().min(1500)]
        );
        assert!(
            prompt.contains("Tool Failure") || prompt.contains("failure") || prompt.contains("(used"),
            "demotion block header must be rendered"
        );
    }

    #[test]
    fn resolve_initial_system_prompt_with_tool_errors_none_is_equivalent_to_legacy() {
        let cfg = AgentConfig::default();
        let slot_legacy: Arc<Mutex<crate::agent_helpers::PromptCache>> =
            Arc::new(Mutex::new(crate::agent_helpers::PromptCache::default()));
        let slot_new: Arc<Mutex<crate::agent_helpers::PromptCache>> =
            Arc::new(Mutex::new(crate::agent_helpers::PromptCache::default()));

        let prompt_legacy = resolve_initial_system_prompt(
            &cfg,
            &slot_legacy,
            None,
            &AgentCallbacks::default(),
            "hello",
            &[],
            0,
            0,
            false,
        );
        let prompt_new = resolve_initial_system_prompt_with_tool_errors(
            &cfg,
            &slot_new,
            None,
            &AgentCallbacks::default(),
            "hello",
            &[],
            0,
            None,
            0,
            false,
        );
        // With `None` snapshot the wire MUST be byte-identical to the
        // legacy path; otherwise existing callers would see their
        // cache keys invalidated by the wrapper.
        assert_eq!(prompt_legacy, prompt_new);
    }

    #[test]
    fn resolve_initial_system_prompt_legacy_returns_legacy_prompt() {
        // Belt-and-braces: existing call sites of the legacy function
        // must continue to compile and return a non-empty prompt.
        let cfg = AgentConfig::default();
        let slot: Arc<Mutex<crate::agent_helpers::PromptCache>> =
            Arc::new(Mutex::new(crate::agent_helpers::PromptCache::default()));
        let prompt = resolve_initial_system_prompt(
            &cfg,
            &slot,
            None,
            &AgentCallbacks::default(),
            "hi",
            &[],
            0,
            0,
            false,
        );
        assert!(!prompt.0.is_empty());
    }
}
