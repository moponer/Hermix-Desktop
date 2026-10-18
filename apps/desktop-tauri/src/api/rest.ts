// Hermix Desktop API Client — REST 运行时（从 api.ts 拆分；类型见 ./types，WS 见 ./ws）
import type { Session, AutomationTask } from "../types";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";
// v5: reuse detectToolError from messageParser for errorReason extraction
// on reloaded tool messages — same helper as the live-streaming path.
import { detectToolError } from "../components/messageParser";
import { wireArrayToChatMessages, type WireMessage } from "../protocolAdapter";
import type {
  AutoCorrectResponse,
  CustomProviderCreateRequest,
  CustomProviderSummary,
  CustomProviderTestConnectionResponse,
  HermesConfig,
  McpServerEntry,
  McpTransport,
  ModelListEntry,
  ModelListResponse,
  PluginSettings,
  RemoteExpert,
  RestoredContextUsage,
  SessionMessagesResult,
  SessionSummary,
  SkillContent,
  SkillInfo,
  TickLoopStatus,
  ToolDescriptor,
  ToolsetInfo,
  ToolsetToggleResult,
  AnalyticsResponse,
  AnalyticsDebugResponse,
  CatalogResponse,
  CatalogInstall,
  CatalogScanHit,
  CatalogVerdict,
  ChannelStatusItem,
  WorkflowEntry,
} from "./types";
export type { WorkflowEntry };

// Transport switched from `fetch()` over `127.0.0.1:3000` to
// `invoke<string>("backend_http_request")` over the Tauri stdio
// bridge. `API_BASE` / `WS_BASE` / `apiUrl()` no longer exist —
// there is no host or port. Anything that still needs the old
// sentinel (e.g. `providerConfig.ts`'s "user left the provider URL
// blank" fallback) must hard-code its own now.

// ── Stdio transport primitives ─────────────────────────────────
//
// All REST-style calls go through the Tauri stdio bridge:
//   1. The bridge (`stdio_bridge.rs::http_request`) sends a JSON Lines
//      frame on the sidecar's stdin with a fresh request_id.
//   2. The server's stdio multiplex (`hermix-server::stdio_session`)
//      dispatches the frame into the same axum router the TCP era
//      exposed — handler code is unchanged.
//   3. The reply is returned as a JSON envelope
//      `{"status":N,"body":"…","enc":"utf8"|"base64"}` so the
//      front-end only ever has one parser.

import { invoke } from "@tauri-apps/api/core";

/** Per-request timeout. The stdio bridge has its own 120s oneshot
 *  timeout; we add a client-side deadline so a hung request surfaces
 *  in 10s instead of 120s.
 *
 *  NOTE: `backend_http_request` takes no cancellation token (see
 *  `src-tauri/src/stdio_bridge.rs`), so we cannot actually interrupt
 *  an in-flight bridge round-trip. The deadline below rejects the
 *  *caller* at 10s via `Promise.race`; the Rust-side request may still
 *  complete in the background. A caller's `AbortSignal` likewise
 *  releases the caller early but cannot stop the bridge. */
const REQUEST_TIMEOUT_MS = 10_000;

/** HTTP-style envelope returned by `backend_http_request`. The
 *  server's stdio handler wraps every axum response in this shape
 *  so the front-end only ever has one parser. Binary bodies come
 *  back as `enc="base64"` so UTF-8 framing can't corrupt them. */
export interface ApiReply {
  status: number;
  body: string;
  enc: "utf8" | "base64";
}

/** Lowest-level entry point. Sends a JSON Lines frame on the stdio
 *  bridge's `http` channel, awaits the matching request_id reply,
 *  and returns the envelope (status + body + enc) for the caller
 *  to interpret. Throws on bridge error / timeout — those are
 *  genuinely fatal (no bridge = no backend). */
export async function apiInvoke(
  method: string,
  path: string,
  body?: unknown,
  signal?: AbortSignal,
): Promise<ApiReply> {
  // 调用点传入的 body 已经是 JSON 文本（fetchJSON 里 JSON.stringify 的产物）。
  // 这里绝不能再 stringify 一次：字符串会被再包一层引号，server 端把该文本
  // 原样作为 axum 请求体，Json extractor 收到的是字符串而非对象 →
  // 422 "invalid type: string ..., expected struct X"。非字符串（对象）才序列化。
  const payload =
    body === undefined
      ? null
      : typeof body === "string"
        ? body
        : JSON.stringify(body);
  const timeoutSignal = AbortSignal.timeout(REQUEST_TIMEOUT_MS);
  const combined = signal
    ? AbortSignal.any([signal, timeoutSignal])
    : timeoutSignal;

  // Tauri forwards the optional 3rd arg as `body` to the Rust
  // command — when null we still pass it so the Rust signature
  // (which expects `Option<String>`) is happy.
  //
  // `Promise.race` against the combined signal is what makes the
  // deadline real. The previous code awaited `invoke` and only then
  // checked `combined.aborted`, so a hung request still blocked for
  // the full bridge timeout and a caller's `abort()` had no effect
  // until the response landed.
  const abortPromise = new Promise<never>((_resolve, reject) => {
    if (combined.aborted) {
      reject(new DOMException("request aborted", "AbortError"));
      return;
    }
    const onAbort = () =>
      reject(
        new DOMException(
          combined.reason?.name === "TimeoutError"
            ? `request timed out after ${REQUEST_TIMEOUT_MS}ms`
            : "request aborted",
          combined.reason?.name === "TimeoutError" ? "TimeoutError" : "AbortError",
        ),
      );
    combined.addEventListener("abort", onAbort, { once: true });
  });

  const raw = await Promise.race([
    invoke<string>("backend_http_request", {
      method,
      path,
      body: payload,
    }),
    abortPromise,
  ]);

  // Server reply is the bridge envelope; parse eagerly so callers
  // can branch on `status` before decoding the body.
  return JSON.parse(raw) as ApiReply;
}

/** JSON fetch over the stdio bridge. Throws on non-2xx, mirroring
 *  the old `fetch()` semantics so call sites keep their
 *  `try { ... } catch { return null }` shape. */
export async function fetchJSON<T>(path: string, init?: RequestInit): Promise<T> {
  const method = (init?.method ?? "GET").toUpperCase();
  const body =
    init?.body == null
      ? undefined
      : typeof init.body === "string"
        ? init.body
        : JSON.stringify(init.body);
  // `init.headers` is deliberately NOT forwarded. The bridge command
  // (`backend_http_request` -> stdio `http` frame -> axum) has no
  // header channel at all, and the server force-injects
  // `content-type: application/json` for any request with a body
  // (stdio_session.rs:190). Every call site passes only Content-Type,
  // so forwarding it would be a no-op that reads as if it worked.
  //
  // If you ever need a real header (Authorization, Accept, …), it must
  // go through raw `fetch()` — see `api/polish.ts` — because silently
  // dropping it here is exactly the failure mode this note exists to
  // prevent.
  const reply = await apiInvoke(method, path, body, init?.signal ?? undefined);
  if (reply.status < 200 || reply.status >= 300) {
    const message = reply.enc === "utf8"
      ? (() => {
          try {
            // Backend error envelope is `{error}` (errors.rs
            // HttpError::into_response), NOT `{message}`. Reading only
            // `message` made every 4xx/5xx surface the raw JSON body.
            // Accept both keys — `error` wins when present.
            const parsed = JSON.parse(reply.body) as { message?: unknown; error?: unknown };
            if (typeof parsed.error === "string" && parsed.error) return parsed.error;
            if (typeof parsed.message === "string" && parsed.message) return parsed.message;
            return reply.body;
          } catch {
            return reply.body;
          }
        })()
      : reply.body;
    throw new Error(message || `HTTP ${reply.status}`);
  }
  if (reply.enc === "base64") {
    // Binary path — caller asked for a Blob, not JSON. We don't try
    // to infer here; `fetchJSONBinary` is the explicit choice.
    throw new Error("fetchJSON: response is binary; use fetchJSONBinary");
  }
  // 204 No Content / 空响应体（如 DELETE /api/pairing/devices/:id 成功时
  // 后端返回 204 无 body）：没有可解析的内容，直接返回 undefined。
  // 之前无条件 JSON.parse("") 抛 "Unexpected end of JSON input"，
  // 撤销成功反被调用方当成失败弹 toast。
  if (!reply.body || reply.body.trim() === "") {
    return undefined as T;
  }
  try {
    return JSON.parse(reply.body) as T;
  } catch (e) {
    throw new Error(`fetchJSON: malformed JSON body: ${String(e)}`);
  }
}

/** Binary fetch — receive blob bytes without going through JSON. */
export async function fetchJSONBinary(
  method: string,
  path: string,
): Promise<Blob> {
  const reply = await apiInvoke(method, path);
  if (reply.status < 200 || reply.status >= 300) {
    throw new Error(`HTTP ${reply.status}: ${reply.body}`);
  }
  if (reply.enc !== "base64") {
    throw new Error("fetchJSONBinary: expected base64 envelope");
  }
  const bin = atob(reply.body);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return new Blob([out]);
}

/** POST/DELETE without a JSON response body. Throws on non-2xx,
 *  identical to `fetchJSON` minus the decode step. */
async function fetchVoid(path: string, init?: RequestInit): Promise<void> {
  const method = (init?.method ?? "GET").toUpperCase();
  const body =
    init?.body == null
      ? undefined
      : typeof init.body === "string"
        ? init.body
        : JSON.stringify(init.body);
  const reply = await apiInvoke(method, path, body, init?.signal ?? undefined);
  if (reply.status < 200 || reply.status >= 300) {
    throw new Error(`HTTP ${reply.status}: ${reply.body}`);
  }
}

/** Legacy stub — kept so `isCircuitTripped` imports from `../api`
 *  don't break. The stdio bridge doesn't have a comparable failure
 *  mode (its on-wire errors are surfaced as Rust `Result::Err`,
 *  not as connection-pool starvation), so the circuit is permanently
 *  closed. Components that polled this to gate UI can keep polling
 *  and just always see `false`. */


export interface ProviderMeta {
  id: string;
  env_var: string;
  /** 后端 `default_model_for_provider` 的人工清单；未收录的 provider 为 null。 */
  default_model?: string | null;
}

export async function getProviderMeta(): Promise<ProviderMeta[]> {
  return fetchJSON<ProviderMeta[]>("/api/providers/meta");
}

// ── P5 L3 订阅快照（设计 §3.5 / 设计 §6.3 P7 前端对账）────────────────
// 服务端 `GET /v1/sessions/{id}/subscribe?since_seq=N` 的客户端封装。
// 重连对账时调用：携本 attachment 的本地 seq 水位（since_seq），
// 服务端回「已追上 → 空增量 + fullSnapshot=false」或「落后 → 全量
// 快照 + fullSnapshot=true」。payload 里 messages 是
// `get_session_messages` 的同一份快照构造（含压缩归档子会话 + 审批卡片），
// 已追上分支回空数组（客户端不替换本地会话状态，只刷新水位）。
export interface SubscribeSnapshot {
  sessionId: string;
  /** 已追上（since_seq >= lastSeq）→ 空数组；落后 → 全量快照条目。 */
  messages: unknown[];
  /** 持久 seq 水位（客户端下次重连的 since_seq 权威锚，跨重启有效）。 */
  lastSeq: number;
  /** 客户端本次传入的 since_seq（回显，便于对账断言）。 */
  sinceSeq: number;
  /** true = 落后全量快照（客户端整体替换本地会话状态）；
   *  false = 已追上空增量（客户端刷新水位、不替换状态）。 */
  fullSnapshot: boolean;
}

/** 订阅快照端点。sinceSeq 缺省 0（首次重连，必然落后 → 全量快照）。
 *  走 stdio bridge `http` 通道（apiInvoke → backend_http_request），与
 *  其它 REST 同一传输——订阅端点本身是 GET，无 body。 */
export async function subscribeSessionSnapshot(
  sessionId: string,
  sinceSeq = 0,
  signal?: AbortSignal,
): Promise<SubscribeSnapshot> {
  return fetchJSON<SubscribeSnapshot>(
    `/v1/sessions/${encodeURIComponent(sessionId)}/subscribe?since_seq=${sinceSeq}`,
    { method: "GET", signal },
  );
}

/** 后端权威解析一批模型的上下文窗口（值 + 来源分层）。
 *  后端拥有 catalog（`get_model_context_length_with_source`），前端不再本地镜像
 *  `DEFAULT_CONTEXT_LENGTHS` —— 那份手抄表与后端必然漂移。 */
export async function resolveContextWindows(
  models: string[],
): Promise<Record<string, { value: number; source: string }>> {
  if (models.length === 0) return {};
  const res = await fetchJSON<{
    windows: Record<string, { value: number; source: string }>;
  }>("/api/models/context-windows", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ models }),
  });
  return res.windows ?? {};
}

// ── Todo (task list) ─────────────────────────────────────────

export async function patchTodo(
  id: string,
  patch: { status?: string; title?: string; priority?: string },
  sessionId?: string,
): Promise<{ ok: boolean; raw?: string }> {
  // 后端从 query 读 session_id；缺省时该行以 session_id=None 落库，
  // 重连快照不再包含它（routes.rs 注释明示的孤儿行问题）。调用方
  // 持有会话上下文时必须传入。
  const qs = sessionId ? `?session_id=${encodeURIComponent(sessionId)}` : "";
  return fetchJSON(`/v1/todos/${encodeURIComponent(id)}${qs}`, {
    method: "PATCH",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(patch),
  });
}

// ── Session API (v1 protocol) ──────────────────────

function toSession(s: SessionSummary): Session {
  const now = new Date().toISOString();
  return {
    id: s.id,
    title: s.title?.trim() || "新建对话",
    created_at: s.created_at ?? now,
    updated_at: s.updated_at ?? now,
    messages: [],
    // Carry the backend's authoritative message count forward.
    message_count: s.message_count ?? 0,
    // Restore workspace/model/expert from backend session metadata.
    workspace: s.workspace ?? undefined,
    model: s.model ?? undefined,
    expert_id: s.expert_id ?? undefined,
    workbench_id: s.workbench_id ?? undefined,
  };
}

/**
 * List sessions. Omitting `workbenchId` returns only ordinary chats —
 * the backend filters `workbench_id IS NULL`, so workbench conversations
 * never appear in the main sidebar.
 */
export const getSessions = async (workbenchId?: string): Promise<Session[]> => {
  const qs = workbenchId
    ? `?workbench_id=${encodeURIComponent(workbenchId)}`
    : "";
  const res = await fetchJSON<{ sessions: SessionSummary[] }>(`/v1/sessions${qs}`);
  return res.sessions.map(toSession);
};

export const createSession = async (title: string, project?: string): Promise<Session> => {
  const res = await fetchJSON<SessionSummary>("/v1/sessions", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ title, project }),
  });
  return toSession(res);
};

export const deleteSession = async (id: string): Promise<boolean> => {
  await fetchVoid(`/v1/sessions/${encodeURIComponent(id)}`, { method: "DELETE" });
  return true;
};

/**
 * PATCH /v1/sessions/{id}
 * Update session metadata (workspace, expert_id, model, workbench_id).
 * Only the fields present in `patch` are updated; `null` clears a field.
 */
export const updateSessionMetadata = async (
  id: string,
  patch: {
    workspace?: string | null;
    expert_id?: string | null;
    model?: string | null;
    workbench_id?: string | null;
  },
): Promise<{ rows_affected: number }> => {
  return fetchJSON<{ ok: boolean; rows_affected: number }>(
    `/v1/sessions/${encodeURIComponent(id)}`,
    {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(patch),
    },
  );
};

/**
 * Soft-delete the most recent `count` user-to-assistant turns of a
 * session. Returns the number of messages that were deactivated, or
 * `null` on transport failure.
 */
export const undoSession = async (
  id: string,
  count: number = 1,
): Promise<{ deactivated: number; turns_requested: number } | null> => {
  try {
    const res = await fetchJSON<{ deactivated: number; turns_requested: number }>(
      `/v1/sessions/${encodeURIComponent(id)}/undo?count=${count}`,
      { method: "POST" },
    );
    return { deactivated: res.deactivated ?? 0, turns_requested: res.turns_requested ?? count };
  } catch (e) {
    console.error("undoSession failed", e);
    return null;
  }
};

/** Restore the most recently deactivated block. Returns the count or null. */
export const restoreSession = async (
  id: string,
): Promise<{ restored: number } | null> => {
  try {
    const res = await fetchJSON<{ restored: number }>(
      `/v1/sessions/${encodeURIComponent(id)}/restore`,
      { method: "POST" },
    );
    return { restored: res.restored ?? 0 };
  } catch (e) {
    console.error("restoreSession failed", e);
    return null;
  }
};

// TP1: dynamic tool discovery. The server's
// `GET /v1/tools` is the single source of truth. The front-end keeps a 30s
// module-level cache so re-renders don't hammer the server.
let _toolsCache: { at: number; tools: ToolDescriptor[] } | null = null;
const TOOLS_CACHE_TTL_MS = 30_000;

export const getAvailableTools = async (
  forceRefresh = false,
): Promise<ToolDescriptor[]> => {
  const now = Date.now();
  if (
    !forceRefresh &&
    _toolsCache &&
    now - _toolsCache.at < TOOLS_CACHE_TTL_MS
  ) {
    return _toolsCache.tools;
  }
  try {
    const res = await fetchJSON<{ tools: ToolDescriptor[] }>("/v1/tools");
    const tools = res.tools ?? [];
    _toolsCache = { at: now, tools };
    return tools;
  } catch (e) {
    console.error("getAvailableTools failed", e);
    // If we have a stale cache, prefer it over an empty list.
    return _toolsCache?.tools ?? [];
  }
};

// invalidateToolsCache 于 2026-10-18 移除（零调用）：调用方直接用
// `getAvailableTools(true)` 即可强制重取。

export const renameSession = async (id: string, title: string): Promise<boolean> => {
  await fetchVoid(`/v1/sessions/${encodeURIComponent(id)}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ title }),
  });
  return true;
};

/** One still-pending approval as returned by the hydration endpoint.
 *  `tool` / `arguments` may be the placeholder ("unknown" / null) when the
 *  persisted row hasn't been written yet — the UI falls back to its own
 *  transcript for the card label in that case. */
export interface PendingApprovalItem {
  request_id: string;
  tool: string;
  arguments: unknown;
  /** Server-emitted deadline (Unix ms). `null` when the server never set
   *  one — the UI then renders no countdown. */
  expires_at: number | null;
}

/** List the approvals still live in the session's ApprovalRegistry.
 *  Used after a reconnect / `task_in_flight` settle to re-render approval
 *  cards the client missed while its socket was closed. An empty list is a
 *  normal outcome (nothing pending), NOT an error — callers must not treat
 *  it as a failure. Network errors resolve to an empty list with a console
 *  warning so the UI never blocks on hydration. */
export const getPendingApprovals = async (
  id: string,
): Promise<PendingApprovalItem[]> => {
  try {
    const res = await fetchJSON<{ approvals: PendingApprovalItem[] }>(
      `/v1/sessions/${encodeURIComponent(id)}/approvals/pending`,
    );
    return res.approvals ?? [];
  } catch (e) {
    console.warn(`getPendingApprovals(${id}) failed — returning empty`, e);
    return [];
  }
};

/** 会话打开时首屏只载最近 N 条 —— 长会话（工作台对话）全量拉取会让
 *  wire 载荷与渲染都随历史线性膨胀；更早的历史走「加载更早」按需翻页。 */
export const INITIAL_MESSAGE_WINDOW = 60;
/** 「加载更早的消息」每页条数。 */
export const HISTORY_PAGE_SIZE = 60;

export const getSessionMessages = async (
  id: string,
  opts?: { limit?: number; tailSkip?: number },
): Promise<SessionMessagesResult> => {
  const params = new URLSearchParams();
  if (opts?.limit && opts.limit > 0) params.set("limit", String(opts.limit));
  if (opts?.tailSkip && opts.tailSkip > 0) params.set("tail_skip", String(opts.tailSkip));
  const qs = params.toString();
  const res = await fetchJSON<{
    messages: WireMessage[];
    has_more?: boolean;
    total_messages?: number;
    context_usage?: {
      used_tokens: number;
      max_tokens: number;
      threshold_tokens: number;
      percent: number;
    } | null;
  }>(
    `/v1/sessions/${encodeURIComponent(id)}/messages${qs ? `?${qs}` : ""}`
  );
  const messages = wireArrayToChatMessages(res.messages, id, detectToolError);
  let contextUsage: RestoredContextUsage | null = null;
  if (res.context_usage) {
    contextUsage = {
      usedTokens: res.context_usage.used_tokens,
      maxTokens: res.context_usage.max_tokens,
      thresholdTokens: res.context_usage.threshold_tokens,
      percent: res.context_usage.percent,
    };
  }
  return {
    messages,
    contextUsage,
    hasMore: res.has_more ?? false,
    totalMessages: res.total_messages ?? messages.length,
  };
};

// sendMessage（REST 版 POST /v1/sessions/{id}/messages）于 2026-10-18
// 移除：消息发送已全面走 stream:<sid> WS 通道（useStreamChat），REST
// 封装零调用（App.tsx 仅注释提及）。后端端点保留给移动端面。

/** 持久化前端构建的 unified_steps 时间线到最近一条 assistant 消息。
 *  流结束后 fire-and-forget，失败静默（不影响 UI / 不阻塞对话）。
 *  后端把它写进 messages.unified_steps 列，reload 时原样回传，
 *  使前端 resolveSteps 走真实内联顺序而非 buildFallbackSteps 的
 *  固定 reasoning→tools→text 顺序（工具不再被归到末尾）。 */
export const saveUnifiedSteps = async (
  id: string,
  steps: unknown[]
): Promise<void> => {
  try {
    await fetchJSON(
      `/v1/sessions/${encodeURIComponent(id)}/messages/unified_steps`,
      {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ unified_steps: steps }),
      }
    );
  } catch {
    // 持久化失败不影响对话：reload 时走 buildFallbackSteps 兜底。
  }
};

/** 切换模型时预热对应 provider，消除首请求冷启动延迟。
 *  fire-and-forget，失败静默（不影响 UI）。 */
export const warmUpProvider = async (model: string): Promise<void> => {
  try {
    await fetchJSON("/v1/llm/warmup", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ model }),
    });
  } catch {
    // 预热失败不阻塞 UI；后端在首次真实对话时仍会自行建立连接。
  }
};

// ── Dashboard API ──────────────────────────────────

export const getConfig = async (): Promise<HermesConfig> =>
  fetchJSON("/api/config");

export const updateConfig = async (config: HermesConfig): Promise<boolean> => {
  await fetchJSON("/api/config", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ config }),
  });
  return true;
};

export interface BackupStatus {
  enabled: boolean;
  dir: string | null;
  total_bytes: number;
  file_count: number;
  last_backup: string | null;
}

export const getBackupStatus = async (): Promise<BackupStatus | null> => {
  try {
    return await fetchJSON<BackupStatus>("/api/backup/status");
  } catch {
    return null;
  }
};

export const getModelList = async (): Promise<ModelListResponse | null> => {
  try {
    return await fetchJSON<ModelListResponse>("/api/model/list");
  } catch {
    return null;
  }
};

/** 当前模型的元信息（上下文窗口 + 能力位）。`capabilities.supports_reasoning`
 *  来自后端静态目录 `model_metadata::known_models()`；模型未收录时后端下发
 *  `false`（卡片据此隐藏思考开关键，不显假能力）。 */
export const getModelInfo = async (): Promise<{
  model: string;
  provider: string;
  effective_context_length: number;
  capabilities: {
    supports_tools: boolean;
    supports_vision: boolean;
    supports_reasoning: boolean;
    context_window: number;
  };
} | null> => {
  try {
    return await fetchJSON<{
      model: string;
      provider: string;
      effective_context_length: number;
      capabilities: {
        supports_tools: boolean;
        supports_vision: boolean;
        supports_reasoning: boolean;
        context_window: number;
      };
    }>("/api/model/info");
  } catch {
    return null;
  }
};

export const saveModelList = async (
  models: ModelListEntry[],
  defaultModel: string | null,
): Promise<void> => {
  try {
    await fetchJSON("/api/model/list", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ models, default_model: defaultModel }),
    });
  } catch (e) {
    console.warn("saveModelList failed:", e);
  }
};

export const getSkills = async (): Promise<SkillInfo[]> =>
  fetchJSON("/api/skills");

export const toggleSkill = async (name: string, enabled: boolean): Promise<boolean> => {
  await fetchJSON("/api/skills/toggle", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name, enabled }),
  });
  return true;
};

export const getSkillContent = async (name: string): Promise<SkillContent | null> => {
  try {
    return await fetchJSON(`/api/skills/${encodeURIComponent(name)}`);
  } catch {
    return null;
  }
};

export const getToolsets = async (): Promise<ToolsetInfo[]> =>
  fetchJSON("/api/tools/toolsets");

export const updateEnabledToolsets = async (
  enabled: string[],
): Promise<ToolsetToggleResult> =>
  fetchJSON<ToolsetToggleResult>("/api/tools/toolsets/enabled", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ enabled }),
  });

export const getMcpCatalog = async (): Promise<CatalogResponse | null> => {
  try {
    const res = await fetchJSON<{
      items: Array<{
        id: string; title: string; description: string; version?: string; homepage?: string;
        official: boolean; required_env: string[]; install: CatalogInstall;
        title_zh?: string; description_zh?: string; category: string;
        scan: { verdict: string; label: string; hits: CatalogScanHit[]; command_preview?: string };
      }>;
    }>("/api/mcp/catalog");
    return {
      items: res.items.map((r) => ({
        id: r.id,
        title: r.title,
        description: r.description,
        version: r.version,
        homepage: r.homepage,
        official: r.official,
        install: r.install,
        requiredEnv: r.required_env ?? [],
        titleZh: r.title_zh,
        descriptionZh: r.description_zh,
        category: r.category,
        scan: {
          verdict: r.scan.verdict as CatalogVerdict,
          label: r.scan.label,
          hits: r.scan.hits ?? [],
          commandPreview: r.scan.command_preview,
        },
      })),
    };
  } catch {
    return null;
  }
};

/** IM 通道状态：凭据绑定 + 网关连接 + 机器人名（仅本机端点）。 */
export const getChannelStatus = async (): Promise<{ platforms: ChannelStatusItem[] } | null> => {
  try {
    return await fetchJSON<{ platforms: ChannelStatusItem[] }>("/api/channels/status");
  } catch {
    return null;
  }
};

// ── 工作流（对话内保存的再跑流程）────────────────────────────

export const getWorkflows = async (): Promise<WorkflowEntry[] | null> => {
  try {
    const res = await fetchJSON<{ items: WorkflowEntry[] }>("/api/workflows");
    return res.items;
  } catch {
    return null;
  }
};

export const saveWorkflow = async (wf: {
  id?: string;
  name: string;
  description?: string;
  workspace?: string | null;
  prompt: string;
}): Promise<WorkflowEntry | null> => {
  try {
    const res = await fetchJSON<{ ok: boolean; workflow: WorkflowEntry }>("/api/workflows", {
      method: "POST",
      body: JSON.stringify(wf),
    });
    return res?.workflow ?? null;
  } catch {
    return null;
  }
};

export const removeWorkflow = async (id: string): Promise<boolean> => {
  try {
    await fetchJSON(`/api/workflows/${encodeURIComponent(id)}`, { method: "DELETE" });
    return true;
  } catch {
    return false;
  }
};

/** 再跑一次。params 替换 prompt 里的 {{参数}} 占位符。 */
export const runWorkflow = async (
  id: string,
  params: Record<string, string> = {},
): Promise<boolean> => {
  try {
    await fetchJSON(`/api/workflows/${encodeURIComponent(id)}/run`, {
      method: "POST",
      body: JSON.stringify({ params }),
    });
    return true;
  } catch {
    return false;
  }
};

/** 从目录安装。只传 id + 可选 env；命令/URL 服务端从目录解析。
 *  `acknowledge` 用于「需确认」档的二次放行（后端 428 时带 true 重发）。
 *  返回失败原因（已中文化），由调用方展示。 */
function isoToMs(iso: string | undefined): number {
  if (!iso) return 0;
  const t = Date.parse(iso);
  return Number.isFinite(t) ? t : 0;
}

function fromBackend(entry: { id: string; name: string; transport: string; command?: string; args?: string[]; url?: string; token?: string; env?: Record<string, string>; enabled?: boolean; created_at?: string; description?: string }): McpServerEntry {
  return {
    id: entry.id,
    name: entry.name,
    transport: (entry.transport === "http" ? "http" : "stdio") as McpTransport,
    command: entry.command,
    args: entry.args,
    url: entry.url,
    token: entry.token,
    env: entry.env,
    enabled: entry.enabled ?? true,
    createdAt: isoToMs(entry.created_at),
    description: entry.description,
  };
}

function toBackend(entry: Omit<McpServerEntry, "id" | "createdAt">) {
  return {
    name: entry.name,
    transport: entry.transport,
    command: entry.command,
    args: entry.args,
    url: entry.url,
    token: entry.token,
    env: entry.env,
    enabled: entry.enabled,
    description: (entry as McpServerEntry).description,
  };
}

interface McpServerListResponse {
  servers: Array<{
    id: string;
    name: string;
    transport: string;
    command?: string;
    args?: string[];
    url?: string;
    token?: string;
    env?: Record<string, string>;
    enabled?: boolean;
    created_at?: string;
    description?: string;
  }>;
  seeded?: boolean;
}


export const installMcpFromCatalog = async (
  id: string,
  env: Record<string, string>,
  acknowledge: boolean,
): Promise<{ ok: true; entry: McpServerEntry } | { ok: false; error: string; needAck: boolean }> => {
  try {
    const created = await fetchJSON<{ id: string; name: string; transport: string; command?: string; args?: string[]; url?: string; env?: Record<string, string>; enabled?: boolean; created_at?: string; description?: string }>(
      "/api/mcp/catalog/install",
      { method: "POST", body: JSON.stringify({ id, env, acknowledge }) },
    );
    return { ok: true, entry: fromBackend(created) };
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    return { ok: false, error: msg, needAck: /需要确认后安装|428/.test(msg) };
  }
};

const MCP_SERVERS_KEY = STORAGE_KEYS.mcpServers;
const OFFICIAL_MCP_SEEDS: McpServerEntry[] = [
  {
    id: "official-filesystem",
    name: "Filesystem（官方示例）",
    description: "读取/写入本地文件系统的 MCP 服务（stdio 启动）",
    transport: "stdio",
    command: "npx",
    args: ["-y", "@modelcontextprotocol/server-filesystem", "~"],
    enabled: false,
    createdAt: 0,
  },
  {
    id: "official-fetch",
    name: "Fetch（官方示例）",
    description: "通过 HTTP 抓取网页并把 HTML/Markdown 返回给模型",
    transport: "stdio",
    command: "npx",
    args: ["-y", "@modelcontextprotocol/server-fetch"],
    enabled: false,
    createdAt: 0,
  },
  {
    id: "official-git",
    name: "Git（官方示例）",
    description: "封装常用 git 命令(read_git_status/diff/log/commit 等)",
    transport: "stdio",
    command: "uvx",
    args: ["mcp-server-git", "--repository", "."],
    enabled: false,
    createdAt: 0,
  },
  {
    id: "official-github",
    name: "GitHub（官方示例）",
    description: "查询 issue / PR / 仓库元数据（需要在环境变量注入 GITHUB_TOKEN）",
    transport: "stdio",
    command: "npx",
    args: ["-y", "@modelcontextprotocol/server-github"],
    env: { GITHUB_TOKEN: "" },
    enabled: false,
    createdAt: 0,
  },
  {
    id: "official-brave-search",
    name: "Brave Search（官方示例）",
    description: "通过 Brave 搜索 API 返回结果(需要 BRAVE_API_KEY)",
    transport: "stdio",
    command: "npx",
    args: ["-y", "@modelcontextprotocol/server-brave-search"],
    env: { BRAVE_API_KEY: "" },
    enabled: false,
    createdAt: 0,
  },
];

// ── MCP server CRUD (backend-first, localStorage fallback) ─────────

function loadMcpServersCache(): McpServerEntry[] {
  try {
    const raw = storage.getItem(MCP_SERVERS_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as McpServerEntry[];
      if (Array.isArray(parsed)) return parsed;
    }
  } catch {}
  return OFFICIAL_MCP_SEEDS.map((s) => ({ ...s, createdAt: Date.now() }));
}
function saveMcpServersCache(list: McpServerEntry[]) {
  try {
    storage.setItem(MCP_SERVERS_KEY, JSON.stringify(list));
  } catch {
    // localStorage disabled / quota — non-fatal, the backend still has truth
  }
}
export const getMcpServers = async (): Promise<McpServerEntry[]> => {
  try {
    const res = await fetchJSON<McpServerListResponse>("/api/mcp/servers");
    return res.servers.map(fromBackend);
  } catch {
    return loadMcpServersCache();
  }
};
export const addMcpServer = async (entry: Omit<McpServerEntry, "id" | "createdAt">): Promise<McpServerEntry> => {
  try {
    const res = await fetchJSON<{ id: string; name: string; transport: string; command?: string; args?: string[]; url?: string; token?: string; env?: Record<string, string>; enabled?: boolean; created_at?: string; description?: string }>(
      "/api/mcp/servers",
      { method: "POST", body: JSON.stringify(toBackend(entry)) },
    );
    const created = fromBackend(res);
    return created;
  } catch {
    const list = loadMcpServersCache();
    const full: McpServerEntry = {
      ...entry,
      id: `mcp-local-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`,
      createdAt: Date.now(),
    };
    list.push(full);
    saveMcpServersCache(list);
    return full;
  }
};
export const updateMcpServer = async (id: string, patch: Partial<McpServerEntry>): Promise<McpServerEntry | null> => {
  try {
    const res = await fetchJSON<{ id: string; name: string; transport: string; command?: string; args?: string[]; url?: string; token?: string; env?: Record<string, string>; enabled?: boolean; created_at?: string; description?: string }>(
      `/api/mcp/servers/${encodeURIComponent(id)}`,
      { method: "PUT", body: JSON.stringify(patch) },
    );
    const updated = fromBackend(res);
    return updated;
  } catch {
    const list = loadMcpServersCache();
    const idx = list.findIndex((s) => s.id === id);
    if (idx < 0) return null;
    list[idx] = { ...list[idx], ...patch };
    saveMcpServersCache(list);
    return list[idx];
  }
};
export const deleteMcpServer = async (id: string): Promise<boolean> => {
  try {
    await fetchJSON<unknown>(`/api/mcp/servers/${encodeURIComponent(id)}`, {
      method: "DELETE",
    });
    const list = loadMcpServersCache();
    const next = list.filter((s) => s.id !== id);
    saveMcpServersCache(next);
    return next.length !== list.length;
  } catch {
    const list = loadMcpServersCache();
    const next = list.filter((s) => s.id !== id);
    saveMcpServersCache(next);
    return next.length !== list.length;
  }
};

export const getSkillsList = async (): Promise<Array<{ name: string; description: string; category: string }>> => {
  try {
    return await fetchJSON<Array<{ name: string; description: string; category: string }>>("/api/skills");
  } catch {
    return [];
  }
};

export const getAutomationTasks = async (): Promise<AutomationTask[]> => {
  try {
    const res = await fetchJSON<{ tasks: AutomationTask[]; seeded: boolean }>(
      "/api/automation/tasks",
    );
    return res.tasks ?? [];
  } catch {
    return [];
  }
};

/**
 * Number of automation runs currently in flight.
 *
 * Feeds the keep-awake signal: a background automation runs with no session
 * the UI is watching, so its activity is invisible from the front-end's own
 * streaming state. Returns 0 when the backend is unreachable — the caller
 * treats that as "nothing running" rather than blocking on it.
 */
export const getActiveAutomationRuns = async (): Promise<number> => {
  try {
    const res = await fetchJSON<{ active: number }>("/api/automation/active-count");
    return res.active ?? 0;
  } catch {
    return 0;
  }
};

export const createAutomationTask = async (
  draft: Omit<AutomationTask, "id" | "run_count" | "history" | "created_at" | "updated_at" | "last_run_at" | "last_status" | "last_error" | "next_run_at">,
): Promise<AutomationTask | null> => {
  try {
    return await fetchJSON<AutomationTask>("/api/automation/tasks", {
      method: "POST",
      body: JSON.stringify({ ...draft, history: [] }),
    });
  } catch (e) {
    console.error("createAutomationTask failed", e);
    return null;
  }
};

export const updateAutomationTask = async (
  id: string,
  patch: Partial<AutomationTask>,
): Promise<AutomationTask | null> => {
  try {
    return await fetchJSON<AutomationTask>(
      `/api/automation/tasks/${encodeURIComponent(id)}`,
      { method: "PUT", body: JSON.stringify(patch) },
    );
  } catch (e) {
    console.error("updateAutomationTask failed", e);
    return null;
  }
};

export const deleteAutomationTask = async (id: string): Promise<boolean> => {
  try {
    await fetchJSON<unknown>(
      `/api/automation/tasks/${encodeURIComponent(id)}`,
      { method: "DELETE" },
    );
    return true;
  } catch {
    return false;
  }
};

export const triggerAutomationTask = async (id: string): Promise<boolean> => {
  try {
    await fetchJSON<unknown>(
      `/api/automation/tasks/${encodeURIComponent(id)}/trigger`,
      { method: "POST" },
    );
    return true;
  } catch {
    return false;
  }
};

/**
 * 手机上是否有回合正在跑（决定要不要阻止电脑休眠）。
 *
 * 取不到就返回 `false`：朝向「允许休眠」失败。反过来的失败模式（探测挂了
 * 就一直不让电脑睡）没人会发现，而前者最多是这一轮没保住。
 */
export const getMobileActivity = async (): Promise<boolean> => {
  try {
    const payload = await fetchJSON<{ active?: boolean }>("/api/dashboard/mobile-activity");
    return payload.active === true;
  } catch {
    return false;
  }
};

/**
 * 上报窗口注意力（审批超时按「可送达时间」计费靠它）。
 *
 * 必须由**知道自己在前台的那一端**主动说：后端把请求流量当在场的话，
 * 桌面端每 10 秒的防休眠轮询会把「进程活着」误当成「人在看」。
 * 失败静默——上报丢一次只影响一次判定，心跳会补上。
 */
export const postPresence = async (active: boolean): Promise<void> => {
  try {
    await fetchJSON<unknown>("/api/dashboard/presence", {
      method: "POST",
      body: JSON.stringify({ active }),
    });
  } catch {
    // 不打扰用户：下一次心跳（或失焦时的 false）会纠正。
  }
};

export const getPluginSettings = async (): Promise<PluginSettings> => {
  try {
    const payload = await fetchJSON<{ plugins?: PluginSettings }>("/api/dashboard/plugins");
    return payload.plugins ?? { mcp_filesystem: true, mcp_terminal: true, mcp_database: false, tool_code_exec: true };
  } catch {
    return { mcp_filesystem: true, mcp_terminal: true, mcp_database: false, tool_code_exec: true };
  }
};

export const updatePluginSettings = async (plugins: PluginSettings): Promise<PluginSettings> => {
  try {
    const payload = await fetchJSON<{ plugins?: PluginSettings }>("/api/dashboard/plugins", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ plugins }),
    });
    return payload.plugins ?? plugins;
  } catch {
    return plugins;
  }
};

// ── Environment Variables (API Keys) ──────────────

export const getEnvVars = async (): Promise<Record<string, { is_set: boolean; redacted_value?: string; description?: string; url?: string }>> =>
  fetchJSON("/api/env");

export const setEnvVar = async (key: string, value: string): Promise<boolean> => {
  await fetchJSON("/api/env", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ key, value }),
  });
  return true;
};

export const autoCorrectConfig = async (): Promise<AutoCorrectResponse> =>
  fetchJSON("/api/config/auto-correct", { method: "POST" });

/** Cheap liveness check used by `BackendStatus` and the splash screen.
 *  Goes through the bridge's `backend_ready` command — that one is a
 *  synchronous `bool` that does NOT round-trip a frame, so it's safe
 *  to call on a render loop without poisoning the request_id table. */
export const checkBackendHealth = async (): Promise<boolean> => {
  try {
    return await invoke<boolean>("backend_ready");
  } catch {
    return false;
  }
};

/** 轮询 `checkBackendHealth` 直到后端应答 `/health`（超时返回 false）。
 *
 *  用途：首屏 hydration 请求（`getAvailableTools` / `getSessionMessages`）
 *  发出前先等后端就绪。dev / 冷启动时前端 effect 会早于 Rust 侧
 *  `stdio_bridge::init_global`（sidecar spawn 成功即置 `BRIDGE_LIVE=true`）
 *  触发，此刻经 stdio 桥的请求直接吃到 503 "backend offline" —— 既刷
 *  console，又让首屏数据空跑且不自动重试（`getAvailableTools` 仅在 window
 *  focus 时重试）。等到真正 ready 再发，请求一次成功、无噪音；超时后照常
 *  发出，交由既有错误分支兜底（连接指示灯此时已显示离线，用户不盲）。
 *  预算是 90s：冷启动实测可达数十秒，短预算会把慢启动误判成失败。 */
export async function waitForBackendReady(
  opts: { timeoutMs?: number; intervalMs?: number } = {},
): Promise<boolean> {
  const timeoutMs = opts.timeoutMs ?? 90_000;
  const intervalMs = opts.intervalMs ?? 1_000;
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await checkBackendHealth()) return true;
    await new Promise((r) => setTimeout(r, intervalMs));
  }
  return false;
}

// ── Analytics / Usage ────────────────────────────────────────

export const getAnalytics = async (days = 30): Promise<AnalyticsResponse> =>
  fetchJSON(`/api/analytics/usage?days=${days}`);

export const getAnalyticsDebug = async (): Promise<AnalyticsDebugResponse> =>
  fetchJSON(`/api/analytics/debug`);

// ── Expert catalog ──────────────────────────────────────────

export const fetchExperts = async (): Promise<RemoteExpert[]> => {
  const res = await fetchJSON<{ experts: RemoteExpert[] }>("/v1/experts");
  return res.experts ?? [];
};

export const registerExperts = async (
  experts: RemoteExpert[],
): Promise<{ inserted: number; total: number }> => {
  return await fetchJSON<{ inserted: number; total: number }>("/v1/experts", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ experts }),
  });
};

export const getCustomProviders = async (): Promise<CustomProviderSummary[]> =>
  fetchJSON<CustomProviderSummary[]>("/api/custom-providers");

export const createCustomProvider = async (
  body: CustomProviderCreateRequest,
): Promise<boolean> => {
  await fetchJSON("/api/custom-providers", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  return true;
};

export const testCustomProviderConnection = async (
  id: string,
): Promise<CustomProviderTestConnectionResponse> =>
  fetchJSON<CustomProviderTestConnectionResponse>(
    `/api/custom-providers/${encodeURIComponent(id)}/test-connection`,
    { method: "POST" },
  );

export interface InlineTestResponse {
  ok: boolean;
  /** apikey / quota / network / provider / bad_request — 前端按类给中文提示。 */
  kind: string | null;
  status: number;
  latency_ms: number;
  error: string | null;
  models: string[] | null;
  model_found: boolean | null;
}

/** 用还没落盘的配置试连通：添加模型弹窗里"填完就测"，不必先建 provider。
 *
 *  `providerId` 用于**内置供应商**：它们的密钥只存在于后端环境变量，前端
 *  读不到。给了它，后端改由 `env_var_name_for_provider` 取密钥，`apiKey`
 *  被忽略——密钥自始至终不过网。base_url 仍由前端传（前端自带映射表，且它
 *  不是机密）。 */
export const testProviderInline = async (params: {
  baseUrl: string;
  apiKey: string;
  model?: string;
  /** 内置 provider id；给了就由后端从 env 取密钥。 */
  providerId?: string;
}): Promise<InlineTestResponse> =>
  fetchJSON<InlineTestResponse>("/api/providers/test", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      base_url: params.baseUrl,
      api_key: params.apiKey,
      model: params.model ?? null,
      provider_id: params.providerId ?? null,
    }),
  });

export const getTickLoopStatus = async (): Promise<TickLoopStatus> =>
  fetchJSON<TickLoopStatus>("/v1/tickloop");

export const toggleTickLoop = async (enabled: boolean): Promise<TickLoopStatus> =>
  fetchJSON<TickLoopStatus>("/v1/tickloop", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ enabled }),
  });

// ── Memory management API（记忆管理设置页）────────────

export interface MemoryGraphNode {
  id: string;
  /** fact | turn | memory_tool | session | delegation | compression_backfill | focus | reflection | daily | md */
  kind: string;
  label: string;
  preview: string;
  salience: number;
  created_at: string;
  session_id?: string;
  pinned: boolean;
}

export interface MemoryGraphEdge {
  source: string;
  target: string;
  relation: string;
}

export interface MemoryOverview {
  frames: number;
  facts: number;
  reflections: number;
  daily_days: number;
  md_entries: number;
  db_bytes: number;
  per_day: Array<[string, number]>;
}

export interface MemoryGraphResponse {
  nodes: MemoryGraphNode[];
  edges: MemoryGraphEdge[];
  stats: MemoryOverview;
  truncated: boolean;
}

export interface MemoryItemResponse {
  id: string;
  kind: string;
  content: string;
  created_at: string;
  salience: number;
  session_id?: string;
  file?: string;
}

export interface DailyMemoryDay {
  date: string;
  goals: string;
  progress: string;
  conclusions: string;
  updated_at: number;
}

export const getMemoryGraph = async (maxNodes = 400): Promise<MemoryGraphResponse> =>
  fetchJSON(`/api/memory/graph?max_nodes=${maxNodes}`);

export const getMemoryItem = async (kind: string, id: string): Promise<MemoryItemResponse> =>
  fetchJSON(`/api/memory/item?kind=${encodeURIComponent(kind)}&id=${encodeURIComponent(id)}`);

export const deleteMemoryItem = async (kind: string, id: string): Promise<boolean> => {
  await fetchJSON(`/api/memory/item?kind=${encodeURIComponent(kind)}&id=${encodeURIComponent(id)}`, {
    method: "DELETE",
  });
  return true;
};

export const searchMemory = async (q: string, limit = 30): Promise<{ results: MemoryGraphNode[] }> =>
  fetchJSON(`/api/memory/search?q=${encodeURIComponent(q)}&limit=${limit}`);

// getMemoryOverview（GET /api/memory/overview）于 2026-10-18 移除：
// 与 getMemoryGraph().stats 同一数据源（后端同一 overview() 函数），
// 记忆统计行走 graph.stats，封装零调用。

export const pinMemory = async (id: string, pinned: boolean): Promise<void> => {
  await fetchJSON("/api/memory/pin", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ id, pinned }),
  });
};

export const getDailyMemoryList = async (limit = 60): Promise<DailyMemoryDay[]> =>
  fetchJSON(`/api/memory/daily?limit=${limit}`);

// ── Persona API（角色提示词 / SOUL / 全局记忆文本）────────────

export interface PersonalitySummary {
  name: string;
  summary: string | null;
  size: number;
  updated_at: number | null;
  is_builtin: boolean;
}

export interface PersonaTextFile {
  path: string;
  content: string | null;
  updated_at: number | null;
}

export const listPersonalities = async (): Promise<PersonalitySummary[]> =>
  fetchJSON("/api/persona/personalities");

export const getPersonality = async (name: string): Promise<PersonaTextFile> =>
  fetchJSON(`/api/persona/personalities/${encodeURIComponent(name)}`);

export const savePersonality = async (name: string, content: string): Promise<PersonaTextFile> => {
  await fetchJSON(`/api/persona/personalities/${encodeURIComponent(name)}`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ content }),
  });
  return fetchJSON(`/api/persona/personalities/${encodeURIComponent(name)}`);
};

export const removePersonality = async (name: string): Promise<void> => {
  await fetchJSON(`/api/persona/personalities/${encodeURIComponent(name)}`, { method: "DELETE" });
};

export const getPersonaSoul = async (): Promise<PersonaTextFile> =>
  fetchJSON("/api/persona/soul");

export const savePersonaSoul = async (content: string): Promise<void> => {
  await fetchJSON("/api/persona/soul", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ content }),
  });
};

export const getPersonaMemory = async (): Promise<PersonaTextFile> =>
  fetchJSON("/api/persona/memory");

export const savePersonaMemory = async (content: string): Promise<void> => {
  await fetchJSON("/api/persona/memory", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ content }),
  });
};

export const getPersonaUser = async (): Promise<PersonaTextFile> =>
  fetchJSON("/api/persona/user");

export const savePersonaUser = async (content: string): Promise<void> => {
  await fetchJSON("/api/persona/user", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ content }),
  });
};
