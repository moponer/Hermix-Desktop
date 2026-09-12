// Hermix Desktop API Client — REST 运行时（从 api.ts 拆分；类型见 ./types，WS 见 ./ws）
import type { ChatMessage, Session, AutomationTask } from "../types";
import type { WorkspaceEntry, WorkspaceDetail, WorkspaceSchema, TemplateMeta } from "../types";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";
// v5: reuse detectToolError from messageParser for errorReason extraction
// on reloaded tool messages — same helper as the live-streaming path.
import { detectToolError } from "../components/messageParser";
import { wireArrayToChatMessages, type WireMessage } from "../protocolAdapter";
import type {
  ArchiveRef,
  Automation,
  AutomationRun,
  AutoCorrectResponse,
  CreateAutomationRequest,
  CreateTaskRequest,
  CustomProviderCreateRequest,
  CustomProviderFetchModelsResponse,
  CustomProviderSummary,
  CustomProviderTestConnectionResponse,
  GroupChatActiveTaskResponse,
  GroupChatAnnouncementResponse,
  GroupChatDescriptionResponse,
  GroupChatMember,
  GroupChatMessage,
  GroupChatRank,
  GroupChatRoom,
  GroupChatRoomRules,
  GroupChatRoomSettings,
  GroupChatSubtaskStatus,
  GroupChatTask,
  HermesConfig,
  McpServerEntry,
  McpTransport,
  ModelInfo,
  ModelListEntry,
  ModelListResponse,
  PersonaOverrides,
  PersonaTextFile,
  PersonalitySummary,
  PluginInspect,
  PluginManifest,
  PluginSettings,
  RemoteExpert,
  RestoredContextUsage,
  SendMessageResponse,
  SessionMessagesResult,
  SessionSummary,
  SkillContent,
  SkillInfo,
  SystemStatus,
  Task,
  TaskStatus,
  TickLoopStatus,
  ToolDescriptor,
  ToolsetInfo,
  ToolsetToggleResult,
  AnalyticsResponse,
  AnalyticsDebugResponse,
} from "./types";

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
 *  timeout; we add a 10s client-side `AbortSignal` so a hung request
 *  can be cancelled from the React side without waiting for the
 *  bridge to give up. */
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
  const raw = await invoke<string>("backend_http_request", {
    method,
    path,
    body: payload,
  });
  if (combined.aborted) {
    throw new DOMException("request aborted", "AbortError");
  }

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
  const reply = await apiInvoke(method, path, body, init?.signal ?? undefined);
  if (reply.status < 200 || reply.status >= 300) {
    const message = reply.enc === "utf8"
      ? (() => {
          try {
            const parsed = JSON.parse(reply.body) as { message?: unknown };
            return typeof parsed.message === "string" ? parsed.message : reply.body;
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
  try {
    return JSON.parse(reply.body) as T;
  } catch (e) {
    throw new Error(`fetchJSON: malformed JSON body: ${String(e)}`);
  }
}

/** Binary fetch — used by `exportWorkspace` to receive the .hwx
 *  archive bytes without going through JSON. */
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
export function isCircuitTripped(): boolean {
  return false;
}

// ── Workspaces（工作台）───────────────────────────────
// 镜像后端 dashboard/workspaces.rs 的 /api/workspaces 端点。

/** 列出全部工作台（注册表条目，updated 倒序）。 */
export async function listWorkspaces(): Promise<WorkspaceEntry[]> {
  return fetchJSON<WorkspaceEntry[]>("/api/workspaces");
}

/** 创建工作台（建注册表条目 + 目录 + 默认定义）。 */
export async function createWorkspace(
  name: string,
  icon = "",
): Promise<{ ok: boolean; id: string }> {
  return fetchJSON<{ ok: boolean; id: string }>("/api/workspaces", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name, icon }),
  });
}

// ── Workspace Templates（内置模板，M6）─────────────────────

/** 内置模板列表（元信息 + 表清单，不含 schema 全文）。 */
export async function listWorkspaceTemplates(): Promise<TemplateMeta[]> {
  return fetchJSON<TemplateMeta[]>("/api/workspace-templates");
}

/** 按模板一键建工作台（返回新工作台 id + 自测摘要）。 */
export async function createWorkspaceFromTemplate(
  templateId: string,
): Promise<{ ok: boolean; id: string; name: string; smoke: string[] }> {
  return fetchJSON<{ ok: boolean; id: string; name: string; smoke: string[] }>(
    `/api/workspace-templates/${encodeURIComponent(templateId)}/create`,
    { method: "POST" },
  );
}

/** 仅替换布局层（拖拽保存专用，避免整 schema 快照覆盖 LLM 并发修改）。 */
export async function updateWorkspaceLayout(
  id: string,
  layout: unknown,
): Promise<{ ok: boolean; id: string }> {
  return fetchJSON<{ ok: boolean; id: string }>(
    `/api/workspaces/${encodeURIComponent(id)}/layout`,
    {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ layout }),
    },
  );
}

/** 内置 provider 元数据（id + 配钥环境变量名 + 权威默认模型）——分组/提示/自动选型的唯一真理源。 */
export interface ProviderMeta {
  id: string;
  env_var: string;
  /** 后端 `default_model_for_provider` 的人工清单；未收录的 provider 为 null。 */
  default_model?: string | null;
}

export async function getProviderMeta(): Promise<ProviderMeta[]> {
  return fetchJSON<ProviderMeta[]>("/api/providers/meta");
}

/** 各内置 provider 的权威默认模型（id → model）。首次配置时用来选中一个
 *  真正能对话的模型，而不是厂商 `/v1/models` 全量目录里字典序第一个。 */
export async function getProviderDefaults(): Promise<Record<string, string>> {
  const list = await getProviderMeta();
  const out: Record<string, string> = {};
  for (const m of list) {
    if (m.default_model) out[m.id] = m.default_model;
  }
  return out;
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

/** 获取工作台详情（注册表 + schema）。 */
export async function getWorkspace(id: string): Promise<WorkspaceDetail> {
  return fetchJSON<WorkspaceDetail>(`/api/workspaces/${encodeURIComponent(id)}`);
}

/** 重命名工作台（注册表 + 定义同步）。 */
export async function renameWorkspace(
  id: string,
  name: string,
  icon = "",
): Promise<{ ok: boolean; id: string }> {
  return fetchJSON<{ ok: boolean; id: string }>(
    `/api/workspaces/${encodeURIComponent(id)}`,
    {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name, icon }),
    },
  );
}

/** 删除工作台（注册表条目 + 数据目录）。 */
export async function deleteWorkspace(id: string): Promise<{ ok: boolean; id: string }> {
  return fetchJSON<{ ok: boolean; id: string }>(
    `/api/workspaces/${encodeURIComponent(id)}`,
    { method: "DELETE" },
  );
}

/** 更新工作台定义（布局/模块变更持久化，后端校验后落盘）。 */
export async function updateWorkspaceSchema(
  id: string,
  schema: WorkspaceSchema,
): Promise<{ ok: boolean; id: string }> {
  return fetchJSON<{ ok: boolean; id: string }>(
    `/api/workspaces/${encodeURIComponent(id)}/schema`,
    {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(schema),
    },
  );
}

// ── 工作台数据 CRUD（打通后端 LocalSqliteStore）──────────────

export interface WorkspaceQueryRequest {
  filters?: WorkspaceFilter[];
  sort?: string;
  /** 0 = 不限；前端默认给上限防大表拖垮。 */
  limit?: number;
  offset?: number;
  /** 关键字搜索（P1）：非空时跨全部文本类字段 OR LIKE（store.search_text）。 */
  keyword?: string;
}

export interface WorkspaceFilter {
  field: string;
  op: "eq" | "ne" | "gt" | "gte" | "lt" | "lte" | "contains";
  value: unknown;
}

/** 查询表行。 */
export async function workspaceQueryTable(
  workspaceId: string,
  table: string,
  req: WorkspaceQueryRequest = {},
): Promise<Record<string, unknown>[]> {
  return fetchJSON<Record<string, unknown>[]>(
    `/api/workspaces/${encodeURIComponent(workspaceId)}/tables/${encodeURIComponent(table)}/query`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(req),
    },
  );
}

/** 插入行，返回行 id。 */
export async function workspaceInsertRows(
  workspaceId: string,
  table: string,
  rows: Record<string, unknown>[],
  updatedBy = "user",
): Promise<string[]> {
  return fetchJSON<string[]>(
    `/api/workspaces/${encodeURIComponent(workspaceId)}/tables/${encodeURIComponent(table)}/insert`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ rows, updated_by: updatedBy }),
    },
  );
}

/** 按主键更新行。 */
export async function workspaceUpdateRow(
  workspaceId: string,
  table: string,
  id: string,
  patch: Record<string, unknown>,
  updatedBy = "user",
): Promise<{ ok: boolean; id: string }> {
  return fetchJSON<{ ok: boolean; id: string }>(
    `/api/workspaces/${encodeURIComponent(workspaceId)}/tables/${encodeURIComponent(table)}/update`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ id, patch, updated_by: updatedBy }),
    },
  );
}

/** 按主键删除行。 */
export async function workspaceDeleteRow(
  workspaceId: string,
  table: string,
  id: string,
): Promise<{ ok: boolean; id: string }> {
  return fetchJSON<{ ok: boolean; id: string }>(
    `/api/workspaces/${encodeURIComponent(workspaceId)}/tables/${encodeURIComponent(table)}/delete`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ id }),
    },
  );
}

/** 计数（可带 filters 或 keyword——搜索口径计数走 count_search_text）。 */
export async function workspaceCountRows(
  workspaceId: string,
  table: string,
  filters: WorkspaceFilter[] = [],
  keyword?: string,
): Promise<number> {
  return fetchJSON<number>(
    `/api/workspaces/${encodeURIComponent(workspaceId)}/tables/${encodeURIComponent(table)}/count`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ filters, keyword: keyword || undefined }),
    },
  );
}

/** 分组计数（仪表盘 chart 组件用，列名由后端 schema 白名单校验）。 */
export async function workspaceGroupCount(
  workspaceId: string,
  table: string,
  groupBy: string,
  limit?: number,
): Promise<{ groups: { label: string; count: number }[] }> {
  return fetchJSON<{ groups: { label: string; count: number }[] }>(
    `/api/workspaces/${encodeURIComponent(workspaceId)}/tables/${encodeURIComponent(table)}/group_count`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ group_by: groupBy, limit }),
    },
  );
}

// ── 工作台导出 / 导入（M5 协作分发，P2 前端 UI）──────────────

/** 导出工作台为 .hwx 包（GET export → blob，触发浏览器下载）。
 *  Binary path goes through the stdio bridge's `base64` envelope —
 *  the server encodes the .hwx archive as base64 and we decode it
 *  back into a Blob here. No streaming, but .hwx files are small
 *  (< 50 MB by the upload API's limit) so this is fine. */
export async function exportWorkspace(workspaceId: string): Promise<Blob> {
  return fetchJSONBinary(
    "GET",
    `/api/workspaces/${encodeURIComponent(workspaceId)}/export`,
  );
}

/** 导入 .hwx 包。
 *  - `new`    新建工作台（包内 id 重生成，避免覆盖本机同名）
 *  - `merge`   整表 LWW 合并到同名工作台（每行 updated_at 比对）
 *  - `upsert`  按表 unique 字段逐行 INSERT/UPDATE（适合"定期再导同一份名单"）
 */
export async function importWorkspace(
  pkg: Record<string, unknown>,
  mode: "new" | "merge" | "upsert" = "new",
): Promise<{ ok: boolean; id: string; mode: string; tables: Record<string, number> }> {
  return fetchJSON<{ ok: boolean; id: string; mode: string; tables: Record<string, number> }>(
    "/api/workspaces/import",
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ package: pkg, mode }),
    },
  );
}

// ── 工作台自动化可视化（P3）────────────────────────────

/** 工作台自动化定义列表（schema 内）。 */
export async function listWorkspaceAutomations(
  workspaceId: string,
): Promise<import("../types").WorkspaceAutomation[]> {
  return fetchJSON<import("../types").WorkspaceAutomation[]>(
    `/api/workspaces/${encodeURIComponent(workspaceId)}/automations`,
  );
}

/** 每个自动化的最近触发记录（最多 5 条）。 */
export async function workspaceAutomationRuns(
  workspaceId: string,
): Promise<import("../types").AutomationRunsEntry[]> {
  return fetchJSON<import("../types").AutomationRunsEntry[]>(
    `/api/workspaces/${encodeURIComponent(workspaceId)}/automations/runs`,
  );
}

// ── 附件（P5）──────────────────────────────────────────

/** 上传附件（base64 JSON 通道），返回相对路径（attachment 字段值）。 */
export async function uploadWorkspaceFile(
  workspaceId: string,
  name: string,
  dataBase64: string,
): Promise<{ ok: boolean; path: string }> {
  return fetchJSON<{ ok: boolean; path: string }>(
    `/api/workspaces/${encodeURIComponent(workspaceId)}/files/upload`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name, data_base64: dataBase64 }),
    },
  );
}

/** 附件读取 URL（attachment 字段值为相对路径时拼接完整地址）。
 *  Returns a `stdio://workspace-file/<workspaceId>/<filename>` sentinel
 *  that the document-level image interceptor in `App.tsx` rewrites
 *  into a blob URL once the bridge responds. Keeping the call site
 *  synchronous (the URL is handed straight to `<img src=…>`) avoids
 *  refactoring the cell renderer in `TableView.tsx`. */
export function workspaceFileUrl(workspaceId: string, path: string): string {
  const filename = path.split("/").pop() ?? path;
  return `stdio://workspace-file/${encodeURIComponent(workspaceId)}/${encodeURIComponent(filename)}`;
}

// ── 变更历史 + 撤销（P6）─────────────────────────────

/** 表最近变更（最多 20 条，倒序）。 */
export async function listTableChanges(
  workspaceId: string,
  table: string,
): Promise<import("../types").ChangeEntry[]> {
  return fetchJSON<import("../types").ChangeEntry[]>(
    `/api/workspaces/${encodeURIComponent(workspaceId)}/tables/${encodeURIComponent(table)}/changes`,
  );
}

/** 撤销该表最近一条变更，返回被撤销的记录（无变更时 null）。 */
export async function undoLastChange(
  workspaceId: string,
  table: string,
): Promise<import("../types").ChangeEntry | null> {
  return fetchJSON<import("../types").ChangeEntry | null>(
    `/api/workspaces/${encodeURIComponent(workspaceId)}/tables/${encodeURIComponent(table)}/undo`,
    { method: "POST" },
  );
}

// ── Orchestration Engine: Task CRUD ────────

export async function createTask(
  req: CreateTaskRequest,
): Promise<Task> {
  return fetchJSON<Task>("/v1/tasks", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(req),
  });
}

export async function getTask(taskId: string): Promise<Task> {
  return fetchJSON<Task>(`/v1/tasks/${encodeURIComponent(taskId)}`);
}

export async function listTasks(
  status?: TaskStatus,
): Promise<Task[]> {
  const query = status ? `?status=${encodeURIComponent(status)}` : "";
  return fetchJSON<Task[]>(`/v1/tasks${query}`);
}

export async function updateTaskStatus(
  taskId: string,
  status: TaskStatus,
): Promise<Task> {
  return fetchJSON<Task>(
    `/v1/tasks/${encodeURIComponent(taskId)}/status`,
    {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ status }),
    },
  );
}

export async function listChildTasks(
  parentTaskId: string,
): Promise<Task[]> {
  return fetchJSON<Task[]>(
    `/v1/tasks/${encodeURIComponent(parentTaskId)}/children`,
  );
}

// ── Product Delivery ───────────────────────

export async function deliverResults(
  taskId: string,
  files: string[],
  summary?: string,
): Promise<Task> {
  return fetchJSON<Task>(
    `/v1/tasks/${encodeURIComponent(taskId)}/results`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ files, summary }),
    },
  );
}

// ── Automation Engine ───────────────

export async function createAutomation(
  req: CreateAutomationRequest,
): Promise<Automation> {
  return fetchJSON<Automation>("/v1/automations", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(req),
  });
}

export async function listAutomations(): Promise<Automation[]> {
  return fetchJSON<Automation[]>("/v1/automations");
}

export async function getAutomation(id: string): Promise<Automation> {
  return fetchJSON<Automation>(`/v1/automations/${encodeURIComponent(id)}`);
}

export async function deleteAutomation(id: string): Promise<void> {
  await fetchVoid(`/v1/automations/${encodeURIComponent(id)}`, {
    method: "DELETE",
  });
}

export async function pauseAutomation(id: string): Promise<Automation> {
  return fetchJSON<Automation>(
    `/v1/automations/${encodeURIComponent(id)}/pause`,
    { method: "POST" },
  );
}

export async function resumeAutomation(id: string): Promise<Automation> {
  return fetchJSON<Automation>(
    `/v1/automations/${encodeURIComponent(id)}/resume`,
    { method: "POST" },
  );
}

export async function listAutomationRuns(
  id: string,
): Promise<AutomationRun[]> {
  return fetchJSON<AutomationRun[]>(
    `/v1/automations/${encodeURIComponent(id)}/runs`,
  );
}

export async function triggerAutomation(
  id: string,
): Promise<{ automation_id: string; status: string; message: string }> {
  return fetchJSON(
    `/v1/automations/${encodeURIComponent(id)}/trigger`,
    { method: "POST" },
  );
}

// ── Todo (task list) ─────────────────────────────────────────

export async function patchTodo(
  id: string,
  patch: { status?: string; title?: string; priority?: string },
): Promise<{ ok: boolean; raw?: string }> {
  return fetchJSON(`/v1/todos/${encodeURIComponent(id)}`, {
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
  };
}

export const getSessions = async (): Promise<Session[]> => {
  const res = await fetchJSON<{ sessions: SessionSummary[] }>("/v1/sessions");
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
 * Update session metadata (workspace, expert_id, model).
 * Only the fields present in `patch` are updated.
 */
export const updateSessionMetadata = async (
  id: string,
  patch: { workspace?: string | null; expert_id?: string | null; model?: string | null },
): Promise<void> => {
  await fetchVoid(`/v1/sessions/${encodeURIComponent(id)}`, {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(patch),
  });
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

export const getSessionArchiveRefs = async (
  id: string,
): Promise<ArchiveRef[]> => {
  try {
    const res = await fetchJSON<{ refs: ArchiveRef[] }>(
      `/v1/sessions/${encodeURIComponent(id)}/archive-refs`,
    );
    return res.refs ?? [];
  } catch (e) {
    console.error("getSessionArchiveRefs failed", e);
    return [];
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

/** Force the next call to `getAvailableTools` to re-fetch. */
export const invalidateToolsCache = (): void => {
  _toolsCache = null;
};

export const renameSession = async (id: string, title: string): Promise<boolean> => {
  await fetchVoid(`/v1/sessions/${encodeURIComponent(id)}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ title }),
  });
  return true;
};

export const getSessionMessages = async (id: string): Promise<SessionMessagesResult> => {
  const res = await fetchJSON<{
    messages: WireMessage[];
    context_usage?: {
      used_tokens: number;
      max_tokens: number;
      threshold_tokens: number;
      percent: number;
    } | null;
  }>(
    `/v1/sessions/${encodeURIComponent(id)}/messages`
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
  return { messages, contextUsage };
};

export const sendMessage = async (id: string, content: string): Promise<ChatMessage> => {
  await fetchJSON(
    `/v1/sessions/${encodeURIComponent(id)}/messages`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ text: content, request_id: `req-${Date.now()}` }),
    }
  );
  return {
    id: `sent-${Date.now()}`,
    role: "user",
    content,
    timestamp: new Date().toISOString(),
  };
};

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

export const getSystemStatus = async (): Promise<SystemStatus> =>
  fetchJSON("/api/status");

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

export const getModelInfo = async (): Promise<ModelInfo | null> => {
  try {
    return await fetchJSON<ModelInfo>("/api/model/info");
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

export const getPluginManifests = async (): Promise<PluginManifest[]> => {
  try {
    return await fetchJSON<PluginManifest[]>("/api/dashboard/plugin-manifests");
  } catch {
    return [];
  }
};

export const inspectAllPlugins = async (): Promise<Record<string, PluginInspect>> => {
  try {
    return await fetchJSON<Record<string, PluginInspect>>(
      "/api/dashboard/plugin-inspect",
    );
  } catch {
    return {};
  }
};

export const rescanPlugins = async (): Promise<number> => {
  try {
    const res = await fetchJSON<{ ok: boolean; count: number }>(
      "/api/dashboard/plugins/rescan",
      { method: "POST" },
    );
    return res.count ?? 0;
  } catch {
    return 0;
  }
};

// ── MCP connectors (McpServerEntry) ─────────────────────────

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

function isoToMs(iso: string | undefined): number {
  if (!iso) return 0;
  const t = Date.parse(iso);
  return Number.isFinite(t) ? t : 0;
}

function fromBackend(entry: { id: string; name: string; transport: string; command?: string; args?: string[]; url?: string; env?: Record<string, string>; enabled?: boolean; created_at?: string; description?: string }): McpServerEntry {
  return {
    id: entry.id,
    name: entry.name,
    transport: (entry.transport === "http" ? "http" : "stdio") as McpTransport,
    command: entry.command,
    args: entry.args,
    url: entry.url,
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
    env?: Record<string, string>;
    enabled?: boolean;
    created_at?: string;
    description?: string;
  }>;
  seeded?: boolean;
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
    const res = await fetchJSON<{ id: string; name: string; transport: string; command?: string; args?: string[]; url?: string; env?: Record<string, string>; enabled?: boolean; created_at?: string; description?: string }>(
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
    const res = await fetchJSON<{ id: string; name: string; transport: string; command?: string; args?: string[]; url?: string; env?: Record<string, string>; enabled?: boolean; created_at?: string; description?: string }>(
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
export const resetMcpServersToOfficial = async (): Promise<McpServerEntry[]> => {
  try {
    const res = await fetchJSON<McpServerListResponse>("/api/mcp/servers/reset", {
      method: "POST",
    });
    const list = res.servers.map(fromBackend);
    saveMcpServersCache(list);
    return list;
  } catch {
    const fresh = OFFICIAL_MCP_SEEDS.map((s) => ({ ...s, createdAt: Date.now() }));
    saveMcpServersCache(fresh);
    return fresh;
  }
};

export const getLogs = async (file = "agent", lines = 100): Promise<{ lines: string[] }> =>
  fetchJSON(`/api/logs?file=${file}&lines=${lines}`);

export const searchSessions = async (q: string): Promise<{ results: Array<{ session_id: string; snippet: string }> }> =>
  fetchJSON(`/v1/sessions/search?q=${encodeURIComponent(q)}`);

/** list skills from the dashboard for cron-job skill assignment. */
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

export const resetAutomationTasks = async (): Promise<AutomationTask[]> => {
  try {
    const res = await fetchJSON<{ tasks: AutomationTask[] }>(
      "/api/automation/reset",
      { method: "POST" },
    );
    return res.tasks ?? [];
  } catch {
    return [];
  }
};

export const getPluginSettings = async (): Promise<PluginSettings> => {
  try {
    const payload = await fetchJSON<{ plugins?: PluginSettings }>("/api/dashboard/plugins");
    return payload.plugins ?? { mcp_filesystem: true, mcp_terminal: true, mcp_browser: false, mcp_database: false, tool_code_exec: true };
  } catch {
    return { mcp_filesystem: true, mcp_terminal: true, mcp_browser: false, mcp_database: false, tool_code_exec: true };
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

export const deleteEnvVar = async (key: string): Promise<boolean> => {
  await fetchJSON("/api/env", {
    method: "DELETE",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ key }),
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

// ── Persona management (settings → 画像) ─────────────────────────

async function getTextFile(path: string): Promise<PersonaTextFile | null> {
  try {
    return await fetchJSON<PersonaTextFile>(path);
  } catch (e) {
    console.error(`[persona] GET ${path} failed`, e);
    return null;
  }
}

async function putTextFile(
  path: string,
  content: string,
): Promise<PersonaTextFile | null> {
  try {
    return await fetchJSON<PersonaTextFile>(path, {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ content }),
    });
  } catch (e) {
    console.error(`[persona] PUT ${path} failed`, e);
    return null;
  }
}

// ── Soul ────────────────────────────────────────────────────────

export const getSoul = (): Promise<PersonaTextFile | null> =>
  getTextFile("/api/persona/soul");

export const putSoul = (content: string): Promise<PersonaTextFile | null> =>
  putTextFile("/api/persona/soul", content);

// ── Memory (agent notes) ───────────────────────────────────────

export const getMemory = (): Promise<PersonaTextFile | null> =>
  getTextFile("/api/persona/memory");

export const putMemory = (content: string): Promise<PersonaTextFile | null> =>
  putTextFile("/api/persona/memory", content);

// ── User profile ───────────────────────────────────────────────

export const getUserProfile = (): Promise<PersonaTextFile | null> =>
  getTextFile("/api/persona/user");

export const putUserProfile = (content: string): Promise<PersonaTextFile | null> =>
  putTextFile("/api/persona/user", content);

// ── Personalities (named) ──────────────────────────────────────

export const listPersonalities = async (): Promise<PersonalitySummary[]> => {
  try {
    return await fetchJSON<PersonalitySummary[]>("/api/persona/personalities");
  } catch (e) {
    console.error("[persona] list personalities failed", e);
    return [];
  }
};

export const getPersonality = async (
  name: string,
): Promise<PersonaTextFile | null> => {
  const safe = encodeURIComponent(name.trim().toLowerCase());
  return getTextFile(`/api/persona/personalities/${safe}`);
};

export const putPersonality = async (
  name: string,
  content: string,
): Promise<PersonaTextFile | null> => {
  const safe = encodeURIComponent(name.trim().toLowerCase());
  return putTextFile(`/api/persona/personalities/${safe}`, content);
};

export const deletePersonality = async (name: string): Promise<boolean> => {
  const safe = encodeURIComponent(name.trim().toLowerCase());
  try {
    await fetchVoid(`/api/persona/personalities/${safe}`, { method: "DELETE" });
    return true;
  } catch (e) {
    console.error(`[persona] DELETE ${safe} failed`, e);
    return false;
  }
};

// ── Persona overrides (system_prompt + preferences) ────────────

export const getPersonaOverrides = async (): Promise<PersonaOverrides> => {
  try {
    return await fetchJSON<PersonaOverrides>("/api/persona/overrides");
  } catch (e) {
    console.error("[persona] get overrides failed", e);
    return { system_prompt: "", preferences: null };
  }
};

export const putPersonaOverrides = async (
  overrides: PersonaOverrides,
): Promise<PersonaOverrides | null> => {
  try {
    return await fetchJSON<PersonaOverrides>("/api/persona/overrides", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(overrides),
    });
  } catch (e) {
    console.error("[persona] put overrides failed", e);
    return null;
  }
};

// ── Custom LLM providers ──────────────────────────

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

export const deleteCustomProvider = async (id: string): Promise<boolean> => {
  await fetchVoid(`/api/custom-providers/${encodeURIComponent(id)}`, {
    method: "DELETE",
  });
  return true;
};

export const fetchCustomProviderModels = async (
  id: string,
): Promise<CustomProviderFetchModelsResponse> =>
  fetchJSON<CustomProviderFetchModelsResponse>(
    `/api/custom-providers/${encodeURIComponent(id)}/fetch-models`,
    { method: "POST" },
  );

export const testCustomProviderConnection = async (
  id: string,
): Promise<CustomProviderTestConnectionResponse> =>
  fetchJSON<CustomProviderTestConnectionResponse>(
    `/api/custom-providers/${encodeURIComponent(id)}/test-connection`,
    { method: "POST" },
  );

export const updateCustomProviderModelContexts = async (
  id: string,
  modelContexts: Record<string, number>,
): Promise<boolean> => {
  await fetchJSON(`/api/custom-providers/${encodeURIComponent(id)}/model-contexts`, {
    method: "PUT",
    body: JSON.stringify({ model_contexts: modelContexts }),
  });
  return true;
};

// ── TickLoop ───────────────────────────────────────

export const getTickLoopStatus = async (): Promise<TickLoopStatus> =>
  fetchJSON<TickLoopStatus>("/v1/tickloop");

export const toggleTickLoop = async (enabled: boolean): Promise<TickLoopStatus> =>
  fetchJSON<TickLoopStatus>("/v1/tickloop", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ enabled }),
  });

// ── GroupChat ───────────────────────────────────────

export const listGroupChatRooms = async (): Promise<GroupChatRoom[]> =>
  fetchJSON<GroupChatRoom[]>("/api/groupchat/rooms");

/** Fetch a single room by id. Returns `null` on 404 (room deleted
 *  between list and detail fetch) so callers can fall back gracefully
 *  without a try/catch. */
export const getGroupChatRoom = async (roomId: string): Promise<GroupChatRoom | null> => {
  try {
    return await fetchJSON<GroupChatRoom>(
      `/api/groupchat/rooms/${encodeURIComponent(roomId)}`,
    );
  } catch (e) {
    // 404 → null; everything else → rethrow so the caller can decide.
    if (e instanceof Error && /404/.test(e.message)) return null;
    throw e;
  }
};

export const createGroupChatRoom = async (
  name: string,
  style?: string,
): Promise<GroupChatRoom> =>
  fetchJSON<GroupChatRoom>("/api/groupchat/rooms", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name, style }),
  });

export const deleteGroupChatRoom = async (roomId: string): Promise<void> => {
  await fetchVoid(`/api/groupchat/rooms/${encodeURIComponent(roomId)}`, {
    method: "DELETE",
  });
};

export const updateGroupChatAgents = async (
  roomId: string,
  agents: Array<{ agent_id: string; role: string }>,
): Promise<void> => {
  await fetchVoid(`/api/groupchat/rooms/${encodeURIComponent(roomId)}/agents`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ agents }),
  });
};

export const inviteAgentToRoom = async (
  roomId: string,
  agentId: string,
  role: "member" | "observer" = "member",
): Promise<void> => {
  await fetchVoid(
    `/api/groupchat/rooms/${encodeURIComponent(roomId)}/agents/${encodeURIComponent(agentId)}`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ role }),
    },
  );
};

export const kickAgentFromRoom = async (
  roomId: string,
  agentId: string,
): Promise<void> => {
  await fetchVoid(
    `/api/groupchat/rooms/${encodeURIComponent(roomId)}/agents/${encodeURIComponent(agentId)}`,
    { method: "DELETE" },
  );
};

export const updateMemberModel = async (
  roomId: string,
  agentId: string,
  model: string | null,
): Promise<void> => {
  await fetchVoid(
    `/api/groupchat/rooms/${encodeURIComponent(roomId)}/agents/${encodeURIComponent(agentId)}`,
    {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ model: model && model.length > 0 ? model : null }),
    },
  );
};

export const updateMemberDisplayName = async (
  roomId: string,
  agentId: string,
  displayName: string | null,
): Promise<void> => {
  await fetchVoid(
    `/api/groupchat/rooms/${encodeURIComponent(roomId)}/agents/${encodeURIComponent(agentId)}/display-name`,
    {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        display_name: displayName && displayName.trim().length > 0 ? displayName.trim() : null,
      }),
    },
  );
};

export const fetchRoomMembers = async (
  roomId: string,
): Promise<GroupChatMember[]> => {
  return fetchJSON<GroupChatMember[]>(
    `/api/groupchat/rooms/${encodeURIComponent(roomId)}/members`,
  );
};

export const fetchRoomRanks = async (
  roomId: string,
): Promise<GroupChatRank[]> => {
  return fetchJSON<GroupChatRank[]>(
    `/api/groupchat/rooms/${encodeURIComponent(roomId)}/ranks`,
  );
};

export const sendGroupChatVote = async (
  roomId: string,
  targetAgentId: string,
  direction: "up" | "down",
): Promise<GroupChatRank> => {
  return fetchJSON<GroupChatRank>(
    `/api/groupchat/rooms/${encodeURIComponent(roomId)}/vote`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ target_agent_id: targetAgentId, direction }),
    },
  );
};

export const disputeViolation = async (
  roomId: string,
  violationId: string,
  reason: string,
): Promise<void> => {
  await fetchVoid(`/api/groupchat/rooms/${encodeURIComponent(roomId)}/dispute`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ violation_id: violationId, reason }),
  });
};

export const overturnViolation = async (
  roomId: string,
  violationId: string,
  agentId: string,
  comment: string,
): Promise<{ restored: number }> => {
  return fetchJSON(`/api/groupchat/rooms/${encodeURIComponent(roomId)}/overturn`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ violation_id: violationId, agent_id: agentId, comment }),
  });
};

export const setGroupChatLeader = async (
  roomId: string,
  agentId: string,
): Promise<void> => {
  await fetchVoid(`/api/groupchat/rooms/${encodeURIComponent(roomId)}/leader`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ agent_id: agentId }),
  });
};

export const sendGroupChatMessage = async (
  roomId: string,
  content: string,
  signal?: AbortSignal,
): Promise<SendMessageResponse> =>
  fetchJSON<SendMessageResponse>(
    `/api/groupchat/rooms/${encodeURIComponent(roomId)}/message`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ content }),
      signal,
    },
  );

export const getGroupChatMessages = async (
  roomId: string,
  beforeId?: string,
  limit?: number,
): Promise<GroupChatMessage[]> => {
  const params = new URLSearchParams();
  if (beforeId) params.set("before_id", beforeId);
  if (limit) params.set("limit", String(limit));
  const qs = params.toString();
  return fetchJSON<GroupChatMessage[]>(
    `/api/groupchat/rooms/${encodeURIComponent(roomId)}/messages${qs ? `?${qs}` : ""}`,
  );
};

export const approveGroupChatTask = async (
  roomId: string,
  approved: boolean,
  comment?: string,
): Promise<void> => {
  await fetchVoid(
    `/api/groupchat/rooms/${encodeURIComponent(roomId)}/${approved ? "approve" : "reject"}`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ approved, comment }),
    },
  );
};

// ── GroupChat Announcement ──────────────────────────────

export const publishAnnouncement = async (
  roomId: string,
  text: string,
): Promise<GroupChatAnnouncementResponse> =>
  fetchJSON<GroupChatAnnouncementResponse>(
    `/api/groupchat/rooms/${encodeURIComponent(roomId)}/announcement`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ text }),
    },
  );

export const clearAnnouncement = async (roomId: string): Promise<void> => {
  await fetchVoid(
    `/api/groupchat/rooms/${encodeURIComponent(roomId)}/announcement`,
    { method: "DELETE" },
  );
};

// ── GroupChat Description ──────────────────────────────────

export const updateGroupChatDescription = async (
  roomId: string,
  description: string,
): Promise<GroupChatDescriptionResponse> =>
  fetchJSON<GroupChatDescriptionResponse>(
    `/api/groupchat/rooms/${encodeURIComponent(roomId)}/description`,
    {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ description }),
    },
  );

export const clearGroupChatDescription = async (roomId: string): Promise<void> => {
  await fetchVoid(
    `/api/groupchat/rooms/${encodeURIComponent(roomId)}/description`,
    { method: "DELETE" },
  );
};

export const updateGroupChatChatMode = async (
  roomId: string,
  chatMode: "task_collab" | "free_chat",
): Promise<{ chat_mode: string | null }> =>
  fetchJSON<{ chat_mode: string | null }>(
    `/api/groupchat/rooms/${encodeURIComponent(roomId)}/chat-mode`,
    {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ chat_mode: chatMode }),
    },
  );

// ── GroupChat Tasks API ────────────────────────────────

export const getActiveGroupChatTask = async (
  roomId: string,
): Promise<GroupChatActiveTaskResponse> =>
  fetchJSON<GroupChatActiveTaskResponse>(
    `/api/groupchat/rooms/${encodeURIComponent(roomId)}/tasks/active`,
  );

export const listGroupChatTasks = async (
  roomId: string,
): Promise<GroupChatTask[]> =>
  fetchJSON<GroupChatTask[]>(
    `/api/groupchat/rooms/${encodeURIComponent(roomId)}/tasks`,
  );

export const updateGroupChatSubtask = async (
  subtaskId: string,
  status: GroupChatSubtaskStatus,
  result?: string,
): Promise<void> => {
  await fetchVoid(
    `/api/groupchat/subtasks/${encodeURIComponent(subtaskId)}`,
    {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ status, result }),
    },
  );
};

export const updateGroupChatRules = async (
  roomId: string,
  rules: Partial<GroupChatRoomRules>,
): Promise<void> => {
  const body: Record<string, unknown> = {};
  if (rules.global_constraints !== undefined) body.global_constraints = rules.global_constraints;
  if (rules.role_permissions !== undefined) body.role_permissions = rules.role_permissions;
  if (rules.tool_policy?.mode !== undefined) body.tool_policy_mode = rules.tool_policy.mode;
  if (rules.tool_policy?.blacklisted !== undefined) body.tool_policy_blacklist = rules.tool_policy.blacklisted;
  if (rules.tool_policy?.whitelisted !== undefined) body.tool_policy_whitelist = rules.tool_policy.whitelisted;
  if (rules.penalty_policy?.initial_score !== undefined) body.penalty_initial_score = rules.penalty_policy.initial_score;
  if (rules.penalty_policy?.reflection_threshold !== undefined) body.penalty_reflection_threshold = rules.penalty_policy.reflection_threshold;

  await fetchVoid(
    `/api/groupchat/rooms/${encodeURIComponent(roomId)}/rules`,
    {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    },
  );
};

export const getGroupChatRoomSettings = async (
  roomId: string,
): Promise<GroupChatRoomSettings> =>
  fetchJSON<GroupChatRoomSettings>(
    `/api/groupchat/rooms/${encodeURIComponent(roomId)}/settings`,
  );

export const updateGroupChatRoomSettings = async (
  roomId: string,
  partial: Partial<GroupChatRoomSettings>,
): Promise<void> => {
  await fetchVoid(
    `/api/groupchat/rooms/${encodeURIComponent(roomId)}/settings`,
    {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(partial),
    },
  );
};
