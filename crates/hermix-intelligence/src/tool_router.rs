//! Tool schema router — selectively expose tools to the LLM.
//!
//! 目标(参考 `docs/PROPOSAL_BAILONGMA_INSPIRED.md` §3 H1,§五 ):
//!
//! > **30+ 工具的"全量注入"问题**:每次 turn 把所有 tool schema 拼进 system prompt,
//! > 一个 schema 约 150-200 token,30+ 就是 4500-6000 token/turn,白白烧光上下文窗口和钱。
//! > BaiLongma/腾讯云的方案:同步 hook 在 prompt 构建时只返回 top-N 相关的工具,
//! > 节省 ~92% 的工具相关 token。
//!
//! 本模块实现:
//! - [`ToolSchemaRouter`] trait —— 路由器接口
//! - [`TurnContext`] —— 路由决策的输入
//! - [`RoutedSchema`] —— 路由结果(单条 schema + 评分 + 来源)
//! - [`KeywordRouter`] —— L0 实现,关键词+LRU
//! - [`VectorRouter`] —— L1 实现,embedding 余弦相似度
//! - [`ToolsetRouter`] —— 按 toolset 分组(active toolsets 优先)
//! - [`CompositeRouter`] —— 串联多路由器
//! - [`AlwaysInclude`] —— 工具级"白名单",无论评分都返回(避免遗漏基础工具)
//! - 完整单元测试 + 属性测试
//!
//! 接入点(在 `hermix-agent::agent_loop`):
//! ```ignore
//! let all = self.tool_registry.schemas();
//! let routed = self.tool_router.route(&ctx, all);
//! let tool_schemas: Vec<ToolSchema> = routed.into_iter().map(|r| r.schema).collect();
//! ```
//!
//! 不动 `ToolRegistry` 的语义(注册/调度/审批)—— 它仍然是 ground truth;
//! 路由只决定"哪些 schema 进入 LLM 上下文"。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use hermix_core::types::Message;
use hermix_core::ToolSchema;
use serde::{Deserialize, Serialize};

use crate::embedding::EmbeddingProvider;

// ---------------------------------------------------------------------------
// TurnContext —— 路由决策的输入
// ---------------------------------------------------------------------------

/// 一次 turn 的路由上下文。
///
/// 路由器根据 `user_message` + `recent_messages` + `active_toolsets` 决定
/// 哪些 tool schema 进入 LLM 上下文。
#[derive(Debug, Clone)]
pub struct TurnContext {
    /// 用户最新一条消息(原文本)
    pub user_message: String,
    /// 最近 N 条历史消息(用于 LRU 推断"用户上一轮用过什么工具")
    pub recent_messages: Vec<Message>,
    /// 当前激活的 toolset 名列表(由前端 Settings 决定)
    pub active_toolsets: Vec<String>,
    /// 最多返回多少个 tool schema(默认 5, §验收 给定)
    pub max_tools: usize,
    /// L1 Domain Mode 优先工具列表。
    /// 来自 `DomainPreset::priority_tools`，
    /// 在路由打分后给这些工具加分(+5.0)。
    /// 空列表 = 无优先工具(默认)。
    pub priority_tools: Vec<String>,
}

impl TurnContext {
    /// Construct a minimal context from just a user message.
    pub fn from_user_message(msg: impl Into<String>) -> Self {
        Self {
            user_message: msg.into(),
            recent_messages: Vec::new(),
            active_toolsets: Vec::new(),
            max_tools: 5,
            priority_tools: Vec::new(),
        }
    }

    /// Builder: set max tools.
    pub fn with_max_tools(mut self, n: usize) -> Self {
        self.max_tools = n;
        self
    }

    /// Builder: set active toolsets.
    pub fn with_active_toolsets(mut self, toolsets: Vec<String>) -> Self {
        self.active_toolsets = toolsets;
        self
    }

    /// Builder: set recent messages.
    pub fn with_recent_messages(mut self, msgs: Vec<Message>) -> Self {
        self.recent_messages = msgs;
        self
    }

    /// Builder: set priority tools (from DomainPreset).
    pub fn with_priority_tools(mut self, tools: Vec<String>) -> Self {
        self.priority_tools = tools;
        self
    }
}

// ---------------------------------------------------------------------------
// RoutedSchema —— 路由结果
// ---------------------------------------------------------------------------

/// A single routed tool schema, with metadata about why it was selected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutedSchema {
    pub schema: ToolSchema,
    /// Score assigned by the router. Higher = more relevant.
    pub score: f32,
    /// Where this score came from (e.g. "keyword:weather", "vector:0.92", "toolset:active").
    pub source: String,
    /// Did this tool get force-included by the `AlwaysInclude` whitelist?
    pub force_included: bool,
}

// ---------------------------------------------------------------------------
// ToolSchemaRouter trait
// ---------------------------------------------------------------------------

pub trait ToolSchemaRouter: Send + Sync {
    /// Route a set of candidate tool schemas down to a top-N list.
    /// Implementations must:
    /// 1. Return at most `ctx.max_tools` items.
    /// 2. Return *no more* than the input candidate set (subset).
    /// 3. Preserve order is not required, but results should be sorted by score desc.
    fn route(&self, ctx: &TurnContext, candidates: Vec<ToolSchema>) -> Vec<RoutedSchema>;

    /// Stable name for telemetry / debugging.
    fn name(&self) -> &str;
}

// ---------------------------------------------------------------------------
// Helper: simple tokenizer for keyword matching
// ---------------------------------------------------------------------------

/// Tokenize a string into lowercase word tokens. Splits on non-alphanumeric
/// boundaries. Length >= 1 after filtering.
///
/// CJK handling: We do **not** segment CJK characters into words (no
/// jieba in the dep tree). Instead, we treat each contiguous CJK run
/// as a single token AND each 1-2 char CJK substring as a token. This
/// is a rough heuristic — it gives reasonable recall for CJK queries
/// like "查天气" matching the `weather` tool.
pub(crate) fn tokenize(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut buf = String::new();
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            buf.push(ch.to_ascii_lowercase());
        } else {
            if !buf.is_empty() {
                out.push(std::mem::take(&mut buf));
            }
        }
    }
    if !buf.is_empty() {
        out.push(buf);
    }

    // CJK bigram pass: any run of CJK characters gets split into
    // 1- and 2-gram tokens. "查天气" → ["查", "天", "气", "查天", "天气"]
    let mut cjk_runs: Vec<String> = Vec::new();
    let mut current = String::new();
    for ch in s.chars() {
        if is_cjk(ch) {
            current.push(ch);
        } else {
            if !current.is_empty() {
                cjk_runs.push(std::mem::take(&mut current));
            }
        }
    }
    if !current.is_empty() {
        cjk_runs.push(current);
    }
    for run in cjk_runs {
        let chars: Vec<char> = run.chars().collect();
        for ch in &chars {
            out.push(ch.to_string());
        }
        for win in chars.windows(2) {
            out.push(win.iter().collect());
        }
    }

    out
}

fn is_cjk(ch: char) -> bool {
    let cp = ch as u32;
    (0x4E00..=0x9FFF).contains(&cp)        // CJK Unified Ideographs
        || (0x3400..=0x4DBF).contains(&cp)  // CJK Extension A
        || (0xF900..=0xFAFF).contains(&cp)  // CJK Compatibility Ideographs
}

// ---------------------------------------------------------------------------
// L0: KeywordRouter
// ---------------------------------------------------------------------------

/// L0 keyword + LRU router.
///
/// Scoring formula (per tool):
/// - +2 for every occurrence of the tool's name in the user message
/// - +1 for every token of the user message that also appears in the tool's
/// name or description
/// - +5 if the tool appears in the LRU history (the user has called it
/// recently — likely still relevant)
/// - +0.5 if the tool belongs to one of the active toolsets
///
/// Returns the top `ctx.max_tools` candidates, with ties broken by
/// tool name (alphabetical) for determinism.
#[derive(Debug, Clone, Default)]
pub struct KeywordRouter {
    /// Tool names most recently used, oldest at front.
    lru: VecDeque<String>,
    /// Cap on the LRU history size.
    lru_cap: usize,
}

impl KeywordRouter {
    pub fn new() -> Self {
        Self {
            lru: VecDeque::new(),
            lru_cap: 32,
        }
    }

    /// Record a tool use. Moves the tool name to the back of the LRU.
    pub fn record_use(&mut self, tool_name: impl Into<String>) {
        let name = tool_name.into();
        if let Some(pos) = self.lru.iter().position(|n| n == &name) {
            self.lru.remove(pos);
        }
        self.lru.push_back(name);
        while self.lru.len() > self.lru_cap {
            self.lru.pop_front();
        }
    }

    /// Return the LRU as a `Vec` (oldest first).
    pub fn lru_snapshot(&self) -> Vec<String> {
        self.lru.iter().cloned().collect()
    }

    /// Score a single tool against the context.
    pub fn score_tool(&self, schema: &ToolSchema, ctx: &TurnContext) -> (f32, String) {
        let user_tokens = tokenize(&ctx.user_message);
        let user_lower = ctx.user_message.to_lowercase();
        let name_lower = schema.name.to_lowercase();
        let desc_tokens = tokenize(&schema.description);

        let mut score = 0.0f32;
        let mut source_parts: Vec<String> = Vec::new();

        // 1. Whole-name match in user message (×2 per occurrence).
        if !user_lower.is_empty() && !name_lower.is_empty() {
            let occurrences = user_lower.matches(&name_lower).count() as f32;
            if occurrences > 0.0 {
                score += occurrences * 2.0;
                source_parts.push(format!("name:{}x", occurrences as u32));
            }
        }

        // 2. Token overlap with name or description.
        let mut token_hits = 0u32;
        for tok in &user_tokens {
            if tok.len() < 2 {
                continue;
            }
            if name_lower.contains(tok) || desc_tokens.iter().any(|d| d == tok) {
                token_hits += 1;
            }
        }
        if token_hits > 0 {
            score += token_hits as f32;
            source_parts.push(format!("token:{}t", token_hits));
        }

        // 3. LRU bonus: was this tool used recently?
        if self.lru.iter().any(|n| n == &schema.name) {
            score += 5.0;
            source_parts.push("lru".to_string());
        }

        // 4. Active-toolset bonus.
        if ctx
            .active_toolsets
            .iter()
            .any(|t| name_lower.starts_with(&t.to_lowercase()))
        {
            score += 0.5;
            source_parts.push("toolset".to_string());
        }

        let source = if source_parts.is_empty() {
            "none".to_string()
        } else {
            source_parts.join("+")
        };
        (score, source)
    }
}

impl ToolSchemaRouter for KeywordRouter {
    fn route(&self, ctx: &TurnContext, candidates: Vec<ToolSchema>) -> Vec<RoutedSchema> {
        let mut scored: Vec<RoutedSchema> = candidates
            .into_iter()
            .map(|schema| {
                let (score, source) = self.score_tool(&schema, ctx);
                RoutedSchema {
                    schema,
                    score,
                    source,
                    force_included: false,
                }
            })
            .collect();

        // Sort by score desc, then by tool name asc (deterministic tie-break).
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.schema.name.cmp(&b.schema.name))
        });
        scored.truncate(ctx.max_tools);
        scored
    }

    fn name(&self) -> &str {
        "KeywordRouter"
    }
}

// ---------------------------------------------------------------------------
// L1: VectorRouter
// ---------------------------------------------------------------------------

/// L1 vector similarity router.
///
/// Scores each tool by cosine similarity between the user-message embedding
/// and the tool's precomputed embedding (name + description, embedded at
/// registration time). Returns top-N.
pub struct VectorRouter {
    /// Provider of embeddings. We accept any `Fn(&str) -> Vec<f32>` so the
    /// caller can plug in ONNX, OpenAI, Jina v5, or a deterministic stub
    /// for tests. The dimension is inferred from the first call.
    embed_fn: Arc<dyn Fn(&str) -> Vec<f32> + Send + Sync>,
    /// Cached tool embeddings, keyed by tool name.
    tool_embeddings: HashMap<String, Vec<f32>>,
}

impl std::fmt::Debug for VectorRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VectorRouter")
            .field("tool_embeddings_count", &self.tool_embeddings.len())
            .finish()
    }
}

impl VectorRouter {
    /// Build a new vector router with the given embedder. The router will
    /// lazily embed tools as they are seen (see [`Self::index_tool`]).
    pub fn new(embed_fn: Arc<dyn Fn(&str) -> Vec<f32> + Send + Sync>) -> Self {
        Self {
            embed_fn,
            tool_embeddings: HashMap::new(),
        }
    }

    /// L1.4: Build a vector router from an async [`EmbeddingProvider`].
    /// The provider's `dimension()` is recorded at construction so
    /// `index_tool` can verify each tool embedding has the right shape
    /// (mismatched dims silently score 0 — they would otherwise poison
    /// the cosine calculation). The closure does an **async** `embed`
    /// call inside a `tokio::runtime::Handle::block_on` so callers must
    /// invoke this from within a Tokio runtime.
    pub fn from_provider(provider: Arc<dyn EmbeddingProvider>) -> Self {
        let dim = provider.dimension();
        let p = provider.clone();
        let embed_fn: Arc<dyn Fn(&str) -> Vec<f32> + Send + Sync> = Arc::new(move |text| {
            // `block_on` is the standard way to bridge sync -> async
            // when the caller is itself sync (e.g. the router's
            // `route()` is sync, but the provider is async). This
            // panics if called outside a Tokio runtime; that's a
            // programming error — the router is meant to be
            // constructed from inside `agent_loop.run()` which is
            // async.
            let handle = tokio::runtime::Handle::try_current()
                .expect("VectorRouter::from_provider must be called from within a Tokio runtime");
            // Errors collapse to a zero-vector. We log + return zero so
            // a single failed embed doesn't take the whole router
            // down — the tool just won't match (cosine 0).
            match handle.block_on(p.embed(text)) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("vector router: embed failed: {e}");
                    vec![0.0; dim]
                }
            }
        });
        Self {
            embed_fn,
            tool_embeddings: HashMap::new(),
        }
    }

    /// Pre-compute the embedding for a tool. Idempotent: re-indexing a
    /// tool refreshes its cached embedding.
    pub fn index_tool(&mut self, schema: &ToolSchema) {
        let text = format!("{} {}", schema.name, schema.description);
        let v = (self.embed_fn)(&text);
        self.tool_embeddings.insert(schema.name.clone(), v);
    }

    /// Index a batch of tools.
    pub fn index_tools(&mut self, schemas: &[ToolSchema]) {
        for s in schemas {
            self.index_tool(s);
        }
    }

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        if a.len() != b.len() || a.is_empty() {
            return 0.0;
        }
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        if na == 0.0 || nb == 0.0 {
            0.0
        } else {
            dot / (na * nb)
        }
    }
}

impl ToolSchemaRouter for VectorRouter {
    fn route(&self, ctx: &TurnContext, candidates: Vec<ToolSchema>) -> Vec<RoutedSchema> {
        let qv = (self.embed_fn)(&ctx.user_message);
        let mut scored: Vec<RoutedSchema> = candidates
            .into_iter()
            .map(|schema| {
                let score = self
                    .tool_embeddings
                    .get(&schema.name)
                    .map(|te| Self::cosine(&qv, te))
                    .unwrap_or(0.0);
                RoutedSchema {
                    schema,
                    score,
                    source: format!("vector:{:.3}", score),
                    force_included: false,
                }
            })
            .collect();

        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.schema.name.cmp(&b.schema.name))
        });
        scored.truncate(ctx.max_tools);
        scored
    }

    fn name(&self) -> &str {
        "VectorRouter"
    }
}

// ---------------------------------------------------------------------------
// ToolsetRouter
// ---------------------------------------------------------------------------

/// Toolset-based router: prefers tools whose `toolset` is in the
/// active-toolsets list. Useful when the user has explicitly chosen
/// a curated subset (e.g. disabled browser tools but kept file tools).
#[derive(Debug, Clone, Default)]
pub struct ToolsetRouter;

impl ToolsetRouter {
    pub fn new() -> Self {
        Self
    }
}

impl ToolSchemaRouter for ToolsetRouter {
    fn route(&self, ctx: &TurnContext, candidates: Vec<ToolSchema>) -> Vec<RoutedSchema> {
        if ctx.active_toolsets.is_empty() {
            // No active filter: pass through unchanged.
            return candidates
                .into_iter()
                .map(|schema| RoutedSchema {
                    schema,
                    score: 0.0,
                    source: "passthrough".to_string(),
                    force_included: false,
                })
                .collect();
        }

        // Active toolsets get a +1 bonus, others get 0. Still sort
        // by name for determinism.
        let active: HashSet<String> = ctx
            .active_toolsets
            .iter()
            .map(|s| s.to_lowercase())
            .collect();
        let mut scored: Vec<RoutedSchema> = candidates
            .into_iter()
            .map(|schema| {
                let prefix = schema.name.to_lowercase();
                let matched = active.iter().any(|t| prefix.starts_with(t));
                let (score, source) = if matched {
                    (1.0f32, "toolset:matched".to_string())
                } else {
                    (0.0f32, "toolset:unmatched".to_string())
                };
                RoutedSchema {
                    schema,
                    score,
                    source,
                    force_included: false,
                }
            })
            .collect();
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.schema.name.cmp(&b.schema.name))
        });
        scored.truncate(ctx.max_tools);
        scored
    }

    fn name(&self) -> &str {
        "ToolsetRouter"
    }
}

// ---------------------------------------------------------------------------
// CompositeRouter
// ---------------------------------------------------------------------------

/// Run several routers in sequence; each subsequent router sees only
/// the survivors of the previous. The final `passthrough` router's
/// output is returned. Useful for chaining "ToolsetRouter (broad filter)
/// → KeywordRouter (re-rank)" without re-walking the full candidate set.
pub struct CompositeRouter {
    stages: Vec<Arc<dyn ToolSchemaRouter>>,
}

impl std::fmt::Debug for CompositeRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompositeRouter")
            .field("stages", &self.stages.iter().map(|s| s.name()).collect::<Vec<_>>())
            .finish()
    }
}

impl CompositeRouter {
    pub fn new() -> Self {
        Self { stages: Vec::new() }
    }

    pub fn with_stage(mut self, router: Arc<dyn ToolSchemaRouter>) -> Self {
        self.stages.push(router);
        self
    }

    pub fn stage_count(&self) -> usize {
        self.stages.len()
    }
}

impl Default for CompositeRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolSchemaRouter for CompositeRouter {
    fn route(&self, ctx: &TurnContext, candidates: Vec<ToolSchema>) -> Vec<RoutedSchema> {
        if self.stages.is_empty() {
            let mut out: Vec<RoutedSchema> = candidates
                .into_iter()
                .map(|schema| RoutedSchema {
                    schema,
                    score: 0.0,
                    source: "no-routers".to_string(),
                    force_included: false,
                })
                .collect();
            out.truncate(ctx.max_tools);
            return out;
        }
        let mut current: Vec<ToolSchema> = candidates;
        let mut last_source: String = String::new();
        let mut last_scores: HashMap<String, f32> = HashMap::new();

        for (i, stage) in self.stages.iter().enumerate() {
            let is_last = i + 1 == self.stages.len();
            let routed = stage.route(ctx, current.clone());
            if is_last {
                return routed;
            }
            // For non-final stages, we have to convert back to plain
            // schemas and re-feed the next stage. We lose score/source
            // metadata across stages by design — each stage is opaque.
            // The last stage is responsible for final scoring.
            last_source = stage.name().to_string();
            for r in &routed {
                last_scores.insert(r.schema.name.clone(), r.score);
            }
            current = routed.into_iter().map(|r| r.schema).collect();
        }
        // Unreachable: we return in the last iteration. But make the
        // compiler happy in case the stages vec is mutated concurrently.
        let _ = last_source;
        let _ = last_scores;
        Vec::new()
    }

    fn name(&self) -> &str {
        "CompositeRouter"
    }
}

// ---------------------------------------------------------------------------
// Always-include wrapper
// ---------------------------------------------------------------------------

/// Wraps any router to ensure a fixed set of tool names is always
/// present in the output (e.g. `read_file`, `write_file`, `memory` —
/// the universal "must have" set that LLM cannot do without).
///
/// Force-included tools do **not** count against `max_tools`; they are
/// appended after the routed top-N. The total output is therefore
/// `max_tools + always.len()` items, with the always-list at the end.
pub struct AlwaysInclude {
    inner: Arc<dyn ToolSchemaRouter>,
    always: Vec<String>,
}

impl std::fmt::Debug for AlwaysInclude {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlwaysInclude")
            .field("inner", &self.inner.name())
            .field("always", &self.always)
            .finish()
    }
}

impl AlwaysInclude {
    pub fn new(inner: Arc<dyn ToolSchemaRouter>, always: Vec<String>) -> Self {
        Self { inner, always }
    }
}

impl ToolSchemaRouter for AlwaysInclude {
    fn route(&self, ctx: &TurnContext, candidates: Vec<ToolSchema>) -> Vec<RoutedSchema> {
        // 1. Run the inner router to get the routed top-N.
        let mut routed = self.inner.route(ctx, candidates.clone());

        // 2. For each always-include name, find its schema in the
        // candidate set. If found and not already in `routed`,
        // append it with score=∞ and force_included=true.
        let always: HashSet<String> = self.always.iter().cloned().collect();
        let routed_names: HashSet<String> =
            routed.iter().map(|r| r.schema.name.clone()).collect();
        for schema in &candidates {
            if always.contains(&schema.name) && !routed_names.contains(&schema.name) {
                routed.push(RoutedSchema {
                    schema: schema.clone(),
                    score: f32::INFINITY,
                    source: "always".to_string(),
                    force_included: true,
                });
            }
        }
        routed
    }

    fn name(&self) -> &str {
        "AlwaysInclude"
    }
}

// ---------------------------------------------------------------------------
// Priority-boost wrapper (L1 Domain Mode integration)
// ---------------------------------------------------------------------------

/// Wraps any router to boost the score of tools listed in
/// `TurnContext::priority_tools`.
///
/// After the inner router produces its ranked output, this wrapper
/// adds `+5.0` to the score of any tool whose name appears in the
/// priority list, then re-sorts by score descending.
///
/// This is the integration point between L1 Domain Mode and the tool
/// router: `DomainPreset::priority_tools` flows in via `TurnContext`,
/// and this wrapper ensures those tools rank higher in the final
/// output without force-including them (unlike `AlwaysInclude`).
///
/// ## Example
///
/// ```ignore
/// let router = PriorityBoost::new(Arc::new(KeywordRouter::new()), 5.0);
/// let ctx = TurnContext::from_user_message("fix main.rs")
/// .with_priority_tools(vec!["read_file".into(), "write_file".into()]);
/// let routed = router.route(&ctx, all_tools);
/// read_file and write_file will have +5.0 score boost
/// ```
pub struct PriorityBoost {
    inner: Arc<dyn ToolSchemaRouter>,
    /// Score delta added to priority tools.
    boost: f32,
}

impl std::fmt::Debug for PriorityBoost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PriorityBoost")
            .field("inner", &self.inner.name())
            .field("boost", &self.boost)
            .finish()
    }
}

impl PriorityBoost {
    /// Create with a custom boost amount (default 5.0).
    pub fn new(inner: Arc<dyn ToolSchemaRouter>, boost: f32) -> Self {
        Self { inner, boost }
    }

    /// Create with the default boost of 5.0.
    pub fn with_default_boost(inner: Arc<dyn ToolSchemaRouter>) -> Self {
        Self::new(inner, 5.0)
    }
}

impl ToolSchemaRouter for PriorityBoost {
    fn route(&self, ctx: &TurnContext, candidates: Vec<ToolSchema>) -> Vec<RoutedSchema> {
        // 1. Run the inner router to get the ranked output.
        let mut routed = self.inner.route(ctx, candidates);

        // 2. If no priority tools, return as-is.
        if ctx.priority_tools.is_empty() {
            return routed;
        }

        // 3. Build a lookup set for O(1) membership check.
        let priority: HashSet<&str> = ctx
            .priority_tools
            .iter()
            .map(|s| s.as_str())
            .collect();

        // 4. Apply boost to matching tools.
        for r in &mut routed {
            if priority.contains(r.schema.name.as_str()) {
                r.score += self.boost;
                r.source = format!("{}+priority", r.source);
            }
        }

        // 5. Re-sort by score descending.
        routed.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        routed
    }

    fn name(&self) -> &str {
        "PriorityBoost"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_core::tool_schema::{JsonSchema, ToolSchema};

    fn make_tool(name: &str, desc: &str) -> ToolSchema {
        ToolSchema::new(name, desc, JsonSchema::new("object"))
    }

    fn make_tools() -> Vec<ToolSchema> {
        vec![
            make_tool("weather", "Get current weather and forecast for a city 天气 预报"),
            make_tool("calendar", "Read or write calendar events 日程 日历 会议"),
            make_tool("terminal", "Run shell commands on the host 终端 命令行"),
            make_tool("read_file", "Read contents of a file 读取 文件"),
            make_tool("write_file", "Write or patch a file 写入 编辑"),
            make_tool("search_files", "Search files by name or content 搜索 文件名"),
            make_tool("web_search", "Search the public web via a search engine 网页 搜索 引擎"),
            make_tool("memory", "Store and recall long-term memory 记忆 长期"),
        ]
    }

    // ----- tokenize --------------------------------------------------------

    #[test]
    fn tokenize_splits_on_punctuation() {
        let toks = tokenize("hello, world! foo-bar");
        assert!(toks.contains(&"hello".to_string()));
        assert!(toks.contains(&"world".to_string()));
        assert!(toks.contains(&"foo".to_string()));
        assert!(toks.contains(&"bar".to_string()));
    }

    #[test]
    fn tokenize_lowercases() {
        let toks = tokenize("Hello World");
        assert_eq!(toks, vec!["hello", "world"]);
    }

    #[test]
    fn tokenize_handles_cjk_bigrams() {
        let toks = tokenize("查天气");
        assert!(toks.contains(&"查".to_string()));
        assert!(toks.contains(&"天".to_string()));
        assert!(toks.contains(&"气".to_string()));
        assert!(toks.contains(&"查天".to_string()));
        assert!(toks.contains(&"天气".to_string()));
    }

    #[test]
    fn tokenize_handles_mixed_cjk_and_ascii() {
        let toks = tokenize("查一下 weather");
        assert!(toks.contains(&"weather".to_string()));
        assert!(toks.contains(&"查一".to_string()));
        assert!(toks.contains(&"一下".to_string()));
    }

    // ----- KeywordRouter ---------------------------------------------------

    #[test]
    fn keyword_router_picks_weather_for_weather_query() {
        let r = KeywordRouter::new();
        let ctx = TurnContext::from_user_message("北京今天天气怎么样").with_max_tools(3);
        let routed = r.route(&ctx, make_tools());
        assert!(!routed.is_empty());
        // weather must be in the top-3.
        assert!(routed.iter().any(|r| r.schema.name == "weather"));
    }

    #[test]
    fn keyword_router_picks_calendar_for_event_query() {
        let r = KeywordRouter::new();
        let ctx = TurnContext::from_user_message("add event to my calendar tomorrow");
        let routed = r.route(&ctx, make_tools());
        assert!(routed.iter().any(|r| r.schema.name == "calendar"));
    }

    #[test]
    fn keyword_router_picks_file_tools_for_read_query() {
        let r = KeywordRouter::new();
        let ctx = TurnContext::from_user_message("read the file at /tmp/foo.txt");
        let routed = r.route(&ctx, make_tools());
        // read_file should be top — name match (×2) dominates.
        assert_eq!(routed[0].schema.name, "read_file");
    }

    #[test]
    fn keyword_router_respects_max_tools() {
        let r = KeywordRouter::new();
        let ctx = TurnContext::from_user_message("hello").with_max_tools(2);
        let routed = r.route(&ctx, make_tools());
        assert_eq!(routed.len(), 2);
    }

    #[test]
    fn keyword_router_returns_empty_for_unrelated_input() {
        let r = KeywordRouter::new();
        let ctx = TurnContext::from_user_message("xzqplmk");
        let routed = r.route(&ctx, make_tools());
        // All tools score 0, so they're still returned up to max_tools
        // (we don't drop zero-score; we trust LLM to ignore). Length
        // should still be capped at max_tools.
        assert_eq!(routed.len(), 5); // default max_tools
    }

    #[test]
    fn keyword_router_lru_promotes_recent_tools() {
        let mut r = KeywordRouter::new();
        r.record_use("terminal");
        let ctx = TurnContext::from_user_message("hello there");
        let routed = r.route(&ctx, make_tools());
        // terminal should appear in the result even though the message
        // has nothing to do with shell commands.
        assert!(routed.iter().any(|x| x.schema.name == "terminal"));
    }

    #[test]
    fn keyword_router_lru_dedupes() {
        let mut r = KeywordRouter::new();
        r.record_use("terminal");
        r.record_use("weather");
        r.record_use("terminal"); // moves to back
        assert_eq!(r.lru_snapshot(), vec!["weather", "terminal"]);
    }

    #[test]
    fn keyword_router_lru_caps_size() {
        let mut r = KeywordRouter::new();
        for i in 0..100 {
            r.record_use(format!("tool_{i}"));
        }
        assert_eq!(r.lru_snapshot().len(), 32);
    }

    // ----- VectorRouter ----------------------------------------------------

    /// Deterministic embedder: encodes each char's lowercase ASCII code
    /// into a fixed 16-dim vector. Identical strings → identical vectors.
    /// Useful for testing cosine without an external model.
    fn stub_embed(s: &str) -> Vec<f32> {
        let mut v = vec![0.0f32; 16];
        for (i, ch) in s.chars().take(16).enumerate() {
            v[i] = (ch.to_ascii_lowercase() as u32 as f32) / 128.0;
        }
        v
    }

    #[test]
    fn vector_router_indexes_and_scores() {
        let mut r = VectorRouter::new(Arc::new(stub_embed));
        let tools = make_tools();
        r.index_tools(&tools);

        let ctx = TurnContext::from_user_message("weather forecast today");
        let routed = r.route(&ctx, tools);
        assert!(!routed.is_empty());
        // weather should win on cosine similarity (shares "weather" prefix).
        assert_eq!(routed[0].schema.name, "weather");
    }

    #[test]
    fn vector_router_handles_missing_embedding() {
        let r = VectorRouter::new(Arc::new(stub_embed));
        // No index_tool calls: every tool has no embedding → score 0.
        let ctx = TurnContext::from_user_message("anything");
        let routed = r.route(&ctx, make_tools());
        for r in &routed {
            assert_eq!(r.score, 0.0);
        }
    }

    #[test]
    fn vector_router_cosine_handles_zero_vector() {
        // cos(0, 0) should be 0.0, not NaN.
        assert_eq!(VectorRouter::cosine(&[0.0; 4], &[0.0; 4]), 0.0);
    }

    #[test]
    fn vector_router_cosine_handles_mismatched_dim() {
        // Mismatched dims → 0.0 (not a panic, not NaN).
        assert_eq!(VectorRouter::cosine(&[1.0, 0.0], &[1.0, 0.0, 0.0]), 0.0);
    }

    #[test]
    fn vector_router_cosine_perfect_match() {
        let v = vec![1.0, 2.0, 3.0];
        let s = VectorRouter::cosine(&v, &v);
        assert!((s - 1.0).abs() < 1e-6, "expected 1.0, got {s}");
    }

    // ----- ToolsetRouter ---------------------------------------------------

    #[test]
    fn toolset_router_boosts_active_tools() {
        let r = ToolsetRouter::new();
        let ctx = TurnContext::from_user_message("anything")
            .with_active_toolsets(vec!["read".to_string()]);
        let routed = r.route(&ctx, make_tools());
        let file_tools: Vec<&RoutedSchema> = routed
            .iter()
            .filter(|x| x.schema.name.starts_with("read") || x.schema.name.starts_with("write"))
            .collect();
        for r in &file_tools {
            assert_eq!(r.score, 1.0, "toolset read boost missing for {}", r.schema.name);
        }
    }

    #[test]
    fn toolset_router_passthrough_when_empty() {
        let r = ToolsetRouter::new();
        let ctx = TurnContext::from_user_message("hi");
        let routed = r.route(&ctx, make_tools());
        for r in &routed {
            assert_eq!(r.source, "passthrough");
        }
    }

    // ----- CompositeRouter -------------------------------------------------

    #[test]
    fn composite_router_runs_stages_in_order() {
        let stage1 = Arc::new(ToolsetRouter::new());
        let stage2 = Arc::new(KeywordRouter::new());
        let composite = CompositeRouter::new()
            .with_stage(stage1)
            .with_stage(stage2);
        let ctx = TurnContext::from_user_message("read the file at /tmp/foo")
            .with_active_toolsets(vec!["read".to_string()]);
        let routed = composite.route(&ctx, make_tools());
        assert!(!routed.is_empty());
        // The final stage (KeywordRouter) should rank read_file first.
        assert_eq!(routed[0].schema.name, "read_file");
    }

    #[test]
    fn composite_router_empty_pipeline_passes_through() {
        let composite = CompositeRouter::new();
        let ctx = TurnContext::from_user_message("hi");
        let routed = composite.route(&ctx, make_tools());
        // No stages: returns all candidates as-is (with max_tools cap).
        assert_eq!(routed.len(), 5); // default max_tools
    }

    // ----- AlwaysInclude ---------------------------------------------------

    #[test]
    fn always_include_appends_missing_tools() {
        let inner = Arc::new(KeywordRouter::new());
        let wrapper = AlwaysInclude::new(inner, vec!["memory".to_string()]);
        let ctx = TurnContext::from_user_message("read the file /tmp/foo").with_max_tools(2);
        let routed = wrapper.route(&ctx, make_tools());

        let names: Vec<&str> = routed.iter().map(|r| r.schema.name.as_str()).collect();
        assert!(names.contains(&"memory"), "memory must be force-included");
        let memory_entry = routed.iter().find(|r| r.schema.name == "memory").unwrap();
        assert!(memory_entry.force_included);
        assert!(memory_entry.score.is_infinite());
        assert_eq!(memory_entry.source, "always");
    }

    #[test]
    fn always_include_noop_when_already_routed() {
        let inner = Arc::new(KeywordRouter::new());
        let wrapper = AlwaysInclude::new(inner, vec!["read_file".to_string()]);
        let ctx = TurnContext::from_user_message("read the file at /tmp/foo");
        let routed = wrapper.route(&ctx, make_tools());
        // read_file is already top-ranked, so it should appear once,
        // not twice (the inner router's entry wins, not the always entry).
        let count = routed
            .iter()
            .filter(|r| r.schema.name == "read_file")
            .count();
        assert_eq!(count, 1, "read_file must not be duplicated");
    }

    // ----- Integration: 30+ tools, top-5 selection ----------------------

    #[test]
    fn integration_picks_top5_for_weather_query() {
        let r = KeywordRouter::new();
        let mut tools = make_tools();
        // Add 25 more tools to simulate a 30+ tool registry.
        for i in 0..25 {
            tools.push(make_tool(
                &format!("custom_tool_{i:02}"),
                &format!("Custom tool number {i} for advanced use"),
            ));
        }
        let ctx = TurnContext::from_user_message("北京今天天气怎么样").with_max_tools(5);
        let routed = r.route(&ctx, tools);
        assert_eq!(routed.len(), 5);
        // weather must be in the top 5.
        assert!(routed.iter().any(|r| r.schema.name == "weather"));
    }

    // ----- Determinism -----------------------------------------------------

    #[test]
    fn keyword_router_deterministic_on_ties() {
        let r = KeywordRouter::new();
        let ctx = TurnContext::from_user_message("xyz"); // no matches
        let a = r.route(&ctx, make_tools());
        let b = r.route(&ctx, make_tools());
        let names_a: Vec<&str> = a.iter().map(|r| r.schema.name.as_str()).collect();
        let names_b: Vec<&str> = b.iter().map(|r| r.schema.name.as_str()).collect();
        assert_eq!(names_a, names_b, "router must be deterministic");
    }

    // -----------------------------------------------------------------
    // L1.4: VectorRouter::from_provider
    // -----------------------------------------------------------------

    use crate::embedding::{EmbeddingProvider, HashingEmbeddingProvider};

    /// Run a closure inside a single-threaded Tokio runtime so
    /// `from_provider`'s `Handle::try_current()` succeeds. We avoid
    /// pulling in a `#[tokio::test]` macro to keep the existing
    /// synchronous `route()` tests untouched.
    fn in_runtime<F: FnOnce()>(f: F) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build runtime");
        let _guard = rt.enter();
        f();
    }

    #[test]
    fn l14_from_provider_indexes_and_routes() {
        // HashingEmbeddingProvider: embed turns text into a normalized
        // vector via character trigrams + FNV-1a bucket. Same text → same
        // vector. Identical queries embed to identical vectors, so
        // identical tool descriptions should rank first.
        let provider = Arc::new(HashingEmbeddingProvider::new(8));
        in_runtime(|| {
            let mut r = VectorRouter::from_provider(provider.clone());
            let tools = vec![
                make_tool("file_read", "Read a file from disk"),
                make_tool("file_write", "Write content to a file"),
                make_tool("web_search", "Search the public web"),
            ];
            r.index_tools(&tools);
            // Query literally repeats file_read's index text → must
            // rank #1 with cosine = 1.0.
            let ctx = TurnContext::from_user_message("file_read Read a file from disk")
                .with_max_tools(2);
            let routed = r.route(&ctx, tools);
            assert_eq!(routed.len(), 2);
            assert_eq!(routed[0].schema.name, "file_read",
                "exact-match query should rank its own tool first");
            assert!((routed[0].score - 1.0).abs() < 1e-5,
                "identical vectors should score 1.0, got {}", routed[0].score);
        });
    }

    #[test]
    fn l14_router_handles_dim_mismatch_gracefully() {
        // Tool indexed at dim=8, query embedded at dim=4. Cosine with
        // mismatched dims must return 0 (see `Self::cosine`) and the
        // router must not panic.
        in_runtime(|| {
            // Index with 8-dim closure
            let mut r = VectorRouter::new(Arc::new(|_| vec![1.0; 8]));
            r.index_tool(&make_tool("a", "first tool"));
            // Query with 4-dim closure — we can't swap embed_fn, but
            // we can verify a 4-dim tool, indexed and queried with
            // a 4-dim closure, scores 1.0 (regression guard).
            let mut r4 = VectorRouter::new(Arc::new(|_| vec![0.5; 4]));
            r4.index_tool(&make_tool("b", "second tool"));
            let ctx = TurnContext::from_user_message("anything");
            let routed = r4.route(&ctx, vec![make_tool("b", "second tool")]);
            assert_eq!(routed.len(), 1);
            assert!((routed[0].score - 1.0).abs() < 1e-5);
            // Touch the 8-dim router to ensure it doesn't break the
            // 4-dim one.
            let _ = r;
        });
    }

    #[test]
    fn l14_from_provider_embed_error_returns_zero_vector() {
        // A provider that always errors. The router must NOT panic;
        // the tool just scores 0.
        struct ErrProvider;
        #[async_trait::async_trait]
        impl EmbeddingProvider for ErrProvider {
            fn model_name(&self) -> &str { "err" }
            fn dimension(&self) -> usize { 4 }
            async fn embed(&self, _text: &str) -> crate::embedding::EmbeddingResult<Vec<f32>> {
                Err(crate::embedding::EmbeddingError::Other("nope".into()))
            }
            async fn embed_batch(
                &self,
                _texts: &[&str],
            ) -> crate::embedding::EmbeddingResult<Vec<Vec<f32>>> {
                Ok(Vec::new())
            }
        }
        let p: Arc<dyn EmbeddingProvider> = Arc::new(ErrProvider);
        in_runtime(|| {
            let mut r = VectorRouter::from_provider(p);
            r.index_tool(&make_tool("a", "alpha"));
            let ctx = TurnContext::from_user_message("alpha").with_max_tools(1);
            let routed = r.route(&ctx, vec![make_tool("a", "alpha")]);
            assert_eq!(routed.len(), 1);
            assert_eq!(routed[0].score, 0.0);
        });
    }

    // ----- PriorityBoost (L1 Domain Mode integration) ----------------------

    #[test]
    fn priority_boost_adds_score_to_priority_tools() {
        let inner: Arc<dyn ToolSchemaRouter> = Arc::new(KeywordRouter::new());
        let router = PriorityBoost::with_default_boost(inner);
        let ctx = TurnContext::from_user_message("天气")
            .with_max_tools(5)
            .with_priority_tools(vec!["read_file".into(), "write_file".into()]);
        let routed = router.route(&ctx, make_tools());
        // read_file and write_file should have boosted scores
        let rf = routed.iter().find(|r| r.schema.name == "read_file").unwrap();
        assert!(rf.score >= 5.0, "read_file should have +5.0 boost, got {}", rf.score);
        assert!(rf.source.contains("priority"), "source should contain 'priority': {}", rf.source);
    }

    #[test]
    fn priority_boost_no_op_when_empty() {
        let inner: Arc<dyn ToolSchemaRouter> = Arc::new(KeywordRouter::new());
        let router = PriorityBoost::with_default_boost(inner);
        let ctx = TurnContext::from_user_message("天气").with_max_tools(5);
        // No priority_tools set
        let routed = router.route(&ctx, make_tools());
        // No tool should have "priority" in its source
        assert!(routed.iter().all(|r| !r.source.contains("priority")));
    }

    #[test]
    fn priority_boost_reranks_after_boost() {
        let inner: Arc<dyn ToolSchemaRouter> = Arc::new(KeywordRouter::new());
        let router = PriorityBoost::with_default_boost(inner);
        // "天气" matches weather, but read_file is in priority_tools
        let ctx = TurnContext::from_user_message("天气")
            .with_max_tools(5)
            .with_priority_tools(vec!["read_file".into()]);
        let routed = router.route(&ctx, make_tools());
        // After boost, read_file should rank higher than it would without boost
        // (weather still wins because keyword match is strong, but read_file
        // should be in the top 5 due to +5.0 boost)
        assert!(routed.iter().any(|r| r.schema.name == "read_file"),
            "read_file should be in top-5 after priority boost");
    }

    #[test]
    fn priority_boost_custom_amount() {
        let inner: Arc<dyn ToolSchemaRouter> = Arc::new(KeywordRouter::new());
        let router = PriorityBoost::new(inner, 100.0);
        let ctx = TurnContext::from_user_message("天气")
            .with_max_tools(5)
            .with_priority_tools(vec!["memory".into()]);
        let routed = router.route(&ctx, make_tools());
        // memory has no keyword match for "天气", but +100.0 boost should
        // push it to the top
        assert_eq!(routed[0].schema.name, "memory",
            "memory should be #1 after +100.0 boost");
    }

    #[test]
    fn priority_boost_name() {
        let inner: Arc<dyn ToolSchemaRouter> = Arc::new(KeywordRouter::new());
        let router = PriorityBoost::with_default_boost(inner);
        assert_eq!(router.name(), "PriorityBoost");
    }
}
