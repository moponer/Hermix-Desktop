//! Convergence detection (AL-8, , revised ).
//!
//! ## Design philosophy
//!
//! The agent loop's job is to keep running until the LLM naturally stops
//! (no tool calls in its response). Convergence detection exists to prevent
//! **wasteful infinite loops**, but it must be extremely conservative —
//! killing a loop that's making progress is far worse than letting one
//! run a few extra turns.
//!
//! Key insight: **if the LLM is calling tools that succeed, it's working**.
//! The LLM knows whether it's making progress — it has the full conversation
//! history. Only interrupt when there's overwhelming evidence of a stuck loop:
//!
//! 1. **ErrorLoop** — ALL tool calls errored for N consecutive turns.
//!    This is the only signal that can *kill* the loop. The LLM is stuck:
//!    every tool it tries fails, and re-prompting won't help.
//!
//! 2. **NoProgressWithTools / SimilarContent** — these are *advisory*.
//!    Instead of killing the loop, they produce a **warning string** that
//!    the caller injects as a system message. The LLM can then decide
//!    whether to summarize and stop, or continue with a different approach.
//!    This respects the "trust the LLM" principle: the LLM has full
//!    context and is best positioned to decide if it's stuck.
//!
//! Why NoProgressWithTools shouldn't kill the loop:
//! - Many legitimate tasks involve 10+ consecutive tool-only turns
//!   (e.g. "search this codebase for all uses of X" → read file →
//!   read another file → grep → … → final summary).
//! - Raising the threshold to 10 wasn't enough; the real fix is
//!   to not kill the loop at all, just nudge the LLM.

/// Signal returned by the convergence detector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConvergenceSignal {
    /// No convergence issues detected — continue the loop.
    Continue,
    /// Advisory warning: the agent *might* be stuck.
    /// Caller should inject this as a system message and continue the loop.
    /// The LLM will decide whether to stop or keep going.
    Warn {
        reason: ConvergenceReason,
        consecutive_count: u32,
        message: String,
    },
    /// Hard stop: the agent is definitely stuck.
    /// Only `ErrorLoop` triggers this — all tools failed for N turns.
    Converge {
        reason: ConvergenceReason,
        consecutive_count: u32,
    },
}

/// Why convergence was detected.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) enum ConvergenceReason {
    /// Too many consecutive turns with tool calls but no visible text.
    NoProgressWithTools,
    /// Too many consecutive turns where ALL tool calls errored.
    ErrorLoop,
    /// Too many consecutive turns with similar visible text.
    SimilarContent,
}

/// Configuration for the convergence detector.
#[derive(Debug, Clone)]
pub(crate) struct ConvergenceConfig {
    /// Max consecutive turns with tool calls but no visible text before
    /// issuing a WARN (advisory).
    pub max_no_progress_turns: u32,
    /// Max consecutive turns with tool calls but no visible text before
    /// issuing a HARD STOP. This is the escalation threshold: after the
    /// advisory warning at `max_no_progress_turns`, the LLM gets
    /// `max_no_progress_hard_stop_turns - max_no_progress_turns` extra
    /// turns to produce visible text before the loop is killed.
    /// Set to 0 to disable escalation (keep old advisory-only behavior).
    pub max_no_progress_hard_stop_turns: u32,
    /// Max *cumulative* turns with tool calls but no visible text in this
    /// run before issuing a WARN (advisory). Unlike `max_no_progress_turns`
    /// (which counts a *consecutive* streak), this counts the *total*
    /// silent-tool turns across the whole run and is NOT reset when an
    /// interleaved turn produces visible text. It catches the
    /// "terminal (silent) ↔ write_file (one line)" alternation that resets
    /// the consecutive counter every other turn and therefore never trips
    /// it, while still burning a full prompt every turn.
    pub max_silent_tool_turns: u32,
    /// Hard-stop escalation for the cumulative silent-tool-turn counter.
    /// Set to 0 to disable.
    pub max_silent_tool_hard_stop_turns: u32,
    /// Max consecutive turns where ALL tool calls errored.
    /// This triggers a hard stop (Converge).
    pub max_error_turns: u32,
    /// Max consecutive turns where the SAME tool errored (even if
    /// other tools succeeded). This triggers an advisory WARN.
    pub max_per_tool_error_turns: u32,
    /// Max consecutive turns with similar visible text before issuing
    /// a WARN (advisory).
    pub max_similar_content_turns: u32,
    /// Max consecutive turns with similar visible text before issuing
    /// a HARD STOP. Escalation threshold after the advisory warning.
    /// Set to 0 to disable escalation.
    pub max_similar_content_hard_stop_turns: u32,
    /// Similarity threshold (0.0-1.0) for content comparison.
    pub similarity_threshold: f64,
}

impl Default for ConvergenceConfig {
    fn default() -> Self {
        Self {
            // 15 turns of tool-only (no visible text) → advisory warning.
            // The LLM might legitimately be doing a long chain of reads.
            max_no_progress_turns: 15,
            // 25 turns of tool-only → HARD STOP. After the advisory
            // warning at 15, the LLM gets 10 more turns to produce
            // visible text. If it doesn't, the loop is killed to prevent
            // infinite tool chaining and context bloat.
            //(: was advisory-only; escalated to hard stop
            // because LLM tends to "persist in current strategy" rather
            // than self-correct, leading to context exhaustion.)
            max_no_progress_hard_stop_turns: 25,
            // 10 cumulative silent-tool turns → advisory warning. Chosen
            // below the consecutive threshold (15) because the cumulative
            // counter is strictly monotonic: a run that alternates
            // "silent tool call ↔ tool call + one line" reaches 10 silent
            // turns by turn ~20, well within a normal max_turns budget.
            max_silent_tool_turns: 10,
            // 18 cumulative silent-tool turns → HARD STOP. Escalation
            // after the advisory at 10, giving the LLM 8 more silent
            // turns to start reporting before the loop is killed.
            max_silent_tool_hard_stop_turns: 18,
            // 3 consecutive turns of ALL-error tool calls → hard stop.
            max_error_turns: 3,
            // 3 consecutive turns where the SAME tool errored → advisory.
            max_per_tool_error_turns: 3,
            // 5 consecutive turns of >80% similar visible text → advisory.
            max_similar_content_turns: 5,
            // 10 consecutive turns of similar content → HARD STOP.
            // After the advisory at 5, give 5 more turns to diverge.
            max_similar_content_hard_stop_turns: 10,
            similarity_threshold: 0.8,
        }
    }
}

impl ConvergenceConfig {
    /// Domain-biased thresholds. Only the **user pin** (`AgentConfig::
    /// domain_override`) is honored: the auto-detected domain changes per
    /// turn while the detector is per-run state, so re-thresholding per
    /// turn would corrupt the similar-content counters.
    ///
    /// Operate 域放宽 similar-content 阈值——批量文件操作天然高重复度
    /// （同路径逐条清单回执、批量重命名），默认 5/10 会误杀。
    pub(crate) fn for_domain(pin: Option<hermix_intelligence::DomainMode>) -> Self {
        let mut cfg = Self::default();
        if matches!(pin, Some(hermix_intelligence::DomainMode::Operate)) {
            cfg.max_similar_content_turns = 8;
            cfg.max_similar_content_hard_stop_turns = 16;
        }
        cfg
    }
}

/// Tracks convergence signals across turns.
#[derive(Debug, Default)]
#[allow(dead_code)]
pub(crate) struct ConvergenceDetector {
    config: ConvergenceConfig,
    /// Consecutive turns where tools were called but no visible text was produced.
    no_progress_count: u32,
    /// Cumulative turns (this run) where tools were called but no visible
    /// text was produced. Never reset by a talking turn — see
    /// [`ConvergenceConfig::max_silent_tool_turns`].
    silent_tool_turns_total: u32,
    /// Whether the cumulative silent-tool warning already fired this run.
    silent_tool_warned: bool,
    /// Consecutive turns where ALL tool calls errored.
    error_loop_count: u32,
    /// Per-tool consecutive error counts. Key = tool name, value =
    /// consecutive turns where this tool errored. Reset when the tool
    /// succeeds or when a different tool errors.
    per_tool_error_counts: std::collections::HashMap<String, u32>,
    /// Consecutive turns where visible text was >threshold similar to the previous turn.
    similar_content_count: u32,
    /// Last turn's visible text (for similarity comparison).
    last_visible_text: String,
    /// Whether a NoProgressWithTools warning has already been issued
    /// this run. We only warn once to avoid flooding the context.
    no_progress_warned: bool,
    /// Whether a SimilarContent warning has already been issued this run.
    similar_content_warned: bool,
    /// Set of tools for which we've already issued a per-tool error
    /// warning this run. Prevents repeated warnings for the same tool.
    per_tool_warned: std::collections::HashSet<String>,
}

impl ConvergenceDetector {
    #[allow(dead_code)]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    #[allow(dead_code)]
    pub(crate) fn with_config(config: ConvergenceConfig) -> Self {
        Self {
            config,
            ..Default::default()
        }
    }

    /// Record a completed turn and check for convergence.
    /// Parameters:
    /// - `has_visible_text`: whether the assistant produced user-visible text this turn
    /// - `tool_call_count`: number of FRESH tool calls made this turn (0 = no tools)
    /// - `tool_error_count`: number of tool calls that errored this turn
    /// - `visible_text`: the visible text produced this turn (empty if none)
    /// - `failed_tool_names`: names of tools that errored this turn
    /// - `succeeded_tool_names`: names of tools that succeeded this turn
    pub(crate) fn check(
        &mut self,
        has_visible_text: bool,
        tool_call_count: usize,
        tool_error_count: usize,
        visible_text: &str,
        failed_tool_names: &[&str],
        succeeded_tool_names: &[&str],
    ) -> ConvergenceSignal {
        // --- No-progress detection (advisory, not hard stop) ---
        // Tools were called but no visible text was produced.
        // The LLM might be making progress through tool chains.
        // Issue a one-time warning; don't kill the loop.
        if tool_call_count > 0 && !has_visible_text {
            self.no_progress_count += 1;
        } else {
            self.no_progress_count = 0;
            self.no_progress_warned = false; // reset so a future streak can warn again
        }

        if self.no_progress_count >= self.config.max_no_progress_turns && !self.no_progress_warned {
            self.no_progress_warned = true;
            let count = self.no_progress_count;
            return ConvergenceSignal::Warn {
                reason: ConvergenceReason::NoProgressWithTools,
                consecutive_count: count,
                message: format!(
                    "You have called tools for {} consecutive turns without producing any visible text for the user. \
                     If you have gathered enough information, please summarize your findings now. \
                     If you still need more information, continue with tool calls but consider providing \
                     a brief progress update to the user.",
                    count
                ),
            };
        }

        // --- No-progress escalation to HARD STOP ---
        // After the advisory warning, if the LLM continues without
        // visible text past the hard-stop threshold, kill the loop.
        // This prevents infinite tool chaining that exhausts the
        // context budget.
        if self.config.max_no_progress_hard_stop_turns > 0
            && self.no_progress_count >= self.config.max_no_progress_hard_stop_turns
        {
            let count = self.no_progress_count;
            self.no_progress_count = 0; // reset after firing
            self.no_progress_warned = false;
            return ConvergenceSignal::Converge {
                reason: ConvergenceReason::NoProgressWithTools,
                consecutive_count: count,
            };
        }

        // --- Cumulative silent-tool-turn detection (advisory + hard stop) ---
        // The `no_progress_count` above only fires on a *consecutive*
        // streak. A loop that alternates "silent tool call" ↔ "tool call
        // + one line" (e.g. `terminal` → `write_file` → `terminal` → …)
        // resets the consecutive counter every other turn and so never
        // trips it, while still paying a full prompt every turn and never
        // telling the user anything real. Count the *total* silent-tool
        // turns instead; it is deliberately NOT reset by a talking turn
        // because a one-line "wrote to file" note is not a user-facing
        // progress report.
        if tool_call_count > 0 && !has_visible_text {
            self.silent_tool_turns_total += 1;
        }

        if self.silent_tool_turns_total >= self.config.max_silent_tool_turns
            && !self.silent_tool_warned
        {
            self.silent_tool_warned = true;
            let count = self.silent_tool_turns_total;
            return ConvergenceSignal::Warn {
                reason: ConvergenceReason::NoProgressWithTools,
                consecutive_count: count,
                message: format!(
                    "You have made tool calls in {count} turns of this run without producing any \
                     visible text for the user. If you are stuck in a repeat loop (for example \
                     observing something and then writing the same status to a file over and over), \
                     stop now and give the user a real summary: what was done, what was actually \
                     verified, and what remains. Do not keep observing without reporting.",
                ),
            };
        }

        if self.config.max_silent_tool_hard_stop_turns > 0
            && self.silent_tool_turns_total >= self.config.max_silent_tool_hard_stop_turns
        {
            let count = self.silent_tool_turns_total;
            self.silent_tool_turns_total = 0; // reset after firing
            self.silent_tool_warned = false;
            return ConvergenceSignal::Converge {
                reason: ConvergenceReason::NoProgressWithTools,
                consecutive_count: count,
            };
        }

        // --- Per-tool error tracking (advisory WARN) ---
        // Even if not ALL tools errored, if the SAME tool keeps failing,
        // the LLM should switch approach. This catches the case where
        // the LLM interleaves a failing terminal call with a successful
        // read_file — the old logic would reset error_loop_count, masking
        // the terminal failure pattern.
        for name in succeeded_tool_names {
            self.per_tool_error_counts.remove(*name);
        }
        for name in failed_tool_names {
            let count = self.per_tool_error_counts.entry((*name).to_string()).or_insert(0);
            *count += 1;
        }
        // Check if any tool has exceeded the per-tool threshold
        for (name, &count) in &self.per_tool_error_counts {
            if count >= self.config.max_per_tool_error_turns && !self.per_tool_warned.contains(name) {
                self.per_tool_warned.insert(name.clone());
                return ConvergenceSignal::Warn {
                    reason: ConvergenceReason::NoProgressWithTools,
                    consecutive_count: count,
                    message: format!(
                        "Tool `{}` has failed for {} consecutive turns. \
                         You MUST switch to a different approach or tool — do NOT retry `{}` again. \
                         If no alternative exists, tell the user what you've tried and ask for guidance.",
                        name, count, name
                    ),
                };
            }
        }

        // --- Error loop detection (HARD STOP) ---
        // ALL tool calls errored. This is the strongest stuck signal.
        if tool_call_count > 0 && tool_error_count == tool_call_count {
            self.error_loop_count += 1;
        } else {
            self.error_loop_count = 0;
        }

        if self.error_loop_count >= self.config.max_error_turns {
            let count = self.error_loop_count;
            self.error_loop_count = 0; // reset after firing
            return ConvergenceSignal::Converge {
                reason: ConvergenceReason::ErrorLoop,
                consecutive_count: count,
            };
        }

        // --- Similar content detection (advisory, not hard stop) ---
        // Only check when there IS visible text.
        if has_visible_text && !visible_text.is_empty() {
            if !self.last_visible_text.is_empty() {
                let sim = jaccard_similarity(&self.last_visible_text, visible_text);
                // High Jaccard alone is a false-positive magnet: long
                // structured replies (status lists, per-file receipts,
                // repeated headers) share most template vocabulary with
                // the previous turn while carrying genuinely new content.
                // Require the turn to add few NEW words before counting
                // it as a repeat — genuine verbatim loops add ~0.
                let new_ratio = new_word_ratio(&self.last_visible_text, visible_text);
                if sim >= self.config.similarity_threshold
                    && new_ratio < SIMILAR_CONTENT_NEW_WORD_FLOOR
                {
                    self.similar_content_count += 1;
                } else {
                    self.similar_content_count = 0;
                    self.similar_content_warned = false;
                }
            }
            self.last_visible_text = visible_text.to_string();
        } else {
            // No visible text — reset similarity tracking
            self.similar_content_count = 0;
            self.similar_content_warned = false;
        }

        if self.similar_content_count >= self.config.max_similar_content_turns && !self.similar_content_warned {
            self.similar_content_warned = true;
            let count = self.similar_content_count;
            return ConvergenceSignal::Warn {
                reason: ConvergenceReason::SimilarContent,
                consecutive_count: count,
                message: format!(
                    "Your recent responses have been very similar for {} consecutive turns. \
                     If the task is complete, please provide a final summary. \
                     If you're stuck, try a different approach.",
                    count
                ),
            };
        }

        // --- Similar content escalation to HARD STOP ---
        if self.config.max_similar_content_hard_stop_turns > 0
            && self.similar_content_count >= self.config.max_similar_content_hard_stop_turns
        {
            let count = self.similar_content_count;
            self.similar_content_count = 0; // reset after firing
            self.similar_content_warned = false;
            self.last_visible_text.clear();
            return ConvergenceSignal::Converge {
                reason: ConvergenceReason::SimilarContent,
                consecutive_count: count,
            };
        }

        ConvergenceSignal::Continue
    }
}

/// Word-level token set with punctuation stripped from token edges, so
/// "successfully." and "successfully" compare equal instead of masquerading
/// as distinct vocabulary (which inflated new-word counts and depressed
/// Jaccard on templated replies).
fn tokenize(s: &str) -> std::collections::HashSet<&str> {
    s.split_whitespace()
        .map(|w| w.trim_matches(|c: char| c.is_ascii_punctuation()))
        .filter(|w| !w.is_empty())
        .collect()
}

/// Compute Jaccard similarity between two strings (word-level).
/// Returns a value between 0.0 (completely different) and 1.0 (identical).
fn jaccard_similarity(a: &str, b: &str) -> f64 {
    let set_a = tokenize(a);
    let set_b = tokenize(b);

    if set_a.is_empty() && set_b.is_empty() {
        return 1.0;
    }
    if set_a.is_empty() || set_b.is_empty() {
        return 0.0;
    }

    let intersection = set_a.intersection(&set_b).count();
    let union = set_a.union(&set_b).count();

    intersection as f64 / union as f64
}

/// Fraction of `b`'s distinct words that do NOT appear in `a`.
/// Measures how much *new* vocabulary the latest turn introduced.
fn new_word_ratio(a: &str, b: &str) -> f64 {
    let set_a = tokenize(a);
    let set_b = tokenize(b);
    if set_b.is_empty() {
        return 0.0;
    }
    let new_words = set_b.difference(&set_a).count();
    new_words as f64 / set_b.len() as f64
}

/// A turn only counts as "similar" to the previous one when it adds less
/// than this fraction of new distinct words. Genuine verbatim loops add
/// ~0; templated but progressing replies add 15-50%.
const SIMILAR_CONTENT_NEW_WORD_FLOOR: f64 = 0.15;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_for_domain_operate_relaxes_similar_content() {
        let cfg = ConvergenceConfig::for_domain(Some(hermix_intelligence::DomainMode::Operate));
        assert_eq!(cfg.max_similar_content_turns, 8);
        assert_eq!(cfg.max_similar_content_hard_stop_turns, 16);
        // 其余阈值不变——只放宽 similar-content
        assert_eq!(cfg.max_no_progress_turns, 15);
        assert_eq!(cfg.max_error_turns, 3);
    }

    #[test]
    fn test_for_domain_non_operate_keeps_defaults() {
        for mode in [
            hermix_intelligence::DomainMode::Chat,
            hermix_intelligence::DomainMode::Code,
            hermix_intelligence::DomainMode::Research,
            hermix_intelligence::DomainMode::Write,
        ] {
            let cfg = ConvergenceConfig::for_domain(Some(mode));
            assert_eq!(cfg.max_similar_content_turns, 5, "mode={mode:?}");
        }
        let cfg = ConvergenceConfig::for_domain(None);
        assert_eq!(cfg.max_similar_content_turns, 5);
    }

    #[test]
    fn test_no_progress_warns_but_does_not_kill() {
        let mut d = ConvergenceDetector::with_config(ConvergenceConfig {
            max_no_progress_turns: 3,
            ..Default::default()
        });

        assert_eq!(
            d.check(false, 2, 0, "", &[], &[]),
            ConvergenceSignal::Continue
        );
        assert_eq!(
            d.check(false, 1, 0, "", &[], &[]),
            ConvergenceSignal::Continue
        );
        // Third no-progress turn: advisory warning, NOT hard stop
        let signal = d.check(false, 1, 0, "", &[], &[]);
        match signal {
            ConvergenceSignal::Warn { reason: ConvergenceReason::NoProgressWithTools, .. } => {}
            other => panic!("expected Warn(NoProgressWithTools), got {:?}", other),
        }
        // Fourth no-progress turn: no repeat warning (already warned)
        assert_eq!(
            d.check(false, 1, 0, "", &[], &[]),
            ConvergenceSignal::Continue
        );
    }

    #[test]
    fn test_progress_resets_counter() {
        let mut d = ConvergenceDetector::with_config(ConvergenceConfig {
            max_no_progress_turns: 3,
            ..Default::default()
        });

        d.check(false, 2, 0, "", &[], &[]); // 1
        d.check(false, 1, 0, "", &[], &[]); // 2
        d.check(true, 0, 0, "Done!", &[], &[]); // visible text -> reset
        d.check(false, 1, 0, "", &[], &[]); // 1 again
        assert_eq!(
            d.check(false, 1, 0, "", &[], &[]),
            ConvergenceSignal::Continue // still at 2, not 3
        );
    }

    #[test]
    fn test_error_loop_is_hard_stop() {
        let mut d = ConvergenceDetector::with_config(ConvergenceConfig {
            max_error_turns: 2,
            ..Default::default()
        });

        // Turn 1: 2 tools, both error
        assert_eq!(
            d.check(false, 2, 2, "", &["terminal", "read_file"], &[]),
            ConvergenceSignal::Continue
        );
        // Turn 2: 1 tool, error -> hard stop (Converge)
        assert_eq!(
            d.check(false, 1, 1, "", &["terminal"], &[]),
            ConvergenceSignal::Converge {
                reason: ConvergenceReason::ErrorLoop,
                consecutive_count: 2,
            }
        );
    }

    #[test]
    fn test_partial_errors_do_not_trigger_error_loop() {
        // If some tools succeeded and some failed, it's not an error loop.
        let mut d = ConvergenceDetector::with_config(ConvergenceConfig {
            max_error_turns: 2,
            ..Default::default()
        });

        d.check(false, 2, 1, "", &["terminal"], &["read_file"]); // 2 tools, 1 error -> not all errors
        assert_eq!(
            d.check(false, 2, 1, "", &["terminal"], &["read_file"]),
            ConvergenceSignal::Continue
        );
    }

    #[test]
    fn test_per_tool_error_triggers_warning() {
        // Even if other tools succeed, if the SAME tool keeps failing,
        // a per-tool warning should fire.
        let mut d = ConvergenceDetector::with_config(ConvergenceConfig {
            max_per_tool_error_turns: 3,
            ..Default::default()
        });

        // terminal fails 2 turns, but read_file succeeds — no warning yet
        d.check(false, 2, 1, "", &["terminal"], &["read_file"]);
        d.check(false, 2, 1, "", &["terminal"], &["read_file"]);
        // Third consecutive terminal failure -> warning
        let signal = d.check(false, 2, 1, "", &["terminal"], &["read_file"]);
        match signal {
            ConvergenceSignal::Warn { message, .. } => {
                assert!(message.contains("terminal"));
                assert!(message.contains("switch"));
            }
            other => panic!("expected Warn for terminal, got {:?}", other),
        }
    }

    #[test]
    fn test_per_tool_error_resets_on_success() {
        let mut d = ConvergenceDetector::with_config(ConvergenceConfig {
            max_per_tool_error_turns: 3,
            ..Default::default()
        });

        // terminal fails 2 turns
        d.check(false, 1, 1, "", &["terminal"], &[]);
        d.check(false, 1, 1, "", &["terminal"], &[]);
        // terminal succeeds -> counter reset
        d.check(false, 1, 0, "", &[], &["terminal"]);
        // terminal fails again — should be at 1, not 3
        assert_eq!(
            d.check(false, 1, 1, "", &["terminal"], &[]),
            ConvergenceSignal::Continue
        );
    }

    #[test]
    fn test_default_no_progress_threshold_is_15() {
        let config = ConvergenceConfig::default();
        assert_eq!(config.max_no_progress_turns, 15);
        assert_eq!(config.max_no_progress_hard_stop_turns, 25);
    }

    #[test]
    fn test_no_progress_warns_then_escalates_to_hard_stop() {
        let mut d = ConvergenceDetector::with_config(ConvergenceConfig {
            max_no_progress_turns: 3,
            max_no_progress_hard_stop_turns: 5,
            ..Default::default()
        });

        // Turns 1-2: no warning yet
        assert_eq!(d.check(false, 1, 0, "", &[], &[]), ConvergenceSignal::Continue);
        assert_eq!(d.check(false, 1, 0, "", &[], &[]), ConvergenceSignal::Continue);
        // Turn 3: advisory warning
        assert!(matches!(
            d.check(false, 1, 0, "", &[], &[]),
            ConvergenceSignal::Warn { reason: ConvergenceReason::NoProgressWithTools, .. }
        ));
        // Turn 4: no repeat warning, still Continue
        assert_eq!(d.check(false, 1, 0, "", &[], &[]), ConvergenceSignal::Continue);
        // Turn 5: HARD STOP
        let signal = d.check(false, 1, 0, "", &[], &[]);
        assert_eq!(
            signal,
            ConvergenceSignal::Converge {
                reason: ConvergenceReason::NoProgressWithTools,
                consecutive_count: 5,
            }
        );
    }

    #[test]
    fn test_no_progress_hard_stop_disabled_when_zero() {
        let mut d = ConvergenceDetector::with_config(ConvergenceConfig {
            max_no_progress_turns: 2,
            max_no_progress_hard_stop_turns: 0, // disabled
            // Also disable the cumulative silent-tool hard stop so this
            // test isolates exactly the consecutive no-progress path.
            max_silent_tool_hard_stop_turns: 0,
            ..Default::default()
        });

        d.check(false, 1, 0, "", &[], &[]);
        let _ = d.check(false, 1, 0, "", &[], &[]); // warns
        // With hard stop disabled, should never Converge
        for _ in 0..20 {
            let signal = d.check(false, 1, 0, "", &[], &[]);
            assert!(!matches!(signal, ConvergenceSignal::Converge { .. }));
        }
    }

    #[test]
    fn test_default_error_loop_threshold_is_3() {
        let config = ConvergenceConfig::default();
        assert_eq!(config.max_error_turns, 3);
    }

    #[test]
    fn test_default_similar_content_threshold_is_5() {
        let config = ConvergenceConfig::default();
        assert_eq!(config.max_similar_content_turns, 5);
        assert_eq!(config.max_similar_content_hard_stop_turns, 10);
    }

    #[test]
    fn test_similar_content_warns_but_does_not_kill() {
        let mut d = ConvergenceDetector::with_config(ConvergenceConfig {
            max_similar_content_turns: 2,
            similarity_threshold: 0.7,
            ..Default::default()
        });

        d.check(true, 0, 0, "The file has been created successfully", &[], &[]);
        d.check(true, 0, 0, "The file has been created successfully.", &[], &[]);
        let signal = d.check(true, 0, 0, "The file has been created successfully!", &[], &[]);
        match signal {
            ConvergenceSignal::Warn { reason: ConvergenceReason::SimilarContent, .. } => {}
            other => panic!("expected Warn(SimilarContent), got {:?}", other),
        }
        // No repeat warning
        assert_eq!(
            d.check(true, 0, 0, "The file has been created successfully!!", &[], &[]),
            ConvergenceSignal::Continue
        );
    }

    #[test]
    fn test_jaccard_similarity() {
        assert!((jaccard_similarity("hello world", "hello world") - 1.0).abs() < 0.01);
        assert!((jaccard_similarity("hello world", "goodbye world") - 0.33).abs() < 0.1);
        assert!((jaccard_similarity("", "") - 1.0).abs() < 0.01);
        assert!((jaccard_similarity("hello", "") - 0.0).abs() < 0.01);
    }

    #[test]
    fn templated_but_new_content_does_not_count_as_similar() {
        // Long structured replies (per-file receipts, status lists) share
        // template vocabulary between turns while introducing genuinely
        // new tokens. High Jaccard alone must not accumulate the
        // similar-content counter — the new-word floor blocks it.
        let mut d = ConvergenceDetector::default();
        let t1 = "已完成批量重命名 status: done path: C:/a/old1.txt -> C:/a/new1.txt";
        let t2 = "已完成批量重命名 status: done path: C:/a/old2.txt -> C:/a/new2.txt";
        d.check(true, 0, 0, t1, &[], &[]);
        // jaccard(t1,t2) is high (~0.7+ with normalized tokens) but
        // new_word_ratio = 2/8 = 0.25 >= 0.15 → must reset, not count.
        assert_eq!(
            d.check(true, 0, 0, t2, &[], &[]),
            ConvergenceSignal::Continue
        );
    }

    #[test]
    fn verbatim_repeat_with_new_punctuation_still_counts() {
        // Punctuation-only variation must NOT evade the detector.
        let mut d = ConvergenceDetector::with_config(ConvergenceConfig {
            max_similar_content_turns: 2,
            max_similar_content_hard_stop_turns: 0,
            ..Default::default()
        });
        d.check(true, 0, 0, "The file has been created successfully", &[], &[]);
        d.check(true, 0, 0, "The file has been created successfully.", &[], &[]);
        assert!(matches!(
            d.check(true, 0, 0, "The file has been created successfully!", &[], &[]),
            ConvergenceSignal::Warn {
                reason: ConvergenceReason::SimilarContent,
                ..
            }
        ));
    }

    #[test]
    fn test_visible_text_resets_no_progress_counter() {
        let mut d = ConvergenceDetector::with_config(ConvergenceConfig {
            max_no_progress_turns: 3,
            max_error_turns: 100, // high to avoid ErrorLoop firing
            ..Default::default()
        });

        d.check(false, 2, 0, "", &[], &[]); // no-progress 1
        d.check(false, 1, 0, "", &[], &[]); // no-progress 2
        d.check(true, 0, 0, "Partial success", &[], &[]); // visible -> reset
        d.check(false, 1, 0, "", &[], &[]); // no-progress 1 again
        assert_eq!(
            d.check(false, 1, 0, "", &[], &[]),
            ConvergenceSignal::Continue // still at 2, not 3
        );
    }

    #[test]
    fn test_no_progress_warning_resets_after_visible_text() {
        // After a warning is issued, if the LLM produces visible text
        // and then goes back to no-progress, a new warning can fire.
        let mut d = ConvergenceDetector::with_config(ConvergenceConfig {
            max_no_progress_turns: 2,
            ..Default::default()
        });

        // 2 no-progress turns → warning
        d.check(false, 1, 0, "", &[], &[]);
        let signal = d.check(false, 1, 0, "", &[], &[]);
        assert!(matches!(signal, ConvergenceSignal::Warn { .. }));

        // No repeat warning
        assert_eq!(d.check(false, 1, 0, "", &[], &[]), ConvergenceSignal::Continue);

        // LLM produces visible text → resets
        d.check(true, 0, 0, "Progress update", &[], &[]);

        // New no-progress streak → can warn again
        d.check(false, 1, 0, "", &[], &[]);
        let signal = d.check(false, 1, 0, "", &[], &[]);
        assert!(matches!(signal, ConvergenceSignal::Warn { .. }));
    }

    // ===== Cumulative silent-tool-turn detection =====

    #[test]
    fn alternating_silent_tool_turns_trip_cumulative_warning() {
        // `terminal` (silent) ↔ `write_file` (one line) alternation: the
        // *consecutive* no-progress counter is reset every other turn, so
        // it never reaches its threshold. The *cumulative* silent-tool
        // counter must still fire.
        let mut d = ConvergenceDetector::with_config(ConvergenceConfig {
            max_no_progress_turns: 100, // keep the consecutive path quiet
            max_silent_tool_turns: 4,
            max_silent_tool_hard_stop_turns: 0, // disable escalation for this test
            ..Default::default()
        });

        // 4 alternating turns: 2 silent (terminal) + 2 talking (write_file)
        assert_eq!(d.check(false, 1, 0, "", &[], &["terminal"]), ConvergenceSignal::Continue);
        assert_eq!(d.check(true, 1, 0, "wrote to file", &[], &["write_file"]), ConvergenceSignal::Continue);
        assert_eq!(d.check(false, 1, 0, "", &[], &["terminal"]), ConvergenceSignal::Continue);
        // talking turns must not count toward the silent total
        let signal = d.check(true, 1, 0, "wrote to file", &[], &["write_file"]);
        assert_eq!(signal, ConvergenceSignal::Continue, "talking turns must not count");
        assert_eq!(d.check(false, 1, 0, "", &[], &["terminal"]), ConvergenceSignal::Continue);
        let signal = d.check(false, 1, 0, "", &[], &["terminal"]);
        assert!(
            matches!(signal, ConvergenceSignal::Warn { reason: ConvergenceReason::NoProgressWithTools, .. }),
            "cumulative silent turns must trip an advisory warning, got {:?}",
            signal
        );
    }

    #[test]
    fn cumulative_silent_tool_turns_escalate_to_hard_stop() {
        let mut d = ConvergenceDetector::with_config(ConvergenceConfig {
            max_no_progress_turns: 100,
            max_silent_tool_turns: 2,
            max_silent_tool_hard_stop_turns: 4,
            ..Default::default()
        });

        // Two silent turns → warning; two more → hard stop, even though a
        // talking turn sits between each silent pair (so the consecutive
        // counter never advances).
        assert_eq!(d.check(false, 1, 0, "", &[], &[]), ConvergenceSignal::Continue);
        assert!(matches!(d.check(false, 1, 0, "", &[], &[]), ConvergenceSignal::Warn { .. }));
        assert_eq!(d.check(true, 1, 0, "progress", &[], &[]), ConvergenceSignal::Continue);
        assert_eq!(d.check(false, 1, 0, "", &[], &[]), ConvergenceSignal::Continue);
        let signal = d.check(false, 1, 0, "", &[], &[]);
        assert_eq!(
            signal,
            ConvergenceSignal::Converge {
                reason: ConvergenceReason::NoProgressWithTools,
                consecutive_count: 4,
            }
        );
    }

    #[test]
    fn talking_turns_do_not_inflate_silent_counter() {
        let mut d = ConvergenceDetector::default();
        // 20 talking tool turns → no silent warning, no hard stop. Distinct
        // visible text each turn so the (unrelated) similar-content detector
        // stays quiet.
        for i in 0..20 {
            let signal = d.check(true, 1, 0, &format!("reporting to user step {i}"), &[], &[]);
            assert_eq!(signal, ConvergenceSignal::Continue);
        }
        assert_eq!(d.silent_tool_turns_total, 0);
    }

}
