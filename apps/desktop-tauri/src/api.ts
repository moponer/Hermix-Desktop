// Hermix Desktop API Client — routes aligned to hermix-server
import type { AutomationTask, ChatMessage, Session } from "./types";
import { storage, STORAGE_KEYS } from "./utils/storageCompat";
// v5: reuse detectToolError from messageParser for errorReason extraction
// on reloaded tool messages — same helper as the live-streaming path.
import { detectToolError } from "./components/messageParser";
import { wireArrayToChatMessages, type WireMessage } from "./protocolAdapter";

export const API_BASE = "http://127.0.0.1:3000";
export const WS_BASE = "ws://127.0.0.1:3000";

export function apiUrl(path: string): string {
  return `${API_BASE}${path.startsWith("/") ? path : `/${path}`}`;
}

/** Wait n milliseconds without blocking the event loop. */
function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

// ── Circuit breaker ─────────────────────────────────────────────
// When the backend is unreachable, multiple components fire requests
// simultaneously, each going through withRetry (N retries each).
// ERR_INSUFFICIENT_RESOURCES also produces TypeError: Failed to fetch,
// so withRetry retries — creating a positive feedback loop that exhausts
// the browser's connection pool (Chrome limits ~6 concurrent per origin).
// The circuit breaker breaks this cycle: after N consecutive network
// failures across ALL callers, all requests are immediately rejected for
// a cooldown period, giving the browser time to reclaim sockets.

let _consecutiveFailures = 0;
const CIRCUIT_FAILURE_THRESHOLD = 8;
const CIRCUIT_COOLDOWN_MS = 15_000;
let _circuitTrippedUntil = 0;

/** True when the circuit breaker is in cooldown — all requests are
 *  immediately rejected without hitting the network. Components that
 *  poll (BackendStatus, CustomAppsView) should check this to skip. */
export function isCircuitTripped(): boolean {
  return Date.now() < _circuitTrippedUntil;
}

function _recordFailure() {
  _consecutiveFailures++;
  if (_consecutiveFailures >= CIRCUIT_FAILURE_THRESHOLD && _circuitTrippedUntil <= Date.now()) {
    _circuitTrippedUntil = Date.now() + CIRCUIT_COOLDOWN_MS;
    console.warn(
      `[circuit] Tripped after ${_consecutiveFailures} consecutive network failures — ` +
      `cooldown ${CIRCUIT_COOLDOWN_MS}ms. All API requests will be rejected until ${new Date(_circuitTrippedUntil).toLocaleTimeString()}.`,
    );
  }
}

function _recordSuccess() {
  if (_consecutiveFailures > 0) {
    _consecutiveFailures = 0;
    _circuitTrippedUntil = 0;
  }
}

/**
 * Retry wrapper: if fetch throws a network error (ECONNREFUSED / ECONNRESET
 * — i.e., the sidecar hasn't bound port 3000 yet), retry up to 3 times with
 * exponential backoff (200ms … 800ms). This avoids the boot-time console
 * spam of ERR_CONNECTION_REFUSED errors.
 *
 * **Circuit breaker**: after CIRCUIT_FAILURE_THRESHOLD consecutive network
 * failures across all callers, the circuit trips and all subsequent requests
 * are immediately rejected for CIRCUIT_COOLDOWN_MS. This prevents the
 * positive feedback loop where ERR_INSUFFICIENT_RESOURCES causes retries
 * that further exhaust the browser's connection pool.
 *
 * If the request succeeds but returns non-2xx, throw immediately (don't retry
 * HTTP errors — those are real problems, not startup races).
 */
async function withRetry<T>(fn: () => Promise<T>): Promise<T> {
  // Circuit breaker: reject immediately if tripped.
  if (isCircuitTripped()) {
    throw new TypeError("Failed to fetch (circuit breaker open)");
  }

  const MAX_RETRIES = 3;
  const BASE_DELAY_MS = 200;

  for (let attempt = 0; attempt <= MAX_RETRIES; attempt++) {
    try {
      const result = await fn();
      _recordSuccess();
      return result;
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : String(err);
      // Only retry on network-level errors (TypeError from fetch when the
      // server isn't listening yet). Don't retry on HTTP 4xx/5xx responses
      // or on AbortError (timeout — retrying would make overload worse).
      if (!msg.includes("Failed to fetch") && !msg.includes("NetworkError")) {
        throw err;
      }
      _recordFailure();
      // If the circuit just tripped, stop retrying immediately.
      if (isCircuitTripped()) {
        throw err;
      }
      if (attempt >= MAX_RETRIES) {
        throw err;
      }
      const delayMs = BASE_DELAY_MS * (1 << attempt); // 200, 400, 800
      await sleep(delayMs);
    }
  }
  throw new Error("retry exhausted"); // unreachable, just for TS
}

/** Per-request timeout in milliseconds. Prevents connections from
 *  hanging indefinitely when the backend is slow or unresponsive,
 *  which would otherwise exhaust the browser's connection pool. */
const FETCH_TIMEOUT_MS = 10_000;

async function fetchApi<T = void>(path: string, init?: RequestInit, { parseJson = true }: { parseJson?: boolean } = {}): Promise<T> {
  return withRetry(async () => {
    // Combine a per-request timeout with any caller-provided signal
    // so long-running requests don't hold connections open indefinitely.
    const timeoutSignal = AbortSignal.timeout(FETCH_TIMEOUT_MS);
    const signal = init?.signal
      ? AbortSignal.any([init.signal, timeoutSignal])
      : timeoutSignal;
    const response = await fetch(apiUrl(path), { ...init, signal });
    if (!response.ok) {
      const text = await response.text().catch(() => response.statusText);
      throw new Error(text || `HTTP ${response.status}`);
    }
    if (parseJson) {
      return response.json() as T;
    }
    return undefined as T;
  });
}

/** Fetch a JSON response from the API. Exported so modules that need
 *  raw endpoint access (e.g. customAppApi.ts) can share the circuit
 *  breaker, timeout, and retry infrastructure. */
export async function fetchJSON<T>(path: string, init?: RequestInit): Promise<T> {
  return fetchApi<T>(path, init, { parseJson: true });
}

/** Fetch without parsing the response body. */
async function fetchVoid(path: string, init?: RequestInit): Promise<void> {
  return fetchApi<void>(path, init, { parseJson: false });
}

// ── Orchestration Engine: Task CRUD ────────
// Task is the top-level work unit in the orchestration engine.
// POST /v1/tasks creates a task, GET /v1/tasks lists them,
// GET /v1/tasks/:id fetches one, PATCH /v1/tasks/:id/status
// transitions the status.

export type TaskMode = "craft" | "plan" | "ask";
export type TaskStatus =
  | "pending"
  | "planning"
  | "in_progress"
  | "completed"
  | "failed"
  | "archived";

export interface TaskContextRef {
  target: string;
  kind: "file" | "directory" | "url" | "session" | "unknown";
}

export interface TaskContext {
  references?: TaskContextRef[];
  screenshots?: string[];
  attachments?: string[];
  notes?: string | null;
}

export interface TaskResultRef {
  path: string;
  artifact_type: "html" | "image" | "report" | "video" | "file";
  summary?: string | null;
}

export interface Task {
  id: string;
  title: string;
  workspace_path: string;
  mode: TaskMode;
  status: TaskStatus;
  parent_task_id?: string | null;
  context: TaskContext;
  results: TaskResultRef[];
  created_at: number;
  updated_at: number;
}

export interface CreateTaskRequest {
  title: string;
  workspace_path: string;
  mode: TaskMode;
  parent_task_id?: string | null;
  context?: TaskContext;
}

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
// POST /v1/tasks/:id/results registers deliverable artifacts
// for a Task, classifies them by type, and emits ResultDelivered
// event via the orchestration bus.

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
// The new three-table automation engine: /v1/automations endpoints
// backed by hermix-automation crate. Triggers go through the unified
// agent_service.send_message() dispatch path (铁律 DD).

export type AutomationScheduleType = "recurring" | "once";
export type AutomationStatus = "ACTIVE" | "PAUSED";

export interface Automation {
  id: string;
  name: string;
  prompt: string;
  schedule_type: AutomationScheduleType;
  rrule?: string | null;
  scheduled_at?: string | null;
  status: AutomationStatus;
  valid_from?: string | null;
  valid_until?: string | null;
  cwds?: string | null;
  connector_ids_json?: string | null;
  expert_id?: string | null;
  model_id?: string | null;
  created_at: number;
  updated_at: number;
}

export interface AutomationRun {
  id: string;
  automation_id: string;
  started_at: number;
  finished_at?: number | null;
  status: string;
  task_id?: string | null;
  error_text?: string | null;
}

export interface CreateAutomationRequest {
  name: string;
  prompt: string;
  schedule_type: AutomationScheduleType;
  rrule?: string | null;
  scheduled_at?: string | null;
  valid_from?: string | null;
  valid_until?: string | null;
  cwds?: string | null;
  connector_ids_json?: string | null;
  expert_id?: string | null;
  model_id?: string | null;
}

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
// The TodoPanel was retired in in favor of inline
// TodoListCard components rendered into the message stream. Card
// state is fed by three WS events:
// - `todo_group_started` — LLM declared a new plan
// - `todo_item_upserted`  — create / update / depend / record_execution
// - `todo_item_removed`   — delete
// See `useStreamChat.ts` for the parser. WS is the single source of
// truth (铁律 H).
// `patchTodo` is what the user's manual checkbox click calls — the
// server dispatches the update through the same `todo` tool the
// LLM uses, the WS forwarder fires, every connected client sees
// the change. `TodoListCard` consumes the optimistic patch result
// and the bounce-back WS event via `justPatchedTodoRef`.

export interface TodoItem {
  id: string;
  title: string;
  description?: string | null;
  status: string;
  priority: string;
  created_at: string;
  depends_on: string[];
  last_result?: string | null;
  execution_count: number;
  /** Plan / group id (UUID per plan). Empty string for legacy items. */
  group_id: string;
  /** Human label for the plan (e.g. "rename all files"). Optional. */
  group_title?: string | null;
}

/**
 * @deprecated since — the TodoPanel that consumed this
 * was replaced by per-message TodoListCard components fed by WS.
 * Front-end callers vanished with the panel. The HTTP endpoint is
 * kept server-side for legacy CLI / script users; the UI no longer
 * polls it.
 */
export async function listTodos(): Promise<{ todos: TodoItem[]; total: number }> {
  return fetchJSON(`/v1/todos`);
}

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

// ── Types ──────────────────────────────────────────

export interface SessionSummary {
  id: string;
  title?: string | null;
  created_at?: string;
  updated_at?: string;
  message_count?: number;
  /** The model used for this session (from sessions.model column). */
  model?: string | null;
  /** The workspace path selected for this session. */
  workspace?: string | null;
  /** The expert ID selected for this session. */
  expert_id?: string | null;
}

export interface DashboardSession {
  id: string;
  source: string;
  model: string;
  title: string;
  started_at: number;
  ended_at: number | null;
  last_active: number;
  is_active: boolean;
  message_count: number;
  tool_call_count: number;
  input_tokens: number;
  output_tokens: number;
  preview: string | null;
}

export interface SystemStatus {
  version: string;
  hermix_home: string;
  config_path: string;
  active_sessions: number;
  gateway_running: boolean;
  rtk_saved_chars: number;
  rtk_saved_calls: number;
}

export interface HermesConfig {
  [key: string]: unknown;
}

export interface SkillInfo {
  name: string;
  description: string;
  category: string;
  enabled: boolean;
}

export interface PluginSettings {
  mcp_filesystem: boolean;
  mcp_terminal: boolean;
  mcp_browser: boolean;
  mcp_database: boolean;
  tool_code_exec: boolean;
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
    // Carry the backend's authoritative message count forward. Without
    // this, the sidebar can't show "N 条 · 时间" and `handleNewChat`'s
    // reuse-empty-session check fires against *every* session — since
    // `toSession` always seeds `messages: []`.
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

// ── Archive refs (short-term context unload) ─────
// Mirrors `hermix_agent::ArchiveRef` (rust) — see
// `crates/hermix-agent/src/archive.rs`. The server writes one
// JSON-Lines row per archived message under
// `<hermix_home>/sessions/<sid>/archive-index.jsonl`; this endpoint
// reads the whole file and returns the list. Empty when the session
// has never triggered a compression round (the common case for
// short sessions), in which case the chat banner is hidden.
// We surface `filePath` exactly as the server returns it (a
// platform-native absolute path). The chat banner hands it to
// `read_artifact_file` to open the anchor in CodeView — that
// helper enforces the allowed-roots whitelist, so a malicious
// `filePath` cannot escape the sandbox.
// `lineRange` is `[start, end]` inclusive 1-based line numbers
// pointing at the `<a id="node:..."></a>` anchor line inside the
// markdown file. CodeView passes it as `#L42` so the markdown
// renderer can jump straight to the anchor.

export interface ArchiveRef {
  nodeId: string;
  filePath: string;
  lineRange: [number, number] | null;
  charCount: number;
}

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
// `GET /v1/tools` is the single source of truth — adding a tool to
// the `ToolRegistry` makes it appear here automatically, with no
// front-end code change required. The front-end keeps a 30s
// module-level cache so re-renders don't hammer the server.
export interface ToolDescriptor {
  name: string;
  toolset: string;
  description: string;
  emoji: string;
  isAsync: boolean;
  isEnabled: boolean;
  envDeps: string[];
  maxResultSizeChars: number | null;
}

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
    // If we have a stale cache, prefer it over an empty list — at
    // least the user keeps their working toolset until the server
    // comes back.
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

/** Session-level context-usage snapshot returned alongside messages when
 *  restoring a session. `null` when the backend has no persisted snapshot
 *  (e.g. a session that never streamed, or a pre-migration DB). */
export interface RestoredContextUsage {
  usedTokens: number;
  maxTokens: number;
  thresholdTokens: number;
  percent: number;
}

export interface SessionMessagesResult {
  messages: ChatMessage[];
  contextUsage: RestoredContextUsage | null;
}

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

export const sendMessageStream = (id: string, content: string) => sendMessage(id, content);

// ── WebSocket Streaming ────────────────────────────

export function resolveWebSocketUrl(sessionId: string): string {
  return `${WS_BASE}/v1/ws-stream/${encodeURIComponent(sessionId)}`;
}

/** WebSocket URL for the group chat streaming endpoint.
 *  Opens a new connection per message. */
export function resolveGroupChatWebSocketUrl(roomId: string): string {
  return `${WS_BASE}/v1/ws-groupchat/${encodeURIComponent(roomId)}`;
}

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

export interface ModelInfo {
  model: string;
  provider: string;
}

export const getModelInfo = async (): Promise<ModelInfo | null> => {
  try {
    return await fetchJSON<ModelInfo>("/api/model/info");
  } catch {
    return null;
  }
};

/** A single user-enabled model in the chat switcher. Mirrors
 *  `hermix_config::ModelSelectorEntry` on the backend. */
export interface ModelListEntry {
  provider_id: string;
  model_id: string;
}

/** Response from `GET /api/model/list` — the durable, backend-persisted
 *  enabled-model list (source of truth across restarts). */
export interface ModelListResponse {
  models: ModelListEntry[];
  default_model: string | null;
}

/** Fetch the authoritative enabled-model list from the backend. Returns
 *  `null` on any error so callers can fall back to localStorage. */
export const getModelList = async (): Promise<ModelListResponse | null> => {
  try {
    return await fetchJSON<ModelListResponse>("/api/model/list");
  } catch {
    return null;
  }
};

/** Persist the full enabled-model list to the backend (config.yaml). This
 *  makes the chat model switcher stable across restarts even if
 *  `localStorage` is wiped. Fire-and-forget — failures are logged, never
 *  thrown, because the localStorage copy still works for the current session. */
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

export interface SkillContent {
  name: string;
  content: string;
  category: string | null;
  description: string | null;
}

export const getSkillContent = async (name: string): Promise<SkillContent | null> => {
  try {
    return await fetchJSON(`/api/skills/${encodeURIComponent(name)}`);
  } catch {
    return null;
  }
};

export interface ToolsetInfo {
  name: string;
  label: string;
  description: string;
  enabled: boolean;
  configured: boolean;
  tools: string[];
}

export const getToolsets = async (): Promise<ToolsetInfo[]> =>
  fetchJSON("/api/tools/toolsets");

export interface ToolsetToggleResult {
  /** Canonical enabled set the server committed (post-toggle). */
  enabled: string[];
  /**
   * Toolset names the UI asked to flip but which the registry
   * does not actually have. Surfaced so the Settings UI can
   * show a "this toolset does not exist" toast instead of
   * silently dropping them.
   */
  unknown: string[];
}

/**
 * Persist the user's toolset enable/disable choices. The server
 * treats the `enabled` array as the *absolute* post-toggle set;
 * any registered toolset absent from `enabled` will be flipped
 * off. This is intentionally a "set state" call, not a diff, so
 * the UI does not have to compute a delta and the server can be
 * the single source of truth for "what is on right now".
 *
 * Returns the canonical state the server committed. We treat
 * transport errors as failures (caller should keep the previous
 * state in the UI) but `unknown` entries are *not* errors — the
 * server applied the rest of the request fine.
 */
export const updateEnabledToolsets = async (
  enabled: string[],
): Promise<ToolsetToggleResult> =>
  fetchJSON<ToolsetToggleResult>("/api/tools/toolsets/enabled", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ enabled }),
  });

export interface PluginManifest {
  id?: string;
  name: string;
  description?: string;
  category?: string;
  icon?: string;
  version?: string;
  author?: string;
  [key: string]: unknown;
}

export const getPluginManifests = async (): Promise<PluginManifest[]> => {
  try {
    return await fetchJSON<PluginManifest[]>("/api/dashboard/plugin-manifests");
  } catch {
    return [];
  }
};

export interface PluginInspect {
  id: string;
  availability: "ready" | "seeded";
  toolset: string | null;
  tools: string[];
  total: number;
}

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
// These are user-configured MCP (Model Context Protocol) servers. Each entry
// is one of:
// * stdio transport — `command` set, hermix-agent spawns the process and
// speaks JSON-RPC over its stdin/stdout.
// * http transport — `url` set, hermix-agent talks JSON-RPC over HTTP/SSE
// to a remote MCP endpoint.
// hermix-mcp (crates/hermix-mcp) provides the client/server; hermix-config
// defines the McpServerEntry model. Until /api/mcp/servers is wired in
// hermix-server we keep the list in localStorage so the UI is testable.

export type McpTransport = "stdio" | "http";

export interface McpServerEntry {
  id: string;
  name: string;
  description?: string;
  transport: McpTransport;
  command?: string;
  args?: string[];
  env?: Record<string, string>;
  url?: string;
  enabled: boolean;
  createdAt: number;
}

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
// As of `feat(server): seed 18 official plugin manifests + /api/mcp/servers
// CRUD`, the Rust server is the source of truth for MCP
// server config — persisted at `$HERMES_HOME/mcp_servers.json` and seeded
// with 5 official entries on first list. We talk to it via the HTTP routes
// exposed by `crates/hermix-server/src/dashboard/mcp.rs`.
// We keep a localStorage mirror as a *cache* so the UI keeps working when
// the backend is offline (typical when the user is in the middle of starting
// or restarting the sidecar). The cache is only ever populated FROM the
// backend's response — never user-edited — so the two never disagree for long.
// If the backend is unreachable, we fall back to the OFFICIAL_MCP_SEEDS
// (same 5 entries the backend seeds with) so the UI is still functional.

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
  // Most backend-created entries have RFC3339-ish stamps like "2026-06-02T...".
  // We can't rely on the Rust now_iso() placeholder (1970-...), so just
  // attempt to parse. Falls back to 0 — only used for tie-breaking in lists.
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
    // Backend offline (sidecar still starting, etc.) — fall back to cache.
    // The cache is only ever populated by user mutations (add/update/delete)
    // or an explicit reset, never by this read path: the backend is the
    // single source of truth on GET.
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
    // Success: backend already persisted; do NOT also write to localStorage
    // here — the cache will be refreshed on the next listMcpServers call.
    // Writing both would create a second source of truth (the localStorage
    // format may drift from the server's canonical state). The catch path
    // below IS the legitimate fallback for offline mode.
    return created;
  } catch {
    // Backend offline — local-only entry, will sync on next successful list.
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
    // Success: backend already persisted. The cache will be refreshed
    // on the next listMcpServers call — avoid a second source of truth.
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

// ── Auto-correct model (config.model → configured provider) ─────
// Backend runs `hermix_config::auto_correct_model`: if the user
// configured a key (e.g. for MiniMax) but the default model still
// points at a provider without a key (e.g. `openai:gpt-4o`), the
// endpoint picks a configured provider, persists the change to
// `config.yaml`, and pushes the new live config into the running
// `AgentService` — so the next chat uses the right model with no
// desktop-app restart.
// `message` is a Chinese summary suitable for a toast; `report` is
// `null` when no correction was needed.
export interface AutoCorrectReport {
  before: string | null;
  after: string;
  provider: string;
  reason: "unset" | "no_key" | "unknown_provider";
  configured_providers: string[];
}

export interface AutoCorrectResponse {
  config: Record<string, unknown>;
  report: AutoCorrectReport | null;
  message: string;
}

export const autoCorrectConfig = async (): Promise<AutoCorrectResponse> =>
  fetchJSON("/api/config/auto-correct", { method: "POST" });

export const checkBackendHealth = async (): Promise<boolean> => {
  try {
    const resp = await fetch(apiUrl("/health"), { signal: AbortSignal.timeout(3000) });
    const data = await resp.json();
    return data.status === "ok";
  } catch {
    return false;
  }
};

// ── Analytics / Usage ────────────────────────────────────────

export interface AnalyticsDailyEntry {
  day: string;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_creation_tokens: number;
  reasoning_tokens: number;
  estimated_cost: number;
  actual_cost: number;
  sessions: number;
  /** estimated cache hit rate (0..100) when the provider
   *  under-reports cache reads; `undefined` means use real data. */
  estimated_cache_hit_rate?: number;
}

export interface AnalyticsModelEntry {
  model: string;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_creation_tokens: number;
  reasoning_tokens: number;
  estimated_cost: number;
  sessions: number;
  /** estimated cache hit rate (0..100) when real cache reads are 0. */
  estimated_cache_hit_rate?: number;
}

export interface AnalyticsTotals {
  total_input: number;
  total_output: number;
  total_cache_read: number;
  total_cache_creation: number;
  total_reasoning: number;
  total_estimated_cost: number;
  total_actual_cost: number;
  total_sessions: number;
  total_compactions: number;
  total_archived_turns: number;
  /** estimated overall cache hit rate (0..100) when real cache
   *  reads are 0; `undefined` means use the real `total_cache_read`. */
  estimated_cache_hit_rate?: number;
}

export interface AnalyticsSkillEntry {
  skill: string;
  view_count: number;
  manage_count: number;
  total_count: number;
  percentage: number;
  last_used_at: number | null;
}

export interface AnalyticsSkillsSummary {
  total_skill_loads: number;
  total_skill_edits: number;
  total_skill_actions: number;
  distinct_skills_used: number;
}

// per-tool call counts derived from messages.tool_calls JSON.
export interface AnalyticsToolEntry {
  tool: string;
  call_count: number;
  session_count: number;
  daily_avg: number;
}

export interface AnalyticsResponse {
  daily: AnalyticsDailyEntry[];
  by_model: AnalyticsModelEntry[];
  totals: AnalyticsTotals;
  skills: {
    summary: AnalyticsSkillsSummary;
    top_skills: AnalyticsSkillEntry[];
  };
  by_tool: AnalyticsToolEntry[];
}

export const getAnalytics = async (days = 30): Promise<AnalyticsResponse> =>
  fetchJSON(`/api/analytics/usage?days=${days}`);

// ── Analytics debug ───────────────────────────────
// Diagnostic view of the stats DB: existence / row counts / sample
// rows / applied migrations. Lets the user pinpoint a broken pipeline
// without opening SQLite by hand.

export interface AnalyticsDebugTable {
  exists: boolean;
  row_count: number | null;
  sample: unknown[];
  error?: string;
}

export interface AnalyticsDebugTables {
  sessions: AnalyticsDebugTable;
  messages: AnalyticsDebugTable;
  token_usage: AnalyticsDebugTable;
}

export interface AnalyticsDebugMigration {
  id: string;
  applied_at: string | null;
  status?: string;
}

export interface AnalyticsDebugResponse {
  db_path: string;
  tables: AnalyticsDebugTables;
  migrations: AnalyticsDebugMigration[];
}

export const getAnalyticsDebug = async (): Promise<AnalyticsDebugResponse> =>
  fetchJSON(`/api/analytics/debug`);

// ── Expert catalog ──────────────────────────────────────────
// Mirrors `hermix_skills::ExpertEntry` in
// `crates/hermix-skills/src/expert.rs`. The desktop binary ships the
// built-in experts in TS; at startup we POST them to /v1/experts and the
// UI reads from there so "what's on screen" is always "what the backend
// has actually registered".

export interface RemoteExpert {
  id: string;
  name: string;
  avatar: string;
  role: string;
  category: string;
  description: string;
  system_prompt: string;
  toolset?: string | null;
  is_builtin: boolean;
}

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
// The agent loop reads four files at session start (see
// `crates/hermix-agent/src/agent_loop.rs::build_system_prompt`):
// • `~/.hermix/SOUL.md`                  — soul / core philosophy
// • `~/.hermix/personalities/<name>.md`  — named personas
// • `~/.hermix/memories/MEMORY.md`       — agent's persistent notes
// • `~/.hermix/memories/USER.md`         — user profile / preferences
// Plus a JSON override file at `~/.hermix/persona_overrides.json`
// (system_prompt + free-form preferences) — currently informational
// for the UI; the agent loop will read it in a focused follow-up.
// The endpoints are file-backed on the Rust side, so "save" is just
// `PUT` and the next session sees the change. There is no caching layer.

export interface PersonaTextFile {
  /** Absolute path on disk (or `<builtin:name>` for built-in personas). */
  path: string;
  /** File contents. `null` when the file does not yet exist. */
  content: string | null;
  /** Last-modified epoch millis. `null` if the file does not exist. */
  updated_at: number | null;
}

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

export interface PersonalitySummary {
  name: string;
  summary: string | null;
  size: number;
  updated_at: number | null;
  is_builtin: boolean;
}

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
// `system_prompt` is the full override that replaces the agent's
// default system message. `preferences` is open-ended JSON the agent
// can read into the prompt. The agent loop currently reads
// `system_prompt` from `HermesConfig`; this file is the new editable
// source of truth,等待接入 `build_system_prompt`。

export interface PersonaOverrides {
  system_prompt: string;
  preferences: Record<string, unknown> | null;
}

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

export interface CustomProviderSummary {
  id: string;
  display_name: string | null;
  base_url: string;
  has_key: boolean;
  models: string[];
}

export interface CustomProviderCreateRequest {
  id: string;
  display_name?: string;
  base_url: string;
  api_key: string;
  models?: string;
}

export interface CustomProviderFetchModelsResponse {
  models: { id: string }[];
  error: string | null;
}

export interface CustomProviderTestConnectionResponse {
  ok: boolean;
  status: number;
  error: string | null;
  models: { id: string }[] | null;
}

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

export interface TickLoopStatus {
  running: boolean;
  phase: string;
}

export const getTickLoopStatus = async (): Promise<TickLoopStatus> =>
  fetchJSON<TickLoopStatus>("/v1/tickloop");

export const toggleTickLoop = async (enabled: boolean): Promise<TickLoopStatus> =>
  fetchJSON<TickLoopStatus>("/v1/tickloop", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ enabled }),
  });

// ── GroupChat ───────────────────────────────────────

export interface GroupChatRoom {
  id: string;
  name: string;
  leader_id: string | null;
  agent_ids: string[];
  /** Per-member profile (id, role, model_override). Present when the
   *  server serializes the new `members` field; missing for older
   *  payloads (treated as empty by the UI). */
  members?: GroupChatMember[];
  /** The built-in leader (项目总管) virtual identity. New rooms always
   *  have one (auto-created on the server). Pre-P1 rooms may not. */
  leader?: GroupChatLeader;
  /** Pinned project announcement from the leader. When set, the chat
   *  surface renders a banner above the messages; the announcement
   *  doubles as the room's current goal — anyone joining sees what
   *  the team is currently working on. */
  announcement?: string | null;
  announcement_published_at?: string | null;
  /** Free-form description of the room (project background, goal, or
   *  constraints). Surfaced in the chat header so members know what
   *  the room is for at a glance. Edited via PATCH
   *  /api/groupchat/rooms/{id}/description. Distinct from
   *  `announcement` which is the leader's pinned "current goal". */
  description?: string | null;
  /** UI rendering style for this room: "modern" (default, bubble-based
   *  like WeChat/Feishu), "classic" (IRC-style monospace linear log),
   *  "compact" (denser bubble layout). Absent on old rooms → "modern". */
  style?: string | null;
  /** Chat mode: "task_collab" (default, leader dispatches tasks to
   *  members) or "free_chat" (all members respond freely without
   *  leader coordination). Absent on old rooms → "task_collab". */
  chat_mode?: string | null;
  /** The currently in-flight task (one of pending/decomposing/executing/
   *  aggregating/awaiting_approval). Includes subtasks inline so the
   *  panel can render progress without a follow-up round trip.
   *  `undefined` when there's no active task. */
  active_task?: GroupChatTask | null;
  created_at: string;
}

/** The built-in leader (项目总管) exposed by the server. Lives once
 *  per room, system-managed — never appears in `agent_ids`, never
 *  invitable, never kickable, never model-overridable.
 *
 *  Messages from the leader have `sender_id === leader.agent_id` so
 *  the chat bubble can render them with the distinct leader style. */
export interface GroupChatLeader {
  /** Sentinel id `__leader__` — used both as `sender_id` on leader
   *  messages and as the agent_id guard in the API. */
  agent_id: string;
  /** Localized display name. Always "项目总管" server-side for now. */
  display_name: string;
  /** Session id the dispatcher binds to when running the
   *  LeaderOrchestrator prompt. Unique per room. */
  session_id: string;
  /** Per-room model override for the leader. Null/absent = use default. */
  model_override?: string | null;
}

export interface GroupChatMember {
  agent_id: string;
  role: "leader" | "member" | "observer";
  /** Room-level model override; `undefined` if not set (use agent default). */
  model_override?: string | null;
  /** Room-level display name (别名); `undefined` if not set (use
   *  agent's global name). P3 — set via `updateMemberDisplayName`. */
  display_name?: string | null;
  /** Backend-generated session id for this (room, agent) pair. Useful
   *  for tracing agent logs without re-deriving the id client-side. */
  session_id?: string;
}

export interface GroupChatMessage {
  id: string;
  room_id: string;
  sender_id: string;
  sender_name: string;
  content: string;
  message_type: string;
  created_at: string;
  /** Optional metadata JSON — when present may contain a `raw` field
   *  with the original unstripped agent output (including `<think>`,
   *  `<plan>` etc.) for a collapsible "thinking" section. */
  metadata?: string | null;
}

export interface SendMessageResponse {
  user_message: GroupChatMessage;
  agent_messages: GroupChatMessage[];
}

// ── GroupChat Tasks ──────────────────────────────────────

export type GroupChatTaskStatus =
  | "pending"
  | "decomposing"
  | "executing"
  | "aggregating"
  | "awaiting_approval"
  | "approved"
  | "completed"
  | "failed";

export type GroupChatSubtaskStatus =
  | "pending"
  | "running"
  | "done"
  | "failed";

export interface GroupChatSubtask {
  id: string;
  task_id: string;
  description: string;
  assigned_agent_id: string;
  status: GroupChatSubtaskStatus;
  result?: string | null;
  started_at?: string | null;
  completed_at?: string | null;
  /** Review verdict (passed/failed + comment) issued by the leader.
   *  `undefined` / `null` when not yet reviewed. The agent on revision
   *  reads this to know what to fix. */
  review?: GroupChatSubtaskReview | null;
}

export interface GroupChatSubtaskReview {
  passed: boolean;
  comment: string;
  reviewer_id: string;
  reviewed_at: string;
}

export interface GroupChatTask {
  id: string;
  room_id: string;
  leader_agent_id: string;
  original_prompt: string;
  status: GroupChatTaskStatus;
  plan_json?: string | null;
  final_report?: string | null;
  approved_at?: string | null;
  created_at: string;
  completed_at?: string | null;
  subtasks: GroupChatSubtask[];
}

export interface GroupChatActiveTaskResponse {
  /** `null` when no active task exists for the room. */
  task: GroupChatTask | null;
}

export interface GroupChatAnnouncementResponse {
  announcement: string | null;
  announcement_published_at: string | null;
}

/** Response shape from PATCH /api/groupchat/rooms/{id}/description.
 *  Mirror of GroupChatAnnouncementResponse for the description field. */
export interface GroupChatDescriptionResponse {
  description: string | null;
}

export const listGroupChatRooms = async (): Promise<GroupChatRoom[]> =>
  fetchJSON<GroupChatRoom[]>("/api/groupchat/rooms");

/** Fetch a single room by id. Cheaper than `listGroupChatRooms` when
 *  you already know the id (e.g. the settings drawer opening on the
 *  active room). Returns `null` when the server replies 404. Other
 *  errors propagate so the caller's existing error-handling path can
 *  surface a toast. Uses a raw fetch because the shared `fetchJSON`
 *  helper throws a generic Error without status information. */
export const getGroupChatRoom = async (roomId: string): Promise<GroupChatRoom | null> => {
  const response = await fetch(apiUrl(`/api/groupchat/rooms/${encodeURIComponent(roomId)}`));
  if (response.status === 404) return null;
  if (!response.ok) {
    throw new Error(`Failed to fetch room ${roomId}: HTTP ${response.status}`);
  }
  return (await response.json()) as GroupChatRoom;
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

/** Add a single agent to a room. Re-inviting an existing member is a
 *  no-op (idempotent); pass `role: "observer"` to invite as a
 *  read-only participant.
 *
 *  Note: the "leader" role is intentionally NOT exposed here. Every
 *  room has a system-built-in 项目总管 auto-created on `create_room`;
 *  users cannot promote a user-invited agent to leader. (The
 *  server-side `invite_agent` handler still maps `"leader"` to
 *  AgentRole::Leader for backward compatibility, but the UI never
 *  sends it.) */
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

/** Remove an agent from a room. The agent itself is preserved on disk
 *  (config, history, skills) — only the room's membership row is
 *  deleted. Idempotent: kicking a non-member returns 200. */
export const kickAgentFromRoom = async (
  roomId: string,
  agentId: string,
): Promise<void> => {
  await fetchVoid(
    `/api/groupchat/rooms/${encodeURIComponent(roomId)}/agents/${encodeURIComponent(agentId)}`,
    { method: "DELETE" },
  );
};

/** Set or clear the per-room model override for a single agent.
 *  Pass `null` (or empty string) to clear — the agent reverts to its
 *  default model. The override is per-room, so the same agent can
 *  speak with one model in "Code Review" and another in "Brainstorm". */
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

/** Set or clear the per-room display name (别名) for a single agent.
 *  Pass `null` (or empty string) to clear — the agent reverts to its
 *  global name. The alias is per-room, so renaming an agent in this
 *  room doesn't touch its catalog entry or other rooms.
 *
 *  Server normalises whitespace: leading/trailing spaces are trimmed,
 *  blank strings are stored as `null`. */
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

/** Fetch the full member list for a room — one row per agent
 *  (including the built-in `__leader__`) with role / model_override
 *  / display_name / session_id. Used by the room-member dialog to
 *  hydrate alias + role + model in a single round trip. Returns
 *  `null` (or throws) if the room doesn't exist. */
export const fetchRoomMembers = async (
  roomId: string,
): Promise<GroupChatMember[]> => {
  return fetchJSON<GroupChatMember[]>(
    `/api/groupchat/rooms/${encodeURIComponent(roomId)}/members`,
  );
};

/** P3 — flat rank projection returned by GET /ranks. The `tier`
 *  string is one of `bronze | silver | gold | platinum | diamond |
 *  star | king | king_legend` (lowercase); `tier_cn` is the
 *  human-readable Chinese tier name (青铜 / 白银 / …); `badge_emoji`
 *  is the rank glyph rendered next to the member's name. */
export interface GroupChatRank {
  agent_id: string;
  room_id: string;
  score: number;
  tier: string;
  tier_cn: string;
  badge_emoji: string;
  total_tasks: number;
  consecutive_no_violation: number;
  /** P1.5 — set when tier changed (升段/掉段) during last adjustment */
  previous_tier?: string | null;
}

/** Fetch rank (勋章) snapshots for every member of a room, sorted
 *  by score DESC. The leader row is included so the dialog can show
 *  the synthetic leader's tier alongside user-invited agents.
 *  Returns an empty list if the room has no ranks yet (shouldn't
 *  happen post-P3 — every room auto-seeds a leader rank on creation). */
export const fetchRoomRanks = async (
  roomId: string,
): Promise<GroupChatRank[]> => {
  return fetchJSON<GroupChatRank[]>(
    `/api/groupchat/rooms/${encodeURIComponent(roomId)}/ranks`,
  );
};

/** P1.4 — cast a vote on a room member (+20 or -20). */
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

/** P4 — dispute a violation record. */
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

/** P4 — overturn a disputed violation. Returns the restored deduction. */
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

/** M4 — paginated message loading with cursor support. */
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

/** Publish (or replace) the room's pinned announcement. Pass an empty
 *  string to clear — both verbs (POST with empty body, DELETE) clear
 *  the announcement on the server side. */
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

/** Publish (or replace) the room's free-form description. Pass an
 *  empty string to clear — both verbs (PATCH with empty body, DELETE)
 *  clear the description on the server side. Whitespace-only strings
 *  are coerced to empty server-side, so the UI doesn't need to. */
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

/** Switch the room's chat mode between "task_collab" (leader dispatches
 *  tasks) and "free_chat" (members respond freely). Returns the updated
 *  chat_mode value from the server. */
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

/** Get the active task (with subtasks) for a room. Returns
 *  `{ task: null }` when no in-flight task exists — the panel
 *  renders an "idle" state in that case. */
export const getActiveGroupChatTask = async (
  roomId: string,
): Promise<GroupChatActiveTaskResponse> =>
  fetchJSON<GroupChatActiveTaskResponse>(
    `/api/groupchat/rooms/${encodeURIComponent(roomId)}/tasks/active`,
  );

/** List all tasks for a room, newest first. Powers the history view
 *  inside the task panel drawer. */
export const listGroupChatTasks = async (
  roomId: string,
): Promise<GroupChatTask[]> =>
  fetchJSON<GroupChatTask[]>(
    `/api/groupchat/rooms/${encodeURIComponent(roomId)}/tasks`,
  );

/** Update a subtask's status / result. The server stamps `started_at`
 *  on the running transition and `completed_at` on terminal transitions
 *  (done / failed) — the client only needs to send `status` + `result`. */
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

export interface GroupChatRoomRules {
  global_constraints: string[];
  role_permissions: Record<string, {
    denied_tools: string[];
    can_delegate: boolean;
    can_write_memory: boolean;
    constraints: string;
  }>;
  tool_policy: {
    mode: string;
    blacklisted: string[];
    whitelisted: string[];
  };
  penalty_policy: {
    initial_score: number;
    reflection_threshold: number;
    can_fire: boolean;
    auto_rebuild: boolean;
  };
}

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

/** Per-room runtime settings. Backed by `RoomSettings` on the server
 *  (see `crates/hermix-gateway/src/groupchat/persistence.rs`).
 *
 *  - `allow_idle_chat`: when `true`, members periodically self-prompt
 *    with reflection / experience-sharing topics (`IdleChatManager`).
 *    Default is `false` (opt-in) so a freshly created room is quiet.
 *  - `idle_interval_secs`: how often the idle timer ticks; default 300s.
 *  - `max_concurrent_agents`: cap on simultaneously active agent
 *    responses per user message (decomposition fan-out bound).
 *  - `require_leader_approval`: the leader must approve a task before
 *    subtasks are dispatched to members. */
export interface GroupChatRoomSettings {
  allow_idle_chat: boolean;
  idle_interval_secs: number;
  max_concurrent_agents: number;
  require_leader_approval: boolean;
}

export const getGroupChatRoomSettings = async (
  roomId: string,
): Promise<GroupChatRoomSettings> =>
  fetchJSON<GroupChatRoomSettings>(
    `/api/groupchat/rooms/${encodeURIComponent(roomId)}/settings`,
  );

/** Update one or more fields. Only the fields present in `partial` are
 *  sent — the server's `UpdateSettingsRequest` is `Option<…>` for every
 *  key, so unspecified values are preserved on disk. The idle timer
 *  is re-evaluated server-side as a side effect of PUT, so toggling
 *  `allow_idle_chat` from off→on starts it, on→off stops it. */
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

// ── Server-only routes (no frontend call needed) ─────────────────
//
// The following backend routes exist but are intentionally NOT called
// from the frontend. Documented here so developers don't add redundant
// calls and know these are server-internal:
//
// | Route | Method | Purpose | Why server-only |
// |-------|--------|---------|-----------------|
// | /v1/protocol/schema | GET | Protocol JSON schema | Debug only; available via curl |
// | /v1/telemetry/client-event | POST | Client telemetry ingest | Server-internal; frontend uses WS for events |
// | /metrics | GET | Prometheus metrics | For monitoring tools, not the SPA |
// | /v1/metrics | GET | Metrics snapshot | For monitoring tools |
// | /v1/sessions/{id}/lineage | GET | Session lineage tree | Available via API but not yet surfaced in UI |
// | /v1/commands | POST | Execute a command | SECURITY: no auth, should be disabled in production |
// | /api/researcher/* | GET/POST | Researcher loop (5 routes) | Experimental feature, not yet wired to UI |
// | /api/migration/* | GET/POST | Config migration (4 routes) | Server-side migration, not user-facing |
// | /ws/consciousness | GET(WS) | Consciousness ECG stream | Experimental; not yet connected to frontend |
