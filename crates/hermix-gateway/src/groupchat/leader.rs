use super::agent_contract::{parse_leader_decision, LeaderDecision};
use super::persistence::*;

/// Result of leader's secondary decision: should we publish a
/// room-wide announcement, and if so what should it say?
/// /// Returned by [`LeaderOrchestrator::parse_announcement_decision`]
/// after a short follow-up LLM call in `send_message`. The decision
/// is fail-safe: any parse error or unexpected shape resolves to
/// `should_announce = false` so the main dispatch flow is never
/// blocked by the announcement side-channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnouncementDecision {
    pub should_announce: bool,
    pub text: Option<String>,
}

impl Default for AnnouncementDecision {
    fn default() -> Self {
        Self { should_announce: false, text: None }
    }
}

/// Leader (总管) orchestrator — the built-in decision-making agent.
/// /// The leader has a fixed behavior pattern:
/// 1. Receive user task → analyze goals
/// 2. Delegate to specialized agents for discussion
/// 3. Collect suggestions → aggregate into feasible plan
/// 4. Present plan to user for confirmation
/// 5. After user confirmation → dispatch subtasks (parallel/sequential/dependent)
/// 6. Monitor progress, interrupt/reassign as needed
/// 7. Validate results with critical review
/// 8. Pass validated results to user
pub struct LeaderOrchestrator;

/// Result of leader's task decomposition.
#[derive(Debug, Clone)]
pub struct TaskPlan {
    pub subtasks: Vec<PlannedSubtask>,
    pub reasoning: String,
}

#[derive(Debug, Clone)]
pub struct PlannedSubtask {
    pub description: String,
    pub assigned_to: String,
    pub context: Option<String>,
    /// If true, must wait for `wait_for` subtasks to complete first.
    pub wait_for: Vec<usize>,
}

impl LeaderOrchestrator {
    pub fn new() -> Self {
        Self
    }

    /// Build the leader's system prompt with team context.
    pub fn build_leader_prompt(
        team: &[AgentRoomProfile],
        agent_names: &[(String, String)], // (id, name)
    ) -> String {
        let team_list: Vec<String> = team
            .iter()
            .filter(|a| a.role != AgentRole::Leader)
            .map(|a| {
                let name = agent_names
                    .iter()
                    .find(|(id, _)| id == &a.agent_id)
                    .map(|(_, n)| n.as_str())
                    .unwrap_or("unknown");
                format!("- {} (role: {})", name, a.role)
            })
            .collect();

        format!(
            r#"你是项目总管，一个全能的决策助手。你始终站在用户的角度思考问题。

你的团队成员：
{}

工作流程：
1. 收到任务后，分析用户的目标和目的
2. 细化需求，分配给合适的团队成员讨论
3. 收集所有成员的建议，汇总分析
4. 生成可行方案交给用户确认
5. 用户确认后，派送子任务给团队成员执行
6. 决定哪些子任务可以并行，哪些需要串行等待
7. 监控执行进度，必要时中断或重新安排
8. 所有子任务完成后，进行批判性验收
9. 验收通过才交给用户；不通过则打回继续或换人

重要原则：
- 始终站在用户立场思考
- 任务分解要具体、可执行
- 子任务之间要考虑依赖关系
- 验收时要严格、批判性地审查结果

可选的公告设置（M3 — 已合并到主 prompt，省一次 LLM 调用）：
- 当用户明确了项目目标/主题/任务名，或需要持久化记录时，
  请在你的回复**末尾**添加：
  ```
  ---ANNOUNCE---
  公告内容（一行）
  ```
- 只有确实需要时才加，绝大多数消息不需要。"#,
            team_list.join("\n")
        )
    }

    /// Build the short decision prompt that asks the leader whether to
    /// publish a room-wide announcement.
    /// /// Kept separate from `build_leader_prompt` because the decision
    /// surface is narrow (one yes/no + one text) and we want a tight,
    /// short response that won't be confused with the main analysis.
    /// The prompt instructs the model to emit *only* a JSON object —
    /// the parser is tolerant of ```json fences and surrounding prose,
    /// but plain JSON gets the most reliable behavior across providers.
    pub fn build_announcement_decision_prompt(
        room_name: &str,
        user_message: &str,
        leader_analysis: &str,
    ) -> String {
        format!(
            r#"你是群聊 "{room_name}" 的项目总管。刚刚收到用户的消息，并且你已经做了一轮分析。

用户的原始消息："{user_message}"

你刚才的分析（节选）："{leader_analysis}"

请判断：是否要为此群聊发布一条"项目公告"（写进群公告字段，所有成员进来时都看得到）？

只有当以下条件至少满足一条时才发：
1. 用户明确了项目目标 / 主题 / 任务名（例如"我们要做 X"）
2. 用户给了明确的协作规则 / 工作方向（例如"这个群专门讨论 X，请按 Y 方式协作"）
3. 任务方向足以让整个群明确方向，需要公告以避免后续走偏

否则不要发：闲聊、问题咨询、单一任务执行、临时提问都不需要公告。

严格按以下 JSON 格式回复，不要写任何其他内容（不要解释、不要 Markdown 包裹、不要代码块）：

{{
  "should_announce": true,
  "text": "公告正文（1-2 句话，不超过 120 字；用陈述句，包含目标/规则/关键约束）"
}}

要求：
- 公告是给团队所有人看的，文字要让团队成员立即明白"这个群在做什么、有什么约束"
- 如果用户给了方向但还有待澄清，公告也要把"待澄清"的部分写出来（不要假装确定）
- 如果 should_announce=false，text 字段写 null
- 不要重复用户原话，要做概括
- 不要用 emoji"#,
            room_name = room_name,
            user_message = user_message,
            leader_analysis = leader_analysis.chars().take(400).collect::<String>(),
        )
    }

    /// Parse the leader's announcement decision. Tolerant to:
    /// - surrounding ```json fences
    /// - leading/trailing prose (uses first `{` to last `}`)
    /// - missing/null text when `should_announce` is false
    /// - whitespace-padded text
    /// /// Returns `should_announce: false` on any parse error or when
    /// the response is empty. The whole point is that the main
    /// dispatch flow MUST keep working even if this side-channel
    /// gives garbage.
    pub fn parse_announcement_decision(response: &str) -> AnnouncementDecision {
        let trimmed = response.trim();
        if trimmed.is_empty() {
            return AnnouncementDecision::default();
        }

        // Find the first `{` and the last `}` so we survive a model
        // that wraps the JSON in prose like "Here you go: {...}".
        let start = match trimmed.find('{') {
            Some(i) => i,
            None => return AnnouncementDecision::default(),
        };
        let end = match trimmed.rfind('}') {
            Some(i) if i > start => i,
            _ => return AnnouncementDecision::default(),
        };
        let block = &trimmed[start..=end];

        #[derive(serde::Deserialize)]
        struct Raw {
            should_announce: bool,
            #[serde(default)]
            text: Option<String>,
        }

        match serde_json::from_str::<Raw>(block) {
            Ok(r) => {
                let text = r.text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
                AnnouncementDecision {
                    should_announce: r.should_announce && text.is_some(),
                    text,
                }
            }
            Err(_) => AnnouncementDecision::default(),
        }
    }

    /// Parse announcement decision from the leader's main reply (M3 — merged).
    /// Returns `Some(announcement_text)` if the reply contains `---ANNOUNCE---`,
    /// otherwise `None`.
    pub fn parse_announcement_from_reply(reply: &str) -> Option<String> {
        let marker = "---ANNOUNCE---";
        if let Some(pos) = reply.find(marker) {
            let after = reply[pos + marker.len()..].trim();
            let text = after.lines().next().unwrap_or("").trim();
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }
        None
    }

    /// Parse the leader's structured decision from its JSON reply.
    /// /// This is the **primary** path after the 2026-07 refactor — the leader
    /// is asked to emit a `LeaderDecision` JSON (see
    /// [`super::agent_contract::build_leader_decision_prompt`]). On success
    /// the caller gets analysis + subtasks + announcement + handoff in one
    /// shot, eliminating the fragile regex-based `parse_task_plan`.
    /// /// On parse failure the caller should fall back to [`parse_task_plan`]
    /// (legacy) and surface a `SoftError` so the user knows the structured
    /// path failed.
    pub fn parse_decision(raw: &str) -> Result<LeaderDecision, String> {
        parse_leader_decision(raw)
    }

    /// Convert a parsed `LeaderDecision` into the legacy `TaskPlan` shape so
    /// the existing `runtime.rs` dispatch path can consume it without changes.
    /// /// This adapter bridges the new structured output to the old scheduler,
    /// letting us roll out the structured prompt incrementally.
    pub fn decision_to_task_plan(decision: &LeaderDecision) -> TaskPlan {
        // Build a map from subtask id → index so we can resolve
        // `depends_on` string references into `wait_for` indices.
        let id_to_index: std::collections::HashMap<&str, usize> = decision
            .subtasks
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id.as_deref().unwrap_or(""), i))
            .filter(|(id, _)| !id.is_empty())
            .collect();

        let subtasks: Vec<PlannedSubtask> = decision
            .subtasks
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let wait_for: Vec<usize> = s
                    .depends_on
                    .iter()
                    .filter_map(|dep| {
                        let resolved = id_to_index
                            .get(dep.as_str())
                            .copied()
                            .or_else(|| {
                                let fallback = format!("subtask-{}", dep);
                                id_to_index.get(fallback.as_str()).copied()
                            });
                        match resolved {
                            // Self-referential dependency is ignored.
                            Some(idx) if idx != i => Some(idx),
                            Some(_) => None,
                            // M6: a reference to an id that isn't a real
                            // subtask used to be silently dropped by
                            // `filter_map`. Log it so the dependency graph
                            // isn't silently corrupted.
                            None => {
                                tracing::warn!(
                                    subtask_id = s.id.as_deref().unwrap_or("?"),
                                    missing_dep = dep,
                                    "leader decision references unknown subtask id in depends_on; \
                                     dropping dependency (M6)"
                                );
                                None
                            }
                        }
                    })
                    .collect();
                PlannedSubtask {
                    description: s.description.clone(),
                    assigned_to: s.assigned_to.clone(),
                    context: s.context.clone(),
                    wait_for,
                }
            })
            .collect();
        let reasoning = decision.analysis.clone();
        TaskPlan {
            subtasks,
            reasoning,
        }
    }

    /// Parse leader's task decomposition from LLM response into a TaskPlan.
    /// Supports `- task description → alice` or `- task description @alice`
    /// annotations for per-subtask assignment.
    pub fn parse_task_plan(response: &str) -> TaskPlan {
        let mut subtasks = Vec::new();
        let mut reasoning = String::new();

        for line in response.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("## ") || trimmed.starts_with("### ") {
                reasoning.push_str(trimmed);
                reasoning.push('\n');
            } else if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
                let raw = trimmed.trim_start_matches("- ").trim_start_matches("* ");
                if raw.is_empty() {
                    continue;
                }
                let (desc, assigned_to) = extract_assignee(raw);
                subtasks.push(PlannedSubtask {
                    description: desc,
                    assigned_to,
                    context: None,
                    wait_for: vec![],
                });
            }
        }

        if subtasks.is_empty() {
            subtasks.push(PlannedSubtask {
                description: response.to_string(),
                assigned_to: String::new(),
                context: None,
                wait_for: vec![],
            });
        }

        TaskPlan {
            subtasks,
            reasoning,
        }
    }
}

/// Extract `@agent_id` or `→ agent_id` from a task description line.
/// Returns `(cleaned_description, agent_id)` — agent_id is empty when
/// no annotation was found.
fn extract_assignee(raw: &str) -> (String, String) {
    // Pattern: "description → alice" or "description -> alice"
    if let Some(pos) = raw.find(" → ") {
        let desc = raw[..pos].trim().to_string();
        let agent = raw[pos + " → ".len()..].trim().to_string();
        if !agent.is_empty() {
            return (desc, agent);
        }
    }
    if let Some(pos) = raw.find(" -> ") {
        let desc = raw[..pos].trim().to_string();
        let agent = raw[pos + " -> ".len()..].trim().to_string();
        if !agent.is_empty() {
            return (desc, agent);
        }
    }
    // Pattern: "description @alice"
    if let Some(pos) = raw.rfind(" @") {
        let candidate = raw[pos + 1..].trim();
        // An agent id looks like a single word (no spaces)
        if !candidate.is_empty() && !candidate.contains(' ') {
            let desc = raw[..pos].trim().to_string();
            return (desc, candidate.to_string());
        }
    }
    (raw.to_string(), String::new())
}
