//! # hermix-workspace
//!
//! 工作台引擎 — Schema 驱动的用户自定义工作台（人员/车辆/安全台账等管理类）。
//!
//! 架构口径（见 `docs/工作台架构设计方案.md`）：
//! - **Schema 驱动为主**：`WorkspaceSchema` 声明式定义（表/视图/仪表盘/提醒），
//!   通用渲染器呈现，数据走后端 SQLite。
//! - **每工作台独立 SQLite 库**（隔离 + 备份/导出天然），定义与数据分离。
//! - **数据模型 sync-friendly**：行主键 UUID + `created_at`/`updated_at`/`updated_by`。
//! - **LLM 永远通过工具操作数据**（M3 起注册 `workspace_*` 工具族），本 crate
//!   只提供校验后的数据访问层。
//!
//! 模块：
//! - [`model`] — `WorkspaceSchema` 领域模型（serde 往返）
//! - [`validate`] — schema 校验（标识符安全/类型/枚举/关系/视图/自动化引用）
//! - [`store`] — `WorkspaceStore` trait + `LocalSqliteStore` 实现
//! - [`registry`] — `registry.sqlite` 工作台列表 CRUD
//! - [`builder`] — 对话建设管线（schema → 建目录/定义/DB/建表/导入/自测）
//! - [`migrate`] — schema 增量迁移（加列/加表/枚举存量校验，删改保留数据）
//! - [`service`] — 数据服务层（id → 目录/定义/store 解析，表字段白名单）
//! - [`condition`] — 条件提醒引擎（M4.2 零 LLM：SQL 扫描 + 消息渲染）
//! - [`automation_plan`] — 自动化注册计划（M4.3/M4.4：workspace 自动化
//!   → 调度器注册数据，数据层与调度层解耦）

pub mod automation_plan;
pub mod builder;
pub mod condition;
pub mod migrate;
pub mod model;
pub mod registry;
pub mod service;
pub mod store;
pub mod validate;

pub use automation_plan::{AutomationPlan, frequency_to_rrule, plan_automations};
pub use builder::{BuildError, BuildResult, TableStatus, create_from_schema};
pub use condition::{ConditionError, ConditionHit, ConditionScanResult, render_message, run_condition_automation};
pub use migrate::{ColumnChange, MigrateError, MigrationReport, migrate_schema};
pub use service::{ServiceError, WorkspaceService};
pub use model::{
    Automation, DashboardWidget, Field, FieldType, Module, View, ViewType, WidgetType,
    WorkspaceSchema,
};
pub use registry::{RegistryEntry, RegistryStore, RegistryStoreError};
pub use store::{Filter, FilterOp, LocalSqliteStore, StoreError, WorkspaceStore};
pub use validate::{validate_schema, ValidationError};
