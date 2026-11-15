//! 工作流：对话内把跑通的流程保存成可再跑一次的工作流。
//!
//! 两种作用域：`workspace = None` 为全局（不依赖某个项目，如深度调研）；
//! `Some(path)` 绑定一个项目目录。存储在 `<hermix_home>/workflows.json`，
//! 与 dashboard 的 `/api/workflows` 端点共用同一份文件——server 通过本
//! 模块的公开函数读写，进程内一把锁防并发撕裂；写盘走 tmp+rename 原子替换。
//!
//! 「再跑一次」不在这里：由 dashboard 的 run 端点走
//! `agent_service.send_message()` 执行（与对话同路径，铁律 DD），
//! 工具只负责保存 / 列出 / 删除。

use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use hermix_config::paths::hermix_home;
use hermix_core::traits::RiskLevel;
use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use crate::ToolRegistry;
use std::sync::Arc;

/// 一条已保存的工作流。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// None = 全局（对所有项目可见）；Some = 绑定的项目目录。
    #[serde(default)]
    pub workspace: Option<String>,
    /// 再跑一次时发给 agent 的完整指令。`{{参数}}` 占位符由运行端替换。
    pub prompt: String,
    pub created_at: u64,
    pub updated_at: u64,
    /// 最近一次运行状态："running" | "ok" | "failed"。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<u64>,
}

fn store_path() -> std::path::PathBuf {
    // 测试注入：hermix_home 是真实用户目录，测试绝不能碰。
    #[cfg(test)]
    if let Some(p) = test_store_override() {
        return p;
    }
    hermix_home().join("workflows.json")
}

#[cfg(test)]
static TEST_STORE: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();

#[cfg(test)]
fn test_store_override() -> Option<std::path::PathBuf> {
    TEST_STORE.get().cloned().flatten()
}

fn store_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 读全部工作流。文件缺失 / 损坏 = 空表（工作流是体验增强，不是正确性依赖）。
pub fn list_workflows() -> Vec<WorkflowEntry> {
    let _guard = store_lock().lock().unwrap_or_else(|p| p.into_inner());
    load_unlocked()
}

/// 新增或更新一条工作流。`id = None` 时新建；同名同作用域的条目会被
/// 更新而不是重复创建（对话里反复保存同一流程不会堆卡片）。
pub fn upsert_workflow(
    id: Option<&str>,
    name: &str,
    description: &str,
    workspace: Option<&str>,
    prompt: &str,
) -> Result<WorkflowEntry, String> {
    if name.trim().is_empty() || prompt.trim().is_empty() {
        return Err("name 和 prompt 不能为空".to_string());
    }
    let _guard = store_lock().lock().unwrap_or_else(|p| p.into_inner());
    let mut items = load_unlocked();
    let now = now_ms();

    // 先用不可变扫描定位下标，再取**唯一一次**可变借用。
    // 原写法 `id.map(..iter_mut..).flatten().or_else(..iter_mut..)` 会把第一个
    // `&mut` 一直活到 or_else 处，构成 E0499（重复可变借用）。
    // 语义保持不变：给了 id 就只按 id 匹配，没给才回落到「同名 + 同作用域」。
    let idx = match id {
        Some(i) => items.iter().position(|e| e.id == i),
        None => items
            .iter()
            .position(|e| e.name == name && e.workspace.as_deref() == workspace),
    };
    let existing = idx.map(|i| &mut items[i]);

    let entry = match existing {
        Some(e) => {
            e.name = name.to_string();
            e.description = description.to_string();
            e.workspace = workspace.map(str::to_string);
            e.prompt = prompt.to_string();
            e.updated_at = now;
            e.clone()
        }
        None => {
            let entry = WorkflowEntry {
                id: format!("wf-{}", now_ms()),
                name: name.to_string(),
                description: description.to_string(),
                workspace: workspace.map(str::to_string),
                prompt: prompt.to_string(),
                created_at: now,
                updated_at: now,
                last_status: None,
                last_run_at: None,
            };
            items.push(entry.clone());
            entry
        }
    };
    save_unlocked(&items)?;
    Ok(entry)
}

/// 按 id 删除。返回被删的条目；不存在时 Err。
pub fn remove_workflow(id: &str) -> Result<WorkflowEntry, String> {
    let _guard = store_lock().lock().unwrap_or_else(|p| p.into_inner());
    let mut items = load_unlocked();
    let Some(pos) = items.iter().position(|e| e.id == id) else {
        return Err(format!("没有 id 为 '{id}' 的工作流"));
    };
    let entry = items.remove(pos);
    save_unlocked(&items)?;
    Ok(entry)
}

/// 按 id 读取。
pub fn get_workflow(id: &str) -> Option<WorkflowEntry> {
    let _guard = store_lock().lock().unwrap_or_else(|p| p.into_inner());
    load_unlocked().into_iter().find(|e| e.id == id)
}

/// 更新最近一次运行状态（run 端点用）。
pub fn update_run_status(id: &str, status: &str) {
    let _guard = store_lock().lock().unwrap_or_else(|p| p.into_inner());
    let mut items = load_unlocked();
    if let Some(e) = items.iter_mut().find(|e| e.id == id) {
        e.last_status = Some(status.to_string());
        e.last_run_at = Some(now_ms());
        let _ = save_unlocked(&items);
    }
}

fn load_unlocked() -> Vec<WorkflowEntry> {
    std::fs::read_to_string(store_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_unlocked(items: &[WorkflowEntry]) -> Result<(), String> {
    let path = store_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{e}"))?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(items).map_err(|e| format!("序列化失败：{e}"))?;
    std::fs::write(&tmp, body).map_err(|e| format!("写入失败：{e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("替换失败：{e}"))
}

// ── 工具 ─────────────────────────────────────────────────

pub struct WorkflowHandler;

/// 必填字符串参数。
fn str_param<'a>(params: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ToolError::InvalidParams(format!("缺少 '{key}' 参数")))
}

#[async_trait]
impl ToolHandler for WorkflowHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidParams("缺少 'action' 参数".into()))?;
        match action {
            "save" => {
                let name = str_param(&params, "name")?;
                let prompt = str_param(&params, "prompt")?;
                let description = params
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let workspace = params
                    .get("workspace")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                let entry = upsert_workflow(None, name, &description, workspace, prompt)
                    .map_err(ToolError::ExecutionFailed)?;
                Ok(json!({
                    "saved": true,
                    "id": entry.id,
                    "name": entry.name,
                    "workspace": entry.workspace,
                    "hint": "已保存为工作流，可在 自动化 → 工作流 页面再跑一次。",
                })
                .to_string())
            }
            "list" => {
                let items = list_workflows();
                Ok(json!({ "count": items.len(), "workflows": items }).to_string())
            }
            "remove" => {
                let id = str_param(&params, "id")?;
                let entry = remove_workflow(id).map_err(ToolError::ExecutionFailed)?;
                Ok(json!({ "removed": true, "id": entry.id, "name": entry.name }).to_string())
            }
            other => Err(ToolError::InvalidParams(format!(
                "未知 action: '{other}'。可选 save / list / remove。"
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "description": "Action to perform",
                "enum": ["save", "list", "remove"]
            }),
        );
        props.insert(
            "name".into(),
            json!({
                "type": "string",
                "description": "工作流名称（save 时必填；同名同作用域会更新而不是重复创建）"
            }),
        );
        props.insert(
            "prompt".into(),
            json!({
                "type": "string",
                "description": "再跑一次时发给 agent 的完整指令。可包含 {{参数}} 占位符，运行时填入"
            }),
        );
        props.insert(
            "description".into(),
            json!({
                "type": "string",
                "description": "一句话描述这个工作流做什么（可选）"
            }),
        );
        props.insert(
            "workspace".into(),
            json!({
                "type": "string",
                "description": "绑定的工作区目录；不传 = 全局工作流（对所有项目可见）"
            }),
        );
        props.insert(
            "id".into(),
            json!({
                "type": "string",
                "description": "工作流 ID（remove 时必填）"
            }),
        );

        tool_schema(
            "workflow",
            "Save, list, or remove reusable workflows: 把跑通的流程存成带参数的可重跑工作流（保存后在 自动化 → 工作流 页面再跑一次）。",
            JsonSchema::object(props, vec!["action".into()]),
        )
    }
}

pub fn register(registry: &ToolRegistry) {
    let handler = Arc::new(WorkflowHandler);
    let schema = handler.schema();
    registry.register_with_risk(
        "workflow",
        "workflow",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        false,
        "Save, list, or remove reusable workflows: 把跑通的流程存成带参数的可重跑工作流。",
        "🔁",
        None,
        RiskLevel::Safe,
        false,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 整套往返放一个测试里：OnceLock 的注入点只能设一次。
    #[tokio::test]
    async fn save_list_upsert_remove_roundtrip() {
        let dir = std::env::temp_dir().join(format!("hermix-wf-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = TEST_STORE.set(Some(dir.join("workflows.json")));

        let entry = upsert_workflow(
            None,
            "深度调研",
            "不依赖项目的调研流程",
            None,
            "对 {{主题}} 做深度调研并输出报告",
        )
        .expect("save 应成功");
        assert_eq!(entry.name, "深度调研");

        let items = list_workflows();
        assert!(items.iter().any(|e| e.id == entry.id));

        // 同名同作用域再存 = 更新，不堆卡片。
        let again =
            upsert_workflow(None, "深度调研", "", None, "改版后的 prompt").expect("upsert 应成功");
        assert_eq!(again.id, entry.id);
        assert_eq!(
            list_workflows().iter().filter(|e| e.name == "深度调研").count(),
            1
        );

        let removed = remove_workflow(&entry.id).expect("remove 应成功");
        assert_eq!(removed.id, entry.id);
        assert!(remove_workflow(&entry.id).is_err(), "重复删除应报错");
    }
}
