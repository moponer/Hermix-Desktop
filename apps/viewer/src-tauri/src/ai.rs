//! AI 对话：直连 OpenAI 兼容端点。
//!
//! 两种模式（见 `settings::AiSettings`）：
//! - **inherit（继承主程序）**：读**同一份** `config.yaml` + `.env`（`hermix_home`
//!   下），拿主程序的默认模型与供应商，端点直连它。不走本地 server——
//!   桌面端 sidecar 是 `--stdio`，**根本不绑 3000 端口**，viewer 自建 HTTP 面
//!   必然连不上（旧实现「AI 栏是摆设」的根因）。
//! - **custom（独立模型）**：自己一套 base_url / model / key（key 只在 .env）。
//!
//! 隐私与性能：两种模式都**只往用户自己配的端点发**，不经任何第三方中转；
//! 密钥只存在本机 .env；直连省掉一跳本地 server 往返，首字更快。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::settings::{self, ViewerSettings};

/// 解析出来的实际生效端点（给设置界面显示"现在到底在用什么"）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AiEndpointInfo {
    /// `主程序配置` / `独立模型`。
    pub source: String,
    pub provider: String,
    pub base_url: String,
    pub model: String,
    pub has_key: bool,
    /// 端点是否配置完整（缺 base_url / model / key 时为 false）。
    pub ready: bool,
    /// 配置不全时的原因（给用户看的中文说明）。
    pub problem: Option<String>,
}

/// 实际要用的端点。
pub struct ResolvedEndpoint {
    pub info: AiEndpointInfo,
    api_key: Option<String>,
}

/// 解析生效端点。inherit 模式读主程序配置；custom 模式读查看器设置。
pub fn resolve(settings: &ViewerSettings) -> Result<ResolvedEndpoint, String> {
    if settings.ai.mode == "custom" {
        let base_url = settings.ai.base_url.trim().trim_end_matches('/').to_string();
        let model = settings.ai.model.trim().to_string();
        let api_key = settings::read_env_value(settings::AI_KEY_ENV)
            .filter(|v| !v.trim().is_empty());
        let mut problem = None;
        if base_url.is_empty() {
            problem = Some("独立模型还没填端点地址（要 OpenAI 兼容的 base_url，含 /v1）".into());
        } else if model.is_empty() {
            problem = Some("独立模型还没填模型 id".into());
        } else if api_key.is_none() {
            problem = Some("独立模型还没配 API Key（写在 .env 的 HERMIX_VIEWER_AI_KEY）".into());
        }
        let ready = problem.is_none();
        return Ok(ResolvedEndpoint {
            info: AiEndpointInfo {
                source: "独立模型".into(),
                provider: "custom".into(),
                base_url,
                model,
                has_key: api_key.is_some(),
                ready,
                problem,
            },
            api_key,
        });
    }

    // —— inherit：继承主程序 ——
    hermix_config::loader::load_dotenv();
    let cfg = hermix_config::loader::load_config(None)
        .map_err(|e| format!("读取主程序配置失败: {e}"))?;

    let model_ref = cfg
        .model_selector
        .as_ref()
        .and_then(|s| s.default_model.clone())
        .or_else(|| cfg.model.clone());
    let (provider, model_from_ref) = match &model_ref {
        Some(m) => match m.split_once(':') {
            Some((p, m)) => (p.to_string(), Some(m.to_string())),
            None => (String::new(), Some(m.to_string())),
        },
        None => (String::new(), None),
    };

    let p = if provider.is_empty() {
        None
    } else {
        cfg.llm_providers.get(&provider)
    };
    // 内置免费渠道（custom-free-*）不在 llm_providers 里：它们的端点与
    // 凭据是编译期表（free_channel.rs），而且**不要** Authorization 头。
    let free = if p.is_none() && !provider.is_empty() {
        hermix_config::free_channel::FREE_CHANNELS
            .iter()
            .find(|c| c.id == provider)
    } else {
        None
    };
    let base_url = p
        .and_then(|p| p.base_url.clone())
        .or_else(|| free.map(|c| c.base_url.to_string()))
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .unwrap_or_default();
    let model = model_from_ref
        .or_else(|| p.and_then(|p| p.model.clone()))
        .unwrap_or_default();
    let api_key = match free {
        Some(c) => {
            if c.api_key == hermix_config::free_channel::KEY_NO_AUTH {
                None // 该端点不校验 Authorization：干脆不发
            } else {
                Some(c.api_key.to_string())
            }
        }
        None => resolve_provider_key(p.and_then(|p| p.api_key.as_deref()), &provider),
    };

    let mut problem = None;
    if provider.is_empty() {
        problem = Some("主程序还没配默认模型（config.yaml 的 model 或模型选择器）".into());
    } else if base_url.is_empty() {
        problem = Some(format!("主程序的供应商「{provider}」没有 base_url"));
    } else if model.is_empty() {
        problem = Some(format!("主程序的供应商「{provider}」没有模型 id"));
    } else if api_key.is_none() && free.is_none() {
        problem = Some(format!("主程序的供应商「{provider}」还没配 API Key"));
    }
    let ready = problem.is_none();
    Ok(ResolvedEndpoint {
        info: AiEndpointInfo {
            source: "主程序配置".into(),
            provider,
            base_url,
            model,
            has_key: api_key.is_some(),
            ready,
            problem,
        },
        api_key,
    })
}

/// 供应商密钥：先看配置里的字面量 / `${ENV}` 引用，再退回按供应商 id 找环境变量。
fn resolve_provider_key(raw: Option<&str>, provider: &str) -> Option<String> {
    if let Some(v) = raw.map(str::trim).filter(|v| !v.is_empty()) {
        if let Some(env) = v
            .strip_prefix("${")
            .and_then(|s| s.strip_suffix('}'))
            .filter(|s| !s.is_empty())
        {
            if let Ok(val) = std::env::var(env) {
                if !val.trim().is_empty() {
                    return Some(val);
                }
            }
            if let Some(val) = settings::read_env_value(env) {
                if !val.trim().is_empty() {
                    return Some(val);
                }
            }
        } else {
            return Some(v.to_string());
        }
    }
    if provider.is_empty() {
        return None;
    }
    hermix_config::loader::provider_api_key_from_env(provider).filter(|v| !v.trim().is_empty())
}

/// 一轮对话的输入。
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ChatTurn {
    /// `user` / `assistant` / `system`。
    pub role: String,
    pub text: String,
}

/// 问一轮（非流式）。返回模型的回复正文。
pub async fn chat(
    endpoint: &ResolvedEndpoint,
    system_prompt: &str,
    history: &[ChatTurn],
    user_text: &str,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
) -> Result<String, String> {
    if !endpoint.info.ready {
        return Err(endpoint
            .info
            .problem
            .clone()
            .unwrap_or_else(|| "AI 后端未配置".into()));
    }

    let mut messages: Vec<Value> = Vec::new();
    let sys = system_prompt.trim();
    if !sys.is_empty() {
        messages.push(json!({ "role": "system", "content": sys }));
    }
    for turn in history {
        let role = match turn.role.as_str() {
            "assistant" => "assistant",
            "system" => "system",
            _ => "user",
        };
        let text = turn.text.trim();
        if text.is_empty() {
            continue;
        }
        messages.push(json!({ "role": role, "content": text }));
    }
    messages.push(json!({ "role": "user", "content": user_text }));

    let mut body = json!({
        "model": endpoint.info.model,
        "messages": messages,
        "stream": false,
    });
    if let Some(t) = temperature {
        body["temperature"] = json!(t);
    }
    if let Some(m) = max_tokens {
        body["max_tokens"] = json!(m);
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|e| format!("HTTP client 构建失败: {e}"))?;

    let url = format!("{}/chat/completions", endpoint.info.base_url);
    let mut req = client.post(&url).json(&body);
    if let Some(key) = &endpoint.api_key {
        req = req.bearer_auth(key);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("连接 AI 端点失败（{}）: {e}", endpoint.info.base_url))?;
    let status = resp.status().as_u16();
    let raw: Value = resp.json().await.unwrap_or(Value::Null);
    if status != 200 {
        let detail = raw
            .get("error")
            .and_then(|v| v.get("message").and_then(|m| m.as_str()))
            .or_else(|| raw.get("error").and_then(|v| v.as_str()))
            .or_else(|| raw.get("message").and_then(|v| v.as_str()))
            .unwrap_or("(无详情)");
        return Err(match status {
            401 | 403 => format!("AI 端点拒绝了请求（{status}）：{detail}"),
            404 => format!("AI 端点路径不对（{status}）：{detail}"),
            429 => format!("AI 端点限流了（{status}）：{detail}"),
            _ => format!("AI 端点返回 {status}：{detail}"),
        });
    }
    let content = raw
        .get("choices")
        .and_then(|v| v.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            raw.get("choices")
                .and_then(|v| v.get(0))
                .and_then(|c| c.get("text"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("")
        .to_string();
    if content.trim().is_empty() {
        return Err("AI 端点返回了空回复".into());
    }
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_key_resolves_env_reference() {
        // "${ENV}" 形式要走环境变量；环境变量不存在时返回 None（不回落成字面量）。
        std::env::set_var("HERMIX_TEST_KEY", "sk-unit");
        assert_eq!(
            resolve_provider_key(Some("${HERMIX_TEST_KEY}"), "openai"),
            Some("sk-unit".into())
        );
        assert_eq!(resolve_provider_key(None, ""), None);
    }

    #[test]
    fn provider_key_uses_literal_when_not_env_ref() {
        assert_eq!(
            resolve_provider_key(Some("sk-literal"), "openai"),
            Some("sk-literal".into())
        );
    }
}
