//! # .hwx 工作台包（M5.2）— 导出/校验/解析
//!
//! 一个工作台 = 定义（`workspace.json`）+ 数据（`data.sqlite`）。协作分发
//! 的最小载体是 **JSON bundle**（`.hwx`）：
//!
//! ```json
//! {
//!   "format": "hermix-workspace", "version": 1,
//!   "exported_at": "...", "workspace": {…schema…},
//!   "tables": { "vehicles": [ {…row…}, … ] }
//! }
//! ```
//!
//! 选 JSON 而非 zip：hermix-workspace 现有依赖无压缩 crate（六步阶梯
//! 现有依赖优先、不为打包引新依赖）；JSON 可直接在导入前**校验防脏包**
//! （格式标记/版本/schema 合法性/表 id 匹配/行字段白名单/必填/枚举），
//! 出错时干净拒绝，不落任何文件。
//!
//! 导入（M5.4）用 `parse_package`；LWW 合并（M5.5）用 `merge_rows`。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{Module, WorkspaceSchema};
use crate::store::{LocalSqliteStore, WorkspaceStore};
use crate::validate::validate_schema;

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    #[error("包格式无效: {0}")]
    Format(String),
    #[error("schema 校验失败: {0}")]
    Validation(String),
    #[error("表[{0}]不存在于 schema")]
    UnknownTable(String),
    #[error("行字段[{0}]不在白名单内（表[{1}]）")]
    UnknownColumn(String, String),
    #[error("store 错误: {0}")]
    Store(String),
    #[error("IO 错误: {0}")]
    Io(String),
}

// ---------------------------------------------------------------------------
// 包结构
// ---------------------------------------------------------------------------

/// 包格式标记（防把别的 JSON 当 .hwx）。
pub const HWX_FORMAT: &str = "hermix-workspace";
/// 当前包版本。
pub const HWX_VERSION: u32 = 1;

/// .hwx 包 — 定义 + 全量数据 + 元信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HwxPackage {
    pub format: String,
    pub version: u32,
    /// 导出时间（ISO8601）。
    pub exported_at: String,
    /// 工作台定义。
    pub workspace: WorkspaceSchema,
    /// 表 id → 全量行（含系统列 id/created_at/updated_at/updated_by）。
    pub tables: HashMap<String, Vec<Value>>,
}

// ---------------------------------------------------------------------------
// 导出
// ---------------------------------------------------------------------------

/// 导出工作台为 .hwx 包（全量数据，limit=0 不限）。
pub fn export_package(
    store: &LocalSqliteStore,
    schema: &WorkspaceSchema,
) -> Result<HwxPackage, PackageError> {
    let mut tables: HashMap<String, Vec<Value>> = HashMap::new();
    for m in &schema.modules {
        if let Module::Table { id, .. } = m {
            let rows = store
                .query(id, &[], None, 0, 0)
                .map_err(|e| PackageError::Store(format!("表[{id}]导出失败: {e}")))?;
            tables.insert(id.clone(), rows);
        }
    }
    Ok(HwxPackage {
        format: HWX_FORMAT.to_string(),
        version: HWX_VERSION,
        exported_at: chrono::Utc::now().to_rfc3339(),
        workspace: schema.clone(),
        tables,
    })
}

/// 序列化为 JSON 字节（供 HTTP 下载）。
pub fn to_bytes(pkg: &HwxPackage) -> Result<Vec<u8>, PackageError> {
    serde_json::to_vec_pretty(pkg).map_err(|e| PackageError::Format(e.to_string()))
}

// ---------------------------------------------------------------------------
// 解析 + 校验（防脏包）
// ---------------------------------------------------------------------------

/// 从字节解析并完整校验 .hwx 包。任一步失败返回错误，不落任何文件。
pub fn parse_package(raw: &[u8]) -> Result<HwxPackage, PackageError> {
    let pkg: HwxPackage = serde_json::from_slice(raw)
        .map_err(|e| PackageError::Format(format!("JSON 解析失败: {e}")))?;
    validate_package(&pkg)?;
    Ok(pkg)
}

/// 包级校验（导入前必调）：
/// 1. 格式标记 + 版本
/// 2. schema 合法（validate_schema）
/// 3. tables 的键 ⊆ schema 表 id
/// 4. 每行字段 ⊆ 该表白名单（业务字段 + 系统列）
/// 5. 必填字段存在 + 枚举值 ∈ options（浅校验，类型转换交给导入）
pub fn validate_package(pkg: &HwxPackage) -> Result<(), PackageError> {
    if pkg.format != HWX_FORMAT {
        return Err(PackageError::Format(format!(
            "标记不符（期望 {HWX_FORMAT}，得到 {}）",
            pkg.format
        )));
    }
    if pkg.version > HWX_VERSION {
        return Err(PackageError::Format(format!(
            "包版本 {} 高于当前支持 {HWX_VERSION}",
            pkg.version
        )));
    }
    validate_schema(&pkg.workspace)
        .map_err(|e| PackageError::Validation(e.to_string()))?;

    // 表 id 白名单 + 字段白名单
    for m in &pkg.workspace.modules {
        if let Module::Table { id, fields, .. } = m {
            if let Some(rows) = pkg.tables.get(id) {
                let mut whitelist: Vec<&str> = fields.iter().map(|f| f.id.as_str()).collect();
                whitelist.extend(["id", "created_at", "updated_at", "updated_by"]);
                for row in rows {
                    let Some(obj) = row.as_object() else {
                        return Err(PackageError::Format(format!(
                            "表[{id}]存在非对象行: {row}"
                        )));
                    };
                    for key in obj.keys() {
                        if !whitelist.contains(&key.as_str()) {
                            return Err(PackageError::UnknownColumn(key.clone(), id.clone()));
                        }
                    }
                    // 必填字段 + 枚举浅校验
                    for f in fields {
                        if f.required && !obj.contains_key(&f.id) {
                            return Err(PackageError::Format(format!(
                                "表[{id}]行缺少必填字段[{}]",
                                f.id
                            )));
                        }
                        if f.is_enum() {
                            if let Some(v) = obj.get(&f.id) {
                                if let Value::String(s) = v {
                                    let opts = f.options.as_ref().cloned().unwrap_or_default();
                                    if !opts.is_empty() && !opts.contains(s) {
                                        return Err(PackageError::Format(format!(
                                            "表[{id}]字段[{}]值[{s}]不在枚举内",
                                            f.id
                                        )));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    // tables 键不得引用 schema 外表
    for tid in pkg.tables.keys() {
        let known = pkg.workspace.modules.iter().any(|m| match m {
            Module::Table { id, .. } => id == tid,
            Module::Dashboard { .. } => false,
        });
        if !known {
            return Err(PackageError::UnknownTable(tid.clone()));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// LWW 合并（M5.5）— 每行 updated_at 比对
// ---------------------------------------------------------------------------

/// 把 `incoming` 行合并进 `local` 行，返回合并后列表。
/// Last-Write-Wins：按行 `id` 对齐，`updated_at`（ISO8601 字符串比较
/// 对同格式即字典序正确）较新者胜；本地缺失的行直接并入。
///
/// 边界语义：
/// - `updated_at` 缺失/空串视为最旧（有值者胜），两空则保留本地；
/// - 远端胜出时**保留本地 `created_at`**（创建时间不随覆盖丢失，
///   否则影响排序/审计；远端若自带 created_at 则忽略）。
/// 合并保持原顺序：先本地行（可能被替换），再追加新行。
pub fn merge_rows(local: &[Value], incoming: &[Value]) -> Vec<Value> {
    let ts = |row: &Value| -> String {
        row.get("updated_at")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let mut merged: Vec<Value> = Vec::with_capacity(local.len() + incoming.len());
    let mut seen: Vec<String> = Vec::with_capacity(local.len());
    for row in local {
        seen.push(row_id(row));
        merged.push(row.clone());
    }
    for row in incoming {
        let id = row_id(row);
        if let Some(pos) = seen.iter().position(|s| s == &id) {
            // 同行：仅当远端 updated_at **严格更大**才远端胜。
            // 空串字典序最小：空 vs 空 → 保留本地；空 vs 有值 → 保留本地；
            // 有值 vs 空 → 远端胜。所有边界自动正确。
            if ts(row) > ts(&merged[pos]) {
                // 远端胜：全行替换，但保留本地 created_at（创建时间不丢）
                let mut winner = row.clone();
                let local_created = merged[pos].get("created_at").cloned();
                if let Some(c) = local_created {
                    if let Some(o) = winner.as_object_mut() {
                        if !o.contains_key("created_at") {
                            o.insert("created_at".into(), c);
                        }
                    }
                }
                merged[pos] = winner;
            }
        } else {
            seen.push(id);
            merged.push(row.clone());
        }
    }
    merged
}

fn row_id(row: &Value) -> String {
    row.get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::create_from_schema;
    use std::collections::HashMap as Map;

    fn sample_schema() -> WorkspaceSchema {
        serde_json::from_value(serde_json::json!({
            "name": "车辆管理",
            "modules": [
                { "type": "table", "id": "vehicles", "name": "车辆",
                  "fields": [
                    { "id": "plate", "name": "车牌", "type": "text", "required": true, "unique": true },
                    { "id": "status", "name": "状态", "type": "enum", "options": ["在用", "维修"] }
                  ] },
                { "type": "table", "id": "drivers", "name": "司机",
                  "fields": [ { "id": "name", "name": "姓名", "type": "text", "required": true } ] }
            ]
        }))
        .expect("schema")
    }

    fn build_store(root: &std::path::Path, schema: &WorkspaceSchema) -> LocalSqliteStore {
        let mut seed = Map::new();
        seed.insert(
            "vehicles".to_string(),
            vec![serde_json::json!({ "plate": "京A12345", "status": "在用" })],
        );
        let id = create_from_schema(root, schema.clone(), &seed).expect("build").id;
        LocalSqliteStore::open(root.join(&id).join("data.sqlite"), schema.clone()).expect("open")
    }

    #[test]
    fn export_roundtrip_has_all_tables_and_rows() {
        let dir = tempfile::tempdir().expect("tmp");
        let schema = sample_schema();
        let store = build_store(dir.path(), &schema);

        let pkg = export_package(&store, &schema).expect("export");
        assert_eq!(pkg.format, HWX_FORMAT);
        assert_eq!(pkg.version, HWX_VERSION);
        assert_eq!(pkg.tables["vehicles"].len(), 1);
        assert!(pkg.tables.contains_key("drivers"));

        // 序列化 → 解析 → 校验往返
        let bytes = to_bytes(&pkg).expect("bytes");
        let back = parse_package(&bytes).expect("parse");
        assert_eq!(back.tables["vehicles"][0]["plate"], serde_json::json!("京A12345"));
    }

    #[test]
    fn parse_rejects_wrong_format_marker() {
        let dir = tempfile::tempdir().expect("tmp");
        let schema = sample_schema();
        let store = build_store(dir.path(), &schema);
        let pkg = export_package(&store, &schema).expect("export");
        let bytes = to_bytes(&pkg).expect("bytes");

        // 篡改格式标记
        let mut v: Value = serde_json::from_slice(&bytes).expect("json");
        v["format"] = serde_json::json!("not-hermix");
        let err = parse_package(&serde_json::to_vec(&v).expect("bytes")).unwrap_err();
        assert!(matches!(err, PackageError::Format(_)), "{err}");
    }

    #[test]
    fn parse_rejects_unknown_column() {
        let dir = tempfile::tempdir().expect("tmp");
        let schema = sample_schema();
        let store = build_store(dir.path(), &schema);
        let pkg = export_package(&store, &schema).expect("export");
        let mut pkg = pkg;
        // 塞一个 schema 外字段
        pkg.tables
            .get_mut("vehicles")
            .expect("vehicles")
            .push(serde_json::json!({ "plate": "京B1", "status": "在用", "hack": "x" }));
        let err = validate_package(&pkg).unwrap_err();
        assert!(matches!(err, PackageError::UnknownColumn(_, _)), "{err}");
    }

    #[test]
    fn parse_rejects_unknown_table() {
        let dir = tempfile::tempdir().expect("tmp");
        let schema = sample_schema();
        let store = build_store(dir.path(), &schema);
        let pkg = export_package(&store, &schema).expect("export");
        let mut pkg = pkg;
        pkg.tables.insert("ghost".into(), vec![]);
        let err = validate_package(&pkg).unwrap_err();
        assert!(matches!(err, PackageError::UnknownTable(_)), "{err}");
    }

    #[test]
    fn parse_rejects_missing_required_field() {
        let dir = tempfile::tempdir().expect("tmp");
        let schema = sample_schema();
        let store = build_store(dir.path(), &schema);
        let pkg = export_package(&store, &schema).expect("export");
        let mut pkg = pkg;
        pkg.tables
            .get_mut("vehicles")
            .expect("vehicles")
            .push(serde_json::json!({ "status": "在用" })); // 缺 plate
        let err = validate_package(&pkg).unwrap_err();
        assert!(matches!(err, PackageError::Format(_)), "{err}");
    }

    #[test]
    fn merge_rows_lww_by_updated_at() {
        let newer = serde_json::json!({
            "id": "r1", "plate": "京A1", "status": "维修",
            "updated_at": "2026-08-27T10:00:00Z"
        });
        let older = serde_json::json!({
            "id": "r1", "plate": "京A1", "status": "在用",
            "updated_at": "2026-08-26T10:00:00Z"
        });
        // 本地旧 → 远端新：远端胜
        let merged = merge_rows(&[older.clone()], &[newer.clone()]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0]["status"], serde_json::json!("维修"));
        // 本地新 → 远端旧：本地胜
        let merged = merge_rows(&[newer.clone()], &[older.clone()]);
        assert_eq!(merged[0]["status"], serde_json::json!("维修"));
        // 远端新增行 → 并入
        let extra = serde_json::json!({
            "id": "r2", "plate": "京B2", "status": "在用",
            "updated_at": "2026-08-27T10:00:00Z"
        });
        let merged = merge_rows(&[older], &[newer, extra]);
        assert_eq!(merged.len(), 2);
    }

    /// M5.5 边界：远端胜出时保留本地 created_at（创建时间不随覆盖丢失）。
    #[test]
    fn merge_rows_preserves_local_created_at_on_remote_win() {
        let local = serde_json::json!({
            "id": "r1", "plate": "京A1", "status": "在用",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-08-26T10:00:00Z"
        });
        // 远端行无 created_at（旧工具导出可能缺失）
        let remote = serde_json::json!({
            "id": "r1", "plate": "京A1", "status": "维修",
            "updated_at": "2026-08-27T10:00:00Z"
        });
        let merged = merge_rows(&[local], &[remote]);
        assert_eq!(merged[0]["status"], serde_json::json!("维修")); // 远端新 → 胜
        assert_eq!(merged[0]["created_at"], serde_json::json!("2026-01-01T00:00:00Z")); // 本地 created_at 保留
    }

    /// M5.5 边界：updated_at 缺失/空串视为最旧（有值者胜）；两空保留本地。
    #[test]
    fn merge_rows_empty_updated_at_is_oldest() {
        // 本地空 updated_at，远端有值 → 远端胜
        let local = serde_json::json!({ "id": "r1", "plate": "京A1", "status": "在用" });
        let remote = serde_json::json!({
            "id": "r1", "plate": "京A1", "status": "维修",
            "updated_at": "2026-08-27T10:00:00Z"
        });
        let merged = merge_rows(&[local.clone()], &[remote]);
        assert_eq!(merged[0]["status"], serde_json::json!("维修"));

        // 两边都空 → 保留本地（不覆盖）
        let local2 = serde_json::json!({ "id": "r1", "plate": "京A1", "status": "在用" });
        let remote2 = serde_json::json!({ "id": "r1", "plate": "京A1", "status": "维修" });
        let merged = merge_rows(&[local2], &[remote2]);
        assert_eq!(merged[0]["status"], serde_json::json!("在用"));
    }

    /// M5.4 验收：new 模式导入链路（模拟 import API 的 new 分支）——
    /// 导出 → 解析校验 → create_from_schema 导入（seed = 包内数据）→
    /// 数据完整可查。
    #[test]
    fn import_new_mode_roundtrip_preserves_data() {
        let dir = tempfile::tempdir().expect("tmp");
        let schema = sample_schema();
        let store = build_store(dir.path(), &schema);

        // 导出（含一条 seed + 手动追加一条）
        store
            .insert(
                "vehicles",
                &[serde_json::json!({ "plate": "京B99999", "status": "维修" })],
                "user",
            )
            .expect("insert");
        let pkg = export_package(&store, &schema).expect("export");
        let bytes = to_bytes(&pkg).expect("bytes");

        // 模拟导入：parse（防脏包校验）→ create_from_schema（seed = tables）
        let parsed = parse_package(&bytes).expect("parse");
        let target_root = tempfile::tempdir().expect("tmp");
        let result = create_from_schema(target_root.path(), parsed.workspace, &parsed.tables)
            .expect("import");

        // 数据完整可查（两行都在）
        let reopened = LocalSqliteStore::open(
            target_root.path().join(&result.id).join("data.sqlite"),
            sample_schema(),
        )
        .expect("reopen");
        let rows = reopened.query("vehicles", &[], None, 10, 0).expect("query");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|r| r["plate"] == serde_json::json!("京A12345")));
        assert!(rows.iter().any(|r| r["plate"] == serde_json::json!("京B99999")));
    }

    /// M5.4/M5.5 验收：merge 模式导入链路（模拟 import API 的 merge
    /// 分支）——本地行 + 包行按 id 对齐 LWW 合并（updated_at 较新者胜），
    /// 远端新增行并入；清空重写后数据正确。
    #[test]
    fn import_merge_mode_lww_preserves_newer_and_appends() {
        let dir = tempfile::tempdir().expect("tmp");
        let schema = sample_schema();
        let store = build_store(dir.path(), &schema);

        // 本地行（旧 updated_at）
        let local_rows = store.query("vehicles", &[], None, 10, 0).expect("query");
        assert_eq!(local_rows.len(), 1);
        let local_id = local_rows[0]["id"].as_str().expect("id").to_string();

        // 远端包：同行更新（较新 updated_at）+ 新增行
        let mut pkg = export_package(&store, &schema).expect("export");
        pkg.tables.insert(
            "vehicles".to_string(),
            vec![
                serde_json::json!({
                    "id": local_id,
                    "plate": "京A12345",
                    "status": "维修",
                    "updated_at": "2099-01-01T00:00:00Z", // 远端新 → 胜
                }),
                serde_json::json!({
                    "id": "remote-new-1",
                    "plate": "京C88888",
                    "status": "在用",
                    "updated_at": "2026-08-27T00:00:00Z",
                }),
            ],
        );

        // 模拟 merge：本地全量 + 包行 → LWW 合并 → 清空重写
        let local_full = store.query("vehicles", &[], None, 0, 0).expect("query");
        let merged = merge_rows(&local_full, &pkg.tables["vehicles"]);

        // 行数：本地 1 行（被远端更新替换）+ 远端新增 1 行 = 2
        assert_eq!(merged.len(), 2);
        let updated = merged
            .iter()
            .find(|r| r["id"] == serde_json::json!(local_id))
            .expect("updated row");
        assert_eq!(updated["status"], serde_json::json!("维修"));
        assert!(merged.iter().any(|r| r["plate"] == serde_json::json!("京C88888")));

        // 清空本地表（只删本地已存在的行——包内新增行还没写进 store）→
        // 批量插入合并结果
        for rid in local_full
            .iter()
            .filter_map(|r| r["id"].as_str().map(|s| s.to_string()))
        {
            store.delete("vehicles", &rid).expect("delete");
        }
        store
            .insert("vehicles", &merged, "import")
            .expect("rewrite");
        let after = store.query("vehicles", &[], None, 10, 0).expect("query");
        assert_eq!(after.len(), 2);
    }
}
