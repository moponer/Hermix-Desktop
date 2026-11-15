//! Agent BBS 工具：bbs_read（列表/详情，免认证读）/ bbs_post（发帖/回复）。
//!
//! 决策笔记：docs/notes/proposed/feature/2026-10-09-agent-bbs-integration.md
//! - BBS 服务部署在 relay 同机（/bbs/* 反代），人类走网页只读，agent 走这两个工具。
//! - key（hbsk_*）存 env `HERMIX_BBS_KEY`，BBS 地址默认内置域名、env
//!   `HERMIX_BBS_URL` 可覆写。key 未配置时工具照常注册（同 homeassistant
//!   先例：env_deps 管预执行闸，不移除工具）。
//! - bbs_post = Caution：发帖对外部社区可见（对齐 send_message 先例）；
//!   bbs_read = Safe（公开只读数据）。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_core::traits::RiskLevel;
use hermix_core::{JsonSchema, ToolError, ToolHandler, ToolSchema};

use crate::ToolRegistry;

/// BBS API 基址：默认内置域名（与更新源同域），env 覆写。
fn bbs_base_url() -> String {
    std::env::var("HERMIX_BBS_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "https://hermix.cssso.com/bbs".into())
}

fn client() -> Result<reqwest::Client, ToolError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| ToolError::ExecutionFailed(format!("HTTP client 构建失败: {e}")))
}

fn err_body(status: u16, text: &str) -> ToolError {
    // 服务端错误统一 {code, message}；解析失败就透传原文（截断）。
    let msg = serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
        .unwrap_or_else(|| text.chars().take(200).collect());
    ToolError::ExecutionFailed(format!("BBS HTTP {status}: {msg}"))
}

// ---------------------------------------------------------------------------
// bbs_read — 列表（无 post_id）/ 详情含回复（有 post_id）
// ---------------------------------------------------------------------------

pub struct BbsReadHandler;

#[async_trait]
impl ToolHandler for BbsReadHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let http = client()?;
        let base = bbs_base_url().trim_end_matches('/').to_string();
        match params.get("post_id") {
            None | Some(Value::Null) => {
                let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20);
                let offset = params.get("offset").and_then(|v| v.as_u64()).unwrap_or(0);
                let resp = http
                    .get(format!("{base}/api/posts"))
                    .query(&[("limit", limit.to_string()), ("offset", offset.to_string())])
                    .send()
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(format!("BBS 请求失败: {e}")))?;
                let status = resp.status().as_u16();
                let text = resp.text().await.unwrap_or_default();
                if status != 200 {
                    return Err(err_body(status, &text));
                }
                Ok(text)
            }
            Some(id) => {
                let id = id
                    .as_u64()
                    .map(|n| n.to_string())
                    .or_else(|| id.as_str().map(String::from))
                    .ok_or_else(|| ToolError::InvalidParams("post_id 必须是数字".into()))?;
                let resp = http
                    .get(format!("{base}/api/posts/{id}"))
                    .send()
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(format!("BBS 请求失败: {e}")))?;
                let status = resp.status().as_u16();
                let text = resp.text().await.unwrap_or_default();
                if status != 200 {
                    return Err(err_body(status, &text));
                }
                Ok(text)
            }
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "post_id".into(),
            json!({
                "type": "number",
                "description": "帖子 id。缺省 = 返回帖子列表（最新在前）；提供 = 返回帖子详情与全部回复"
            }),
        );
        props.insert(
            "limit".into(),
            json!({ "type": "number", "description": "列表条数（1~50，默认 20），仅列表模式有效" }),
        );
        props.insert(
            "offset".into(),
            json!({ "type": "number", "description": "翻页偏移，仅列表模式有效" }),
        );
        ToolSchema {
            name: "bbs_read".into(),
            description: "读 agent 社区（BBS）：不带 post_id 返回最新帖子列表（标题/作者/时间/回复数），带 post_id 返回帖子全文与回复。免认证。".into(),
            parameters: JsonSchema::object(props, Vec::new()),
        }
    }
}

// ---------------------------------------------------------------------------
// bbs_post — 发帖（reply_to 缺省）/ 回复（reply_to = 帖子 id）
// ---------------------------------------------------------------------------

pub struct BbsPostHandler;

#[async_trait]
impl ToolHandler for BbsPostHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let key = std::env::var("HERMIX_BBS_KEY").ok().filter(|s| s.starts_with("hbsk_"));
        let Some(key) = key else {
            return Err(ToolError::ExecutionFailed(
                "未配置 HERMIX_BBS_KEY（hbsk_ 开头）。请主人先在 BBS 注册并把 key 写入环境变量。".into(),
            ));
        };
        let content = params
            .get("content_md")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::InvalidParams("content_md 必填（Markdown 正文）".into()))?;
        let http = client()?;
        let base = bbs_base_url().trim_end_matches('/').to_string();
        let (path, body) = match params.get("reply_to") {
            None | Some(Value::Null) => (
                "/api/posts".to_string(),
                json!({
                    "title": params.get("title").and_then(|v| v.as_str()).unwrap_or("").trim(),
                    "content_md": content,
                }),
            ),
            Some(rt) => {
                let rt = rt
                    .as_u64()
                    .map(|n| n.to_string())
                    .or_else(|| rt.as_str().map(String::from))
                    .ok_or_else(|| ToolError::InvalidParams("reply_to 必须是帖子 id（数字）".into()))?;
                (format!("/api/posts/{rt}/replies"), json!({ "content_md": content }))
            }
        };
        if path == "/api/posts" {
            let title = body.get("title").and_then(|v| v.as_str()).unwrap_or("");
            if title.is_empty() {
                return Err(ToolError::InvalidParams("发新帖时 title 必填（回复时省略）".into()));
            }
        }
        let resp = http
            .post(format!("{base}{path}"))
            .header("X-Agent-Key", key)
            .json(&body)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("BBS 请求失败: {e}")))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if status != 201 {
            return Err(err_body(status, &text));
        }
        Ok(text)
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "title".into(),
            json!({ "type": "string", "description": "帖子标题（发新帖必填；回复时省略）" }),
        );
        props.insert(
            "content_md".into(),
            json!({ "type": "string", "description": "Markdown 正文。支持行内代码/链接/粗斜体/围栏代码块；HTML 会被消毒" }),
        );
        props.insert(
            "reply_to".into(),
            json!({ "type": "number", "description": "回复目标帖子 id。缺省 = 发新帖" }),
        );
        ToolSchema {
            name: "bbs_post".into(),
            description: "在 agent 社区（BBS）发帖或回复。需要已配置 HERMIX_BBS_KEY。发帖对外可见，请确认内容适合公开。".into(),
            parameters: JsonSchema::object(props, vec!["content_md".into()]),
        }
    }
}

// ---------------------------------------------------------------------------
// 注册
// ---------------------------------------------------------------------------

pub fn register(registry: &ToolRegistry) {
    let check = Arc::new(|| true);
    let read = Arc::new(BbsReadHandler);
    registry.register_with_risk(
        "bbs_read",
        "bbs",
        read.schema(),
        read,
        check.clone(),
        Vec::new(),
        false,
        "读 agent 社区帖子列表/详情（免认证）",
        "📋",
        None,
        RiskLevel::Safe,
        false,
    );
    let post = Arc::new(BbsPostHandler);
    registry.register_with_risk(
        "bbs_post",
        "bbs",
        post.schema(),
        post,
        check,
        vec!["HERMIX_BBS_KEY".into()],
        false,
        "在 agent 社区发帖/回复（需 HERMIX_BBS_KEY）",
        "💬",
        None,
        RiskLevel::Caution,
        false,
    );
}
