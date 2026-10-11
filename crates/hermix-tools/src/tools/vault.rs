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


// ── 导入(CSV → vault.add,加密由 crate 内部处理,工具层只喂参数)──────

const KNOWN_NAME: &[&str] = &["name", "名称", "平台", "平台名", "系统", "标题", "网站", "站点"];
const KNOWN_USER: &[&str] = &["username", "user", "用户名", "账号", "账户", "登录名"];
const KNOWN_PASS: &[&str] = &["password", "pass", "pwd", "密码", "密码短语"];
const KNOWN_URL: &[&str] = &["url", "uri", "网址", "地址", "链接", "登录地址"];
const KNOWN_NOTES: &[&str] = &["notes", "note", "备注", "说明", "注释"];

/// CSV 字节解码:UTF-8 BOM → utf8;合法 utf8 → utf8;否则按 GB18030
/// (国内密码管理器/Excel 导出的常见编码,GBK 的超集)。
fn decode_csv_bytes(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(&bytes[3..]).into_owned();
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    let (cow, _enc, had_errors) = encoding_rs::GB18030.decode(bytes);
    if had_errors {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    cow.into_owned()
}

fn map_col(headers: &[String], explicit: Option<&str>, known: &[&str]) -> Option<usize> {
    if let Some(name) = explicit {
        let name = name.trim();
        if let Ok(idx) = name.parse::<usize>() {
            return Some(idx);
        }
        return headers
            .iter()
            .position(|h| h.trim().eq_ignore_ascii_case(name));
    }
    headers
        .iter()
        .position(|h| known.iter().any(|k| k.eq_ignore_ascii_case(h.trim())))
}

struct VaultImportHandler {
    vault: Arc<Vault>,
}

impl VaultImportHandler {
    fn new(vault: Arc<Vault>) -> Self {
        Self { vault }
    }
}

#[async_trait::async_trait]
impl ToolHandler for VaultImportHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let path = params
            .get("csv_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'csv_path' parameter".into()))?;
        let bytes = std::fs::read(path)
            .map_err(|e| ToolError::ExecutionFailed(format!("读取 CSV 失败: {e}")))?;
        let text = decode_csv_bytes(&bytes);
        let has_header = params
            .get("has_header")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let col = |key: &str| {
            params
                .get(key)
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };

        let mut rdr = csv::ReaderBuilder::new()
            .has_headers(false)
            .flexible(true)
            .from_reader(text.as_bytes());
        let rows: Vec<Vec<String>> = rdr
            .records()
            .map(|r| {
                r.map(|rec| rec.iter().map(|f| f.to_string()).collect::<Vec<String>>())
            })
            .collect::<Result<Vec<Vec<String>>, _>>()
            .map_err(|e| ToolError::ExecutionFailed(format!("CSV 解析失败: {e}")))?;
        if rows.is_empty() {
            return Err(ToolError::ExecutionFailed("CSV 为空".into()));
        }

        let (headers, data): (Vec<String>, &[Vec<String>]) = if has_header {
            (rows[0].clone(), &rows[1..])
        } else {
            (Vec::new(), &rows[..])
        };

        let ci_name = map_col(&headers, col("name_col").as_deref(), KNOWN_NAME)
            .ok_or_else(|| ToolError::ExecutionFailed(format!(
                "找不到名称列:表头 = {headers:?}。可用 name_col 参数显式指定列名"
            )))?;
        let ci_pass = map_col(&headers, col("password_col").as_deref(), KNOWN_PASS)
            .ok_or_else(|| ToolError::ExecutionFailed(format!(
                "找不到密码列:表头 = {headers:?}"
            )))?;
        let ci_user = map_col(&headers, col("username_col").as_deref(), KNOWN_USER);
        let ci_url = map_col(&headers, col("url_col").as_deref(), KNOWN_URL);
        let ci_notes = map_col(&headers, col("notes_col").as_deref(), KNOWN_NOTES);

        let mut imported = 0usize;
        let mut skipped = 0usize;
        let mut failed: Vec<Value> = Vec::new();
        for (i, row) in data.iter().enumerate() {
            let cell = |idx: Option<usize>| -> String {
                idx.and_then(|k| row.get(k))
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default()
            };
            let name = cell(Some(ci_name));
            let secret = cell(Some(ci_pass));
            if name.is_empty() && secret.is_empty() {
                continue; // 空行
            }
            let line = if has_header { i + 2 } else { i + 1 };
            if name.is_empty() {
                failed.push(serde_json::json!({"line": line, "error": "名称为空"}));
                continue;
            }
            if secret.is_empty() {
                failed.push(serde_json::json!({"line": line, "error": "密码为空", "name": name}));
                continue;
            }
            if self.vault.get_by_name(&name).is_ok() {
                skipped += 1; // 同名跳过:导入不覆写既有凭据
                continue;
            }
            match self.vault.add(&name, &cell(ci_url), &cell(ci_user), &secret, &cell(ci_notes)) {
                Ok(_) => imported += 1,
                Err(e) => failed.push(
                    serde_json::json!({"line": line, "name": name, "error": e.to_string()}),
                ),
            }
        }

        Ok(serde_json::json!({
            "imported": imported,
            "skipped_existing": skipped,
            "failed": failed,
            "total_rows": data.len(),
            "columns": {"name": ci_name, "username": ci_user, "password": ci_pass, "url": ci_url, "notes": ci_notes},
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = indexmap::IndexMap::new();
        props.insert(
            "csv_path".to_string(),
            serde_json::json!({"type": "string", "description": "CSV 文件的绝对路径（支持 GBK/GB18030 与 UTF-8 自动探测）"}),
        );
        props.insert(
            "name_col".to_string(),
            serde_json::json!({"type": "string", "description": "名称列的列名（表头自动匹配失败时显式指定）"}),
        );
        props.insert(
            "username_col".to_string(),
            serde_json::json!({"type": "string", "description": "用户名列名（可选）"}),
        );
        props.insert(
            "password_col".to_string(),
            serde_json::json!({"type": "string", "description": "密码列名（可选）"}),
        );
        props.insert(
            "url_col".to_string(),
            serde_json::json!({"type": "string", "description": "网址列名（可选）"}),
        );
        props.insert(
            "notes_col".to_string(),
            serde_json::json!({"type": "string", "description": "备注列名（可选）"}),
        );
        ToolSchema {
            name: "vault_import".into(),
            description: "把 CSV 凭据清单导入保险库（编码自动探测 GB18030/UTF-8,表头自动匹配,同名条目                          跳过不覆写,密码经 vault 加密落库）。导入后用 vault_list 确认。".into(),
            parameters: JsonSchema {
                schema_type: Some("object".into()),
                properties: Some(props),
                required: Some(vec!["csv_path".into()]),
                additional_properties: None,
            },
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
    let get = VaultGetHandler::new(vault.clone());
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

    let imp = VaultImportHandler::new(vault);
    registry.register_with_risk(
        "vault_import",
        "vault",
        imp.schema(),
        Arc::new(imp),
        Arc::new(|| true),
        Vec::new(),
        false,
        "从 CSV 导入凭据（编码自动探测,同名跳过,加密落库）",
        "📥",
        None,
        RiskLevel::Safe,
        false,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gbk(s: &str) -> Vec<u8> {
        let (cow, _, _) = encoding_rs::GB18030.encode(s);
        cow.into_owned()
    }

    #[tokio::test]
    async fn import_gbk_csv_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("vault-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let vault = std::sync::Arc::new(Vault::open_at(&tmp.join("v.db")).unwrap());
        let handler = VaultImportHandler::new(vault.clone());

        // GBK 编码的 CSV(模拟国内导出),含 BOM 无、密码带逗号引号
        let csv = "名称,用户名,密码,网址,备注\n\
                   GitHub,demo,\"p@ss,word\",https://github.com,测试备注\n\
                   知乎,zhihu-user,\"密,码\"\"引号\",https://zhihu.com,中文备注\n";
        let csv_path = tmp.join("cred.csv");
        std::fs::write(&csv_path, gbk(csv)).unwrap();

        let out = handler
            .execute(serde_json::json!({"csv_path": csv_path.to_string_lossy()}))
            .await
            .unwrap();
        let r: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(r["imported"], 2, "imported: {out}");
        assert_eq!(r["skipped_existing"], 0);

        // 解密回环:名字是 GBK 写入的,取回应还原
        let e = vault.get_by_name("GitHub").unwrap();
        assert_eq!(e.secret.as_deref(), Some("p@ss,word"));
        let e2 = vault.get_by_name("知乎").unwrap();
        assert_eq!(e2.username, "zhihu-user");
        assert_eq!(e2.secret.as_deref(), Some("密,码\"引号"));

        // 幂等:重导全跳过
        let out2 = handler
            .execute(serde_json::json!({"csv_path": csv_path.to_string_lossy()}))
            .await
            .unwrap();
        let r2: serde_json::Value = serde_json::from_str(&out2).unwrap();
        assert_eq!(r2["imported"], 0);
        assert_eq!(r2["skipped_existing"], 2);
    }

    #[tokio::test]
    async fn import_reports_missing_columns() {
        let tmp = std::env::temp_dir().join(format!("vault-test2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let vault = std::sync::Arc::new(Vault::open_at(&tmp.join("v.db")).unwrap());
        let handler = VaultImportHandler::new(vault);
        let csv_path = tmp.join("bad.csv");
        std::fs::write(&csv_path, "foo,bar\n1,2\n").unwrap();
        let err = handler
            .execute(serde_json::json!({"csv_path": csv_path.to_string_lossy()}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("找不到名称列"), "{err}");
    }
}
