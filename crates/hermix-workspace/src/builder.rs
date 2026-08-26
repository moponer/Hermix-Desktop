//! # WorkspaceBuilder — 对话建设管线后端（M2.2）
//!
//! 用户确认 schema 后自动建设工作台（`docs/工作台架构设计方案.md` §7.5）：
//!
//! ```text
//! 校验 schema → 建目录 + workspace.json → 开 DB 建表 → 导入种子数据
//! → 自测（每表 count + 自动化条件试跑）→ 返回 BuildResult
//! ```
//!
//! 失败原子性：任一步失败返回 `BuildError`，调用方删除半成品目录
//! （`rollback` 在此 crate 提供；事务性由调用方保证）。
//!
//! 自测（§17.2 质量闭环）：建表后不依赖用户手动测试，先机器验证
//! 每表可查、自动化条件可编译——全过才报"建设完成"。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{Automation, Module, WorkspaceSchema};
use crate::store::{LocalSqliteStore, WorkspaceStore};
use crate::validate::validate_schema;

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("schema 校验失败: {0}")]
    Validation(String),
    #[error("IO 错误: {0}")]
    Io(String),
    #[error("数据导入失败: {0}")]
    Import(String),
    #[error("自测失败: {0}")]
    Smoke(String),
}

// ---------------------------------------------------------------------------
// 结果
// ---------------------------------------------------------------------------

/// 单个表的状态（自测摘要）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableStatus {
    pub id: String,
    pub name: String,
    /// 建表后导入的行数（不含系统自动数据）。
    pub rows: usize,
}

/// 建设结果（工具/API 返回给上层）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildResult {
    /// 工作台 id（= 目录名）。
    pub id: String,
    /// 数据目录（相对/绝对，由调用方决定语义）。
    pub dir: PathBuf,
    /// 各表状态。
    pub tables: Vec<TableStatus>,
    /// 自测摘要（每条通过项）。
    pub smoke: Vec<String>,
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// 按 schema 建设工作台。
///
/// - `root`：工作台根目录（`~/.hermix/workspaces/`），内部建 `<ws_id>/`。
/// - `seed_rows`：表 id → 初始行（可选；LLM 从导入文件抽样的数据）。
/// - 返回 `BuildResult`；失败时**已创建的目录由调用方清理**（本函数
///   保证失败时不留下已注册的 schema 文件之外的东西——见实现细节）。
pub fn create_from_schema(
    root: &Path,
    schema: WorkspaceSchema,
    seed_rows: &HashMap<String, Vec<Value>>,
) -> Result<BuildResult, BuildError> {
    // 1. 校验（防 LLM 坏 schema 写坏磁盘）
    validate_schema(&schema).map_err(|e| BuildError::Validation(e.to_string()))?;

    let id = uuid::Uuid::new_v4().to_string();
    let dir = root.join(&id);
    std::fs::create_dir_all(&dir).map_err(|e| BuildError::Io(format!("建目录失败: {e}")))?;

    // 2. 写定义文件
    let schema_path = dir.join("workspace.json");
    let raw = serde_json::to_vec_pretty(&schema)
        .map_err(|e| BuildError::Io(format!("序列化定义失败: {e}")))?;
    std::fs::write(&schema_path, raw)
        .map_err(|e| BuildError::Io(format!("写定义失败: {e}")))?;

    // 3. 开 DB 建表（LocalSqliteStore::open 内含 ensure_schema）
    let data_path = dir.join("data.sqlite");
    let store = LocalSqliteStore::open(&data_path, schema.clone())
        .map_err(|e| BuildError::Io(e.to_string()))?;

    // 4. 导入种子数据
    for m in &schema.modules {
        if let Module::Table { id, .. } = m {
            if let Some(rows) = seed_rows.get(id) {
                if !rows.is_empty() {
                    store
                        .insert(id, rows, "build")
                        .map_err(|e| BuildError::Import(format!("表[{id}]导入失败: {e}")))?;
                }
            }
        }
    }

    // 5. 自测：每表 count + 自动化条件试跑
    let mut smoke: Vec<String> = Vec::new();
    let mut tables: Vec<TableStatus> = Vec::new();
    for m in &schema.modules {
        if let Module::Table { id, name, .. } = m {
            let n = store
                .count(id, &[])
                .map_err(|e| BuildError::Smoke(format!("表[{id}]计数失败: {e}")))?;
            smoke.push(format!("表[{name}]可查（{n} 行）"));
            tables.push(TableStatus {
                id: id.clone(),
                name: name.clone(),
                rows: n as usize,
            });
        }
    }
    for a in &schema.automations {
        if let Automation::Condition { id: aid, table, condition, .. } = a {
            store
                .probe_condition(table, condition)
                .map_err(|e| BuildError::Smoke(format!("自动化[{aid}]条件编译失败: {e}")))?;
            smoke.push(format!("自动化[{aid}]条件可编译"));
        }
    }

    Ok(BuildResult { id, dir, tables, smoke })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vehicle_schema() -> WorkspaceSchema {
        serde_json::from_value(serde_json::json!({
            "name": "车辆管理",
            "modules": [
                { "type": "table", "id": "vehicles", "name": "车辆",
                  "fields": [
                    { "id": "plate", "name": "车牌", "type": "text", "required": true, "unique": true },
                    { "id": "status", "name": "状态", "type": "enum", "options": ["在用", "维修"] },
                    { "id": "inspection_date", "name": "年检", "type": "date" }
                  ] }
            ],
            "automations": [
                { "kind": "condition", "id": "a1", "name": "临期", "table": "vehicles",
                  "condition": "inspection_date <= date('now','+7 days')", "message": "{plate}",
                  "frequency": "daily" }
            ]
        }))
        .expect("schema")
    }

    #[test]
    fn build_creates_workspace_and_imports_rows() {
        let dir = tempfile::tempdir().expect("tmp");
        let mut seed = HashMap::new();
        seed.insert(
            "vehicles".to_string(),
            vec![
                serde_json::json!({ "plate": "京A12345", "status": "在用", "inspection_date": "2027-03-15" }),
                serde_json::json!({ "plate": "京B67890", "status": "维修" }),
            ],
        );

        let result = create_from_schema(dir.path(), vehicle_schema(), &seed).expect("build");

        // 目录结构齐全
        assert!(dir.path().join(&result.id).join("workspace.json").exists());
        assert!(dir.path().join(&result.id).join("data.sqlite").exists());

        // 表状态 + 行数
        assert_eq!(result.tables.len(), 1);
        assert_eq!(result.tables[0].rows, 2);

        // 自测通过（表可查 + 条件可编译）
        assert!(result.smoke.iter().any(|s| s.contains("表[车辆]可查")));
        assert!(result.smoke.iter().any(|s| s.contains("自动化[a1]条件可编译")));

        // 数据真实可查（用同一 schema 重新打开验证持久化）
        let reopened = LocalSqliteStore::open(
            dir.path().join(&result.id).join("data.sqlite"),
            vehicle_schema(),
        )
        .expect("reopen");
        let n = reopened.count("vehicles", &[]).expect("count");
        assert_eq!(n, 2);
    }

    #[test]
    fn invalid_schema_rejected_before_any_io() {
        let dir = tempfile::tempdir().expect("tmp");
        // Enum 无 options → 校验失败，不应建任何目录
        let bad: WorkspaceSchema = serde_json::from_value(serde_json::json!({
            "name": "bad",
            "modules": [{ "type": "table", "id": "a", "name": "A",
                          "fields": [{ "id": "st", "name": "状态", "type": "enum" }] }]
        }))
        .expect("schema");
        let err = create_from_schema(dir.path(), bad, &HashMap::new()).unwrap_err();
        assert!(matches!(err, BuildError::Validation(_)));
        // 目录应该是空的（没有创建任何 ws_id 子目录）
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(|e| e.ok())
            .collect();
        assert!(entries.is_empty(), "校验失败不应建目录");
    }

    #[test]
    fn seed_row_with_bad_enum_fails_import() {
        let dir = tempfile::tempdir().expect("tmp");
        let mut seed = HashMap::new();
        seed.insert(
            "vehicles".to_string(),
            vec![serde_json::json!({ "plate": "京A1", "status": "报废" })],
        );
        let err = create_from_schema(dir.path(), vehicle_schema(), &seed).unwrap_err();
        assert!(matches!(err, BuildError::Import(_)));
    }

    #[test]
    fn bad_automation_condition_fails_smoke() {
        let dir = tempfile::tempdir().expect("tmp");
        let schema: WorkspaceSchema = serde_json::from_value(serde_json::json!({
            "name": "t",
            "modules": [{ "type": "table", "id": "a", "name": "A",
                          "fields": [{ "id": "x", "name": "X", "type": "text" }] }],
            "automations": [
                { "kind": "condition", "id": "a1", "name": "r", "table": "a",
                  "condition": "nonexistent_col = 1", "message": "x", "frequency": "daily" }
            ]
        }))
        .expect("schema");
        let err = create_from_schema(dir.path(), schema, &HashMap::new()).unwrap_err();
        assert!(matches!(err, BuildError::Smoke(_)));
    }

    /// 手写样例 LLM 调用链（M2.6 验收）：
    /// 提案（schema v1）→ 建设（create_from_schema + CSV 抽样种子行）→
    /// 自测（smoke 摘要）→ 迭代（migrate 到 v2：加字段+加表）→ 数据保留。
    #[test]
    fn llm_chain_propose_build_smoke_migrate_preserves_data() {
        let dir = tempfile::tempdir().expect("tmp");

        // ── 1. 提案：LLM 生成 schema v1（车辆管理 + 年检临期提醒） ──
        let v1: WorkspaceSchema = serde_json::from_value(serde_json::json!({
            "name": "车辆管理工作台",
            "icon": "car",
            "modules": [
                { "type": "table", "id": "vehicles", "name": "车辆台账",
                  "fields": [
                    { "id": "plate", "name": "车牌号", "type": "text", "required": true, "unique": true },
                    { "id": "status", "name": "状态", "type": "enum", "options": ["在用", "维修", "停用"] },
                    { "id": "inspection_date", "name": "年检日期", "type": "date" }
                  ],
                  "views": [
                    { "id": "list", "name": "列表", "type": "grid", "fields": ["plate", "status", "inspection_date"] }
                  ] }
            ],
            "automations": [
                { "kind": "condition", "id": "a1", "name": "年检临期提醒", "table": "vehicles",
                  "condition": "inspection_date <= date('now','+7 days') and status != '停用'",
                  "message": "车辆 {plate} 年检将于 {inspection_date} 到期", "frequency": "daily" }
            ]
        }))
        .expect("v1");

        // ── 2. 建设：种子行 = 用户丢入 CSV 的抽样（完整数据后续录入） ──
        let mut seed = HashMap::new();
        seed.insert(
            "vehicles".to_string(),
            vec![
                serde_json::json!({ "plate": "京A12345", "status": "在用", "inspection_date": "2027-03-15" }),
                serde_json::json!({ "plate": "京B67890", "status": "维修" }),
            ],
        );
        let result = create_from_schema(dir.path(), v1.clone(), &seed).expect("build");

        // ── 3. 自测：smoke 摘要（表可查 + 自动化条件可编译） ──
        assert!(result.smoke.iter().any(|s| s.contains("表[车辆台账]可查")), "smoke: {:?}", result.smoke);
        assert!(result.smoke.iter().any(|s| s.contains("自动化[a1]条件可编译")), "smoke: {:?}", result.smoke);
        assert_eq!(result.tables[0].rows, 2);

        let db = dir.path().join(&result.id).join("data.sqlite");

        // ── 4. 迭代：用户对话"加里程字段 + 加司机表" → migrate 到 v2 ──
        let v2: WorkspaceSchema = serde_json::from_value(serde_json::json!({
            "name": "车辆管理工作台",
            "icon": "car",
            "modules": [
                { "type": "table", "id": "vehicles", "name": "车辆台账",
                  "fields": [
                    { "id": "plate", "name": "车牌号", "type": "text", "required": true, "unique": true },
                    { "id": "status", "name": "状态", "type": "enum", "options": ["在用", "维修", "停用"] },
                    { "id": "inspection_date", "name": "年检日期", "type": "date" },
                    { "id": "mileage", "name": "里程", "type": "number" }
                  ],
                  "views": [
                    { "id": "list", "name": "列表", "type": "grid", "fields": ["plate", "status", "inspection_date", "mileage"] }
                  ] },
                { "type": "table", "id": "drivers", "name": "司机台账",
                  "fields": [
                    { "id": "name", "name": "姓名", "type": "text", "required": true },
                    { "id": "phone", "name": "电话", "type": "text" }
                  ] }
            ],
            "automations": [
                { "kind": "condition", "id": "a1", "name": "年检临期提醒", "table": "vehicles",
                  "condition": "inspection_date <= date('now','+7 days') and status != '停用'",
                  "message": "车辆 {plate} 年检将于 {inspection_date} 到期", "frequency": "daily" }
            ]
        }))
        .expect("v2");

        let report = crate::migrate::migrate_schema(&db, &v1, &v2).expect("migrate");
        assert!(report.added_columns.iter().any(|c| c.column == "mileage"));
        assert!(report.added_tables.contains(&"drivers".to_string()));

        // ── 5. 数据保留：v2 打开存量 2 行仍在，新列可写、新表可插 ──
        let store = LocalSqliteStore::open(&db, v2).expect("open v2");
        let rows = store.query("vehicles", &[], None, 10, 0).expect("query");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["plate"], serde_json::json!("京A12345"));

        let vid = rows[0]["id"].as_str().expect("id").to_string();
        store
            .update("vehicles", &vid, &serde_json::json!({ "mileage": 12000 }), "user")
            .expect("update mileage");
        store
            .insert("drivers", &[serde_json::json!({ "name": "张三", "phone": "13800000000" })], "user")
            .expect("insert driver");

        // ── 6. 提醒规则在新 schema 下仍可编译（LLM 定时分析/条件提醒就绪） ──
        store.probe_condition("vehicles", "inspection_date <= date('now','+7 days') and status != '停用'")
            .expect("probe condition");
    }
}
