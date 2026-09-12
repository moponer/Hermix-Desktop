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
//! - **布局合法性**：`layout` 的键必须是已存在的 dashboard 模块，每项必须含
//!   `i/x/y/w/h`、`i` 必须在 widgets 内、尺寸与坐标必须落在 12 列网格内
//!   （重叠不拦，react-grid-layout 会推开，且存量数据可能已重叠）。
//!
//! 返回首个错误（`Err(ValidationError::Invalid(msg))`），消息带具体
//! 位置（如 `module[vehicles].field[status]`），便于 LLM 修正后重试。

use crate::model::{Automation, Field, Module, PermissionMode, View, WorkspaceSchema};

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

    // 0. 权限（M7.2）：mode 若存在必须是合法枚举字符串（非字符串同样拒绝，
    //    否则运行时按缺省 readwrite 解析，权限收紧会被静默吞掉）；
    //    allow_delete 必须是布尔。合法值集合以 PermissionMode::parse 为准。
    if let Some(perms_obj) = schema.permissions.as_object() {
        if let Some(mode) = perms_obj.get("mode") {
            if mode.as_str().and_then(PermissionMode::parse).is_none() {
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

    // 5. 布局（LLM 编排画布的唯一入口，从源头拦截越界/悬空布局）
    validate_layout(schema)?;

    Ok(())
}

/// 布局网格列数 — 与前端 `DashboardView.tsx` 的 `COLS` 对齐。
const GRID_COLS: f64 = 12.0;

/// 校验 `schema.layout`（react-grid-layout 可序列化输出）：
/// - 顶层必须是对象，键必须是已存在的模块 id（悬空即报）；
/// - 每个 item 必须含 `i/x/y/w/h`，尺寸与坐标必须落在网格内
///   （`w ∈ [1,12]`、`h ≥ 1`、`x/y ≥ 0`、`x+w ≤ 12`）；
/// - 键指向 **dashboard** 时额外校验 `i` 在 widgets 内；键指向 **table** 时
///   只校几何（table 无 widgets 概念，内置的空数组占位属正常存量）。
///
/// **重叠不拒绝**：react-grid-layout 渲染时会自动推开，且存量工作台可能已有
/// 重叠布局，严格拒绝会破坏既有数据。这里只拦"渲染不出东西"的硬错误。
fn validate_layout(schema: &WorkspaceSchema) -> Result<(), ValidationError> {
    let layout = match schema.layout.as_object() {
        Some(o) if !o.is_empty() => o,
        _ => return Ok(()),
    };

    for (module_id, raw_items) in layout {
        // None = table 等非 dashboard 模块，无 widgets 可核对，只做几何校验。
        let widgets = match schema.module(module_id) {
            Some(Module::Dashboard { widgets, .. }) => Some(widgets),
            Some(_) => None,
            None => return err(&format!("layout[{module_id}] 引用的模块不存在")),
        };
        let items = match raw_items.as_array() {
            Some(a) => a,
            None => return err(&format!("layout[{module_id}] 必须是数组")),
        };

        for (idx, item) in items.iter().enumerate() {
            let loc = format!("layout[{module_id}][{idx}]");
            let obj = match item.as_object() {
                Some(o) => o,
                None => return err(&format!("{loc} 必须是对象（含 i/x/y/w/h）")),
            };
            let num = |key: &str| -> Option<f64> { obj.get(key).and_then(|v| v.as_f64()) };
            let i = match obj.get("i").and_then(|v| v.as_str()) {
                Some(s) => s,
                None => return err(&format!("{loc} 缺少 i（组件 id）")),
            };
            if let Some(widgets) = widgets {
                if !widgets.iter().any(|w| w.id == i) {
                    return err(&format!("{loc} 的 i[{i}] 不在模块[{module_id}]的 widgets 中"));
                }
            }
            for key in ["x", "y", "w", "h"] {
                if num(key).is_none() {
                    return err(&format!("{loc} 的 {key} 必须是数值"));
                }
            }
            let x = num("x").unwrap_or(0.0);
            let y = num("y").unwrap_or(0.0);
            let w = num("w").unwrap_or(0.0);
            let h = num("h").unwrap_or(0.0);
            if w < 1.0 {
                return err(&format!("{loc} 的 w 必须 ≥ 1（网格共 {} 列）", GRID_COLS));
            }
            if h < 1.0 {
                return err(&format!("{loc} 的 h 必须 ≥ 1"));
            }
            if x < 0.0 || y < 0.0 {
                return err(&format!("{loc} 的 x/y 不能为负"));
            }
            if x + w > GRID_COLS {
                return err(&format!(
                    "{loc} 越界：x({x}) + w({w}) 超过 {GRID_COLS} 列网格"
                ));
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

    // -----------------------------------------------------------------
    // 布局校验（LLM 编排画布的质量底线）
    // -----------------------------------------------------------------

    /// 带一个 dashboard 模块（含 stat/chart 两个组件）的最小 schema 前缀，
    /// 各用例拼上不同 layout 复用。
    const DASH: &str = r#"{"name":"t","modules":[{"type":"dashboard","id":"ov","name":"总览",
        "widgets":[
            {"id":"w1","type":"stat","title":"在用","query":"count where status=在用"},
            {"id":"w2","type":"chart","title":"趋势","query":"count group by month"}
        ]}]}"#;

    fn with_layout(layout: &str) -> WorkspaceSchema {
        parse(&format!(
            r#"{{"name":"t","modules":[{{"type":"dashboard","id":"ov","name":"总览",
                "widgets":[{{"id":"w1","type":"stat","title":"在用","query":"q1"}}]}}],
                "layout":{layout}}}"#
        ))
    }

    #[test]
    fn layout_valid_passes() {
        let s = with_layout(r#"{"ov":[{"i":"w1","x":0,"y":0,"w":3,"h":2}]}"#);
        assert!(validate_schema(&s).is_ok());
        // 空 layout / 无 layout 一律放行（前端 autoLayout 兜底）
        assert!(validate_schema(&parse(DASH)).is_ok());
        assert!(validate_schema(&parse(r#"{"name":"t"}"#)).is_ok());

        // table 模块的空数组占位放行 —— 内置模板（如人员管理）就是这种形态，
        // table 无 widgets 概念，几何越界仍会被拦，但不要求 i 匹配 widgets。
        let s = parse(
            r#"{"name":"t","modules":[{"type":"table","id":"a","name":"A","fields":[]}],
                "layout":{"a":[]}}"#,
        );
        assert!(validate_schema(&s).is_ok());
    }

    #[test]
    fn layout_width_overflow_rejected() {
        // w=13 在 12 列网格里越界
        let s = with_layout(r#"{"ov":[{"i":"w1","x":0,"y":0,"w":13,"h":2}]}"#);
        let err = validate_schema(&s).unwrap_err().to_string();
        assert!(err.contains("越界"), "{err}");

        // x=8 + w=6 = 14 同样越界
        let s = with_layout(r#"{"ov":[{"i":"w1","x":8,"y":0,"w":6,"h":2}]}"#);
        let err = validate_schema(&s).unwrap_err().to_string();
        assert!(err.contains("越界"), "{err}");
    }

    #[test]
    fn layout_dangling_widget_rejected() {
        // i 指向不存在的组件 → 渲染时该布局项无所指
        let s = with_layout(r#"{"ov":[{"i":"ghost","x":0,"y":0,"w":3,"h":2}]}"#);
        let err = validate_schema(&s).unwrap_err().to_string();
        assert!(err.contains("ghost"), "{err}");

        // layout 的键不是已存在的 dashboard 模块
        let s = with_layout(r#"{"nope":[{"i":"w1","x":0,"y":0,"w":3,"h":2}]}"#);
        let err = validate_schema(&s).unwrap_err().to_string();
        assert!(err.contains("nope"), "{err}");
    }

    #[test]
    fn layout_bad_geometry_rejected() {
        // 缺字段 / 零尺寸 / 负坐标
        let s = with_layout(r#"{"ov":[{"i":"w1","x":0,"y":0,"w":3}]}"#);
        assert!(validate_schema(&s).unwrap_err().to_string().contains("h"));

        let s = with_layout(r#"{"ov":[{"i":"w1","x":0,"y":0,"w":0,"h":2}]}"#);
        let err = validate_schema(&s).unwrap_err().to_string();
        assert!(err.contains("w 必须"), "{err}");

        let s = with_layout(r#"{"ov":[{"i":"w1","x":-1,"y":0,"w":3,"h":2}]}"#);
        assert!(validate_schema(&s).unwrap_err().to_string().contains("不能为负"));
    }

    #[test]
    fn layout_overlap_allowed() {
        // 重叠不拒绝：react-grid-layout 会推开，且存量数据可能已重叠
        let s = with_layout(
            r#"{"ov":[{"i":"w1","x":0,"y":0,"w":6,"h":2},{"i":"w1","x":2,"y":0,"w":6,"h":2}]}"#,
        );
        assert!(validate_schema(&s).is_ok());
    }
}
