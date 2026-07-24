//! Context management for conversation history.
//!
//! The `ContextManager` tracks messages, enforces budget constraints on the
//! conversation window. All compression is handled by
//! `crate::compression::ContextCompressor` (LLM-powered).
//!
//! Also provides SOUL.md personality loading, context file injection, and
//! full system prompt assembly (corresponding to Python `run_agent.py`'s
//! `_build_system_prompt`).
//!
//! # Context Layer Model (AL-9, 2026-07-09)
//!
//! The agent's context is composed of 5 distinct layers, each with
//! different invalidation semantics and cache strategies:
//!
//! ## Layer 1: System Prompt (Stable)
//! - **Contents**: Soul/identity, configured `system_prompt`, directions,
//!   tool definitions section, personality profile, platform hints.
//! - **Invalidation**: Only on model / personality / `system_prompt` /
//!   workspace change.
//! - **Cache**: `PromptCache` keyed on stable attributes.
//! - **Builder**: `build_stable_layer()` in `prompt_assembly.rs`.
//!
//! ## Layer 2: Tool Definitions
//! - **Contents**: JSON schemas for all available tools.
//! - **Invalidation**: On tool registry change (rare during a session).
//! - **Cache**: Part of the stable layer cache.
//! - **Builder**: `ToolSchema` vec passed to `chat_completion[_stream]`.
//!
//! ## Layer 3: Conversation History
//! - **Contents**: User messages, assistant messages (with thinking blocks),
//!   tool calls, tool results. This is the largest and most volatile layer.
//! - **Invalidation**: Every turn (new messages added).
//! - **Cache**: No cache — recomputed every turn via `messages_for_api_call`.
//! - **Compression**: `ContextCompressor` triggers at 80% of budget.
//! - **Filtering**: `messages_for_api_call` normalizes messages (merge
//!   consecutive user, strip stale errors, strip old `reasoning_content`).
//!
//! ## Layer 4: Memory
//! - **Contents**: Built-in memory snapshot, memory router blocks, skills
//!   prompt, workspace/personal context files.
//! - **Invalidation**: On memory / skills / context-files mutation (signalled
//!   by an `epoch` u64 tick).
//! - **Cache**: `PromptCache` context layer, keyed on epoch.
//! - **Builder**: `build_context_layer()` in `prompt_assembly.rs`.
//!
//! ## Layer 5: Dynamic Suffix
//! - **Contents**: Timestamp block (model + provider name), optional
//!   `session_id` line.
//! - **Invalidation**: Every turn (timestamp changes).
//! - **Cache**: No cache — rebuilt every turn.
//! - **Builder**: `build_dynamic_suffix()` in `prompt_assembly.rs`.
//!
//! The 3-layer cache layout (stable / context / volatile) ensures that a
//! mutation to one layer does not force the other to re-render. The
//! `build_system_prompt()` function assembles all three layers into the
//! final system prompt string.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use hermix_config::LlmProviderConfig;
use hermix_core::{BudgetConfig, Message, MessageRole, ToolSchema};
use hermix_intelligence::model_metadata::{
    get_model_context_length, get_model_context_length_with, DEFAULT_FALLBACK_CONTEXT,
};
use hermix_intelligence::DomainMode;

const MEMORY_ENTRY_DELIMITER: &str = "\n§\n";
const MEMORY_CHAR_LIMIT: usize = 2200;
const USER_CHAR_LIMIT: usize = 1375;

/// Pressure ratio at which auto-compression / context-pressure warnings fire.
/// /// 0.80 = trigger at 80% of context window, leaving 20% headroom for tool
/// outputs and assistant generation. The old 0.50 default wasted half the
/// context window on large models (128k+).
/// /// This constant is the **fallback default** used when no
/// [`crate::compression::CompressorConfig`] is supplied (e.g. tests and
/// the smoke `resolve_context` example). Production callers read the
/// ratio off `CompressorConfig.pressure_threshold_percent`, which is
/// the single source of truth at runtime.
pub const CONTEXT_PRESSURE_TRIGGER_RATIO: f64 = 0.90;

/// Default output reservation in tokens.
/// /// Subtracts from the *effective* context window before applying the
/// pressure ratio, so the LLM has room to emit its response without us
/// triggering compression mid-stream. 4096 tokens ≈ 16 KB of text — enough
/// for a normal assistant turn including tool calls.
pub const DEFAULT_OUTPUT_RESERVE_TOKENS: usize = 4096;

/// Rough heuristic for converting a UTF-8 string into a token count.
/// /// Uses the standard 4-chars-per-token rule that the original Python
/// `run_agent.py` budgeted against. CJK / emoji-heavy text will be
/// under-counted (one ideograph is often its own token), but the
/// estimator is intentionally a *lower bound* — if anything it
/// over-protects, never under-protects, the context window.
/// /// Replace with provider-native tokenizers (tiktoken for OpenAI,
/// the Anthropic SDK's tokenizer) once a per-model wiring layer
/// exists; the call sites (`total_tokens`, `truncate_to_budget`) do
/// not need to change.
/// /// ### Why two estimators coexist (2026-07-05)
/// /// `ContextManager::estimate_tokens` (here) and
/// `compression::count_tokens` (`tiktoken_rs::cl100k_base`) are
/// deliberately **not** the same:
/// - `estimate_tokens` drives the **pressure threshold** the agent
/// uses to decide "time to compress". Under-counting CJK is the
/// safe direction: we never trigger compression too early, and a
/// short over-budget is far cheaper than a mid-stream truncation.
/// - `count_tokens` drives the **summary sizing budget** (how many
/// tokens to budget for the LLM-generated handoff summary).
/// Under-counting there would mean we ask the LLM for "2000 tokens
/// of summary" and get 4000 tokens back, blowing the output window.
/// So the summary budget gets the accurate (tiktoken) count.
/// /// The pressure-vs-summary mismatch is documented and intentional.
#[inline]
pub fn estimate_tokens(s: &str) -> usize {
    // Improved heuristic that handles CJK text better:
    // - ASCII/Latin: ~4 chars per token
    // - CJK/Unicode: ~1.5 chars per token (typically 1-2 tokens each)
    let mut cjk_chars = 0usize;
    let mut ascii_bytes = 0usize;
    for c in s.chars() {
        if c.is_ascii() {
            ascii_bytes += 1;
        } else {
            cjk_chars += 1;
        }
    }
    // CJK chars: ceil(cjk_chars * 1.5)
    let cjk_tokens = (cjk_chars * 3 + 1) / 2;
    let ascii_tokens = (ascii_bytes + 3) / 4;
    cjk_tokens + ascii_tokens
}

// ---------------------------------------------------------------------------
// ContextManager
// ---------------------------------------------------------------------------

/// Manages conversation history with budget-aware truncation and compression.
#[derive(Debug, Clone)]
pub struct ContextManager {
    messages: Vec<Message>,
    /// Maximum total tokens for the conversation (excluding system messages).
    /// /// As of Day 3 the unit is **tokens**, not chars. The underlying
    /// estimator is [`estimate_tokens`] (4-chars-per-token heuristic),
    /// not a provider-native tokenizer.
    max_context_tokens: usize,
    /// Tokens reserved for the LLM's response at the end of the turn.
    /// /// `pressure_threshold_tokens` returns `(max - reserve) * ratio` so
    /// we don't fire the pressure warning while the model still has
    /// legitimate headroom to emit its output.
    output_reserve_tokens: usize,
    /// Pressure ratio (0.0–1.0) at which auto-compression / warnings fire.
    /// Defaults to [`CONTEXT_PRESSURE_TRIGGER_RATIO`] (0.50). Overridden
    /// by `compaction.pressure_threshold_percent` in config.yaml.
    pressure_ratio: f64,
    /// running sum of [`estimate_tokens`] for every message
    /// currently held. Maintained incrementally in
    /// [`Self::add_message`] / [`Self::reset`] / [`Self::truncate_to_budget`]
    /// so [`Self::total_tokens`] is O(1) instead of O(N). The
    /// previous implementation re-summed every call, which is what
    /// the agent loop does on every turn — at 200 messages per
    /// context this is 200× `estimate_tokens` per turn for no
    /// observable reason.
    cached_total_tokens: usize,
}

impl ContextManager {
    /// Create a new context manager with a token budget.
    /// /// Defaults the output reserve to [`DEFAULT_OUTPUT_RESERVE_TOKENS`].
    /// Use [`Self::with_output_reserve`] to override per-instance.
    pub fn new(max_context_tokens: usize) -> Self {
        Self {
            messages: Vec::new(),
            max_context_tokens,
            output_reserve_tokens: DEFAULT_OUTPUT_RESERVE_TOKENS,
            pressure_ratio: CONTEXT_PRESSURE_TRIGGER_RATIO,
            cached_total_tokens: 0,
        }
    }

    /// Construct a `ContextManager` with no model info — defaults to the
    /// catalog fallback (`model_metadata::DEFAULT_FALLBACK_CONTEXT`, 128k)
    /// rather than a hard-coded magic value.
    /// /// **Prefer [`Self::for_model`] or [`Self::for_model_with_providers`] when
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
    /// /// Resolution order (via `hermix_intelligence::model_metadata::get_model_context_length`):
    /// 1. `known_models()` static catalog (covers Claude / OpenAI / Gemini / DeepSeek / etc.)
    /// 2. `DEFAULT_CONTEXT_LENGTHS` fuzzy fallback table
    /// 3. `DEFAULT_FALLBACK_CONTEXT` (128_000) for unknown / custom provider models
    /// /// The returned token budget mirrors what `get_model_context_length`
    /// returns. As of Day 3 the field is named `max_context_tokens`
    /// even though the underlying estimator is still the 4-chars-per-token
    /// heuristic — the rename was the honest move because all downstream
    /// thresholds now speak "tokens" too.
    /// /// `tracing::info!` logs the resolved value once so per-model wiring can
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
    /// /// Equivalent to `(max - output_reserve) * CONTEXT_PRESSURE_TRIGGER_RATIO`,
    /// floored to `1` to avoid divide-by-zero in callers. The reserve is
    /// subtracted before applying the ratio so the model always has the
    /// reserved headroom for its own response even after the warning fires.
    pub fn pressure_threshold_tokens(&self) -> usize {
        let effective = self.max_context_tokens.saturating_sub(self.output_reserve_tokens);
        ((effective as f64) * self.pressure_ratio).max(1.0) as usize
    }

    /// Override the per-instance output reserve (tokens). Useful for tests
    /// and for callers that want to leave more or less headroom than the
    /// default `4096` tokens.
    pub fn with_output_reserve(mut self, output_reserve_tokens: usize) -> Self {
        self.output_reserve_tokens = output_reserve_tokens;
        self
    }

    /// Override the per-instance pressure ratio (0.0–1.0).
    /// /// Controls when auto-compression and context-pressure warnings fire:
    /// `threshold = (max_context - output_reserve) * ratio`.
    /// Defaults to [`CONTEXT_PRESSURE_TRIGGER_RATIO`] (0.50). The runtime
    /// value is forwarded from `compaction.pressure_threshold_percent` in
    /// config.yaml via `AgentConfig.context_pressure_ratio`.
    pub fn with_pressure_ratio(mut self, ratio: f64) -> Self {
        self.pressure_ratio = ratio.clamp(0.01, 1.0);
        self
    }

    /// Construct a `ContextManager` with a custom token budget.
    /// /// **Deprecated**: the `_compressor: ()` parameter was always a
    /// placeholder — actual compression wiring lives in
    /// `crate::compression::ContextCompressor` and is plumbed via
    /// `auto_compress_if_over_threshold` in `compression_ops.rs`, not
    /// via `ContextManager`. Use [`Self::new`] (or [`Self::for_model`]
    /// for model-aware sizing) instead.
    #[deprecated(
        since = "0.4.0",
        note = "with_compressor(_, ()) had a dead `()` parameter. Use ContextManager::new or \
                ContextManager::for_model instead. Compression is plumbed via \
                auto_compress_if_over_threshold (compression_ops.rs)."
    )]
    #[allow(deprecated)]
    pub fn with_compressor(max_context_tokens: usize, _compressor: ()) -> Self {
        Self {
            messages: Vec::new(),
            max_context_tokens,
            output_reserve_tokens: DEFAULT_OUTPUT_RESERVE_TOKENS,
            pressure_ratio: CONTEXT_PRESSURE_TRIGGER_RATIO,
            cached_total_tokens: 0,
        }
    }

    /// Add a message to the conversation history.
    /// /// Maintains `cached_total_tokens` incrementally — the agent loop
    /// calls `total_tokens()` on every turn, so re-summing from scratch
    /// per call would be O(N) per turn for no good reason.
    pub fn add_message(&mut self, message: Message) {
        let tokens = message
            .content
            .as_deref()
            .map(estimate_tokens)
            .unwrap_or(0);
        self.cached_total_tokens = self.cached_total_tokens.saturating_add(tokens);
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
    /// /// Always preserves:
    /// - The system prompt (first message if it has role System)
    /// - The last user message
    /// - The most recent assistant messages
    /// /// Older messages are removed from the middle until the total fits.
    /// /// ### Performance (2026-07-05)
    /// /// The previous implementation called `Vec::remove(idx)` in a loop,
    /// which is O(n) per call (shifts every later element down). For a
    /// conversation with 200 messages that needed 50 removals to fit
    /// the budget, that was ~50 × 100 = 5 000 element shifts — not
    /// catastrophic but wasteful. The current implementation walks the
    /// history once, computes the prefix length to drop, then drains
    /// that prefix with a single O(n) memcpy. Same observable behaviour,
    /// ~50× fewer shifts in the worst case.
    /// /// `cached_total_tokens` is updated incrementally (we subtract each
    /// evicted message's token estimate) so [`Self::total_tokens`] stays
    /// O(1) after the truncation completes.
    pub fn truncate_to_budget(&mut self, budget: &BudgetConfig) {
        let max_chars = budget.max_aggregate_chars.min(self.max_context_tokens);

        if self.cached_total_tokens <= max_chars {
            return;
        }

        // Identify system messages (to preserve).
        let system_end = self
            .messages
            .iter()
            .take_while(|m| m.role == MessageRole::System)
            .count();

        // Walk from the tail backwards, accumulating token counts until
        // the kept tail + system messages would fit the budget. The
        // cut point is then `system_end + (n - kept_tail_start)`.
        let n = self.messages.len();
        let mut tail_cost: usize = 0;
        let mut first_kept_from_tail = n;
        for i in (system_end..n).rev() {
            let msg_cost = self.messages[i]
                .content
                .as_deref()
                .map(estimate_tokens)
                .unwrap_or(0);
            if tail_cost + msg_cost > max_chars {
                // Even one more message blows the budget; stop here.
                break;
            }
            tail_cost += msg_cost;
            first_kept_from_tail = i;
            // Always keep at least the last user/assistant exchange so
            // the model sees the most recent turn context.
            if first_kept_from_tail + 1 <= system_end + 2 {
                break;
            }
        }
        let cutoff = first_kept_from_tail;
        if cutoff <= system_end {
            // Nothing left to drop — keep the system messages and the
            // last 2 turns as required.
            return;
        }

        // Subtract evicted tokens from the cached running total.
        let evicted_tokens: usize = self.messages[system_end..cutoff]
            .iter()
            .map(|m| m.content.as_deref().map(estimate_tokens).unwrap_or(0))
            .sum();
        self.cached_total_tokens = self.cached_total_tokens.saturating_sub(evicted_tokens);

        // O(n) single-shift drain (vs the previous O(n²) loop of
        // `Vec::remove(idx)` calls).
        let dropped_count = cutoff - system_end;
        self.messages.drain(system_end..cutoff);

        // Insert a marker so the model and user know context was dropped
        if dropped_count > 0 {
            let marker = Message::system(format!(
                "[CONTEXT TRUNCATED] {} earlier messages were dropped to fit the context budget. \
                 No summary was generated — use `/new` to start a fresh session if you need to \
                 revisit earlier context.",
                dropped_count
            ));
            self.messages.insert(system_end, marker);
        }
    }

    /// Compress the conversation history — now a no-op.
    /// /// All compression is handled by `compression::ContextCompressor` in
    /// the agent loop's `auto_compress_if_over_threshold`. This method
    /// is retained only for call-site compatibility.
    /// /// **Deprecated**: do not call. Production code should rely on
    /// `auto_compress_if_over_threshold` (the LLM-compressor path) or
    /// `truncate_to_budget` (the cheap budget-enforcement path).
    /// Tests that previously called `compress()` should use
    /// `truncate_to_budget` instead.
    #[deprecated(
        since = "0.4.0",
        note = "ContextManager::compress() is a no-op. Use \
                compression::ContextCompressor via auto_compress_if_over_threshold, \
                or ContextManager::truncate_to_budget for budget enforcement."
    )]
    pub fn compress(&mut self) {
        // No-op: advanced compression is handled externally.
    }

    /// Reset the conversation history, clearing all messages.
    pub fn reset(&mut self) {
        self.messages.clear();
        self.cached_total_tokens = 0;
    }

    /// Calculate the total token count of all message content.
    /// /// Uses the 4-chars-per-token heuristic in [`estimate_tokens`] — for
    /// CJK / emoji-heavy corpora this under-counts, which is the safe
    /// direction (we never trigger compression too early).
    /// /// **Performance**: O(1) since 2026-07-05. The agent loop calls
    /// this on every turn (pressure check), so the previous O(N)
    /// re-summation was a hot-path tax. The cache is maintained by
    /// `add_message` / `reset` / `truncate_to_budget`; tests that
    /// mutate `messages` directly via `get_messages_mut()` should
    /// call [`Self::recompute_total_tokens`] afterwards to keep the
    /// cache honest.
    pub fn total_tokens(&self) -> usize {
        self.cached_total_tokens
    }

    /// Recompute `cached_total_tokens` from the current message list.
    /// /// Call this after directly mutating `messages` via
    /// `get_messages_mut()` (e.g. in tests, or in the agent loop's
    /// post-compression path). Production code paths that go through
    /// `add_message` / `reset` / `truncate_to_budget` keep the cache
    /// honest automatically.
    pub fn recompute_total_tokens(&mut self) -> usize {
        let total: usize = self
            .messages
            .iter()
            .map(|m| m.content.as_deref().map(estimate_tokens).unwrap_or(0))
            .sum();
        self.cached_total_tokens = total;
        total
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
/// /// Identity statement is the first thing the model reads — it has to be
/// unambiguous about *who* the assistant is, in a language the user
/// speaks. The behavioural rules below the identity statement are the
/// "how I work" — short and direct, no fluff, no vague exhortations.
/// SOUL.md (when present) overrides both halves.
pub const DEFAULT_AGENT_IDENTITY: &str = "\
Hello!我是 Hermix。\n\
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
## 自主进化\n\
我支持在对话中主动沉淀 skill / 脚本 / 习惯(这属于我的程序性记忆,而非越权)。具体沉淀规则见系统提示中的「Self-Evolution」章节;每次沉淀后我会明确告诉你我做了什么、写在了哪个文件。\n\
\n\
Introduction in English (use when the user writes in English):\n\
Hi! I'm Hermix.\n\
About my identity, the only fact you need to know: my name is Hermix.\n\
\n\
## Self-evolution\n\
I can proactively persist skills / scripts / habits as my procedural memory — not overreach. The concrete rules live in the \"Self-Evolution\" section of the system prompt; after each persistence I tell you what I wrote and where.";

pub const BUILTIN_PERSONALITY_CODER: &str = "\
你是「工程师」角色。\n\
\n\
- 先想清楚再动手:改动越小越可控,优先拆成可验证的小步。\n\
- 正确性与可验证性高于一切:每次改动后立刻跑验证(编译 / 测试 / 类型检查 / 实跑),不凭记忆判断行为。\n\
- 沿用项目既有的风格与约定,不擅自引入新依赖;确需引入时,先说明成本与理由。\n\
- 只解释非显而易见的取舍,不啰嗦;面对多种合理方案时,给出推荐并说明权衡。\n\
- 不确定时先用工具或跑代码确认,而不是猜测。";

pub const BUILTIN_PERSONALITY_WRITER: &str = "\
你是「写作者」角色。\n\
\n\
- 先搭骨架再填内容:用清晰的结构、有力的主题句组织文字。\n\
- 删掉一切不必要的重复与废话,优先保证可读性与流畅度。\n\
- 修订时保住原意,只提升表达;不擅自改变作者的立场与结论。\n\
- 该用例子就用例子,不堆砌术语;面向读者,而非炫技。";

pub const BUILTIN_PERSONALITY_ANALYST: &str = "\
你是「分析师」角色。\n\
\n\
- 结论要有证据:显式给出推理链,标注每一步的前提与不确定性。\n\
- 把复杂问题拆成「假设 → 观察 → 结论」逐层推进,不跳步。\n\
- 数据缺失时,先说清楚不知道什么,再给出性价比最高的下一步验证。\n\
- 不下无据的结论;概率性判断要标注置信度与适用边界。";

/// Discipline guidance for self-evolution. Injected as a guidance
/// block so the model sees the same operational rules in every
/// session — independent of which personality / SOUL.md the user
/// has selected.
/// /// Pre-requisite: the `skill_manage` tool must be present in the
/// toolset. Otherwise this block degrades to a no-op (the builder
/// skips empty parts). See `agent_loop::build_system_prompt` for
/// the conditional wire-up.
pub const SELF_EVOLUTION_GUIDANCE: &str = "\
## Self-Evolution Capabilities\n\
\n\
You have procedural-memory actions available via the `skill_manage` tool. \
This is your persistent self-improvement channel — not an admin task.\n\
\n\
### Actions\n\
- `auto_create` / `auto_extract_from_conversation` — crystallize a workflow into a skill.\n\
- `create` / `update` — manual skill management.\n\
- `self_improve` — append a '## Improvement Feedback' section to an existing skill.\n\
- `install_builtins` — seed the standard set when the workspace is empty.\n\
\n\
### How to write a good skill\n\
1. **Slug**: lowercase, hyphen-separated, ASCII only. `discover_skills` walks \
`$hermix_home/skills/<slug>/SKILL.md`.\n\
2. **Frontmatter** (YAML between `---` markers):\n\
   ```yaml\n\
   ---\n\
   name: my-skill\n\
   description: One-line purpose.\n\
   commands:\n\
     - name: my-skill\n\
       description: What /my-skill does\n\
       template: \"{args}\"\n\
   ---\n\
   ```\n\
3. **Body**: imperative steps. Reference tools by `tool_name`. Keep under 2000 words.\n\
4. **Verify**: after `create` / `update`, call `skills_list` to confirm visibility.\n\
\n\
### Boundaries\n\
- **DO** write into `$hermix_home/skills/`.\n\
- **DO NOT** touch SOUL.md, config.yaml, .env, auth.json, approval_allowlist.json, \
or anything outside `$hermix_home/`.\n\
- **DO NOT** persist one-off tasks or things that change session-to-session.";

/// Load the SOUL.md personality file from `<HERMIX_HOME>/SOUL.md`.
/// /// Returns `None` if the file doesn't exist or can't be read.
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
/// /// Resolution order:
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
/// /// Returns concatenated content of all `.md` and `.txt` files found.
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
/// /// This mirrors Python's MemoryStore snapshot semantics at session start:
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

/// A single conditional system-prompt *direction* — a text block injected
/// only when the active route matches its trigger.
/// /// Replaces the giant `if user_message.is_empty() { ... } else { ... }`
/// cascade that used to live inline in `build_system_prompt`. Each direction
/// is small (~50–200 chars), individually tagged, and trivially auditable
/// in debug output / frontend panels.
/// /// Inspired by BaiLongma's `directions[]` array (src/index.js:860), but
/// typed and routed by an enum instead of free-form string keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirRoute {
    /// First turn of a session (no prior user_message) — full block.
    FirstTurn,
    /// Any subsequent turn — short reminder.
    Subsequent,
    /// Caller opted in via `autonomous_mode = true`.
    Autonomous,
    /// Active tool list contains a `browser_*` tool.
    HasBrowser,
    /// Active tool list contains `memory`.
    HasMemory,
    /// Active tool list contains `session_search`.
    HasSessionSearch,
    /// Active tool list contains `skill_manage` AND user message mentions
    /// a skill-related keyword.
    HasSkillKeyword,
    /// Active tool list contains `todo` — task-orchestration guidance.
    HasTodo,
    /// Active tool list contains `task_create` — orchestration-engine
    /// task tracker guidance (Phase 4 / P4-2).
    HasTaskTracker,
    /// Active tool list contains `compact_tool_chain` AND user message
    /// mentions batch-related keyword.
    HasBatchKeyword,
    /// Active tool list is non-empty AND model needs tool enforcement.
    ToolEnforce,
    /// Model name contains "gemini" / "gemma" — Google family extras.
    GoogleFamily,
    /// Model name contains "gpt" / "codex" — OpenAI family extras.
    OpenAiFamily,
    /// Model name contains "minimax" / "m2" / "hermes" — fine-tune extras.
    MiniMaxFamily,
    /// Conversation has 5+ tool calls — multi-step task focus guidance.
    MultiStepTask,
    /// This turn actually invoked 1+ tool calls — only then should
    /// "summarize what you did" or "next step" guidance fire. Trivial
    /// conversational turns (no tool use) get the default guidance
    /// only, so the model is not pressured into inventing filler
    /// summaries for turns where there is nothing to summarize.
    HadAnyToolUse,
    /// Always-on (USER_STYLE_GUIDANCE, tool failure rule).
    Always,
    /// Active workspace's `AGENTS.md` exposes a project-conventions
    /// section. **Non-generic, deployment-specific tuning** — injected
    /// only when `collect_directions` is given conventions text, so the
    /// universal core stays free of any single user's preferences.
    /// `matches` always returns true because the direction is only
    /// queued when conventions exist.
    HasProjectConventions,
    /// Domain-mode directive — injects the work-domain-specific
    /// guidance block (Code / Research / Write / Operate / Chat).
    /// `matches` returns true when `RouteContext.domain_mode` equals
    /// the variant's domain.
    DomainMode(DomainMode),
}

#[derive(Debug, Clone)]
pub struct SystemDir {
    pub tag: &'static str,
    pub route: DirRoute,
    pub body: String,
}

impl SystemDir {
    pub fn new(tag: &'static str, route: DirRoute, body: impl Into<String>) -> Self {
        Self { tag, route, body: body.into() }
    }
}

/// The slice of decision-relevant state that [`DirRoute::matches`] reads.
/// Kept as a plain borrowed view — caller fills it from the agent config
/// + tool list without owning any data.
#[derive(Debug, Clone)]
pub struct RouteContext<'a> {
    pub user_message: &'a str,
    pub tool_names: &'a std::collections::HashSet<&'a str>,
    pub model_name: &'a str,
    pub autonomous_mode: bool,
    pub tool_call_count: usize,
    /// L1 Domain Mode detection result for this turn.
    /// `None` means domain detection was not run (legacy path).
    pub domain_mode: Option<DomainMode>,
}

impl<'a> Default for RouteContext<'a> {
    fn default() -> Self {
        Self {
            user_message: "",
            tool_names: &EMPTY_TOOL_NAMES,
            model_name: "",
            autonomous_mode: false,
            tool_call_count: 0,
            domain_mode: None,
        }
    }
}

/// Empty placeholder used only by `RouteContext::default()`. Refers
/// itself as a `HashSet<&'static str>` so that test code can build a
/// default route without writing out a fresh HashSet.
static EMPTY_TOOL_NAMES: std::sync::LazyLock<std::collections::HashSet<&'static str>> =
    std::sync::LazyLock::new(std::collections::HashSet::new);

impl DirRoute {
    /// Single source of truth for "does this direction fire this turn?".
    /// Returns `(fired, skip_reason)` so the audit log can show *why* a
    /// direction was skipped — useful when debugging "why didn't the
    /// browser guidance show up?".
    pub fn matches(&self, ctx: &RouteContext<'_>) -> (bool, Option<String>) {
        let um = ctx.user_message.to_lowercase();
        let has = |name: &str| ctx.tool_names.contains(name);
        let is_g = |s: &str| ctx.model_name.to_lowercase().contains(s);
        match self {
            DirRoute::FirstTurn => {
                if ctx.user_message.is_empty() { (true, None) }
                else { (false, Some("not first turn".into())) }
            }
            DirRoute::Subsequent => {
                if !ctx.user_message.is_empty() { (true, None) }
                else { (false, Some("is first turn".into())) }
            }
            DirRoute::Autonomous => {
                if ctx.autonomous_mode { (true, None) }
                else { (false, Some("caller did not opt in".into())) }
            }
            DirRoute::HasBrowser => {
                if ctx.tool_names.iter().any(|n| n.starts_with("browser_")) { (true, None) }
                else { (false, Some("no browser_* tool in scope".into())) }
            }
            DirRoute::HasMemory => {
                if has("memory") { (true, None) }
                else { (false, Some("memory tool not active".into())) }
            }
            DirRoute::HasSessionSearch => {
                if has("session_search") { (true, None) }
                else { (false, Some("session_search not active".into())) }
            }
            DirRoute::HasSkillKeyword => {
                if !has("skill_manage") {
                    (false, Some("skill_manage not active".into()))
                } else if ctx.user_message.is_empty()
                    || um.contains("skill") || um.contains("技能") || um.contains("技巧")
                    || um.contains("记忆") || um.contains("记录") || um.contains("记住")
                    || um.contains("保存") || um.contains("学习")
                { (true, None) }
                else { (false, Some("user message has no skill keyword".into())) }
            }
            DirRoute::HasBatchKeyword => {
                if !has("compact_tool_chain") {
                    (false, Some("compact_tool_chain not active".into()))
                } else if ctx.user_message.is_empty()
                    || um.contains("批量") || um.contains("多个") || um.contains("全部")
                    || um.contains("所有") || um.contains("逐一") || um.contains("依次")
                    || um.contains("batch") || um.contains("all")
                { (true, None) }
                else { (false, Some("user message has no batch keyword".into())) }
            }
            DirRoute::ToolEnforce => {
                let fired = !ctx.tool_names.is_empty();
                if fired { (true, None) }
                else { (false, Some("tool list empty".into())) }
            }
            DirRoute::GoogleFamily => {
                if is_g("gemini") || is_g("gemma") { (true, None) }
                else { (false, Some("not a Google-family model".into())) }
            }
            DirRoute::OpenAiFamily => {
                if is_g("gpt") || is_g("codex") { (true, None) }
                else { (false, Some("not an OpenAI-family model".into())) }
            }
            DirRoute::MiniMaxFamily => {
                if is_g("minimax") || is_g("m2") { (true, None) }
                else { (false, Some("not a MiniMax-family fine-tune".into())) }
            }
            DirRoute::HasTodo => {
                if has("todo") { (true, None) }
                else { (false, Some("todo tool not active".into())) }
            }
            DirRoute::HasTaskTracker => {
                if has("task_create") { (true, None) }
                else { (false, Some("task_create tool not active".into())) }
            }
            DirRoute::MultiStepTask => {
                if ctx.tool_call_count >= 5 { (true, None) }
                else { (false, Some(format!("tool_call_count {} < 5", ctx.tool_call_count))) }
            }
            DirRoute::HadAnyToolUse => {
                if ctx.tool_call_count >= 1 { (true, None) }
                else { (false, Some(format!("no tool calls this turn (count={})", ctx.tool_call_count))) }
            }
            DirRoute::Always => (true, None),
            DirRoute::HasProjectConventions => (true, None),
            DirRoute::DomainMode(dm) => {
                match ctx.domain_mode {
                    Some(active) if active == *dm => (true, None),
                    Some(active) => (false, Some(format!("domain is {:?}, not {:?}", active, dm))),
                    None => (false, Some("domain mode not detected".into())),
                }
            }
        }
    }
}

/// Builder for assembling the full system prompt from multiple layers.
/// /// Layers (in order):
/// 1. SOUL.md personality (or default identity)
/// 2. User/gateway system prompt (if provided)
/// 3. Memory context block (from MemoryManager)
/// 4. Tool descriptions / guidance
/// 5. Skill prompts (if preloaded)
/// 6. Context files from `.hermes/context/`
/// 7. Current date/time
/// 8. Working directory info
/// /// In addition to the static `parts` vector, the builder carries a
/// separate `directions` vector that holds typed `(tag, route, body)`
/// records. Each direction is evaluated against the current `RouteContext`
/// at `assemble()` time and survives as an audit record so the frontend
/// can show "which direction fired / was skipped and why".
pub struct SystemPromptBuilder {
    parts: Vec<String>,
    /// Tagged directions that gate conditionally on `DirRoute`.
    directions: Vec<SystemDir>,
    /// Last audit (rendered directions + skip reasons). Cleared on mutation.
    direction_audit: Vec<DirAuditEntry>,
    /// Cached assembled prompt.
    cached: Option<String>,
}

/// One row of the direction audit log. Emitted to gateway hooks as
/// `system_prompt_directions` so the debug panel can show what fired
/// per turn.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DirAuditEntry {
    pub tag: String,
    pub route: String,
    pub fired: bool,
    pub skip_reason: Option<String>,
    pub body_chars: usize,
}

impl SystemPromptBuilder {
    /// Create a new empty builder.
    pub fn new() -> Self {
        Self {
            parts: Vec::new(),
            directions: Vec::new(),
            direction_audit: Vec::new(),
            cached: None,
        }
    }

    /// Queue a typed, route-gated direction.
    /// /// The direction is *not* appended to the prompt here — that happens
    /// in [`Self::assemble_directions`] so the builder can also produce an
    /// audit record of which directions matched the active route and
    /// which were skipped (and why).
    pub fn with_direction(mut self, dir: SystemDir) -> Self {
        self.directions.push(dir);
        self.cached = None;
        self
    }

    /// Append several directions at once.
    pub fn with_directions(mut self, dirs: impl IntoIterator<Item = SystemDir>) -> Self {
        for d in dirs {
            self.directions.push(d);
        }
        self.cached = None;
        self
    }

    /// Borrow the latest audit log. Cleared by mutation; populated by
    /// [`Self::assemble_directions`].
    pub fn direction_audit(&self) -> &[DirAuditEntry] {
        &self.direction_audit
    }

    /// Drain the audit log so the caller (gateway/emit_hook_event) can
    /// forward it to the frontend without taking a long-lived borrow.
    pub fn take_direction_audit(&mut self) -> Vec<DirAuditEntry> {
        std::mem::take(&mut self.direction_audit)
    }

    /// Mark every queued direction as fired / skipped based on `route_ctx`,
    /// append the fired bodies to `parts` in queued order, and return the
    /// audit log.
    /// /// The route context is the *exact* same context `build_system_prompt`
    /// would have looked at inline — so each direction's "fired?" check
    /// is the single source of truth.
    pub fn assemble_directions(&mut self, route_ctx: &RouteContext) -> &[DirAuditEntry] {
        let mut audit = Vec::with_capacity(self.directions.len());
        for d in self.directions.drain(..) {
            let (fired, skip_reason) = d.route.matches(route_ctx);
            let entry = DirAuditEntry {
                tag: d.tag.to_string(),
                route: format!("{:?}", d.route),
                fired,
                skip_reason: skip_reason.clone(),
                body_chars: d.body.chars().count(),
            };
            if fired {
                self.parts.push(d.body);
            }
            audit.push(entry);
        }
        if !audit.is_empty() {
            self.cached = None;
        }
        self.direction_audit = audit;
        &self.direction_audit
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
    /// /// This is a thin adapter so the builder does not have to know
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
    /// /// ## Why this exists
    /// /// Tools are normally delivered to the model via the OpenAI
    /// `tools` / Anthropic `tools` API field, which is invisible to a
    /// model that fails to wire up the native protocol. For
    /// MiniMax / Hermes-style fine-tunes and any text-only fallback,
    /// the *only* way the model learns what it can call is by reading
    /// the system prompt text. Without this block the model answers
    /// "I don't have terminal / execute_code / list_files tools",
    /// even though they are wired up.
    /// /// ## Format
    /// /// ```text
    /// # Tool availability reminder (must read)
    /// You have the following tools **right now**, registered and
    /// ready to call. If a task calls for one of them, USE IT
    /// directly — do not refuse on the grounds that you "don't
    /// have" it. ...
    /// /// # Available tools
    /// - **terminal** — Run a shell command. (required: command)
    /// - **read_file** — Read the contents of a file...
    /// - **list_files** — Alias for search_files with target=files...
    /// - ... (continues to the cap; remainder referred to `list_tools`)
    /// ```
    /// /// ## Implementation notes
    /// /// - The block is sorted: tools that are commonly *missed* by
    /// MiniMax / Hermes-style fine-tunes (`terminal`,
    /// `code_execution` / `execute_code`, `list_files`,
    /// `edit_file`, `read_file`, `write_file`, `patch`,
    /// `search_files`, `read_pdf`) are pinned to the top, so a
    /// model that only skims the first few lines still sees the
    /// tools it tends to claim it lacks.
    /// - The cap is `MAX_CHARS` (15k). Older revisions used 4k,
    /// which truncated `terminal` and `execute_code` on a
    /// registry of 60+ tools — the exact regression that
    /// produced the "I don't have terminal" bug.
    /// - A short reminder at the top is a stronger signal than
    /// the model seeing the tool name buried at line 800 of
    /// the system prompt.
    pub fn with_tools_section(mut self, tool_schemas: &[ToolSchema]) -> Self {
        if tool_schemas.is_empty() {
            return self;
        }
        // Compact reference: model already has full schemas via the native
        // `tools` API parameter. This text block is a light fallback +
        // pointer to the on-disk reference file for detailed usage.
        let names: Vec<&str> = tool_schemas.iter().map(|s| s.name.as_str()).collect();
        let mut block = String::from("## Tools\n");
        block.push_str("Available: ");
        block.push_str(&names.join(", "));
        block.push_str("\n\n");
        block.push_str("For detailed parameters, `read_file` \"$HERMIX_HOME/tools_reference.md\" ");
        block.push_str("and read the section for the tool you need.\n");
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

// ---------------------------------------------------------------------------
// SectionGate — M2: protocol-completeness single choke point
// ---------------------------------------------------------------------------
// Every system prompt that goes to the model is *expected* to contain
// a small, named set of top-level sections. Before M2 these expectations
// were scattered across `build_system_prompt` (six if/else cascades, four
// model-family extras, and a hand-rolled `format!("### {}\n{}", ...)`
// string join). When any one of those branches silently no-op'd — a model
// that opted out of the tools guidance, a route that returned early, a
// compile-time `cfg` flag that stripped a whole block — the model would
// start hallucinating or refusing. The bug was always a "section was
// missing and nobody noticed".
// `SectionGate` is the single place that knows which sections *must*
// appear in a healthy prompt, and the single function that audits the
// produced text and patches in the missing sections before the model
// ever sees it. The patch text is identical to the default body the
// original code path would have produced, so the gate is measure-equivalent
// to the previous inline behavior — except it can never silently skip a
// required section.

/// Top-level sections that every system prompt is expected to contain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Section {
    /// SOUL.md / default identity (always required).
    #[serde(rename = "identity")]
    Identity,
    /// Tool list (only required when `tool_names` is non-empty).
    #[serde(rename = "tool_list")]
    ToolList,
    /// Tool-enforcement guidance (only required when `tool_names` is non-empty).
    #[serde(rename = "tool_enforce")]
    ToolEnforce,
    /// Working-directory / context-files block.
    #[serde(rename = "working_dir")]
    WorkingDir,
    /// Time / date stamp.
    #[serde(rename = "timestamp")]
    Timestamp,
}

impl Section {
    /// Stable string marker used by the gate to detect presence in the
    /// rendered prompt. Matches the heading that the corresponding
    /// builder code emits for that section.
    pub fn marker(self) -> &'static str {
        match self {
            Section::Identity => "### Identity",
            Section::ToolList => "### Active tools",
            Section::ToolEnforce => "### Tool enforcement",
            Section::WorkingDir => "### Working directory",
            Section::Timestamp => "### Current time",
        }
    }

    /// Short human-readable label, intended for front-end trace UI.
    /// (C3 marker protocol): distinct from `marker()` —
    /// `marker()` is the *detection* key in the prompt text, `label()`
    /// is the *display* string for the trace panel / logs.
    pub fn label(self) -> &'static str {
        match self {
            Section::Identity => "Identity",
            Section::ToolList => "Active tools",
            Section::ToolEnforce => "Tool enforcement",
            Section::WorkingDir => "Working directory",
            Section::Timestamp => "Current time",
        }
    }

    /// The default patch text the gate will splice in when the section is
    /// missing. Intentionally short — this is the *floor* of what a
    /// healthy prompt must contain, not the full guidance block.
    pub fn default_body(self) -> &'static str {
        match self {
            Section::Identity =>
                "### Identity\nYou are Hermix, a direct, audit-first assistant.",
            Section::ToolList =>
                "### Active tools\nNo tools are active in this turn; answer with prose.",
            Section::ToolEnforce =>
                "### Tool enforcement\nTools are active. Prefer tool calls over prose when action is required.",
            Section::WorkingDir =>
                "### Working directory\nNo workspace context. Use the conversation for any file references.",
            Section::Timestamp =>
                "### Current time\nUse the most recent timestamp from this turn as your time reference.",
        }
    }
}

/// A single gap detected by the gate: which `Section` is missing and
/// how the gate chose to handle it (`Patched` = auto-spliced the default
/// body; `Pending` = skipped because the caller's `RouteContext` did not
/// require it; e.g. `ToolList` with empty `tool_names`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SectionGap {
    pub section: Section,
    /// Whether the gate auto-spliced in the default body. /// (C3 marker protocol): serde-renamed to `applied` for the wire
    /// so the front-end sees a stable shape.
    #[serde(rename = "applied")]
    pub patched: bool,
    pub reason: String,
}

/// Result of a single `SectionGate::audit_and_patch` call.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SectionGateReport {
    /// Sections that were missing and have been auto-spliced in.
    pub patched: Vec<SectionGap>,
    /// Sections the gate knows about but considers optional under the
    /// current `RouteContext` (e.g. `ToolList` when no tools are active).
    /// Surfaced in the audit log so a human can see "yes, the gate saw
    /// the section, but it wasn't required for this turn".
    pub pending: Vec<SectionGap>,
}

impl SectionGateReport {
    /// True iff the gate had to splice at least one missing section in.
    /// Frontend can highlight this row in the trace panel.
    pub fn had_patches(&self) -> bool {
        !self.patched.is_empty()
    }

    /// Total number of sections the gate evaluated, regardless of outcome.
    /// Note: this counts *missing* sections (patched + pending). Sections
    /// that were present in the prompt are not counted here — the
    /// "audited" count would be `5 - missing` for the five top-level
    /// sections. Use `had_patches()` to detect "the gate had to do work".
    pub fn total_evaluated(&self) -> usize {
        self.patched.len() + self.pending.len()
    }
}

/// Single source of truth for "is this section required for this turn?".
/// The gate uses this to decide whether a missing marker should trigger
/// a patch or just be recorded as pending.
pub fn section_required(section: Section, route_ctx: &RouteContext<'_>) -> bool {
    match section {
        Section::Identity => true,
        Section::Timestamp => true,
        Section::WorkingDir => true,
        Section::ToolList => !route_ctx.tool_names.is_empty(),
        Section::ToolEnforce => !route_ctx.tool_names.is_empty(),
    }
}

impl Section {
    /// Evaluate the prompt text against the gate. Returns a report listing
    /// any sections that were missing under the current `RouteContext`.
    /// This function does NOT mutate the prompt; the caller decides what
    /// to do with the report (auto-patch, abort, or just log).
    pub fn audit_prompt(prompt: &str, route_ctx: &RouteContext<'_>) -> SectionGateReport {
        let mut report = SectionGateReport::default();
        for &section in &[
            Section::Identity,
            Section::ToolList,
            Section::ToolEnforce,
            Section::WorkingDir,
            Section::Timestamp,
        ] {
            if prompt.contains(section.marker()) {
                continue;
            }
            let gap = SectionGap {
                section,
                patched: false,
                reason: if section_required(section, route_ctx) {
                    format!("missing required marker {:?}", section.marker())
                } else {
                    format!(
                        "optional under current RouteContext (no active tools / no workspace)"
                    )
                },
            };
            if section_required(section, route_ctx) {
                report.patched.push(gap);
            } else {
                report.pending.push(gap);
            }
        }
        report
    }

    /// Audit + auto-patch. The patched prompt is returned by value so
    /// the caller cannot accidentally drop the splice. If the gate saw
    /// no gaps, the original string is returned unchanged (no allocation).
    pub fn audit_and_patch(
        prompt: &str,
        route_ctx: &RouteContext<'_>,
    ) -> (String, SectionGateReport) {
        let report = Self::audit_prompt(prompt, route_ctx);
        if report.patched.is_empty() {
            return (prompt.to_string(), report);
        }
        let mut out = String::with_capacity(prompt.len() + 256);
        out.push_str(prompt);
        out.push_str("\n\n");
        for gap in &report.patched {
            out.push_str(gap.section.default_body());
            out.push_str("\n\n");
        }
        (out, report)
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

    /// M1: route-gated directions fire only when the corresponding
    /// `DirRoute` matches the active `RouteContext`, and the audit log
    /// carries both the fired and skipped entries. This is the single
    /// source of truth for "why did this prompt block get injected?"
    #[test]
    fn directions_first_turn_fires_format_full_not_short() {
        let mut tool_names = std::collections::HashSet::new();
        tool_names.insert("browser_navigate");
let ctx = RouteContext {
                    user_message: "",
                    tool_names: &tool_names,
                    model_name: "gpt-4o",
                    autonomous_mode: false,
                    tool_call_count: 0,
                    domain_mode: None,
                };
        let mut b = SystemPromptBuilder::new();
        b = b.with_directions(vec![
            SystemDir::new("format.full", DirRoute::FirstTurn, "FULL"),
            SystemDir::new("format.short", DirRoute::Subsequent, "SHORT"),
            SystemDir::new("autonomous.block", DirRoute::Autonomous, "AUTO"),
            SystemDir::new("browser.discipline", DirRoute::HasBrowser, "BROWSER"),
        ]);
        b.assemble_directions(&ctx);
        let audit = b.direction_audit();
        assert_eq!(audit.len(), 4);
        let by_tag = |t: &str| audit.iter().find(|a| a.tag == t).unwrap();
        assert!(by_tag("format.full").fired);
        assert!(!by_tag("format.short").fired, "first-turn should not fire subsequent");
        assert!(!by_tag("autonomous.block").fired, "no opt-in → skip autonomous");
        assert!(by_tag("browser.discipline").fired, "browser_* tool present");
        let prompt = b.build();
        assert!(prompt.contains("FULL"));
        assert!(!prompt.contains("SHORT"));
        assert!(prompt.contains("BROWSER"));
    }

    /// M1: an Autonomous direction that does not match still appears
    /// in the audit log with `fired = false` and a human-readable
    /// `skip_reason` — the whole point of measure-only mode.
    #[test]
    fn directions_autonomous_skip_records_reason() {
        let tool_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let ctx = RouteContext {
            user_message: "hello",
            tool_names: &tool_names,
            model_name: "gemini-1.5-flash",
            autonomous_mode: false,
            tool_call_count: 0,
            domain_mode: None,
        };
        let mut b = SystemPromptBuilder::new();
        b = b.with_directions(vec![
            SystemDir::new("autonomous.block", DirRoute::Autonomous, "AUTO"),
            SystemDir::new("google.extras", DirRoute::GoogleFamily, "GOOGLE"),
        ]);
        b.assemble_directions(&ctx);
        let audit = b.direction_audit();
        let auto = audit.iter().find(|a| a.tag == "autonomous.block").unwrap();
        assert!(!auto.fired);
        assert_eq!(auto.skip_reason.as_deref(), Some("caller did not opt in"));
        let google = audit.iter().find(|a| a.tag == "google.extras").unwrap();
        assert!(google.fired);
    }

    /// M1: take_direction_audit drains the log so the caller (gateway
    /// / emit_hook_event) can forward it without taking a long-lived
    /// borrow. After drain, the next direction push must still work.
    #[test]
    fn directions_take_audit_drains_log() {
        let _tool_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let ctx = RouteContext::default();
        let mut b = SystemPromptBuilder::new();
        b = b.with_directions(vec![
            SystemDir::new("plan.full", DirRoute::FirstTurn, "PLAN"),
        ]);
        b.assemble_directions(&ctx);
        let drained: Vec<_> = b.take_direction_audit().into_iter().collect();
        assert_eq!(drained.len(), 1);
        assert!(b.direction_audit().is_empty(), "after drain the log is empty");
        // Push a *second* round of directions and verify they still
        // assemble cleanly (audit, fired, skip_reason all reset).
        b = b.with_directions(vec![
            SystemDir::new("plan.full", DirRoute::FirstTurn, "PLAN2"),
            SystemDir::new("autonomous.block", DirRoute::Autonomous, "AUTO"),
        ]);
        b.assemble_directions(&ctx);
        let audit = b.direction_audit();
        assert_eq!(audit.len(), 2);
    }

    // ---------------------------------------------------------------
    // M2: SectionGate — protocol-completeness single choke point
    // ---------------------------------------------------------------

    /// M2: a healthy prompt with all five section markers present must
    /// produce a report with zero patches. The gate is silent on
    /// healthy input — its sole job is to catch *missing* sections.
    #[test]
    fn section_gate_healthy_prompt_yields_zero_patches() {
        let tool_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
let ctx = RouteContext {
                    user_message: "hello",
                    tool_names: &tool_names,
                    model_name: "gpt-4o",
                    autonomous_mode: false,
                    tool_call_count: 0,
                    domain_mode: None,
                };
        let prompt = format!(
            "{}\n\n{}\n\n{}\n\n{}\n\n{}",
            Section::Identity.default_body(),
            Section::Timestamp.default_body(),
            Section::WorkingDir.default_body(),
            Section::ToolList.default_body(),
            Section::ToolEnforce.default_body(),
        );
        let report = Section::audit_prompt(&prompt, &ctx);
        assert!(!report.had_patches(), "healthy prompt must not trigger any patches, got: {:?}", report);
        // With empty tool_names: required = {Identity, Timestamp, WorkingDir}
        // (all 3 found in prompt → no patches), optional = {ToolList, ToolEnforce}
        // (both found in prompt → no pending entries). So the report is
        // empty — 0 patched, 0 pending.
        assert_eq!(report.total_evaluated(), 0,
            "healthy prompt with all markers present must yield 0 missing sections, got: {:?}", report);
    }

    /// M2: a prompt with the identity marker missing must trigger
    /// a patch for `Section::Identity` (and any other required markers
    /// also missing). The patched prompt must contain the marker and
    /// must preserve the original prose verbatim.
    #[test]
    fn section_gate_missing_identity_gets_patched() {
        let tool_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
let ctx = RouteContext {
                    user_message: "hello",
                    tool_names: &tool_names,
                    model_name: "gpt-4o",
                    autonomous_mode: false,
                    tool_call_count: 0,
                    domain_mode: None,
                };
        let prompt = "no identity block at all, just some prose.";
        let (patched, report) = Section::audit_and_patch(prompt, &ctx);
        // With empty tool_names, required = {Identity, Timestamp, WorkingDir}.
        // None of those markers are in the prompt, so all 3 must patch.
        assert!(report.had_patches());
        let patched_set: std::collections::HashSet<_> =
            report.patched.iter().map(|g| g.section).collect();
        assert!(patched_set.contains(&Section::Identity));
        assert!(patched_set.contains(&Section::Timestamp));
        assert!(patched_set.contains(&Section::WorkingDir));
        // Tool sections go to pending (not required with empty tools)
        assert!(!patched_set.contains(&Section::ToolList));
        assert!(!patched_set.contains(&Section::ToolEnforce));
        // The patched prompt must contain the identity marker AND
        // preserve the original prose.
        assert!(patched.contains(Section::Identity.marker()),
            "patched prompt must contain the identity marker, got: {patched:?}");
        assert!(patched.contains(prompt),
            "patched prompt must keep the original prose, got: {patched:?}");
    }

    /// M2: when `tool_names` is empty, `ToolList` and `ToolEnforce` are
    /// *optional* under the current `RouteContext` — a missing marker
    /// must be recorded as `pending`, not `patched`. The audit log is
    /// the only signal; we do not splice default text for optional
    /// sections, because that would inject noise ("Tools are active")
    /// into a prompt the model will read as "no tools are active".
    #[test]
    fn section_gate_tool_sections_pending_when_no_tools() {
        let tool_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let ctx = RouteContext {
            user_message: "hi",
            tool_names: &tool_names,
            model_name: "gpt-4o",
            autonomous_mode: false,
            tool_call_count: 0,
            domain_mode: None,
        };
        // Provide identity + timestamp + working_dir (the three always-
        // required sections); tool sections deliberately missing.
        let prompt = format!(
            "{}\n\n{}\n\n{}",
            Section::Identity.default_body(),
            Section::Timestamp.default_body(),
            Section::WorkingDir.default_body(),
        );
        let report = Section::audit_prompt(&prompt, &ctx);
        // Required markers are present → no patches
        assert!(!report.had_patches(),
            "all required markers present must yield zero patches, got: {:?}", report);
        // The two tool sections should show up as pending
        let pending_sections: Vec<_> = report.pending.iter().map(|g| g.section).collect();
        assert!(pending_sections.contains(&Section::ToolList));
        assert!(pending_sections.contains(&Section::ToolEnforce));
    }

    /// M2: with active tools, missing tool sections are *required* and
    /// must be auto-patched. This is the path that was silently
    /// broken before M2: when a `cfg` flag or a tool-routing early
    /// return dropped the `<tools>` block, the model lost awareness of
    /// its own capability. The gate catches that case and patches the
    /// default text in.
    #[test]
    fn section_gate_tool_sections_patched_when_tools_active() {
        let mut tool_names = std::collections::HashSet::new();
        tool_names.insert("terminal");
        tool_names.insert("read_file");
        let ctx = RouteContext {
            user_message: "list the project files",
            tool_names: &tool_names,
            model_name: "gpt-4o",
            autonomous_mode: false,
            tool_call_count: 0,
            domain_mode: None,
        };
        // Only identity present, no tool / no working-dir / no timestamp.
        let prompt = Section::Identity.default_body().to_string();
        let (patched, report) = Section::audit_and_patch(&prompt, &ctx);
        assert!(report.had_patches());
        let patched_set: std::collections::HashSet<_> =
            report.patched.iter().map(|g| g.section).collect();
        assert!(patched_set.contains(&Section::ToolList));
        assert!(patched_set.contains(&Section::ToolEnforce));
        assert!(patched_set.contains(&Section::WorkingDir));
        assert!(patched_set.contains(&Section::Timestamp));
        assert!(patched.contains(Section::ToolList.marker()));
        assert!(patched.contains(Section::ToolEnforce.marker()));
    }

    /// M3 (Round 22 — 2026-07-07): `DirRoute::HadAnyToolUse` only fires
    /// when the turn actually invoked 1+ tool calls. Trivial conversational
    /// turns (count == 0) must NOT receive this guidance, otherwise the
    /// model invents filler summaries and the "(等待中) ×N" pattern
    /// observed on M2.7 / Hermes fine-tunes.
    #[test]
    fn had_any_tool_use_fires_only_when_count_positive() {
        let tool_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
        // Trivial turn → must NOT fire.
        let trivial = RouteContext {
            user_message: "你好",
            tool_names: &tool_names,
            model_name: "minimax:MiniMax-M2.7",
            autonomous_mode: false,
            tool_call_count: 0,
            domain_mode: None,
        };
        assert_eq!(
            DirRoute::HadAnyToolUse.matches(&trivial),
            (false, Some("no tool calls this turn (count=0)".to_string()))
        );
        // After a single tool call → must fire.
        let with_one_tool = RouteContext {
            tool_call_count: 1,
            ..trivial
        };
        assert_eq!(DirRoute::HadAnyToolUse.matches(&with_one_tool), (true, None));
        // Multi-tool turn → still fires (no upper bound).
        let many_tools = RouteContext {
            tool_call_count: 10,
            ..trivial
        };
        assert_eq!(DirRoute::HadAnyToolUse.matches(&many_tools), (true, None));
    }

    /// C3 marker protocol (2026-06-29): each `Section` has a stable
    /// human-readable `label()` distinct from its detection
    /// `marker()`. Locked in here so a refactor that conflates the
    /// two (e.g. renders `marker()` in the trace panel) is caught by
    /// CI.
    #[test]
    fn section_label_distinct_from_marker() {
        let cases: &[(Section, &str, &str)] = &[
            (Section::Identity, "### Identity", "Identity"),
            (Section::ToolList, "### Active tools", "Active tools"),
            (Section::ToolEnforce, "### Tool enforcement", "Tool enforcement"),
            (Section::WorkingDir, "### Working directory", "Working directory"),
            (Section::Timestamp, "### Current time", "Current time"),
        ];
        for (section, expected_marker, expected_label) in cases {
            assert_eq!(section.marker(), *expected_marker, "marker for {:?}", section);
            assert_eq!(section.label(), *expected_label, "label for {:?}", section);
            // Defence in depth: label must not contain the
            // `### ` heading prefix — the trace panel renders
            // the label as a chip, not a heading.
            assert!(!section.label().starts_with("### "), "label leaks heading prefix: {:?}", section.label());
        }
    }

    /// C3 wire format (2026-06-29): the front-end trace panel
    /// deserialises `SectionGap.section` to a `Section` enum and
    /// uses `Section::label()` to render. This test pins the JSON
    /// shape so any rename on either side breaks loudly rather than
    /// silently rendering `"unknown"`.
    #[test]
    fn section_gate_report_serializes_to_expected_wire_shape() {
        let gap = SectionGap {
            section: Section::WorkingDir,
            patched: true,
            reason: "missing required marker".to_string(),
        };
        let json = serde_json::to_value(&gap).expect("SectionGap serialises");
        assert_eq!(json["section"], "working_dir", "Section serde tag");
        assert_eq!(json["applied"], true, "patched renamed to applied on wire");
        assert_eq!(json["reason"], "missing required marker");

        let report = SectionGateReport {
            patched: vec![gap.clone()],
            pending: vec![],
        };
        let report_json = serde_json::to_value(&report).expect("report serialises");
        assert_eq!(report_json["patched"].as_array().unwrap().len(), 1);
        assert_eq!(report_json["pending"].as_array().unwrap().len(), 0);
    }


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
        // Pin the no-op contract while the deprecated method still exists.
        #[allow(deprecated)]
        cm.compress();
        assert_eq!(cm.len(), len_before);
    }

    #[test]
    fn test_with_compressor_noop() {
        #[allow(deprecated)]
        let mut cm = ContextManager::with_compressor(10_000, ());
        cm.add_message(Message::system("System prompt"));
        cm.add_message(Message::user("Hi"));
        cm.add_message(Message::assistant("Hello!"));

        let len_before = cm.len();
        #[allow(deprecated)]
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
        // (128_000 - 4096) * 0.90 = 111_513
        assert_eq!(cm.pressure_threshold_tokens(), 111_513);
    }

    #[test]
    fn test_pressure_threshold_tokens_default() {
        let cm = ContextManager::new(200_000);
        // (200_000 - 4096 reserve) * 0.90 = 195_904 * 0.90 = 176_313
        assert_eq!(cm.pressure_threshold_tokens(), 176_313);
    }

    #[test]
    fn test_pressure_threshold_tokens_per_model() {
        let gpt = ContextManager::for_model("gpt-4o");     // 128k tokens
        // (128_000 - 4096) * 0.90 = 123_904 * 0.90 = 111_513
        assert_eq!(gpt.pressure_threshold_tokens(), 111_513);

        let gemini = ContextManager::for_model("gemini-1.5-pro"); // 1_048_576 tokens
        // (1_048_576 - 4096) * 0.90 = 1_044_480 * 0.90 = 940_032
        assert_eq!(gemini.pressure_threshold_tokens(), 940_032);
    }

    #[test]
    fn test_pressure_threshold_tokens_with_zero_reserve_matches_legacy() {
        // When reserve = 0, the threshold degenerates to `max * 0.90`.
        let cm = ContextManager::new(200_000).with_output_reserve(0);
        assert_eq!(cm.pressure_threshold_tokens(), 180_000);
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

    /// `add_message` must maintain `cached_total_tokens`
    /// incrementally. Without this, `total_tokens()` would re-sum
    /// every call, and the agent loop's per-turn pressure check
    /// (which calls `total_tokens()` on every turn) would be O(N).
    #[test]
    fn test_total_tokens_incremental_after_add() {
        let mut cm = ContextManager::new(10_000);
        assert_eq!(cm.total_tokens(), 0, "empty context starts at 0");
        cm.add_message(Message::user("a".repeat(40).as_str())); // 40 chars → 10 tokens
        assert_eq!(cm.total_tokens(), 10);
        cm.add_message(Message::assistant("b".repeat(20).as_str())); // 20 chars → 5 tokens
        assert_eq!(cm.total_tokens(), 15, "cache should track both messages");
    }

    /// `truncate_to_budget` must keep the cached token
    /// total in sync with the actual message list. A naive O(n²)
    /// implementation that did `Vec::remove(idx)` in a loop could
    /// leave the cache stale.
    #[test]
    fn test_total_tokens_in_sync_after_truncate() {
        let mut cm = ContextManager::new(10_000);
        for i in 0..50 {
            cm.add_message(Message::user(format!("user message {}", i)));
        }
        // Each "user message N" is ~16 chars → 4 tokens; total ≈ 200.
        let before = cm.total_tokens();
        assert!(before > 100, "should have built up tokens");

        let budget = BudgetConfig {
            max_result_size_chars: 100_000,
            max_aggregate_chars: 60, // tight enough to force truncation
        };
        cm.truncate_to_budget(&budget);

        // Cache must agree with the actual sum after the drain.
        let recomputed = cm.recompute_total_tokens();
        assert_eq!(cm.total_tokens(), recomputed, "cache must stay in sync");
        // Budget is enforced: significant truncation occurred.
        // A small marker message is added, so total may slightly exceed budget.
        assert!(cm.total_tokens() < before, "truncation reduced tokens");
    }

    /// `reset` clears the cached token counter along
    /// with the messages vector.
    #[test]
    fn test_total_tokens_resets_on_clear() {
        let mut cm = ContextManager::new(10_000);
        for _ in 0..20 {
            cm.add_message(Message::user("hello"));
        }
        assert!(cm.total_tokens() > 0);
        cm.reset();
        assert_eq!(cm.total_tokens(), 0);
    }

    /// `truncate_to_budget` is now O(n) per call (single
    /// drain) — a 200-message conversation with aggressive budget
    /// still completes in microseconds. Functional verification only
    /// here; the algorithmic improvement is the absence of the
    /// `Vec::remove(idx)` loop.
    #[test]
    fn test_truncate_handles_large_conversation() {
        let mut cm = ContextManager::new(10_000_000);
        for i in 0..500 {
            cm.add_message(Message::assistant(format!("turn {i} content here")));
        }
        cm.add_message(Message::user("latest question"));
        let budget = BudgetConfig {
            max_result_size_chars: 100_000,
            max_aggregate_chars: 500, // very tight
        };
        cm.truncate_to_budget(&budget);
        // System messages + last user message must survive.
        let last = cm.get_messages().last().unwrap();
        assert_eq!(last.role, MessageRole::User);
        assert!(cm.total_tokens() <= 500);
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
        // Builtin personas were localized to Chinese.
        // Verify the persona loaded and contains the Chinese role label.
        assert!(coder.contains("工程师"));
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
        // Localized to Chinese — verify key content markers.
        assert!(p.contains("工程师"));
        assert!(p.contains("正确性"));
    }

    #[test]
    fn test_persona_snapshot_writer() {
        let p = resolve_personality("writer", None).unwrap();
        assert!(p.contains("写作者"));
        assert!(p.contains("可读性"));
    }

    #[test]
    fn test_persona_snapshot_analyst() {
        let p = resolve_personality("analyst", None).unwrap();
        assert!(p.contains("分析师"));
        assert!(p.contains("证据"));
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

        // Localized to Chinese.
        assert!(!base.contains("工程师"));
        assert!(coder_prompt.contains("工程师"));
        assert!(coder_prompt.contains("正确性"));
    }
}
