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

/// 常见供应商的内置默认端点（OpenAI 兼容），与主程序
/// `hermix_agent::runtime_provider` 的默认表保持一致：主程序里这些 id
/// 不写 base_url 也能跑，viewer 若不认识同一张表，就会在主程序一切正常时
/// 报「供应商没有 base_url」—— inherit 模式下最高频的误报。
/// 刻意不含 anthropic / copilot：它们的端点不是裸 OpenAI 兼容
/// （/v1/messages、GitHub token 交换），补了也连不上，报错反而更诚实。
const BUILTIN_PROVIDER_BASE_URLS: &[(&str, &str)] = &[
    ("openai", "https://api.openai.com/v1"),
    ("qwen", "https://dashscope.aliyuncs.com/compatible-mode/v1"),
    ("qwen-oauth", "https://dashscope.aliyuncs.com/compatible-mode/v1"),
    ("kimi", "https://api.moonshot.cn/v1"),
    ("moonshot", "https://api.moonshot.cn/v1"),
    ("minimax", "https://api.minimax.chat/v1"),
    ("nous", "https://api.nousresearch.com/v1"),
];

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
    resolve_inherit(&cfg, settings.ai.selected_model.as_deref())
        .ok_or_else(|| "主程序还没配默认模型".to_string())
}

/// inherit 模式的纯核心（可测，不读全局配置）：手选模型（`selected_model`）
/// 优先于主程序默认——默认模型可能是凭据池/OAuth 认证（viewer 直连拿不到
/// key，表现为 401），用户选一个静态 key 就绪的即可用。`None` = 供应商
/// 不存在且没有默认可落。
fn resolve_inherit(
    cfg: &hermix_config::GatewayConfig,
    selected: Option<&str>,
) -> Option<ResolvedEndpoint> {
    // 手选命中 → 用它；手选指向不存在的供应商 → 落回默认。
    if let Some(sel) = selected {
        let (sp, sm) = split_model_ref(Some(sel));
        if !sp.is_empty() {
            if let Some(sm) = sm.as_deref() {
                if !sm.is_empty() {
                    if let Some(resolved) = resolve_provider_model(cfg, &sp, Some(sm)) {
                        return Some(resolved);
                    }
                }
            }
        }
    }

    let model_ref = cfg
        .model_selector
        .as_ref()
        .and_then(|s| s.default_model.clone())
        .or_else(|| cfg.model.clone());
    let (provider, model_from_ref) = split_model_ref(model_ref.as_deref());
    resolve_provider_model(cfg, &provider, model_from_ref.as_deref())
}

/// `"provider:model"` → `(provider, Some(model))`；裸 model → `("", Some(model))`。
fn split_model_ref(model_ref: Option<&str>) -> (String, Option<String>) {
    match model_ref {
        Some(m) => match m.split_once(':') {
            Some((p, m)) => (p.to_string(), Some(m.to_string())),
            None => (String::new(), Some(m.to_string())),
        },
        None => (String::new(), None),
    }
}

/// 解析主程序配置里的一个「供应商 + 模型」为可用端点。
/// 返回 `None` = 供应商不存在（llm_providers 与免费渠道都没有）。
/// 就绪与否看 `info.ready` / `info.problem`（缺 key / 缺 base_url 不是 None）。
fn resolve_provider_model(
    cfg: &hermix_config::GatewayConfig,
    provider: &str,
    model: Option<&str>,
) -> Option<ResolvedEndpoint> {
    if provider.is_empty() {
        return None;
    }
    let p = cfg.llm_providers.get(provider);
    // 内置免费渠道（custom-free-*）不在 llm_providers 里：它们的端点与
    // 凭据是编译期表（free_channel.rs），而且**不要** Authorization 头。
    let free = if p.is_none() {
        hermix_config::free_channel::FREE_CHANNELS
            .iter()
            .find(|c| c.id == provider)
    } else {
        None
    };
    if p.is_none() && free.is_none() {
        return None;
    }
    let base_url = p
        .and_then(|p| p.base_url.clone())
        .or_else(|| free.map(|c| c.base_url.to_string()))
        .or_else(|| builtin_base_url(provider).map(str::to_string))
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .unwrap_or_default();
    let model = model
        .map(str::to_string)
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
        None => resolve_provider_key(p.and_then(|p| p.api_key.as_deref()), provider),
    };

    let mut problem = None;
    if base_url.is_empty() {
        problem = Some(format!("主程序的供应商「{provider}」没有 base_url"));
    } else if model.is_empty() {
        problem = Some(format!("主程序的供应商「{provider}」没有模型 id"));
    } else if api_key.is_none() && free.is_none() {
        problem = Some(format!("主程序的供应商「{provider}」还没配 API Key"));
    }
    let ready = problem.is_none();
    Some(ResolvedEndpoint {
        info: AiEndpointInfo {
            source: "主程序配置".into(),
            provider: provider.to_string(),
            base_url,
            model,
            has_key: api_key.is_some(),
            ready,
            problem,
        },
        api_key,
    })
}

/// 内置默认端点查询：provider id 命中表项返回默认 base_url。
fn builtin_base_url(provider: &str) -> Option<&str> {
    BUILTIN_PROVIDER_BASE_URLS
        .iter()
        .find(|(id, _)| *id == provider)
        .map(|(_, url)| *url)
}

/// 查看器可选的一个模型项（**不含明文 key**——只报就绪态）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AiModelOption {
    /// `"provider:model"`，写回 settings.ai.selected_model 的值。
    pub id: String,
    /// 展示名（供应商显示名/免费渠道名 · 模型 id）。
    pub label: String,
    pub provider: String,
    pub model: String,
    pub has_key: bool,
    pub ready: bool,
    /// 不可用原因（就绪时为 null）。
    pub problem: Option<String>,
    /// 是否是主程序当前的默认模型（下拉里置顶标注）。
    pub is_default: bool,
}

/// 主程序里**用户启用过的模型清单**（model_selector.enabled）+
/// 各供应商自身的 model 兜底 + 免费渠道 seed 模型。
/// 供设置界面下拉：用户手选后写 `settings.ai.selected_model`，
/// `resolve()` 会优先用它——主程序默认模型走凭据池/OAuth 时
/// viewer 直连 401，选一个静态 key 就绪的即可用。
pub fn list_models() -> Result<Vec<AiModelOption>, String> {
    hermix_config::loader::load_dotenv();
    let cfg = hermix_config::loader::load_config(None)
        .map_err(|e| format!("读取主程序配置失败: {e}"))?;
    let default_ref = cfg
        .model_selector
        .as_ref()
        .and_then(|s| s.default_model.clone())
        .or_else(|| cfg.model.clone());

    let mut out: Vec<AiModelOption> = Vec::new();
    let mut push = |provider: &str, model: &str, display: String| {
        if provider.is_empty() || model.is_empty() {
            return;
        }
        let id = format!("{provider}:{model}");
        if out.iter().any(|o| o.id == id) {
            return;
        }
        let resolved = resolve_provider_model(&cfg, provider, Some(model));
        let (has_key, ready, problem, base_empty) = match &resolved {
            Some(r) => (r.info.has_key, r.info.ready, r.info.problem.clone(), r.info.base_url.is_empty()),
            None => (false, false, Some("供应商不存在".to_string()), true),
        };
        // 供应商存在但连 base_url 都没有——报出来让用户知道为什么不就绪。
        let problem = if base_empty && problem.is_none() {
            Some(format!("供应商「{provider}」没有 base_url"))
        } else {
            problem
        };
        out.push(AiModelOption {
            label: format!("{display} · {model}"),
            is_default: default_ref.as_deref() == Some(id.as_str()),
            id,
            provider: provider.to_string(),
            model: model.to_string(),
            has_key,
            ready,
            problem,
        });
    };

    // ① 用户在主程序模型切换器里启用过的（主程序列表的真实口径）。
    if let Some(sel) = &cfg.model_selector {
        for e in &sel.enabled {
            let display = cfg
                .llm_providers
                .get(&e.provider_id)
                .and_then(|p| p.display_name.clone())
                .unwrap_or_else(|| e.provider_id.clone());
            push(&e.provider_id, &e.model_id, display);
        }
    }
    // ② 各供应商自己的默认 model（配了 provider 但没进切换器的）。
    //    主程序允许一个供应商配**逗号分隔的 fallback 链**（免费渠道轮换），
    //    整串当模型 id 推出去会得到一个把全部候选拼在一起的坏选项，
    //    原生下拉的弹层还被它撑到横跨整窗——按链拆开逐个推，push 内去重。
    for (id, p) in &cfg.llm_providers {
        if let Some(m) = &p.model {
            let display = p.display_name.clone().unwrap_or_else(|| id.clone());
            for one in m.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                push(id, one, display.clone());
            }
        }
    }
    // ③ 免费渠道 seed 模型。
    for c in hermix_config::free_channel::FREE_CHANNELS {
        for (m, _) in c.seed_models {
            push(c.id, m, c.display_name.to_string());
        }
    }
    Ok(out)
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
/// OpenAI 兼容端点的完整 URL。
///
/// 绝大多数供应商的 `base_url` 写成 `https://api.xxx.com`（不带版本段），
/// 而真实路径是 `/v1/chat/completions`。此前一律拼 `/chat/completions`，
/// 于是「配了 key 却 404 / 对话失败」——这是最高频的一类误配。
/// 规则：末段已经是 `v1` / `v2` 之类版本号就直拼；已经写全了就不动；
/// 否则补 `/v1`。
fn completions_url(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    if base.ends_with("/chat/completions") || base.ends_with("/completions") {
        return base.to_string();
    }
    let last = base.rsplit('/').next().unwrap_or("");
    let mut cs = last.chars();
    let is_version = matches!(cs.next(), Some('v')) && last.len() > 1 && cs.all(|c| c.is_ascii_digit());
    if is_version {
        format!("{base}/chat/completions")
    } else {
        format!("{base}/v1/chat/completions")
    }
}

/// 发一次请求，返回（状态码, 原始响应文本）。
/// 刻意返回文本而不是直接 `json()`：解析失败时调用方要能把原文展示出来。
async fn send_once(
    client: &reqwest::Client,
    url: &str,
    body: &Value,
    api_key: Option<&str>,
) -> Result<(u16, String), String> {
    let mut req = client.post(url).json(body);
    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("连接 AI 端点失败（{url}）：{e}"))?;
    let status = resp.status().as_u16();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("读取 AI 响应失败（{url}）：{e}"))?;
    Ok((status, text))
}

/// 400 是否因为「不支持的参数」而非真的请求错误。
/// 只认明确的参数类关键字，避免把真正的错误请求误判成"方言不同"而重试。
fn looks_like_param_reject(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    ["temperature", "max_tokens", "max_completion_tokens", "unsupported", "unrecognized"]
        .iter()
        .any(|k| t.contains(k))
}

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

    let url = completions_url(&endpoint.info.base_url);
    let (status, text) = send_once(&client, &url, &body, endpoint.api_key.as_deref()).await?;

    // 400 兼容重试：o1/o3/deepseek-reasoner 这类推理模型不接受 temperature
    // 与 max_tokens（有的只认 max_completion_tokens），会把整个请求直接打回
    // 400。这不是配置错误，是同一件事的两种方言 —— 去掉这两个可选参数重发
    // 一次即可，用户不该为此去改配置。
    let (status, text) = if status == 400 && looks_like_param_reject(&text) {
        let mut relaxed = body.clone();
        if let Some(obj) = relaxed.as_object_mut() {
            obj.remove("temperature");
            obj.remove("max_tokens");
        }
        send_once(&client, &url, &relaxed, endpoint.api_key.as_deref()).await?
    } else {
        (status, text)
    };

    let raw: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if status != 200 {
        let detail = raw
            .get("error")
            .and_then(|v| v.get("message").and_then(|m| m.as_str()))
            .or_else(|| raw.get("error").and_then(|v| v.as_str()))
            .or_else(|| raw.get("message").and_then(|v| v.as_str()))
            .map(|s| s.to_string())
            // 解不出 JSON 时把原文前 300 字带上：最常见的是网关返回 HTML 错误页
            // 或 SSE 流，以前统一显示「(无详情)」，等于把唯一线索扔掉了。
            .unwrap_or_else(|| {
                let t = text.trim();
                if t.is_empty() { "(响应体为空)".to_string() }
                else { format!("{}…", t.chars().take(300).collect::<String>()) }
            });
        return Err(match status {
            // 免费渠道（kilo 等）的凭据是主程序登录态派生的 token，过期时
            // 端点回「Please sign in again」——报错带上指引，别让用户对着
            // 一句英文猜该去哪登录。
            401 | 403 => {
                let hint = if detail.contains("sign in")
                    || detail.contains("authentication")
                    || detail.contains("token")
                {
                    "。该模型的凭据可能已过期：免费渠道类模型请回主程序重新登录后重试，或在上方下拉改选其它模型"
                } else {
                    ""
                };
                format!("AI 端点拒绝了请求（{status}）：{detail}{hint}")
            }
            404 => format!("AI 端点路径不对（{url}）：{detail}"),
            429 => format!("AI 端点限流了（{status}）：{detail}"),
            _ => format!("AI 端点返回 {status}（{url}）：{detail}"),
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
        // 推理模型（deepseek-reasoner / o 系列）有时把正文放在 reasoning_content，
        // content 为空。直接报「空回复」会让用户以为模型没反应。
        let reasoning = raw
            .get("choices")
            .and_then(|v| v.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("reasoning_content"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if !reasoning.is_empty() {
            return Ok(reasoning);
        }
        // 空回复也把 finish_reason 带出来：length 表示被 max_tokens 截断，
        // content_filter 表示被安全策略拦了，二者处理方式完全不同。
        let finish = raw
            .get("choices")
            .and_then(|v| v.get(0))
            .and_then(|c| c.get("finish_reason"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        return Err(format!(
            "AI 端点返回了空回复（finish_reason={finish}，模型 {}）",
            endpoint.info.model
        ));
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

    /// 手选模型（selected_model）优先于主程序默认——401 消除路径的锚点。
    #[test]
    fn resolve_prefers_selected_model_over_default() {
        let mut cfg = hermix_config::GatewayConfig::default();
        cfg.llm_providers.insert(
            "good".to_string(),
            hermix_config::LlmProviderConfig {
                display_name: Some("好的".into()),
                api_key: Some("sk-ok".into()),
                base_url: Some("https://good.example.com/v1".into()),
                model: Some("good-model".into()),
                ..Default::default()
            },
        );
        let ep = resolve_inherit(&cfg, Some("good:good-model")).expect("resolve");
        assert_eq!(ep.info.provider, "good");
        assert_eq!(ep.info.model, "good-model");
        assert!(ep.info.ready, "ready={:?}", ep.info.problem);
    }

    /// selected_model 指向不存在的供应商 → 回落主程序默认，不报错。
    #[test]
    fn resolve_falls_back_when_selected_model_unknown() {
        let mut cfg = hermix_config::GatewayConfig::default();
        cfg.model = Some("openai:gpt-4o".into());
        cfg.llm_providers.insert(
            "openai".to_string(),
            hermix_config::LlmProviderConfig {
                api_key: Some("sk-x".into()),
                base_url: Some("https://api.openai.com/v1".into()),
                model: Some("gpt-4o".into()),
                ..Default::default()
            },
        );
        let ep = resolve_inherit(&cfg, Some("ghost:nope")).expect("resolve");
        assert_eq!(ep.info.provider, "openai", "应回落默认");
    }

    #[test]
    fn builtin_base_url_covers_main_program_defaults() {
        // 与主程序 runtime_provider 的默认端点一致；截图中报错的 minimax 必须命中。
        assert_eq!(
            builtin_base_url("minimax"),
            Some("https://api.minimax.chat/v1")
        );
        assert_eq!(builtin_base_url("kimi"), Some("https://api.moonshot.cn/v1"));
        assert_eq!(builtin_base_url("qwen").is_some(), true);
        // 未知供应商不瞎猜（宁可报「没有 base_url」也不打错端点）。
        assert_eq!(builtin_base_url("my-gateway"), None);
    }
}
