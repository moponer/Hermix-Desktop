//! # 条件提醒引擎（M4.2）— 零 LLM 的确定性提醒
//!
//! `condition` 类自动化（年检临期、到期提醒等）是**确定性 SQL 条件**，
//! 不需要 LLM：调度器到期 → 本引擎按 `WHERE 片段` 扫描匹配行 →
//! 用消息模板 `{field}` 插值渲染通知 → 送达用户。
//!
//! 与 LLM 定时分析（`llm_summary`，见 M4.3）的分工：
//!
//! | 场景 | 引擎 | 成本 |
//! |---|---|---|
//! | 年检临期 / 状态阈值 / 到期提醒 | 条件引擎（本模块） | 零 LLM |
//! | 周报总结 / 数据异常语义分析 | LLM 定时任务 | 有 token 成本 |
//!
//! 安全模型：表名来自模块 id（标识符白名单）；条件片段在建表/迁移时
//! 已 `probe_condition` 编译校验；`LIMIT` 上限防大结果集拖垮通知通道。

use serde::Serialize;
use serde_json::Value;

use crate::model::Automation;
use crate::store::{LocalSqliteStore, StoreError};

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ConditionError {
    #[error("store 错误: {0}")]
    Store(#[from] StoreError),
    #[error("自动化定义无效: {0}")]
    Invalid(String),
}

// ---------------------------------------------------------------------------
// 结果
// ---------------------------------------------------------------------------

/// 一条条件命中（通知单元）。
#[derive(Debug, Clone, Serialize)]
pub struct ConditionHit {
    /// 命中行（白名单列 + 系统列）。
    pub row: Value,
    /// 消息模板渲染结果（`{field}` 插值）。
    pub message: String,
}

/// 扫描摘要（供调度器记入运行记录 / 通知侧使用）。
#[derive(Debug, Clone, Serialize)]
pub struct ConditionScanResult {
    /// 自动化 id（调度器关联用）。
    pub automation_id: String,
    /// 命中总数（未截断的实际命中数）。
    pub total: usize,
    /// 实际取回的行数（受 limit 钳制）。
    pub fetched: usize,
    /// 渲染后的通知消息（每条命中一条）。
    pub hits: Vec<ConditionHit>,
}

// ---------------------------------------------------------------------------
// 引擎
// ---------------------------------------------------------------------------

/// 渲染消息模板：`{field}` 替换为行值。
/// 语义：行内字段为 null → 空串；**行里不存在的字段保留原占位符**——
/// 因为 `query_by_condition` 用白名单列 SELECT（schema 字段一定在行里，
/// 值为 null 也出现），保留占位符只意味着模板引用了 schema 外字段，
/// 这是暴露模板错误的失败信号，不应静默吞掉。
pub fn render_message(template: &str, row: &Value) -> String {
    let mut out = template.to_string();
    if let Some(obj) = row.as_object() {
        for (k, v) in obj {
            let placeholder = format!("{{{k}}}");
            if !out.contains(&placeholder) {
                continue;
            }
            let text = match v {
                Value::Null => String::new(),
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            out = out.replace(&placeholder, &text);
        }
    }
    out
}

/// 对工作台执行一条条件自动化（零 LLM）。
/// - `limit`：命中行上限（防大结果集；0 = 不限，谨慎使用）。
pub fn run_condition_automation(
    store: &LocalSqliteStore,
    auto: &Automation,
    limit: usize,
) -> Result<ConditionScanResult, ConditionError> {
    let (id, table, condition, message) = match auto {
        Automation::Condition { id, table, condition, message, .. } => {
            (id, table, condition, message)
        }
        _ => {
            return Err(ConditionError::Invalid(
                "非 condition 类自动化不能走条件引擎".into(),
            ))
        }
    };

    // 全量计数（不加 limit）→ 实际取回（加 limit）
    let total_sql_count = store.count_by_condition(table, condition)? as usize;
    let rows = store.query_by_condition(table, condition, limit)?;

    let hits: Vec<ConditionHit> = rows
        .iter()
        .map(|row| ConditionHit {
            row: row.clone(),
            message: render_message(message, row),
        })
        .collect();

    Ok(ConditionScanResult {
        automation_id: id.clone(),
        total: total_sql_count,
        fetched: hits.len(),
        hits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::create_from_schema;
    use std::collections::HashMap;

    fn build_vehicle_db() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("tmp");
        let schema: crate::model::WorkspaceSchema =
            serde_json::from_value(serde_json::json!({
                "name": "车辆管理",
                "modules": [
                    { "type": "table", "id": "vehicles", "name": "车辆",
                      "fields": [
                        { "id": "plate", "name": "车牌", "type": "text", "required": true, "unique": true },
                        { "id": "status", "name": "状态", "type": "enum", "options": ["在用", "维修", "停用"] },
                        { "id": "inspection_date", "name": "年检", "type": "date" }
                      ] }
                ]
            }))
            .expect("schema");
        let mut seed = HashMap::new();
        seed.insert(
            "vehicles".to_string(),
            vec![
                // 未来很远 → 不命中；今天过期 → 命中
                serde_json::json!({ "plate": "京A11111", "status": "在用", "inspection_date": "2030-01-01" }),
                serde_json::json!({ "plate": "京A22222", "status": "在用", "inspection_date": "2026-08-01" }),
            ],
        );
        let id = create_from_schema(dir.path(), schema, &seed).expect("build").id;
        (dir, id)
    }

    fn automation() -> Automation {
        serde_json::from_value(serde_json::json!({
            "kind": "condition",
            "id": "a1",
            "name": "年检临期",
            "table": "vehicles",
            "condition": "inspection_date <= date('now','+7 days') and status != '停用'",
            "message": "车辆 {plate} 年检将于 {inspection_date} 到期，请安排年检",
            "frequency": "daily"
        }))
        .expect("automation")
    }

    #[test]
    fn condition_engine_hits_matching_rows_and_renders_message() {
        let (dir, id) = build_vehicle_db();
        let store = LocalSqliteStore::open(
            dir.path().join(&id).join("data.sqlite"),
            serde_json::from_value(serde_json::json!({
                "name": "车辆管理",
                "modules": [
                    { "type": "table", "id": "vehicles", "name": "车辆",
                      "fields": [
                        { "id": "plate", "name": "车牌", "type": "text", "required": true, "unique": true },
                        { "id": "status", "name": "状态", "type": "enum", "options": ["在用", "维修", "停用"] },
                        { "id": "inspection_date", "name": "年检", "type": "date" }
                      ] }
                ]
            }))
            .expect("schema"),
        )
        .expect("open");

        let result = run_condition_automation(&store, &automation(), 10).expect("run");

        // 今天过期的那台命中，未来那台不命中
        assert_eq!(result.total, 1);
        assert_eq!(result.fetched, 1);
        assert_eq!(result.hits.len(), 1);
        assert!(result.hits[0].message.contains("京A22222"));
        assert!(result.hits[0].message.contains("年检将于"));
    }

    #[test]
    fn limit_clamps_fetched_rows() {
        let (dir, id) = build_vehicle_db();
        let store = LocalSqliteStore::open(
            dir.path().join(&id).join("data.sqlite"),
            serde_json::from_value(serde_json::json!({
                "name": "车辆管理",
                "modules": [
                    { "type": "table", "id": "vehicles", "name": "车辆",
                      "fields": [
                        { "id": "plate", "name": "车牌", "type": "text", "required": true, "unique": true },
                        { "id": "status", "name": "状态", "type": "enum", "options": ["在用", "维修", "停用"] },
                        { "id": "inspection_date", "name": "年检", "type": "date" }
                      ] }
                ]
            }))
            .expect("schema"),
        )
        .expect("open");

        // 无条件匹配所有行，limit=1 → fetched=1, total=2
        let auto: Automation = serde_json::from_value(serde_json::json!({
            "kind": "condition",
            "id": "a2",
            "name": "全量",
            "table": "vehicles",
            "condition": "1 = 1",
            "message": "{plate}",
            "frequency": "daily"
        }))
        .expect("auto");

        let result = run_condition_automation(&store, &auto, 1).expect("run");
        assert_eq!(result.total, 2);
        assert_eq!(result.fetched, 1);
    }

    #[test]
    fn render_message_null_becomes_empty_unknown_field_preserved() {
        let row = serde_json::json!({ "plate": "京A1", "inspection_date": null });
        let msg = render_message("车 {plate} 年检 {inspection_date} 状态 {status}", &row);
        // 行内 null → 空串；行里不存在（schema 外）→ 保留占位符（失败信号）
        assert_eq!(msg, "车 京A1 年检  状态 {status}");
    }

    #[test]
    fn non_condition_automation_rejected() {
        let (dir, id) = build_vehicle_db();
        let store = LocalSqliteStore::open(
            dir.path().join(&id).join("data.sqlite"),
            serde_json::from_value(serde_json::json!({
                "name": "车辆管理",
                "modules": [
                    { "type": "table", "id": "vehicles", "name": "车辆",
                      "fields": [
                        { "id": "plate", "name": "车牌", "type": "text", "required": true, "unique": true },
                        { "id": "status", "name": "状态", "type": "enum", "options": ["在用", "维修", "停用"] },
                        { "id": "inspection_date", "name": "年检", "type": "date" }
                      ] }
                ]
            }))
            .expect("schema"),
        )
        .expect("open");

        let llm_auto: Automation = serde_json::from_value(serde_json::json!({
            "kind": "llm_summary",
            "id": "s1",
            "name": "周报",
            "schedule": "0 9 * * 1",
            "prompt": "总结本周",
            "output": "chat"
        }))
        .expect("auto");
        let err = run_condition_automation(&store, &llm_auto, 10).unwrap_err();
        assert!(matches!(err, ConditionError::Invalid(_)));
    }
}
