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
            // Append to the last user message instead of creating a new system message.
            // This matches Hermes Agent behavior: plugin context goes into the user
            // message, not the cached system prompt.
            let messages = ctx.get_messages_mut();
            if let Some(last_user) = messages.iter_mut().rev().find(|m| m.role == hermix_core::MessageRole::User) {
                if let Some(ref mut content) = last_user.content {
                    content.push_str("\n\n");
                    content.push_str(text);
                } else {
                    last_user.content = Some(text.clone());
                }
            } else {
                // Fallback: no user message yet, add as system message
                ctx.add_message(hermix_core::Message::system(text));
            }
        }
    }
}