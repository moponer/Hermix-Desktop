//! Shared agent-spec data model — canonical Agent 抽象。
//!
//! Pulls the openai-agents-python `Agent` dataclass vocabulary
//! (`agent_id`, `display_name`, `instructions`, `tools`, `handoffs`,
//! `model_override`) into hermix-core as a single canonical struct so any
//! downstream crate can speak the same language:
//!
//! - `hermix_agent::agent_config::AgentConfig` is **the runtime-shaped**
//!   config for an in-flight agent loop.
//!
//! `AgentConfig` doesn't carry an `agent_id`; this struct is the neutral
//! ground between a static spec and the runtime config. The
//! `spec_from_contract_parts` constructor produces one field-by-field
//! without copying logic, and downstream dispatchers can consume it via
//! the same shape.

use serde::{Deserialize, Serialize};

/// Canonical agent-spec data, borrowing the
/// [openai-agents-python `Agent`](https://github.com/openai/openai-agents-python)
/// dataclass field set.
///
/// `Clone` and `Serialize + Deserialize` round-trip cleanly so a spec
/// built server-side can pass through the WS layer to
/// the frontend or sit in a SQLite row without bespoke (de)serializers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSpecData {
    /// Stable id for this agent (the same value used as `sessions.session_id`
    /// after the canonical-UUID refactor).
    pub agent_id: String,

    /// Human-readable name. Defaults to `agent_id` when the upstream
    /// contract doesn't carry one.
    pub display_name: String,

    /// System-prompt instructions. For per-turn overrides the runtime
    /// may layer context-engineering on top of this; `AgentSpecData`
    /// only stores the static base.
    pub instructions: String,

    /// Tools this agent is allowed to invoke. Empty = inherit whatever
    /// the agent loop exposes by default. Dispatchers may layer runtime
    /// policy on top of this static whitelist.
    #[serde(default)]
    pub tools: Vec<String>,

    /// Other `agent_id`s this agent may hand off / delegate to. Empty
    /// for non-coordinating agents. Always sibling-references; the
    /// runtime validates against the room roster.
    #[serde(default)]
    pub handoffs: Vec<String>,

    /// Optional model identifier (e.g. `"gpt-4o"`, `"claude-3-5-sonnet"`).
    /// `None` = use the room / server default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_override: Option<String>,
}

impl AgentSpecData {
    /// Construct a spec with only `agent_id`. The display name defaults
    /// to the id; all other fields are empty. Use the `from_*` adapters
    /// for richer conversions from existing types.
    pub fn new(agent_id: impl Into<String>) -> Self {
        let id = agent_id.into();
        Self {
            display_name: id.clone(),
            agent_id: id,
            instructions: String::new(),
            tools: Vec::new(),
            handoffs: Vec::new(),
            model_override: None,
        }
    }
}

/// Adapter: build an [`AgentSpecData`] from an explicit field subset.
/// Kept as a free function so callers never need to construct the struct
/// literal directly.
pub fn spec_from_contract_parts(
    agent_id: impl Into<String>,
    display_name: impl Into<String>,
    instructions: impl Into<String>,
    tools: Vec<String>,
    handoffs: Vec<String>,
    model_override: Option<String>,
) -> AgentSpecData {
    AgentSpecData {
        agent_id: agent_id.into(),
        display_name: display_name.into(),
        instructions: instructions.into(),
        tools,
        handoffs,
        model_override,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_fills_display_name_from_agent_id() {
        let s = AgentSpecData::new("agent-007");
        assert_eq!(s.agent_id, "agent-007");
        assert_eq!(s.display_name, "agent-007");
        assert!(s.instructions.is_empty());
        assert!(s.tools.is_empty());
        assert!(s.handoffs.is_empty());
        assert!(s.model_override.is_none());
    }

    #[test]
    fn serde_roundtrip_preserves_all_fields() {
        let s = AgentSpecData {
            agent_id: "a".into(),
            display_name: "Alice".into(),
            instructions: "be helpful".into(),
            tools: vec!["terminal".into(), "memory".into()],
            handoffs: vec!["b".into()],
            model_override: Some("claude-opus-4".into()),
        };
        let json = serde_json::to_string(&s).expect("serialize");
        let back: AgentSpecData = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(s, back);
    }

    #[test]
    fn serde_skips_none_model_override_to_keep_wire_minimal() {
        // On the wire, an unspecified model_override must not leak the
        // `null` literal — frontend grouping / diff tools treat it as
        // "no override" either way, but skipping makes diffs cleaner.
        let s = AgentSpecData::new("a");
        let v: serde_json::Value = serde_json::to_value(&s).unwrap();
        assert!(
            !v.as_object().unwrap().contains_key("model_override"),
            "model_override must skip when None; got {}",
            v
        );
    }

    #[test]
    fn spec_from_contract_parts_preserves_inputs() {
        let s = spec_from_contract_parts(
            "a1",
            "Alice",
            "expert coder",
            vec!["terminal".into()],
            vec!["b1".into(), "b2".into()],
            Some("gpt-4o".into()),
        );
        assert_eq!(s.agent_id, "a1");
        assert_eq!(s.display_name, "Alice");
        assert_eq!(s.instructions, "expert coder");
        assert_eq!(s.tools, vec!["terminal".to_string()]);
        assert_eq!(s.handoffs, vec!["b1".to_string(), "b2".to_string()]);
        assert_eq!(s.model_override.as_deref(), Some("gpt-4o"));
    }
}
