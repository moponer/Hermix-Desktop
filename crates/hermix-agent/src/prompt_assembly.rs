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
//!   provider (drops all reasoning, applies ephemeral system prompt, marks
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
use crate::context::DEFAULT_AGENT_IDENTITY;
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

/// Total character budget for the `<available_skills>` metadata block
/// (ZCode `DEFAULT_SKILL_METADATA_BUDGET` analog). Over budget → degrade
/// to names-only: the model keeps discoverability (it can still
/// `skills_list` / `skill_view`) without the block eating the system
/// prompt.
const SKILL_METADATA_BUDGET_CHARS: usize = 20_000;

/// 详细模式下最多列多少行。
///
/// 之前这个 80 是写在渲染循环里的字面量，且超出的部分**无声消失** ——
/// 实测本机装了 87 个 skill，字母序最后 7 个永远不会出现在提示词里，
/// `skills_list` 能看到、模型不知道它们存在。现在上限是常量、超限有日志，
/// 且降级成 names-only 时**全量列出名字**（名字很便宜）。
const SKILL_INDEX_MAX_ROWS: usize = 80;

/// Render the skill index. Two tiers:
/// 1. Detailed — grouped by category, `- cmd: description [tags]` rows, capped
///    at [`SKILL_INDEX_MAX_ROWS`] (over the cap → `warn` with the drop count);
/// 2. Over budget → names-only (`- cmd`), **all** names listed (they are cheap),
///    truncated only if the names themselves exceed the budget, with an explicit
///    "N more, use `skills_list`" tail.
///
/// 参数形状跟着 `SkillOrchestrator::scan_skill_commands` 走：它返回
/// `&HashMap`（避免每次组装 prompt 重建一张表），排序在这里做。
fn render_skill_index(
    commands: &std::collections::HashMap<String, crate::skill_orchestrator::SkillCommandInfo>,
    budget: usize,
) -> String {
    let detailed = |rows: &[(&String, &crate::skill_orchestrator::SkillCommandInfo)]| {
        let mut categories: std::collections::BTreeMap<String, Vec<_>> =
            std::collections::BTreeMap::new();
        for (cmd, info) in rows.iter() {
            let cat = if info.category.is_empty() { "other" } else { &info.category };
            categories
                .entry(cat.to_string())
                .or_default()
                .push((cmd, info));
        }
        let mut body = String::new();
        for (cat, skills) in &categories {
            body.push_str(&format!("### {cat}\n"));
            for (cmd, info) in skills {
                let tags_str = if info.tags.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", info.tags.join(", "))
                };
                body.push_str(&format!("- {cmd}: {}{tags_str}\n", info.description.trim()));
            }
        }
        body
    };

    let mut rows: Vec<_> = commands.iter().collect();
    // ★ 行数上限（`SKILL_INDEX_MAX_ROWS`）之前必须先按"可复用价值"排序，
    // 而不是按字典序。此前纯字典序 + 98 个技能只有 80 行 → 被砍掉的 18 个
    // 全凭名字首字母决定，其中可能有手写技能；而 `skills/auto` 下自动沉淀
    // 的垃圾条目（名字就是内容碎片）反而常常排进前 80（2026-11-06 实机：
    // `total=98 shown=80 dropped=18` 每次运行都在刷）。
    // 排序键：手写优先于 auto，同类内保持字典序（稳定、可预测）。
    rows.sort_by(|a, b| {
        let auto_a = crate::skill_loader::is_auto_generated(a.1);
        let auto_b = crate::skill_loader::is_auto_generated(b.1);
        auto_a.cmp(&auto_b).then_with(|| a.0.cmp(b.0))
    });
    let total = rows.len();
    let shown = total.min(SKILL_INDEX_MAX_ROWS);

    let body = detailed(&rows[..shown]);
    let chars = body.chars().count();
    if chars <= budget {
        if total > shown {
            // 详细模式放不下全部：剩下的只能靠 skills_list 发现。这条日志是
            // 「模型为什么没想到某个技能」的唯一线索，所以用 warn。
            tracing::warn!(
                target: "hermix.skills_index",
                total,
                shown,
                dropped = total - shown,
                chars,
                budget,
                "skill index: row cap reached — the remaining skills are only \
                 reachable through skills_list"
            );
        } else {
            tracing::debug!(
                target: "hermix.skills_index",
                mode = "detailed",
                total,
                shown,
                chars,
                budget,
                "skill index rendered"
            );
        }
        return body;
    }

    // Names-only 降级：保留可发现性（skills_list/skill_view 仍在），
    // 描述全部让位给预算。名字本身很便宜，所以这一档**不设行数上限**——
    // 只在真的超出预算时才按预算截，并明确告诉模型还有几个没列。
    let mut names_only = String::from(
        "[技能数量超出元数据预算 — 已降级为仅名称列表。用 `skill_view <名称>` 查看任一技能的完整说明。]\n",
    );
    let mut listed = 0usize;
    let mut omitted = 0usize;
    for (cmd, _) in rows.iter() {
        let line = format!("- {cmd}\n");
        if names_only.chars().count() + line.chars().count() > budget {
            omitted += 1;
            continue;
        }
        names_only.push_str(&line);
        listed += 1;
    }
    if omitted > 0 {
        names_only.push_str(&format!(
            "…（另有 {omitted} 个未列出，用 `skills_list` 查看）\n"
        ));
    }
    tracing::info!(
        target: "hermix.skills_index",
        mode = "names-only",
        total,
        listed,
        omitted,
        chars = names_only.chars().count(),
        budget,
        "skill index degraded to names-only (descriptions dropped, names kept)"
    );
    names_only
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
        body.push_str(&render_skill_index(commands, SKILL_METADATA_BUDGET_CHARS));
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
        // 预算与自动匹配那条路共用同一个旋钮：正文内联总量上限。勾选本身
        // 不会丢，超预算的技能降级成指针（名字 + 描述 + 先 `skill_view`）。
        let (ephemeral_body, loaded, missing) = orch2
            .build_preloaded_skills_prompt(&ephemeral, config.skill_injection_char_budget);
        if !loaded.is_empty() {
            body.push_str("\n\n## Active Skills (this turn)\n");
            body.push_str(
                "The user explicitly enabled the following skills for this conversation. \
                 Small ones are inlined; any that exceeded the preload budget only get a \
                 pointer — load those with `skill_view` before acting.\n",
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
/// 3. Drop `reasoning_content` from **every** message — thinking blocks are
/// never echoed back, which keeps the assistant rows byte-identical
/// across calls (see the note at the strip site).
/// 4. Apply Anthropic prompt-cache markers.
///
/// Every step here must be **stable across calls**: a row that was already
/// sent has to serialize to the same bytes on every later call, otherwise
/// the provider's prefix cache is invalidated from that position to the end
/// of the request.
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

    // Never echo assistant thinking blocks back to the provider.
    //
    // The previous rule kept the *last* assistant's reasoning and cleared
    // every earlier one. That is a per-call rewrite of already-sent bytes:
    // call N sends assistant_A **with** reasoning, call N+1 (after a new
    // assistant arrives) sends the same row **without** it, so the prefix
    // cache breaks at assistant_A and every token after it — its tool
    // results and the whole following round — is billed again. Measured on
    // a real session: 60 assistant rows carried 190K chars of thinking,
    // and each call re-paid ~3-12K tokens for that alone.
    //
    // Keeping *all* thinking would also be byte-stable but would inflate
    // every request by that 190K chars (plus it tripped compression far
    // earlier, and each compression is a full cache reset). Dropping *all*
    // of it is stable AND smaller: the payload is built the same way on
    // every call, so position 0..n never changes because of reasoning.
    // `ctx` keeps the reasoning for persistence / UI replay — this is a
    // send-time projection, not a mutation of history.
    for msg in messages.iter_mut() {
        msg.reasoning_content = None;
    }
    observe_prefix_reuse(config.session_id.as_deref(), &messages);
    apply_prompt_cache_markers(config, &mut messages);
    messages
}

/// Per-session state carried between two consecutive API calls.
///
/// Beyond the message fingerprints this also remembers the **first** thing the
/// provider sees: the `tools` array. OpenAI-compatible chat templates render
/// `tools` *before* the system message, so a changed tool array invalidates
/// every byte after it — including the whole conversation — even when every
/// message row is byte-identical. Without this field a `shared_msgs = 0` line
/// says "something broke" but not "what broke".
#[derive(Default)]
struct PrefixState {
    /// 上一次 payload 的逐条指纹（role + content + tool_calls）。
    msgs: Vec<(u64, usize)>,
    /// 上一次**实际发出**的 `tools` 数组指纹（0 = 尚未记录）。
    tools_fp: u64,
    /// 本轮将要发出的 `tools` 数组指纹，由 `prepare_run_context` 写入
    /// （`observe_prefix_reuse` 在每次调用时把它换进 `tools_fp`）。
    pending_tools_fp: u64,
    /// 上一次 payload 里 system 消息（第 0 条）的指纹。
    system_fp: u64,
}

/// Last payload fingerprint per session. Bounded so parallel sessions
/// (and sub-agents) can't grow it without limit.
static PREFIX_STATE: Mutex<Option<std::collections::HashMap<String, PrefixState>>> =
    Mutex::new(None);
const MAX_PREFIX_SESSIONS: usize = 32;

/// 计算 `tools` 数组的字节指纹（顺序敏感：同样的集合换个顺序也算变了）。
pub fn tool_array_fingerprint(tools: &[ToolSchema]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    tools.len().hash(&mut h);
    for t in tools {
        t.name.hash(&mut h);
        t.description.hash(&mut h);
        format!("{:?}", t.parameters).hash(&mut h);
    }
    h.finish()
}

/// 记录本轮路由出的 `tools` 数组指纹。
///
/// 调用点在 `prepare_run_context`（工具集合定稿之后、第一次 API 调用之前），
/// 每次调用时由 [`observe_prefix_reuse`] 换进快照并比对。
pub fn record_tool_array_fingerprint(session_id: Option<&str>, fingerprint: u64) {
    let key = session_id.unwrap_or("").to_string();
    let Ok(mut slot) = PREFIX_STATE.lock() else {
        return;
    };
    let map = slot.get_or_insert_with(std::collections::HashMap::new);
    map.entry(key).or_default().pending_tools_fp = fingerprint;
}

/// Per-session fingerprint of the previous request body, used to measure how
/// much of this request the provider can serve from its prefix cache.
///
/// The provider's number arrives one call late and only as an aggregate, so a
/// regression ("we started rewriting history again") is invisible until the
/// hit rate has already dropped. This keeps the *previous* payload's
/// per-message fingerprints and logs the shared prefix — both at message
/// granularity and inside the first differing message — which is exactly the
/// quantity a prefix cache can bill as a hit.
fn observe_prefix_reuse(session_id: Option<&str>, messages: &[Message]) {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    fn fingerprint(msg: &Message) -> (u64, usize) {
        let mut h = DefaultHasher::new();
        (msg.role as u8).hash(&mut h);
        msg.content.hash(&mut h);
        msg.content_parts
            .as_ref()
            .map(|p| format!("{p:?}"))
            .hash(&mut h);
        msg.tool_calls
            .as_ref()
            .map(|c| {
                c.iter()
                    .map(|t| format!("{}|{}", t.id, t.function.arguments))
                    .collect::<Vec<_>>()
                    .join("\u{1}")
            })
            .hash(&mut h);
        let chars = msg.content.as_deref().map(str::len).unwrap_or(0)
            + msg
                .tool_calls
                .as_ref()
                .map(|c| c.iter().map(|t| t.function.arguments.len()).sum::<usize>())
                .unwrap_or(0)
            + msg
                .content_parts
                .as_ref()
                .map(|p| format!("{p:?}").len())
                .unwrap_or(0);
        (h.finish(), chars)
    }

    let current: Vec<(u64, usize)> = messages.iter().map(fingerprint).collect();
    let total_msgs = current.len();
    let total_chars: usize = current.iter().map(|c| c.1).sum::<usize>().max(1);
    let system_fp = current.first().map(|c| c.0).unwrap_or(0);
    let key = session_id.unwrap_or("").to_string();

    // 取上次快照 + 换入本轮快照（同一把锁内完成，避免两次调用交错时互相覆盖）。
    let (prev, tools_changed, system_changed, tools_fp) = {
        let Ok(mut slot) = PREFIX_STATE.lock() else {
            return;
        };
        let map = slot.get_or_insert_with(std::collections::HashMap::new);
        if map.len() >= MAX_PREFIX_SESSIONS && !map.contains_key(&key) {
            map.clear();
        }
        let entry = map.entry(key.clone()).or_default();
        // 本轮 `tools` 指纹由 `prepare_run_context` 预先写入；没有记录的调用
        // （测试、子代理）退化为「沿用上一次」，不会噪声式报告 tools_changed。
        let current_tools = if entry.pending_tools_fp != 0 {
            entry.pending_tools_fp
        } else {
            entry.tools_fp
        };
        let tools_changed =
            entry.tools_fp != 0 && current_tools != 0 && current_tools != entry.tools_fp;
        let system_changed = entry.system_fp != 0 && entry.system_fp != system_fp;
        let prev = std::mem::take(&mut entry.msgs);
        entry.msgs = current.clone();
        entry.tools_fp = current_tools;
        entry.pending_tools_fp = 0;
        entry.system_fp = system_fp;
        (prev, tools_changed, system_changed, current_tools)
    };

    if prev.is_empty() {
        return;
    }
    let mut shared_msgs = 0usize;
    while shared_msgs < prev.len().min(current.len()) && prev[shared_msgs].0 == current[shared_msgs].0
    {
        shared_msgs += 1;
    }
    let shared_chars: usize = current.iter().take(shared_msgs).map(|c| c.1).sum();
    // `rewritten` = an already-sent row changed shape. That is the one
    // thing that must never happen: the provider can then only bill
    // what precedes it as a hit.
    let rewritten = shared_msgs < prev.len().min(current.len());
    tracing::info!(
        target: "hermix.prompt_cache",
        session = %key,
        shared_msgs,
        total_msgs,
        rewritten,
        // 归因三元组：`tools` 数组和 system 消息排在消息体之前，
        // 任一变化都会让 provider 从 position 0 起全部重算。
        // 只报 shared_msgs 时，"为什么 22 万 token 一个都没命中" 无从判断。
        tools_changed,
        system_changed,
        tools_fp = format!("{tools_fp:016x}"),
        shared_chars,
        total_chars,
        predicted_hit_pct = format!("{:.1}", 100.0 * shared_chars as f64 / total_chars as f64),
        "prompt_cache: payload prefix reuse (provider can only bill this prefix as a hit)"
    );
}

/// Anthropic-specific: mark the system message and the last 3 user/assistant
/// messages as `Ephemeral` cache. The system message gets the marker so the
/// provider-side prompt cache (Anthropic's `cache_control: {type: "ephemeral"}`)
/// can reuse the stable prefix across turns — the largest, most invariant
/// block (soul + directions + tools) lives in `build_stable_layer`, which is
/// rendered into the system message. Other providers do not consume these
/// markers, so the function is a no-op when `config.provider` is not
/// Anthropic/Claude.
///
/// The system marker lands on the **first** `System` message only, and the
/// provider emits one system block per message (see
/// `AnthropicProvider::convert_messages`) — that is what keeps the cached
/// prefix byte-identical while recall/skill/plan-mode blocks keep growing
/// behind it.
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
    // Anthropic accepts at most 4 cache break-points per request, and the
    // stable system block above already consumes one. Five markers used to
    // be emitted (1 + 4), so the last one was silently dropped — or, on
    // stricter paths, the whole request 400'd.
    let ephemeral_budget = 3;
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
        .unwrap_or_else(|| hermix_core::time::beijing_rfc3339_secs());
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
    // the verbose summary/stopping guidance routes (HadAnyToolUse /
    // MultiStepTask) were removed from DirRoute along with the guidance
    // they gated (see directions.rs "the verbose rules ... have been
    // removed").
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

    // ★ The tool-error demotion block, kept OUT of the directions layer.
    //
    // It is a separate `## Tool Failures` section. Empty when
    // `tool_error_counts` is None or no tool has reached the configured
    // threshold. Capped at the configured max rows.
    //
    // It used to be appended to the END of the directions layer — which
    // sits in the *middle* of the system message, ahead of the context
    // layer (35K characters). A tool failing mid-turn therefore
    // invalidated everything after it, context layer included, on every
    // later call of the turn. The provider's prefix cache bills by
    // position (measured 2026-10-23: a change at the end of the system
    // message keeps the earlier prefix cached, a change at the start
    // wipes it), so a mutating block belongs at the **tail**. It is also
    // the right place for it: it is feedback about the most recent tool
    // call, and recency is what makes it readable.
    let mut tool_failures_text = String::new();
    if let Some(counts) = tool_error_counts {
        if let Some(block) = crate::tool_error_demotion::build_tool_error_demotion_block(
            counts,
            config.tool_error_demotion_threshold,
            config.tool_error_demotion_max_items,
        ) {
            tool_failures_text.push_str("\n\n## Tool Failures\n\n");
            tool_failures_text.push_str(&block);
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

    // ★ The active tool list, at the **tail**.
    //
    // It is injected as a `<tools>` text block so the model is always
    // aware of what it can call — even if the native
    // OpenAI/Anthropic `tools` API field is ignored, filtered out by
    // the router, or the model is a text-only fine-tune that has no
    // notion of tool-calling wire protocols.
    //
    // It sits at the end because the routed tool set changes from turn
    // to turn. In the middle of the prompt it invalidated every byte
    // after it; at the tail it costs only the tail.
    let tools_text = {
        let mut b = SystemPromptBuilder::new();
        b = b.with_tools_section(tool_schemas);
        b.build().to_string()
    };

    let prompt = format!(
        "{stable_text}{directions_text}{context_text}{suffix_text}{tools_text}{tool_failures_text}"
    );
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
    // the verbose summary/stopping guidance routes (HadAnyToolUse /
    // MultiStepTask) were removed from DirRoute along with the guidance
    // they gated (see directions.rs "the verbose rules ... have been
    // removed").
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

    // ★ The active tool list, at the **tail**. See the note on the
    // same block in `build_system_prompt_with_tool_errors`.
    let tools_text = {
        let mut b = SystemPromptBuilder::new();
        b = b.with_tools_section(tool_schemas);
        b.build().to_string()
    };

    let prompt = format!(
        "{stable_text}{directions_text}{context_text}{suffix_text}{tools_text}"
    );
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
        directions_chars = directions_text.len(),
        directions_est_tokens = directions_text.len() / 4,
        tools_chars = tools_text.len(),
        tools_est_tokens = tools_text.len() / 4,
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
        // The stable layer no longer carries the tool list (it moved to
        // the tail of the system prompt), so the *set* of tools no
        // longer affects the cached text — only whether there are any
        // at all, via the `### Active tools` protocol marker.
        !tool_schemas.is_empty(),
        // An on-disk guidance override edited mid-session must
        // invalidate the cache, or the model keeps seeing the compiled
        // default until the process restarts.
        crate::directions::guidance_override_fingerprint(),
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
    //
    // A **named** personality is the acting entity's identity — it takes
    // the identity slot (the first thing the model reads), not a block
    // appended after the Hermix/SOUL identity. Appending made the model
    // read "你好，我是 Hermix" first and answer under the wrong name:
    // the twin:ali session introduced itself as "hermix" even though the
    // `ali` profile was active. Inline personality text (freeform, not a
    // named profile) keeps the old append behavior — it IS a style, not
    // an entity. SOUL.md remains the identity only when no named
    // personality is active.
    let named_profile = config
        .personality
        .as_ref()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .and_then(|p| resolve_personality(p, config.hermix_home.as_deref()));
    let mut builder = match named_profile.as_deref() {
        Some(profile) => SystemPromptBuilder::new().with_personality(Some(profile)),
        None => SystemPromptBuilder::new().with_personality(soul.as_deref()),
    };
    if let Some(base) = config.system_prompt.as_deref() {
        builder = builder.with_system_message(base);
    }

    // ★ directions[] moved to the per-turn dynamic layer in
    // `build_system_prompt`. The stable layer must NOT depend on
    // per-turn state (user message, active tool set) so the cache key
    // stays stable across turns.
    //
    // ★ The active tool list used to be injected here as a `<tools>`
    // text block so the model is *always* aware of what it can call —
    // even if the native OpenAI/Anthropic `tools` API field is ignored,
    // filtered out by the router, or the model is a text-only fine-tune
    // that has no notion of tool-calling wire protocols.
    //
    // It no longer lives here: the routed tool set changes every turn,
    // and a per-turn-varying block in the middle of the cached layer
    // invalidated everything after it. It now sits at the **tail** of
    // the system prompt (see `build_system_prompt`), where a change
    // costs only the tail.

    if let Some(ref personality) = config.personality {
        let requested = personality.trim();
        if !requested.is_empty() {
            if named_profile.is_some() {
                // Named profile already occupies the identity slot above —
                // appending it again would duplicate the whole card.
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

    // 临时文件规则不再在这里写一份：它和 `FILE_LIFECYCLE_GUIDANCE`（stable 层的
    // always-on 块）说的是同一件事，而且措辞互相矛盾（一边说"一律写到 scratch"，
    // 一边说"根本不要写盘"）。2026-10-30 合并进 `file_lifecycle.always`，
    // 具体路径（`%TEMP%\hermix-scratch\`）、免审批清理由那一条统一负责。
    // 省下 ~1.1K 字符/请求，并消掉一处自相矛盾。

    // 自定义应用开发闭环纪律: validate+patch 外科手术式修复, 禁止整篇重写/整篇读入上下文.
    builder = builder.with_block(
        r#"## 自定义应用开发循环纪律 (Custom App Dev Loop)
开发/修改自定义应用 (custom_app_manager) 时，强制以下闭环，禁止把整篇源码读入上下文或整篇重写：
0. **先分清需求**：用户要「定时/自动化」（每天自动、定时提醒、周期执行）→ 用 `cronjob`，**不是**创建自定义应用；只有用户明确要一个可打开使用的桌面应用时才走 custom_app_manager。
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

    // ★ Always-on guidance (safety, tool failure, env/shell, file
    // lifecycle, git safety, URL safety, web automation, reply
    // termination, bare-label replies, parallel execution).
    //
    // These blocks are compile-time constants with an unconditional
    // route, so they used to be re-evaluated on every turn inside the
    // per-turn `directions` layer — which put ~10K characters inside
    // the part of the prompt that changes every turn, the part a
    // provider's prefix cache cannot reuse. They are rendered once
    // here instead, into the cached stable layer.
    //
    // Placed last so the block that follows them (the per-turn
    // directions) starts as late as possible — every character before
    // the churn boundary is a character the provider can serve from
    // cache.
    builder = builder.with_guidance_bodies(crate::directions::stable_guidance_bodies());

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
                "### Active tools\nSee the active tools list at the end of this prompt.".to_string()
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

    // 逐块尺寸归因（2026-11-11）：context 层实测 27.7K 字符/轮，但此前无法
    // 知道 27.7K 里谁是大头。挂载前先记尺寸，build 后一条日志给全五块。
    let mut block_sizes: Vec<(&'static str, usize)> = Vec::new();

    if !config.skip_memory {
        let memory_dir = config.resolved_memory_dir();
        let (memory_block, user_block) = load_builtin_memory_snapshot(
            &memory_dir,
            config.memory_reads_global_fallback(),
        );
        if let Some(block) = memory_block {
            block_sizes.push(("builtin_memory", block.len()));
            builder = builder.with_block(&block);
        }
        if let Some(block) = user_block {
            block_sizes.push(("user_memory", block.len()));
            builder = builder.with_block(&block);
        }
    }

    let mem_block = memory_router
        .map(|r| r.collect_system_prompt_blocks())
        .unwrap_or_default();
    if !mem_block.is_empty() {
        block_sizes.push(("memory_router", mem_block.len()));
        builder = builder.with_memory_context(&mem_block);
    }

    if let Some(skills_prompt) = skills_system_prompt(config, tool_names_set) {
        block_sizes.push(("skills_index", skills_prompt.len()));
        builder = builder.with_skills_prompt(&skills_prompt);
    }

    if let Some(context_prompt) = context_files_prompt(config) {
        block_sizes.push(("context_files", context_prompt.len()));
        builder = builder.with_context_files(&context_prompt);
    }

    // Custom-apps index: tiny per-app line so the LLM knows what's
    // installed without a list() round-trip. Cheap enough to live in
    // every turn; details fetched on demand via `custom_app_manager get`.
    if let Some(apps_index) = custom_apps_index_prompt(config) {
        block_sizes.push(("apps_index", apps_index.len()));
        builder = builder.with_block(&apps_index);
    }

    let rendered = builder.build().to_string();
    block_sizes.push(("__total", rendered.len()));
    tracing::info!(
        target: "hermix.context_breakdown",
        blocks = ?block_sizes,
        "context layer per-block chars"
    );

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

// ---------------------------------------------------------------------------
// Session-scoped system prompt snapshot
// ---------------------------------------------------------------------------
//
// provider 的前缀缓存只认「从 position 0 开始的字节」。system 消息是消息体的
// 第一行，只要它变了，后面 200K token 的历史一条都不算命中。而 system 消息
// 里装着三样**每轮都会变**的东西：`directions`（依赖当前用户消息与工具集合）、
// 工具清单文本（依赖路由结果）、内存快照（依赖记忆 epoch）。
//
// 因此把首次构建的结果按 session 冻结下来，后续 run 直接复用同一串字节。
// 失效条件只有三个，且都是「真的变了」：缓存键（模型 / 人格 / 自定义提示 /
// 工作区）、身份锚点（SOUL.md / 人格卡变更）、记忆 epoch（强制重建）。

/// 快照键。与 stable 层的 `compute_prompt_cache_key` 同源 —— 那两个键描述的是
/// 同一批输入，分成两套迟早会漂移。
fn session_prompt_key(
    config: &AgentConfig,
    tool_schemas: &[ToolSchema],
    model_for_prompt: &str,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let base = crate::agent_helpers::compute_prompt_cache_key(
        model_for_prompt,
        config.personality.as_deref(),
        config.system_prompt.as_deref(),
        config.workspace.as_deref(),
        !tool_schemas.is_empty(),
        crate::directions::guidance_override_fingerprint(),
    );
    // 会话快照冻结了 `directions`，而 directions 的另外两个输入（用户手选的
    // 领域模式 / 绑定的工作台）不在 base 键里。用户中途切「编码模式」或换
    // 工作台时必须重建一次 —— 那是用户主动改的状态，值得付一次缓存失效；
    // 不重建则整个会话都在用旧的领域指令。
    let mut h = std::collections::hash_map::DefaultHasher::new();
    base.hash(&mut h);
    format!("{:?}", config.domain_override).hash(&mut h);
    format!("{:?}", config.workbench_id).hash(&mut h);
    // 记忆文件的元数据：冻结 system 提示会让「记忆写入在下一条消息生效」
    // 失效（那是 epoch 机制的原始承诺）。用 (mtime, len) 当键最省事——
    // 只有真的改了记忆才重建一次，其余时间保持字节稳定。
    memory_snapshot_fingerprint(config).hash(&mut h);
    h.finish()
}

/// `MEMORY.md` / `USER.md` 的 (路径, mtime, len) 指纹。
///
/// 与 `build_context_layer` 读取的是同一批文件（含全局 fallback 目录），
/// 所以指纹变化 ⟺ 注入内容可能变化。
fn memory_snapshot_fingerprint(config: &AgentConfig) -> u64 {
    use std::hash::{Hash, Hasher};
    if config.skip_memory {
        return 0;
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut dirs = vec![config.resolved_memory_dir()];
    if config.memory_reads_global_fallback() {
        dirs.push(hermix_config::paths::hermix_home().join("memories"));
    }
    for dir in dirs {
        dir.display().to_string().hash(&mut h);
        for name in ["MEMORY.md", "USER.md"] {
            match std::fs::metadata(dir.join(name)) {
                Ok(m) => {
                    m.len().hash(&mut h);
                    let mtime = m
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs());
                    mtime.hash(&mut h);
                }
                Err(_) => 0u8.hash(&mut h),
            }
        }
    }
    h.finish()
}

/// 进程内 session → (缓存键, 系统提示全文)。
static SESSION_PROMPTS: Mutex<Option<std::collections::HashMap<String, (u64, String)>>> =
    Mutex::new(None);
const MAX_SESSION_PROMPTS: usize = 64;

/// 该会话是否已冻结过系统提示。
///
/// `AgentLoop` 用它决定「临时内存变更要不要触发重建」—— 只看
/// `config.stored_system_prompt` 会漏掉快照路径，于是记忆写入永远不生效。
pub fn has_session_prompt(session_id: Option<&str>) -> bool {
    let Some(sid) = session_id.filter(|s| !s.trim().is_empty()) else {
        return false;
    };
    SESSION_PROMPTS
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().map(|m| m.contains_key(sid)))
        .unwrap_or(false)
}

/// 丢掉某会话的快照（工具集合重新冻结、`/reset`、身份变更等场景）。
pub fn forget_session_prompt(session_id: Option<&str>) {
    let Some(sid) = session_id.filter(|s| !s.trim().is_empty()) else {
        return;
    };
    if let Ok(mut slot) = SESSION_PROMPTS.lock() {
        if let Some(map) = slot.as_mut() {
            map.remove(sid);
        }
    }
}

fn lookup_session_prompt(
    session_id: Option<&str>,
    key: u64,
    anchor: Option<&str>,
) -> Option<String> {
    let sid = session_id.filter(|s| !s.trim().is_empty())?;
    let slot = SESSION_PROMPTS.lock().ok()?;
    let (stored_key, text) = slot.as_ref()?.get(sid)?;
    if *stored_key != key || text.trim().is_empty() {
        return None;
    }
    // 身份锚点检查与 `config.stored_system_prompt` 路径同规则：SOUL.md / 人格卡
    // 换了名字而快照里没有新锚点 → 宁可付一次重建，也不能让助手用旧名字回答。
    if let Some(anchor) = anchor {
        if !text.contains(anchor) {
            return None;
        }
    }
    Some(text.clone())
}

fn store_session_prompt(session_id: Option<&str>, key: u64, prompt: &str) {
    let Some(sid) = session_id.filter(|s| !s.trim().is_empty()) else {
        return;
    };
    let Ok(mut slot) = SESSION_PROMPTS.lock() else {
        return;
    };
    let map = slot.get_or_insert_with(std::collections::HashMap::new);
    if map.len() >= MAX_SESSION_PROMPTS && !map.contains_key(sid) {
        map.clear();
    }
    map.insert(sid.to_string(), (key, prompt.to_string()));
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
    //
    // ★ Stale-identity check: the stored prompt freezes the identity
    // layer at first turn, so a persona change (named personality bound
    // to the session — e.g. twin:ali — or a SOUL.md edit) would never
    // reach an existing session and the assistant keeps answering under
    // the old name. When the current identity's first line is absent
    // from the stored text, rebuild once; the rebuilt prompt carries the
    // anchor and goes back to being reused.
    let session_id = config.session_id.as_deref();
    let session_key = session_prompt_key(config, tool_schemas, &config.model);
    let anchor = identity_anchor_line(config);
    // ★ 会话快照优先：同一会话的每条用户消息都拿到**同一串字节**的 system
    // 提示。没有这一步，`directions`（依赖当前用户消息）与工具清单（依赖本轮
    // 路由）会在每条新消息上重排，provider 的前缀缓存从 position 0 起全部作废
    // —— 2026-10-30 实测：22 万 token 的冷启动调用只命中 184 token。
    let session_cached = if force_rebuild {
        None
    } else {
        lookup_session_prompt(session_id, session_key, anchor.as_deref())
    };
    let (prompt, restored) = if let Some(cached) = session_cached {
        tracing::debug!(
            target: "hermix.prompt_cache",
            session = session_id.unwrap_or(""),
            chars = cached.len(),
            "prompt_cache: system prompt restored from the session snapshot"
        );
        (cached, true)
    } else if !force_rebuild {
        match config.stored_system_prompt.as_deref().map(str::trim) {
            Some(t) if !t.is_empty() => {
                match identity_anchor_line(config) {
                    Some(anchor) if !t.contains(anchor.as_str()) => {
                        tracing::info!(
                            "stored system prompt predates the current identity layer; rebuilding"
                        );
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
                    _ => (t.to_string(), true),
                }
            }
            _ => (
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
            ),
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
    // 快照存的是 SectionGate 打补丁**之后**的最终字节：下次复用的就是真正
    // 发出去的那一串，而不是补丁前的半成品（否则补丁会在每次重建时重新
    // 追加一遍，前缀永远对不上）。
    store_session_prompt(session_id, session_key, &final_prompt);
    (final_prompt, restored)
}

/// First line of the identity layer as it would be rendered **now**.
///
/// Used to detect stale stored prompts: `sessions.system_prompt` freezes
/// the whole prompt at first turn, so an identity change (a named
/// personality bound to the session — e.g. twin:ali — or a SOUL.md edit)
/// would never reach an existing session and the assistant keeps
/// answering under the old name. `resolve_initial_system_prompt_with_tool_errors`
/// rebuilds once when this anchor is absent from the stored text; the
/// rebuilt prompt carries the anchor and goes back to being reused.
///
/// `None` = the first line is too short to discriminate (bare heading /
/// empty) — the caller skips the check rather than force pointless
/// rebuilds.
fn identity_anchor_line(config: &AgentConfig) -> Option<String> {
    let named = config
        .personality
        .as_ref()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .and_then(|p| resolve_personality(p, config.hermix_home.as_deref()));
    let body = named.or_else(load_soul_md);
    let line = body
        .as_deref()
        .unwrap_or(DEFAULT_AGENT_IDENTITY)
        .lines()
        .next()?
        .trim()
        .to_string();
    if line.chars().count() < 12 {
        return None;
    }
    Some(line)
}

/// Returns `(prompt, restored_from_storage)` — restored prompts skip fresh
/// `build_system_prompt`. Thin shim over
/// [`resolve_initial_system_prompt_with_tool_errors`] with
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

    // ---------------------------------------------------------------
    // Persona identity — named personalities & stored-prompt staleness
    // ---------------------------------------------------------------

    /// A named personality must occupy the **identity slot** (first thing
    /// the model reads) and must NOT be duplicated as a later block.
    /// Regression guard for twin:ali introducing itself as "Hermix": the
    /// `ali` card used to land *after* the hard-coded Hermix identity.
    #[test]
    fn named_personality_takes_the_identity_slot() {
        let mut cfg = AgentConfig::default();
        cfg.personality = Some("ali".to_string());
        let slot: Arc<Mutex<crate::agent_helpers::PromptCache>> =
            Arc::new(Mutex::new(crate::agent_helpers::PromptCache::default()));
        let (prompt, restored) = resolve_initial_system_prompt(
            &cfg, &slot, None, &AgentCallbacks::default(), "你好", &[], 0, 0, false,
        );
        assert!(!restored);
        assert!(prompt.contains("### Identity\n你是助手阿黎（Ali）"));
        assert!(!prompt.contains("你好，我是 Hermix"), "default identity must not leak into a persona session");
        assert!(!prompt.contains("## Active Personality (ali)"));
    }

    /// A stored prompt frozen before the persona was bound must rebuild
    /// once (stale-identity anchor check), then the fresh prompt — which
    /// carries the anchor — is reused again on the next call.
    #[test]
    fn stale_stored_prompt_with_old_identity_rebuilds_once() {
        let mut cfg = AgentConfig::default();
        cfg.personality = Some("ali".to_string());
        cfg.stored_system_prompt = Some(
            "### Identity\nYou are Hermix, a direct, audit-first assistant.\n### Active tools\nx"
                .to_string(),
        );
        let slot: Arc<Mutex<crate::agent_helpers::PromptCache>> =
            Arc::new(Mutex::new(crate::agent_helpers::PromptCache::default()));
        let (prompt, restored) = resolve_initial_system_prompt(
            &cfg, &slot, None, &AgentCallbacks::default(), "你好", &[], 0, 0, false,
        );
        assert!(!restored, "frozen prompt lacks the ali anchor → rebuild");
        assert!(prompt.contains("你是助手阿黎（Ali）"));

        // Second call: the rebuilt prompt carries the anchor → reused.
        cfg.stored_system_prompt = Some(prompt);
        let (_, restored2) = resolve_initial_system_prompt(
            &cfg, &slot, None, &AgentCallbacks::default(), "你好", &[], 0, 0, false,
        );
        assert!(restored2, "fresh prompt carries the anchor → reuse");
    }

    /// A stored prompt whose identity still matches (default identity) is
    /// reused untouched — the anchor check must not churn healthy sessions.
    #[test]
    fn matching_stored_prompt_is_still_reused() {
        let cfg = AgentConfig::default();
        let stored = format!("### Identity\n{DEFAULT_AGENT_IDENTITY}");
        let mut cfg = cfg;
        cfg.stored_system_prompt = Some(stored);
        let slot: Arc<Mutex<crate::agent_helpers::PromptCache>> =
            Arc::new(Mutex::new(crate::agent_helpers::PromptCache::default()));
        let (_, restored) = resolve_initial_system_prompt(
            &cfg, &slot, None, &AgentCallbacks::default(), "你好", &[], 0, 0, false,
        );
        assert!(restored);
    }

    // ---------------------------------------------------------------
    // M6 — the always-on guidance lives in the cached stable layer
    // ---------------------------------------------------------------

    /// The unconditional always-on blocks (safety, tool failure,
    /// env/shell, file lifecycle, git safety, URL safety, web
    /// automation, reply termination, bare-label replies, parallel
    /// execution) must be rendered into the **stable** layer, not the
    /// per-turn directions layer.
    ///
    /// Why it matters: the per-turn region is the part a provider's
    /// prefix cache cannot reuse. ~10K characters of compile-time
    /// constants living there meant paying for them on every turn.
    /// The tool-failure demotion block mutates mid-turn (a tool fails)
    /// and must therefore sit at the **tail** of the system message.
    ///
    /// It used to be appended to the end of the directions layer, which
    /// sits ahead of the 35K-character context layer — so one failing
    /// tool invalidated the context layer's cache on every later call of
    /// the turn. The provider bills the prefix cache by position, so a
    /// mutating block belongs last.
    #[test]
    fn tool_failure_block_sits_at_the_tail() {
        use hermix_core::{JsonSchema, ToolSchema};
        use std::collections::HashMap;
        let cfg = AgentConfig::default();
        let slot: Arc<Mutex<crate::agent_helpers::PromptCache>> =
            Arc::new(Mutex::new(crate::agent_helpers::PromptCache::default()));
        let mut counts: HashMap<String, u32> = HashMap::new();
        counts.insert("read_file".to_string(), 9);
        let tools = vec![ToolSchema::new("read_file", "Read", JsonSchema::new("object"))];

        let prompt = build_system_prompt_with_tool_errors(
            &cfg,
            &slot,
            None,
            "hello",
            &tools,
            &cfg.model,
            0,
            Some(&counts),
            0,
        );

        let failures_at = prompt
            .find("## Tool Failures")
            .expect("the demotion block must be present when a tool keeps failing");
        let tools_at = prompt
            .find("For detailed parameters")
            .expect("the tool list must be present");
        assert!(
            failures_at > tools_at,
            "the mutating tool-failure block must come after the tool list (tail), \
             not in the middle of the prompt"
        );
    }

    /// The always-on guidance blocks live in the stable layer.
    #[test]
    fn always_on_guidance_is_rendered_into_the_stable_layer() {
        let cfg = AgentConfig::default();
        let slot: Arc<Mutex<crate::agent_helpers::PromptCache>> =
            Arc::new(Mutex::new(crate::agent_helpers::PromptCache::default()));
        let _ = build_system_prompt(&cfg, &slot, None, "", &[], &cfg.model, 0);

        let stable = slot.lock().unwrap().stable.clone().expect("stable layer");
        // Every block, not a sample of four: the previous version probed
        // 4 of the 13, so dropping any of the other nine from
        // `collect_stable_guidance` went unnoticed. The probe is the
        // block's own body prefix (a hand-written marker table drifts
        // silently when the wording changes), and the count assertion
        // catches a block being dropped from the list.
        let blocks = crate::directions::collect_stable_guidance();
        assert_eq!(blocks.len(), 13, "always-on 块的数量变了，请同步注释与笔记");
        for d in &blocks {
            let probe: String = d.body.chars().take(40).collect();
            assert!(
                stable.contains(&probe),
                "stable 层缺少 always-on 块 {}（探针 {:?}）",
                d.tag,
                probe
            );
        }
    }

    /// The stable layer must be **turn-invariant with respect to the
    /// routed tool set**.
    ///
    /// Before M6 the stable layer rendered the tool name list, so a
    /// tool-set change invalidated the whole cached layer and every byte
    /// after it. The list now sits at the tail of the system prompt, so
    /// a changed tool set must leave the stable layer byte-identical.
    #[test]
    fn stable_layer_survives_a_tool_set_change() {
        use hermix_core::{JsonSchema, ToolSchema};
        let cfg = AgentConfig::default();
        let slot: Arc<Mutex<crate::agent_helpers::PromptCache>> =
            Arc::new(Mutex::new(crate::agent_helpers::PromptCache::default()));

        let one = vec![ToolSchema::new("read_file", "Read", JsonSchema::new("object"))];
        let two = vec![
            ToolSchema::new("read_file", "Read", JsonSchema::new("object")),
            ToolSchema::new("write_file", "Write", JsonSchema::new("object")),
        ];

        let _ = build_system_prompt(&cfg, &slot, None, "", &one, &cfg.model, 0);
        let (key_a, stable_a) = {
            let g = slot.lock().unwrap();
            (g.cached_stable_prompt_key, g.stable.clone())
        };

        let _ = build_system_prompt(&cfg, &slot, None, "", &two, &cfg.model, 0);
        let (key_b, stable_b) = {
            let g = slot.lock().unwrap();
            (g.cached_stable_prompt_key, g.stable.clone())
        };

        assert_eq!(
            key_a, key_b,
            "a tool-set change must not invalidate the stable layer"
        );
        assert_eq!(
            stable_a, stable_b,
            "the stable layer text must not change when the routed tool set changes"
        );
    }

    /// The tool list moved to the **tail** of the system prompt: it is
    /// the one block that changes every turn, so it must not sit in the
    /// middle where it would invalidate everything after it.
    #[test]
    fn tool_list_sits_at_the_tail_of_the_system_prompt() {
        use hermix_core::{JsonSchema, ToolSchema};
        let cfg = AgentConfig::default();
        let slot: Arc<Mutex<crate::agent_helpers::PromptCache>> =
            Arc::new(Mutex::new(crate::agent_helpers::PromptCache::default()));
        let tools = vec![ToolSchema::new("read_file", "Read", JsonSchema::new("object"))];
        let prompt = build_system_prompt(&cfg, &slot, None, "", &tools, &cfg.model, 0);

        let tools_at = prompt.find("## Tools").expect("tool list must be present");
        let marker_at = prompt
            .find("### Active tools")
            .expect("protocol marker must be present");
        assert!(
            marker_at < tools_at,
            "the tool list must come after the protocol markers (tail), not before them"
        );
        assert!(
            prompt.contains("Available: read_file"),
            "the tool list must still name the active tools"
        );
    }

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
    // Prefix-cache invariant
    // -------------------------------------------------------------

    /// **已经发出去的内容不得被后续请求改写。**
    ///
    /// Provider 的 prefix cache 只认从 position 0 开始的字节前缀，所以一个
    /// turn 内的每一轮工具循环都必须满足：上一轮的 payload 是这一轮 payload
    /// 的前缀。这条不变量此前被两处打破 ——
    /// 1. `messages_for_api_call` 保留“最后一条 assistant”的
    ///    `reasoning_content`：上一次请求里它带着 thinking，这一次变成非最后
    ///    一条于是被清空，同一位置的字节变了；
    /// 2. 每轮 `rollup_tool_results` 按“距尾部条数”改写旧工具结果。
    ///
    /// 两者叠加把真实命中率钉死在 ~77%。这个测试跑 6 轮工具循环，逐条比对
    /// 相邻两次 payload 的已发送部分；任何回溯改写都会在这里红。
    #[test]
    fn payload_never_rewrites_already_sent_rows() {
        use hermix_core::{FunctionCall, ToolCall};

        let cfg = AgentConfig::default();
        let mut ctx = ContextManager::new(128_000);
        ctx.add_message(Message::system("你是一个助手。"));
        ctx.add_message(Message::user("帮我查一下日志"));

        let mut prev: Vec<Message> = Vec::new();
        for round in 0..6 {
            let mut a = Message::assistant(format!("round {round}"));
            a.reasoning_content = Some(format!("thinking-{round} {}", "x".repeat(500)));
            a.tool_calls = Some(vec![ToolCall {
                id: format!("call_{round}"),
                function: FunctionCall {
                    name: "read_file".to_string(),
                    arguments: "{\"path\":\"a.log\"}".to_string(),
                },
            }]);
            ctx.add_message(a);
            ctx.add_message(Message::tool_result(
                format!("call_{round}"),
                format!("result-{round} {}", "y".repeat(2_000)),
                Some("read_file"),
            ));

            let payload = messages_for_api_call(&cfg, &ctx);
            assert!(
                payload.iter().all(|m| m.reasoning_content.is_none()),
                "thinking blocks must never be echoed back"
            );
            if !prev.is_empty() {
                let overlap = prev.len().min(payload.len());
                for i in 0..overlap {
                    assert_eq!(
                        prev[i].role, payload[i].role,
                        "round {round}: row {i} role changed"
                    );
                    assert_eq!(
                        prev[i].content, payload[i].content,
                        "round {round}: row {i} content rewritten — already-sent bytes \
                         must be frozen or the provider prefix cache dies from here on"
                    );
                    assert_eq!(
                        prev[i].tool_calls, payload[i].tool_calls,
                        "round {round}: row {i} tool_calls rewritten"
                    );
                    assert_eq!(
                        prev[i].reasoning_content, payload[i].reasoning_content,
                        "round {round}: row {i} reasoning changed"
                    );
                }
            }
            prev = payload;
        }
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
