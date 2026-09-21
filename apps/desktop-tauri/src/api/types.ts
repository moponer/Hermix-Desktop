// Hermix Desktop API — 纯类型声明（从 api.ts 拆分，运行时代码见 ./rest、./ws）
import type { ChatMessage } from "../types";

// ── Orchestration Engine: Task types ────────

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

// ── Automation Engine types ───────────────

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

// ── Todo ─────────────────────────────────────────

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
  /** Id of the workbench owning this session. Absent/null means ordinary chat. */
  workbench_id?: string | null;
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

// ── Archive refs (short-term context unload) ─────

export interface ArchiveRef {
  nodeId: string;
  filePath: string;
  lineRange: [number, number] | null;
  charCount: number;
}

// ── Dynamic tool discovery ────────────────────────

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

// ── Session messages result ───────────────────────

/** Session-level context-usage snapshot returned alongside messages when
 *  restoring a session. `null` when the backend has no persisted snapshot. */
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

// ── Dashboard / model types ───────────────────────

export interface ModelInfo {
  model: string;
  provider: string;
}

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

export interface SkillContent {
  name: string;
  content: string;
  category: string | null;
  description: string | null;
}

export interface ToolsetInfo {
  name: string;
  label: string;
  description: string;
  enabled: boolean;
  configured: boolean;
  tools: string[];
}

export interface ToolsetToggleResult {
  /** Canonical enabled set the server committed (post-toggle). */
  enabled: string[];
  /** Toolset names the UI asked to flip but which the registry
   *  does not actually have. */
  unknown: string[];
}

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

export interface PluginInspect {
  id: string;
  availability: "ready" | "seeded";
  toolset: string | null;
  tools: string[];
  total: number;
}

// ── MCP connectors (McpServerEntry) ─────────────────────────

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

// ── Auto-correct model ──────────────────────────

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

// ── Analytics debug ───────────────────────────────

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

// ── Expert catalog ──────────────────────────────────────────

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

// ── Persona management ─────────────────────────────────────

export interface PersonaTextFile {
  /** Absolute path on disk (or `<builtin:name>` for built-in personas). */
  path: string;
  /** File contents. `null` when the file does not yet exist. */
  content: string | null;
  /** Last-modified epoch millis. `null` if the file does not exist. */
  updated_at: number | null;
}

export interface PersonalitySummary {
  name: string;
  summary: string | null;
  size: number;
  updated_at: number | null;
  is_builtin: boolean;
}

export interface PersonaOverrides {
  system_prompt: string;
  preferences: Record<string, unknown> | null;
}

// ── Custom LLM providers ──────────────────────────

export interface CustomProviderSummary {
  id: string;
  display_name: string | null;
  base_url: string;
  has_key: boolean;
  models: string[];
  context_window?: number;
  model_contexts?: Record<string, number>;
}

export interface CustomProviderCreateRequest {
  id: string;
  display_name?: string;
  base_url: string;
  api_key: string;
  models?: string;
}

export interface CustomProviderFetchModelsResponse {
  // `context_window` 由后端 fetch_models 从 `/v1/models` 响应里探测并写入
  // `LlmProviderConfig.model_contexts`（同时持久化到 config.yaml）。多数 OpenAI
  // 兼容 API 不返回该字段 → `undefined` 时由前端 catalog 兜底或显式 128K 默认。
  models: { id: string; context_window?: number }[];
  error: string | null;
}

export interface CustomProviderTestConnectionResponse {
  ok: boolean;
  status: number;
  error: string | null;
  models: { id: string }[] | null;
}

// ── TickLoop ───────────────────────────────────────

export interface TickLoopStatus {
  running: boolean;
  phase: string;
}
