//! Tool policy evaluator (Groupchat 未来③ — 工具权限运行时拦截, ).
//!
//! Bridges the declarative [`RoomRules`] policy (prompt-time only today)
//! with a queryable view of "which tools can this agent actually call right
//! now". The previous path — `RuleEngine::check_violation(agent_id, tool,
//! rules, role)` — exists but is unreachable from any dispatch site, so
//! the policy was effectively advisory. This module is the **evaluator**
//! half of the future fix: pure, deterministic, easy to call from any
//! dispatch seam without taking an `&RoomRules`-shaped dependency on a
//! runtime handle. The future wiring (calling this from
//! `ServerAgentDispatcher::dispatch_to_agent` and feeding
//! `AgentOverrides.tools` / an injected denied list downstream) is a
//! separate commit.
//!
//! Layering rules (mirrors `RoomRules::effective_denied_tools` to keep
//! one source of truth):
//!   1. role builtins (always denied — bypassable by no rule)
//!   2. `role_permissions[role].denied_tools`
//!   3. `agent_overrides[agent_id].extra_denied`
//!   4. depending on `ToolPolicyMode`:
//!       - `Blacklist`: union of all `tool_policy.blacklisted`
//!       - `Whitelist`: only tools in `tool_policy.whitelisted` are kept;
//!           everything else is rejected at eval time
//!       - `Strict`:   role builtins + every BUILTIN_SAFETY_RULES tool name
//!
//! The evaluator takes an explicit `registry: &[String]` (the agent's
//! currently-loaded tool names) so the caller decides which slice of the
//! catalog to evaluate against. This keeps the helper node-local and
//! avoids pulling a `ToolRegistryHandle` into hermix-gateway.
//!
//! Quick start:
//! ```ignore
//! let acc = tool_policy_eval::evaluate_tool_access(
//!     &rules, "agent-1", &AgentRole::Member,
//!     &["terminal".into(), "memory".into(), "delegation".into()],
//! );
//! if !acc.allowed.contains(&"terminal".to_string()) {
//!     // "terminal" is denied → block the call, emit ToolBlocked event
//! }
//! ```

use serde::{Deserialize, Serialize};

use super::persistence::{
    builtin_role_denied_tools, AgentRole, RoomRules, ToolPolicyMode,
};

/// Result of evaluating [`RoomRules`] against one agent's current
/// tool registry. Cheap to clone (`String`-only, O(registry size)).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveToolAccess {
    /// Tools the agent is currently allowed to call (registry ⊓ rules).
    /// Always sorted lexicographically to keep the wire form stable for
    /// `serde_assert` style tests and for cache keys downstream.
    pub allowed: Vec<String>,
    /// Tools present in the registry that the policy forbids. The caller
    /// can emit a structured `RoomEvent::ToolBlocked { tool, reason }`
    /// for each one if the LLM surfaces it. Sorted, dedup.
    pub denied_in_registry: Vec<String>,
    /// Effective mode after merging all layers.
    pub mode: ToolPolicyMode,
}

/// Evaluate what an agent can call right now.
///
/// `agent_id` is required because `agent_overrides[agent_id].extra_denied`
/// is per-agent. `registry` is the slice of tool names currently exposed
/// to the agent by the runtime; the evaluator never reaches outside that
/// set. The function is total (no panics, no I/O).
pub fn evaluate_tool_access(
    rules: &RoomRules,
    agent_id: &str,
    role: &AgentRole,
    registry: &[String],
) -> EffectiveToolAccess {
    // Empty registry → empty allow/deny; mode still surfaces so callers
    // can tell "policy says whitelist but nothing is registered" apart
    // from "policy says blacklist and nothing matches".
    if registry.is_empty() {
        return EffectiveToolAccess {
            allowed: Vec::new(),
            denied_in_registry: Vec::new(),
            mode: rules.tool_policy.mode.clone(),
        };
    }

    // Layer 1 + 2 + 3 + 4a/4c: union of "deny" reasons for this agent.
    let mut denied: Vec<String> = Vec::new();

    for t in builtin_role_denied_tools(role) {
        denied.push((*t).to_string());
    }
    if let Some(perms) = rules.role_permissions.get(&role.to_string()) {
        denied.extend(perms.denied_tools.iter().cloned());
    }
    if let Some(agent_ov) = rules.agent_overrides.get(agent_id) {
        denied.extend(agent_ov.extra_denied.iter().cloned());
    }
    if matches!(rules.tool_policy.mode, ToolPolicyMode::Blacklist)
        || matches!(rules.tool_policy.mode, ToolPolicyMode::Strict)
    {
        denied.extend(rules.tool_policy.blacklisted.iter().cloned());
    }

    // Dedupe + sort (case-sensitive; tool names are stable strings).
    denied.sort();
    denied.dedup();

    // Resolve per-tool: allowed iff the tool is in `registry` AND not in
    // `denied` AND (in Whitelist mode) present in `whitelisted`.
    let mut allowed = Vec::new();
    let mut denied_in_registry = Vec::new();
    for tool in registry {
        // Whitelist mode: tools not in `whitelisted` are denied, even if
        // they would otherwise be allowed. An empty whitelist in
        // Whitelist mode means "nothing is allowed" — that's the
        // documented semantics of an explicit (but unconfigured)
        // whitelist and we preserve it.
        let allowed_by_whitelist = match rules.tool_policy.mode {
            ToolPolicyMode::Whitelist => {
                rules.tool_policy.whitelisted.iter().any(|w| w == tool)
            }
            ToolPolicyMode::Blacklist | ToolPolicyMode::Strict => true,
        };
        let allowed_by_blacklist = !denied.iter().any(|d| d == tool);

        if allowed_by_whitelist && allowed_by_blacklist {
            allowed.push(tool.clone());
        } else {
            denied_in_registry.push(tool.clone());
        }
    }

    allowed.sort();
    allowed.dedup();
    denied_in_registry.sort();
    denied_in_registry.dedup();

    EffectiveToolAccess {
        allowed,
        denied_in_registry,
        mode: rules.tool_policy.mode.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::groupchat::persistence::{
        AgentOverride, RolePermissions, RoomRules, ToolPolicy, ToolPolicyMode,
    };

    fn mk_rules(mode: ToolPolicyMode, blacklist: Vec<String>, whitelist: Vec<String>) -> RoomRules {
        RoomRules {
            global_constraints: Vec::new(),
            role_permissions: HashMap::new(),
            agent_overrides: HashMap::new(),
            tool_policy: ToolPolicy {
                mode,
                blacklisted: blacklist,
                whitelisted: whitelist,
            },
            penalty_policy: Default::default(),
        }
    }

    use std::collections::HashMap;

    fn member_role() -> AgentRole {
        AgentRole::Member
    }

    fn leader_role() -> AgentRole {
        AgentRole::Leader
    }

    fn registry(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn empty_registry_yields_empty_buckets_with_mode_preserved() {
        let rules = mk_rules(ToolPolicyMode::Whitelist, vec![], vec![]);
        let acc = evaluate_tool_access(&rules, "a1", &member_role(), &[]);
        assert_eq!(acc.allowed, Vec::<String>::new());
        assert_eq!(acc.denied_in_registry, Vec::<String>::new());
        assert_eq!(acc.mode, ToolPolicyMode::Whitelist);
    }

    #[test]
    fn blacklist_mode_blocks_listed_only() {
        let mut rules = mk_rules(
            ToolPolicyMode::Blacklist,
            vec!["terminal".into()],
            vec![],
        );
        rules
            .role_permissions
            .insert("member".into(), RolePermissions {
                denied_tools: vec!["memory".into()],
                can_delegate: true,
                can_write_memory: true,
                constraints: String::new(),
            });
        rules.agent_overrides.insert(
            "a1".into(),
            AgentOverride {
                extra_denied: vec!["delegation".into()],
                extra_constraints: vec![],
            },
        );

        let acc = evaluate_tool_access(
            &rules,
            "a1",
            &member_role(),
            &registry(&["terminal", "memory", "delegation", "search"]),
        );
        // sorted
        assert_eq!(acc.allowed, vec!["search".to_string()]);
        assert_eq!(
            acc.denied_in_registry,
            vec![
                "delegation".to_string(),
                "memory".to_string(),
                "terminal".to_string(),
            ]
        );
    }

    #[test]
    fn leader_role_always_loses_terminal_file_write_code_execution() {
        // Built-in deny is non-overridable. Even on an empty room
        // blacklist, these three are still denied.
        let rules = mk_rules(ToolPolicyMode::Blacklist, vec![], vec![]);
        let acc = evaluate_tool_access(
            &rules,
            "leader-x",
            &leader_role(),
            &registry(&["terminal", "file_write", "code_execution", "memory"]),
        );
        assert_eq!(acc.allowed, vec!["memory".to_string()]);
        assert_eq!(acc.denied_in_registry.len(), 3);
        assert!(acc.denied_in_registry.contains(&"terminal".to_string()));
        assert!(acc.denied_in_registry.contains(&"file_write".to_string()));
        assert!(acc
            .denied_in_registry
            .contains(&"code_execution".to_string()));
    }

    #[test]
    fn whitelist_mode_admits_only_listed_tools() {
        let rules = mk_rules(
            ToolPolicyMode::Whitelist,
            vec![],
            vec!["search".into(), "memory".into()],
        );
        let acc = evaluate_tool_access(
            &rules,
            "a1",
            &member_role(),
            &registry(&["search", "memory", "terminal", "delegation"]),
        );
        assert_eq!(
            acc.allowed,
            vec!["memory".to_string(), "search".to_string()]
        );
        assert_eq!(
            acc.denied_in_registry,
            vec![
                "delegation".to_string(),
                "terminal".to_string(),
            ]
        );
    }

    #[test]
    fn whitelist_mode_with_empty_whitelist_drops_everything() {
        // Documented semantics: an unconfigured Whitelist means
        // "nothing permitted" — operators must populate the list to
        // grant access. This prevents accidental open access via
        // "Whitelist mode but the whitelist is empty".
        let rules = mk_rules(ToolPolicyMode::Whitelist, vec![], vec![]);
        let acc = evaluate_tool_access(
            &rules,
            "a1",
            &member_role(),
            &registry(&["search", "memory"]),
        );
        assert!(acc.allowed.is_empty());
        assert_eq!(
            acc.denied_in_registry,
            vec!["memory".to_string(), "search".to_string()]
        );
    }

    #[test]
    fn member_role_cannot_delegate_even_when_not_in_blacklist() {
        // Built-in deny for member → "delegation" always off, even if
        // neither room blacklist nor per-agent overrides mention it.
        let rules = mk_rules(ToolPolicyMode::Blacklist, vec![], vec![]);
        let acc = evaluate_tool_access(
            &rules,
            "a1",
            &member_role(),
            &registry(&["delegation", "search"]),
        );
        assert_eq!(acc.allowed, vec!["search".to_string()]);
        assert_eq!(
            acc.denied_in_registry,
            vec!["delegation".to_string()]
        );
    }

    #[test]
    fn agent_override_does_not_leak_to_other_agents() {
        let mut rules = mk_rules(ToolPolicyMode::Blacklist, vec![], vec![]);
        rules.agent_overrides.insert(
            "a1".into(),
            AgentOverride {
                extra_denied: vec!["search".into()],
                extra_constraints: vec![],
            },
        );
        let for_a1 = evaluate_tool_access(
            &rules,
            "a1",
            &member_role(),
            &registry(&["search", "memory"]),
        );
        let for_a2 = evaluate_tool_access(
            &rules,
            "a2",
            &member_role(),
            &registry(&["search", "memory"]),
        );
        // a1 is denied `search`; a2 is not.
        assert_eq!(for_a1.allowed, vec!["memory".to_string()]);
        assert_eq!(
            for_a2.allowed,
            vec!["memory".to_string(), "search".to_string()]
        );
    }
}
