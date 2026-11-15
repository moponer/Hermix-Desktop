//! Delegation backends: in-process signal vs HTTP RPC.
//!
//! [`SignalDelegationBackend`] is the **fallback** registered as the default
//! `delegate_task` backend. When the sub-agent orchestrator IS attached
//! (the normal production path), `tool_executor.rs` intercepts
//! `delegate_task` calls before they reach this backend — the orchestrator
//! spawns the child and returns the result synchronously.
//!
//! When the orchestrator is **NOT** attached (e.g. an older binary, a test
//! configuration, or a misconfigured agent), this backend is reached
//! directly. It returns an **error** so the LLM knows delegation is
//! unavailable, instead of returning a fake "pending" signal that nobody
//! will act on (which caused infinite polling loops in Bug ③).
//!
//! [`RpcDelegationBackend`] POSTs JSON to a user-provided HTTP endpoint
//! and returns the response body as the tool result — use when a remote
//! worker implements delegation.

use async_trait::async_trait;
use serde_json::json;

use crate::tools::delegation::DelegationBackend;
use hermix_core::ToolError;

/// Delegation backend that returns an error when the orchestrator is not
/// attached. In normal production, `tool_executor.rs` intercepts
/// `delegate_task` before this backend is ever reached. If this backend IS
/// reached, it means the orchestrator was never wired (e.g. an older binary)
/// — returning a fake "pending" signal would cause the LLM to poll
/// `get_subagent_result` indefinitely (always "not_found"), creating a
/// dead loop. Failing loudly is the correct behavior.
pub struct SignalDelegationBackend {
    current_depth: u32,
    max_depth: u32,
    parent_budget_remaining_usd: Option<f64>,
}

impl SignalDelegationBackend {
    pub fn new() -> Self {
        Self {
            current_depth: 0,
            max_depth: 4,
            parent_budget_remaining_usd: None,
        }
    }

    pub fn with_depth(mut self, current: u32, max: u32) -> Self {
        self.current_depth = current;
        self.max_depth = max;
        self
    }

    pub fn with_parent_budget(mut self, remaining_usd: f64) -> Self {
        self.parent_budget_remaining_usd = Some(remaining_usd);
        self
    }
}

impl Default for SignalDelegationBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DelegationBackend for SignalDelegationBackend {
    async fn delegate(
        &self,
        task: &str,
        _context: Option<&str>,
        _toolset: Option<&str>,
        _model: Option<&str>,
        child_depth: Option<u32>,
        max_depth: Option<u32>,
        _parent_budget_remaining_usd: Option<f64>,
    ) -> Result<String, ToolError> {
        let effective_child_depth = child_depth.unwrap_or(self.current_depth + 1);
        let effective_max_depth = max_depth.unwrap_or(self.max_depth);
        if effective_child_depth > effective_max_depth {
            return Err(ToolError::ExecutionFailed(format!(
                "Delegation depth limit reached ({}/{}); cannot spawn further sub-agents",
                effective_child_depth, effective_max_depth
            )));
        }
        // This backend is the fallback registered in the tool registry.
        // In production, tool_executor.rs intercepts delegate_task calls
        // BEFORE they reach this handler — the orchestrator runs the child
        // and returns the result synchronously.
        //
        // If we're here, the orchestrator is NOT attached. Returning a
        // fake "pending" signal (the old behavior) caused the LLM to:
        //   1. See "pending" → think a sub-agent was spawned
        //   2. Call get_subagent_result → file doesn't exist → "not_found"
        //   3. Retry delegate_task → same fake "pending" → infinite loop
        //
        // Fail loudly so the LLM knows delegation is unavailable and
        // tries a different approach. The convergence detector will also
        // catch repeated failures via ErrorLoop (hard stop).
        let _ = task; // suppress unused warning
        Err(ToolError::ExecutionFailed(
            "delegate_task failed: the sub-agent orchestrator is not attached \
             to this agent instance — sub-agents cannot be spawned. \
             This usually means the agent was built without calling \
             with_sub_agent_orchestrator() (e.g. an older binary or a \
             test configuration). Try a different approach that does not \
             require sub-agent delegation."
                .to_string(),
        ))
    }
}

/// Delegation backend that forwards requests to an RPC endpoint.
pub struct RpcDelegationBackend {
    endpoint: String,
    client: reqwest::Client,
}

impl RpcDelegationBackend {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl DelegationBackend for RpcDelegationBackend {
    async fn delegate(
        &self,
        task: &str,
        context: Option<&str>,
        toolset: Option<&str>,
        model: Option<&str>,
        child_depth: Option<u32>,
        max_depth: Option<u32>,
        parent_budget_remaining_usd: Option<f64>,
    ) -> Result<String, ToolError> {
        let payload = json!({
            "task": task,
            "context": context,
            "toolset": toolset,
            "model": model,
            "child_depth": child_depth,
            "max_depth": max_depth,
            "parent_budget_remaining_usd": parent_budget_remaining_usd,
        });
        let resp = self
            .client
            .post(&self.endpoint)
            .json(&payload)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("RPC delegation failed: {}", e)))?;
        let text = resp.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed reading RPC response: {}", e))
        })?;
        Ok(text)
    }
}
