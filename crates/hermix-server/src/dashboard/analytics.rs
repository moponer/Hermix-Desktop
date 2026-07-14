//! Usage analytics endpoint.

use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;

use super::types::*;
use crate::HttpServerState;

#[derive(Debug, Deserialize)]
pub struct AnalyticsParams {
    #[serde(default = "default_days")]
    pub days: u32,
}

fn default_days() -> u32 {
    30
}

/// GET /api/analytics/usage
///
/// P1-3 (2026-06-27): now reads real token totals from `token_usage`
/// and tool-call counts from `messages.tool_calls` JSON, instead of
/// returning zeros. Legacy sessions that predate the `token_usage`
/// migration contribute 0 — the chart shows the migration date as
/// "0 baseline".
pub async fn get_usage(
    State(state): State<HttpServerState>,
    Query(params): Query<AnalyticsParams>,
) -> Json<AnalyticsResponse> {
    // 调持久化层聚合
    let agg = state
        .session_persistence
        .aggregate_usage(params.days)
        .unwrap_or_else(|e| {
            eprintln!("[analytics] aggregate_usage failed: {e}");
            hermix_agent::session_persistence::UsageAggregate {
                total_sessions: 0,
                total_messages: 0,
                by_model: vec![],
                recent_sessions: vec![],
                total_input_tokens: 0,
                total_output_tokens: 0,
                total_cache_read_tokens: 0,
                total_cache_creation_tokens: 0,
                total_reasoning_tokens: 0,
                total_estimated_cost: 0.0,
                daily: vec![],
                by_tool: vec![],
            }
        });

    // 转换为 API 响应格式
    let by_model: Vec<AnalyticsModelEntry> = agg
        .by_model
        .into_iter()
        .map(|m| AnalyticsModelEntry {
            model: m.model,
            input_tokens: m.input_tokens,
            output_tokens: m.output_tokens,
            cache_read_tokens: m.cache_read_tokens,
            cache_creation_tokens: m.cache_creation_tokens,
            reasoning_tokens: m.reasoning_tokens,
            sessions: m.sessions.max(0) as u32,
            estimated_cost: m.estimated_cost,
        })
        .collect();

    let daily: Vec<AnalyticsDailyEntry> = agg
        .daily
        .into_iter()
        .map(|d| AnalyticsDailyEntry {
            day: d.day,
            input_tokens: d.input_tokens,
            output_tokens: d.output_tokens,
            cache_read_tokens: d.cache_read_tokens,
            cache_creation_tokens: d.cache_creation_tokens,
            reasoning_tokens: d.reasoning_tokens,
            estimated_cost: d.estimated_cost,
            actual_cost: 0.0,
            sessions: d.sessions,
        })
        .collect();

    let by_tool: Vec<AnalyticsToolEntry> = agg
        .by_tool
        .into_iter()
        .map(|t| AnalyticsToolEntry {
            tool: t.tool,
            call_count: t.call_count,
            session_count: t.session_count,
            daily_avg: t.daily_avg,
        })
        .collect();

    Json(AnalyticsResponse {
        daily,
        by_model,
        totals: AnalyticsTotals {
            total_input: agg.total_input_tokens,
            total_output: agg.total_output_tokens,
            total_cache_read: agg.total_cache_read_tokens,
            total_cache_creation: agg.total_cache_creation_tokens,
            total_reasoning: agg.total_reasoning_tokens,
            total_estimated_cost: agg.total_estimated_cost,
            total_actual_cost: 0.0,
            total_sessions: agg.total_sessions.max(0) as u32,
        },
        skills: AnalyticsSkillsResponse {
            summary: AnalyticsSkillsSummary {
                total_skill_loads: 0,
                total_skill_edits: 0,
                total_skill_actions: 0,
                distinct_skills_used: 0,
            },
            top_skills: vec![],
        },
        by_tool,
    })
}