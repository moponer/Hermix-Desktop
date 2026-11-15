//! Shared request/response types for the dashboard API.

use serde::{Deserialize, Serialize};

// ── Sessions ────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub source: Option<String>,
    pub model: Option<String>,
    pub title: Option<String>,
    pub started_at: f64,
    pub ended_at: Option<f64>,
    pub last_active: f64,
    pub is_active: bool,
    pub message_count: u32,
    pub tool_call_count: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub preview: Option<String>,
}

// 删掉 `PaginatedSessions`（2026-10-30）：全仓零引用，是"分页契约曾设计但
// 从未落地"的残留。真正的分页见 `routes.rs` 的 `handle`（sessions 硬上限 +
// messages 的 `limit`/`tail_skip` 窗口）。

#[derive(Debug, Serialize)]
pub struct SessionMessage {
    pub role: String,
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct ToolCallInfo {
    pub id: String,
    pub function: ToolCallFunction,
}

#[derive(Debug, Serialize)]
pub struct ToolCallFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Serialize)]
pub struct SessionMessagesResponse {
    pub session_id: String,
    pub messages: Vec<SessionMessage>,
}

#[derive(Debug, Serialize)]
pub struct SessionSearchResult {
    pub session_id: String,
    pub snippet: String,
    pub role: Option<String>,
    pub source: Option<String>,
    pub model: Option<String>,
    pub session_started: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct SessionSearchResponse {
    pub results: Vec<SessionSearchResult>,
}

// ── Config ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ConfigUpdate {
    pub config: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct RawConfigUpdate {
    pub yaml_text: String,
}

#[derive(Debug, Serialize)]
pub struct ModelInfoResponse {
    pub model: String,
    pub provider: String,
    pub auto_context_length: u32,
    pub config_context_length: u32,
    pub effective_context_length: u32,
    pub capabilities: serde_json::Value,
}

// ── Env ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct EnvVarInfo {
    pub is_set: bool,
    pub redacted_value: Option<String>,
    pub description: String,
    pub url: Option<String>,
    pub category: String,
    pub is_password: bool,
    pub tools: Vec<String>,
    pub advanced: bool,
}

#[derive(Debug, Deserialize)]
pub struct EnvVarUpdate {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub struct EnvVarDelete {
    pub key: String,
}

#[derive(Debug, Deserialize)]
pub struct EnvVarReveal {
    pub key: String,
}

// ── Logs ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct LogsResponse {
    pub file: String,
    pub lines: Vec<String>,
}

// ── Cron ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct CronJobResponse {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub prompt: String,
    pub schedule: CronScheduleInfo,
    pub schedule_display: String,
    pub enabled: bool,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deliver: Option<String>,
    pub last_run_at: Option<String>,
    pub next_run_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CronScheduleInfo {
    pub kind: String,
    pub expr: String,
    pub display: String,
}

#[derive(Debug, Deserialize)]
pub struct CronJobCreate {
    pub prompt: String,
    pub schedule: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub deliver: Option<String>,
}

// ── Skills ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct SkillInfo {
    pub name: String,
    /// 技能的中文展示名（来自 frontmatter `display_name`）。
    /// 缺失时前端回落到 `name`。
    pub display_name: Option<String>,
    pub description: String,
    pub category: String,
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct SkillToggle {
    pub name: String,
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct SkillContentResponse {
    pub name: String,
    pub content: String,
    pub category: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ToolsetInfo {
    pub name: String,
    pub label: String,
    pub description: String,
    pub enabled: bool,
    pub configured: bool,
    pub tools: Vec<String>,
}

/// Request body for `PUT /api/tools/toolsets/enabled`. The frontend
/// sends the *full* intended set as both lists (enabled + disabled)
/// so the server does not need to merge with disk state — the
/// server's `EnabledToolsetState` is overwritten with the
/// intersection: `enabled` is honoured, every other registered
/// toolset is disabled. This makes the call idempotent and
/// avoids the "stale disk state wins" footgun where the UI thinks
/// `web` is off but the file still has it as enabled.
#[derive(Debug, Deserialize, Default)]
pub struct ToolsetToggle {
    #[serde(default)]
    pub enabled: Vec<String>,
    #[serde(default)]
    pub disabled: Vec<String>,
}

/// Response body for `PUT /api/tools/toolsets/enabled` — returns
/// the canonical post-toggle state so the UI does not have to
/// re-fetch the full list to confirm the write.
#[derive(Debug, Serialize)]
pub struct ToolsetToggleResult {
    /// Toolset names now enabled (after the toggle was applied).
    pub enabled: Vec<String>,
    /// Number of toolset names the UI asked to flip but which
    /// the registry does not actually have (typos, removed
    /// toolsets, etc.). Surfaced to the UI so it can show a
    /// "these toolsets do not exist" warning.
    pub unknown: Vec<String>,
}

// ── Analytics ───────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct AnalyticsDailyEntry {
    pub day: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// now sourced from `token_usage.cache_read_tokens`
    /// instead of being hardcoded to 0. Lets the daily curve layer
    /// cache-hit as a stacked band on top of input/output.
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub reasoning_tokens: u64,
    pub estimated_cost: f64,
    pub actual_cost: f64,
    /// when the provider reports 0 cache reads
    /// but is *known* to support caching, this holds the estimated hit
    /// rate (from provider config `cache_hit_rate_estimate`). `None`
    /// means "use the real `cache_read_tokens` value" (which may be 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cache_hit_rate: Option<f64>,
    pub sessions: u32,
}

#[derive(Debug, Serialize)]
pub struct AnalyticsModelEntry {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// per-model cache + reasoning totals so each row
    /// can show its own cache hit rate.
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub reasoning_tokens: u64,
    pub estimated_cost: f64,
    pub sessions: u32,
    /// calls in this bucket whose provider-reported counts failed the
    /// local-anchor sanity check (values already replaced by the local
    /// estimate) — drives the "上报口径异常" badge.
    #[serde(default)]
    pub untrusted_calls: u64,
    /// true when the pricing DB knows this model (or routes it as
    /// subscription-included). false ⇒ the UI shows "—" instead of
    /// a misleading $0.0000.
    #[serde(default)]
    pub cost_known: bool,
    /// estimated cache hit rate when the
    /// provider under-reports cache usage. `None` ⇒ use real data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cache_hit_rate: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct AnalyticsTotals {
    pub total_input: u64,
    pub total_output: u64,
    /// now aggregated from real `token_usage` rows.
    /// Distinct from `total_cache_creation` so we can chart "saved"
    /// vs "spent-on-caching" separately.
    pub total_cache_read: u64,
    pub total_cache_creation: u64,
    pub total_reasoning: u64,
    pub total_estimated_cost: f64,
    pub total_actual_cost: f64,
    pub total_sessions: u32,
    /// provider reports that failed the local-anchor sanity check
    /// across all models (values already replaced upstream).
    #[serde(default)]
    pub total_untrusted_calls: u64,
    /// true when at least one model in the window has known pricing —
    /// lets the UI distinguish "no cost data" from "genuinely free".
    #[serde(default)]
    pub cost_known: bool,
    /// estimated overall cache hit rate when
    /// the active providers under-report cache usage. `None` ⇒ real.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cache_hit_rate: Option<f64>,
    /// compression telemetry.
    /// `total_compactions` = number of compression child sessions
    /// spawned in the window; `total_archived_turns` = the preserved
    /// (not lost) history folded into those summaries.
    pub total_compactions: u32,
    pub total_archived_turns: u64,
}

#[derive(Debug, Serialize)]
pub struct AnalyticsSkillEntry {
    pub skill: String,
    pub view_count: u32,
    pub manage_count: u32,
    pub total_count: u32,
    pub percentage: f64,
    pub last_used_at: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct AnalyticsSkillsSummary {
    pub total_skill_loads: u32,
    pub total_skill_edits: u32,
    pub total_skill_actions: u32,
    pub distinct_skills_used: u32,
}

/// per-tool call counts derived from `messages.tool_calls` JSON
/// inside the date window. The frontend renders this as a horizontal
/// bar list next to the daily curve.
#[derive(Debug, Serialize)]
pub struct AnalyticsToolEntry {
    pub tool: String,
    pub call_count: u64,
    pub session_count: u32,
    /// Last 7-day rolling call frequency (per-day average).
    pub daily_avg: f64,
}

#[derive(Debug, Serialize)]
pub struct AnalyticsResponse {
    pub daily: Vec<AnalyticsDailyEntry>,
    pub by_model: Vec<AnalyticsModelEntry>,
    pub totals: AnalyticsTotals,
    pub skills: AnalyticsSkillsResponse,
    /// real tool-call aggregation parsed from
    /// `messages.tool_calls` JSON. Sorted by `call_count` desc.
    pub by_tool: Vec<AnalyticsToolEntry>,
}

#[derive(Debug, Serialize)]
pub struct AnalyticsSkillsResponse {
    pub summary: AnalyticsSkillsSummary,
    pub top_skills: Vec<AnalyticsSkillEntry>,
}

/// structured error body returned by
/// `GET /api/analytics/usage` when the underlying aggregation fails
/// (DB unavailable, query error, …). Replaces the old behaviour of
/// swallowing the error into an all-zero `200 OK` response, so the
/// frontend can show the real cause instead of "you used 0 tokens".
#[derive(Debug, Serialize)]
pub struct AnalyticsErrorResponse {
    /// Short machine-readable tag, e.g. "aggregate_usage_failed".
    pub error: String,
    /// Human-readable underlying cause (the `AgentError` string).
    pub cause: String,
}

/// diagnostic payload for `GET /api/analytics/debug`.
/// Surfaces the DB path, per-table existence/row-count/sample, and the
/// applied migration list so a user can pinpoint where the stats
/// pipeline is broken without poking at SQLite manually.
#[derive(Debug, Serialize)]
pub struct AnalyticsDebugResponse {
    pub db_path: String,
    pub tables: AnalyticsDebugTables,
    pub migrations: Vec<AnalyticsDebugMigration>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnalyticsDebugTables {
    pub sessions: AnalyticsDebugTable,
    pub messages: AnalyticsDebugTable,
    pub token_usage: AnalyticsDebugTable,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnalyticsDebugTable {
    pub exists: bool,
    /// Row count, or `None` if the table could not be queried.
    pub row_count: Option<u64>,
    /// Up to 3 recent sample rows (as pretty JSON), empty if none.
    pub sample: Vec<serde_json::Value>,
    /// Error string if the table query failed (otherwise `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AnalyticsDebugMigration {
    pub id: String,
    pub applied_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

// ── Common ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct OkResponse {
    pub ok: bool,
}
