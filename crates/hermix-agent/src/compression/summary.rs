//! Summary generation: budget, serialize, prompt build, LLM summary, cooldown.
use std::time::{Duration, Instant};

/// Soft cap for the serialized summary block (≈60K tokens at 4 chars/token).
/// Keeps the LLM summary call inside a practical input size so generation
/// finishes within the aux chain's per-candidate timeout instead of
/// timing out and freezing the stream.
const MAX_SERIALIZE_CHARS: usize = 240_000;


use hermix_core::{
    Message, MessageRole,
};
use hermix_intelligence::auxiliary::{
    AuxiliaryRequest, AuxiliaryTask,
};

use super::*;

/// Remove a previously injected task-tree section from a summary body.
///
/// A second compaction re-summarizes the *previous summary* (which
/// already carries the section), so without this the tree would appear
/// twice.
fn strip_task_tree_section(body: &str) -> String {
    let Some(start) = body.find(crate::compression::TASK_TREE_HEADING) else {
        return body.to_string();
    };
    let mut out = body[..start].trim_end().to_string();
    // The section runs to the end of the body; drop the separator that
    // preceded it.
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

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
        let joined = parts.join("\n\n");
        // Cap the serialized block (~60K tokens). An unbounded block
        // (485 msgs ≈ 150K+ tokens) makes the LLM summary outlast the
        // aux chain's per-candidate timeout and freeze the stream for
        // N×45s. Head+tail preservation keeps the oldest task context
        // and the newest work leading into the protected tail.
        if joined.len() > MAX_SERIALIZE_CHARS {
            truncate_middle(
                &joined,
                MAX_SERIALIZE_CHARS,
                MAX_SERIALIZE_CHARS * 2 / 3,
                MAX_SERIALIZE_CHARS / 3,
            )
        } else {
            joined
        }
    }

    pub(crate) fn build_summary_prompt(&self, content_block: &str, summary_budget: u64) -> String {
        // Shared preservation rules — the compression policy must keep the
        // most recent task instruction and prioritise task goals / pending
        // todos over other history. Injected into both the create and update
        // branches so the model never silently drops a key requirement.
        const PRESERVE_RULES: &str = "\
CRITICAL PRESERVATION RULES (apply before condensing anything):\n\
• PRESERVE THE LAST TASK INSTRUCTION: the most recent user-assigned task/request must survive this summary. \
Reproduce its core requirements, acceptance criteria, file paths, and constraints VERBATIM or in near-verbatim detail — \
never paraphrase them into vague bullets that lose the actual ask.\n\
• PRIORITISE TASK GOALS AND PENDING TODOS: keep the '## Goal' and '## Progress → In Progress' / '## Next Steps' \
sections COMPLETE. When you must shorten, drop greetings, redundant commentary, and duplicate tool output FIRST — \
never sacrifice a task requirement, goal, or pending todo item to save space.\n\
• A pending task list must never be silently truncated, merged, or reordered away.\n\
• DROP SUPERSEDED OPTIONS: if the user's most recent message rejects, revises, or supersedes \
options / plans / alternatives that an earlier assistant turn proposed (e.g. the user abandons an \
offer of multiple choices and redirects the work, or issues a new instruction that replaces pending \
follow-ups), those superseded options MUST be removed from '## Next Steps' / '## In Progress' — do \
NOT carry them forward as pending work. The user's latest instruction is the authoritative task; \
when it conflicts with earlier proposed options or pending items, the earlier ones are obsolete and \
must be discarded so a downstream assistant does not re-issue or execute them.\n\n";

        if let Some(prev) = self.previous_summary.as_ref() {
            format!(
                "You are updating a context compaction summary. A previous compaction produced the summary below. \
New conversation turns have occurred since then and need to be incorporated.\n\n\
PREVIOUS SUMMARY:\n{prev}\n\nNEW TURNS TO INCORPORATE:\n{content_block}\n\n\
Update the summary using this exact structure. PRESERVE all existing information that is still relevant. \
ADD new progress. Move items from \"In Progress\" to \"Done\" when completed. Remove information only if it is clearly obsolete.\n\n\
{PRESERVE_RULES}\
{COMPACTION_TEMPLATE}\n\n\
Target ~{summary_budget} tokens. Be specific — include file paths, command outputs, error messages, and concrete values rather than vague descriptions.\n\n\
Write only the summary body. Do not include any preamble or prefix."
            )
        } else {
            format!(
                "Create a structured handoff summary for a later assistant that will continue this conversation after earlier turns are compacted.\n\n\
TURNS TO SUMMARIZE:\n{content_block}\n\nUse this exact structure:\n\n{PRESERVE_RULES}\
{COMPACTION_TEMPLATE}\n\n\
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
                    // ★ Append the authoritative task tree (C2). The LLM
                    // summary only *recollects* the task state; after a
                    // compaction the model must not lose track of which
                    // task it is on. The tree is read through the
                    // `task_list` tool — the same path the model uses —
                    // so it sees exactly the list it would get itself.
                    let body = self.append_task_tree(body).await;
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

    /// Append the session's task tree to a compaction summary.
    ///
    /// The section is **appended verbatim**, not asked of the
    /// summarising LLM: the task tree is authoritative state, and a
    /// model that re-derives it can drop or garble it. Appending also
    /// means the tree survives even when the summary call is degraded.
    ///
    /// Idempotent: a previously injected section is stripped first, so
    /// a second compaction does not carry two copies.
    async fn append_task_tree(&self, body: String) -> String {
        let Some(source) = self.task_source.as_ref() else {
            return body;
        };
        let Some(section) = self.render_task_tree(source).await else {
            return body;
        };
        let stripped = strip_task_tree_section(&body);
        if stripped.trim().is_empty() {
            section
        } else {
            format!("{stripped}\n\n{section}")
        }
    }

    /// Read the task tree through the `task_list` tool and render it.
    async fn render_task_tree(&self, source: &TaskTreeSource) -> Option<String> {
        use hermix_tools::context::ToolCallContext;
        let ctx = ToolCallContext::for_session(source.session_id.clone());
        let raw = source
            .registry
            .dispatch_async_with_context("task_list", serde_json::json!({}), &ctx)
            .await;
        let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;
        let tasks = parsed.get("tasks")?.as_array()?;
        let mut lines = Vec::new();
        for t in tasks {
            let status = t.get("status").and_then(|v| v.as_str()).unwrap_or("");
            // Terminal-and-successful tasks carry no forward-looking
            // state; the summary is about what is still to do.
            if matches!(status, "completed" | "archived") {
                continue;
            }
            let title = t.get("title").and_then(|v| v.as_str()).unwrap_or("(untitled)");
            let id = t.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let mine = t.get("mine").and_then(|v| v.as_bool()).unwrap_or(false);
            lines.push(format!(
                "- [{status}]{} {title} (id={id})",
                if mine { "[本会话]" } else { "" }
            ));
            if lines.len() >= crate::compression::TASK_TREE_MAX_ROWS {
                break;
            }
        }
        if lines.is_empty() {
            return None;
        }
        let mut out = String::from(crate::compression::TASK_TREE_HEADING);
        out.push_str(
            "\n以下是该会话任务树的**当前真实状态**（不是对话里的回忆）。\
只有标注「本会话」的任务属于本次对话；未标注的属于别的会话，不要改动。\n",
        );
        for l in lines {
            out.push_str(&l);
            out.push('\n');
        }
        Some(out.trim_end().to_string())
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
