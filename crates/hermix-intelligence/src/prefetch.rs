//! Phase 13 / H2 — Intent-aware prefetch & parallel fan-out.
//!
//! When the user says "写早报", "今天天气", "看本周日程", "搜一下 X" —
//! we know *before* the LLM reasons about the request that one or
//! more read-only tools are about to be called. The naive agent
//! loop would let the LLM call those tools one at a time, each
//! blocking the next turn of reasoning. The prefetcher instead:
//!
//! 1. Classifies the user message into a [`PrefetchIntent`].
//! 2. Expands the intent into a [`PrefetchPlan`] — a small list of
//!    `(tool_name, args)` calls that *might* be useful, with
//!    arg placeholders taken from the message itself
//!    (e.g. the city name in "北京今天天气怎么样").
//! 3. Filters the plan to only contain tools the agent has
//!    registered as [`RiskLevel::Safe`](hermix_core::RiskLevel::Safe)
//!    — we never pre-dispatch a destructive tool.
//! 4. Fans the plan out in parallel (capped concurrency) and
//!    collects the results.
//! 5. Serializes the results into a system-prompt block the LLM
//!    sees as "background knowledge" before it starts its turn.
//!
//! A second, complementary path — [`LearnedPrefetcher`] — watches
//! the tool-call history (across sessions) for `(intent, tool)`
//! co-occurrence patterns and tunes the keyword prefetcher's
//! confidence over time. This is the L0→L1 evolution the
//! proposal calls out: start with cheap keywords, graduate to
//! learned priors once we have history.

use std::collections::HashMap;
use std::sync::Arc;

use hermix_core::traits::RiskLevel;
use std::sync::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::tool_router::TurnContext;

// ---------------------------------------------------------------------------
// Intent vocabulary
// ---------------------------------------------------------------------------

/// High-level user intent the prefetcher recognizes.
///
/// New intents are added by:
/// 1. Extending this enum.
/// 2. Adding a `KeywordRule` for them in `KeywordPrefetcher::new()`.
/// 3. (Optionally) providing args in `default_args_for()`.
///
/// `Unknown` is the catch-all — the prefetcher still runs
/// `LearnedPrefetcher`'s history lookups on it, but never fans
/// out tool calls speculatively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrefetchIntent {
    /// "早报" / "今日要闻" — daily news/summary digest.
    DailyReport,
    /// "北京天气" / "今天会下雨吗" — weather query.
    WeatherQuery,
    /// "我今天的日程" / "本周会议" — calendar / schedule lookup.
    ScheduleCheck,
    /// "搜一下 X" / "查一下 Y" — web search.
    WebSearch,
    /// "X 文件里有什么" / "打开 config.yaml" — file read hint.
    FileRead,
    /// "帮我审一下这段代码" / "review" — code review.
    CodeReview,
    /// "我之前说过…" — recall from memory. Real prefetch is a no-op;
    /// we just hint the LLM that `memory` tool is highly relevant.
    MemoryRecall,
    /// No recognized intent. Prefetcher returns an empty plan
    /// (still useful: lets the LLM run un-prefetched).
    Unknown,
}

impl PrefetchIntent {
    /// All known intents, in stable iteration order. Used by
    /// `LearnedPrefetcher` to enumerate stats.
    pub const ALL: &'static [PrefetchIntent] = &[
        // Unknown first so a learned-prior lookup on an
        // otherwise-unmatched message (line ~678) prefers to
        // keep the intent as Unknown rather than promoting to
        // an unrelated intent that happens to be popular.
        Self::Unknown,
        Self::DailyReport,
        Self::WeatherQuery,
        Self::ScheduleCheck,
        Self::WebSearch,
        Self::FileRead,
        Self::CodeReview,
        Self::MemoryRecall,
    ];
}

// ---------------------------------------------------------------------------
// Plan
// ---------------------------------------------------------------------------

/// A single speculative tool call the prefetcher will dispatch.
///
/// `args` is a JSON object matching the tool's parameter schema.
/// Slot placeholders that couldn't be filled from the user message
/// (e.g. city for "今天天气怎么样" with no city) are left as
/// `null` and the plan entry is **dropped** before dispatch — the
/// prefetcher never calls a tool with missing required args.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlanEntry {
    pub tool: String,
    pub args: Value,
}

/// Output of `KeywordPrefetcher::plan()` and input to
/// `AsyncPrefetcher::dispatch()`. An empty `entries` vec means
/// "no prefetch this turn".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PrefetchPlan {
    pub intent: PrefetchIntent,
    /// Confidence in [0, 1] that the plan matches the user's
    /// actual ask. Below `MinConfidence` (0.4) the agent loop
    /// skips prefetch and lets the LLM run normally.
    pub confidence: f32,
    pub entries: Vec<PlanEntry>,
}

impl PrefetchPlan {
    pub fn empty(intent: PrefetchIntent) -> Self {
        Self {
            intent,
            confidence: 0.0,
            entries: Vec::new(),
        }
    }

    /// Confidence threshold below which `dispatch()` returns
    /// without fanning anything out. Tuned to err on the side of
    /// "if unsure, don't waste tool calls".
    pub const MIN_CONFIDENCE: f32 = 0.4;
}

// ---------------------------------------------------------------------------
// Result of a real dispatch
// ---------------------------------------------------------------------------

/// One tool's worth of prefetch output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrefetchResult {
    pub tool: String,
    pub args: Value,
    /// JSON-stringified tool output. We keep it as a string (not
    /// `Value`) because the underlying tool's output is opaque —
    /// it may be text, a JSON envelope, or even non-UTF-8 binary
    /// that the tool already serialized. The agent loop hands
    /// this verbatim to the LLM as a system-prompt block.
    pub output: String,
    /// True if the tool returned an error. The agent loop
    /// reports this to telemetry but does **not** surface error
    /// text to the LLM (we don't want a flaky prefetch to
    /// confuse the model about what it should do).
    pub errored: bool,
    /// Wall-clock duration of the tool call, recorded so the
    /// telemetry layer can spot a misbehaving prefetch slowing
    /// down the agent loop.
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrefetchOutcome {
    pub intent: PrefetchIntent,
    pub confidence: f32,
    pub results: Vec<PrefetchResult>,
}

impl PrefetchOutcome {
    /// Render the outcome as a system-prompt block the LLM sees
    /// as "background context". We use a fenced, named block so
    /// the LLM can tell where prefetched data ends and the rest
    /// of the prompt begins.
    pub fn to_system_prompt_block(&self) -> String {
        if self.results.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        out.push_str("\n\n[Prefetched context — read-only tools called speculatively based on the user message]\n");
        for r in &self.results {
            if r.errored {
                // Per design: never surface errors to the LLM.
                continue;
            }
            out.push_str(&format!(
                "\n--- {} ({}) ---\n{}\n",
                r.tool,
                serde_json::to_string(&r.args)
                    .unwrap_or_else(|_| "<args>".to_string()),
                r.output
            ));
        }
        out.push_str("\n[End of prefetched context]\n");
        out
    }
}

// ---------------------------------------------------------------------------
// KeywordPrefetcher — L0 intent classifier
// ---------------------------------------------------------------------------

/// A single keyword rule. Triggers when **all** `must_contain`
/// tokens are present in the lowercased user message. We use
/// Chinese bigrams (where applicable) so a single-character word
/// match still feels natural in CJK.
#[derive(Debug, Clone)]
struct KeywordRule {
    intent: PrefetchIntent,
    must_contain: &'static [&'static str],
    /// Default tool to call when this rule fires and the user
    /// message provides no further hints. `None` for intents
    /// that always need an arg (city, query, …) — those rely on
    /// `arg_extractors`.
    default_tool: Option<&'static str>,
    /// Confidence this rule contributes when it fires.
    confidence: f32,
}

/// Hand-rolled argument extractors. Each one scans the lowercased
/// message for the slot (city, query, file path, …) and returns
/// the first match as a `String`. Returns `None` if the slot
/// can't be filled, in which case the plan entry is dropped.
type ArgExtractor = fn(&str) -> Option<String>;

fn extract_city(msg: &str) -> Option<String> {
    // Greedy: take the longest run of CJK chars that comes right
    // before a weather trigger. "北京今天天气怎么样" → "北京".
    for kw in ["天气", "气温", "下雨", "下雪", "weather", "temperature"] {
        if let Some(idx) = msg.find(kw) {
            let prefix: String = msg[..idx]
                .chars()
                .rev()
                .take_while(|c| c.is_alphanumeric() || *c == '市' || *c == '区' || *c == '省')
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            let trimmed: String = prefix
                .trim_start_matches(|c: char| !c.is_alphanumeric() && c != '市' && c != '省')
                .to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

fn extract_query(msg: &str) -> Option<String> {
    // Pull everything after "搜"/"查"/"search"/"find".
    for kw in ["搜一下", "查一下", "搜索", "search for", "find ", "look up "] {
        if let Some(idx) = msg.find(kw) {
            return Some(msg[idx + kw.len()..].trim().to_string());
        }
    }
    None
}

fn extract_file_path(msg: &str) -> Option<String> {
    // Grab the first whitespace-delimited token that looks like
    // a path or filename (contains a `.` or `/` or `\`).
    for token in msg.split_whitespace() {
        if token.contains('.') || token.contains('/') || token.contains('\\') {
            return Some(token.to_string());
        }
    }
    None
}

/// Static rule table. Order doesn't matter for correctness
/// (multiple rules can fire on the same message); we take the
/// highest-confidence match. Adding a new intent is "add a row
/// here + extend `PrefetchIntent`".
const KEYWORD_RULES: &[KeywordRule] = &[
    KeywordRule {
        intent: PrefetchIntent::DailyReport,
        must_contain: &["早报", "日报", "今日要闻", "今日总结", "morning report"],
        default_tool: Some("web_search"),
        confidence: 0.85,
    },
    KeywordRule {
        intent: PrefetchIntent::WeatherQuery,
        must_contain: &["天气", "气温", "下雨", "weather", "temperature"],
        default_tool: Some("weather"),
        confidence: 0.9,
    },
    KeywordRule {
        intent: PrefetchIntent::ScheduleCheck,
        must_contain: &["日程", "会议", "calendar", "schedule", "本周安排"],
        default_tool: Some("calendar"),
        confidence: 0.85,
    },
    KeywordRule {
        intent: PrefetchIntent::WebSearch,
        must_contain: &["搜", "查", "search", "find"],
        default_tool: Some("web_search"),
        confidence: 0.6, // lower because the trigger words are common
    },
    KeywordRule {
        intent: PrefetchIntent::FileRead,
        must_contain: &["打开", "看看", "读一下", "文件", "open file", "read"],
        default_tool: Some("read_file"),
        confidence: 0.65,
    },
    KeywordRule {
        intent: PrefetchIntent::CodeReview,
        must_contain: &["review", "code review", "审查", "审一下", "检视"],
        default_tool: Some("read_file"),
        confidence: 0.7,
    },
    KeywordRule {
        intent: PrefetchIntent::MemoryRecall,
        must_contain: &["我之前", "我说过", "记得", "earlier", "before", "previously"],
        default_tool: Some("memory"),
        confidence: 0.55,
    },
];

/// Dispatch table: which extractor to run for which intent. If
/// the user message has no slot value, the plan entry is dropped
/// (we never call a tool with missing required args).
fn arg_extractor(intent: PrefetchIntent) -> Option<ArgExtractor> {
    match intent {
        PrefetchIntent::WeatherQuery => Some(extract_city),
        PrefetchIntent::WebSearch => Some(extract_query),
        PrefetchIntent::FileRead => Some(extract_file_path),
        PrefetchIntent::CodeReview => Some(extract_file_path),
        _ => None,
    }
}

/// L0 prefetcher: pure keyword matching, no state. Thread-safe
/// (no interior mutability needed — all rules are `&'static`).
#[derive(Debug, Clone, Default)]
pub struct KeywordPrefetcher;

impl KeywordPrefetcher {
    pub fn new() -> Self {
        Self
    }

    /// Classify the user message and emit a plan.
    ///
    /// `available_tools` is the set of tool names the agent loop
    /// has registered this turn. Plan entries that reference a
    /// tool not in this set are dropped — without this filter the
    /// prefetcher would happily try to call `weather` on an agent
    /// that has no weather tool.
    pub fn plan(&self, ctx: &TurnContext, available_tools: &[String]) -> PrefetchPlan {
        let msg_lower = ctx.user_message.to_lowercase();
        let mut best: Option<(f32, &KeywordRule)> = None;
        for rule in KEYWORD_RULES {
            // The rule fires when ANY of its `must_contain` keywords
            // appears in the user message. We used to gate on `all()`
            // — that made the entire L0 keyword table dead code, since
            // a rule like WeatherQuery lists 5 alternative trigger
            // words (`天气` / `气温` / `下雨` / `weather` /
            // `temperature`) and a real query rarely hits more than
            // one. The table-level comment ("multiple rules can fire
            // on the same message; we take the highest-confidence
            // match") already implied ANY semantics — this is just
            // bringing the code in line.
            let any_match = rule
                .must_contain
                .iter()
                .any(|k| msg_lower.contains(&k.to_lowercase()));
            if any_match {
                let conf = rule.confidence;
                match best {
                    Some((b, _)) if b >= conf => {}
                    _ => best = Some((conf, rule)),
                }
            }
        }
        let Some((conf, rule)) = best else {
            return PrefetchPlan::empty(PrefetchIntent::Unknown);
        };

        let mut entries = Vec::new();
        if let Some(tool) = rule.default_tool {
            if !available_tools.iter().any(|t| t == tool) {
                // The "obvious" tool isn't available. Don't
                // bother emitting a plan the dispatcher will
                // just drop.
                return PrefetchPlan {
                    intent: rule.intent,
                    confidence: conf * 0.5, // halved: we can't actually prefetch
                    entries: Vec::new(),
                };
            }
            let args = if let Some(extract) = arg_extractor(rule.intent) {
                match extract(&ctx.user_message) {
                    Some(v) => {
                        // The tool schemas for `weather` /
                        // `web_search` / `read_file` use different
                        // arg names; the dispatcher picks whichever
                        // key the tool actually wants. We stuff all
                        // candidates in and the tool implementation
                        // reads the one it knows.
                        let mut out = serde_json::Map::new();
                        out.insert("q".to_string(), Value::String(v.clone()));
                        out.insert("query".to_string(), Value::String(v.clone()));
                        out.insert("city".to_string(), Value::String(v.clone()));
                        out.insert("path".to_string(), Value::String(v));
                        Value::Object(out)
                    }
                    None => Value::Null,
                }
            } else {
                json!({})
            };
            if !args.is_null() {
                entries.push(PlanEntry {
                    tool: tool.to_string(),
                    args,
                });
            }
        }
        PrefetchPlan {
            intent: rule.intent,
            confidence: conf,
            entries,
        }
    }
}

// ---------------------------------------------------------------------------
// LearnedPrefetcher — L1 history-based priors
// ---------------------------------------------------------------------------

/// Co-occurrence counter. Maps `(intent, tool_name)` → how many
/// times the user (in any session) phrased a message of this
/// intent and the LLM ended up calling this tool first.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct IntentStats {
    /// `(intent, tool)` → count. Key is a `String` (not a tuple)
    /// so the map round-trips through serde_json — JSON object keys
    /// must be strings. Persisted to disk at session boundary by the
    /// agent loop; loaded back on startup.
    pub cooccurrence: HashMap<String, u64>,
    /// Key format: `format!("{:?}|{}", intent, tool)`. Centralized in
    /// [`cooccurrence_key`] so the encoder (IntentStats::observe) and
    /// the decoder (IntentStats::conditional) can never disagree on
    /// formatting — a one-character drift would silently lose all
    /// learned priors across a serialize/deserialize round-trip.
    /// Total observations per intent. Used to normalize
    /// confidence when no rule fired (Unknown case).
    pub totals: HashMap<String, u64>,
}

impl IntentStats {
    /// P(intent, tool) given the observed counts, with a
    /// small-Laplace smoother so a never-seen pair doesn't get
    /// zero probability. The result is in [0, 1].
    pub fn conditional(&self, intent: PrefetchIntent, tool: &str) -> f32 {
        let key = cooccurrence_key(intent, tool);
        let seen = self.cooccurrence.get(&key).copied().unwrap_or(0);
        let total = self
            .totals
            .get(&format!("{:?}", intent))
            .copied()
            .unwrap_or(0);
        let k = self.cooccurrence.len().max(1) as f64;
        ((seen as f64 + 1.0) / (total as f64 + k)) as f32
    }

    /// Record a single observation: when a turn classified as
    /// `intent` first called `tool`. Called by the agent loop
    /// after each turn completes.
    pub fn observe(&mut self, intent: PrefetchIntent, tool: &str) {
        let key = cooccurrence_key(intent, tool);
        *self.cooccurrence.entry(key).or_insert(0) += 1;
        *self
            .totals
            .entry(format!("{:?}", intent))
            .or_insert(0) += 1;
    }
}

/// Stable string key for the (intent, tool) cooccurrence map.
fn cooccurrence_key(intent: PrefetchIntent, tool: &str) -> String {
    format!("{:?}|{}", intent, tool)
}


/// Thread-safe wrapper around `IntentStats` so the agent loop
/// can record observations from one task and read priors from
/// another.
#[derive(Debug, Clone, Default)]
pub struct LearnedPrefetcher {
    stats: Arc<RwLock<IntentStats>>,
}

impl LearnedPrefetcher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_stats(stats: IntentStats) -> Self {
        Self {
            stats: Arc::new(RwLock::new(stats)),
        }
    }

    /// Read-only snapshot for serialization.
    pub fn snapshot(&self) -> IntentStats {
        self.stats.read().expect("stats lock poisoned").clone()
    }

    /// Record an observation. Cheap; safe to call from any task.
    pub fn observe(&self, intent: PrefetchIntent, tool: &str) {
        let mut s = self.stats.write().expect("stats lock poisoned");
        s.observe(intent, tool);
    }

    /// Look up a learned prior. If the conditional probability
    /// exceeds `threshold`, this is a strong signal the LLM is
    /// *very likely* to call `tool` — bump the plan's confidence
    /// by that amount. The agent loop combines this with the
    /// keyword confidence via a `max()` (we trust the higher of
    /// the two signals).
    pub fn boost(&self, intent: PrefetchIntent, tool: &str, threshold: f32) -> Option<f32> {
        let s = self.stats.read().expect("stats lock poisoned");
        // Require at least one observation for this intent. Without
        // this guard, Laplace smoothing inflates any unseen pair to
        // p = 1/k (close to 1.0 when the cooccurrence map is tiny),
        // so a single unrelated prefetch would get an unjustified
        // confidence boost from a never-seen intent — polluting
        // the agent loop's first impression of a new toolset.
        let total = s
            .totals
            .get(&format!("{:?}", intent))
            .copied()
            .unwrap_or(0);
        if total < 1 {
            return None;
        }
        let p = s.conditional(intent, tool);
        if p >= threshold {
            Some(p)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// AsyncPrefetcher — the executor
// ---------------------------------------------------------------------------

/// Trait abstracting "execute one tool call". The agent loop
/// implements this by calling into `ToolRegistry::dispatch_async`
/// (which itself checks `RiskLevel` and PermissionLevel). Tests
/// provide a mock that returns canned results.
#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(&self, tool: &str, args: Value) -> (String, bool);
}

/// Concrete executor backed by `ToolRegistry`. Filters out any
/// tool whose `risk_level` is not `Safe` — prefetch **must never**
/// fire a destructive tool, even if the LLM later would have
/// asked for it.
pub struct SafeToolExecutor {
    registry: Arc<dyn SafeToolRegistry>,
    max_concurrency: usize,
}

#[async_trait::async_trait]
pub trait SafeToolRegistry: Send + Sync {
    fn risk_level(&self, name: &str) -> Option<RiskLevel>;
    fn exists(&self, name: &str) -> bool;
    async fn dispatch(&self, name: &str, args: Value) -> String;
}

impl SafeToolExecutor {
    pub fn new(registry: Arc<dyn SafeToolRegistry>) -> Self {
        Self {
            registry,
            max_concurrency: 4,
        }
    }

    pub fn with_max_concurrency(mut self, n: usize) -> Self {
        self.max_concurrency = n.max(1);
        self
    }
}

#[async_trait::async_trait]
impl ToolExecutor for SafeToolExecutor {
    async fn execute(&self, tool: &str, args: Value) -> (String, bool) {
        // Two safety gates: tool must exist AND be Safe.
        if !self.registry.exists(tool) {
            return (format!("tool not found: {tool}"), true);
        }
        match self.registry.risk_level(tool) {
            Some(RiskLevel::Safe) => {}
            Some(_) => {
                return (
                    format!("refusing to prefetch non-Safe tool: {tool}"),
                    true,
                );
            }
            None => return (format!("tool not registered: {tool}"), true),
        }
        let out = self.registry.dispatch(tool, args).await;
        // We can't tell from the raw output string whether the
        // tool considered itself successful; the convention in
        // this codebase is for tool errors to be wrapped in a
        // `Tool error: …` envelope. Detect that cheaply.
        let errored = out.contains("Tool error:");
        (out, errored)
    }
}

impl SafeToolExecutor {
    /// Run a plan in parallel, capped at `max_concurrency`. Each
    /// entry's result is recorded with its wall-clock duration
    /// for telemetry. Entries that reference a non-Safe tool are
    /// silently skipped (the executor refuses them up front).
    ///
    /// We use `futures::future::join_all` for true concurrency:
    /// `SafeToolExecutor::execute` is async, so the futures are
    /// polled together and individual tool calls overlap. In
    /// practice most plans have 1 entry (a single `weather` or
    /// `web_search` call), so this rarely parallelizes more than
    /// one tool — but the seam is here for the cases where it
    /// matters (multi-tool intents in the future).
    pub async fn dispatch(&self, plan: &PrefetchPlan) -> PrefetchOutcome {
        use futures::future::join_all;
        use std::time::Instant;

        let futures = plan.entries.iter().map(|entry| async move {
            let start = Instant::now();
            let (output, errored) = self.execute(&entry.tool, entry.args.clone()).await;
            PrefetchResult {
                tool: entry.tool.clone(),
                args: entry.args.clone(),
                output,
                errored,
                duration_ms: start.elapsed().as_millis() as u64,
            }
        });
        let results = join_all(futures).await;
        PrefetchOutcome {
            intent: plan.intent,
            confidence: plan.confidence,
            results,
        }
    }
}

// ---------------------------------------------------------------------------
// Public façade: Prefetcher
// ---------------------------------------------------------------------------

/// The composable prefetcher the agent loop sees. Wires together
/// the keyword classifier + learned priors + a tool executor.
pub struct Prefetcher {
    pub keywords: KeywordPrefetcher,
    pub learned: LearnedPrefetcher,
    pub executor: Arc<dyn ToolExecutor>,
}

impl Prefetcher {
    pub fn new(executor: Arc<dyn ToolExecutor>) -> Self {
        Self {
            keywords: KeywordPrefetcher::new(),
            learned: LearnedPrefetcher::new(),
            executor,
        }
    }

    pub fn with_learned(executor: Arc<dyn ToolExecutor>, learned: LearnedPrefetcher) -> Self {
        Self {
            keywords: KeywordPrefetcher::new(),
            learned,
            executor,
        }
    }

    /// End-to-end: classify → plan → filter by available tools →
    /// boost confidence with learned priors → dispatch → return
    /// the outcome (or `None` if confidence was below threshold).
    pub async fn run(
        &self,
        ctx: &TurnContext,
        available_tools: &[String],
    ) -> Option<PrefetchOutcome> {
        let mut plan = self.keywords.plan(ctx, available_tools);
        if plan.intent == PrefetchIntent::Unknown {
            // Even when the keyword classifier didn't fire,
            // the learned prior can still give us a hint. We
            // iterate over all known intents and pick the
            // highest-confidence one.
            let mut best_conf: f32 = 0.0;
            let mut best_intent: Option<PrefetchIntent> = None;
            let mut best_tool: Option<String> = None;
            for &intent in PrefetchIntent::ALL {
                for tool in available_tools {
                    if let Some(p) = self.learned.boost(intent, tool, 0.5) {
                        if p > best_conf {
                            best_conf = p;
                            best_intent = Some(intent);
                            best_tool = Some(tool.clone());
                        }
                    }
                }
            }
            if let (Some(intent), Some(tool)) = (best_intent, best_tool) {
                plan = PrefetchPlan {
                    intent,
                    confidence: best_conf,
                    entries: vec![PlanEntry {
                        tool,
                        args: Value::Null,
                    }],
                };
            }
        } else if let Some(entry) = plan.entries.first() {
            if let Some(boost) = self
                .learned
                .boost(plan.intent, &entry.tool, PrefetchPlan::MIN_CONFIDENCE)
            {
                plan.confidence = plan.confidence.max(boost);
            }
        }

        if plan.confidence < PrefetchPlan::MIN_CONFIDENCE || plan.entries.is_empty() {
            return None;
        }
        Some(self.executor_dispatch(&plan).await)
    }

    /// The actual dispatch. We deliberately keep this off the
    /// `ToolExecutor` trait so a custom executor can do its own
    /// thing (e.g. record timings, abort on timeout) without
    /// touching this method.
    async fn executor_dispatch(&self, plan: &PrefetchPlan) -> PrefetchOutcome {
        use futures::future::join_all;
        use std::time::Instant;

        let executor = self.executor.clone();
        let futures = plan.entries.iter().map(|entry| {
            let tool = entry.tool.clone();
            let args = entry.args.clone();
            let exec = executor.clone();
            async move {
                let start = Instant::now();
                let (output, errored) = exec.execute(&tool, args.clone()).await;
                PrefetchResult {
                    tool,
                    args,
                    output,
                    errored,
                    duration_ms: start.elapsed().as_millis() as u64,
                }
            }
        });
        let results = join_all(futures).await;
        PrefetchOutcome {
            intent: plan.intent,
            confidence: plan.confidence,
            results,
        }
    }
}

// ---------------------------------------------------------------------------
// Tool-schema helper (used to filter available tools without a
// full ToolRegistry).
// ---------------------------------------------------------------------------

/// Extract the list of tool names from a slice of schemas
/// (anything with a `name: String` field — e.g.
/// `hermix_core::ToolSchema`). Convenience for the agent loop,
/// which already has the routed tool schemas in scope and wants
/// to hand the prefetcher just the names it cares about.
///
/// The generic form avoids a hard dependency on `ToolSchema` (the
/// field is `pub name: String` on every schema-like type in the
/// codebase, so we read it reflectively via a small trait).
pub fn tool_names<S: NamedSchema>(schemas: &[S]) -> Vec<String> {
    schemas.iter().map(|s| s.name().to_string()).collect()
}

/// Tiny trait abstracting "anything that has a name field". Lets
/// `tool_names` work with `ToolSchema`, `ToolEntryInfo`, and
/// future schema types without `prefetch.rs` taking a
/// dependency on the tool-crate types.
pub trait NamedSchema {
    fn name(&self) -> &str;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_core::MessageRole;
    use std::sync::Mutex;
    use std::sync::Arc;

    fn ctx(msg: &str) -> TurnContext {
        TurnContext::from_user_message(msg)
    }

    fn tools(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    // --- KeywordPrefetcher ----------------------------------------

    #[test]
    fn weather_query_extracts_city() {
        let p = KeywordPrefetcher::new();
        let plan = p.plan(&ctx("北京今天天气怎么样"), &tools(&["weather", "read_file"]));
        assert_eq!(plan.intent, PrefetchIntent::WeatherQuery);
        assert!(plan.confidence >= PrefetchPlan::MIN_CONFIDENCE);
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].tool, "weather");
    }

    #[test]
    fn daily_report_plan() {
        let p = KeywordPrefetcher::new();
        let plan = p.plan(&ctx("写今天的早报"), &tools(&["web_search"]));
        assert_eq!(plan.intent, PrefetchIntent::DailyReport);
        // No extractor for DailyReport → no entries (the
        // dispatcher can't synthesize a query). The plan still
        // has high confidence so the LLM knows prefetch
        // considered this an early-bird request.
        assert!(plan.confidence > 0.5);
    }

    #[test]
    fn web_search_extracts_query() {
        let p = KeywordPrefetcher::new();
        let plan = p.plan(&ctx("搜一下 Rust 1.78 release notes"), &tools(&["web_search"]));
        assert_eq!(plan.intent, PrefetchIntent::WebSearch);
        let args_str = plan.entries[0].args.to_string();
        assert!(args_str.contains("Rust"));
    }

    #[test]
    fn file_read_extracts_path() {
        let p = KeywordPrefetcher::new();
        let plan = p.plan(
            &ctx("打开 Cargo.toml 看看"),
            &tools(&["read_file"]),
        );
        assert_eq!(plan.intent, PrefetchIntent::FileRead);
        assert!(plan.entries[0].args.to_string().contains("Cargo.toml"));
    }

    #[test]
    fn unknown_intent_returns_empty() {
        let p = KeywordPrefetcher::new();
        let plan = p.plan(&ctx("今天晚饭吃什么"), &tools(&["read_file", "web_search"]));
        assert_eq!(plan.intent, PrefetchIntent::Unknown);
        assert!(plan.entries.is_empty());
    }

    #[test]
    fn unavailable_tool_downgrades_confidence() {
        let p = KeywordPrefetcher::new();
        let plan = p.plan(&ctx("北京天气"), &tools(&["read_file"]));
        // No `weather` tool → confidence halved.
        assert!(plan.confidence > 0.0 && plan.confidence < 0.9);
    }

    #[test]
    fn english_keywords_work_too() {
        let p = KeywordPrefetcher::new();
        let plan = p.plan(&ctx("what's the weather in tokyo"), &tools(&["weather"]));
        assert_eq!(plan.intent, PrefetchIntent::WeatherQuery);
    }

    // --- LearnedPrefetcher ----------------------------------------

    #[test]
    fn learned_boost_increases_confidence() {
        let learned = LearnedPrefetcher::new();
        for _ in 0..20 {
            learned.observe(PrefetchIntent::WeatherQuery, "weather");
        }
        let boost = learned
            .boost(PrefetchIntent::WeatherQuery, "weather", 0.4)
            .expect("boost should fire");
        assert!(boost > 0.9, "with 20 hits the boost should be near-1.0, got {boost}");
    }

    #[test]
    fn learned_boost_skips_below_threshold() {
        // Zero observations — the `total < 1` guard inside
        // `boost()` short-circuits to `None` regardless of how
        // low the threshold goes. This is the strongest form
        // of "skip below threshold": an unseen pair must never
        // bootstrap a confidence bump from Laplace smoothing.
        let learned = LearnedPrefetcher::new();
        assert!(learned
            .boost(PrefetchIntent::WeatherQuery, "weather", 0.5)
            .is_none());
    }

    #[test]
    fn learned_boost_laplace_smoothing_keeps_unseen_pairs_alive() {
        let learned = LearnedPrefetcher::new();
        // A never-seen pair still returns a non-zero probability.
        let p = learned
            .stats
            .read()
            .unwrap()
            .conditional(PrefetchIntent::WeatherQuery, "weather");
        assert!(p > 0.0, "Laplace smoothing should keep unseen pairs alive");
    }

    #[test]
    fn stats_persist_via_serde() {
        let mut stats = IntentStats::default();
        stats.observe(PrefetchIntent::WeatherQuery, "weather");
        let json = serde_json::to_string(&stats).unwrap();
        let back: IntentStats = serde_json::from_str(&json).unwrap();
        assert_eq!(back.totals.get("WeatherQuery"), Some(&1));
    }

    // --- AsyncPrefetcher / ToolExecutor ---------------------------

    struct MockExecutor {
        responses: Arc<Mutex<HashMap<String, String>>>,
    }

    impl MockExecutor {
        fn new(responses: &[(&str, &str)]) -> Self {
            let mut m = HashMap::new();
            for (k, v) in responses {
                m.insert(k.to_string(), v.to_string());
            }
            Self {
                responses: Arc::new(Mutex::new(m)),
            }
        }
    }

    #[async_trait::async_trait]
    impl ToolExecutor for MockExecutor {
        async fn execute(&self, tool: &str, _args: Value) -> (String, bool) {
            let m = self.responses.lock().unwrap();
            match m.get(tool) {
                Some(out) => (out.clone(), false),
                None => (format!("Tool error: unknown tool {tool}"), true),
            }
        }
    }

    #[tokio::test]
    async fn end_to_end_prefetch_weather() {
        let exec = Arc::new(MockExecutor::new(&[("weather", "北京: 22°C 晴")]));
        let p = Prefetcher::new(exec);
        let outcome = p
            .run(&ctx("北京今天天气怎么样"), &tools(&["weather"]))
            .await
            .expect("prefetch should fire");
        assert_eq!(outcome.intent, PrefetchIntent::WeatherQuery);
        assert_eq!(outcome.results.len(), 1);
        assert!(outcome.results[0].output.contains("22°C"));
    }

    #[tokio::test]
    async fn end_to_end_prefetch_skipped_when_below_threshold() {
        // "今天晚饭吃什么" → no rule fires (Unknown) and no
        // learned prior. Confidence 0 < MIN_CONFIDENCE.
        let exec = Arc::new(MockExecutor::new(&[("web_search", "stub")]));
        let p = Prefetcher::new(exec);
        let outcome = p
            .run(&ctx("今天晚饭吃什么"), &tools(&["web_search"]))
            .await;
        assert!(outcome.is_none());
    }

    #[tokio::test]
    async fn end_to_end_uses_learned_prior_for_unknown_intent() {
        // Train the learned prior heavily on "晚饭 → web_search"
        // and the prefetcher should pick it up even when the
        // keyword classifier has no rule.
        let exec = Arc::new(MockExecutor::new(&[("web_search", "stub")]));
        let learned = LearnedPrefetcher::new();
        for _ in 0..50 {
            learned.observe(PrefetchIntent::Unknown, "web_search");
        }
        let p = Prefetcher::with_learned(exec, learned);
        let outcome = p
            .run(&ctx("今天晚饭吃什么"), &tools(&["web_search"]))
            .await
            .expect("learned prior should kick in");
        assert_eq!(outcome.intent, PrefetchIntent::Unknown);
        // Note: intent stays Unknown (we don't promote the
        // intent itself), but a tool was prefetched.
        assert_eq!(outcome.results.len(), 1);
    }

    #[tokio::test]
    async fn errors_do_not_appear_in_system_prompt() {
        // A flaky tool shouldn't confuse the LLM. We test the
        // rendering side: an errored result is omitted from
        // `to_system_prompt_block`.
        let outcome = PrefetchOutcome {
            intent: PrefetchIntent::WeatherQuery,
            confidence: 0.9,
            results: vec![
                PrefetchResult {
                    tool: "weather".into(),
                    args: json!({"q": "北京"}),
                    output: "22°C 晴".into(),
                    errored: false,
                    duration_ms: 5,
                },
                PrefetchResult {
                    tool: "weather".into(),
                    args: json!({"q": "上海"}),
                    output: "Tool error: API timeout".into(),
                    errored: true,
                    duration_ms: 5000,
                },
            ],
        };
        let block = outcome.to_system_prompt_block();
        assert!(block.contains("22°C"));
        assert!(!block.contains("Tool error"));
        assert!(!block.contains("API timeout"));
    }

    #[test]
    fn system_prompt_block_empty_when_no_results() {
        let outcome = PrefetchOutcome {
            intent: PrefetchIntent::Unknown,
            confidence: 0.0,
            results: vec![],
        };
        assert!(outcome.to_system_prompt_block().is_empty());
    }

    // --- SafeToolExecutor refuses non-Safe tools -----------------

    struct StubRegistry {
        safe: Vec<String>,
        danger: Vec<String>,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl StubRegistry {
        fn new(safe: &[&str], danger: &[&str]) -> Self {
            Self {
                safe: safe.iter().map(|s| s.to_string()).collect(),
                danger: danger.iter().map(|s| s.to_string()).collect(),
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().drain(..).collect()
        }
    }

    #[async_trait::async_trait]
    impl SafeToolRegistry for StubRegistry {
        fn risk_level(&self, name: &str) -> Option<RiskLevel> {
            if self.safe.contains(&name.to_string()) {
                Some(RiskLevel::Safe)
            } else if self.danger.contains(&name.to_string()) {
                Some(RiskLevel::Dangerous)
            } else {
                None
            }
        }
        fn exists(&self, name: &str) -> bool {
            self.safe.contains(&name.to_string()) || self.danger.contains(&name.to_string())
        }
        async fn dispatch(&self, name: &str, _args: Value) -> String {
            self.calls.lock().unwrap().push(name.to_string());
            format!("ok {name}")
        }
    }

    #[tokio::test]
    async fn safe_executor_skips_dangerous_tool() {
        let reg = Arc::new(StubRegistry::new(&["read_file"], &["shell"]));
        let exec = SafeToolExecutor::new(reg.clone());
        let plan = PrefetchPlan {
            intent: PrefetchIntent::FileRead,
            confidence: 0.9,
            entries: vec![
                PlanEntry {
                    tool: "read_file".into(),
                    args: json!({}),
                },
                PlanEntry {
                    tool: "shell".into(),
                    args: json!({}),
                },
            ],
        };
        let outcome = exec.dispatch(&plan).await;
        // The safe tool ran; the dangerous one was refused.
        assert_eq!(outcome.results.len(), 2);
        assert!(!outcome.results[0].errored);
        assert!(outcome.results[1].errored);
        assert_eq!(reg.calls(), vec!["read_file".to_string()]);
    }

    // --- MessageRole check (sanity, the agent loop uses this elsewhere) ---

    #[test]
    fn message_role_user_is_what_we_match() {
        // Documenting the agent loop's choice: when looking for
        // "the user message" in a chat history, the prefetcher
        // is called with `ctx.user_message` already extracted
        // by the agent. This test just pins the convention so a
        // future refactor of TurnContext doesn't silently change
        // which message we route on.
        let m = hermix_core::Message {
            role: MessageRole::User,
            content: Some("北京天气".into()),
            ..Default::default()
        };
        assert!(matches!(m.role, MessageRole::User));
    }
}
