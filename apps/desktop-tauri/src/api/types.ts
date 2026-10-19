// Hermix Desktop API — 纯类型声明（从 api.ts 拆分，运行时代码见 ./rest、./ws）
import type { ChatMessage } from "../types";

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

export interface HermesConfig {
  [key: string]: unknown;
}

export interface SkillInfo {
  name: string;
  /** 中文展示名（frontmatter `display_name`）。缺失时界面回落到 `name`。 */
  display_name?: string | null;
  description: string;
  category: string;
  enabled: boolean;
}

export interface PluginSettings {
  mcp_filesystem: boolean;
  mcp_terminal: boolean;
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
  /** 窗口化加载时：完整列表在本次窗口之前还有更早的消息（"加载更早"入口）。 */
  hasMore: boolean;
  /** 组装后的完整列表长度（含压缩归档子会话消息），窗口化时 ≥ messages.length。 */
  totalMessages: number;
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
  /** Bearer token for remote (http) servers — sent as Authorization header. */
  token?: string;
  enabled: boolean;
  createdAt: number;
}

// ── MCP 社区目录（backend /api/mcp/catalog）─────────────────────────

export type CatalogVerdict = "pass" | "warn" | "block";

export interface CatalogScanHit {
  rule: string;
  detail: string;
}

export interface CatalogScan {
  verdict: CatalogVerdict;
  label: string;
  hits: CatalogScanHit[];
  /** 将要执行的原始命令（仅本地包），二次确认时原样展示 */
  commandPreview?: string;
}

export type CatalogInstall =
  | { kind: "remote"; url: string }
  | { kind: "npm"; package: string; version?: string }
  | { kind: "manual"; package: string; version?: string; hint: string };

export interface CatalogEntry {
  id: string;
  title: string;
  description: string;
  version?: string;
  homepage?: string;
  official: boolean;
  install: CatalogInstall;
  requiredEnv: string[];
  titleZh?: string;
  descriptionZh?: string;
  category: string;
  scan: CatalogScan;
}

export interface CatalogResponse {
  items: CatalogEntry[];
}

// 2026-10-04：取消远程拉取后，目录是编译期打包的常量——没有"源"可切，
// 也就没有 CatalogSourceInfo / CatalogSourcesResponse / CatalogProbeResult。
// 连同 /api/mcp/catalog/sources 与 /probe 端点一并从后端删除。

// ── IM 通道状态（/api/channels/status）──────────────────────

export interface ChannelStatusItem {
  platform: string;
  bound: boolean;
  /** 网关是否已连上。bound && !connected = 凭据在但通道没活。 */
  connected: boolean;
  /** 最近一次网关故障原文（连上后清空）。 */
  detail?: string;
  /** 机器人名称（QQ 网关 READY 事件上报）。 */
  name?: string;
}

// ── 工作流（/api/workflows）─────────────────────────────────

export interface WorkflowEntry {
  id: string;
  name: string;
  description?: string;
  /** null/undefined = 全局（对所有项目可见）；否则为绑定的项目目录。 */
  workspace?: string | null;
  prompt: string;
  created_at: number;
  updated_at: number;
  /** "running" | "ok" | "failed" */
  last_status?: string;
  last_run_at?: number;
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

// ── Custom LLM providers ──────────────────────────

export interface CustomProviderSummary {
  id: string;
  display_name: string | null;
  base_url: string;
  has_key: boolean;
  models: string[];
  context_window?: number;
  model_contexts?: Record<string, number>;
  /** 内置免费渠道（`custom-free-*`）：UI 显示"免费源"标记与免责提示。 */
  is_free_channel?: boolean;
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
