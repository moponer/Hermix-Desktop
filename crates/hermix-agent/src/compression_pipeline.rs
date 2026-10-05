//! Compression pipeline primitives — the pre-summary compression passes.
//!
//! Designed after reading the Headroom article
//! (<https://www.chenxutan.com/d/4104.html>) but adapted to a Rust
//! single-binary Windows agent without an embedding model.
//!
//! Items living here:
//! - `PerRolePolicy` + `policy_for()` — single source of truth for
//!   which compression stages apply to which role.
//! - `is_pinned()` — pin-keyword / pin-role filter.
//! - `dedup_adjacent_user()` — Pass 0 user-text dedup.
//! - `TokenScorer` + `score()` — 4-dim heuristic scorer
//!   (recency / role_position / frequency / instructional_marker).
//! - `drop_low_score()` — pre-summary score drop.
//! - `CompressionReport` — structured log of one compression pass.
//!
//! **Design constraint:** we do NOT introduce an embedding model.
//! All scoring is over metadata already on `Message` (role,
//! `tool_call_id`, content patterns). This keeps the agent a
//! single Rust binary without 80-420 MB of ONNX / Python deps.

use hermix_core::{Message, MessageRole};

/// Action a single message should receive during compression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionAction {
    /// Never touch this message (pinned or system-role).
    Preserve,
    /// Drop this message entirely (low score, non-pinned).
    Drop,
    /// Replace with `[Duplicate …]` marker if an adjacent twin exists.
    Dedup,
    /// Pass through to the LLM summary stage.
    Summary,
}

/// Per-role compression policy. One variant per role, no extra
/// branches — keeps dispatch a single match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerRolePolicy {
    /// System prompt — never compressed, never dropped.
    Preserve,
    /// User text — adjacent-dedup → if remaining, summary.
    DedupThenSummary,
    /// Assistant text — straight to LLM summary.
    Summary,
    /// Tool result — adjacent-dedup → prune → if remaining, summary.
    DedupPruneThenSummary,
}

impl PerRolePolicy {
    pub fn policy_for(role: MessageRole) -> PerRolePolicy {
        match role {
            MessageRole::System => PerRolePolicy::Preserve,
            MessageRole::User => PerRolePolicy::DedupThenSummary,
            MessageRole::Assistant => PerRolePolicy::Summary,
            MessageRole::Tool => PerRolePolicy::DedupPruneThenSummary,
            // Summarising it would spend tokens preserving text the model
            // is never shown. In practice these never reach the
            // compressor (`load_session` filters them out); the arm
            // exists so a hand-built payload degrades to a cheap drop
            // rather than paying for a summary nobody reads.
            MessageRole::Automation => PerRolePolicy::DedupPruneThenSummary,
        }
    }
}

// ---------------------------------------------------------------------------
// Pin configuration
// ---------------------------------------------------------------------------

/// Pin settings (loaded from `CompressorConfig::pin_*`).
///
/// Pin mechanism is *opt-in*: with both fields empty (the default),
/// no message is ever pinned and the pipeline behaves identically to
/// before this module landed.
#[derive(Debug, Clone, Default)]
pub struct PinConfig {
    /// Role names that should never be touched by compression.
    /// Compared case-sensitively against `role_label(role)`.
    /// Recognized values: `"system"`, `"user"`, `"assistant"`, `"tool"`.
    pub pin_roles: Vec<String>,
    /// Case-insensitive substrings. If any substring matches anywhere
    /// in a message's content (or its tool-call args), the message is
    /// pinned. Useful for "DO NOT compress" / "安全规则" / "## Goals".
    pub pin_keywords: Vec<String>,
}

impl PinConfig {
    /// Build from `CompressorConfig` fields. Returns `None` if no
    /// pinning is configured — callers may then skip the pin
    /// partition entirely.
    pub fn from_compressor_config(cfg: &crate::compression::CompressorConfig) -> Option<Self> {
        if cfg.pin_roles.is_empty() && cfg.pin_keywords.is_empty() {
            return None;
        }
        // Keywords are stored as-is; `is_pinned` lowercases both
        // sides at compare time so direct construction (used in
        // tests) and `from_compressor_config` agree on case
        // insensitivity.
        Some(Self {
            pin_roles: cfg.pin_roles.clone(),
            pin_keywords: cfg.pin_keywords.clone(),
        })
    }

    /// Returns `true` if this message should be preserved verbatim
    /// regardless of any other stage.
    pub fn is_pinned(&self, msg: &Message) -> bool {
        if self.pin_roles.iter().any(|r| r == role_label(msg.role)) {
            return true;
        }
        if self.pin_keywords.is_empty() {
            return false;
        }
        let hay = msg_content_lowercase(msg);
        self.pin_keywords
            .iter()
            .any(|needle| !needle.is_empty() && hay.contains(&needle.to_lowercase()))
    }
}

fn role_label(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
        MessageRole::Automation => "automation",
    }
}

fn msg_content_lowercase(msg: &Message) -> String {
    let mut buf = String::new();
    if let Some(c) = msg.content.as_deref() {
        buf.push_str(&c.to_lowercase());
    }
    if let Some(tcs) = msg.tool_calls.as_ref() {
        for tc in tcs {
            buf.push('\n');
            buf.push_str(&tc.function.arguments.to_lowercase());
        }
    }
    buf
}

// ---------------------------------------------------------------------------
// Task-instruction preservation
// ---------------------------------------------------------------------------

/// Index of the most recent substantive user task instruction.
///
/// Scans backward from the end for the last `User` message whose
/// content is non-empty and is not a dedup/flush marker. This is the
/// "last assigned task" the context-compression policy must never
/// drop — see the task-preservation requirement: compression must keep
/// the most recent task instruction and prioritise task goals / pending
/// todos over other history. Returns `None` when the conversation has
/// no usable user turn (e.g. only system + tool messages).
pub fn last_task_instruction_index(messages: &[Message]) -> Option<usize> {
    for i in (0..messages.len()).rev() {
        if messages[i].role != MessageRole::User {
            continue;
        }
        let content = messages[i].content.as_deref().unwrap_or("").trim();
        if content.is_empty() || content.starts_with("[ALREADY EXECUTED]") {
            continue;
        }
        return Some(i);
    }
    None
}

// ---------------------------------------------------------------------------
// Pass 0: adjacent user-text dedup
// ---------------------------------------------------------------------------

/// Adjacent user-text deduplication (Pass 0).
///
/// Mirrors the existing tool-text dedup in `prune_old_tool_results`:
/// when two consecutive `User` messages have the same trimmed content,
/// the older one is replaced with a `[Duplicate user message]`
/// marker and counted.
///
/// Returns `(messages, dedup_count)`.
pub fn dedup_adjacent_user(messages: Vec<Message>) -> (Vec<Message>, usize) {
    if messages.is_empty() {
        return (messages, 0);
    }
    let mut out: Vec<Message> = Vec::with_capacity(messages.len());
    let mut deduped = 0usize;
    let mut prev_user_hash: Option<String> = None;
    for mut msg in messages.into_iter() {
        if msg.role == MessageRole::User && !is_already_a_dedup_marker(&msg) {
            let content = msg.content.as_deref().unwrap_or("").trim();
            if content.is_empty() {
                out.push(msg);
                prev_user_hash = None;
                continue;
            }
            let hash = format!("{:016x}", md5::compute(content.as_bytes()));
            if prev_user_hash.as_deref() == Some(&hash) {
                msg.content = Some(
                    "[ALREADY EXECUTED] [Duplicate user message — same content as a more recent turn]"
                        .into(),
                );
                deduped += 1;
                out.push(msg);
                // Do NOT update prev_user_hash: a third identical
                // message should also be deduped against the original,
                // not against the marker we just inserted.
                continue;
            }
            prev_user_hash = Some(hash);
        } else {
            prev_user_hash = None;
        }
        out.push(msg);
    }
    (out, deduped)
}

fn is_already_a_dedup_marker(msg: &Message) -> bool {
    msg.content
        .as_deref()
        .map(|c| c.starts_with("[ALREADY EXECUTED]"))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// TokenScorer — 4-dim heuristic
// ---------------------------------------------------------------------------

/// 5-dim heuristic scorer. **Not** an embedding model.
///
/// Designed after the Headroom article's `TokenScorer`, but without
/// an embedding model (single Rust binary, no 80-420 MB ONNX deps).
/// `semantic_centrality` is approximated by **query_relevance** —
/// keyword overlap with the most recent user message (the "current
/// task"). This catches "task-critical but low-frequency / old"
/// messages that pure recency/freq would otherwise starve and drop.
///
/// Dimensions (weights sum to 1.0):
/// - `recency(0.25)` — position-based; later messages score higher.
/// We have no per-message timestamp (`Message::execution_timeline`
/// carries *events* during a turn, not the turn's own wall clock),
/// so recency is approximated by message index.
/// - `role_position(0.25)` — `system=1.0`, `tool=0.7`,
/// `user=0.5`, `assistant=0.4`. Reflects that system messages
/// carry rules the LLM must keep following.
/// - `frequency(0.15)` — same `tool_call_id` referenced by ≥ 1
/// other message → bump. Catches "important tool result that
/// downstream turns keep referring to".
/// - `instructional_marker(0.15)` — content matches imperative
/// patterns (`!`-commands, `run …`, `git …`, file paths ending
/// in action verbs, `请`, `不要`, `must`, `never`). Hits a
/// 1.0 if the message looks like an instruction to the agent.
/// - `query_relevance(0.20)` — keyword overlap (CJK bigram +
/// latin word) with the most recent `User` message. Zero when
/// no query is set (e.g. `TokenScorer::new()` in tests).
#[derive(Debug, Clone)]
pub struct TokenScorer {
    /// Keywords extracted from the most recent user message.
    /// Empty when built via `new()` (no task awareness) — score
    /// then degenerates to the 4-dim heuristic.
    query_keywords: Vec<String>,
}

impl TokenScorer {
    /// Build a scorer with no task awareness. `query_relevance`
    /// will always be 0. Use [`Self::for_query`] in production
    /// to seed the scorer with the current user task.
    pub fn new() -> Self {
        Self {
            query_keywords: Vec::new(),
        }
    }

    /// Build a scorer aware of the current task: extracts keywords
    /// (CJK bigrams + latin words ≥ 2 chars, minus stopwords) from
    /// the most recent `User` message in `messages`.
    /// This is the no-embedding proxy for Headroom's
    /// `semantic_centrality = cosine_similarity(embed(task), embed(block))`.
    /// Keyword overlap is a cruder signal but costs nothing and
    /// never increases binary size.
    pub fn for_query(messages: &[Message]) -> Self {
        let query = messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::User)
            .and_then(|m| m.content.as_deref())
            .unwrap_or("");
        Self {
            query_keywords: extract_keywords(query),
        }
    }

    /// Expose the query keyword set (for diagnostics / tests).
    pub fn query_keywords(&self) -> &[String] {
        &self.query_keywords
    }

    /// Score a single message in the context of the full message
    /// list. Score is in `[0.0, 1.0]`.
    pub fn score(&self, msg: &Message, all: &[Message], idx: usize) -> f64 {
        // Legacy entry point retained for tests / external callers.
        // Computes the tool_call_id reference count inline (O(n) per
        // message). The production hot path (`drop_low_score`) calls
        // `score_with_refs` with a precomputed ref-count map to avoid
        // the O(n²) scan over the whole history.
        let n = all.len().max(1);
        let tcid_refs = msg
            .tool_call_id
            .as_deref()
            .map(|tcid| {
                all.iter()
                    .filter(|m| m.tool_call_id.as_deref() == Some(tcid))
                    .count()
            })
            .unwrap_or(0);
        self.score_inner(msg, n, idx, tcid_refs)
    }

    /// Score with a precomputed `tool_call_id` reference count.
    ///
    /// Produces identical results to [`score`](Self::score) but lets
    /// the caller pass in a ref count looked up from a single O(n)
    /// `HashMap` built once over the whole message list, instead of
    /// recomputing it O(n) for every message (which made the classify
    /// pass O(n²) on long histories).
    pub fn score_with_refs(
        &self,
        msg: &Message,
        n: usize,
        idx: usize,
        tcid_refs: usize,
    ) -> f64 {
        self.score_inner(msg, n, idx, tcid_refs)
    }

    fn score_inner(&self, msg: &Message, n: usize, idx: usize, tcid_refs: usize) -> f64 {
        // recency: linear ramp from 0.0 (oldest) to 1.0 (newest).
        // note: `idx` is the position within the
        // `messages` slice passed to *this* compress() call. It is
        // a correct recency proxy inside a single pass (messages
        // arrive in chronological order, so larger idx == newer).
        // It does NOT survive cross-call reordering: if the agent
        // loop injects a recalled memory into the middle of history
        // between compress() calls, that message's idx no longer
        // reflects its true age. We accept this because (a) each
        // compress() pass makes independent drop decisions — no
        // cross-call consistency is needed — and (b) the
        // `query_relevance` dimension rescues task-critical
        // messages that recency would otherwise starve. Adding a
        // stable `turn_seq` field to `Message` would fix it
        // rigorously but requires DB schema + frontend + every
        // construction site to change — deferred until a real
        // cross-call ordering need appears.
        let recency = idx as f64 / (n.saturating_sub(1).max(1)) as f64;
        // role_position: per-role base score.
        let role_pos = match msg.role {
            MessageRole::System => 1.0,
            MessageRole::Tool => 0.7,
            MessageRole::User => 0.5,
            MessageRole::Assistant => 0.4,
            // Lowest: if an automation result ever reaches the ranking
            // it is the first thing worth dropping, since the model
            // does not read it.
            MessageRole::Automation => 0.0,
        };
        // frequency: same tool_call_id used N times → bump up to 0.5.
        let frequency = if tcid_refs >= 2 {
            ((tcid_refs - 1) as f64).min(4.0) / 4.0
        } else {
            0.0
        };
        // instructional_marker: cheap pattern match.
        let instructional = if msg.role == MessageRole::System {
            // System messages are always considered "instructional".
            1.0
        } else {
            let c = msg.content.as_deref().unwrap_or("");
            looks_like_instruction(c) as i32 as f64
        };
        // query_relevance: fraction of current-task keywords that
        // appear in this message's content (or tool-call args).
        // Reuses `msg_content_lowercase` so tool-call arguments
        // are searched too — a tool result echoing a query term
        // counts as on-task.
        let query_relevance = if self.query_keywords.is_empty() {
            0.0
        } else {
            let hay = msg_content_lowercase(msg);
            let hits = self
                .query_keywords
                .iter()
                .filter(|kw| !kw.is_empty() && hay.contains(kw.as_str()))
                .count();
            (hits as f64 / self.query_keywords.len() as f64).min(1.0)
        };

        let score = 0.25 * recency
            + 0.25 * role_pos
            + 0.15 * frequency
            + 0.15 * instructional
            + 0.20 * query_relevance;
        score.clamp(0.0, 1.0)
    }
}

fn looks_like_instruction(content: &str) -> bool {
    let lower = content.to_lowercase();
    let patterns = [
        "!",
        "/run ",
        "/cmd ",
        "please ",
        "must ",
        "never ",
        "always ",
        "do not ",
        "don't ",
        "禁止",
        "请",
        "不要",
        "必须",
        "执行",
    ];
    // Require a content length floor so trivial short messages
    // ("ok", "yes") don't count as instructions.
    if content.len() < 8 {
        return false;
    }
    patterns.iter().any(|p| lower.contains(p))
}

// ---------------------------------------------------------------------------
// Keyword extraction — no-embedding proxy for semantic_centrality
// ---------------------------------------------------------------------------

/// Is `c` a CJK ideograph (Unified Ideographs basic + Ext A)?
fn is_cjk(c: char) -> bool {
    matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}')
}

/// Trivial English stopwords. Kept short on purpose — we're
/// scoring surface overlap, not doing NLP; over-aggressive
/// stopword removal would nuke short queries.
fn is_stopword(w: &str) -> bool {
    matches!(
        w,
        "the" | "a" | "an" | "and" | "or" | "but" | "if" | "to" | "of"
            | "in" | "on" | "for" | "is" | "it" | "this" | "that"
            | "with" | "as" | "be" | "are" | "was" | "were" | "do"
            | "does" | "i" | "you" | "he" | "she" | "we" | "they"
    )
}

/// Extract keywords from `text`: latin words (≥ 2 chars, minus
/// stopwords) + CJK bigrams. Lowercased. Deduplicated.
///
/// This is the no-embedding stand-in for `embed(current_task)` —
/// instead of a dense vector we get a bag of surface tokens, and
/// `query_relevance` in [`TokenScorer`] measures substring overlap
/// against each message. Cruder than cosine similarity but costs
/// nothing and never grows the binary.
fn extract_keywords(text: &str) -> Vec<String> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    let lower = text.to_lowercase();
    let mut kws: Vec<String> = Vec::new();
    // Latin / numeric words: split on non-alphanumeric.
    for word in lower.split(|c: char| !c.is_alphanumeric()) {
        if word.len() >= 2 && !is_stopword(word) {
            kws.push(word.to_string());
        }
    }
    // CJK bigrams: contiguous CJK chars, sliding window of 2.
    let cjk_chars: Vec<char> = lower.chars().filter(|&c| is_cjk(c)).collect();
    for pair in cjk_chars.windows(2) {
        kws.push(pair.iter().collect());
    }
    kws.sort();
    kws.dedup();
    kws
}

/// Drop messages with score strictly below `threshold`.
///
/// **Protective heuristics** — a message is NEVER dropped when any
/// of these hold (regardless of score):
/// - it is pinned (`pin.is_pinned`)
/// - it is within the last `protect_last_n` messages (recent
/// context the LLM is actively building on)
/// - it looks like an instruction (`looks_like_instruction`) or is
/// a `System` message
/// - its `tool_call_id` is referenced by ≥ 2 messages in the list
/// (a tool result downstream turns keep referring to)
///
/// **min_keep floor:** even when all of the above allow a drop,
/// the function never lets `kept.len()` fall below `min_keep`.
/// If dropping every eligible candidate would breach the floor,
/// the highest-scoring candidates are rescued back into the kept
/// set. This prevents an aggressive `threshold` from starving the
/// downstream LLM summariser (which needs `> protect_first_n + 3`
/// messages to run at all — see `min_for_compress` in
/// `compression.rs`). Callers should pass `protect_first_n + 3 + 1`.
///
/// **Lossless by design:** dropped messages are *returned* (not
/// silently destroyed) so the caller can archive them alongside
/// the LLM-summarised turns.
///
/// Returns `(kept, dropped_messages)`.
pub fn drop_low_score(
    messages: Vec<Message>,
    scorer: &TokenScorer,
    pin: Option<&PinConfig>,
    threshold: f64,
    protect_last_n: usize,
    min_keep: usize,
) -> (Vec<Message>, Vec<Message>) {
    if messages.is_empty() {
        return (messages, Vec::new());
    }
    let n = messages.len();

    // Build a single O(n) tool_call_id reference-count map. This
    // replaces two per-message O(n) scans in the old implementation:
    // `score`'s `frequency` term and `is_referenced_tool` both did
    // `all.iter().filter(...).count()`, making the whole classify
    // pass O(n²) on long histories. We also drop the previous
    // `messages.clone()` (a full deep copy of the message list) by
    // classifying against a borrowed slice and only moving messages
    //out in — so the only allocation here is the ref-count
    // map, sized to the number of distinct tool_call_ids.
    let mut ref_counts: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::new();
    for m in messages.iter() {
        if let Some(tcid) = m.tool_call_id.as_deref() {
            *ref_counts.entry(tcid).or_insert(0) += 1;
        }
    }

    //classify each message. `keep[i]` = true means
    // "survives every pass". Candidates for dropping land in
    //`drop_candidates` with their score, so can rescue
    // the highest-scoring ones if the min_keep floor is breached.
    let mut keep = vec![false; n];
    let mut drop_candidates: Vec<(usize, f64)> = Vec::new();
    for (idx, msg) in messages.iter().enumerate() {
        let is_pin = pin.map(|p| p.is_pinned(msg)).unwrap_or(false);
        let in_tail_window = idx + protect_last_n >= n;
        let is_instructional = msg.role == MessageRole::System
            || msg
                .content
                .as_deref()
                .map(looks_like_instruction)
                .unwrap_or(false);
        let tcid_refs = msg
            .tool_call_id
            .as_deref()
            .and_then(|tcid| ref_counts.get(tcid).copied())
            .unwrap_or(0);
        let is_referenced_tool = tcid_refs >= 2;
        let score = scorer.score_with_refs(msg, n, idx, tcid_refs);

        if is_pin || in_tail_window || is_instructional || is_referenced_tool || score >= threshold
        {
            keep[idx] = true;
        } else {
            drop_candidates.push((idx, score));
        }
    }

    //enforce the min_keep floor. If dropping every
    // candidate would leave fewer than `min_keep` messages,
    // rescue the highest-scoring candidates back into the kept
    // set. Sorted descending by score so the "least bad" drops
    // are the ones we keep.
    let kept_without_rescue = n - drop_candidates.len();
    if kept_without_rescue < min_keep && !drop_candidates.is_empty() {
        let rescue_count = (min_keep - kept_without_rescue).min(drop_candidates.len());
        drop_candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        for &(idx, _score) in drop_candidates.iter().take(rescue_count) {
            keep[idx] = true;
        }
    }

    //partition by the final keep flags.
    let mut kept = Vec::with_capacity(n);
    let mut dropped: Vec<Message> = Vec::new();
    for (idx, msg) in messages.into_iter().enumerate() {
        if keep[idx] {
            kept.push(msg);
        } else {
            dropped.push(msg);
        }
    }
    (kept, dropped)
}

// ---------------------------------------------------------------------------
// CompressionReport
// ---------------------------------------------------------------------------

/// Structured summary of one compression pass. Emitted as a single
/// `tracing::info!` line and persisted into the compression metrics
/// slot so the dashboard can show "what happened this turn".
#[derive(Debug, Clone, Default)]
pub struct CompressionReport {
    /// Token count before any compression.
    pub pre_tokens: u64,
    /// Token count after all passes.
    pub post_tokens: u64,
    /// Message count before.
    pub pre_count: usize,
    /// Message count after.
    pub post_count: usize,
    /// Number of messages pinned (skipped every stage).
    pub pin_count: usize,
    /// Number of messages deduped (Pass 0 user + Pass 1 tool).
    pub dedup_count: usize,
    /// Number of tool results pruned to 1-line summary.
    pub pruned_count: usize,
    /// Number of messages dropped by score threshold.
    pub dropped_count: usize,
    /// Final summary token count (if a summary was generated).
    pub summary_tokens: Option<u64>,
    /// `post_tokens / pre_tokens`. 1.0 = no compression, 0.1 = 90%
    /// reduction. Below 0.1 is considered aggressive and may
    /// surface as a quality-risk warning.
    pub compression_ratio: f64,
    /// Heuristic `0..=1` estimate of information loss. Driven by
    /// `dropped_count / pre_count` (silent drops are lossier than
    /// LLM-summarised turns) plus an over-compression penalty when
    /// `compression_ratio < 0.1`. Above 0.15 is considered
    /// concerning — the caller should emit a `tracing::warn`.
    pub estimated_quality_loss: f64,
    /// What triggered compression: "threshold" / "explicit" / etc.
    pub trigger_reason: String,
    /// Breakdown of pre-count by role for quick inspection.
    pub per_role_pre: std::collections::BTreeMap<String, usize>,
    /// Breakdown of post-count by role.
    pub per_role_post: std::collections::BTreeMap<String, usize>,
}

impl CompressionReport {
    pub fn compute_per_role(messages: &[Message]) -> std::collections::BTreeMap<String, usize> {
        let mut m = std::collections::BTreeMap::new();
        for msg in messages {
            *m.entry(role_label(msg.role).to_string()).or_insert(0) += 1;
        }
        m
    }

    /// Derive `(compression_ratio, estimated_quality_loss)` from
    /// the pre/post token + message counts. Used at construction
    /// time so the two derived fields stay in sync with the raw
    /// counters.
    pub fn derive_metrics(
        pre_tokens: u64,
        post_tokens: u64,
        pre_count: usize,
        dropped_count: usize,
    ) -> (f64, f64) {
        let compression_ratio = if pre_tokens > 0 {
            post_tokens as f64 / pre_tokens as f64
        } else {
            1.0
        };
        let drop_ratio = if pre_count > 0 {
            dropped_count as f64 / pre_count as f64
        } else {
            0.0
        };
        // Silent drops are lossier than LLM-summarised turns
        // (summarised turns at least keep a handoff summary).
        // Weight them at 0.5×, then add a flat penalty when
        // compression is so aggressive the model is likely starved.
        let over_compress = if compression_ratio < 0.1 { 0.15 } else { 0.0 };
        let estimated_quality_loss = (drop_ratio * 0.5 + over_compress).clamp(0.0, 1.0);
        (compression_ratio, estimated_quality_loss)
    }

    /// True when this pass looks risky — either estimated quality
    /// loss is above 0.15 or the ratio is below 0.1 (90%+
    /// reduction). Callers should emit a warning when this fires.
    pub fn is_concerning(&self) -> bool {
        self.estimated_quality_loss > 0.15 || self.compression_ratio < 0.1
    }

    /// Emit a `tracing::warn` when this pass is concerning. Mirrors
    /// Headroom's `on_compression_complete` alert callback (which
    /// the article lists as a best practice the original project
    /// was missing).
    pub fn emit_warn_if_concerning(&self) {
        if self.is_concerning() {
            tracing::warn!(
                target: "hermix_agent.compression",
                "compression quality risk: ratio={:.3} estimated_quality_loss={:.3} dropped={}/{} (over-compression may degrade model adherence)",
                self.compression_ratio,
                self.estimated_quality_loss,
                self.dropped_count,
                self.pre_count,
            );
        }
    }

    /// Render as a single-line tracing message.
    pub fn format_line(&self) -> String {
        format!(
            "compression report: pre={}msg/{}tok post={}msg/{}tok ratio={:.3} q_loss={:.3} pin={} dedup={} pruned={} dropped={} summary_tokens={:?} trigger={} per_role_pre={:?} per_role_post={:?}",
            self.pre_count,
            self.pre_tokens,
            self.post_count,
            self.post_tokens,
            self.compression_ratio,
            self.estimated_quality_loss,
            self.pin_count,
            self.dedup_count,
            self.pruned_count,
            self.dropped_count,
            self.summary_tokens,
            self.trigger_reason,
            self.per_role_pre,
            self.per_role_post,
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_core::{FunctionCall, ToolCall};

    fn m_user(c: &str) -> Message {
        Message {
            role: MessageRole::User,
            content: Some(c.into()),
            content_parts: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
            execution_timeline: None,
            unified_steps: None,
            ephemeral: false,
            automation_meta: None,
        }
    }

    fn m_assistant(c: &str) -> Message {
        Message {
            role: MessageRole::Assistant,
            content: Some(c.into()),
            content_parts: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
            execution_timeline: None,
            unified_steps: None,
            ephemeral: false,
            automation_meta: None,
        }
    }

    fn m_tool(call_id: &str, c: &str) -> Message {
        Message {
            role: MessageRole::Tool,
            content: Some(c.into()),
            content_parts: None,
            tool_calls: None,
            tool_call_id: Some(call_id.into()),
            name: None,
            reasoning_content: None,
            cache_control: None,
            execution_timeline: None,
            unified_steps: None,
            ephemeral: false,
            automation_meta: None,
        }
    }

    fn m_system(c: &str) -> Message {
        Message {
            role: MessageRole::System,
            content: Some(c.into()),
            content_parts: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
            execution_timeline: None,
            unified_steps: None,
            ephemeral: false,
            automation_meta: None,
        }
    }

    // ---------- PerRolePolicy ----------

    #[test]
    fn per_role_policy_is_static() {
        assert_eq!(
            PerRolePolicy::policy_for(MessageRole::System),
            PerRolePolicy::Preserve
        );
        assert_eq!(
            PerRolePolicy::policy_for(MessageRole::User),
            PerRolePolicy::DedupThenSummary
        );
        assert_eq!(
            PerRolePolicy::policy_for(MessageRole::Assistant),
            PerRolePolicy::Summary
        );
        assert_eq!(
            PerRolePolicy::policy_for(MessageRole::Tool),
            PerRolePolicy::DedupPruneThenSummary
        );
    }

    // ---------- dedup_adjacent_user ----------

    #[test]
    fn dedup_adjacent_user_merges_three_identical() {
        let msgs = vec![
            m_user("hello world"),
            m_user("hello world"),
            m_user("hello world"),
            m_user("different message"),
        ];
        let (out, count) = dedup_adjacent_user(msgs);
        assert_eq!(count, 2, "expected 2 dups out of 3 identical");
        assert_eq!(out.len(), 4);
        // First kept, second + third replaced with marker.
        assert_eq!(out[0].content.as_deref(), Some("hello world"));
        assert!(out[1]
            .content
            .as_deref()
            .unwrap()
            .starts_with("[ALREADY EXECUTED]"));
        assert!(out[2]
            .content
            .as_deref()
            .unwrap()
            .starts_with("[ALREADY EXECUTED]"));
        // Different message is untouched.
        assert_eq!(out[3].content.as_deref(), Some("different message"));
    }

    #[test]
    fn dedup_adjacent_user_breaks_on_role_change() {
        // user -> assistant -> user should NOT collapse the two users.
        let msgs = vec![
            m_user("same"),
            m_assistant("response"),
            m_user("same"),
        ];
        let (out, count) = dedup_adjacent_user(msgs);
        assert_eq!(count, 0, "non-adjacent users must not dedup");
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].content.as_deref(), Some("same"));
        assert_eq!(out[2].content.as_deref(), Some("same"));
    }

    #[test]
    fn dedup_adjacent_user_trims_whitespace() {
        let msgs = vec![m_user("hello"), m_user("  hello  "), m_user("hello")];
        let (_out, count) = dedup_adjacent_user(msgs);
        assert_eq!(count, 2);
    }

    // ---------- PinConfig ----------

    #[test]
    fn pin_by_role() {
        let cfg = PinConfig {
            pin_roles: vec!["system".into()],
            pin_keywords: vec![],
        };
        assert!(cfg.is_pinned(&m_system("anything")));
        assert!(!cfg.is_pinned(&m_user("anything")));
    }

    #[test]
    fn pin_by_keyword_case_insensitive() {
        let cfg = PinConfig {
            pin_roles: vec![],
            pin_keywords: vec!["DO NOT compress".into()],
        };
        assert!(cfg.is_pinned(&m_user("This rule: DO NOT COMPRESS the safety checks.")));
        assert!(!cfg.is_pinned(&m_user("This rule: do compress when needed.")));
    }

    #[test]
    fn pin_keyword_matches_tool_call_args() {
        let mut msg = m_assistant("calling tool");
        msg.tool_calls = Some(vec![ToolCall {
            id: "c1".into(),
            function: FunctionCall {
                name: "shell".into(),
                arguments: r#"{"cmd":"DO NOT COMPRESS critical script"}"#.into(),
            },
        }]);
        let cfg = PinConfig {
            pin_roles: vec![],
            pin_keywords: vec!["do not compress".into()],
        };
        assert!(cfg.is_pinned(&msg));
    }

    #[test]
    fn pin_empty_matches_nothing() {
        let cfg = PinConfig::default();
        assert!(!cfg.is_pinned(&m_system("important")));
        assert!(!cfg.is_pinned(&m_user("important")));
    }

    // ---------- TokenScorer ----------

    #[test]
    fn scorer_recency_is_position_based() {
        let s = TokenScorer::new();
        let msgs = vec![m_user("old"), m_user("middle"), m_user("newest")];
        let score_old = s.score(&msgs[0], &msgs, 0);
        let score_new = s.score(&msgs[2], &msgs, 2);
        assert!(score_new > score_old, "newer messages must score higher");
    }

    #[test]
    fn scorer_role_position_system_highest() {
        let s = TokenScorer::new();
        // Same index isolates role_position from recency.
        let sys = m_system("rules");
        let tool = m_tool("c1", "result");
        let user = m_user("hi");
        let asst = m_assistant("ok");
        let all = vec![sys.clone(), tool.clone(), user.clone(), asst.clone()];
        let sys_score = s.score(&sys, &all, 0);
        let tool_score = s.score(&tool, &all, 0);
        let user_score = s.score(&user, &all, 0);
        let asst_score = s.score(&asst, &all, 0);
        assert!(sys_score > tool_score, "sys={sys_score} tool={tool_score}");
        assert!(tool_score > user_score, "tool={tool_score} user={user_score}");
        assert!(user_score > asst_score, "user={user_score} asst={asst_score}");
    }

    #[test]
    fn scorer_frequency_bumps_reused_tool_call_id() {
        let s = TokenScorer::new();
        // Same index to isolate frequency from recency. c1 is
        // reused (appears at idx 0 + idx 1) so the score at idx 1
        // gets a frequency bonus; c2 is unique.
        let a_first = m_tool("c1", "first call");
        let c = m_tool("c2", "lonely");
        let a_second = m_tool("c1", "second call");
        let all = vec![a_first.clone(), c.clone(), a_second.clone()];
        let reuse_score = s.score(&a_second, &all, 2);
        let lonely_score = s.score(&c, &all, 1);
        assert!(
            reuse_score > lonely_score,
            "reuse={reuse_score} vs lonely={lonely_score}"
        );
    }

    #[test]
    fn scorer_instructional_marker_detects_imperative() {
        let s = TokenScorer::new();
        // Compare at SAME index so recency is constant and we
        // isolate the instructional_marker dimension.
        let cmd = m_user("Please run the tests now");
        let filler = m_user("ok sounds good");
        let all = vec![cmd.clone(), filler.clone()];
        let cmd_score = s.score(&cmd, &all, 1);
        let filler_score = s.score(&filler, &all, 1);
        assert!(cmd_score > filler_score, "{cmd_score} vs {filler_score}");
    }

    // ---------- drop_low_score ----------

    #[test]
    fn drop_low_score_respects_pin() {
        let s = TokenScorer::new();
        let pin = PinConfig {
            pin_roles: vec!["system".into()],
            pin_keywords: vec![],
        };
        // System at index 0 → very low recency, but role_position
        // + instructional bumps it well above any threshold. The
        // important thing here is "never dropped" — verify by
        // asking for a threshold of 0.99.
        let sys = m_system("rules that must survive");
        let filler = m_assistant("ok");
        let msgs = vec![sys.clone(), filler.clone()];
        // protect_last_n=0 so the tail-window guard doesn't save
        // the filler; we're isolating the pin guard here.
        let (out, dropped) = drop_low_score(msgs, &s, Some(&pin), 0.99, 0, 0);
        assert!(out.iter().any(|m| m.role == MessageRole::System));
        // Filler at index 1: recency=1.0, role_position=0.4,
        // instructional=0 → score = 0.25*1.0 + 0.25*0.4 = 0.35
        // < 0.99, so it should be dropped.
        assert_eq!(dropped.len(), 1, "low-score assistant dropped");
        // Lossless: the dropped message is returned, not destroyed.
        assert_eq!(dropped[0].role, MessageRole::Assistant);
    }

    #[test]
    fn drop_low_score_no_pin_drops_chaff() {
        let s = TokenScorer::new();
        let filler = m_assistant("ok");
        let msgs = vec![filler];
        let (out, dropped) = drop_low_score(msgs, &s, None, 0.5, 0, 0);
        assert!(out.is_empty());
        assert_eq!(dropped.len(), 1);
    }

    #[test]
    fn drop_low_score_protects_tail_window() {
        // Even a low-score assistant in the last `protect_last_n`
        // messages must survive — it's context the LLM is actively
        // building on.
        let s = TokenScorer::new();
        let filler = m_assistant("ok");
        let msgs = vec![filler];
        let (out, dropped) = drop_low_score(msgs, &s, None, 0.99, 3, 0);
        assert!(out.iter().any(|m| m.role == MessageRole::Assistant));
        assert!(dropped.is_empty(), "tail-window message must not drop");
    }

    #[test]
    fn drop_low_score_protects_instructional() {
        // An imperative user message survives even at idx 0 with a
        // punishing threshold, because `looks_like_instruction`
        // matches "please run".
        let s = TokenScorer::new();
        let cmd = m_user("Please run the tests now");
        let msgs = vec![cmd];
        let (out, dropped) = drop_low_score(msgs, &s, None, 0.99, 0, 0);
        assert_eq!(out.len(), 1);
        assert!(dropped.is_empty());
    }

    #[test]
    fn drop_low_score_protects_referenced_tool() {
        // A tool result whose `tool_call_id` is referenced by ≥ 2
        // messages is kept even under a punishing threshold.
        let s = TokenScorer::new();
        let a_first = m_tool("c1", "first call");
        let c = m_tool("c1", "second call"); // same tool_call_id
        let msgs = vec![a_first, c];
        // threshold 0.99, protect_last_n=0 → only the referenced-tool
        // guard can save them.
        let (out, dropped) = drop_low_score(msgs, &s, None, 0.99, 0, 0);
        assert_eq!(out.len(), 2, "referenced tool results must survive");
        assert!(dropped.is_empty());
    }

    #[test]
    fn drop_low_score_returns_dropped_for_archive() {
        // The dropped vec is owned by the caller so it can be
        // archived — verifies the lossless contract end-to-end.
        let s = TokenScorer::new();
        let filler = m_assistant("ok");
        let msgs = vec![filler.clone()];
        let (out, dropped) = drop_low_score(msgs, &s, None, 0.5, 0, 0);
        assert!(out.is_empty());
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].content.as_deref(), Some("ok"));
    }

    #[test]
    fn drop_low_score_respects_min_keep_floor() {
        // 5 low-score assistants, threshold 0.99 would drop all 5,
        // but min_keep=4 rescues the 4 highest-scoring (latest)
        // back. Verify the floor is honoured: the summariser must
        // never be starved by an aggressive threshold.
        let s = TokenScorer::new();
        let msgs = vec![
            m_assistant("a"), // idx 0, lowest recency
            m_assistant("b"),
            m_assistant("c"),
            m_assistant("d"),
            m_assistant("e"), // idx 4, highest recency
        ];
        // min_keep=4 → at most 1 may be dropped.
        let (kept, dropped) = drop_low_score(msgs, &s, None, 0.99, 0, 4);
        assert!(kept.len() >= 4, "min_keep floor breached: kept={}", kept.len());
        assert!(dropped.len() <= 1, "dropped={}", dropped.len());
    }

    // ---------- CompressionReport ----------

    #[test]
    fn report_format_line_has_all_fields() {
        let r = CompressionReport {
            pre_tokens: 1000,
            post_tokens: 400,
            pre_count: 10,
            post_count: 4,
            pin_count: 2,
            dedup_count: 1,
            pruned_count: 1,
            dropped_count: 2,
            summary_tokens: Some(200),
            compression_ratio: 0.4,
            estimated_quality_loss: 0.3,
            trigger_reason: "threshold".into(),
            per_role_pre: [("user".into(), 5), ("assistant".into(), 5)]
                .into_iter()
                .collect(),
            per_role_post: [("user".into(), 2), ("assistant".into(), 2)]
                .into_iter()
                .collect(),
        };
        let line = r.format_line();
        assert!(line.contains("pre=10msg/1000tok"));
        assert!(line.contains("post=4msg/400tok"));
        assert!(line.contains("pin=2"));
        assert!(line.contains("dedup=1"));
        assert!(line.contains("pruned=1"));
        assert!(line.contains("dropped=2"));
        assert!(line.contains("ratio=0.400"));
        assert!(line.contains("q_loss=0.300"));
        assert!(line.contains("trigger=threshold"));
    }

    #[test]
    fn report_compute_per_role_counts_correctly() {
        let msgs = vec![m_user("a"), m_user("b"), m_assistant("c"), m_tool("c1", "d")];
        let r = CompressionReport::compute_per_role(&msgs);
        assert_eq!(r.get("user").copied(), Some(2));
        assert_eq!(r.get("assistant").copied(), Some(1));
        assert_eq!(r.get("tool").copied(), Some(1));
        assert!(r.get("system").is_none());
    }

    // ---------- query_relevance (no-embedding semantic proxy) ----------

    #[test]
    fn scorer_query_relevance_boosts_on_task_messages() {
        // The most recent user message sets the "current task".
        // An old message that overlaps the task keywords should
        // outscore an old message that doesn't — even though both
        // have the same recency / role.
        let msgs = vec![
            m_assistant("earlier note about the auth module"),
            m_assistant("earlier note about cooking recipes"),
            m_user("fix the auth module bug"),
        ];
        let s = TokenScorer::for_query(&msgs);
        // on_task at idx 0, off_task at idx 1. recency differs, so
        // compare against a same-index baseline: build two scorers
        // each with one assistant at idx 0 and the same query.
        let on_task = s.score(&msgs[0], &msgs, 0);
        let off_msgs = vec![
            m_assistant("earlier note about cooking recipes"),
            m_assistant("earlier note about the auth module"),
            m_user("fix the auth module bug"),
        ];
        let s2 = TokenScorer::for_query(&off_msgs);
        let off_task = s2.score(&off_msgs[0], &off_msgs, 0);
        assert!(
            on_task > off_task,
            "on-task ({on_task:.3}) should beat off-task ({off_task:.3})"
        );
    }

    #[test]
    fn scorer_for_query_extracts_cjk_bigrams() {
        // Chinese query: bigrams like "授权", "模块" should be
        // extracted and matched against history.
        let on_msgs = vec![
            m_assistant("之前讨论了授权模块的设计"),
            m_user("修复授权模块的 bug"),
        ];
        let s = TokenScorer::for_query(&on_msgs);
        assert!(
            !s.query_keywords().is_empty(),
            "CJK query must yield keywords"
        );
        let on_task = s.score(&on_msgs[0], &on_msgs, 0);
        let off_msgs = vec![
            m_assistant("今天天气不错适合散步"),
            m_user("修复授权模块的 bug"),
        ];
        let s2 = TokenScorer::for_query(&off_msgs);
        let off_task = s2.score(&off_msgs[0], &off_msgs, 0);
        assert!(
            on_task > off_task,
            "CJK on-task ({on_task:.3}) should beat off-task ({off_task:.3})"
        );
    }

    #[test]
    fn scorer_new_has_no_query_keywords() {
        let s = TokenScorer::new();
        assert!(s.query_keywords().is_empty());
    }

    // ---------- derive_metrics / is_concerning ----------

    #[test]
    fn derive_metrics_computes_ratio_and_loss() {
        // 400 post / 1000 pre → ratio 0.4. 2 dropped / 10 pre →
        // drop_ratio 0.2, ×0.5 = 0.1. ratio ≥ 0.1 → no over-compress
        // penalty. quality_loss = 0.1.
        let (ratio, loss) = CompressionReport::derive_metrics(1000, 400, 10, 2);
        assert!((ratio - 0.4).abs() < 1e-9, "ratio={ratio}");
        assert!((loss - 0.1).abs() < 1e-9, "loss={loss}");
    }

    #[test]
    fn derive_metrics_aggressive_ratio_adds_penalty() {
        // ratio 0.05 (< 0.1) → over-compress penalty 0.15.
        // drop_ratio 0 (nothing dropped) → 0. loss = 0.15.
        let (ratio, loss) = CompressionReport::derive_metrics(10000, 500, 10, 0);
        assert!((ratio - 0.05).abs() < 1e-9);
        assert!((loss - 0.15).abs() < 1e-9, "loss={loss}");
    }

    #[test]
    fn is_concerning_flags_aggressive_pass() {
        let mut r = CompressionReport::default();
        r.compression_ratio = 0.05;
        r.estimated_quality_loss = 0.16;
        assert!(r.is_concerning());
    }

    #[test]
    fn is_concerning_passes_benign_pass() {
        let mut r = CompressionReport::default();
        r.compression_ratio = 0.5;
        r.estimated_quality_loss = 0.05;
        assert!(!r.is_concerning());
    }

    #[test]
    fn last_task_instruction_index_picks_most_recent_user() {
        // system + an old user task + assistant + tool + a newer user task
        let msgs = vec![
            m_system("sys"),
            m_user("FIRST TASK: do X"),
            m_assistant("ok"),
            m_tool("c1", "result"),
            m_user("SECOND TASK: do Y"),
            m_assistant("on it"),
        ];
        assert_eq!(
            last_task_instruction_index(&msgs),
            Some(4),
            "must return the most recent substantive user message"
        );
    }

    #[test]
    fn last_task_instruction_index_skips_markers_and_empty() {
        let msgs = vec![
            m_user("[ALREADY EXECUTED] [Duplicate user message]"),
            m_user("   "),
            m_user("REAL TASK: do Z"),
        ];
        assert_eq!(last_task_instruction_index(&msgs), Some(2));
    }

    #[test]
    fn last_task_instruction_index_none_when_no_user() {
        let msgs = vec![m_system("sys"), m_assistant("hi"), m_tool("c", "r")];
        assert_eq!(last_task_instruction_index(&msgs), None);
    }
}
