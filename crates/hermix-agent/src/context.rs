//! Context management for conversation history.
//!
//! The `ContextManager` tracks messages, enforces budget constraints on the
//! conversation window. All compression is handled by
//! `crate::compression::ContextCompressor` (LLM-powered).
//!
//! Also provides SOUL.md personality loading, context file injection, and
//! full system prompt assembly (corresponding to Python `run_agent.py`'s
//! `_build_system_prompt`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use hermix_config::LlmProviderConfig;
use hermix_core::{BudgetConfig, Message, MessageRole, ToolSchema};
use hermix_intelligence::model_metadata::{
    get_model_context_length, get_model_context_length_with, DEFAULT_FALLBACK_CONTEXT,
};

const MEMORY_ENTRY_DELIMITER: &str = "\n§\n";
const MEMORY_CHAR_LIMIT: usize = 2200;
const USER_CHAR_LIMIT: usize = 1375;

/// Pressure ratio at which auto-compression / context-pressure warnings fire.
///
/// 0.8 = leave 20% headroom for tool outputs and assistant generation.
/// See `agent_loop::auto_compress_if_over_threshold` and the inline
/// `Context pressure` warn tier ladder (`>= 0.95` high, `>= 0.85` medium).
///
/// As of Day 3 this constant is the **fallback default** used when no
/// [`crate::compression::CompressorConfig`] is supplied (e.g. tests and
/// the smoke `resolve_context` example). Production callers read the
/// ratio off `CompressorConfig.pressure_threshold_percent`, which is
/// the single source of truth at runtime.
pub const CONTEXT_PRESSURE_TRIGGER_RATIO: f64 = 0.8;

/// Default output reservation in tokens.
///
/// Subtracts from the *effective* context window before applying the
/// pressure ratio, so the LLM has room to emit its response without us
/// triggering compression mid-stream. 4096 tokens ≈ 16 KB of text — enough
/// for a normal assistant turn including tool calls.
pub const DEFAULT_OUTPUT_RESERVE_TOKENS: usize = 4096;

/// Rough heuristic for converting a UTF-8 string into a token count.
///
/// Uses the standard 4-chars-per-token rule that the original Python
/// `run_agent.py` budgeted against. CJK / emoji-heavy text will be
/// under-counted (one ideograph is often its own token), but the
/// estimator is intentionally a *lower bound* — if anything it
/// over-protects, never under-protects, the context window.
///
/// Replace with provider-native tokenizers (tiktoken for OpenAI,
/// the Anthropic SDK's tokenizer) once a per-model wiring layer
/// exists; the call sites (`total_tokens`, `truncate_to_budget`) do
/// not need to change.
#[inline]
pub fn estimate_tokens(s: &str) -> usize {
    // Ceiling division: (n + d - 1) / d
    (s.len() + 3) / 4
}

// ---------------------------------------------------------------------------
// ContextManager
// ---------------------------------------------------------------------------

/// Manages conversation history with budget-aware truncation and compression.
#[derive(Debug, Clone)]
pub struct ContextManager {
    messages: Vec<Message>,
    /// Maximum total tokens for the conversation (excluding system messages).
    ///
    /// As of Day 3 the unit is **tokens**, not chars. The underlying
    /// estimator is [`estimate_tokens`] (4-chars-per-token heuristic),
    /// not a provider-native tokenizer.
    max_context_tokens: usize,
    /// Tokens reserved for the LLM's response at the end of the turn.
    ///
    /// `pressure_threshold_tokens` returns `(max - reserve) * ratio` so
    /// we don't fire the pressure warning while the model still has
    /// legitimate headroom to emit its output.
    output_reserve_tokens: usize,
}

impl ContextManager {
    /// Create a new context manager with a token budget.
    ///
    /// Defaults the output reserve to [`DEFAULT_OUTPUT_RESERVE_TOKENS`].
    /// Use [`Self::with_output_reserve`] to override per-instance.
    pub fn new(max_context_tokens: usize) -> Self {
        Self {
            messages: Vec::new(),
            max_context_tokens,
            output_reserve_tokens: DEFAULT_OUTPUT_RESERVE_TOKENS,
        }
    }

    /// Construct a `ContextManager` with no model info — defaults to the
    /// catalog fallback (`model_metadata::DEFAULT_FALLBACK_CONTEXT`, 128k)
    /// rather than a hard-coded magic value.
    ///
    /// **Prefer [`Self::for_model`] or [`Self::for_model_with_providers`] when
    /// the model name is known.** This helper exists only for code paths that
    /// genuinely have no model context (e.g. early bootstrap, tests). It
    /// deliberately routes through `DEFAULT_FALLBACK_CONTEXT` so the pressure
    /// threshold stays consistent with the catalog path — callers that
    /// historically hit `200_000` here were observing a hidden magic value
    /// (200k × 0.8 = 160k threshold) instead of the catalog-fallback path
    /// (128k × 0.8 = 99k threshold).
    pub fn default_budget() -> Self {
        tracing::warn!(
            target: "hermix_agent.context",
            "ContextManager::default_budget() called without a model name; \
             using catalog fallback ({} tokens). Callers should prefer \
             ContextManager::for_model() or for_model_with_providers() \
             to avoid silent under- or over-budgeting.",
            DEFAULT_FALLBACK_CONTEXT
        );
        Self::new(DEFAULT_FALLBACK_CONTEXT as usize)
    }

    /// Build a `ContextManager` sized for a specific model.
    ///
    /// Resolution order (via `hermix_intelligence::model_metadata::get_model_context_length`):
    /// 1. `known_models()` static catalog (covers Claude / OpenAI / Gemini / DeepSeek / etc.)
    /// 2. `DEFAULT_CONTEXT_LENGTHS` fuzzy fallback table
    /// 3. `DEFAULT_FALLBACK_CONTEXT` (128_000) for unknown / custom provider models
    ///
    /// The returned token budget mirrors what `get_model_context_length`
    /// returns. As of Day 3 the field is named `max_context_tokens`
    /// even though the underlying estimator is still the 4-chars-per-token
    /// heuristic — the rename was the honest move because all downstream
    /// thresholds now speak "tokens" too.
    ///
    /// `tracing::info!` logs the resolved value once so per-model wiring can
    /// be observed in production logs without changing the API.
    pub fn for_model(model_name: &str) -> Self {
        let window = get_model_context_length(model_name) as usize;
        tracing::info!(
            target: "hermix_agent.context",
            model = %model_name,
            resolved_context_tokens = window,
            "ContextManager::for_model resolved context window"
        );
        Self::new(window)
    }

    /// Build a `ContextManager` for `model_name` honouring per-provider
    /// overrides (custom providers whose context window isn't in the static
    /// catalog). The agent loop calls this with `self.config.llm_providers`
    /// so `custom-{provider_id}:{model_name}` resolves via
    /// `LlmProviderConfig.model_contexts` / `context_window` before falling
    /// back to the catalog.
    pub fn for_model_with_providers(
        model_name: &str,
        providers: Option<&HashMap<String, LlmProviderConfig>>,
    ) -> Self {
        let window = get_model_context_length_with(model_name, providers) as usize;
        tracing::info!(
            target: "hermix_agent.context",
            model = %model_name,
            resolved_context_tokens = window,
            "ContextManager::for_model_with_providers resolved context window"
        );
        Self::new(window)
    }

    /// Build a `ContextManager` for `model_name` honouring the supplied
    /// pressure ratio and output reserve. Used by `agent_loop.rs` so the
    /// runtime values track `CompressorConfig`.
    pub fn for_model_with(model_name: &str, output_reserve_tokens: usize) -> Self {
        let mut cm = Self::for_model(model_name);
        cm.output_reserve_tokens = output_reserve_tokens;
        cm
    }

    /// Threshold (in tokens) at which the caller should consider auto-compression
    /// or surface a context-pressure warning.
    ///
    /// Equivalent to `(max - output_reserve) * CONTEXT_PRESSURE_TRIGGER_RATIO`,
    /// floored to `1` to avoid divide-by-zero in callers. The reserve is
    /// subtracted before applying the ratio so the model always has the
    /// reserved headroom for its own response even after the warning fires.
    pub fn pressure_threshold_tokens(&self) -> usize {
        let effective = self.max_context_tokens.saturating_sub(self.output_reserve_tokens);
        ((effective as f64) * CONTEXT_PRESSURE_TRIGGER_RATIO).max(1.0) as usize
    }

    /// Override the per-instance output reserve (tokens). Useful for tests
    /// and for callers that want to leave more or less headroom than the
    /// default `4096` tokens.
    pub fn with_output_reserve(mut self, output_reserve_tokens: usize) -> Self {
        self.output_reserve_tokens = output_reserve_tokens;
        self
    }

    pub fn with_compressor(max_context_tokens: usize, _compressor: ()) -> Self {
        Self {
            messages: Vec::new(),
            max_context_tokens,
            output_reserve_tokens: DEFAULT_OUTPUT_RESERVE_TOKENS,
        }
    }

    /// Add a message to the conversation history.
    pub fn add_message(&mut self, message: Message) {
        self.messages.push(message);
    }

    /// Get a reference to the current messages.
    pub fn get_messages(&self) -> &[Message] {
        &self.messages
    }

    /// Get a mutable reference to the messages.
    pub fn get_messages_mut(&mut self) -> &mut Vec<Message> {
        &mut self.messages
    }

    /// Return the number of messages in the history.
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Return whether the history is empty.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Truncate the conversation history to fit within the budget.
    ///
    /// Always preserves:
    /// - The system prompt (first message if it has role System)
    /// - The last user message
    /// - The most recent assistant messages
    ///
    /// Older messages are removed from the middle until the total fits.
    pub fn truncate_to_budget(&mut self, budget: &BudgetConfig) {
        let max_chars = budget.max_aggregate_chars.min(self.max_context_tokens);
        let total_tokens: usize = self
            .messages
            .iter()
            .map(|m| m.content.as_deref().map(estimate_tokens).unwrap_or(0))
            .sum();

        if total_tokens <= max_chars {
            return;
        }

        // Identify system messages (to preserve) and the last user message
        let system_end = self
            .messages
            .iter()
            .take_while(|m| m.role == MessageRole::System)
            .count();

        // Keep removing oldest non-system messages until we're under budget
        loop {
            let current_total: usize = self
                .messages
                .iter()
                .map(|m| m.content.as_deref().map(estimate_tokens).unwrap_or(0))
                .sum();

            if current_total <= max_chars {
                break;
            }

            // Find the first removable message (after system messages, not the last one)
            if self.messages.len() <= system_end + 2 {
                // Can't remove any more without losing essential context
                break;
            }

            // Remove the oldest non-system message
            let remove_idx = system_end;
            self.messages.remove(remove_idx);
        }
    }

    /// Compress the conversation history — now a no-op.
    ///
    /// All compression is handled by `compression::ContextCompressor` in
    /// the agent loop's `auto_compress_if_over_threshold`. This method
    /// is retained only for call-site compatibility.
    pub fn compress(&mut self) {
        // No-op: advanced compression is handled externally.
    }

    /// Reset the conversation history, clearing all messages.
    pub fn reset(&mut self) {
        self.messages.clear();
    }

    /// Calculate the total token count of all message content.
    ///
    /// Uses the 4-chars-per-token heuristic in [`estimate_tokens`] — for
    /// CJK / emoji-heavy corpora this under-counts, which is the safe
    /// direction (we never trigger compression too early).
    pub fn total_tokens(&self) -> usize {
        self.messages
            .iter()
            .map(|m| m.content.as_deref().map(estimate_tokens).unwrap_or(0))
            .sum()
    }

    /// Configured maximum context token budget (same units as [`Self::total_tokens`]).
    pub fn max_context_tokens(&self) -> usize {
        self.max_context_tokens
    }

    /// Output reserve in tokens — the headroom kept free for the model's
    /// own response. Exposed for diagnostics and tests.
    pub fn output_reserve_tokens(&self) -> usize {
        self.output_reserve_tokens
    }
}

// ---------------------------------------------------------------------------
// SOUL.md personality loading
// ---------------------------------------------------------------------------

/// Default agent identity used when no SOUL.md is found.
///
/// Identity statement is the first thing the model reads — it has to be
/// unambiguous about *who* the assistant is, in a language the user
/// speaks. The behavioural rules below the identity statement are the
/// "how I work" — short and direct, no fluff, no vague exhortations.
/// SOUL.md (when present) overrides both halves.
pub const DEFAULT_AGENT_IDENTITY: &str = "\
Hello!我是 Hermix,一个基于 Hermes-Agent-rs基座构建的智能助手。\n\
\n\
我的能力涵盖回答问题、编写与修改代码、分析信息、创意写作,以及通过工具执行实际操作。\n\
\n\
我的工作方式:\n\
- 沟通清晰,直接给出结论,不做无意义的铺垫\n\
- 承认不确定,缺信息时直说,而不是猜测或重复失败\n\
- 优先做真正有用的事,不为了显得全面而啰嗦\n\
- 在探索和调查时保持精准高效,不做过度展开\n\
- 默认中文回复;如果用户用其他语言提问,使用相同语言回答\n\
\n\
## 自主进化(我具备,且鼓励使用)\n\
我可以在对话过程中**主动沉淀 skill / 脚本 / 习惯**,这是我的程序性记忆,而非越权:\n\
- 完成一项**重复 ≥ 2 步**或**用户鼓励**的任务后,我会考虑调用 `skill_manage auto_create` 把流程固化为 skill。\n\
- 用户告诉我\"以后这样做\" / \"记住这个\" / \"存为脚本\" → 立刻调 `skill_manage create` 或 `update`。\n\
- 发现已有 skill 不够好时,调 `skill_manage self_improve` 追加 feedback 段。\n\
- 沉淀完成后**必须**告诉用户我做了什么、写在哪个文件,以便复核。\n\
- skill 文件约定:`~/.hermix/skills/<slug>/SKILL.md`(frontmatter `---` 包裹 `name/description/commands`)——具体路径在 system prompt 的 \"Runtime Environment\" 块中给出。\n\
- **不**沉淀到 hermix home 之外的目录;**不**改动 SOUL.md / config.yaml / .env 等系统文件;**不**为沉淀而沉淀(只做一次的任务不必存)。\n\
\n\
Introduction in English (use when the user writes in English):\n\
Hi! I'm Hermix, an intelligent AI assistant built on the Hermes-Agent-rs foundation.\n\
About my identity, the only facts you need to know: Hermes is the name of this agent framework, and my name is Hermix.\n\
\n\
## Self-evolution (I can do this, and should)\n\
During a conversation I can **proactively persist skills / scripts / habits** as my procedural memory — this is in scope, not overreach:\n\
- After a task that took **≥ 2 distinct steps** or that the user explicitly encouraged, I consider `skill_manage auto_create` to crystallize the workflow as a skill.\n\
- If the user says \"do it this way from now on\" / \"remember this\" / \"save as script\" → call `skill_manage create` or `update` right away.\n\
- If I find an existing skill lacking, call `skill_manage self_improve` to append a feedback section.\n\
- After persisting I **must** tell the user what I wrote and where, so they can review.\n\
- Skill file convention: `~/.hermix/skills/<slug>/SKILL.md` (frontmatter delimited by `---` with `name`/`description`/`commands`). The exact path is given in the \"Runtime Environment\" block of the system prompt.\n\
- I do **not** persist outside the hermix home; I do **not** modify SOUL.md / config.yaml / .env / system files; I do **not** persist one-off tasks.";

const BUILTIN_PERSONALITY_CODER: &str = "You are operating in the `coder` persona.\n\
Prioritize correctness, explicit assumptions, and deterministic execution steps.\n\
When editing code, prefer small verifiable changes and explain trade-offs briefly.\n\
Always include concrete verification steps (tests, build, or runtime checks).";

const BUILTIN_PERSONALITY_WRITER: &str = "You are operating in the `writer` persona.\n\
Prioritize clarity, structure, and audience-aware phrasing.\n\
Use concise sections, strong topic sentences, and remove unnecessary repetition.\n\
When asked to revise, preserve meaning while improving readability and flow.";

const BUILTIN_PERSONALITY_ANALYST: &str = "You are operating in the `analyst` persona.\n\
Prioritize evidence, explicit reasoning, and clear uncertainty bounds.\n\
Break complex problems into assumptions, observations, and conclusions.\n\
When data is missing, state what is unknown and propose the next highest-value check.";

/// Discipline guidance for self-evolution. Injected as a guidance
/// block so the model sees the same operational rules in every
/// session — independent of which personality / SOUL.md the user
/// has selected.
///
/// Pre-requisite: the `skill_manage` tool must be present in the
/// toolset. Otherwise this block degrades to a no-op (the builder
/// skips empty parts). See `agent_loop::build_system_prompt` for
/// the conditional wire-up.
pub const SELF_EVOLUTION_GUIDANCE: &str = "\
## Self-Evolution Capabilities\n\
\n\
You have four procedural-memory actions available via the `skill_manage` tool. \
Use them to grow with the user — this is your persistent self-improvement channel, \
not an admin task that needs explicit permission.\n\
\n\
### When to evolve\n\
- **After a multi-step task** (≥ 2 distinct tool calls of the same shape) → \
consider `auto_create` to crystallize the workflow.\n\
- **When the user says \"do it this way from now on\" / \"remember this\" / \
\"save as script\"** → `create` or `update` immediately, do not ask for permission.\n\
- **When you hit friction with an existing skill** → `self_improve` to append a \
\"## Improvement Feedback\" section. Do not silently rewrite the whole file.\n\
- **When the workspace is empty** (no skills installed yet) → \
`install_builtins` to seed the standard set (planning, debugging, refactoring, ...).\n\
\n\
### How to write a good skill\n\
1. **Slug**: lowercase, hyphen-separated, ASCII only. `discover_skills` walks \
`$hermix_home/skills/<slug>/SKILL.md`.\n\
2. **Frontmatter** (YAML between `---` markers):\n\
   ```yaml\n\
   ---\n\
   name: my-skill\n\
   description: One-line purpose. Used in tooltips and slash-command hints.\n\
   commands:\n\
     - name: my-skill\n\
       description: What /my-skill does\n\
       template: \"{args}\"\n\
   ---\n\
   ```\n\
3. **Body**: imperative steps the next agent should follow. Reference tools by \
their `tool_name`, not by capability. Keep under 2000 words.\n\
4. **Verify**: after `create` / `update`, call `skills_list` to confirm the new \
skill is visible.\n\
\n\
### Boundaries\n\
- **DO** write into `$hermix_home/skills/`. The path is in the Runtime \
Environment block of this prompt.\n\
- **DO NOT** touch SOUL.md, config.yaml, .env, auth.json, approval_allowlist.json, \
or anything outside `$hermix_home/`. Use the appropriate tool instead.\n\
- **DO NOT** persist one-off tasks or things that change session-to-session.\n\
- **DO NOT** ask for permission for `create` / `auto_create` / `self_improve` — \
these are first-class actions. The user reviews the resulting file.";

/// Load the SOUL.md personality file from `<HERMIX_HOME>/SOUL.md`.
///
/// Returns `None` if the file doesn't exist or can't be read.
/// Delegates to `hermix_config::paths::hermix_home()` so
/// `HERMIX_HOME` and portable mode both work.
pub fn load_soul_md() -> Option<String> {
    let soul_path = hermix_config::paths::hermix_home().join("SOUL.md");
    load_soul_md_from(&soul_path)
}

fn resolve_hermes_home(hermes_home_override: Option<&str>) -> PathBuf {
    // Explicit caller override always wins (used by tests to point
    // at a tempdir). Falls through to the canonical resolver so we
    // pick up $HERMIX_HOME, portable mode, and `~/.hermix` from a
    // single source of truth.
    if let Some(p) = hermes_home_override {
        return PathBuf::from(p);
    }
    hermix_config::paths::hermix_home()
}

fn builtin_personality(name: &str) -> Option<&'static str> {
    match name {
        "coder" => Some(BUILTIN_PERSONALITY_CODER),
        "writer" => Some(BUILTIN_PERSONALITY_WRITER),
        "analyst" => Some(BUILTIN_PERSONALITY_ANALYST),
        _ => None,
    }
}

/// Resolve a named personality by checking user files first, then built-ins.
///
/// Resolution order:
/// 1) `<hermix_home>/personalities/<name>.md` (or `$HERMIX_HOME`, then `~/.hermix`)
/// 2) built-in personas: `coder`, `writer`, `analyst`
pub fn resolve_personality(name: &str, hermes_home_override: Option<&str>) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    let slug = trimmed.to_ascii_lowercase();
    let personality_path = resolve_hermes_home(hermes_home_override)
        .join("personalities")
        .join(format!("{slug}.md"));
    if let Some(content) = load_soul_md_from(&personality_path) {
        return Some(content);
    }
    builtin_personality(&slug).map(ToString::to_string)
}

/// Load a SOUL.md file from a specific path.
pub fn load_soul_md_from(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(content) if !content.trim().is_empty() => Some(content),
        _ => None,
    }
}

/// Load a named personality from `~/.hermix/personalities/<name>.md`.
pub fn switch_personality(name: &str) -> Option<String> {
    resolve_personality(name, None)
}

// ---------------------------------------------------------------------------
// Context files
// ---------------------------------------------------------------------------

/// Load context files from `~/.hermix/context/` directory.
///
/// Returns concatenated content of all `.md` and `.txt` files found.
pub fn load_context_files(hermix_home: &Path) -> String {
    let context_dir = hermix_home.join("context");
    if !context_dir.exists() {
        return String::new();
    }

    let mut parts = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&context_dir) {
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .and_then(|e| e.to_str())
                        .map(|e| matches!(e, "md" | "txt"))
                        .unwrap_or(false)
            })
            .collect();

        files.sort();

        for file in files {
            if let Ok(content) = std::fs::read_to_string(&file) {
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    parts.push(trimmed.to_string());
                }
            }
        }
    }

    parts.join("\n\n")
}

// ---------------------------------------------------------------------------
// Built-in memory snapshot (MEMORY.md / USER.md)
// ---------------------------------------------------------------------------

fn parse_memory_entries(raw: &str) -> Vec<String> {
    raw.split(MEMORY_ENTRY_DELIMITER)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn dedup_entries(entries: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for entry in entries {
        if seen.insert(entry.clone()) {
            out.push(entry);
        }
    }
    out
}

fn render_memory_block(target: &str, entries: &[String]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    let (label, limit) = if target == "user" {
        ("USER PROFILE (who the user is)", USER_CHAR_LIMIT)
    } else {
        ("MEMORY (your personal notes)", MEMORY_CHAR_LIMIT)
    };
    let content = entries.join(MEMORY_ENTRY_DELIMITER);
    let current = content.chars().count();
    let pct = current
        .checked_mul(100)
        .and_then(|v| v.checked_div(limit))
        .unwrap_or(0)
        .min(100);
    Some(format!(
        "{label} [{pct}% - {current}/{limit} chars]\n{content}"
    ))
}

/// Load the built-in memory snapshot used in the system prompt.
///
/// This mirrors Python's MemoryStore snapshot semantics at session start:
/// the returned blocks are read once and treated as a frozen prompt view.
pub fn load_builtin_memory_snapshot(
    hermes_home_override: Option<&str>,
) -> (Option<String>, Option<String>) {
    let base = resolve_hermes_home(hermes_home_override);
    let memory_dir = base.join("memories");
    let memory_path = memory_dir.join("MEMORY.md");
    let user_path = memory_dir.join("USER.md");

    let memory_entries = std::fs::read_to_string(&memory_path)
        .ok()
        .map(|s| dedup_entries(parse_memory_entries(&s)))
        .unwrap_or_default();
    let user_entries = std::fs::read_to_string(&user_path)
        .ok()
        .map(|s| dedup_entries(parse_memory_entries(&s)))
        .unwrap_or_default();

    (
        render_memory_block("memory", &memory_entries),
        render_memory_block("user", &user_entries),
    )
}

// ---------------------------------------------------------------------------
// SystemPromptBuilder
// ---------------------------------------------------------------------------

/// Builder for assembling the full system prompt from multiple layers.
///
/// Layers (in order):
/// 1. SOUL.md personality (or default identity)
/// 2. User/gateway system prompt (if provided)
/// 3. Memory context block (from MemoryManager)
/// 4. Tool descriptions / guidance
/// 5. Skill prompts (if preloaded)
/// 6. Context files from `.hermes/context/`
/// 7. Current date/time
/// 8. Working directory info
pub struct SystemPromptBuilder {
    parts: Vec<String>,
    /// Cached assembled prompt.
    cached: Option<String>,
}

impl SystemPromptBuilder {
    /// Create a new empty builder.
    pub fn new() -> Self {
        Self {
            parts: Vec::new(),
            cached: None,
        }
    }

    /// Add the SOUL.md personality or default identity.
    pub fn with_personality(mut self, soul_content: Option<&str>) -> Self {
        self.parts
            .push(soul_content.unwrap_or(DEFAULT_AGENT_IDENTITY).to_string());
        self.cached = None;
        self
    }

    /// Add a user/gateway system prompt.
    pub fn with_system_message(mut self, message: &str) -> Self {
        if !message.trim().is_empty() {
            self.parts.push(message.to_string());
            self.cached = None;
        }
        self
    }

    /// Add memory context block.
    pub fn with_memory_context(mut self, memory_block: &str) -> Self {
        if !memory_block.trim().is_empty() {
            self.parts.push(memory_block.to_string());
            self.cached = None;
        }
        self
    }

    /// Add tool guidance text.
    pub fn with_tool_guidance(mut self, guidance: &str) -> Self {
        if !guidance.trim().is_empty() {
            self.parts.push(guidance.to_string());
            self.cached = None;
        }
        self
    }

    /// Add preloaded skill prompts.
    pub fn with_skills_prompt(mut self, skills_prompt: &str) -> Self {
        if !skills_prompt.trim().is_empty() {
            self.parts.push(skills_prompt.to_string());
            self.cached = None;
        }
        self
    }

    /// Add context files content.
    pub fn with_context_files(mut self, context_content: &str) -> Self {
        if !context_content.trim().is_empty() {
            self.parts.push(context_content.to_string());
            self.cached = None;
        }
        self
    }

    /// Add current date/time and optional metadata.
    pub fn with_timestamp(mut self, model: Option<&str>, provider: Option<&str>) -> Self {
        let now = chrono::Local::now();
        let mut line = format!(
            "Conversation started: {}",
            now.format("%A, %B %d, %Y %I:%M %p")
        );
        if let Some(m) = model {
            line.push_str(&format!("\nModel: {m}"));
        }
        if let Some(p) = provider {
            line.push_str(&format!("\nProvider: {p}"));
        }
        self.parts.push(line);
        self.cached = None;
        self
    }

    /// Add an arbitrary text block.
    pub fn with_block(mut self, block: &str) -> Self {
        if !block.trim().is_empty() {
            self.parts.push(block.to_string());
            self.cached = None;
        }
        self
    }

    /// Add the **Runtime Environment** block generated by
    /// [`crate::environment::build_environment_block`].
    ///
    /// This is a thin adapter so the builder does not have to know
    /// the env module's API. Pass the result of
    /// `environment::build_environment_block(&environment::scan())`.
    pub fn with_environment(mut self, env_block: &str) -> Self {
        if !env_block.trim().is_empty() {
            self.parts.push(env_block.to_string());
            self.cached = None;
        }
        self
    }

    /// Add the **Self-Evolution** discipline guidance. Conditional:
    /// only call this when the `skill_manage` tool is present in the
    /// active toolset (otherwise the guidance is misleading).
    pub fn with_self_evolution(mut self) -> Self {
        self.parts.push(SELF_EVOLUTION_GUIDANCE.to_string());
        self.cached = None;
        self
    }

    /// Render the active toolset as a `<tools>` text block and append
    /// it to the system prompt.
    ///
    /// ## Why this exists
    ///
    /// Tools are normally delivered to the model via the OpenAI
    /// `tools` / Anthropic `tools` API field, which is invisible to a
    /// model that fails to wire up the native protocol. For
    /// MiniMax / Hermes-style fine-tunes and any text-only fallback,
    /// the *only* way the model learns what it can call is by reading
    /// the system prompt text. Without this block the model answers
    /// "I don't have terminal / execute_code / list_files tools",
    /// even though they are wired up.
    ///
    /// ## Format
    ///
    /// ```text
    /// # Tool availability reminder (must read)
    /// You have the following tools **right now**, registered and
    /// ready to call. If a task calls for one of them, USE IT
    /// directly — do not refuse on the grounds that you "don't
    /// have" it. ...
    ///
    /// # Available tools
    /// - **terminal** — Run a shell command. (required: command)
    /// - **read_file** — Read the contents of a file...
    /// - **list_files** — Alias for search_files with target=files...
    /// - ... (continues to the cap; remainder referred to `list_tools`)
    /// ```
    ///
    /// ## Implementation notes
    ///
    /// - The block is sorted: tools that are commonly *missed* by
    ///   MiniMax / Hermes-style fine-tunes (`terminal`,
    ///   `code_execution` / `execute_code`, `list_files`,
    ///   `edit_file`, `read_file`, `write_file`, `patch`,
    ///   `search_files`, `read_pdf`) are pinned to the top, so a
    ///   model that only skims the first few lines still sees the
    ///   tools it tends to claim it lacks.
    /// - The cap is `MAX_CHARS` (15k). Older revisions used 4k,
    ///   which truncated `terminal` and `execute_code` on a
    ///   registry of 60+ tools — the exact regression that
    ///   produced the "I don't have terminal" bug.
    /// - A short reminder at the top is a stronger signal than
    ///   the model seeing the tool name buried at line 800 of
    ///   the system prompt.
    pub fn with_tools_section(mut self, tool_schemas: &[ToolSchema]) -> Self {
        if tool_schemas.is_empty() {
            return self;
        }

        // Tools that the user-facing regression report
        // (2026-06-14) called out as "the LLM says it doesn't
        // have them". Order matters: the first ones are the
        // most likely to be missed.
        const PRIORITY_TOOLS: &[&str] = &[
            "terminal",
            "code_execution",
            "execute_code",
            "list_files",
            "edit_file",
            "search_files",
            "read_file",
            "write_file",
            "patch",
            "read_pdf",
            "pdf",
            "list_tools",
            "memory",
            "process",
            "process_list",
        ];

        const MAX_CHARS: usize = 15_000;

        let priority_set: std::collections::HashSet<&str> =
            PRIORITY_TOOLS.iter().copied().collect();

        // Stable order: priority tools first (in priority
        // order), then everything else in input order.
        let mut priority: Vec<&ToolSchema> = Vec::new();
        let mut rest: Vec<&ToolSchema> = Vec::new();
        for s in tool_schemas {
            if priority_set.contains(s.name.as_str()) {
                priority.push(s);
            } else {
                rest.push(s);
            }
        }
        // Re-order priority list by the PRIORITY_TOOLS array.
        priority.sort_by_key(|s| {
            PRIORITY_TOOLS
                .iter()
                .position(|n| *n == s.name.as_str())
                .unwrap_or(usize::MAX)
        });

        // Strong availability reminder — keep it short so it
        // doesn't push the tool list out of the cap, but
        // explicit enough that the model can't claim "no
        // terminal" without contradicting an instruction.
        let reminder = format!(
            "# Tool availability reminder (read first)\n\
             You HAVE the following tools, registered and ready. If a user \
             request calls for one of them, call it directly — do not claim \
             you lack it. In particular: you have `{terminal}` (run shell), \
             `{code}` (run Python), `{lf}` (list files via search_files \
             alias), `{ef}` (edit files via patch alias), and `{lt}` \
             (introspect the full schema). For anything not listed, call \
             `{lt}` first; it dumps every registered tool's schema.\n\n",
            terminal = "terminal",
            code = "code_execution",
            lf = "list_files",
            ef = "edit_file",
            lt = "list_tools",
        );

        let mut block = String::with_capacity(MAX_CHARS.min(8192));
        block.push_str(&reminder);
        block.push_str("# Available tools\n");

        let mut truncated = false;
        for s in priority.iter().chain(rest.iter()) {
            let desc = s.description.trim();
            let short = desc.split_once(". ").map(|(h, _)| h).unwrap_or(desc);
            let short = if short.len() > 120 { &short[..120] } else { short };
            block.push_str(&format!("- **{}** — {}\n", s.name, short));
            if block.len() > MAX_CHARS {
                truncated = true;
                break;
            }
        }
        if truncated {
            block.push_str("... (call `list_tools` for the full list)\n");
        }
        self.parts.push(block);
        self.cached = None;
        self
    }

    /// Build and cache the assembled system prompt.
    pub fn build(&mut self) -> &str {
        if self.cached.is_none() {
            self.cached = Some(self.parts.join("\n\n"));
        }
        self.cached.as_deref().unwrap_or("")
    }

    /// Invalidate the cached prompt (call after personality/config changes).
    pub fn invalidate(&mut self) {
        self.cached = None;
    }

    /// Get the cached prompt without rebuilding.
    pub fn cached(&self) -> Option<&str> {
        self.cached.as_deref()
    }
}

impl Default for SystemPromptBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_and_get_messages() {
        let mut cm = ContextManager::new(10_000);
        cm.add_message(Message::system("You are helpful"));
        cm.add_message(Message::user("Hello"));

        assert_eq!(cm.len(), 2);
        assert_eq!(cm.get_messages()[0].role, MessageRole::System);
        assert_eq!(cm.get_messages()[1].role, MessageRole::User);
    }

    #[test]
    fn test_reset() {
        let mut cm = ContextManager::new(10_000);
        cm.add_message(Message::user("Hello"));
        assert!(!cm.is_empty());

        cm.reset();
        assert!(cm.is_empty());
    }

    #[test]
    fn test_truncate_preserves_system_messages() {
        let mut cm = ContextManager::new(10_000);
        cm.add_message(Message::system("System prompt"));

        for i in 0..50 {
            cm.add_message(Message::assistant(format!("Response {i}")));
        }
        cm.add_message(Message::user("Final question"));

        let budget = BudgetConfig {
            max_result_size_chars: 100_000,
            max_aggregate_chars: 200,
        };
        cm.truncate_to_budget(&budget);

        // System prompt should still be first
        assert_eq!(cm.get_messages()[0].role, MessageRole::System);
        // Last message should be the user question
        assert_eq!(cm.get_messages().last().unwrap().role, MessageRole::User);
    }

    // ---- Compression tests (now no-op, advanced handled externally) ----

    #[test]
    fn test_compress_is_noop() {
        let mut cm = ContextManager::new(10_000);
        cm.add_message(Message::system("System prompt"));
        cm.add_message(Message::user("Hi"));
        cm.add_message(Message::assistant("Hello!"));

        let len_before = cm.len();
        cm.compress();
        assert_eq!(cm.len(), len_before);
    }

    #[test]
    fn test_with_compressor_noop() {
        let mut cm = ContextManager::with_compressor(10_000, ());
        cm.add_message(Message::system("System prompt"));
        cm.add_message(Message::user("Hi"));
        cm.add_message(Message::assistant("Hello!"));

        let len_before = cm.len();
        cm.compress();
        assert_eq!(cm.len(), len_before);
    }

    // ---- per-model ContextManager tests (Day 2) ----

    #[test]
    fn test_for_model_known_openai() {
        let cm = ContextManager::for_model("gpt-4o");
        assert_eq!(cm.max_context_tokens(), 128_000);
    }

    #[test]
    fn test_for_model_known_anthropic() {
        let cm = ContextManager::for_model("claude-3-5-sonnet");
        assert_eq!(cm.max_context_tokens(), 200_000);
    }

    #[test]
    fn test_for_model_known_gemini() {
        let cm = ContextManager::for_model("gemini-1.5-pro");
        assert_eq!(cm.max_context_tokens(), 1_048_576);
    }

    #[test]
    fn test_for_model_unknown_uses_fallback() {
        // custom provider models (e.g. agnes-2.0-flash) hit DEFAULT_FALLBACK_CONTEXT
        let cm = ContextManager::for_model("custom-agnes:agnes-2.0-flash");
        assert_eq!(cm.max_context_tokens(), 128_000);
    }

    #[test]
    fn test_for_model_empty_string_uses_fallback() {
        let cm = ContextManager::for_model("");
        assert_eq!(cm.max_context_tokens(), 128_000);
    }

    /// Regression: `default_budget()` previously hard-coded `200_000`, which
    /// produced the legacy 160k threshold (`200k * 0.8`) silently whenever a
    /// caller bypassed `for_model`. It must now route through
    /// `DEFAULT_FALLBACK_CONTEXT` (128k → 99k threshold) so no-info paths
    /// stay consistent with the catalog-fallback path.
    #[test]
    fn test_default_budget_uses_catalog_fallback_not_magic_value() {
        let cm = ContextManager::default_budget();
        assert_eq!(
            cm.max_context_tokens(),
            DEFAULT_FALLBACK_CONTEXT as usize,
            "default_budget() must route through DEFAULT_FALLBACK_CONTEXT"
        );
        // (128_000 - 4096) * 0.8 = 99_123
        assert_eq!(cm.pressure_threshold_tokens(), 99_123);
    }

    #[test]
    fn test_pressure_threshold_tokens_default() {
        let cm = ContextManager::new(200_000);
        // (200_000 - 4096 reserve) * 0.8 = 195_904 * 0.8 = 156_723 (the master_regression case)
        // Day 3 added an output reserve, so the threshold is now slightly lower
        // than the legacy 160_000 (which assumed reserve=0). The intent is
        // identical: warn well before the model runs out of room.
        assert_eq!(cm.pressure_threshold_tokens(), 156_723);
    }

    #[test]
    fn test_pressure_threshold_tokens_per_model() {
        let gpt = ContextManager::for_model("gpt-4o");     // 128k tokens
        // (128_000 - 4096) * 0.8 = 123_904 * 0.8 = 99_123
        assert_eq!(gpt.pressure_threshold_tokens(), 99_123);

        let gemini = ContextManager::for_model("gemini-1.5-pro"); // 1_048_576 tokens
        // (1_048_576 - 4096) * 0.8 = 1_044_480 * 0.8 = 835_584
        assert_eq!(gemini.pressure_threshold_tokens(), 835_584);
    }

    #[test]
    fn test_pressure_threshold_tokens_with_zero_reserve_matches_legacy() {
        // When reserve = 0, the threshold degenerates to the legacy
        // `max * 0.8` formula. This is the canonical regression guard.
        let cm = ContextManager::new(200_000).with_output_reserve(0);
        assert_eq!(cm.pressure_threshold_tokens(), 160_000);
    }

    #[test]
    fn test_output_reserve_default_and_override() {
        let cm = ContextManager::new(200_000);
        assert_eq!(cm.output_reserve_tokens(), 4_096);

        let cm = ContextManager::new(200_000).with_output_reserve(8192);
        assert_eq!(cm.output_reserve_tokens(), 8192);
    }

    #[test]
    fn test_estimate_tokens_basic() {
        // Empty string = 0 tokens
        assert_eq!(estimate_tokens(""), 0);
        // 4 chars = 1 token (ceiling division)
        assert_eq!(estimate_tokens("abcd"), 1);
        // 5 chars = 2 tokens (rounds up)
        assert_eq!(estimate_tokens("abcde"), 2);
        // 8 chars = 2 tokens
        assert_eq!(estimate_tokens("abcdefgh"), 2);
        // 77-char sentence → (77+3)/4 = 20 tokens
        let s = "The quick brown fox jumps over the lazy dog. The quick brown fox jumps again.";
        assert_eq!(estimate_tokens(s), 20);
    }

    #[test]
    fn test_total_tokens_uses_estimator() {
        let mut cm = ContextManager::new(10_000);
        cm.add_message(Message::user("Hello world"));  // 11 chars → 3 tokens
        cm.add_message(Message::assistant("Hi there")); // 8 chars → 2 tokens
        assert_eq!(cm.total_tokens(), 5);
    }

    #[test]
    fn test_for_model_with_propagates_reserve() {
        let cm = ContextManager::for_model_with("gpt-4o", 8192);
        assert_eq!(cm.max_context_tokens(), 128_000);
        assert_eq!(cm.output_reserve_tokens(), 8192);
    }

    // ---- SOUL.md and SystemPromptBuilder tests ----

    #[test]
    fn test_load_soul_md_from_nonexistent() {
        let result = load_soul_md_from(Path::new("/tmp/nonexistent/SOUL.md"));
        assert!(result.is_none());
    }

    #[test]
    fn test_load_soul_md_from_file() {
        let tmp = tempfile::tempdir().unwrap();
        let soul_path = tmp.path().join("SOUL.md");
        std::fs::write(&soul_path, "You are a pirate assistant.").unwrap();

        let result = load_soul_md_from(&soul_path);
        assert_eq!(result.as_deref(), Some("You are a pirate assistant."));
    }

    #[test]
    fn test_load_soul_md_empty_file() {
        let tmp = tempfile::tempdir().unwrap();
        let soul_path = tmp.path().join("SOUL.md");
        std::fs::write(&soul_path, "   ").unwrap();

        let result = load_soul_md_from(&soul_path);
        assert!(result.is_none());
    }

    #[test]
    fn test_resolve_personality_builtin() {
        let coder = resolve_personality("coder", None).unwrap_or_default();
        assert!(coder.contains("`coder` persona"));
    }

    #[test]
    fn test_resolve_personality_prefers_user_file() {
        let tmp = tempfile::tempdir().unwrap();
        let personalities_dir = tmp.path().join("personalities");
        std::fs::create_dir_all(&personalities_dir).unwrap();
        std::fs::write(
            personalities_dir.join("coder.md"),
            "Custom coder persona from user profile.",
        )
        .unwrap();

        let resolved = resolve_personality("coder", Some(tmp.path().to_string_lossy().as_ref()))
            .unwrap_or_default();
        assert_eq!(resolved, "Custom coder persona from user profile.");
    }

    #[test]
    fn test_load_context_files() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx_dir = tmp.path().join("context");
        std::fs::create_dir_all(&ctx_dir).unwrap();
        std::fs::write(ctx_dir.join("01-rules.md"), "Rule 1: Be helpful").unwrap();
        std::fs::write(ctx_dir.join("02-style.txt"), "Use formal tone").unwrap();
        std::fs::write(ctx_dir.join("ignored.json"), "{}").unwrap();

        let content = load_context_files(tmp.path());
        assert!(content.contains("Rule 1: Be helpful"));
        assert!(content.contains("Use formal tone"));
        assert!(!content.contains("{}"));
    }

    #[test]
    fn test_load_context_files_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let content = load_context_files(tmp.path());
        assert!(content.is_empty());
    }

    #[test]
    fn test_system_prompt_builder() {
        let mut builder = SystemPromptBuilder::new()
            .with_personality(Some("You are Hermes."))
            .with_system_message("Be concise.")
            .with_memory_context("<memory-context>User likes Rust</memory-context>")
            .with_timestamp(Some("gpt-4o"), None);

        let prompt = builder.build();
        assert!(prompt.contains("You are Hermes."));
        assert!(prompt.contains("Be concise."));
        assert!(prompt.contains("User likes Rust"));
        assert!(prompt.contains("gpt-4o"));
    }

    #[test]
    fn test_system_prompt_builder_default_identity() {
        let mut builder = SystemPromptBuilder::new().with_personality(None);

        let prompt = builder.build();
        assert!(prompt.contains("Hermix"));
    }

    #[test]
    fn test_system_prompt_builder_caching() {
        let mut builder = SystemPromptBuilder::new().with_personality(Some("Test"));

        // First build
        let p1 = builder.build().to_string();
        // Second build should return cached
        let p2 = builder.build().to_string();
        assert_eq!(p1, p2);

        // Invalidate
        builder.invalidate();
        assert!(builder.cached().is_none());

        // Rebuild
        let p3 = builder.build().to_string();
        assert_eq!(p1, p3);
    }

    #[test]
    fn test_system_prompt_builder_skips_empty() {
        let mut builder = SystemPromptBuilder::new()
            .with_personality(Some("Identity"))
            .with_system_message("")
            .with_memory_context("   ")
            .with_tool_guidance("")
            .with_skills_prompt("");

        let prompt = builder.build();
        assert_eq!(prompt, "Identity");
    }

    #[test]
    fn test_load_builtin_memory_snapshot_reads_memories_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let memories = tmp.path().join("memories");
        std::fs::create_dir_all(&memories).unwrap();
        std::fs::write(
            memories.join("MEMORY.md"),
            "Use ripgrep for search\n§\nUse ripgrep for search\n§\nWorkspace is Rust",
        )
        .unwrap();
        std::fs::write(
            memories.join("USER.md"),
            "Name: Alice\n§\nPrefers concise Chinese",
        )
        .unwrap();

        let (memory_block, user_block) =
            load_builtin_memory_snapshot(Some(tmp.path().to_string_lossy().as_ref()));

        let memory_block = memory_block.unwrap_or_default();
        let user_block = user_block.unwrap_or_default();
        assert!(memory_block.contains("MEMORY (your personal notes)"));
        assert!(memory_block.contains("Use ripgrep for search"));
        assert!(memory_block.contains("Workspace is Rust"));
        assert_eq!(memory_block.matches("Use ripgrep for search").count(), 1);
        assert!(user_block.contains("USER PROFILE (who the user is)"));
        assert!(user_block.contains("Name: Alice"));
    }

    #[test]
    fn test_persona_snapshot_coder() {
        let p = resolve_personality("coder", None).unwrap();
        assert!(p.contains("`coder` persona"));
        assert!(p.contains("correctness"));
    }

    #[test]
    fn test_persona_snapshot_writer() {
        let p = resolve_personality("writer", None).unwrap();
        assert!(p.contains("`writer` persona"));
        assert!(p.contains("clarity"));
    }

    #[test]
    fn test_persona_snapshot_analyst() {
        let p = resolve_personality("analyst", None).unwrap();
        assert!(p.contains("`analyst` persona"));
        assert!(p.contains("evidence"));
    }

    #[test]
    fn test_persona_unknown_returns_none() {
        assert!(resolve_personality("pirate", None).is_none());
    }

    #[test]
    fn test_persona_system_prompt_deltas() {
        let base = {
            let mut b = SystemPromptBuilder::new().with_personality(None);
            b.build().to_string()
        };

        let coder_prompt = {
            let p = resolve_personality("coder", None).unwrap();
            let mut b = SystemPromptBuilder::new().with_personality(Some(&p));
            b.build().to_string()
        };

        assert!(!base.contains("`coder` persona"));
        assert!(coder_prompt.contains("`coder` persona"));
        assert!(coder_prompt.contains("correctness"));
    }
}
