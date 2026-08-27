//! # Schema 校验
//!
//! 建表/注册工具前必须通过本校验，从源头拦截 LLM 生成的错误 schema
//! （`docs/工作台架构设计方案.md` §14.1 失败模式的对策）。
//!
//! 校验项：
//! - **标识符安全**：表/字段/视图/组件 id 必须是 ASCII 标识符
//!   （`[A-Za-z_][A-Za-z0-9_]*`）——这是防 SQL 注入的第一道防线
//!   （`store` 用 id 拼列名/表名，绝不经用户/LLM 裸拼接）。
//! - **类型约束**：Enum 必须有非空 `options`；Relation 必须有 `target`。
//! - **引用完整性**：视图字段/`group_by` 必须存在于所属表；Relation 的
//!   `target` 必须指向已存在的表模块；Automation 的 `table` 必须存在。
//! - **id 唯一性**：模块/字段/视图/组件 id 在各自作用域内唯一。
//!
//! 返回首个错误（`Err(ValidationError::Invalid(msg))`），消息带具体
//! 位置（如 `module[vehicles].field[status]`），便于 LLM 修正后重试。

use crate::model::{Automation, Field, Module, View, WorkspaceSchema};

/// Schema 校验错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("schema 校验失败: {0}")]
    Invalid(String),
}

/// 校验整个工作台 schema。通过返回 `Ok(())`。
pub fn validate_schema(schema: &WorkspaceSchema) -> Result<(), ValidationError> {
    if schema.name.trim().is_empty() {
        return err("工作台 name 不能为空");
    }

    // 0. 权限（M7.2）：mode 必须是合法值；allow_delete 必须是布尔
    if let Some(perms_obj) = schema.permissions.as_object() {
        if let Some(mode) = perms_obj.get("mode").and_then(|v| v.as_str()) {
            if mode != "readwrite" && mode != "readonly" {
                return err(&format!(
                    "permissions.mode 必须是 readwrite/readonly，得到 {mode}"
                ));
            }
        }
        if let Some(ad) = perms_obj.get("allow_delete") {
            if !ad.is_boolean() {
                return err("permissions.allow_delete 必须是布尔");
            }
        }
    }

    // 1. 模块 id 全局唯一
    let mut seen_modules: Vec<&str> = Vec::new();
    for m in &schema.modules {
        let id = m.id();
        check_identifier("module", id)?;
        if seen_modules.contains(&id) {
            return err(&format!("module[{id}] 重复"));
        }
        seen_modules.push(id);
    }

    // 2. 表 id 集合（Relation target / Automation table 的引用目标）
    let table_ids: Vec<&str> = schema
        .modules
        .iter()
        .filter_map(|m| match m {
            Module::Table { id, .. } => Some(id.as_str()),
            Module::Dashboard { .. } => None,
        })
        .collect();

    // 3. 逐模块校验
    for m in &schema.modules {
        match m {
            Module::Table { id, fields, views, .. } => {
                validate_table(id, fields, views, &table_ids)?;
            }
            Module::Dashboard { id, widgets, .. } => {
                validate_dashboard(id, widgets)?;
            }
        }
    }

    // 4. 自动化引用
    for a in &schema.automations {
        if let Automation::Condition { id, table, .. } = a {
            check_identifier("automation", id)?;
            if !table_ids.contains(&table.as_str()) {
                return err(&format!("automation[{id}] 引用的表[{table}]不存在"));
            }
        }
    }

    Ok(())
}

fn validate_table(
    table_id: &str,
    fields: &[Field],
    views: &[View],
    table_ids: &[&str],
) -> Result<(), ValidationError> {
    // 字段 id 唯一 + 标识符安全 + 类型约束
    let mut seen_fields: Vec<&str> = Vec::new();
    for f in fields {
        let loc = format!("module[{table_id}].field[{}]", f.id);
        check_identifier("field", &f.id).map_err(|_| {
            ValidationError::Invalid(format!("{loc}: 字段 id 必须是 ASCII 标识符"))
        })?;
        if seen_fields.contains(&f.id.as_str()) {
            return err(&format!("{loc} 重复"));
        }
        seen_fields.push(&f.id);

        if f.is_enum() {
            let opts = f.options.as_ref().map(Vec::as_slice).unwrap_or(&[]);
            if opts.is_empty() {
                return err(&format!("{loc}: Enum 类型必须提供非空 options"));
            }
            // 枚举选项不能为空字符串
            if opts.iter().any(|o| o.trim().is_empty()) {
                return err(&format!("{loc}: Enum options 不能含空字符串"));
            }
        }
        if f.is_relation() {
            match f.target.as_deref() {
                None => return err(&format!("{loc}: Relation 类型必须提供 target 表 id")),
                Some(t) => {
                    if !table_ids.contains(&t) {
                        return err(&format!("{loc}: Relation target[{t}] 指向的表不存在"));
                    }
                }
            }
        }
    }

    // 视图 id 唯一 + 字段引用存在
    let mut seen_views: Vec<&str> = Vec::new();
    for v in views {
        let loc = format!("module[{table_id}].view[{}]", v.id);
        check_identifier("view", &v.id).map_err(|_| {
            ValidationError::Invalid(format!("{loc}: 视图 id 必须是 ASCII 标识符"))
        })?;
        if seen_views.contains(&v.id.as_str()) {
            return err(&format!("{loc} 重复"));
        }
        seen_views.push(&v.id);

        for fid in &v.fields {
            if !seen_fields.contains(&fid.as_str()) {
                return err(&format!("{loc}: 引用字段[{fid}]不存在"));
            }
        }
        if let Some(g) = &v.group_by {
            if !seen_fields.contains(&g.as_str()) {
                return err(&format!("{loc}: group_by 字段[{g}]不存在"));
            }
        }
    }

    Ok(())
}

fn validate_dashboard(
    dashboard_id: &str,
    widgets: &[crate::model::DashboardWidget],
) -> Result<(), ValidationError> {
    let mut seen: Vec<&str> = Vec::new();
    for w in widgets {
        let loc = format!("module[{dashboard_id}].widget[{}]", w.id);
        check_identifier("widget", &w.id).map_err(|_| {
            ValidationError::Invalid(format!("{loc}: 组件 id 必须是 ASCII 标识符"))
        })?;
        if seen.contains(&w.id.as_str()) {
            return err(&format!("{loc} 重复"));
        }
        seen.push(&w.id);
        if w.title.trim().is_empty() {
            return err(&format!("{loc}: title 不能为空"));
        }
        if w.query.trim().is_empty() {
            return err(&format!("{loc}: query 不能为空"));
        }
    }
    Ok(())
}

/// 标识符安全校验：`[A-Za-z_][A-Za-z0-9_]*`。
fn check_identifier(kind: &str, id: &str) -> Result<(), ValidationError> {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return err(&format!("{kind}[{id}] 不是合法标识符")),
    }
    if chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        Ok(())
    } else {
        err(&format!("{kind}[{id}] 不是合法标识符"))
    }
}

fn err(msg: &str) -> Result<(), ValidationError> {
    Err(ValidationError::Invalid(msg.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::WorkspaceSchema;

    fn parse(json: &str) -> WorkspaceSchema {
        serde_json::from_str(json).expect("schema json must parse")
    }

    const VALID: &str = r#"{
        "name": "车辆管理",
        "modules": [
            { "type": "table", "id": "vehicles", "name": "车辆",
              "fields": [
                { "id": "plate", "name": "车牌", "type": "text", "required": true, "unique": true },
                { "id": "status", "name": "状态", "type": "enum", "options": ["在用", "维修"] },
                { "id": "owner", "name": "负责人", "type": "relation", "target": "personnel" },
                { "id": "inspection_date", "name": "年检", "type": "date" }
              ],
              "views": [
                { "id": "list", "name": "列表", "type": "grid", "fields": ["plate", "status"] },
                { "id": "board", "name": "看板", "type": "kanban", "group_by": "status" }
              ] },
            { "type": "table", "id": "personnel", "name": "人员", "fields": [] },
            { "type": "dashboard", "id": "overview", "name": "总览",
              "widgets": [ { "id": "w1", "type": "stat", "title": "在用", "query": "count where status=在用" } ] }
        ],
        "automations": [
            { "kind": "condition", "id": "a1", "name": "临期", "table": "vehicles",
              "condition": "inspection_date <= date('now','+7 days')", "message": "{plate}", "frequency": "daily" }
        ]
    }"#;

    #[test]
    fn valid_schema_passes() {
        let s = parse(VALID);
        assert!(validate_schema(&s).is_ok());
    }

    #[test]
    fn empty_name_rejected() {
        let s = parse(r#"{ "name": "  ", "modules": [] }"#);
        assert!(validate_schema(&s).is_err());
    }

    #[test]
    fn unsafe_identifier_rejected() {
        // 字段 id 含中文/连字符/空格 → 拒绝（防注入）
        let s = parse(
            r#"{"name":"t","modules":[{"type":"table","id":"ok","name":"T",
                "fields":[{"id":"bad-id 名","name":"X","type":"text"}]}]}"#,
        );
        let err = validate_schema(&s).unwrap_err().to_string();
        assert!(err.contains("field"), "{err}");
        assert!(err.contains("标识符"), "{err}");
    }

    #[test]
    fn duplicate_module_rejected() {
        let s = parse(
            r#"{"name":"t","modules":[
                {"type":"table","id":"a","name":"A","fields":[]},
                {"type":"table","id":"a","name":"B","fields":[]}]}"#,
        );
        assert!(validate_schema(&s).unwrap_err().to_string().contains("重复"));
    }

    #[test]
    fn enum_without_options_rejected() {
        let s = parse(
            r#"{"name":"t","modules":[{"type":"table","id":"a","name":"A",
                "fields":[{"id":"st","name":"状态","type":"enum"}]}]}"#,
        );
        let err = validate_schema(&s).unwrap_err().to_string();
        assert!(err.contains("options"), "{err}");
    }

    #[test]
    fn relation_target_missing_rejected() {
        let s = parse(
            r#"{"name":"t","modules":[{"type":"table","id":"a","name":"A",
                "fields":[{"id":"owner","name":"负责人","type":"relation","target":"nope"}]}]}"#,
        );
        let err = validate_schema(&s).unwrap_err().to_string();
        assert!(err.contains("不存在"), "{err}");
    }

    #[test]
    fn view_referencing_missing_field_rejected() {
        let s = parse(
            r#"{"name":"t","modules":[{"type":"table","id":"a","name":"A",
                "fields":[{"id":"x","name":"X","type":"text"}],
                "views":[{"id":"v","name":"V","type":"grid","fields":["y"]}]}]}"#,
        );
        let err = validate_schema(&s).unwrap_err().to_string();
        assert!(err.contains("y"), "{err}");
    }

    #[test]
    fn kanban_group_by_missing_rejected() {
        let s = parse(
            r#"{"name":"t","modules":[{"type":"table","id":"a","name":"A",
                "fields":[{"id":"x","name":"X","type":"text"}],
                "views":[{"id":"v","name":"V","type":"kanban","group_by":"nope"}]}]}"#,
        );
        assert!(validate_schema(&s).is_err());
    }

    #[test]
    fn automation_table_missing_rejected() {
        let s = parse(
            r#"{"name":"t","modules":[{"type":"table","id":"a","name":"A","fields":[]}],
                "automations":[{"kind":"condition","id":"a1","name":"r","table":"ghost",
                  "condition":"1=1","message":"x","frequency":"daily"}]}"#,
        );
        let err = validate_schema(&s).unwrap_err().to_string();
        assert!(err.contains("ghost"), "{err}");
    }

    #[test]
    fn duplicate_field_rejected() {
        let s = parse(
            r#"{"name":"t","modules":[{"type":"table","id":"a","name":"A",
                "fields":[{"id":"x","name":"X","type":"text"},{"id":"x","name":"Y","type":"text"}]}]}"#,
        );
        assert!(validate_schema(&s).unwrap_err().to_string().contains("重复"));
    }
}
