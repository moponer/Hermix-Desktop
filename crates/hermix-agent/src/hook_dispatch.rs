//! Plugin-hook dispatch helpers used by `agent_loop`.
//!
//! Extracted from `agent_loop.rs` (P2.x, ) so the 10k-line
//! `agent_loop.rs` can focus on the `impl AgentLoop` block. **No
//! behavior change** — pure relocation.
//!
//! Items living here:
//! - `invoke_hook` — call `PluginManager::invoke_hook` if a plugin
//!   manager is attached; return empty when not.
//! - `inject_hook_context` — splice `InjectContext` results into the
//!   agent context as system messages.

use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::context::ContextManager;
use crate::plugins::{HookResult, HookType, PluginManager};

/// Invoke a plugin hook. Returns an empty vector when no plugin
/// manager is attached or the lock is poisoned.
pub fn invoke_hook(
    plugin_manager: Option<&Arc<Mutex<PluginManager>>>,
    hook: HookType,
    ctx_val: &Value,
) -> Vec<HookResult> {
    let Some(pm) = plugin_manager else {
        return Vec::new();
    };
    let Ok(pm) = pm.lock() else {
        return Vec::new();
    };
    pm.invoke_hook(hook, ctx_val)
}

/// Splice `HookResult::InjectContext(text)` results into `ctx` as
/// system messages. Other variants are ignored (callers that care
/// about `Block` / `Modify` examine `invoke_hook`'s return
/// directly).
pub fn inject_hook_context(results: &[HookResult], ctx: &mut ContextManager) {
    for r in results {
        if let HookResult::InjectContext(text) = r {
            if text.trim().is_empty() {
                continue;
            }
            // Append a NEW system message at the tail — never rewrite an
            // existing one.
            //
            // This used to append the text to the *last user message*, which
            // was already sent to the provider on earlier calls. Provider
            // prefix caches match bytes from position 0, so mutating that
            // user row invalidated every token after it (the whole
            // conversation tail) on each hook, and `PostLlmCall` fires once
            // per API call. Appending is free: the new row sits after the
            // cached prefix.
            //
            // Dedupe the exact same payload against the recent window —
            // otherwise a per-call hook with static text grows the history
            // without adding information.
            let already_present = ctx
                .get_messages()
                .iter()
                .rev()
                .take(8)
                .any(|m| m.content.as_deref() == Some(text.as_str()));
            if !already_present {
                ctx.add_message(hermix_core::Message::system(text));
            }
        }
    }
}