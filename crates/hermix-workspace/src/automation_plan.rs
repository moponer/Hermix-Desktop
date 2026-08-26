//! # 自动化注册计划（M4.3/M4.4）— workspace 自动化 → 调度器注册数据
//!
//! workspace schema 里的自动化（condition / llm_summary）需要翻译成
//! hermix-automation 调度器可注册的形态。本模块产出**纯数据计划**
//! （serde 可序列化），由 hermix-server 消费后调用
//! `hermix_automation::AutomationService::create` 注册——保持
//! hermix-workspace 不依赖调度 crate（数据层与调度层解耦）。
//!
//! ```text
//! workspace automations ──plan_automations──▶ AutomationPlan (数据)
//!                                              │
//! hermix-server ──▶ AutomationService::create（rrule 调度）
//! ```
//!
//! 执行分工（M4.2/M4.3）：
//! - `Condition` 计划 → 触发时走**零 LLM 条件引擎**（`condition.rs`）
//! - `LlmSummary` 计划 → 触发时走 `agent.send_message`（会话隔离
//!   `automation:{id}`，复用 delegate_task 机制做 sub-agent 隔离）

use serde::{Deserialize, Serialize};

use crate::model::{Automation, WorkspaceSchema};

// ---------------------------------------------------------------------------
// 计划
// ---------------------------------------------------------------------------

/// 一条自动化的注册计划（hermix-server 消费的数据）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum AutomationPlan {
    /// 条件提醒：确定性 SQL 条件 + 消息模板（零 LLM）。
    Condition {
        /// 关联的工作台 id（调度器触发后定位数据）。
        workspace_id: String,
        automation_id: String,
        name: String,
        table: String,
        condition: String,
        message: String,
        /// 调度频率（daily / hourly / cron 表达式）。
        frequency: String,
    },
    /// LLM 定时分析：语义综合，走 sub-agent 隔离执行。
    #[serde(rename = "llm_summary")]
    LlmSummary {
        workspace_id: String,
        automation_id: String,
        name: String,
        /// cron 表达式。
        schedule: String,
        prompt: String,
        /// 产出落点：notify / report / chat。
        output: String,
    },
}

impl AutomationPlan {
    /// 自动化 id（调度器运行记录关联用）。
    pub fn automation_id(&self) -> &str {
        match self {
            AutomationPlan::Condition { automation_id, .. }
            | AutomationPlan::LlmSummary { automation_id, .. } => automation_id,
        }
    }

    /// 关联的工作台 id。
    pub fn workspace_id(&self) -> &str {
        match self {
            AutomationPlan::Condition { workspace_id, .. }
            | AutomationPlan::LlmSummary { workspace_id, .. } => workspace_id,
        }
    }
}

// ---------------------------------------------------------------------------
// 计划生成
// ---------------------------------------------------------------------------

/// 把工作台 schema 里全部自动化翻译成注册计划。
/// 返回顺序与 schema.automations 一致。
pub fn plan_automations(
    workspace_id: &str,
    schema: &WorkspaceSchema,
) -> Vec<AutomationPlan> {
    schema
        .automations
        .iter()
        .map(|a| plan_one(workspace_id, a))
        .collect()
}

fn plan_one(workspace_id: &str, auto: &Automation) -> AutomationPlan {
    match auto {
        Automation::Condition { id, name, table, condition, message, frequency } => {
            AutomationPlan::Condition {
                workspace_id: workspace_id.to_string(),
                automation_id: id.clone(),
                name: name.clone(),
                table: table.clone(),
                condition: condition.clone(),
                message: message.clone(),
                frequency: frequency.clone(),
            }
        }
        Automation::LlmSummary { id, name, schedule, prompt, output } => {
            AutomationPlan::LlmSummary {
                workspace_id: workspace_id.to_string(),
                automation_id: id.clone(),
                name: name.clone(),
                schedule: schedule.clone(),
                prompt: prompt.clone(),
                output: output.clone(),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 频率 → rrule（cron）
// ---------------------------------------------------------------------------

/// workspace 频率字段 → hermix-automation rrule（cron 表达式）。
/// - `daily` → `0 9 * * *`（每天 09:00）
/// - `hourly` → `0 * * * *`（每小时整点）
/// - 其他值按 cron 表达式原样透传（如 `0 0 * * 1` 每周一零点）。
pub fn frequency_to_rrule(frequency: &str) -> String {
    match frequency.trim().to_lowercase().as_str() {
        "daily" => "0 9 * * *".to_string(),
        "hourly" => "0 * * * *".to_string(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_schema() -> WorkspaceSchema {
        serde_json::from_value(serde_json::json!({
            "name": "车辆管理",
            "modules": [
                { "type": "table", "id": "vehicles", "name": "车辆",
                  "fields": [ { "id": "plate", "name": "车牌", "type": "text", "required": true } ] }
            ],
            "automations": [
                { "kind": "condition", "id": "a1", "name": "年检临期", "table": "vehicles",
                  "condition": "inspection_date <= date('now','+7 days')",
                  "message": "车辆 {plate} 年检临期", "frequency": "daily" },
                { "kind": "llm_summary", "id": "a2", "name": "每周安全周报",
                  "schedule": "0 9 * * 1", "prompt": "汇总上周安全事件并给出建议", "output": "notify" }
            ]
        }))
        .expect("schema")
    }

    #[test]
    fn plans_both_kinds_with_workspace_id() {
        let schema = sample_schema();
        let plans = plan_automations("ws-1", &schema);
        assert_eq!(plans.len(), 2);

        match &plans[0] {
            AutomationPlan::Condition { workspace_id, automation_id, table, frequency, .. } => {
                assert_eq!(workspace_id, "ws-1");
                assert_eq!(automation_id, "a1");
                assert_eq!(table, "vehicles");
                assert_eq!(frequency, "daily");
            }
            other => panic!("期望 Condition，得到 {other:?}"),
        }

        match &plans[1] {
            AutomationPlan::LlmSummary { workspace_id, automation_id, schedule, prompt, output, .. } => {
                assert_eq!(workspace_id, "ws-1");
                assert_eq!(automation_id, "a2");
                assert_eq!(schedule, "0 9 * * 1");
                assert!(prompt.contains("安全"));
                assert_eq!(output, "notify");
            }
            other => panic!("期望 LlmSummary，得到 {other:?}"),
        }
    }

    #[test]
    fn no_automations_yields_empty_plan() {
        let schema: WorkspaceSchema = serde_json::from_value(serde_json::json!({
            "name": "空", "modules": []
        }))
        .expect("schema");
        assert!(plan_automations("ws-x", &schema).is_empty());
    }

    #[test]
    fn frequency_mapping() {
        assert_eq!(frequency_to_rrule("daily"), "0 9 * * *");
        assert_eq!(frequency_to_rrule("hourly"), "0 * * * *");
        assert_eq!(frequency_to_rrule("0 0 * * 1"), "0 0 * * 1");
        assert_eq!(frequency_to_rrule("DAILY"), "0 9 * * *"); // 大小写不敏感
    }

    #[test]
    fn plan_serializes_to_json() {
        let schema = sample_schema();
        let plans = plan_automations("ws-1", &schema);
        let raw = serde_json::to_string(&plans).expect("serialize");
        assert!(raw.contains("\"kind\":\"condition\""));
        assert!(raw.contains("\"kind\":\"llm_summary\""));
        // 反序列化回读（serde tag 往返）
        let back: Vec<AutomationPlan> = serde_json::from_str(&raw).expect("deserialize");
        assert_eq!(back, plans);
    }
}
