//! 成员（member）——总管方案的**执行单元**：独立长会话、可暂停恢复、
//! 对交付物负责。与子代理（`delegate_task` 数据通道，防上下文污染）是
//! 两个概念，互不复用通道，见
//! docs/notes/proposed/feature/2026-10-24-ali-chief-orchestrator.md D0/D2。
//!
//! - `roles`：成员角色文件（`~/.hermix/agents/*.md`）解析与内置种子。
//! - runner（P1 后续）：internal 成员的独立会话运行器。

pub mod roles;
pub mod runner;
