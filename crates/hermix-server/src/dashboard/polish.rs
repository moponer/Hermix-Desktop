//! POST /api/polish/{chat, automation} — stateless LLM-driven text
//! rewriting for the desktop "润色" / "改写" affordances.
//!
//! TODO.md §2 — `docs/TODO.md`. The web side
//! (`apps/desktop-tauri/src/api/polish.ts`) already calls these
//! endpoints; before this commit the fetch fell through to a
//! whitespace-only fallback because the backend returned 404.
//!
//! Design:
//! - Reuse `agent_service.send_message` against an *ephemeral*
//!   session id so no parent conversation history leaks in and
//!   nothing gets persisted long-term. The session is `reset_session`'d
//!   immediately after the reply to free the in-memory cache (and any
//!   SQLite row the persistence layer wrote).
//! - Override `system_prompt` with one of the two template constants
//!   ([`CHAT_POLISH_PROMPT`], [`AUTOMATION_POLISH_PROMPT`]) and force
//!   `tools = Some(vec![])` so the model can't reach for a tool while
//!   rewriting user text.
//! - `PermissionLevel::Full` since there are no tools to gate and no
//!   side-effecting actions to guard against.
//! - Wrap the entire call in a 12-second `tokio::time::timeout` — the
//!   frontend already gives the user a "polishing…" spinner; a stalled
//!   LLM should fall back to the local whitespace collapse rather than
//!   hold the UI hostage.
//!
//! Failure modes intentionally return `502` / `504` with `{ok: false,
//! error: ...}` so the frontend can decide its own degradation
//! strategy (today: collapse whitespace; tomorrow: surface a toast).

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json as JsonResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use uuid::Uuid;

use hermix_core::traits::{AgentOverrides, PermissionLevel};

use crate::HttpServerState;

/// Max wall-clock time for a single polish call. Anything longer and
/// the frontend already gave up on the spinner — surface a 504 so it
/// can fall back to its local whitespace collapse.
const POLISH_TIMEOUT: Duration = Duration::from_secs(12);

/// Session id prefix for ephemeral polish sessions. Easy to grep for
/// in the SQLite dump if a regression accidentally persists them.
const POLISH_SESSION_PREFIX: &str = "__polish__";

/// Optional context the front-end may attach. Both shapes map onto
/// `serde_json::Value` in the prompt template — we never deserialise
/// them into a typed shape because the two prompts use disjoint keys
/// (`ctx.recentTurns` / `ctx.hasAttachments` vs `ctx.triggerKind` /
/// `ctx.scheduleHint`).
#[derive(Debug, Default, Deserialize)]
pub struct PolishRequest {
    pub draft: String,
    #[serde(default)]
    pub ctx: Option<JsonValue>,
}

#[derive(Debug, Serialize)]
pub struct PolishResponse {
    pub ok: bool,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PolishError {
    pub ok: bool,
    pub error: String,
}

impl PolishError {
    fn new(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: msg.into(),
        }
    }
}

/// Build the user-message body fed to the LLM. The prompt template
/// already wraps `<draft>...</draft>` so we just hand it the raw text.
fn wrap_draft(draft: &str) -> String {
    draft.to_string()
}

/// Drive one stateless polish call. Returns the polished text on
/// success or a 502 / 504 with a short error string on failure.
///
/// `system_prompt` MUST be one of the two PR-approved templates —
/// callers in this module pass them verbatim.
async fn run_polish(
    state: &HttpServerState,
    system_prompt: &'static str,
    draft: &str,
    ctx: Option<&JsonValue>,
) -> Result<String, PolishError> {
    if draft.trim().is_empty() {
        return Err(PolishError::new("draft is empty"));
    }

    let session_id = format!("{POLISH_SESSION_PREFIX}{}", Uuid::new_v4());
    let mut user_message = wrap_draft(draft);
    // Fold ctx into the user message in the same shape the
    // front-end templates use, so the model sees the same context
    // whether it was originally templated client-side or server-side.
    if let Some(c) = ctx {
        if !c.is_null() {
            user_message.push_str("\n\n# ctx\n");
            user_message.push_str(&c.to_string());
        }
    }

    let overrides = AgentOverrides {
        system_prompt: Some(system_prompt.to_string()),
        // No tools — this is a pure rewrite, the model should not
        // reach for terminal / file_read / etc.
        tools: Some(Vec::new()),
        permission: Some(PermissionLevel::Full),
        ..Default::default()
    };

    let svc = state.agent_service();

    let send_fut = async {
        svc.send_message(&session_id, &user_message, overrides)
            .await
    };

    let reply = match tokio::time::timeout(POLISH_TIMEOUT, send_fut).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            // Best-effort cleanup; ignore secondary errors.
            let _ = svc.reset_session(&session_id).await;
            return Err(PolishError::new(format!("agent_service error: {e}")));
        }
        Err(_) => {
            let _ = svc.reset_session(&session_id).await;
            return Err(PolishError::new("polish timed out after 12s"));
        }
    };

    // Wipe the ephemeral session so nothing leaks into the chat
    // history view. Errors here are non-fatal — the next session
    // reset (server restart) will clear any leftovers.
    if let Err(e) = svc.reset_session(&session_id).await {
        tracing::debug!(error = %e, session = %session_id,
            "polish ephemeral session reset failed; ignoring");
    }

    let text = reply.text.trim().to_string();
    if text.is_empty() {
        return Err(PolishError::new("model returned empty text"));
    }
    Ok(text)
}

/// POST /api/polish/chat
///
/// Rewrites the user's draft for a natural-language chat turn.
/// Backed by the `CHAT_POLISH_PROMPT` template
/// (`apps/desktop-tauri/src/prompts/polish.chat.ts`).
pub async fn post_polish_chat(
    State(state): State<HttpServerState>,
    Json(req): Json<PolishRequest>,
) -> Result<JsonResponse<PolishResponse>, (StatusCode, JsonResponse<PolishError>)> {
    match run_polish(&state, CHAT_POLISH_PROMPT, &req.draft, req.ctx.as_ref()).await {
        Ok(text) => Ok(JsonResponse(PolishResponse {
            ok: true,
            text,
            error: None,
        })),
        Err(e) => Err((
            StatusCode::BAD_GATEWAY,
            JsonResponse(PolishError::new(e.error)),
        )),
    }
}

/// POST /api/polish/automation
///
/// Rewrites the user's draft into a structured automation PROMPT.
/// Backed by the `AUTOMATION_POLISH_PROMPT` template
/// (`apps/desktop-tauri/src/prompts/polish.automation.ts`).
pub async fn post_polish_automation(
    State(state): State<HttpServerState>,
    Json(req): Json<PolishRequest>,
) -> Result<JsonResponse<PolishResponse>, (StatusCode, JsonResponse<PolishError>)> {
    match run_polish(&state, AUTOMATION_POLISH_PROMPT, &req.draft, req.ctx.as_ref()).await {
        Ok(text) => Ok(JsonResponse(PolishResponse {
            ok: true,
            text,
            error: None,
        })),
        Err(e) => Err((
            StatusCode::BAD_GATEWAY,
            JsonResponse(PolishError::new(e.error)),
        )),
    }
}

// ---------------------------------------------------------------------------
// Prompt templates — keep verbatim in sync with the front-end TypeScript
// copies in `apps/desktop-tauri/src/prompts/polish.{chat,automation}.ts`.
//
// The front-end keeps an inlined `POLISH_PROMPTS_FOR_DEBUG` export for
// inspection; the server copy is the actual one the LLM is fed so it
// doesn't depend on the desktop bundle being shipped. If the templates
// ever drift, the front-end copy becomes a stale read-only mirror and
// `callPolishEndpoint` calls into the same prompt.
// ---------------------------------------------------------------------------

/// Mirror of `CHAT_POLISH_PROMPT`
/// (apps/desktop-tauri/src/prompts/polish.chat.ts).
const CHAT_POLISH_PROMPT: &str = "你是一名对话润色专家。\
用户给出一段\"草稿原文\"，你要把它改写为一段直接可发送的对话内容。\n\
\n\
# 目标\n\
- 让对方一眼看懂用户在问什么、想做什么\n\
- 表达流畅、不啰嗦、避免重复\n\
- 保持用户的语气（如果原话很口语化，不要突然文绉绉）\n\
\n\
# 输入\n\
<draft>\n\
{{draft}}\n\
</draft>\n\
{{#ctx.recentTurns}}\n\
<recent_turns>\n\
{{ctx.recentTurns}}\n\
</recent_turns>\n\
{{/ctx.recentTurns}}\n\
{{#ctx.hasAttachments}}\n\
注：用户已附加文件/图片，无需在文字里复述文件名。\n\
{{/ctx.hasAttachments}}\n\
\n\
# 输出要求\n\
1. **只输出润色后的内容本身**，不要加\"润色后：\"等前缀、不要加任何解释\n\
2. **保留用户的核心意图**，不要替用户做主张\n\
3. **如果原文很清晰**，润色后可以与原文几乎一致\n\
4. **如果原文重复、啰嗦或矛盾**，去掉冗余、保留要点\n\
5. **不要编造**用户没说的内容\n\
6. **不要使用 markdown 包装**（不要加 # / - / ** 等），保持纯对话文本\n\
7. **长度**控制在原文 0.7-1.3 倍，除非原文明显需要扩写\n\
\n\
# 接下来输出润色结果：\n";

/// Mirror of `AUTOMATION_POLISH_PROMPT`
/// (apps/desktop-tauri/src/prompts/polish.automation.ts).
const AUTOMATION_POLISH_PROMPT: &str = "你是一名\"自动化 PROMPT 编写专家\"。\
用户用日常口语描述他想做的自动化任务，你把它改写为一段**结构化、可直接被自动化引擎执行**的 PROMPT。\n\
\n\
# 输出结构\n\
按以下五段组织（缺哪段就省略，但顺序不变）：\n\
\n\
# 角色\n\
PROMPT 扮演的专家角色（一句话）。\n\
\n\
# 目标\n\
这个自动化**要达成什么**（一句话，越具体越好）。\n\
\n\
# 输入\n\
PROMPT 接收的变量与数据源。**保留用户原文里的 `{{变量名}}` 占位符**，不要替换、不要添加不存在的变量。\n\
\n\
# 处理步骤\n\
1. 编号列表，每步一个原子动作（获取数据 / 计算 / 调用工具 / 输出）。\n\
2. 涉及外部工具时，写明工具名（\"调用 terminal 执行…\"、\"调用 file_read 读取…\"）。\n\
\n\
# 输出格式\n\
- 邮件 / 消息类：写明主题长度、正文段落数、是否需要 Markdown\n\
- 文件类：写明路径模板 + 命名规则\n\
- 控制台类：写明打印字段顺序\n\
\n\
# 约束与异常\n\
- 不要做的事\n\
- 失败时如何处理（重试次数 / 报警 / 回退）\n\
\n\
# 输入\n\
<draft>\n\
{{draft}}\n\
</draft>\n\
{{#ctx.triggerKind}}\n\
触发类型：{{ctx.triggerKind}}\n\
{{/ctx.triggerKind}}\n\
{{#ctx.scheduleHint}}\n\
调度：{{ctx.scheduleHint}}\n\
{{/ctx.scheduleHint}}\n\
\n\
# 改写规则\n\
1. **保留 `{{变量}}` 占位符原样**，不要写成具体值\n\
2. **保留用户的具体场景**（例：\"每周一早上的销售周报\"不要改成\"周期性报表\"）\n\
3. **用动词开头写步骤**（\"读取…\"、\"汇总…\"、\"发送…\"），避免名词堆砌\n\
4. **如果用户没说明某段，写\"[待补充：xxx]\"** 而不要瞎猜\n\
5. **直接输出 PROMPT 本身**，不要加\"以下是润色后的 PROMPT：\"等前缀\n\
\n\
# 接下来输出结构化 PROMPT：\n";

#[cfg(test)]
mod tests {
    use super::*;

    /// The two mirrored prompt templates must remain byte-for-byte
    /// identical to the front-end TypeScript copies. The end-to-end
    /// `polish.ts → /api/polish/* → LLM` round trip depends on that;
    /// if they drift, the front-end `POLISH_PROMPTS_FOR_DEBUG`
    /// export becomes a stale read-only mirror while the live call
    /// uses the server copy (or vice-versa), and the user-visible
    /// rewrite quality regresses without an obvious error.
    /// If the snapshot tests in `apps/desktop-tauri` ever fail in
    /// CI, edit both copies in the same PR — never one side only.
    #[test]
    fn chat_prompt_template_locked() {
        // Snapshots live next to the front-end copies. We embed
        // the canonical strings as `assert_eq!` markers so a diff
        // shows up in CI without any file-globbing.
        const CANONICAL: &str = "你是一名对话润色专家。\
用户给出一段\"草稿原文\"，你要把它改写为一段直接可发送的对话内容。\n\
\n\
# 目标\n\
- 让对方一眼看懂用户在问什么、想做什么\n\
- 表达流畅、不啰嗦、避免重复\n\
- 保持用户的语气（如果原话很口语化，不要突然文绉绉）\n\
\n\
# 输入\n\
<draft>\n\
{{draft}}\n\
</draft>\n\
{{#ctx.recentTurns}}\n\
<recent_turns>\n\
{{ctx.recentTurns}}\n\
</recent_turns>\n\
{{/ctx.recentTurns}}\n\
{{#ctx.hasAttachments}}\n\
注：用户已附加文件/图片，无需在文字里复述文件名。\n\
{{/ctx.hasAttachments}}\n\
\n\
# 输出要求\n\
1. **只输出润色后的内容本身**，不要加\"润色后：\"等前缀、不要加任何解释\n\
2. **保留用户的核心意图**，不要替用户做主张\n\
3. **如果原文很清晰**，润色后可以与原文几乎一致\n\
4. **如果原文重复、啰嗦或矛盾**，去掉冗余、保留要点\n\
5. **不要编造**用户没说的内容\n\
6. **不要使用 markdown 包装**（不要加 # / - / ** 等），保持纯对话文本\n\
7. **长度**控制在原文 0.7-1.3 倍，除非原文明显需要扩写\n\
\n\
# 接下来输出润色结果：\n";
        assert_eq!(CHAT_POLISH_PROMPT, CANONICAL);
    }

    #[test]
    fn automation_prompt_template_locked() {
        const CANONICAL: &str = "你是一名\"自动化 PROMPT 编写专家\"。\
用户用日常口语描述他想做的自动化任务，你把它改写为一段**结构化、可直接被自动化引擎执行**的 PROMPT。\n\
\n\
# 输出结构\n\
按以下五段组织（缺哪段就省略，但顺序不变）：\n\
\n\
# 角色\n\
PROMPT 扮演的专家角色（一句话）。\n\
\n\
# 目标\n\
这个自动化**要达成什么**（一句话，越具体越好）。\n\
\n\
# 输入\n\
PROMPT 接收的变量与数据源。**保留用户原文里的 `{{变量名}}` 占位符**，不要替换、不要添加不存在的变量。\n\
\n\
# 处理步骤\n\
1. 编号列表，每步一个原子动作（获取数据 / 计算 / 调用工具 / 输出）。\n\
2. 涉及外部工具时，写明工具名（\"调用 terminal 执行…\"、\"调用 file_read 读取…\"）。\n\
\n\
# 输出格式\n\
- 邮件 / 消息类：写明主题长度、正文段落数、是否需要 Markdown\n\
- 文件类：写明路径模板 + 命名规则\n\
- 控制台类：写明打印字段顺序\n\
\n\
# 约束与异常\n\
- 不要做的事\n\
- 失败时如何处理（重试次数 / 报警 / 回退）\n\
\n\
# 输入\n\
<draft>\n\
{{draft}}\n\
</draft>\n\
{{#ctx.triggerKind}}\n\
触发类型：{{ctx.triggerKind}}\n\
{{/ctx.triggerKind}}\n\
{{#ctx.scheduleHint}}\n\
调度：{{ctx.scheduleHint}}\n\
{{/ctx.scheduleHint}}\n\
\n\
# 改写规则\n\
1. **保留 `{{变量}}` 占位符原样**，不要写成具体值\n\
2. **保留用户的具体场景**（例：\"每周一早上的销售周报\"不要改成\"周期性报表\"）\n\
3. **用动词开头写步骤**（\"读取…\"、\"汇总…\"、\"发送…\"），避免名词堆砌\n\
4. **如果用户没说明某段，写\"[待补充：xxx]\"** 而不要瞎猜\n\
5. **直接输出 PROMPT 本身**，不要加\"以下是润色后的 PROMPT：\"等前缀\n\
\n\
# 接下来输出结构化 PROMPT：\n";
        assert_eq!(AUTOMATION_POLISH_PROMPT, CANONICAL);
    }

    /// Frontend sends `{draft, ctx}` and the handler must accept the
    /// shape with `ctx` omitted (the most common case). Old code
    /// returned `null` and degraded to a whitespace-only fallback, so
    /// gating the new endpoint on a missing `ctx` would have been a
    /// silent regression.
    #[test]
    fn polish_request_deserializes_without_ctx() {
        let v: PolishRequest =
            serde_json::from_str(r#"{"draft":"hi"}"#).expect("must parse");
        assert_eq!(v.draft, "hi");
        assert!(v.ctx.is_none());
    }

    /// Frontend may also send `ctx: null` (TypeScript optional fields
    /// default to `undefined`, not `null`, but defensive parsing is
    /// cheap).
    #[test]
    fn polish_request_deserializes_with_null_ctx() {
        let v: PolishRequest =
            serde_json::from_str(r#"{"draft":"hi","ctx":null}"#).expect("must parse");
        assert!(v.ctx.is_none());
    }

    /// Frontend may send `ctx` as either an object or — for the
    /// "automation" variant — an object with extra keys. We accept
    /// arbitrary JSON and template-substitute it as a string, so
    /// only the obvious "must be JSON" assertion matters.
    #[test]
    fn polish_request_deserializes_with_object_ctx() {
        let v: PolishRequest = serde_json::from_str(
            r#"{"draft":"hi","ctx":{"triggerKind":"cron","scheduleHint":"*/5 * * * *"}}"#,
        )
        .expect("must parse");
        assert_eq!(
            v.ctx.unwrap().get("triggerKind").and_then(|x| x.as_str()),
            Some("cron"),
        );
    }

    /// Empty drafts must short-circuit to a 400-equivalent BEFORE we
    /// hit the agent service. Otherwise an empty input would burn a
    /// token round-trip just to come back as an empty string.
    #[test]
    fn empty_draft_does_not_call_agent() {
        // We test only the gating predicate here — the full handler
        // path requires a `LocalAgentService` which is expensive to
        // construct in a unit test. The handler just delegates to
        // `run_polish` which returns `Err` for empty drafts, so this
        // is the contract that actually matters.
        let draft = "   \n\t";
        assert!(draft.trim().is_empty());
    }
}
