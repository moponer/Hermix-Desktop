//! Compressor core: construction, breaker, snapshot, should_compress, collapse.
use std::sync::Arc;
use std::time::{Duration, Instant};

use hermix_core::Message;
use hermix_intelligence::auxiliary::AuxiliaryClient;

use super::*;

impl ContextCompressor {
    /// Create a compressor for `model` with the given configuration.
    /// Token budgets are derived eagerly so they don't change as the
    /// compressor is used. To re-target a different model, build a new
    /// instance.
    pub fn new(config: CompressorConfig, auxiliary: Arc<AuxiliaryClient>) -> Self {
        let summary_ratio = config.summary_target_ratio.clamp(0.10, 0.80);
        // 阈值取「窗口百分比」与「绝对上限」的较小值。
        //
        // 只按百分比算时，大窗口模型永远压不到：实测 MiniMax-M3.1-Flash 窗口
        // 1M，0.90 → 阈值 900K tokens，而真实会话在 66K 时就已经让每轮成本
        // 很高了（28 轮累计约 180 万 prompt tokens），却一次都不会压缩。
        // 加绝对上限后：小窗口模型仍由百分比主导（不受影响），大窗口模型在
        // 绝对上限处开始压缩——这才是「上下文成本显著」的时刻。
        let pct_threshold = (config.context_length as f64 * config.threshold_percent) as u64;
        let threshold_tokens = pct_threshold.min(config.threshold_tokens_ceiling);
        let target_tokens = (threshold_tokens as f64 * summary_ratio) as u64;
        let max_summary_tokens =
            ((config.context_length as f64 * 0.05) as u64).min(config.summary_tokens_ceiling);

        if !config.quiet_mode {
            tracing::info!(
                "Context compressor initialised: context_length={} threshold={} ({:.0}%) target_ratio={:.0}% tail_budget={}",
                config.context_length,
                threshold_tokens,
                config.threshold_percent * 100.0,
                summary_ratio * 100.0,
                target_tokens,
            );
        }

        Self {
            config: CompressorConfig {
                summary_target_ratio: summary_ratio,
                ..config
            },
            auxiliary,
            task_source: None,
            threshold_tokens,
            tail_token_budget: target_tokens,
            max_summary_tokens,
            compression_count: 0,
            last_prompt_tokens: 0,
            previous_summary: None,
            failure_cooldown_until: None,
            ineffective_compression_count: 0,
            last_metrics: CompressionMetrics::default(),
            // Per-compression counters:
            last_user_dedup_count: 0,
            last_pin_count: 0,
            last_trigger_reason: String::new(),
            last_compression_report: None,
            last_archived_turns: Vec::new(),
            last_dropped_turns: Vec::new(),
            fast_mode: false,
            // Day 4: circuit breaker starts closed (no failures yet).
            consecutive_llm_failures: 0,
            breaker_threshold: DEFAULT_BREAKER_THRESHOLD,
            breaker_cooldown: Duration::from_secs(DEFAULT_BREAKER_COOLDOWN_SECS),
            breaker_open_until: None,
            last_breaker_state: BreakerState::Closed,
            last_state_change_unix_ms: None,
        }
    }

    /// Total in-window threshold (tokens). Compression triggers above this.
    pub fn threshold_tokens(&self) -> u64 {
        self.threshold_tokens
    }

    /// Borrow the active `CompressorConfig` (used by callers that want
    /// to call [`CompressorConfig::validate`] before deciding whether
    /// to install this compressor).
    pub fn config(&self) -> &CompressorConfig {
        &self.config
    }

    /// Borrow the auxiliary LLM client (used by callers that want to
    /// call [`CompressorConfig::validate`] before deciding whether to
    /// install this compressor).
    pub fn auxiliary(&self) -> &AuxiliaryClient {
        &self.auxiliary
    }

    /// How many compactions this instance has performed.
    pub fn compression_count(&self) -> u64 {
        self.compression_count
    }

    /// Update the tracked usage from the latest response.
    pub fn update_from_usage(&mut self, prompt_tokens: u64) {
        self.last_prompt_tokens = prompt_tokens;
    }

    /// (A): raw `turns_to_summarize` slice from the most
    /// recent compression, captured *before* `compress()` mutates the
    /// conversation. `agent_loop.rs` reads this immediately after
    /// `compress()` returns and writes it to
    /// `compression_archives` via
    /// `SessionPersistence::archive_compressed_turns`.
    pub fn last_archived_turns(&self) -> &[Message] {
        &self.last_archived_turns
    }

    /// Messages dropped by 's `drop_low_score`. These are
    /// already folded into [`Self::last_archived_turns`] by the
    /// end of `compress()` (so the agent loop's single archive
    /// path picks them up). Exposed separately for diagnostics /
    /// dashboards that want to distinguish "summarised turns"
    /// from "silently dropped turns".
    pub fn last_dropped_turns(&self) -> &[Message] {
        &self.last_dropped_turns
    }

    /// Reset per-session state (call on /new or /reset).
    pub fn reset_session_state(&mut self) {
        self.compression_count = 0;
        self.last_prompt_tokens = 0;
        self.previous_summary = None;
        self.failure_cooldown_until = None;
        self.ineffective_compression_count = 0;
        self.last_metrics = CompressionMetrics::default();
        self.last_archived_turns.clear();
        self.last_dropped_turns.clear();
        self.fast_mode = false;
        // Day 4: also reset the breaker — a /new session starts with a
        // clean slate, not inheriting the previous session's failed LLM
        // attempts. This is conservative: if the underlying provider
        // is still broken, the breaker will trip again within 3 attempts.
        self.consecutive_llm_failures = 0;
        self.breaker_open_until = None;
        self.last_breaker_state = BreakerState::Closed;
    }

    // ------------------------------------------------------------------
    // Day 4: circuit-breaker API
    // ------------------------------------------------------------------

    /// Override the breaker threshold and cooldown. Wired by the agent
    /// loop from `hermix_config::CompactionConfig` after `new()`.
    /// Backward-compatible: callers that don't care about the breaker
    /// use the defaults ([`DEFAULT_BREAKER_THRESHOLD`] = 3,
    /// [`DEFAULT_BREAKER_COOLDOWN_SECS`] = 600).
    pub fn with_breaker_config(mut self, threshold: u32, cooldown_secs: u64) -> Self {
        if threshold > 0 {
            self.breaker_threshold = threshold;
        }
        if cooldown_secs > 0 {
            self.breaker_cooldown = Duration::from_secs(cooldown_secs);
        }
        self
    }

    /// Wire the session's task tree into the summary.
    ///
    /// Without this the compaction summary carries only the
    /// summariser's *recollection* of the task state, and after a
    /// compaction the model can lose track of which task it is on.
    pub fn with_task_source(
        mut self,
        registry: Arc<hermix_tools::ToolRegistry>,
        session_id: impl Into<String>,
    ) -> Self {
        self.task_source = Some(TaskTreeSource {
            registry,
            session_id: session_id.into(),
        });
        self
    }

    /// Current breaker state — drives the front-end `CompactionStatusBanner`.
    /// Recomputes the live state every call so a `CooldownElapsed` Open
    /// automatically shows as `HalfOpen` once the wall-clock passes.
    pub fn breaker_state(&mut self) -> BreakerState {
        let now = Instant::now();
        let live = match self.last_breaker_state {
            BreakerState::Open => {
                match self.breaker_open_until {
                    Some(deadline) if now >= deadline => {
                        // Cooldown elapsed → probe next attempt.
                        BreakerState::HalfOpen
                    }
                    _ => BreakerState::Open,
                }
            }
            other => other,
        };
        if live != self.last_breaker_state {
            self.last_breaker_state = live;
        }
        live
    }

    /// Borrow-only peek at the breaker state without recomputing. Use
    /// this when you need the state without holding a mutable borrow
    /// (e.g. inside a tracing event).
    pub fn breaker_state_peek(&self) -> BreakerState {
        self.last_breaker_state
    }

    /// Consecutive hard LLM-failure count (resets on first success).
    pub fn consecutive_llm_failures(&self) -> u32 {
        self.consecutive_llm_failures
    }

    /// Number of consecutive LLM failures that trip the breaker open.
    pub fn breaker_threshold(&self) -> u32 {
        self.breaker_threshold
    }

    /// `true` if the breaker currently permits a compression attempt.
    /// HalfOpen always returns `true` (probe), Open returns `false`
    /// (skip), Closed returns `true` (normal).
    pub fn breaker_allows_attempt(&mut self) -> bool {
        match self.breaker_state() {
            BreakerState::Open => false,
            BreakerState::HalfOpen | BreakerState::Closed => true,
        }
    }

    /// Record the outcome of a single compression attempt.
    /// * `success` — the LLM returned a usable summary. Resets
    /// `consecutive_llm_failures` to 0 and transitions the breaker
    /// to `Closed` regardless of prior state.
    /// * `failure` — increment `consecutive_llm_failures`; if it reaches
    /// `breaker_threshold`, transition to `Open` and arm the cooldown.
    /// also stamps `last_state_change_unix_ms` whenever
    /// the state actually changes, so the `CompactionStatusBanner` can
    /// show a "since X" hint. `Closed` is the default — we only stamp
    /// when transitioning *into* Open (failure) or back *out* of
    /// Open/HalfOpen (success); the "not yet at threshold" branch
    /// intentionally does not stamp because the public state didn't
    /// change.
    /// Returns the post-call breaker state so callers can log/forward
    /// it without re-locking the compressor.
    pub fn record_compression_outcome(&mut self, success: bool) -> BreakerState {
        let now_unix_ms = chrono::Utc::now().timestamp_millis();
        if success {
            self.consecutive_llm_failures = 0;
            self.breaker_open_until = None;
            let prior = self.last_breaker_state;
            self.last_breaker_state = BreakerState::Closed;
            if prior != BreakerState::Closed {
                self.last_state_change_unix_ms = Some(now_unix_ms);
            }
            return BreakerState::Closed;
        }

        self.consecutive_llm_failures = self.consecutive_llm_failures.saturating_add(1);
        if self.consecutive_llm_failures >= self.breaker_threshold {
            self.breaker_open_until = Some(Instant::now() + self.breaker_cooldown);
            self.last_breaker_state = BreakerState::Open;
            self.last_state_change_unix_ms = Some(now_unix_ms);
            if !self.config.quiet_mode {
                tracing::warn!(
                    consecutive_failures = self.consecutive_llm_failures,
                    cooldown_secs = self.breaker_cooldown.as_secs(),
                    "Context-compression circuit breaker tripped Open. \
                     Further LLM-compression attempts paused for {:?}.",
                    self.breaker_cooldown,
                );
            }
            BreakerState::Open
        } else {
            // Not yet at threshold — leave the breaker state alone (Closed
            // or HalfOpen). The next failure may push it over.
            self.last_breaker_state
        }
    }

    /// Snapshot of the current breaker state — serialise and forward
    /// to the front-end as a `compaction_status` WS event.
    /// takes the live pressure (`used_tokens`,
    /// `threshold_tokens`) and the last failure reason as parameters
    /// so a single snapshot carries everything `CompactionStatusBanner`
    /// needs to render. Pass `0` / `None` for the optional fields if
    /// the snapshot is produced outside an active compression attempt
    /// (the values default to the zero/None versions and the banner
    /// simply hides the corresponding line).
    pub fn breaker_snapshot(
        &mut self,
        used_tokens: u64,
        threshold_tokens: u64,
        max_tokens: u64,
        last_failure_reason: Option<String>,
    ) -> BreakerSnapshot {
        self.breaker_snapshot_with_phase(
            used_tokens,
            threshold_tokens,
            max_tokens,
            last_failure_reason,
            "idle",
        )
    }

    /// Same as [`Self::breaker_snapshot`] but lets the caller set the
    /// compression `phase` explicitly. introduced so the
    /// `CompactionStatusBanner` can render the "starting / in_progress
    /// complete / failed" transitions instead of jumping straight
    /// from the pressure warning to the post-attempt result.
    pub fn breaker_snapshot_with_phase(
        &mut self,
        used_tokens: u64,
        threshold_tokens: u64,
        max_tokens: u64,
        last_failure_reason: Option<String>,
        phase: &str,
    ) -> BreakerSnapshot {
        let state = self.breaker_state();
        let cooldown_remaining_secs = match (state, self.breaker_open_until) {
            (BreakerState::Open, Some(deadline)) => {
                let now = Instant::now();
                if now >= deadline {
                    0
                } else {
                    (deadline - now).as_secs()
                }
            }
            _ => 0,
        };
        let last_savings_ratio = if self.last_metrics.original_tokens > 0 {
            Some(self.last_metrics.tokens_saved as f64 / self.last_metrics.original_tokens as f64)
        } else {
            None
        };
        BreakerSnapshot {
            state,
            consecutive_llm_failures: self.consecutive_llm_failures,
            breaker_threshold: self.breaker_threshold,
            cooldown_remaining_secs,
            compression_count: self.compression_count,
            last_savings_ratio,
            used_tokens,
            threshold_tokens,
            max_tokens,
            unit: default_unit(),
            last_failure_reason,
            last_changed_at_unix_ms: self.last_state_change_unix_ms,
            phase: phase.to_string(),
        }
    }

    /// Get metrics from the most recent compression operation.
    pub fn last_metrics(&self) -> &CompressionMetrics {
        &self.last_metrics
    }

    /// Get the previous summary text (for cross-session persistence).
    pub fn previous_summary(&self) -> Option<&str> {
        self.previous_summary.as_deref()
    }

    /// Compression trigger predicate.
    /// Includes anti-thrashing protection: if the last two compressions
    /// each saved less than 10%, skip compression to avoid infinite loops.
    pub fn should_compress(&self, current_prompt_tokens: Option<u64>) -> bool {
        let tokens = current_prompt_tokens.unwrap_or(self.last_prompt_tokens);
        if tokens < self.threshold_tokens {
            return false;
        }
        if self.ineffective_compression_count >= 2 {
            if !self.config.quiet_mode {
                tracing::warn!(
                    "Compression skipped — last {} compressions saved <10% each. \
                     Consider /new to start a fresh session.",
                    self.ineffective_compression_count,
                );
            }
            return false;
        }
        true
    }

    // ------------------------------------------------------------------
    //Snip — remove ancient messages (cheapest, no API cost).
    // ------------------------------------------------------------------

    /// Remove very old messages from the conversation history.
    /// This is the cheapest compression step — zero API cost, just drops
    /// messages that are too old to be relevant.
    /// Returns `(messages, snipped_count)` where snipped_count is the
    /// number of messages removed.
    pub fn snip_ancient_messages(
        &self,
        messages: &[Message],
        max_age_turns: usize,
    ) -> (Vec<Message>, usize) {
        if messages.len() <= max_age_turns + self.config.protect_first_n + 1 {
            return (messages.to_vec(), 0);
        }

        // Keep: protect_first_n head messages + last max_age_turns messages
        let keep_start = self.config.protect_first_n;
        let keep_end = messages.len().saturating_sub(max_age_turns);

        if keep_start >= keep_end {
            return (messages.to_vec(), 0);
        }

        let snipped_count = keep_end - keep_start;
        let mut result: Vec<Message> = Vec::with_capacity(messages.len() - snipped_count);

        // Keep head
        for i in 0..keep_start {
            result.push(messages[i].clone());
        }

        // Insert snip marker
        result.push(Message::system(
            "[SNIP] Earlier conversation turns were removed to save context space."
        ));

        // Keep tail
        for i in keep_end..messages.len() {
            result.push(messages[i].clone());
        }

        if !self.config.quiet_mode {
            tracing::info!(
                "Snip: removed {} ancient messages (keeping {} head + {} tail)",
                snipped_count,
                keep_start,
                messages.len() - keep_end,
            );
        }

        (result, snipped_count)
    }

    // ------------------------------------------------------------------
    //Context Collapse (read-time projection).
    // ------------------------------------------------------------------

    /// Compute a "compressed view" of messages at read time without
    /// modifying the original. This is the Context Collapse step from
    /// Claude Code's 5-step compression pipeline.
    /// The key insight: instead of permanently modifying messages,
    /// compute a compressed view only when sending to the API.
    /// This preserves the full history for debugging while reducing
    /// the context sent to the model.
    /// Strategy: keep recent N messages intact, compress older messages
    /// by replacing tool results with summaries.
    pub fn context_collapse(
        messages: &[Message],
        protect_recent: usize,
        compress_threshold: f64,
        context_length: Option<u64>,
    ) -> Vec<Message> {
        // Dynamically adjust protect_recent: never protect more than half
        // the messages, otherwise there's nothing to compress.
        let effective_protect = protect_recent.min(messages.len() / 2).max(2);
        if messages.len() <= effective_protect {
            return messages.to_vec();
        }

        // Calculate total tokens
        let total_tokens: u64 = messages
            .iter()
            .map(|m| estimate_message_tokens(m) as u64)
            .sum();

        // Use provided context_length or fall back to default (128K)
        let estimated_context = context_length
            .unwrap_or(hermix_intelligence::model_metadata::DEFAULT_FALLBACK_CONTEXT as u64);
        if (total_tokens as f64 / estimated_context as f64) < compress_threshold {
            return messages.to_vec();
        }

        // Compress: keep recent messages intact, summarize older ones
        let mut result: Vec<Message> = Vec::with_capacity(messages.len());
        let compress_boundary = messages.len().saturating_sub(effective_protect);

        // Keep head messages (system prompt + first exchange)
        for i in 0..3.min(compress_boundary) {
            result.push(messages[i].clone());
        }

        // Add collapse marker
        result.push(Message::system(
            "[CONTEXT COLLAPSE] Older messages have been compressed. \
             Recent messages follow. Refer to the summary above for earlier context."
        ));

        // Keep recent messages
        for i in compress_boundary..messages.len() {
            result.push(messages[i].clone());
        }

        result
    }
}
