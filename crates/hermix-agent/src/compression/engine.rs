//! Compression engine: sanitize, boundary align, compress_fast, compress, restore.

use hermix_core::{Message, MessageRole};

use super::*;

impl ContextCompressor {
    // ------------------------------------------------------------------

    /// Fix orphaned tool_call / tool_result pairs after compression.
    pub fn sanitize_tool_pairs(&self, messages: Vec<Message>) -> Vec<Message> {
        let mut surviving_call_ids = std::collections::HashSet::new();
        for msg in &messages {
            if msg.role == MessageRole::Assistant {
                if let Some(tcs) = msg.tool_calls.as_ref() {
                    for tc in tcs {
                        if !tc.id.is_empty() {
                            surviving_call_ids.insert(tc.id.clone());
                        }
                    }
                }
            }
        }

        let mut result_call_ids = std::collections::HashSet::new();
        for msg in &messages {
            if msg.role == MessageRole::Tool {
                if let Some(cid) = msg.tool_call_id.as_ref() {
                    result_call_ids.insert(cid.clone());
                }
            }
        }

        let orphaned_results: std::collections::HashSet<_> = result_call_ids
            .difference(&surviving_call_ids)
            .cloned()
            .collect();
        let missing_results: std::collections::HashSet<_> = surviving_call_ids
            .difference(&result_call_ids)
            .cloned()
            .collect();

        let messages: Vec<Message> = messages
            .into_iter()
            .filter(|m| {
                !(m.role == MessageRole::Tool
                    && m.tool_call_id
                        .as_ref()
                        .map(|cid| orphaned_results.contains(cid))
                        .unwrap_or(false))
            })
            .collect();

        if !orphaned_results.is_empty() && !self.config.quiet_mode {
            tracing::info!(
                "Compression sanitiser: removed {} orphaned tool result(s)",
                orphaned_results.len()
            );
        }

        if missing_results.is_empty() {
            return messages;
        }

        let mut patched = Vec::with_capacity(messages.len() + missing_results.len());
        for msg in messages.into_iter() {
            let role = msg.role;
            let tool_calls = msg.tool_calls.clone();
            patched.push(msg);
            if role == MessageRole::Assistant {
                if let Some(tcs) = tool_calls {
                    for tc in tcs {
                        if missing_results.contains(&tc.id) {
                            patched.push(Message {
                                role: MessageRole::Tool,
                                content: Some(
                                    "[ALREADY EXECUTED] [Result from earlier conversation — see context summary above]"
                                        .into(),
                                ),
                                content_parts: None,
                                tool_calls: None,
                                tool_call_id: Some(tc.id.clone()),
                                name: None,
                                reasoning_content: None,
                                cache_control: None,
                                execution_timeline: None,
                                unified_steps: None,
                                ephemeral: false,
                            });
                        }
                    }
                }
            }
        }

        if !self.config.quiet_mode {
            tracing::info!(
                "Compression sanitiser: added {} stub tool result(s)",
                missing_results.len()
            );
        }

        patched
    }

    // ------------------------------------------------------------------
    // Boundary alignment helpers.
    // ------------------------------------------------------------------

    pub(crate) fn align_boundary_forward(&self, messages: &[Message], idx: usize) -> usize {
        let mut idx = idx;
        while idx < messages.len() && messages[idx].role == MessageRole::Tool {
            idx += 1;
        }
        idx
    }

    pub(crate) fn align_boundary_backward(&self, messages: &[Message], idx: usize) -> usize {
        if idx == 0 || idx >= messages.len() {
            return idx;
        }
        let mut check = idx as isize - 1;
        while check >= 0 && messages[check as usize].role == MessageRole::Tool {
            check -= 1;
        }
        if check >= 0 {
            let candidate = &messages[check as usize];
            if candidate.role == MessageRole::Assistant
                && candidate
                    .tool_calls
                    .as_ref()
                    .map(|t| !t.is_empty())
                    .unwrap_or(false)
            {
                return check as usize;
            }
        }
        idx
    }

    pub(crate) fn find_tail_cut_by_tokens(
        &self,
        messages: &[Message],
        head_end: usize,
        token_budget: Option<u64>,
    ) -> usize {
        let budget = token_budget.unwrap_or(self.tail_token_budget);
        let n = messages.len();
        let min_tail = if n > head_end + 1 {
            3.min(n - head_end - 1)
        } else {
            0
        };
        let soft_ceiling = (budget as f64 * 1.5) as u64;
        let mut accumulated: u64 = 0;
        let mut cut_idx = n;

        for i in (head_end..n).rev() {
            let msg = &messages[i];
            let msg_tokens = estimate_message_tokens(msg) as u64;
            if accumulated + msg_tokens > soft_ceiling && (n - i) >= min_tail {
                break;
            }
            accumulated += msg_tokens;
            cut_idx = i;
        }

        let fallback_cut = n.saturating_sub(min_tail);
        if cut_idx > fallback_cut {
            cut_idx = fallback_cut;
        }
        if cut_idx <= head_end {
            cut_idx = fallback_cut.max(head_end + 1);
        }
        let cut_idx = self.align_boundary_backward(messages, cut_idx);
        cut_idx.max(head_end + 1)
    }

    // ------------------------------------------------------------------
    // Main entry point.
    // ------------------------------------------------------------------

    /// Run a full compression pass on `messages`.
    /// `current_tokens` is used purely for logging — pass `None` to fall
    /// back to the last tracked `prompt_tokens` value.
    /// fast-mode compression — runs every pass
    /// (archive / dedup / score-drop / boundary) but skips the
    /// LLM summary call, replacing it with the static fallback
    /// marker. Used by `auto_compress_if_over_threshold` when
    /// pressure is moderate (50–85%) so the agent loop doesn't
    /// block a turn on an LLM round-trip.
    /// `fast_mode` is set to `true` for the duration of the inner
    /// [`Self::compress`] call and reset to `false` afterwards —
    /// including early-return paths inside `compress`, because
    /// this wrapper awaits `compress` and runs the reset
    /// unconditionally in its own frame.
    pub async fn compress_fast(
        &mut self,
        messages: Vec<Message>,
        current_tokens: Option<u64>,
    ) -> Vec<Message> {
        self.fast_mode = true;
        let result = self.compress(messages, current_tokens).await;
        self.fast_mode = false;
        result
    }

    pub async fn compress(
        &mut self,
        messages: Vec<Message>,
        current_tokens: Option<u64>,
    ) -> Vec<Message> {
        //+ wire (read-only): diagnose current context
        // health and consult the flush-integration helper to decide
        // whether the agent should be given a sentinel to memory-
        // flush before we touch messages. We never *block* compression
        // on this — flush is advisory.
        // Char-estimate: tokens * 4 is a reasonable upper bound (one
        // ASCII char ~= 1 token in worst-case repetition). The health
        // is a soft warning, not a hard gate, so a coarse estimate is
        // acceptable.
        let pre_chars = (estimate_messages_tokens(&messages) as u64).saturating_mul(4);
        let pre_health = crate::context_engine::ContextHealth::diagnose(
            pre_chars,
            self.config.context_length as u64,
        );
        if pre_health.is_actionable() && !self.config.quiet_mode {
            tracing::warn!(
                health = ?pre_health.level,
                utilization = pre_health.utilization,
                reasons = pre_health.reasons.len(),
                "compress(): pre-check found actionable context pressure"
            );
        }

        //wire: if the session is long enough and the health
        // threshold (Red / Clamped) is met, inject a memory-flush
        // sentinel so the next turn can write to the memory tool.
        // Idempotent: if a sentinel is already in `messages`, the
        // helper returns the same vector unchanged.
        let flush_min = self.config.flush_sentinel_min_turns.max(6);
        let flush_helper = crate::context_engine::FlushIntegration {
            min_health_to_skip: crate::context_engine::HealthLevel::Red,
            flush_min_turns: flush_min,
            flush_already_requested: false,
        };
        let messages = if flush_helper.should_flush(&pre_health, messages.len()) {
            crate::flush_memories::maybe_inject_flush_sentinel(messages, flush_min)
        } else {
            messages
        };

        // Flush sentinel strip. If a caller left
        // flush-memories sentinels in `messages`, drop them now — must
        // happen *before* `n_messages` is captured so all downstream
        // bookkeeping sees the post-strip length. Idempotent: a clean
        // vector stays clean.
        let messages = crate::flush_memories::strip_flush_sentinels(messages);

        // `n_messages` and the min_for_compress gate
        //are now re-bound AFTER the pre-summary passes (,
        // 3.5, A1, 1, 1.5) below — those passes shrink the slice,
        // so the original length-capture point was off by
        // however many messages were reduced.

        let display_tokens = current_tokens
            .or(Some(self.last_prompt_tokens))
            .filter(|t| *t > 0)
            .unwrap_or_else(|| estimate_messages_tokens(&messages));

        //Snip ancient messages (cheapest, no API cost).
        // Remove messages older than 50 turns to free context space.
        let (messages, snipped) = self.snip_ancient_messages(&messages, 50);
        if snipped > 0 && !self.config.quiet_mode {
            tracing::info!("Phase 0 (Snip): removed {} ancient messages", snipped);
        }

        //Context Collapse (read-time projection).
        //This is applied to the messages before to provide
        // an additional layer of compression without API cost.
        let messages = Self::context_collapse(
            &messages,
            self.config.protect_last_n,
            0.90,
            Some(self.config.context_length),
        );

        // build pin config once per compress
        // call so every pass below sees a consistent view.
        let pin_cfg = crate::compression_pipeline::PinConfig::from_compressor_config(&self.config);

        // Pass 0 — adjacent user-text dedup.
        //Runs before so the tool dedup pass operates on
        // the post-user-dedup slice. Pinned messages are exempt.
        let user_messages_for_dedup = messages.clone();
        let (messages, user_dedup_count) =
            crate::compression_pipeline::dedup_adjacent_user(messages);
        self.last_user_dedup_count = user_dedup_count;
        if user_dedup_count > 0 && !self.config.quiet_mode {
            tracing::info!(
                "Pre-compression: Pass 0 deduped {} adjacent user message(s)",
                user_dedup_count,
            );
        }
        // Quiet the unused-variable lint when min_tail forces a no-op above.
        let _ = user_messages_for_dedup;

        //prune old tool results.
        let (mut messages, dedup_count, pruned_count) = self.prune_old_tool_results(
            &messages,
            self.config.protect_last_n,
            Some(self.tail_token_budget),
            pin_cfg.as_ref(),
        );
        if pruned_count > 0 && !self.config.quiet_mode {
            tracing::info!(
                "Pre-compression: pruned {} old tool result(s) ({} deduped)",
                pruned_count,
                dedup_count,
            );
        }

        // Compute pin count once for the report. Pinned messages
        // survive every pass above (Pass 0 user dedup, Pass 1
        // tool dedup, Pass 2 tool summary, Pass 3 args, Pass 4
        //screenshots) and below.
        self.last_pin_count = pin_cfg
            .as_ref()
            .map(|p| messages.iter().filter(|m| p.is_pinned(m)).count())
            .unwrap_or(0);

        // score-based drop.
        // Messages whose TokenScorer score is strictly below
        // `drop_score_threshold` are removed from context — but
        // **returned** (not destroyed) so they can be archived
        // alongside the LLM-summarised slice. Protective
        // heuristics (pin / tail window / instructional /
        // referenced tool) override the score. Disabled when
        // threshold <= 0.0.
        let drop_threshold = self.config.drop_score_threshold;
        let mut dropped_count: usize = 0;
        if drop_threshold > 0.0 {
            // Build the scorer with awareness of the current task
            // (most recent user message) so the query_relevance
            // dimension can rescue task-critical-but-low-frequency
            // messages that pure recency/freq would starve.
            let scorer = crate::compression_pipeline::TokenScorer::for_query(&messages);
            // min_keep = head + tail + 3 middle messages the
            // summariser can actually compress. Drop must never
            // shrink the list below this floor — otherwise
            // `n_messages <= min_for_compress` (or
            // `compress_start >= compress_end`) trips and the whole
            // pass becomes a no-op with no CompressionReport. If
            // breaching the floor is unavoidable, the highest-
            // scoring candidates are rescued back into the kept set.
            let min_keep = self.config.protect_first_n + self.config.protect_last_n + 3;
            let (kept, dropped) = crate::compression_pipeline::drop_low_score(
                messages,
                &scorer,
                pin_cfg.as_ref(),
                drop_threshold,
                self.config.protect_last_n,
                min_keep,
            );
            dropped_count = dropped.len();
            messages = kept;
            //Lossless: stash dropped messages so 's archive
            // path picks them up alongside `turns_to_summarize`.
            //Previously these were dropped in (before the
            // boundary cut) and were therefore in neither
            // `turns_to_summarize` nor the compressed tail — i.e.
            // permanently lost.
            self.last_dropped_turns = dropped;
            if dropped_count > 0 && !self.config.quiet_mode {
                tracing::info!(
                    "Pre-compression: dropped {} low-score message(s) below threshold {} (archived, not destroyed)",
                    dropped_count,
                    drop_threshold,
                );
            }
        } else {
            self.last_dropped_turns.clear();
        }

        // re-bind `n_messages` after every reduction
        //pass ( snip, collapse, user
        //dedup, tool prune, score drop). The
        // old value was captured before these passes, so using it
        // as the tail-loop upper bound would index past the end of
        // the now-shorter `messages` slice.
        let n_messages = messages.len();
        let min_for_compress = self.config.protect_first_n + 3 + 1;
        if n_messages <= min_for_compress {
            if !self.config.quiet_mode {
                tracing::warn!(
                    "Cannot compress: only {} messages (need > {})",
                    n_messages,
                    min_for_compress,
                );
            }
            return messages;
        }

        // Trigger reason: this is the auto-compression path
        // triggered by the agent loop when context pressure crosses
        // the threshold. Explicit (user-initiated) compress calls
        // would set a different value here, but today every caller
        // goes through `auto_compress_if_over_threshold`.
        self.last_trigger_reason = "threshold".to_string();

        //determine boundaries.
        let compress_start = self.align_boundary_forward(&messages, self.config.protect_first_n);
        let mut compress_end = self.find_tail_cut_by_tokens(&messages, compress_start, None);
        // Policy: the last assigned task instruction must never be
        // summarised away. If it falls inside the middle slice that
        // would be condensed, pull the tail boundary back so it stays
        // verbatim in the protected tail (it is recent and task-critical).
        if let Some(task_idx) = crate::compression_pipeline::last_task_instruction_index(&messages) {
            if task_idx < compress_end {
                compress_end = task_idx;
            }
        }
        if compress_start >= compress_end {
            return messages;
        }
        let turns_to_summarize: Vec<Message> = messages[compress_start..compress_end].to_vec();

        if !self.config.quiet_mode {
            let tail_msgs = n_messages - compress_end;
            tracing::info!(
                "Context compression triggered ({} tokens >= {} threshold)",
                display_tokens,
                self.threshold_tokens
            );
            tracing::info!(
                "Summarising turns {}-{} ({} turns), protecting {} head + {} tail messages",
                compress_start + 1,
                compress_end,
                turns_to_summarize.len(),
                compress_start,
                tail_msgs
            );
        }

        //generate structured summary.
        // fast_mode skips the LLM call entirely
        // and uses the static fallback. The original turns are
        //already archived ( / `last_archived_turns` +
        // `last_dropped_turns`), so nothing is lost — the LLM
        // just doesn't get a semantic handoff for the removed slice.
        let (summary_opt, summary_generated): (Option<String>, bool) = if self.fast_mode {
            if !self.config.quiet_mode {
                tracing::info!(
                    "Fast-mode compression: skipping LLM summary, using static fallback ({} turns archived)",
                    turns_to_summarize.len(),
                );
            }
            (None, false)
        } else {
            match self.generate_summary(&turns_to_summarize).await {
                Ok(s) => (s, true),
                Err(err) => {
                    if !self.config.quiet_mode {
                        tracing::warn!("Summary generation error (using fallback): {err}");
                    }
                    (None, false)
                }
            }
        };

        //assemble the compressed message list.
        let mut compressed: Vec<Message> = Vec::with_capacity(messages.len());
        for i in 0..compress_start {
            let mut msg = messages[i].clone();
            if i == 0 && msg.role == MessageRole::System && self.compression_count == 0 {
                let extra = "\n\n[Note: Some earlier conversation turns have been compacted into a handoff summary to preserve context space. The current session state may still reflect earlier work, so build on that summary and state rather than re-doing work.]";
                let new_content = msg.content.unwrap_or_default() + extra;
                msg.content = Some(new_content);
            }
            compressed.push(msg);
        }

        let n_dropped = compress_end - compress_start;
        let summary_from_llm = summary_opt.is_some();
        let mut summary = summary_opt.unwrap_or_else(|| {
            if !self.config.quiet_mode {
                tracing::warn!(
                    "Summary generation failed — inserting static fallback context marker"
                );
            }
            format!(
                "{SUMMARY_PREFIX}\nSummary generation was unavailable. {n_dropped} \
                 conversation turns were removed to free context space but could not be \
                 summarized. The removed turns contained earlier work in this session. \
                 Continue based on the recent messages below and the current state of \
                 any files or resources."
            )
        });

        //(round 5 safety net): the summary produced by the
        // LLM is now sanitised before it is spliced into the prompt.
        // The LLM is instructed to behave but is not trusted to obey:
        // a hallucinated file path or a quoted "ignore previous
        // instructions" instruction from a captured tool result could
        // propagate into every downstream turn if we don't filter it.
        // The safety net runs *only* on the LLM-derived summary path,
        // not on the static fallback (which is hard-coded).
        if summary_from_llm {
            summary = apply_summary_safety_net(summary);
        } else if !self.config.quiet_mode {
            tracing::debug!(
                "Skipping summary safety net — using static fallback (n_dropped={n_dropped})"
            );
        }

        let last_head_role = compress_start
            .checked_sub(1)
            .map(|i| messages[i].role)
            .unwrap_or(MessageRole::User);
        let first_tail_role = if compress_end < n_messages {
            messages[compress_end].role
        } else {
            MessageRole::User
        };

        let mut summary_role =
            if matches!(last_head_role, MessageRole::Assistant | MessageRole::Tool) {
                MessageRole::User
            } else {
                MessageRole::Assistant
            };
        let mut merge_summary_into_tail = false;
        if summary_role == first_tail_role {
            let flipped = if summary_role == MessageRole::User {
                MessageRole::Assistant
            } else {
                MessageRole::User
            };
            if flipped != last_head_role {
                summary_role = flipped;
            } else {
                merge_summary_into_tail = true;
            }
        }
        if !merge_summary_into_tail {
            compressed.push(Message {
                role: summary_role,
                content: Some(format!("{summary}{SUMMARY_END_MARKER}")),
                content_parts: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
                reasoning_content: None,
                cache_control: None,
                execution_timeline: None,
                unified_steps: None,
                ephemeral: false,
            });
        }

        for i in compress_end..n_messages {
            let mut msg = messages[i].clone();
            if merge_summary_into_tail && i == compress_end {
                let original = msg.content.unwrap_or_default();
                // Strip image placeholders in the *original* tail content
                // before merging — the summary itself contains the
                // instructional `image#1: ...` example inside
                // SUMMARY_PREFIX and must be left intact.
                let (cleaned_original, _) = strip_image_placeholders(&original);
                msg.content = Some(format!(
                    "{summary}{SUMMARY_END_MARKER}\n\n{cleaned_original}"
                ));
                merge_summary_into_tail = false;
                compressed.push(msg);
                continue;
            }
            // Even messages inside the "protect last N" window can carry
            // raw `@image#N` placeholders from interrupted browser tools.
            // Strip them so the agent never echoes them as fresh content
            // and the frontend never hangs on a phantom "running" card.
            if let Some(content) = msg.content.as_ref() {
                let (cleaned, stripped) = strip_image_placeholders(content);
                if stripped > 0 {
                    msg.content = Some(cleaned);
                }
            }
            compressed.push(msg);
        }

        self.compression_count += 1;
        let mut compressed = self.sanitize_tool_pairs(compressed);

        // Anti-thrashing: track whether compression was effective.
        let old_estimate = display_tokens;
        let new_estimate = estimate_messages_tokens(&compressed);
        let saved = old_estimate.saturating_sub(new_estimate);
        let savings_pct = if old_estimate > 0 {
            (saved as f64 / old_estimate as f64) * 100.0
        } else {
            100.0
        };
        let anti_thrashing = savings_pct < 10.0;
        if anti_thrashing {
            self.ineffective_compression_count += 1;
        } else {
            self.ineffective_compression_count = 0;
        }

        // Populate metrics.
        self.last_metrics = CompressionMetrics {
            original_tokens: old_estimate,
            compressed_tokens: new_estimate,
            tokens_saved: saved,
            compression_ratio: if old_estimate > 0 {
                new_estimate as f64 / old_estimate as f64
            } else {
                1.0
            },
            original_turns: n_messages,
            compressed_turns: compressed.len(),
            turns_removed: compress_end.saturating_sub(compress_start),
            tool_results_pruned: pruned_count,
            // previously hard-coded to 0 with a TODO.
            // `prune_old_tool_results` now returns the count of
            // messages that were replaced with the
            // `[Duplicate tool output]` marker (MD5 dedup pass).
            deduplication_count: dedup_count,
            // Compression-report counters:
            user_dedup_count: self.last_user_dedup_count,
            pin_count: self.last_pin_count,
            dropped_count,
            trigger_reason: self.last_trigger_reason.clone(),
            summary_api_calls: 1,
            summary_errors: if summary_generated { 0 } else { 1 },
            anti_thrashing_triggered: anti_thrashing,
        };

        if !self.config.quiet_mode {
            tracing::info!(
                "Compressed: {} -> {} messages (~{} tokens saved, {:.1}% savings)",
                n_messages,
                compressed.len(),
                saved,
                savings_pct,
            );
            tracing::info!("Compression #{} complete: {}", self.compression_count, self.last_metrics);
        }

        // emit the structured
        // CompressionReport so dashboards and trace logs see the
        // full pipeline breakdown (pin / dedup / prune / drop /
        // per-role counts). also derive
        // compression_ratio + estimated_quality_loss and emit a
        // warning when the pass looks risky (mirrors Headroom's
        // `on_compression_complete` alert).
        let (compression_ratio, estimated_quality_loss) =
            crate::compression_pipeline::CompressionReport::derive_metrics(
                old_estimate,
                new_estimate,
                n_messages,
                dropped_count,
            );
        let report = crate::compression_pipeline::CompressionReport {
            pre_tokens: old_estimate,
            post_tokens: new_estimate,
            pre_count: n_messages,
            post_count: compressed.len(),
            pin_count: self.last_pin_count,
            dedup_count: dedup_count.saturating_add(self.last_user_dedup_count),
            pruned_count,
            dropped_count,
            summary_tokens: Some(estimate_messages_tokens(&compressed)),
            compression_ratio,
            estimated_quality_loss,
            trigger_reason: if self.last_trigger_reason.is_empty() {
                "auto".into()
            } else {
                self.last_trigger_reason.clone()
            },
            per_role_pre: crate::compression_pipeline::CompressionReport::compute_per_role(&messages),
            per_role_post: crate::compression_pipeline::CompressionReport::compute_per_role(&compressed),
        };
        if !self.config.quiet_mode {
            tracing::info!("{}", report.format_line());
        }
        report.emit_warn_if_concerning();
        self.last_compression_report = Some(report);

        // (A): stash the raw `turns_to_summarize` slice so
        // the agent loop can hand it to
        // `SessionPersistence::archive_compressed_turns` after we
        // return. Without this, the original wording is gone the
        // moment `compressed` replaces the in-memory slice — the
        // handoff summary is all that's left, and it's lossy by
        // design.
        self.last_archived_turns = turns_to_summarize.clone();
        //Fold in the dropped messages so the agent loop's
        // single archive path persists them too. Previously these
        // were lost (dropped before the boundary cut, so in neither
        // the summarised slice nor the compressed tail).
        self.last_archived_turns.extend(self.last_dropped_turns.clone());

        // Post-compression restoration: inject guidance about recently
        // accessed files to help the model recover context.
        Self::post_compression_restore(&mut compressed, &messages, None);

        // Quiet the unused `messages` lint when min_tail forces a no-op above.
        drop(messages);

        compressed
    }

    // ------------------------------------------------------------------
    // Post-compression context restoration.
    // ------------------------------------------------------------------

    /// Restore important context after compression.
    /// After Auto-Compact, the model may lose track of recently accessed
    /// files, active skills, or current task state. This method injects
    /// restoration guidance into the compressed messages to help the
    /// model recover.
    /// Inspired by Claude Code's Post-Compact Restoration which re-injects
    /// recently accessed files (up to 5, 50K tokens) and active skills.
    pub fn post_compression_restore(
        compressed: &mut Vec<Message>,
        _original_messages: &[Message],
        _session_id: Option<&str>,
    ) {
        if compressed.len() < 3 {
            return; // Too few messages to restore
        }

        // Extract recently accessed file paths from the tail messages
        let recent_files: Vec<String> = compressed
            .iter()
            .rev()
            .take(10)
            .filter_map(|m| {
                if let Some(content) = &m.content {
                    // Look for file path patterns in tool results
                    if content.contains("path:") || content.contains("file:") {
                        // Extract file paths (simplified)
                        Some(content.lines().filter(|l| l.contains("path:")).take(3).collect::<Vec<_>>().join(", "))
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .filter(|s| !s.is_empty())
            .take(5)
            .collect();

        if recent_files.is_empty() {
            return;
        }

        // Inject restoration guidance
        let restoration_msg = Message::system(&format!(
            "[POST-COMPRESSION RESTORE] Recently accessed files that may be relevant:\n{}\n\n\
             If you need to reference these files, use read_file to load them again.",
            recent_files.iter().map(|f| format!("- {}", f)).collect::<Vec<_>>().join("\n")
        ));

        // Insert after the first message (system prompt)
        if compressed.len() > 1 {
            compressed.insert(1, restoration_msg);
        }
    }
}
