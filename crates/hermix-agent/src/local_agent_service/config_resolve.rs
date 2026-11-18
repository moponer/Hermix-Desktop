//! 每轮启动前的配置解析：workspace / overrides / thinking 指令 / permission。
//!
//! 从 `local_agent_service` 搬出的第四批（P1-13）。本来都是 `impl` 上的关联
//! 函数，从不碰 `&self` —— 放在组合根里只让那个文件更难读。
//!
//! 一处易错点：`apply_overrides` 不写 `TERMINAL_CWD` 环境变量。多线程 async
//! 里 `set_var` 是数据竞争（两个会话不同 workspace 会互相覆盖），改由调用方
//! 通过 `ToolRegistry::set_terminal_workspace` 传入。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use hermix_core::traits::{AgentOverrides, PermissionLevel};
use hermix_intelligence::DomainMode;

use crate::agent_loop::AgentConfig;
use crate::smart_model_routing::ApiMode;

/// Resolve a workspace path. Returns `None` for empty input, missing
/// directories, or any failure. Uses literal pass-through (no canonicalize)
/// to preserve the exact path the user selected — following file.rs's
/// philosophy that the user's configured path is the source of truth.
pub(super) fn resolve_workspace(workspace: Option<&str>) -> Option<PathBuf> {
    let raw = workspace?.trim();
    if raw.is_empty() {
        return None;
    }
    let p = Path::new(raw);
    if p.is_dir() {
        Some(p.to_path_buf())
    } else {
        tracing::warn!(
            workspace = %raw,
            "workspace path is not a directory; ignoring"
        );
        None
    }
}

/// Apply [`AgentOverrides`] to a base [`AgentConfig`] and produce the
/// per-turn tool whitelist. Workspace canonicalization happens here so
/// downstream code can rely on a real absolute path or `None`.
pub(super) fn apply_overrides(
    base: AgentConfig,
    overrides: &AgentOverrides,
) -> (AgentConfig, Option<HashSet<String>>) {
    let mut config = base;
    if let Some(prompt) = overrides.system_prompt.as_ref() {
        config.system_prompt = Some(prompt.clone());
    }
    config.workspace = resolve_workspace(overrides.workspace.as_deref())
        .map(|p| p.to_string_lossy().to_string());
    // NOTE: TERMINAL_CWD is set via ToolRegistry::set_terminal_workspace
    // in the caller (send_message / send_message_stream), NOT via
    // std::env::set_var. The env::set_var path was removed because it is
    // unsafe in multi-threaded async contexts (data race on concurrent
    // sessions with different workspaces).
    config.permission = overrides.permission;
    // Propagate a group-chat room-scoped memory directory (if any) so
    // the agent reads/writes under its own room instead of the shared
    // global `<hermix_home>/memories`. Private chats resolve their own
    // session-scoped directory in `send_message` / `send_message_stream`
    // and ignore this field.
    if let Some(md) = overrides.memory_dir.as_ref() {
        config.memory_dir = Some(md.clone());
    }
    // Workbench binding: gates the `workbench.context` direction and
    // pins the `workbench` skill every turn (module design §13 layer 3 —
    // the session exists only to serve this workbench, so the skill is
    // resident with zero intent detection). Ordinary chats: both None.
    config.workbench_id = overrides.workbench_id.clone();
    if overrides.workbench_id.is_some() && !config.ephemeral_skills.iter().any(|s| s == "workbench") {
        config.ephemeral_skills.push("workbench".to_string());
    }
    // L1 domain-mode pin: `"auto"` / empty / `None` → auto-detect;
    // otherwise parse the wire string into `DomainMode`. Unknown values
    // fall back to auto — never panic on a malformed frontend payload.
    config.domain_override = overrides
        .domain_override
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("auto"))
        .and_then(DomainMode::from_str);
    // Per-turn thinking on/off switch. Merge into `extra_body` so the
    // provider's existing `extra_body` → top-level body expansion
    // (provider.rs:952-958, streaming:1027-1034) carries it to the LLM
    // request unchanged — same pattern `reasoning_effort` uses.
    // `None` (old clients / field omitted) → no injection, behavior
    // identical to before this field existed.
    if let Some(enabled) = overrides.enable_thinking {
        let mut eb = config.extra_body.unwrap_or(serde_json::json!({}));
        if let serde_json::Value::Object(ref mut map) = eb {
            // 统一开关 → 按 provider 翻译成正确的请求字段（见
            // module 级 `inject_thinking_directive`）。写死
            // `enable_thinking` 只对国内原生厂商有效，对 OpenRouter
            // （reasoning 对象）/ Anthropic（thinking）/ OpenAI
            // （reasoning_effort）无效——这正是实测"开关没用"的根因。
            inject_thinking_directive(
                map,
                &config.api_mode,
                &config.model,
                enabled,
            );
        }
        config.extra_body = Some(eb);
    }
    let allowed = overrides
        .tools
        .as_ref()
        .map(|names| names.iter().cloned().collect::<HashSet<_>>());
    (config, allowed)
}

/// 把统一的"深度思考 开/关"开关翻译成各 provider 认识的请求字段。
///
/// 控制思考的参数名因通道而异，写死 `enable_thinking` 只对国内原生厂商
/// （火山/豆包/直连 DeepSeek 等）有效；OpenRouter 走 `reasoning` 对象、
/// Anthropic 走 `thinking`、OpenAI o-series 走 `reasoning_effort`。不翻译的话
/// 开关对绝大多数经 OpenRouter 中转的模型毫无作用（用户实测"没啥用"）。
pub(super) fn inject_thinking_directive(
body: &mut serde_json::Map<String, serde_json::Value>,
api_mode: &ApiMode,
model: &str,
enabled: bool,
) {
// 先清掉上一次（含其它代码路径）可能残留的任一字段，保证开关状态干净切换、不串台。
body.remove("enable_thinking");
body.remove("reasoning");
body.remove("reasoning_effort");
body.remove("thinking");

if !enabled {
    // 仅 OpenRouter 需要显式关闭才有意义；其它通道省略该字段即默认关闭。
    if looks_like_openrouter(model) {
        body.insert("reasoning".to_string(), serde_json::json!({ "enabled": false }));
    }
    return;
}

match api_mode {
    ApiMode::AnthropicMessages => {
        // Anthropic extended thinking：必须带 budget_tokens，且小于 max_tokens。
        body.insert(
            "thinking".to_string(),
            serde_json::json!({ "type": "enabled", "budget_tokens": 8000 }),
        );
    }
    ApiMode::ChatCompletions | ApiMode::CodexResponses => {
        if looks_like_openrouter(model) {
            body.insert(
                "reasoning".to_string(),
                serde_json::json!({ "enabled": true }),
            );
        } else if model.to_ascii_lowercase().starts_with("gpt")
            || model.starts_with("o1")
            || model.starts_with("o3")
            || model.starts_with("o4")
        {
            // 直连 OpenAI o-series：reasoning_effort。
            body.insert("reasoning_effort".to_string(), serde_json::json!("high"));
        } else {
            // 国内原生厂商（火山/豆包/直连 DeepSeek 等）仍用 enable_thinking。
            body.insert("enable_thinking".to_string(), serde_json::json!(true));
        }
    }
}
}

/// 是否经 OpenRouter 或兼容中转。OpenRouter 的模型 id 形如 `provider/model-name`
/// （带 `/`），直连 OpenAI 的 id 无 `/`；`anthropic/` 前缀排除——Anthropic
/// 直连走 `thinking`，不走 reasoning 对象。
pub(super) fn looks_like_openrouter(model: &str) -> bool {
if model.to_ascii_lowercase().starts_with("anthropic/") {
    return false;
}
model.contains('/')
}

/// Default [`PermissionLevel`] used when the caller doesn't specify one.
pub(super) fn default_permission() -> PermissionLevel {
    PermissionLevel::Default
}
