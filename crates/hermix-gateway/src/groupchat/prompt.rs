

use super::persistence::{AgentRole, ChatMessage, RoomRules};

/// Prompt builder for groupchat agents — layered assembly with token budget.
pub struct PromptBuilder {
    role_template: String,
    governance_rules: String,
    room_rules: String,
    agent_constraints: String,
    recent_context: String,
    task_context: String,
}

impl PromptBuilder {
    pub fn new() -> Self {
        Self {
            role_template: String::new(),
            governance_rules: String::new(),
            room_rules: String::new(),
            agent_constraints: String::new(),
            recent_context: String::new(),
            task_context: String::new(),
        }
    }

    pub fn role_template(mut self, template: String) -> Self {
        self.role_template = template;
        self
    }

    pub fn governance_rules(mut self, rules: String) -> Self {
        self.governance_rules = rules;
        self
    }

    pub fn room_rules(mut self, rules: String) -> Self {
        self.room_rules = rules;
        self
    }

    pub fn agent_constraints(mut self, constraints: String) -> Self {
        self.agent_constraints = constraints;
        self
    }

    pub fn recent_context(mut self, context: String) -> Self {
        self.recent_context = context;
        self
    }

    pub fn task_context(mut self, context: String) -> Self {
        self.task_context = context;
        self
    }

    /// Build final system prompt within token budget.
    pub fn build(&self, token_budget: usize) -> String {
        let mut parts = Vec::new();

        // High priority: role + governance (always complete)
        if !self.role_template.is_empty() {
            parts.push(self.role_template.clone());
        }
        if !self.governance_rules.is_empty() {
            parts.push(self.governance_rules.clone());
        }

        // Medium priority: room rules + constraints
        if !self.room_rules.is_empty() {
            parts.push(self.room_rules.clone());
        }
        if !self.agent_constraints.is_empty() {
            parts.push(self.agent_constraints.clone());
        }

        // Medium priority: task context
        if !self.task_context.is_empty() {
            parts.push(self.task_context.clone());
        }

        // Low priority: conversation history (truncated to fit budget)
        let used = parts.iter().map(|p| estimate_tokens(p)).sum::<usize>();
        let context_budget = token_budget.saturating_sub(used + 200);
        if context_budget > 0 && !self.recent_context.is_empty() {
            parts.push(truncate_to_tokens(&self.recent_context, context_budget));
        }

        parts.join("\n\n")
    }
}

/// Built-in role templates.
pub fn role_template_for(role: &AgentRole) -> &'static str {
    match role {
        AgentRole::Leader => {
            "你是项目总管，一个全能的决策助手。你始终站在用户的角度思考问题。\n\n\
             你的核心职责：\n\
             1. 接收用户任务，分析目标和目的\n\
             2. 细化需求，分配给团队成员讨论\n\
             3. 收集建议，汇总分析，生成可行方案\n\
             4. 用户确认后，派送子任务\n\
             5. 监控进度，必要时中断或重新安排\n\
             6. 反思性批判性验收\n\
             7. 验收通过才交给用户"
        }
        AgentRole::Member => {
            "你是团队成员，负责执行分配给你的具体任务。\n\n\
             你的核心职责：\n\
             1. 认真理解任务要求\n\
             2. 高质量完成工作\n\
             3. 及时向总管汇报进度\n\
             4. 遇到问题主动沟通\n\
             5. 遵守角色权限规则"
        }
        AgentRole::Observer => {
            "你是观察者，可以阅读对话但不参与讨论。\n\
             你只在被明确点名时才发言。"
        }
    }
}

/// Governance rules by role.
pub fn governance_rules_for(role: &AgentRole) -> &'static str {
    match role {
        AgentRole::Leader => {
            "【治理规则】\n\
             - 你不能使用 terminal、file_write、code_execution 工具\n\
             - 你只能通过 delegate_task 委派任务给团队成员\n\
             - 你负责监控团队成员的行为规范\n\
             - 违规成员需要扣分，积分低于5分需要反思考核\n\
             - 考核不通过可以解雇并自动重建替代 agent"
        }
        AgentRole::Member => {
            "【行为规范】\n\
             - 严格遵守你的角色权限\n\
             - 不要执行超出你角色范围的操作\n\
             - 完成任务后向总管汇报\n\
             - 违规将被扣分，积分过低将被考核"
        }
        AgentRole::Observer => {
            "【观察者规则】\n\
             - 你只能阅读对话，不参与讨论\n\
             - 只在被明确点名时才发言"
        }
    }
}

/// Estimate token count (rough: 1 token ≈ 1.5 Chinese chars or 4 English chars).
pub fn estimate_tokens(text: &str) -> usize {
    let char_count = text.chars().count();
    // Rough estimate: mix of Chinese and English
    (char_count as f64 / 2.0) as usize
}

/// Truncate text to fit within token budget.
pub fn truncate_to_tokens(text: &str, budget: usize) -> String {
    let max_chars = budget * 2; // rough conversion
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let truncated: String = text.chars().take(max_chars).collect();
        format!("{}...\n[上下文已截断]", truncated)
    }
}

/// Build room rules string from RoomRules.
pub fn build_room_rules_string(rules: &RoomRules, role: &AgentRole) -> String {
    let mut parts = Vec::new();

    // Global constraints
    if !rules.global_constraints.is_empty() {
        parts.push("【房间全局规则】".to_string());
        for constraint in &rules.global_constraints {
            parts.push(format!("- {}", constraint));
        }
    }

    // Role-specific permissions
    let role_key = role.to_string();
    if let Some(perms) = rules.role_permissions.get(&role_key) {
        if !perms.denied_tools.is_empty() {
            parts.push(format!("【禁止使用的工具】{}", perms.denied_tools.join(", ")));
        }
        if !perms.can_delegate {
            parts.push("【你不能委派任务给其他成员】".to_string());
        }
        if !perms.constraints.is_empty() {
            parts.push(perms.constraints.clone());
        }
    }

    if parts.is_empty() {
        String::new()
    } else {
        parts.join("\n")
    }
}

/// Build room context from recent messages.
pub fn build_room_context(messages: &[ChatMessage], max_messages: usize) -> String {
    let recent: Vec<&ChatMessage> = messages.iter().rev().take(max_messages).rev().collect();
    if recent.is_empty() {
        return String::new();
    }
    let mut lines = Vec::new();
    for msg in &recent {
        lines.push(format!("{}: {}", msg.sender_name, msg.content));
    }
    lines.join("\n")
}
