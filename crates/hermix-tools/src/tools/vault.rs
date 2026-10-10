//! Vault（凭据保险库）agent 工具。
//!
//! 决策笔记：docs/notes/implemented/feature/2026-10-09-vault-credentials.md
//! - `vault_list` — 列凭据名称/网址/用户名（**永不含密码**，结构保证）
//! - `vault_get`  — 按名称取一条（含明文密码；调用场景 = 用户向 agent 要凭据）
//!
//! 信任边界：vault 的门槛在「谁能跑 agent 的工具」——本地会话天然可信；
//! 手机/IM 通道是否放行 vault_get 由通道侧的设备配对/信任级判定（见
//! hermix-server mobile 面），工具层不做二次鉴权（无会话身份可判）。
//! RiskLevel 取 Safe：只读、不改系统状态；风险在数据面，由通道闸把守。

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_core::traits::RiskLevel;
use hermix_core::{JsonSchema, ToolError, ToolHandler, ToolSchema};

use hermix_vault::Vault;

use crate::ToolRegistry;

/// 共享 Vault 句柄（server 装配时注入；进程级单例在 vault_api 同源打开，
/// SQLite 同文件多连接只读安全，这里独立连接避免跨 crate 拿 server 状态）。
pub struct VaultListHandler {
    vault: Arc<Vault>,
}

impl VaultListHandler {
    pub fn new(vault: Arc<Vault>) -> Self {
        Self { vault }
    }
}

#[async_trait]
impl ToolHandler for VaultListHandler {
    async fn execute(&self, _params: Value) -> Result<String, ToolError> {
        let entries = self
            .vault
            .list()
            .map_err(|e| ToolError::ExecutionFailed(format!("vault 读取失败: {e}")))?;
        // Entry.secret 在 list 路径恒为 None，且序列化时 skip——双保险。
        Ok(serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".into()))
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "vault_list".into(),
            description: "列出凭据保险库中的所有条目（名称/网址/用户名）。出于安全设计，此工具永远不返回密码——需要密码时用 vault_get 按名称取单条。".into(),
            parameters: JsonSchema::object(IndexMap::new(), Vec::new()),
        }
    }
}

pub struct VaultGetHandler {
    vault: Arc<Vault>,
}

impl VaultGetHandler {
    pub fn new(vault: Arc<Vault>) -> Self {
        Self { vault }
    }
}

#[async_trait]
impl ToolHandler for VaultGetHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;
        let entry = self
            .vault
            .get_by_name(name)
            .map_err(|e| ToolError::ExecutionFailed(format!("vault 读取失败: {e}")))?;
        // get_by_name 不存在时返回 VaultError::NotFound（已在 map_err 转
        // ExecutionFailed），这里直接序列化 Entry。
        Ok(serde_json::json!({
            "name": entry.name,
            "url": entry.url,
            "username": entry.username,
            "password": entry.secret,
            "notes": entry.notes,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "name".into(),
            json!({
                "type": "string",
                "description": "凭据条目名称（大小写不敏感），如 \"GitHub\""
            }),
        );
        ToolSchema {
            name: "vault_get".into(),
            description: "按名称从凭据保险库取一条凭据（含密码）。仅在用户明确要求提供该凭据时使用；不要把密码写入日志或无关输出。".into(),
            parameters: JsonSchema::object(props, vec!["name".into()]),
        }
    }
}

/// 注册（Safe：只读；数据面信任边界在设备配对层）。
pub fn register(registry: &ToolRegistry, vault: Arc<Vault>) {
    let list = VaultListHandler::new(vault.clone());
    registry.register_with_risk(
        "vault_list",
        "vault",
        list.schema(),
        Arc::new(list),
        Arc::new(|| true),
        Vec::new(),
        false,
        "列出凭据保险库条目（名称/网址/用户名，永不含密码）",
        "🔐",
        None,
        RiskLevel::Safe,
        false,
    );
    let get = VaultGetHandler::new(vault);
    registry.register_with_risk(
        "vault_get",
        "vault",
        get.schema(),
        Arc::new(get),
        Arc::new(|| true),
        Vec::new(),
        false,
        "按名称从凭据保险库取一条凭据（含密码；仅用户明确要求时用）",
        "🔑",
        None,
        RiskLevel::Safe,
        false,
    );
}
