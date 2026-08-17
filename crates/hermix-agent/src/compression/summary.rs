//! Summary generation: budget, serialize, prompt build, LLM summary, cooldown.
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use hermix_core::{
    AgentError, CompressionEngine, CompressionEngineMetrics, CompressionEngineSnapshot,
    CompressionEngineState, LlmProvider, Message, MessageRole,
};
use hermix_core::traits::CoreContextEngine;
use hermix_core::types::ContentPart;
use hermix_intelligence::auxiliary::{
    AuxiliaryClient, AuxiliaryError, AuxiliaryRequest, AuxiliaryTask,
};

use super::*;

impl ContextCompressor {
    //summary budget + serialisation.
    // ------------------------------------------------------------------

    pub(crate) fn compute_summary_budget(&self, turns: &[Message]) -> u64 {
        let content_tokens = estimate_messages_tokens(turns);
        let budget = (content_tokens as f64 * self.config.summary_target_ratio) as u64;
        budget.clamp(
            self.config.min_summary_tokens,
            self.max_summary_tokens.max(self.config.min_summary_tokens),
        )
    }

    pub(crate) fn serialize_for_summary(&self, turns: &[Message]) -> String {
        let mut parts = Vec::with_capacity(turns.len());
        for msg in turns {
            let role = msg.role;
            let raw_content = msg.content.clone().unwrap_or_default();

            // Redact sensitive information and filter MEDIA directives.
            let redacted = redact_sensitive_text(&raw_content);
            let filtered = MEDIA_DIRECTIVE_RE.replace_all(&redacted, "[media attachment]");
            let content = truncate_middle(&filtered, CONTENT_MAX, CONTENT_HEAD, CONTENT_TAIL);
            match role {
                MessageRole::Tool => {
                    let id = msg.tool_call_id.as_deref().unwrap_or("");
                    parts.push(format!("[TOOL RESULT {}]: {}", id, content));
                }
                MessageRole::Assistant => {
                    let mut body = content;
                    if let Some(tcs) = msg.tool_calls.as_ref() {
                        if !tcs.is_empty() {
                            let mut tc_parts = Vec::with_capacity(tcs.len());
                            for tc in tcs {
                                let args = if tc.function.arguments.len() > TOOL_ARGS_MAX {
                                    format!("{}...", &tc.function.arguments[..tc.function.arguments.floor_char_boundary(TOOL_ARGS_HEAD)])
                                } else {
                                    tc.function.arguments.clone()
                                };
                                tc_parts.push(format!("  {}({})", tc.function.name, args));
                            }
                            body.push_str("\n[Tool calls:\n");
                            body.push_str(&tc_parts.join("\n"));
                            body.push_str("\n]");
                        }
                    }
                    parts.push(format!("[ASSISTANT]: {}", body));
                }
                _ => {
                    parts.push(format!(
                        "[{}]: {}",
                        role_label(role).to_uppercase(),
                        content
                    ));
                }
            }
        }
        parts.join("\n\n")
    }

    pub(crate) fn build_summary_prompt(&self, content_block: &str, summary_budget: u64) -> String {
        if let Some(prev) = self.previous_summary.as_ref() {
            format!(
                "You are updating a context compaction summary. A previous compaction produced the summary below. \
New conversation turns have occurred since then and need to be incorporated.\n\n\
PREVIOUS SUMMARY:\n{prev}\n\nNEW TURNS TO INCORPORATE:\n{content_block}\n\n\
Update the summary using this exact structure. PRESERVE all existing information that is still relevant. \
ADD new progress. Move items from \"In Progress\" to \"Done\" when completed. Remove information only if it is clearly obsolete.\n\n\
{COMPACTION_TEMPLATE}\n\n\
Target ~{summary_budget} tokens. Be specific — include file paths, command outputs, error messages, and concrete values rather than vague descriptions.\n\n\
Write only the summary body. Do not include any preamble or prefix."
            )
        } else {
            format!(
                "Create a structured handoff summary for a later assistant that will continue this conversation after earlier turns are compacted.\n\n\
TURNS TO SUMMARIZE:\n{content_block}\n\nUse this exact structure:\n\n{COMPACTION_TEMPLATE}\n\n\
Target ~{summary_budget} tokens. Be specific — include file paths, command outputs, error messages, and concrete values rather than vague descriptions. \
The goal is to prevent the next assistant from repeating work or losing important details.\n\n\
Write only the summary body. Do not include any preamble or prefix."
            )
        }
    }

    /// Generate a structured summary of the supplied turns using the
    /// auxiliary client.
    /// Returns `Ok(Some(summary))` on success, `Ok(None)` when the cooldown
    /// is active, and `Err(...)` for hard auxiliary failures (which also
    /// arm the cooldown for `SUMMARY_FAILURE_COOLDOWN`).
    pub async fn generate_summary(
        &mut self,
        turns: &[Message],
    ) -> Result<Option<String>, CompressionError> {
        if let Some(deadline) = self.failure_cooldown_until {
            let now = Instant::now();
            if now < deadline {
                return Err(CompressionError::CooldownActive(deadline - now));
            }
            self.failure_cooldown_until = None;
        }

        let budget = self.compute_summary_budget(turns);
        let block = self.serialize_for_summary(turns);
        let prompt = self.build_summary_prompt(&block, budget);

        let mut request =
            AuxiliaryRequest::new(AuxiliaryTask::Compression, vec![Message::user(prompt)])
                .with_max_tokens((budget * 2) as u32);
        if let Some(model) = self.config.summary_model_override.as_ref() {
            request = request.with_model(model.clone());
        }

        match self.auxiliary.call(request).await {
            Ok(resp) => {
                let body = resp
                    .text()
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                if body.is_empty() {
                    self.arm_cooldown();
                    Ok(None)
                } else {
                    self.previous_summary = Some(body.clone());
                    Ok(Some(with_summary_prefix(&body)))
                }
            }
            Err(err) => {
                let cooldown = self.arm_cooldown();
                if !self.config.quiet_mode {
                    tracing::warn!(
                        "Failed to generate context summary: {err}. \
                         Further summary attempts paused for {:?}.",
                        cooldown
                    );
                }
                Err(CompressionError::Auxiliary(err))
            }
        }
    }

    /// Arm the summary-failure cooldown.
    /// Returns the duration actually armed. The cooldown is shortened to
    /// [`SUMMARY_FAILURE_COOLDOWN_SHORT`] when the auxiliary chain only has
    /// a single registered candidate — a one-candidate chain is almost
    /// always the result of a missing/invalid env var (`OPENAI_API_KEY`,
    /// `MINIMAX_API_KEY`, …). In that case the user can fix the env and
    /// expect the next compression to retry on the order of seconds, not
    /// ten minutes. Multi-candidate chains keep the long cooldown because
    /// failures there usually mean quota exhaustion / network issues that
    /// won't recover in seconds.
    pub(crate) fn arm_cooldown(&mut self) -> Duration {
        let chain_len = self.auxiliary.chain_len();
        let cooldown = if chain_len <= 1 {
            if !self.config.quiet_mode {
                tracing::info!(
                    "Auxiliary chain has {} candidate; shortening summary cooldown \
                     to {:?} so an env-var fix retries on the next attempt rather \
                     than after the full {:?} wait.",
                    chain_len,
                    SUMMARY_FAILURE_COOLDOWN_SHORT,
                    SUMMARY_FAILURE_COOLDOWN,
                );
            }
            SUMMARY_FAILURE_COOLDOWN_SHORT
        } else {
            SUMMARY_FAILURE_COOLDOWN
        };
        self.failure_cooldown_until = Some(Instant::now() + cooldown);
        cooldown
    }
}
