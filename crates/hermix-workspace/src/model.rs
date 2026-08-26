//! # WorkspaceSchema 领域模型
//!
//! 工作台定义（`workspace.json`）的 serde 结构，与
//! `docs/工作台架构设计方案.md` §5.3 的定义模型一一对应。
//!
//! 设计口径：
//! - **定义与数据分离**：本模型描述"长什么样"，数据存独立 SQLite（见 `store`）。
//! - **布局即数据**：`layout` 是 react-grid-layout 的可序列化输出，后端不解释
//!   其内部结构，LLM/前端可读写。
//! - **标识符 vs 展示名**：`id` 是建列/建表的标识符（必须通过 `validate` 的
//!   标识符安全校验），`name` 是可任意文本的展示名（可中文）。
//! - 字段类型白名单与 `store` 的建表映射一一对应。

use serde::{Deserialize, Serialize};

/// 字段类型白名单（`store` 建表时按此映射 SQLite 列类型）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    /// 单行文本
    Text,
    /// 数字（整数或浮点）
    Number,
    /// 日期（本地时区，`YYYY-MM-DD`）
    Date,
    /// 时间点（`YYYY-MM-DD HH:MM:SS`）
    Datetime,
    /// 布尔
    Boolean,
    /// 枚举（必填 `options`）
    Enum,
    /// 关系外键（必填 `target` 表 id，存目标表行 id）
    Relation,
    /// 附件（文件引用，存 `files/<row_id>/` 相对路径）
    Attachment,
}

/// 字段定义。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Field {
    /// 标识符（建列名，须过标识符安全校验）
    pub id: String,
    /// 展示名（可中文）
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: FieldType,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub unique: bool,
    /// Enum 选项（`field_type == Enum` 时必须非空）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    /// Relation 目标表 id（`field_type == Relation` 时必须存在）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// 默认值（可选）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
}

impl Field {
    pub fn is_enum(&self) -> bool {
        self.field_type == FieldType::Enum
    }

    pub fn is_relation(&self) -> bool {
        self.field_type == FieldType::Relation
    }
}

/// 视图类型（同一数据表的不同呈现方式）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewType {
    /// 表格
    Grid,
    /// 表单
    Form,
    /// 看板（`group_by` 指定分组字段）
    Kanban,
    /// 日历（`group_by` 指定日期字段）
    Calendar,
}

/// 视图定义（引用所属表内已存在的字段 id）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct View {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub view_type: ViewType,
    /// 展示的字段 id 列表（须存在于所属表）
    #[serde(default)]
    pub fields: Vec<String>,
    /// Kanban 分组字段 / Calendar 日期字段（须存在于所属表）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_by: Option<String>,
}

/// 仪表盘组件类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WidgetType {
    /// 统计卡（数字 + 文案）
    Stat,
    /// 图表
    Chart,
    /// 列表
    List,
}

/// 仪表盘组件（`query` 是受控查询片段，`store` 白名单校验后执行）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardWidget {
    pub id: String,
    #[serde(rename = "type")]
    pub widget_type: WidgetType,
    pub title: String,
    pub query: String,
}

/// 模块 = 数据表 或 仪表盘。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Module {
    /// 数据表：字段 + 视图
    Table {
        id: String,
        name: String,
        #[serde(default)]
        fields: Vec<Field>,
        #[serde(default)]
        views: Vec<View>,
    },
    /// 仪表盘：组件集合
    Dashboard {
        id: String,
        name: String,
        #[serde(default)]
        widgets: Vec<DashboardWidget>,
    },
}

impl Module {
    /// 模块 id（Table/Dashboard 共用访问）。
    pub fn id(&self) -> &str {
        match self {
            Module::Table { id, .. } | Module::Dashboard { id, .. } => id,
        }
    }

    /// 模块展示名。
    pub fn name(&self) -> &str {
        match self {
            Module::Table { name, .. } | Module::Dashboard { name, .. } => name,
        }
    }

    /// 取该表的字段列表（非表模块返回空）。
    pub fn table_fields(&self) -> Option<&[Field]> {
        match self {
            Module::Table { fields, .. } => Some(fields),
            Module::Dashboard { .. } => None,
        }
    }
}

/// 自动化规则（`kind` 区分确定性条件与 LLM 语义任务，见 §10）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Automation {
    /// 条件提醒：确定性 SQL 条件 + 消息模板（`{field}` 变量插值）。
    Condition {
        id: String,
        name: String,
        /// 目标表 id
        table: String,
        /// 单表 WHERE 片段（白名单校验后由调度器执行）
        condition: String,
        /// 消息模板，支持 `{field}` 插值
        message: String,
        /// 频率：daily / hourly / cron 表达式
        frequency: String,
    },
    /// LLM 定时分析（语义综合，走 sub-agent 隔离）。
    #[serde(rename = "llm_summary")]
    LlmSummary {
        id: String,
        name: String,
        /// cron 表达式
        schedule: String,
        /// 任务 prompt
        prompt: String,
        /// 产出落点：notify / report / chat
        output: String,
    },
}

/// 工作台定义 — `workspace.json` 的完整结构。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSchema {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// 工作台展示名
    pub name: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub modules: Vec<Module>,
    /// react-grid-layout 布局（后端不解释，LLM/前端可读写）
    #[serde(default)]
    pub layout: serde_json::Value,
    #[serde(default)]
    pub automations: Vec<Automation>,
    /// 权限模型预留（§17.4：工作台/表/视图级 + 行级）
    #[serde(default)]
    pub permissions: serde_json::Value,
}

const fn default_schema_version() -> u32 {
    1
}

impl WorkspaceSchema {
    /// 按 id 查模块。
    pub fn module(&self, id: &str) -> Option<&Module> {
        self.modules.iter().find(|m| m.id() == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_roundtrip_full_schema() {
        let json = r#"{
            "schema_version": 1,
            "name": "车辆管理工作台",
            "icon": "car",
            "modules": [
                {
                    "type": "table",
                    "id": "vehicles",
                    "name": "车辆台账",
                    "fields": [
                        { "id": "plate", "name": "车牌号", "type": "text", "required": true, "unique": true },
                        { "id": "inspection_date", "name": "年检日期", "type": "date" },
                        { "id": "status", "name": "状态", "type": "enum", "options": ["在用", "维修", "停用"] },
                        { "id": "owner", "name": "负责人", "type": "relation", "target": "personnel" }
                    ],
                    "views": [
                        { "id": "list", "name": "列表", "type": "grid", "fields": ["plate", "inspection_date", "status"] },
                        { "id": "board", "name": "看板", "type": "kanban", "group_by": "status" }
                    ]
                },
                {
                    "type": "dashboard",
                    "id": "overview",
                    "name": "总览",
                    "widgets": [
                        { "id": "w1", "type": "stat", "title": "在用车辆", "query": "count where status=在用" }
                    ]
                }
            ],
            "layout": { "overview": [{ "i": "w1", "x": 0, "y": 0, "w": 4, "h": 2 }] },
            "automations": [
                { "kind": "condition", "id": "a1", "name": "年检临期提醒", "table": "vehicles",
                  "condition": "inspection_date <= date('now','+7 days')", "message": "车辆 {plate} 年检到期", "frequency": "daily" },
                { "kind": "llm_summary", "id": "a2", "name": "每周安全周报", "schedule": "0 9 * * 1",
                  "prompt": "汇总上周安全事件", "output": "notify" }
            ],
            "permissions": { "roles": ["owner", "editor", "viewer"] }
        }"#;

        let schema: WorkspaceSchema = serde_json::from_str(json).expect("parse");
        assert_eq!(schema.name, "车辆管理工作台");
        assert_eq!(schema.modules.len(), 2);

        // Table 模块字段/视图解析
        let Module::Table { id, fields, views, .. } = &schema.modules[0] else {
            panic!("expected table module");
        };
        assert_eq!(id, "vehicles");
        assert_eq!(fields.len(), 4);
        assert_eq!(fields[2].field_type, FieldType::Enum);
        assert_eq!(fields[2].options.as_ref().unwrap().len(), 3);
        assert_eq!(fields[3].target.as_deref(), Some("personnel"));
        assert_eq!(views[1].view_type, ViewType::Kanban);
        assert_eq!(views[1].group_by.as_deref(), Some("status"));

        // Dashboard 模块
        let Module::Dashboard { id, widgets, .. } = &schema.modules[1] else {
            panic!("expected dashboard module");
        };
        assert_eq!(id, "overview");
        assert_eq!(widgets[0].widget_type, WidgetType::Stat);

        // 自动化
        assert_eq!(schema.automations.len(), 2);
        let Automation::Condition { table, frequency, .. } = &schema.automations[0] else {
            panic!("expected condition automation");
        };
        assert_eq!(table, "vehicles");
        assert_eq!(frequency, "daily");
        let Automation::LlmSummary { schedule, .. } = &schema.automations[1] else {
            panic!("expected llm_summary automation");
        };
        assert_eq!(schedule, "0 9 * * 1");

        // 布局/权限保持原始 JSON
        assert!(schema.layout["overview"].is_array());
        assert!(schema.permissions["roles"].is_array());

        // 序列化往返一致
        let out = serde_json::to_value(&schema).expect("serialize");
        let back: WorkspaceSchema = serde_json::from_value(out).expect("re-parse");
        assert_eq!(back.name, schema.name);
        assert_eq!(back.modules.len(), schema.modules.len());
    }

    #[test]
    fn serde_roundtrip_minimal_schema() {
        // 缺省字段走 default（schema_version=1 / 空模块 / 空布局）
        let json = r#"{ "name": "空工作台" }"#;
        let schema: WorkspaceSchema = serde_json::from_str(json).expect("parse");
        assert_eq!(schema.schema_version, 1);
        assert!(schema.modules.is_empty());
        assert!(schema.automations.is_empty());
    }

    #[test]
    fn module_lookup_helpers() {
        let schema: WorkspaceSchema = serde_json::from_str(
            r#"{"name":"t","modules":[{"type":"table","id":"a","name":"A","fields":[]}]}"#,
        )
        .expect("parse");
        assert!(schema.module("a").is_some());
        assert!(schema.module("nope").is_none());
        let m = schema.module("a").unwrap();
        assert_eq!(m.name(), "A");
        assert!(m.table_fields().is_some());
        assert!(m.table_fields().unwrap().is_empty());
    }
}
