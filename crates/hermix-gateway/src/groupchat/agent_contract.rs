//! Structured Agent contract — borrowed from openai-agents-python's `Agent`
//! dataclass design.
//!
//! In openai-agents-python, an `Agent` is a dataclass holding `instructions`,
//! `tools`, `handoffs`, and `guardrails`. We adapt this to the group-chat
//! context: a `RoomAgentContract` describes one member's runtime shape inside
//! a room, and `LeaderDecision` is the structured JSON output that replaces
//! the fragile regex-based `parse_task_plan`.
//!
//! ## Why structured output?
//!
//! The old `LeaderOrchestrator::parse_task_plan` parsed `- task → agent` lines
//! with regex. LLMs frequently emit slightly different formats (`->`, `@`,
//! numbered lists, bold markdown), causing silent parse failures where the
//! leader's entire reply was stuffed into a single subtask with empty
//! `assigned_to`. The structured approach asks the LLM for strict JSON, with
//! a tolerant fallback parser that survives ```json fences and surrounding
//! prose.

use serde::{Deserialize, Serialize};

use super::persistence::{AgentRole, AgentRoomProfile};

/// Runtime contract for one agent inside a room (openai-agents-python `Agent`
/// analog). Built from `AgentRoomProfile` + room rules at dispatch time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomAgentContract {
    pub agent_id: String,
    pub display_name: String,
    pub role: AgentRole,
    /// System-prompt instructions (role definition + constraints).
    pub instructions: String,
    /// Tool whitelist for this agent in this room. Empty = inherit default.
    pub tools: Vec<String>,
    /// Other agent_ids this agent may hand off to. Empty for members.
    pub handoffs: Vec<String>,
    pub model_override: Option<String>,
}

impl RoomAgentContract {
    /// Build a contract from a persisted profile. The `instructions` and
    /// `tools` fields are left empty — callers (the dispatcher) fill them
    /// from the room's rules + the agent's persona before dispatch.
    pub fn from_profile(profile: &AgentRoomProfile) -> Self {
        Self {
            agent_id: profile.agent_id.clone(),
            display_name: profile
                .display_name
                .clone()
                .unwrap_or_else(|| profile.agent_id.clone()),
            role: profile.role.clone(),
            instructions: String::new(),
            tools: Vec::new(),
            handoffs: Vec::new(),
            model_override: profile.model_override.clone(),
        }
    }
}

/// Leader's structured decision output — replaces regex-parsed `TaskPlan`.
/// /// The leader LLM is asked to emit this as strict JSON. The parser is
/// tolerant of ```json fences and surrounding prose (see
/// [`parse_leader_decision`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaderDecision {
    /// Human-readable analysis of the user's request.
    pub analysis: String,
    /// Subtask assignments. May be empty when the leader answers directly
    /// without delegation.
    #[serde(default)]
    pub subtasks: Vec<SubtaskAssignment>,
    /// Optional room announcement (merged into the same LLM call to save
    /// a round trip — previously a separate `build_announcement_decision_prompt`).
    #[serde(default)]
    pub announcement: Option<String>,
    /// Whether the leader wants explicit user confirmation before executing.
    #[serde(default)]
    pub needs_user_confirmation: bool,
    /// Optional handoff: if set, the leader transfers control of the
    /// conversation to the named agent (openai-agents-python Handoff pattern).
    #[serde(default)]
    pub handoff: Option<HandoffDirective>,
}

/// One subtask assignment from the leader.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtaskAssignment {
    pub description: String,
    /// Target agent_id. Empty = unassigned (leader dispatches to all members).
    #[serde(default)]
    pub assigned_to: String,
    /// Extra context / spec for this subtask.
    #[serde(default)]
    pub context: Option<String>,
    /// IDs of subtasks that must complete before this one starts.
    /// References the `id` field of sibling subtasks. Empty = no deps.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Optional explicit id for dependency wiring. If absent, the runtime
    /// assigns `subtask-{index}`.
    #[serde(default)]
    pub id: Option<String>,
}

/// Handoff directive — openai-agents-python's two orchestration primitives
/// mapped onto one enum.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HandoffDirective {
    /// Agents-as-tools: leader delegates a subtask and retains control.
    /// This is the default — `subtasks` already covers this case, so this
    /// variant is only used when the leader explicitly wants to flag a
    /// delegation in the handoff field.
    Delegate {
        agent_id: String,
        task: String,
    },
    /// Handoff: leader transfers the whole conversation to a specialist.
    /// The specialist takes over until it hands back or the task completes.
    Transfer {
        agent_id: String,
        reason: String,
    },
}

/// Tolerant JSON parser for `LeaderDecision`.
/// /// Survives:
/// - ```json ... ``` code fences
/// - leading/trailing prose (uses first `{` to last `}`)
/// - missing optional fields (defaults applied via `#[serde(default)]`)
/// /// Returns `Err` with a human-readable message on hard parse failure so the
/// runtime can surface it as a `SoftError` instead of silently falling back.
pub fn parse_leader_decision(raw: &str) -> Result<LeaderDecision, String> {
    let trimmed = raw.trim();

    // Fast path: the model returned clean JSON.
    if let Ok(d) = serde_json::from_str::<LeaderDecision>(trimmed) {
        return Ok(d);
    }

    // Tolerant path: strip ```json ... ``` (or bare ``` ... ```) code
    // fences if present. The leader prompt asks for strict JSON, but the
    // model often wraps it in a markdown fence plus surrounding prose such
    // as "好的，我来分析一下。 ... ```json { ... } ```  希望对你有帮助。".
    // Without this step, the streaming deserializer below hits the leading
    // Chinese prose and bails out before reaching the JSON object.
    let body = strip_code_fence(trimmed);

    // Tolerant path (M8): use the streaming deserializer so surrounding
    // prose — including a trailing `}` inside commentary after the JSON
    // object — doesn't break parsing. The old `find('{')` + `rfind('}')`
    // slice truncated on the **last** `}` in the reply, which corrupted
    // any JSON value or trailing text containing a brace. The streaming
    // deserializer reads exactly one value and stops, ignoring trailing
    // tokens (and correctly handles `}` inside JSON string values).
    let mut de = serde_json::Deserializer::from_str(&body).into_iter();
    match de.next() {
        Some(Ok(d)) => Ok(d),
        Some(Err(e)) => Err(format!("failed to parse leader decision JSON: {}", e)),
        None => Err("no JSON object found in leader reply".to_string()),
    }
}

/// Extract the inner content of a fenced code block
/// (`` ```json ... ``` `` or `` ``` ... ``` ``) if present. Returns the
/// trimmed inner content, or the original input when no fence is found.
///
/// Handles both the language-tagged form (```json, ```javascript, etc.)
/// and the bare ```` ``` ```` form (skipping a single-line language tag on
/// the same line as the opening fence). Multi-line bodies and braces
/// inside JSON string values are preserved — we only match the *first*
/// opening fence and the *first* closing fence after it, which mirrors
/// how the leader model is expected to emit blocks.
fn strip_code_fence(s: &str) -> String {
    let open = match s.find("```") {
        Some(i) => i,
        None => return s.to_string(),
    };
    let after_open = open + 3;
    // Skip the optional language tag on the same line as the opening
    // fence (e.g. ```json). If the opener sits on the same line as the
    // content, fall back to the byte right after the opener.
    let after_lang = s[after_open..]
        .find('\n')
        .map(|p| after_open + p + 1)
        .unwrap_or(after_open);
    if let Some(close_rel) = s[after_lang..].find("```") {
        return s[after_lang..after_lang + close_rel].trim().to_string();
    }
    s.to_string()
}

/// Build the prompt that asks the leader to return a `LeaderDecision` JSON.
/// /// This replaces both `build_leader_prompt` (system prompt) and
/// `build_announcement_decision_prompt` (separate LLM call) — the leader
/// emits analysis + subtasks + announcement in one structured response.
pub fn build_leader_decision_prompt(
    room_name: &str,
    team: &[AgentRoomProfile],
    user_message: &str,
    room_context: &str,
) -> String {
    let team_list: Vec<String> = team
        .iter()
        .filter(|a| a.role != AgentRole::Leader)
        .map(|a| {
            let name = a
                .display_name
                .as_deref()
                .unwrap_or(a.agent_id.as_str());
            format!("- id: \"{}\", name: \"{}\", role: {}", a.agent_id, name, a.role)
        })
        .collect();
    let team_str = if team_list.is_empty() {
        "(暂无团队成员)".to_string()
    } else {
        team_list.join("\n")
    };

    format!(
        r#"你是群聊 "{room_name}" 的项目总管，一个全能的决策助手。你始终站在用户的角度思考问题。

## 团队成员
{team_str}

## 房间上下文（最近对话）
{room_context}

## 用户消息
{user_message}

## 你的任务
分析用户的需求，决定如何分配任务给团队成员。你必须**严格**按以下 JSON 格式回复，不要输出任何其他内容（不要解释、不要 Markdown 包裹、不要代码块标记之外的文字）：

{{
  "analysis": "你对用户需求的分析（2-4 句话）",
  "subtasks": [
    {{
      "id": "subtask-1",
      "description": "子任务描述（具体、可执行）",
      "assigned_to": "agent_id（必须是团队成员列表中的 id；留空表示不指定）",
      "context": "额外上下文（可选，null 表示无）",
      "depends_on": []
    }}
  ],
  "announcement": "公告内容（可选，仅当用户明确了项目目标/主题时填写；否则为 null）",
  "needs_user_confirmation": false,
  "handoff": null
}}

## 规则
1. `subtasks` 可以为空数组 `[]`——当你自己就能回答用户时，不要硬拆任务
2. `assigned_to` 必须是上面团队成员列表中存在的 `id`，否则留空 `""`
3. `depends_on` 填写前置子任务的 `id`，表示"等它完成后再执行"
4. `announcement` 仅在以下情况填写：用户明确了项目目标/主题/任务名、或需要持久化协作规则。闲聊、单一问题不要发公告
5. `needs_user_confirmation` 设为 true 当你的方案需要用户点头才执行（默认 false）
6. `handoff` 通常为 null。仅当你认为某个专家应该完全接管对话时填写：{{"kind":"transfer","agent_id":"xxx","reason":"..."}}
7. 不要输出 JSON 之外的任何文字

## 工作流程参考
1. 分析用户的目标和目的
2. 如果需要团队协作，拆解为子任务并分配
3. 考虑子任务之间的依赖关系（并行/串行）
4. 如果用户明确了项目方向，设置公告
5. 如果方案有风险或需要用户决策，设置 needs_user_confirmation=true"#,
        room_name = room_name,
        team_str = team_str,
        room_context = if room_context.is_empty() {
            "(无)"
        } else {
            room_context
        },
        user_message = user_message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_clean_json() {
        let raw = r#"{"analysis":"need code review","subtasks":[{"id":"s1","description":"review backend","assigned_to":"alice","context":null,"depends_on":[]}],"announcement":null,"needs_user_confirmation":false,"handoff":null}"#;
        let d = parse_leader_decision(raw).unwrap();
        assert_eq!(d.analysis, "need code review");
        assert_eq!(d.subtasks.len(), 1);
        assert_eq!(d.subtasks[0].assigned_to, "alice");
        assert!(!d.needs_user_confirmation);
    }

    #[test]
    fn parse_with_json_fence_and_prose() {
        let raw = r#"好的，我来分析一下。

```json
{
  "analysis": "用户需要一个登录页面",
  "subtasks": [],
  "announcement": "本项目目标：实现登录页面",
  "needs_user_confirmation": true,
  "handoff": null
}
```

希望对你有帮助。"#;
        let d = parse_leader_decision(raw).unwrap();
        assert_eq!(d.analysis, "用户需要一个登录页面");
        assert!(d.subtasks.is_empty());
        assert_eq!(
            d.announcement.as_deref(),
            Some("本项目目标：实现登录页面")
        );
        assert!(d.needs_user_confirmation);
    }

    #[test]
    fn parse_with_handoff_transfer() {
        let raw = r#"{
  "analysis": "这是法律问题，交给法务专家",
  "subtasks": [],
  "announcement": null,
  "needs_user_confirmation": false,
  "handoff": {"kind":"transfer","agent_id":"lawyer-1","reason":"法律专业问题"}
}"#;
        let d = parse_leader_decision(raw).unwrap();
        match d.handoff {
            Some(HandoffDirective::Transfer { agent_id, reason }) => {
                assert_eq!(agent_id, "lawyer-1");
                assert_eq!(reason, "法律专业问题");
            }
            other => panic!("expected Transfer, got {:?}", other),
        }
    }

    #[test]
    fn parse_empty_subtasks_default() {
        let raw = r#"{"analysis":"direct answer","subtasks":[]}"#;
        let d = parse_leader_decision(raw).unwrap();
        assert_eq!(d.analysis, "direct answer");
        assert!(d.subtasks.is_empty());
        assert!(!d.needs_user_confirmation);
        assert!(d.announcement.is_none());
    }

    #[test]
    fn parse_missing_optional_fields_uses_defaults() {
        // Only `analysis` is required by the struct (no #[serde(default)] on it).
        let raw = r#"{"analysis":"ok"}"#;
        let d = parse_leader_decision(raw).unwrap();
        assert_eq!(d.analysis, "ok");
        assert!(d.subtasks.is_empty());
        assert!(!d.needs_user_confirmation);
    }

    #[test]
    fn parse_garbage_returns_err() {
        let raw = "this is not json at all";
        assert!(parse_leader_decision(raw).is_err());
    }

    #[test]
    fn parse_no_brace_returns_err() {
        let raw = "no braces here";
        assert!(parse_leader_decision(raw).is_err());
    }

    #[test]
    fn build_prompt_contains_team_and_message() {
        use super::super::persistence::AgentRole;
        let team = vec![AgentRoomProfile {
            agent_id: "alice".into(),
            room_id: "r1".into(),
            role: AgentRole::Member,
            session_id: "s1".into(),
            model_override: None,
            display_name: Some("Alice".into()),
        }];
        let prompt = build_leader_decision_prompt("TestRoom", &team, "hello", "ctx");
        assert!(prompt.contains("alice"));
        assert!(prompt.contains("Alice"));
        assert!(prompt.contains("TestRoom"));
        assert!(prompt.contains("hello"));
        assert!(prompt.contains("ctx"));
        assert!(prompt.contains("\"subtasks\""));
    }
}
