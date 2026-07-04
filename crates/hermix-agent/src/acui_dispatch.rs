//! ACUI directive dispatch helpers used by `agent_loop`.
//!
//! Extracted from `agent_loop.rs` (P2.x, 2026-07-04) so the 10k-line
//! `agent_loop.rs` can focus on the `impl AgentLoop` block. **No
//! behavior change** — pure relocation.
//!
//! Items living here:
//! - `emit_acui` — fire an `AcuiDirective` through the
//!   `AgentCallbacks::on_acui_directive` channel.
//! - `emit_acui_show` / `emit_acui_update` / `emit_acui_hide` —
//!   convenience wrappers around the three variant constructors.
//! - `default_tool_router` — Phase 7 H1 default
//!   `KeywordRouter + AlwaysInclude` wiring.

use std::sync::Arc;

use hermix_core::{AcuiDirective, AcuiHide, AcuiShow, AcuiUpdate};
use hermix_intelligence::{AlwaysInclude, KeywordRouter, ToolSchemaRouter};

use crate::agent_helpers::AgentCallbacks;

/// Fire an `AcuiDirective` through `AgentCallbacks::on_acui_directive`.
///
/// This function never blocks and never returns an error — the ACUI
/// pipeline is best-effort. A failure in the callback is logged and
/// swallowed (the agent keeps running).
pub fn emit_acui(callbacks: &AgentCallbacks, directive: AcuiDirective) {
    let Some(cb) = callbacks.on_acui_directive.as_ref() else {
        return;
    };
    match serde_json::to_string(&directive) {
        Ok(payload) => cb(&payload),
        Err(e) => {
            tracing::warn!(
                target: "hermix.agent",
                error = %e,
                "failed to serialize AcuiDirective, dropping emit"
            );
        }
    }
}

/// Convenience: emit a `Show` directive.
pub fn emit_acui_show(
    callbacks: &AgentCallbacks,
    id: impl Into<String>,
    kind: impl Into<String>,
    props: serde_json::Value,
) {
    emit_acui(callbacks, AcuiDirective::Show(AcuiShow::new(id, kind, props)));
}

/// Convenience: emit an `Update` directive.
pub fn emit_acui_update(callbacks: &AgentCallbacks, id: impl Into<String>, patch: serde_json::Value) {
    emit_acui(callbacks, AcuiDirective::Update(AcuiUpdate::new(id, patch)));
}

/// Convenience: emit a `Hide` directive.
pub fn emit_acui_hide(callbacks: &AgentCallbacks, id: impl Into<String>) {
    emit_acui(callbacks, AcuiDirective::Hide(AcuiHide::new(id)));
}

/// Build a default router (KeywordRouter + AlwaysInclude).
/// Convenience for callers who want the Phase 7 H1 default without
/// hand-rolling the wiring.
pub fn default_tool_router() -> Arc<dyn ToolSchemaRouter> {
    let kw = Arc::new(KeywordRouter::new());
    Arc::new(AlwaysInclude::new(
        kw,
        vec![
            "read_file".to_string(),
            "write_file".to_string(),
            "terminal".to_string(),
            "memory".to_string(),
            // read_pdf and list_files must be always-forwarded so the
            // LLM does not fall back to "guess the path via terminal"
            // when the user supplies an explicit file path (e.g.
            // "根据 G:\…\112号…通知.pdf 起草通知").  Previously the
            // KeywordRouter scored read_pdf ≈ 0 on CJK-heavy queries,
            // causing the agent to waste turns on terminal cd/ls
            // instead of calling the tool directly.
            "read_pdf".to_string(),
            "list_files".to_string(),
            // Browser and computer-use tools must be always-forwarded
            // so the LLM can access them when the user explicitly
            // requests web browsing, page interaction, or GUI
            // automation. Without this, KeywordRouter scores them ≈ 0
            // on CJK-heavy queries and the LLM never sees them.
            "browser_navigate".to_string(),
            "browser_snapshot".to_string(),
            "browser_click".to_string(),
            "browser_type".to_string(),
            "browser_scroll".to_string(),
            "computer_use".to_string(),
        ],
    ))
}