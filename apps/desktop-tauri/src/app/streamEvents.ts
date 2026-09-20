// 流式事件相关的纯类型与辅助函数，从 App.tsx 抽出，避免根组件过度膨胀。
// 这些函数均为模块级纯函数，不依赖任何组件作用域状态。
import type { TimelineStep, ExecutionTimelineEvent } from "../types";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";

// Welcome-state draft workspace. Read at App-level so
// both `handleNewChat` (新建对话 button) and `handleSendMessage`
// (first send in an empty session) can pass it to
// `api.createSession(title, project)` — that way the server records
// it on `sessions.workspace` from the start, and a desktop restart
// restores the same Sidebar bucket instead of dumping the row into
// "未分组"（draft workspace 未随 session 持久化的回归）。
function readDraftWorkspace(): string | null {
  try {
    return storage.getItem(STORAGE_KEYS.draftWorkspace);
  } catch {
    return null;
  }
}

/**
 * Coarse-grained error category used by the stream error card to
 * pick an icon, title, and short hint. Mirrors the backend's
 * `user_facing_agent_error` `code` field; the frontend infers a
 * category when the backend omitted one (e.g. raw transport errors
 * landing in `done.error` or the WS onclose path).
 */
export type StreamErrorKind =
  | "overloaded"
  | "quota"
  | "auth"
  | "network"
  | "timeout"
  | "bad_request"
  | "unknown";

/**
 * Map known English backend error strings to Chinese for a better
 * desktop user experience. Falls through to English for anything
 * unrecognised.
 */
export function localizeStreamError(err: string): string {
  // Normalize: strip redundant wrappers ("LLM API error:" repeated, etc.)
  const normalized = err
    .replace(/^LLM\s+API\s+error:\s*/gi, "")
    .replace(/^LLM\s+API\s+error:\s*/gi, "")
    .replace(/^API\s+error:\s*/gi, "")
    .trim();

  // Try to extract a human-readable message from JSON error bodies
  // e.g. {"error":{"message":"unknown model..."}} or {"type":"error","error":{...}}
  let extracted = normalized;
  try {
    const parsed = JSON.parse(normalized);
    const msg =
      parsed?.error?.message ??
      parsed?.error?.error?.message ??
      parsed?.message ??
      null;
    if (msg && typeof msg === "string") {
      extracted = msg;
    }
  } catch {
    // not JSON — keep normalized as-is
  }

  // No API key configured for provider
  if (
    /no\s+api\s+key/i.test(extracted) ||
    /api[\s_]*key[\s:]*(not\s+found|missing|empty|not\s+configured)/i.test(extracted)
  ) {
    return "LLM API 密钥未配置。请前往「设置 → API Keys」添加 API 密钥后重试。";
  }
  // LLM provider URL not configured
  if (
    /no\s+llm_provider_url/i.test(extracted) ||
    /provider.*url.*not\s+configured/i.test(extracted)
  ) {
    return "LLM 服务地址未配置。请检查 Settings → API Keys 中的服务地址配置。";
  }
  // Connection refused / unreachable
  if (/(refused|unreachable|connection\s+refused|ECONNREFUSED)/i.test(extracted)) {
    return "无法连接到后端服务，请确认 hermix 进程是否正常运行。";
  }
  // Timeout
  if (/(timeout|timed\s*out|stream_overall_timeout|turn_transition_stall|no_first_chunk)/i.test(extracted)) {
    return "请求超时，请检查网络连接或稍后重试。";
  }
  // auth / token / 401 / 403
  if (/(401|403|unauthorized|forbidden|invalid\s+(api\s*)?key|auth\s+failed)/i.test(extracted)) {
    return "API 密钥认证失败，请检查 Settings → API Keys 中的密钥是否正确。";
  }
  // Rate limit / quota
  if (/(rate\s*limit|429|too\s+many\s+requests|quota|usage\s+limit|用量上限|token\s+plan)/i.test(extracted)) {
    return "用量已达上限或触发限流，请稍后重试或检查账户配额。";
  }
  // 本地后端（stdio 桥）不可达：桥未初始化 / 子进程已退出。
  // 必须排在 provider 的 503 分支之前 —— 两者最终都是 503，但处置相反：
  // provider 503 是"等一会儿再试"，后端 503 是"本地服务没起来，去重启"。
  if (/backend\s+offline|backend\s+exited|stdio\s+bridge\s+not\s+initialised/i.test(extracted)) {
    return "本地后端未运行。请点击标题栏连接指示灯重启服务后重试。";
  }
  // 本地后端的 4xx：请求被自己人拒绝（协议/参数不匹配），与网络无关。
  // 原文（含 axum 的字段级报错）已经由调用方压进 detail 供复制，
  // 这里只给一句不误导的结论，避免用户去查端口。
  if (/^HTTP\s+4\d\d\b/.test(extracted)) {
    return "请求被后端拒绝（协议或参数不匹配）。请复制技术详情反馈，无需检查网络或端口。";
  }
  if (/^HTTP\s+5\d\d\b/.test(extracted)) {
    return "后端处理请求时出错。请稍后重试；若持续出现请复制技术详情反馈。";
  }
  // Provider overloaded / service unavailable (529 / 503 / 5xx)
  if (/(529|overloaded|负载较高|service\s+unavailable|503|server\s+error)/i.test(extracted)) {
    return "AI 服务暂时过载，请稍后重试。";
  }
  // 400 / bad request — model not found (common MiniMax error)
  if (/(400|bad\s+request|unknown\s+model|invalid\s+model|model.*not\s+found)/i.test(extracted)) {
    // Extract the model name if present
    const modelMatch = extracted.match(/model\s+['"]?([^\s'"]+)['"]?/i);
    const modelName = modelMatch ? modelMatch[1] : null;
    return modelName
      ? `模型「${modelName}」不可用，请在输入框左侧切换模型后重试。`
      : "模型不可用或参数错误，请检查模型配置后重试。";
  }
  // WebSocket / connection lost
  if (/WebSocket\s*error/i.test(extracted)) {
    return "连接异常（WebSocket 错误），请检查网络后重新发送消息。";
  }
  if (/Connection\s*closed/i.test(extracted)) {
    return "连接已断开，请重新发送消息。";
  }
  // Browser fetch / HTTP request failures often contain the provider URL,
  // which is not useful in the composer and makes the banner look like a
  // raw log line. Reduce these to a short actionable message.
  if (/(HTTP\s+request\s+failed|Failed\s+to\s+fetch|NetworkError|network\s+error|error\s+sending\s+request)/i.test(extracted)) {
    return "网络请求失败，请检查网络连接或稍后重试。";
  }
  // Fallback: return the extracted message (already normalized)
  return extracted || err;
}

/**
 * Coarse-grained classifier for stream errors. Pairs with
 * `localizeStreamError` — both are pure functions on the error
 * string so the StreamErrorCard can render a category-specific
 * title and icon without leaning on the backend's `code` field.
 */
export function classifyStreamErrorKind(err: string): StreamErrorKind {
  const e = err || "";
  // 本地后端不可达优先判定：它的 4xx 是"请求被自己人拒绝"（协议 bug），
  // 与 provider 的 401/400 语义完全不同，放在后面会被误分类成 auth /
  // bad_request，把用户引向"检查 API key"这条错误的路。
  if (/^HTTP\s+4\d\d\b/.test(e) || /backend\s+offline|backend\s+exited|stdio\s+bridge\s+not\s+initialised/i.test(e)) {
    return "bad_request";
  }
  if (/^HTTP\s+5\d\d\b/.test(e)) {
    return "overloaded";
  }
  if (/(529|overloaded|负载较高|service\s+unavailable|503|server\s+error|api\s*error\s*5\d{2})/i.test(e)) {
    return "overloaded";
  }
  if (/(429|too\s+many|rate\s*limit|rate_limit|quota|usage\s+limit|用量上限|token\s+plan)/i.test(e)) {
    return "quota";
  }
  if (/(401|403|unauthorized|forbidden|invalid\s+(api\s*)?key|auth\s+failed|no\s+api\s+key|api[\s_]*key.*not)/i.test(e)) {
    return "auth";
  }
  if (/(timeout|timed\s*out|stream_overall_timeout|turn_transition_stall|no_first_chunk)/i.test(e)) {
    return "timeout";
  }
  if (/(connection\s+refused|unreachable|ECONNREFUSED|network\s+error|Failed\s+to\s+fetch|NetworkError|HTTP\s+request\s+failed|websocket|connection\s+closed|econn|reset\s+by\s+peer)/i.test(e)) {
    return "network";
  }
  if (/(400|bad\s+request|unknown\s+model|invalid\s+model|model.*not\s+found|invalid\s+request)/i.test(e)) {
    return "bad_request";
  }
  return "unknown";
}

/**
 * The backend's `user_facing_agent_error` emits its own `code`
 * taxonomy (`provider_overloaded`, `quota_exhausted`, `auth_error`,
 * `agent_error`). Translate those into the StreamErrorCard's
 * `StreamErrorKind` so the UI stays decoupled from the wire format.
 * When the backend omitted a code, fall back to local pattern
 * matching on the human-readable message.
 */
export function normalizeStreamErrorKind(
  code: string | undefined,
  fallbackText: string,
): StreamErrorKind {
  if (!code) return classifyStreamErrorKind(fallbackText);
  const k = code.toLowerCase();
  if (k === "provider_overloaded" || k === "overloaded") return "overloaded";
  if (k === "quota_exhausted" || k === "quota" || k === "rate_limit") return "quota";
  if (k === "auth_error" || k === "auth" || k === "unauthorized") return "auth";
  if (k === "timeout") return "timeout";
  if (k === "network" || k === "transport") return "network";
  if (k === "bad_request" || k === "invalid_request") return "bad_request";
  return classifyStreamErrorKind(fallbackText);
}

// ── Unified timeline step helpers ─────────────────────────────────
// These functions maintain the `unified_steps` array on the streaming
// assistant message. They track phase transitions (reasoning → tools →
// text) so that multi-iteration agent loops display each step in
// chronological order instead of a fixed 3-section layout.

/**
 * Append reasoning text to the last reasoning step, or push a new one.
 *
 * When the LLM (e.g. MiniMax M3) sends `reasoning_content`
 * and `content` in the same SSE chunk, the server forwards them as
 * separate `StreamEvent::Thinking` and `StreamEvent::Text` events.
 * The text event creates a "text" step that breaks reasoning accumulation,
 * causing a single thinking process to render as multiple "已深度思考"
 * blocks. Fix: scan backwards past "text" steps to find the last
 * "reasoning" step. If found, append to it so all reasoning stays in
 * one collapsible block. Only "text" steps are skipped — "tools" or
 * other step types mark a real phase boundary and trigger a new block.
 */
export function appendReasoningStep(steps: TimelineStep[] | undefined, content: string): TimelineStep[] {
  const arr = steps ? [...steps] : [];
  const last = arr[arr.length - 1];
  if (last && last.kind === "reasoning") {
    arr[arr.length - 1] = { kind: "reasoning", content: last.content + content };
    return arr;
  }
  // Scan backwards past "text" steps to find the last "reasoning" step.
  // This prevents interleaved text events from splitting the reasoning.
  for (let i = arr.length - 1; i >= 0; i--) {
    if (arr[i].kind === "reasoning") {
      arr[i] = { kind: "reasoning", content: (arr[i] as { content: string }).content + content };
      return arr;
    }
    // Non-text step (e.g. "tools") marks a real phase boundary — stop.
    if (arr[i].kind !== "text") {
      break;
    }
  }
  arr.push({ kind: "reasoning", content });
  return arr;
}

/** Append text to the last text step, or push a new one. */
export function appendTextStep(steps: TimelineStep[] | undefined, content: string): TimelineStep[] {
  const arr = steps ? [...steps] : [];
  const last = arr[arr.length - 1];
  if (last && last.kind === "text") {
    arr[arr.length - 1] = { kind: "text", content: last.content + content };
  } else {
    arr.push({ kind: "text", content });
  }
  return arr;
}

/** Append a tool event to the last tools step, or push a new one. */
export function appendToolStep(steps: TimelineStep[] | undefined, event: ExecutionTimelineEvent): TimelineStep[] {
  const arr = steps ? [...steps] : [];
  const last = arr[arr.length - 1];
  if (last && last.kind === "tools") {
    arr[arr.length - 1] = { kind: "tools", events: [...last.events, event] };
  } else {
    arr.push({ kind: "tools", events: [event] });
  }
  return arr;
}

/**
 * Update (or create) a `tool_stdout` event in the last tools step,
 * mirroring the same logic that `onToolChunk` applies to
 * `execution_timeline`. This keeps the unified_steps in sync so the
 * rendered tool cards show live stdout.
 */
export function updateToolStepStdout(
  steps: TimelineStep[] | undefined,
  toolCallId: string,
  content: string,
): TimelineStep[] {
  const arr = steps ? [...steps] : [];
  const lastIdx = arr.length - 1;
  if (lastIdx < 0 || arr[lastIdx].kind !== "tools") return arr;
  const events = [...arr[lastIdx].events];
  let found = false;
  for (let i = events.length - 1; i >= 0; i--) {
    if (events[i].type === "tool_stdout" && events[i].tool_call_id === toolCallId) {
      events[i] = { ...events[i], content: (events[i].content ?? "") + content };
      found = true;
      break;
    }
  }
  if (!found) {
    events.push({
      type: "tool_stdout",
      tool_call_id: toolCallId,
      content,
      created_at: new Date().toISOString(),
    });
  }
  arr[lastIdx] = { kind: "tools", events };
  return arr;
}

export { readDraftWorkspace };
