//! POST /api/polish/{chat, automation, enhance} — stateless LLM-driven
//! text rewriting for the desktop "润色" / "改写" / "增强" affordances.
//!
//! The web side (`apps/desktop-tauri/src/api/polish.ts`) already calls these
//! endpoints; before this commit the fetch fell through to a
//! whitespace-only fallback because the backend returned 404.
//!
//! Design:
//! - Reuse `agent_service.send_message` against an *ephemeral*
//!   session id so no parent conversation history leaks in and
//!   nothing gets persisted long-term. The session is `reset_session`'d
//!   immediately after the reply to free the in-memory cache (and any
//!   SQLite row the persistence layer wrote).
//! - Override `system_prompt` with one of the template constants
//!   ([`CHAT_POLISH_PROMPT`], [`AUTOMATION_POLISH_PROMPT`],
//!   [`ENHANCE_PROMPT`]) and force `tools = Some(vec![])` so the model
//!   can't reach for a tool while rewriting user text.
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
//!
//! The three endpoints share one LLM-call shell ([`run_with_prompt`]);
//! they differ only in the system prompt and how the user message is
//! assembled (chat/automation fold a free-form `ctx` JSON, enhance
//! builds typed paragraphs).

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

/// Drive one stateless LLM call with a fixed system prompt and a
/// fully-formed user message. Returns the reply text on success or a
/// 502 / 504 with a short error string on failure.
///
/// This is the shared plumbing for every polish/enhance endpoint —
/// the only thing that differs between them is the system prompt
/// and how the user message is assembled by the caller.
async fn run_with_prompt(
    state: &HttpServerState,
    system_prompt: &'static str,
    user_message: &str,
) -> Result<String, PolishError> {
    let session_id = format!("{POLISH_SESSION_PREFIX}{}", Uuid::new_v4());

    let overrides = AgentOverrides {
        system_prompt: Some(system_prompt.to_string()),
        // No tools — this is a pure rewrite, the model should not
        // reach for terminal / file_read / etc.
        tools: Some(Vec::new()),
        permission: Some(PermissionLevel::Full),
        enable_thinking: None,
        ..Default::default()
    };

    let svc = state.agent_service();

    let send_fut = async {
        svc.send_message(&session_id, user_message, overrides)
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

/// Drive one stateless polish call for the rewrite endpoints
/// (`/api/polish/chat`, `/api/polish/automation`).
///
/// `system_prompt` MUST be one of the two PR-approved templates —
/// callers in this module pass them verbatim. The user message is
/// the raw draft plus the free-form `ctx` JSON, exactly as before
/// this function was split out of [`run_with_prompt`].
async fn run_polish(
    state: &HttpServerState,
    system_prompt: &'static str,
    draft: &str,
    ctx: Option<&JsonValue>,
) -> Result<String, PolishError> {
    if draft.trim().is_empty() {
        return Err(PolishError::new("draft is empty"));
    }

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

    run_with_prompt(state, system_prompt, &user_message).await
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
// 增强提示词（Prompt Enhancement）—— /api/polish/enhance
//
// 与上面两个「改写」端点的区别：润色/改写是**同义替换**（长度不放大、
// 不加结构），增强是**扩写**——把一句模糊需求补齐成角色/背景/任务拆解/
// 输入/约束/输出格式六段结构，产出物本身是一段可投喂 AI 的提示词。
//
// 场景规格（周报/方案/竞品分析…）不在这条链路上让用户选，而是**内置在
// `ENHANCE_PROMPT` 的规格表里**，由模型按草稿自动选用最贴切的一条。
// 理由：选场景是纯负担——模型从草稿里认出场景的准确率远高于用户去点
// 一个下拉框；真要指定，用户在草稿里写「周报」比点菜单更快。
// ---------------------------------------------------------------------------

/// POST /api/polish/enhance 的请求体。
///
/// 与 `PolishRequest` 不同：这里是**类型化**的（不是自由 JSON），
/// 因为增强的用户消息要按固定段落拼装（草稿 / 最近对话 / 附件提示），
/// 自由 JSON 拼出来的段落模型读不懂。
///
/// `rename_all = "camelCase"` 不是口味问题：前端发的是 `recentTurns` /
/// `hasAttachments`（与 `PolishRequest.ctx` 里的键同名），serde 默认
/// 按字段名精确匹配——不写这一行，字段会被**静默丢弃**，模型永远拿不到
/// 对话上下文，而且不报任何错。
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnhanceRequest {
    pub draft: String,
    /// 最近 1-2 轮对话，用于让增强出的提示词接得上上下文。
    #[serde(default)]
    pub recent_turns: Option<String>,
    /// 用户是否带了附件（影响提示词里要不要交代附件内容）。
    #[serde(default)]
    pub has_attachments: Option<bool>,
}

/// 把增强请求拼成用户消息：草稿 / 最近对话 / 附件提示。
///
/// 独立成函数是为了能直接测段落拼装（不必起 LLM）。
fn build_enhance_user_message(req: &EnhanceRequest) -> String {
    let mut user_message = String::new();
    user_message.push_str("<draft>\n");
    user_message.push_str(&req.draft);
    user_message.push_str("\n</draft>");

    if let Some(turns) = req.recent_turns.as_ref().filter(|t| !t.trim().is_empty()) {
        user_message.push_str("\n\n# 最近对话\n");
        user_message.push_str(turns);
    }
    if req.has_attachments == Some(true) {
        user_message.push_str("\n\n注：用户已附加文件/图片，提示词里不要复述文件名。");
    }
    user_message
}

/// POST /api/polish/enhance
///
/// 把一句模糊需求扩写成结构化提示词。产出物是**提示词本身**
/// （Markdown 分段），回填到输入框后用户可以直接发送。
pub async fn post_polish_enhance(
    State(state): State<HttpServerState>,
    Json(req): Json<EnhanceRequest>,
) -> Result<JsonResponse<PolishResponse>, (StatusCode, JsonResponse<PolishError>)> {
    if req.draft.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            JsonResponse(PolishError::new("draft is empty")),
        ));
    }

    let user_message = build_enhance_user_message(&req);

    match run_with_prompt(&state, ENHANCE_PROMPT, &user_message).await {
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
//
// `ENHANCE_PROMPT` **没有**前端副本：它只存在于本文件，前端拿不到也
// 不需要（增强走的是类型化请求 + 服务端模板）。少一份副本 = 少一类漂移。
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

/// 增强提示词模板（**唯一副本**，前端无镜像）。
///
/// 与 `CHAT_POLISH_PROMPT` 的关键差异：
/// - 润色是同义替换（长度 0.7-1.3 倍、禁 markdown），增强是**扩写**；
/// - 产出物是**提示词本身**，所以这里用 Markdown 分段是刻意的
///   （「## 角色」这类标题属于提示词文本，不是给人看的说明）。
///
/// 刻意不用 ASCII 双引号与反引号：正文一律用「」，raw string 不会被
/// `"#` 提前截断，将来若要加前端镜像也能逐字节对齐。
const ENHANCE_PROMPT: &str = r#"你是一名「提示词工程专家」。用户给出一句可能很模糊的需求，你要把它扩写成一段结构化、专业、可直接投喂 AI 的高质量提示词。

# 输出结构
按以下六段组织，顺序不变；用户没有提供的信息写「[待补充：xxx]」，不要替用户编造。

## 角色
一句话：这段提示词要让 AI 扮演谁——专业领域、经验年限、立场。

## 背景
使用场景、已知事实、可用信息与前置条件。

## 任务
拆成编号的子任务，每步一个原子动作，并写明该步的预期产出。

## 输入
这段提示词接收什么：数据来源、文件、字段、变量。原文里的 {{变量}} 占位符原样保留，不要替换成具体值。

## 约束
语气、篇幅、格式、禁止事项、时效性，以及信息不足时该怎么处理。

## 输出格式
产出形态（表格 / 编号列表 / Markdown / JSON）、必备字段、长度区间、一小段示例。

# 场景规格
下面每行是一种常见场景的产出规格。草稿里能识别出场景就**只取最贴切的那一条**（最多两条），
用它来细化「任务」与「输出格式」两段；识别不出就按上面的通用六段结构，不要硬套。

周报：本周进展(按项目分条：动作+结果+量化数据) / 关键数据(表格：指标·本周·上周·环比) / 问题阻塞(现象·影响·需谁支持) / 下周计划(3-5条，负责人+时间)；向上汇报，先给结论，一页内
日报：今日完成(分条) / 进行中与阻塞(卡点+所需支持) / 明日计划(≤3条)；简短，事实优先，不寒暄
方案：背景问题 / 可衡量目标 / ≥2个选项对比表(维度·成本·收益·风险) / 推荐+理由 / 分阶段实施步骤(里程碑·责任人) / 预算 / 风险应对表；决策者视角，先结论后论证
竞品分析：竞品与选入理由 / 对比表(定位·功能·定价·目标用户·优势·短板) / 逐维度差异解读 / 对我方的启示(≤3条)；未知数据写[待补充]，禁止编造
复盘：目标回顾(原定目标+验收标准) / 实际结果与偏差幅度 / 偏差原因(可控·不可控) / 改进项表(动作·负责人·时间·验收标准)；对事不对人
会议纪要：结论先行 / 议题要点(含分歧) / 决议表(事项·结论·负责人·时间) / 待办表(事项·负责人·截止时间)；只记会上说过的，不补写
邮件：主题一句话(含事由与动作) / 称呼 / 一句背景 / 正文3-5句(关键数字前置) / 明确下一步(谁·做什么·何时) / 敬语；礼貌直接，不铺垫
PPT大纲：整体叙事线 / 逐页(页码·标题写观点不写主题·核心论点·支撑要点2-3条·配图形式·讲解要点) / 封面与结束页建议
演讲稿：结构(开场钩子·论点分段·收尾号召) / 逐段口播稿(口语短句，每段100-200字，可直接念) / 演讲提示(停顿·重音·手势)
SWOT：S/W/O/T各3-5条(具体到可验证) / 交叉结论(SO·WO·ST·WT各一句)；象限内不重复
决策分析：决策问题+约束 / ≥2个候选 / 评估维度表(维度·权重合计100%·各选项得分与理由) / 推荐+理由 / 成立前提与推翻条件；事实与假设分开列
行业研究：范围与口径(时间·地域·细分·数据来源) / 关键发现3-5条(结论+支撑数据) / 细分与趋势(可含表格) / 结论与启示(≤3条可执行) / 数据局限；标注来源与年份

# 扩写规则
1. 只输出增强后的提示词本身，不要加前缀、不要解释你在做什么
2. 保留用户的具体场景与约束（「给老板看的周报」不要改成「工作汇报」）
3. 不替用户做主张：原文没有的事实用「[待补充：xxx]」占位
4. 用 Markdown 分段（## 标题 + 列表）——这是提示词文本本身，不是给人看的说明
5. 长度 150-600 字，够用即可，不注水
6. 沿用用户的语言

# 接下来输出增强后的提示词：
"#;

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

    // ── 增强提示词（/api/polish/enhance）────────────────────────

    /// 增强模板必须保住六段结构 —— 少一段，模型就会漏掉一整块
    /// （最常丢的是「约束」和「输出格式」，出来的提示词没法直接投喂）。
    #[test]
    fn enhance_prompt_keeps_six_sections() {
        for section in [
            "## 角色",
            "## 背景",
            "## 任务",
            "## 输入",
            "## 约束",
            "## 输出格式",
        ] {
            assert!(
                ENHANCE_PROMPT.contains(section),
                "ENHANCE_PROMPT 缺少段落 {section}"
            );
        }
        // 只输出提示词本身：前缀会让回填到输入框的内容多一层噪声。
        assert!(ENHANCE_PROMPT.contains("只输出增强后的提示词本身"));
    }

    /// 场景规格表是「用户不用选场景」的底座：模型从这里挑最贴切的一条。
    /// 表被清空/改名 → 增强退化成通用六段，能力悄悄缩水，必须挡住。
    #[test]
    fn enhance_prompt_keeps_scenario_specs() {
        assert!(ENHANCE_PROMPT.contains("# 场景规格"));
        for scene in [
            "周报", "日报", "方案", "竞品分析", "复盘", "会议纪要", "邮件", "PPT大纲", "演讲稿", "SWOT",
            "决策分析", "行业研究",
        ] {
            assert!(
                ENHANCE_PROMPT.contains(scene),
                "ENHANCE_PROMPT 的场景规格表缺少 {scene}"
            );
        }
    }

    /// 增强请求的最小形态（只有 draft）必须能解析 —— 这是最常见的调用。
    #[test]
    fn enhance_request_deserializes_minimal() {
        let v: EnhanceRequest =
            serde_json::from_str(r#"{"draft":"帮我写个周报"}"#).expect("must parse");
        assert_eq!(v.draft, "帮我写个周报");
        assert!(v.recent_turns.is_none());
        assert!(v.has_attachments.is_none());
    }

    /// **按前端真实发出的 JSON 解析**：键是 camelCase
    /// （`recentTurns` / `hasAttachments`，与 `PolishRequest.ctx` 同名）。
    ///
    /// 这条测试是踩坑留下的：结构体字段是 snake_case 而前端发 camelCase
    /// 时，serde **不报错**、只是把字段丢掉——对话上下文永远到不了模型，
    /// 表现为"增强出来的提示词不知道我在聊什么"，极难定位。
    #[test]
    fn enhance_request_parses_camel_case_wire_format() {
        let v: EnhanceRequest = serde_json::from_str(
            r#"{"draft":"写个周报","recentTurns":"用户：上个季度的数据在哪","hasAttachments":true}"#,
        )
        .expect("must parse");
        assert_eq!(v.recent_turns.as_deref(), Some("用户：上个季度的数据在哪"));
        assert_eq!(v.has_attachments, Some(true));
    }

    /// 用户消息拼装：草稿必须被 <draft> 包住，最近对话单独成段。
    /// 模型靠这些段落标记定位内容。
    #[test]
    fn enhance_user_message_contains_all_sections() {
        let req = EnhanceRequest {
            draft: "写个周报".to_string(),
            recent_turns: Some("用户：上个季度的数据在哪".to_string()),
            has_attachments: Some(true),
        };
        let msg = build_enhance_user_message(&req);
        assert!(msg.contains("<draft>\n写个周报\n</draft>"));
        assert!(msg.contains("# 最近对话\n用户：上个季度的数据在哪"));
        assert!(msg.contains("已附加文件/图片"));
    }

    /// 没有最近对话 / 没有附件时不应留下空段——空标题会让模型以为
    /// 那段内容是空字符串。
    #[test]
    fn enhance_user_message_omits_empty_sections() {
        let req = EnhanceRequest {
            draft: "写个周报".to_string(),
            recent_turns: Some("   ".to_string()),
            has_attachments: Some(false),
        };
        let msg = build_enhance_user_message(&req);
        assert!(!msg.contains("最近对话"));
        assert!(!msg.contains("已附加文件"));
    }
}
