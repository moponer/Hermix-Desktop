//! `list_dir` — 非递归单目录列举（名称 / 类型 / 字节大小）。
//!
//! 背景：系统提示（directions.rs）、terminal 的拒绝文案、result_budget
//! 与 tool_state 都引用 `list_dir`，但注册表里从未有过这个工具——模型照
//! 提示调用只会得到 Unknown tool，随后回退 `terminal`（"该用内置工具却
//! 抄 shell"的主要诱因之一）。本工具补齐这个洞：单层列举 + 每项大小，
//! 正好也是子代理"读前先看大小"纪律需要的形态。

use std::path::Path;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};
use std::sync::Arc;

use hermix_core::{
    tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema,
    traits::RiskLevel,
};

use crate::ToolRegistry;

/// 构造侧条目硬闸（出口另有 3K 字符预算截断，这里是第一道）。
const MAX_ENTRIES: usize = 500;

pub struct ListDirHandler;

#[async_trait]
impl ToolHandler for ListDirHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let path = params.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let dir = Path::new(path);
        let mut rd = tokio::fs::read_dir(dir).await.map_err(|e| {
            ToolError::from_message(format!("cannot list '{}': {e}", dir.display()))
        })?;

        let mut entries: Vec<Value> = Vec::new();
        let mut truncated = false;
        while let Some(entry) = rd.next_entry().await.map_err(|e| {
            ToolError::from_message(format!("readdir '{}': {e}", dir.display()))
        })? {
            if entries.len() >= MAX_ENTRIES {
                truncated = true;
                break;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let meta = entry.metadata().await.ok();
            let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            let size = if is_dir {
                Value::Null
            } else {
                meta.map(|m| json!(m.len())).unwrap_or(Value::Null)
            };
            entries.push(json!({
                "name": name,
                "type": if is_dir { "dir" } else { "file" },
                "size": size,
            }));
        }
        entries.sort_by(|a, b| {
            a["name"]
                .as_str()
                .unwrap_or("")
                .cmp(b["name"].as_str().unwrap_or(""))
        });

        Ok(json!({
            "path": dir.display().to_string(),
            "total": entries.len(),
            "truncated": truncated,
            "entries": entries,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "path".into(),
            json!({
                "type": "string",
                "description": "Directory path to list (one level, non-recursive)"
            }),
        );
        tool_schema(
            "list_dir",
            "List the immediate children of ONE directory (non-recursive): name, type, byte size. \
             For recursive name/content search use `search_files` / `list_files`.",
            JsonSchema::object(props, vec![]),
        )
    }
}

pub fn register(registry: &ToolRegistry) {
    registry.register_with_risk(
        "list_dir",
        "file",
        ListDirHandler.schema(),
        Arc::new(ListDirHandler),
        Arc::new(|| true),
        vec![],
        true,
        "List the immediate children of one directory (non-recursive): name, type, byte size. \
         For recursive search use `search_files` / `list_files`.",
        "📂",
        None,
        RiskLevel::Safe,
        /* override_existing = */ false,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lists_one_level_with_sizes() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "hello").unwrap();
        std::fs::create_dir(tmp.path().join("sub")).unwrap();

        let out = ListDirHandler
            .execute(json!({ "path": tmp.path().to_str().unwrap() }))
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let entries = v["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["name"], "a.txt");
        assert_eq!(entries[0]["type"], "file");
        assert_eq!(entries[0]["size"], 5);
        assert_eq!(entries[1]["type"], "dir");
        assert!(entries[1]["size"].is_null());
        assert_eq!(v["truncated"], false);
    }

    #[tokio::test]
    async fn missing_dir_is_an_error_not_a_panic() {
        let out = ListDirHandler
            .execute(json!({ "path": "Z:/definitely/not/here" }))
            .await;
        assert!(out.is_err());
    }
}
