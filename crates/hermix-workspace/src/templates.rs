//! # 内置工作台模板（M6.2）— 预置台账，一键建工作台
//!
//! 模板本质就是一份 `WorkspaceSchema`（用户最早提的三个例子：人员管理、
//! 车辆管理、安全管理台账），存为内联 JSON——离线可用、零新依赖，
//! 直接复用 M2 的 `create_from_schema` 建设（M6.4）。
//!
//! 与 .hwx 的分工：模板是**干净起步**（可带少量示例数据）；.hwx 是
//! **带真实数据的交换格式**（M5）。模板不含用户数据。
//!
//! 每个模板经过 `validate_schema` 校验后才允许分发（防坏模板落地）；
//! 列表/详情只暴露元信息，schema 在建工作台时按需取用。

use crate::model::WorkspaceSchema;
#[cfg(test)]
use crate::validate::validate_schema;

/// 模板元信息 + 定义。
#[derive(Debug, Clone)]
pub struct WorkspaceTemplate {
    pub id: &'static str,
    pub name: &'static str,
    pub icon: &'static str,
    pub description: &'static str,
    pub schema: WorkspaceSchema,
}

// ---------------------------------------------------------------------------
// 模板定义（内联 JSON，与 model.rs 测试同风格）
// ---------------------------------------------------------------------------

fn personnel_schema() -> WorkspaceSchema {
    serde_json::from_value(serde_json::json!({
        "name": "人员管理",
        "icon": "users",
        "modules": [
            { "type": "table", "id": "personnel", "name": "人员台账",
              "fields": [
                { "id": "name", "name": "姓名", "type": "text", "required": true },
                { "id": "department", "name": "部门", "type": "enum",
                  "options": ["管理部", "生产部", "安全部", "维修部", "外协"] },
                { "id": "position", "name": "职位", "type": "text" },
                { "id": "phone", "name": "电话", "type": "text" },
                { "id": "hire_date", "name": "入职日期", "type": "date" },
                { "id": "status", "name": "状态", "type": "enum",
                  "options": ["在职", "休假", "离职"] }
              ],
              "views": [
                { "id": "list", "name": "列表", "type": "grid",
                  "fields": ["name", "department", "position", "phone", "status"] },
                { "id": "board", "name": "部门看板", "type": "kanban", "group_by": "department" }
              ] }
        ],
        "layout": { "personnel": [] }
    }))
    .expect("personnel 模板")
}

fn vehicle_schema() -> WorkspaceSchema {
    serde_json::from_value(serde_json::json!({
        "name": "车辆管理",
        "icon": "car",
        "modules": [
            { "type": "table", "id": "vehicles", "name": "车辆台账",
              "fields": [
                { "id": "plate", "name": "车牌号", "type": "text", "required": true, "unique": true },
                { "id": "model", "name": "车型", "type": "text" },
                { "id": "owner", "name": "负责人", "type": "text" },
                { "id": "inspection_date", "name": "年检日期", "type": "date" },
                { "id": "mileage", "name": "里程", "type": "number" },
                { "id": "status", "name": "状态", "type": "enum",
                  "options": ["在用", "维修", "停用", "报废"] }
              ],
              "views": [
                { "id": "list", "name": "列表", "type": "grid",
                  "fields": ["plate", "model", "owner", "inspection_date", "status"] },
                { "id": "board", "name": "状态看板", "type": "kanban", "group_by": "status" }
              ] },
            { "type": "dashboard", "id": "overview", "name": "总览",
              "widgets": [
                { "id": "w1", "type": "stat", "title": "在用车辆",
                  "query": "count where status=在用" },
                { "id": "w2", "type": "stat", "title": "维修中",
                  "query": "count where status=维修" }
              ] }
        ],
        "layout": { "overview": [
            { "i": "w1", "x": 0, "y": 0, "w": 3, "h": 2 },
            { "i": "w2", "x": 3, "y": 0, "w": 3, "h": 2 }
        ] },
        "automations": [
            { "kind": "condition", "id": "a1", "name": "年检临期提醒", "table": "vehicles",
              "condition": "inspection_date <= date('now','+7 days') and status != '停用'",
              "message": "车辆 {plate} 年检将于 {inspection_date} 到期，请安排年检",
              "frequency": "daily" }
        ]
    }))
    .expect("车辆模板")
}

fn safety_schema() -> WorkspaceSchema {
    serde_json::from_value(serde_json::json!({
        "name": "安全管理台账",
        "icon": "shield",
        "modules": [
            { "type": "table", "id": "safety_events", "name": "安全事件",
              "fields": [
                { "id": "date", "name": "日期", "type": "date", "required": true },
                { "id": "category", "name": "类别", "type": "enum",
                  "options": ["隐患排查", "违章记录", "事故报告", "培训记录", "设备检查"] },
                { "id": "location", "name": "地点", "type": "text" },
                { "id": "description", "name": "描述", "type": "text" },
                { "id": "owner", "name": "责任人", "type": "text" },
                { "id": "status", "name": "状态", "type": "enum",
                  "options": ["待处理", "处理中", "已闭环"] }
              ],
              "views": [
                { "id": "list", "name": "列表", "type": "grid",
                  "fields": ["date", "category", "location", "owner", "status"] },
                { "id": "board", "name": "状态看板", "type": "kanban", "group_by": "status" }
              ] },
            { "type": "dashboard", "id": "overview", "name": "总览",
              "widgets": [
                { "id": "w1", "type": "stat", "title": "待处理",
                  "query": "count where status=待处理" },
                { "id": "w2", "type": "list", "title": "最近隐患",
                  "query": "list where category=隐患排查 order by date desc limit 5" }
              ] }
        ],
        "layout": { "overview": [
            { "i": "w1", "x": 0, "y": 0, "w": 3, "h": 2 },
            { "i": "w2", "x": 3, "y": 0, "w": 5, "h": 4 }
        ] },
        "automations": [
            { "kind": "condition", "id": "a1", "name": "未闭环提醒", "table": "safety_events",
              "condition": "status != '已闭环' and date <= date('now','-7 days')",
              "message": "安全事件（{date}，{category}，{location}）超过 7 天未闭环，请处理",
              "frequency": "daily" }
        ]
    }))
    .expect("安全模板")
}

// ---------------------------------------------------------------------------
// 列表 / 按 id 取
// ---------------------------------------------------------------------------

/// 全部内置模板（保持固定顺序：人员 / 车辆 / 安全）。
pub fn list_templates() -> Vec<WorkspaceTemplate> {
    vec![
        WorkspaceTemplate {
            id: "personnel",
            name: "人员管理",
            icon: "users",
            description: "人员台账：部门/职位/联系方式/在职状态，支持部门看板。",
            schema: personnel_schema(),
        },
        WorkspaceTemplate {
            id: "vehicles",
            name: "车辆管理",
            icon: "car",
            description: "车辆台账：车牌/年检/里程/状态看板 + 总览统计 + 年检临期自动提醒。",
            schema: vehicle_schema(),
        },
        WorkspaceTemplate {
            id: "safety",
            name: "安全管理台账",
            icon: "shield",
            description: "安全事件台账：隐患排查/违章/事故分类管理 + 未闭环自动提醒。",
            schema: safety_schema(),
        },
    ]
}

/// 按 id 取模板（不存在 → None）。
pub fn get_template(id: &str) -> Option<WorkspaceTemplate> {
    list_templates().into_iter().find(|t| t.id == id)
}

/// 模板列表的元信息（M6.3 列表 API 的返回，不含 schema 全文）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TemplateMeta {
    pub id: &'static str,
    pub name: &'static str,
    pub icon: &'static str,
    pub description: &'static str,
    /// 表模块清单（id + name），供前端预览"这个模板有哪些表"。
    pub tables: Vec<TemplateTableMeta>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TemplateTableMeta {
    pub id: String,
    pub name: String,
}

/// 从 schema 提取全部表 id（测试辅助，按模块顺序）。
#[cfg(test)]
fn store_schema_tables(schema: &WorkspaceSchema) -> Vec<String> {
    schema
        .modules
        .iter()
        .filter_map(|m| match m {
            crate::model::Module::Table { id, .. } => Some(id.clone()),
            crate::model::Module::Dashboard { .. } => None,
        })
        .collect()
}

/// 全部模板的元信息（表清单从 schema 提取）。
pub fn list_template_metas() -> Vec<TemplateMeta> {
    list_templates()
        .into_iter()
        .map(|t| {
            let tables = t
                .schema
                .modules
                .iter()
                .filter_map(|m| match m {
                    crate::model::Module::Table { id, name, .. } => {
                        Some(TemplateTableMeta {
                            id: id.clone(),
                            name: name.clone(),
                        })
                    }
                    crate::model::Module::Dashboard { .. } => None,
                })
                .collect();
            TemplateMeta {
                id: t.id,
                name: t.name,
                icon: t.icon,
                description: t.description,
                tables,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::WorkspaceStore;

    /// 每个模板必须通过 validate_schema（防坏模板落地）。
    #[test]
    fn all_templates_pass_validation() {
        for t in list_templates() {
            let err = validate_schema(&t.schema);
            assert!(err.is_ok(), "模板[{}]校验失败: {:?}", t.id, err);
        }
    }

    #[test]
    fn list_has_three_presets_in_order() {
        let templates = list_templates();
        assert_eq!(templates.len(), 3);
        assert_eq!(templates[0].id, "personnel");
        assert_eq!(templates[1].id, "vehicles");
        assert_eq!(templates[2].id, "safety");
    }

    #[test]
    fn get_by_id() {
        assert!(get_template("vehicles").is_some());
        assert!(get_template("nope").is_none());
    }

    #[test]
    fn metas_expose_tables_not_schema() {
        let metas = list_template_metas();
        let vehicle = metas.iter().find(|m| m.id == "vehicles").expect("vehicles");
        assert!(vehicle.tables.iter().any(|t| t.id == "vehicles"));
        assert!(vehicle.tables.iter().any(|t| t.id == "overview" || t.id == "drivers") == false);
        // 表清单只含 table 模块（overview 是 dashboard，不应出现在表清单）
        assert!(!vehicle.tables.iter().any(|t| t.id == "overview"));
    }

    /// 车辆模板含条件提醒自动化（M6.4 建工作台后自测应通过）。
    #[test]
    fn vehicle_template_has_condition_automation() {
        let t = get_template("vehicles").expect("vehicles");
        assert!(t.schema.automations.iter().any(|a| match a {
            crate::model::Automation::Condition { id, .. } => id == "a1",
            _ => false,
        }));
    }

    /// M6.4 验收：模板 → create_from_schema 一键建成 → 自测通过 →
    /// 数据可写（模拟模板 API 的 create_from_template 全链路）。
    #[test]
    fn template_builds_working_workspace() {
        for tpl in list_templates() {
            let dir = tempfile::tempdir().expect("tmp");
            let result = crate::builder::create_from_schema(
                dir.path(),
                tpl.schema.clone(),
                &Default::default(),
            )
            .expect("模板建设失败");

            // 自测通过（表可查；车辆/安全模板的自动化条件可编译）
            assert!(!result.smoke.is_empty(), "模板[{}]自测应为空摘要", tpl.id);
            assert!(
                result.smoke.iter().all(|s| !s.contains("失败")),
                "模板[{}]自测含失败项: {:?}",
                tpl.id,
                result.smoke
            );

            // 数据可写：每个模板的第一张表插入一行（用模板 schema 重开 store）。
            // 构造满足必填字段的行（按类型填默认值，空对象会违反必填校验）
            let first_table = store_schema_tables(&tpl.schema)
                .into_iter()
                .next()
                .expect("模板应有至少一张表");
            let store = crate::store::LocalSqliteStore::open(
                dir.path().join(&result.id).join("data.sqlite"),
                crate::templates::get_template(tpl.id).expect("tpl").schema,
            )
            .expect("reopen");
            let fields = store
                .schema()
                .modules
                .iter()
                .find_map(|m| match m {
                    crate::model::Module::Table { id, fields, .. } if id == &first_table => {
                        Some(fields)
                    }
                    crate::model::Module::Table { .. } | crate::model::Module::Dashboard { .. } => {
                        None
                    }
                })
                .expect("表字段");
            let mut row = serde_json::Map::new();
            for f in fields {
                if f.required && !row.contains_key(&f.id) {
                    let v = match f.field_type {
                        crate::model::FieldType::Text
                        | crate::model::FieldType::Relation
                        | crate::model::FieldType::Attachment => serde_json::json!("测试"),
                        crate::model::FieldType::Number => serde_json::json!(0),
                        crate::model::FieldType::Date
                        | crate::model::FieldType::Datetime => serde_json::json!("2026-01-01"),
                        crate::model::FieldType::Boolean => serde_json::json!(false),
                        crate::model::FieldType::Enum => serde_json::json!(
                            f.options
                                .as_ref()
                                .and_then(|o| o.first())
                                .cloned()
                                .unwrap_or_default()
                        ),
                    };
                    row.insert(f.id.clone(), v);
                }
            }
            let inserted = store
                .insert(&first_table, &[serde_json::Value::Object(row)], "template-test")
                .expect("模板表应可插入");
            assert_eq!(inserted.len(), 1);
        }
    }
}
