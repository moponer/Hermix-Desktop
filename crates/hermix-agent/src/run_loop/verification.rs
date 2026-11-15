//! Gather-act-verify: tool result verification notes
//!
//! Moved verbatim from `run_loop.rs` (pure relocation, no behavior
//! change). Methods are `pub(super)` because their callers live in the
//! parent `run_loop` module. All names resolve via the parent glob.

#[allow(unused_imports)]
use super::*;
use crate::agent_loop::extract_file_change;
use crate::context::ContextManager;

impl AgentLoop {
    /// Verify tool execution results and inject verification status.
    /// This is the "verify work" step from Claude Code's gather→act→verify pattern.
    /// Checks file modifications, terminal exit codes, and tool errors.
    pub fn verify_tool_results(
        &self,
        tool_calls: &[ToolCall],
        results: &[hermix_core::ToolResult],
    ) -> Vec<String> {
        let mut verification_notes = Vec::new();

        for res in results {
            let Some(tc) = tool_calls.iter().find(|tc| tc.id == res.tool_call_id) else {
                continue;
            };

            if res.is_error {
                // Tool execution failed
                verification_notes.push(format!(
                    "VERIFICATION: {} failed - {}",
                    tc.function.name, res.content
                ));
                continue;
            }

            // Check file modification results
            if let Some(change) = extract_file_change(&tc.function.name, &tc.function.arguments, &res.content) {
                verification_notes.push(format!(
                    "VERIFICATION: {} modified {} ({})",
                    tc.function.name, change.path, change.change_type
                ));
            }

            // Check terminal command results
            if tc.function.name == "terminal" {
                if let Ok(data) = serde_json::from_str::<Value>(&res.content) {
                    if let Some(exit_code) = data.get("exit_code").and_then(|v| v.as_i64()) {
                        if exit_code != 0 {
                            verification_notes.push(format!(
                                "VERIFICATION: {} exited with code {}",
                                tc.function.name, exit_code
                            ));
                        }
                    }
                }
            }
        }

        verification_notes
    }

    /// Inject verification notes into the context as a system message.
    pub fn inject_verification_notes(
        ctx: &mut ContextManager,
        notes: &[String],
    ) {
        if notes.is_empty() {
            return;
        }
        let verification_text = format!(
            "[VERIFICATION]\n{}\n\nIf any verification failed, investigate and fix before proceeding.",
            notes.join("\n")
        );
        ctx.add_message(Message::system(&verification_text));
    }
}
