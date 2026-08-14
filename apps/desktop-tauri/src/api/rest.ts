// Hermix Desktop API Client — REST 运行时（从 api.ts 拆分；类型见 ./types，WS 见 ./ws）
import type { ChatMessage, Session, AutomationTask } from "../types";
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

/** Fetch a single room by id. */
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
