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

use hermix_core::Message;

use crate::agent_config::AgentConfig;
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