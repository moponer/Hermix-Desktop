//! Usage analytics endpoints.
//!
//! `GET /api/analytics/usage` — real token/tool/skill aggregates.
//! `GET /api/analytics/debug` — diagnostic view of the DB state.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use hermix_agent::session_persistence::SkillUsageAggregate;
use hermix_config::GatewayConfig;
use rusqlite::types::Value as SqlValue;
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
/// Returns real aggregates from `token_usage` + `messages.tool_calls`.
/// Errors are NO LONGER swallowed into an all-zero 200 : a
/// failed aggregation returns `500` with a structured
/// `AnalyticsErrorResponse` so the frontend can show the real cause.
pub async fn get_usage(
    State(state): State<HttpServerState>,
    Query(params): Query<AnalyticsParams>,
) -> Result<Json<AnalyticsResponse>, (StatusCode, Json<AnalyticsErrorResponse>)> {
    // aggregate_usage 是同步 SQLite（9+ 条查询，其中两条把整表内容拉到 Rust
    // 侧逐行解析 JSON）。直接在 handler 里跑会占住 tokio worker —— 与分析页
    // 并发的 WS 心跳/流式推送都跟着抖。搬到阻塞池。
    let persistence = state.session_persistence.clone();
    let days = params.days;
    let agg = match tokio::task::spawn_blocking(move || persistence.aggregate_usage(days)).await {
        Ok(Ok(a)) => a,
        Ok(Err(e)) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AnalyticsErrorResponse {
                    error: "aggregate_usage_failed".to_string(),
                    cause: e.to_string(),
                }),
            ));
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AnalyticsErrorResponse {
                    error: "aggregate_usage_failed".to_string(),
                    cause: format!("blocking task failed: {e}"),
                }),
            ));
        }
    };

    let config = &state.config;

    //Per-model estimated cache hit rate : only when the
    // provider reports 0 cache reads but is known to support caching.
    let by_model: Vec<AnalyticsModelEntry> = agg
        .by_model
        .into_iter()
        .map(|m| {
            let est = if m.cache_read_tokens > 0 {
                None
            } else {
                estimate_cache_for_model(&m.model, config)
            };
            AnalyticsModelEntry {
                cost_known: hermix_intelligence::has_known_pricing(&m.model, None, None),
                model: m.model,
                input_tokens: m.input_tokens,
                output_tokens: m.output_tokens,
                cache_read_tokens: m.cache_read_tokens,
                cache_creation_tokens: m.cache_creation_tokens,
                reasoning_tokens: m.reasoning_tokens,
                sessions: m.sessions.max(0) as u32,
                estimated_cost: m.estimated_cost,
                untrusted_calls: m.untrusted_calls,
                estimated_cache_hit_rate: est,
            }
        })
        .collect();

    let daily: Vec<AnalyticsDailyEntry> = agg
        .daily
        .into_iter()
        .map(|d| {
            // Daily estimate: mirror the global estimate when this day
            // has no real cache reads.
            let totals_est = if agg.total_cache_read_tokens > 0 {
                None
            } else {
                by_model
                    .iter()
                    .find_map(|m| m.estimated_cache_hit_rate)
            };
            let est = if d.cache_read_tokens > 0 {
                None
            } else {
                totals_est
            };
            AnalyticsDailyEntry {
                day: d.day,
                input_tokens: d.input_tokens,
                output_tokens: d.output_tokens,
                cache_read_tokens: d.cache_read_tokens,
                cache_creation_tokens: d.cache_creation_tokens,
                reasoning_tokens: d.reasoning_tokens,
                estimated_cost: d.estimated_cost,
                actual_cost: 0.0,
                estimated_cache_hit_rate: est,
                sessions: d.sessions,
            }
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

    //Global estimated cache hit rate : used by the totals card
    // when the real cache_read total is 0.
    let totals_est = if agg.total_cache_read_tokens > 0 {
        None
    } else {
        by_model.iter().find_map(|m| m.estimated_cache_hit_rate)
    };

    // Any model with known pricing ⇒ the totals cost figure means
    // something; all-unknown ⇒ the UI shows "—" instead of $0.0000.
    let totals_cost_known = by_model.iter().any(|m| m.cost_known);

    Ok(Json(AnalyticsResponse {
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
            total_untrusted_calls: agg.total_untrusted_calls,
            cost_known: totals_cost_known,
            total_compactions: agg.total_compactions.max(0) as u32,
            total_archived_turns: agg.total_archived_turns,
            estimated_cache_hit_rate: totals_est,
        },
        //real skill usage, no longer hardcoded to zeros.
        skills: build_skills(&agg.skills),
        by_tool,
    }))
}

/// Build the skills response from the real aggregate .
fn build_skills(skills: &SkillUsageAggregate) -> AnalyticsSkillsResponse {
    let total_activity: u64 = skills
        .entries
        .iter()
        .map(|e| e.view_count + e.manage_count)
        .sum::<u64>()
        .max(1);
    let top_skills = skills
        .entries
        .iter()
        .map(|e| {
            let tc = e.view_count + e.manage_count;
            AnalyticsSkillEntry {
                skill: e.skill.clone(),
                view_count: e.view_count as u32,
                manage_count: e.manage_count as u32,
                total_count: tc as u32,
                percentage: (tc as f64 / total_activity as f64) * 100.0,
                last_used_at: e
                    .last_used_at
                    .as_ref()
                    .and_then(|s| {
                        chrono::DateTime::<chrono::FixedOffset>::parse_from_rfc3339(s)
                            .ok()
                            .map(|dt| dt.timestamp() as f64)
                    }),
            }
        })
        .collect();
    AnalyticsSkillsResponse {
        summary: AnalyticsSkillsSummary {
            total_skill_loads: skills.total_views as u32,
            total_skill_edits: skills.total_manages as u32,
            total_skill_actions: (skills.total_views + skills.total_manages) as u32,
            distinct_skills_used: skills.distinct_skills as u32,
        },
        top_skills,
    }
}

/// GET /api/analytics/debug
/// one-shot diagnostic of the stats DB. Surfaces the DB path,
/// per-table existence/row-count/sample, and the applied migrations so
/// a user can pinpoint where the pipeline is broken without opening
/// SQLite by hand.
pub async fn get_debug(
    State(state): State<HttpServerState>,
) -> Json<AnalyticsDebugResponse> {
    state.session_persistence.ensure_db().ok();
    let db_path = state.session_persistence.db_path().to_path_buf();
    let conn = rusqlite::Connection::open(&db_path).ok();

    let (tables, migrations) = match &conn {
        Some(c) => (
            AnalyticsDebugTables {
                sessions: debug_table(c, "sessions"),
                messages: debug_table(c, "messages"),
                token_usage: debug_table(c, "token_usage"),
            },
            debug_migrations(c),
        ),
        None => {
            let err_table = AnalyticsDebugTable {
                exists: false,
                row_count: None,
                sample: vec![],
                error: Some("cannot open database".to_string()),
            };
            (AnalyticsDebugTables { sessions: err_table.clone(), messages: err_table.clone(), token_usage: err_table }, vec![])
        }
    };

    Json(AnalyticsDebugResponse {
        db_path: db_path.to_string_lossy().to_string(),
        tables,
        migrations,
    })
}

fn debug_table(conn: &rusqlite::Connection, table: &str) -> AnalyticsDebugTable {
    let mut out = AnalyticsDebugTable {
        exists: false,
        row_count: None,
        sample: vec![],
        error: None,
    };
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
            rusqlite::params![table],
            |row| row.get(0),
        )
        .unwrap_or(false);
    out.exists = exists;
    if !exists {
        return out;
    }
    match conn.query_row(
        &format!("SELECT COUNT(*) FROM \"{table}\""),
        [],
        |row| row.get::<_, i64>(0),
    ) {
        Ok(c) => out.row_count = Some(c.max(0) as u64),
        Err(e) => {
            out.error = Some(e.to_string());
            return out;
        }
    }
    match conn.prepare(&format!(
        "SELECT * FROM \"{table}\" ORDER BY rowid DESC LIMIT 3"
    )) {
        Ok(mut stmt) => {
            let col_count = stmt.column_count();
            let col_names: Vec<String> = (0..col_count)
                .map(|i| stmt.column_name(i).unwrap_or("?").to_string())
                .collect();
            match stmt
                .query_map([], |row| {
                    let mut obj = serde_json::Map::new();
                    for i in 0..col_count {
                        let name = col_names[i].clone();
                        let val: SqlValue = row.get(i)?;
                        let json_val = match val {
                            SqlValue::Null => serde_json::Value::Null,
                            SqlValue::Integer(i) => serde_json::Value::from(i),
                            SqlValue::Real(r) => serde_json::Value::from(r),
                            SqlValue::Text(t) => serde_json::Value::String(t),
                            SqlValue::Blob(b) => {
                                serde_json::Value::String(format!("<blob {} bytes>", b.len()))
                            }
                        };
                        obj.insert(name, json_val);
                    }
                    Ok(serde_json::Value::Object(obj))
                })
                .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            {
                Ok(samples) => out.sample = samples,
                Err(e) => out.error = Some(e.to_string()),
            }
        }
        Err(e) => out.error = Some(e.to_string()),
    }
    out
}

fn debug_migrations(conn: &rusqlite::Connection) -> Vec<AnalyticsDebugMigration> {
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='schema_migrations'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(false);
    if !exists {
        return vec![];
    }
    conn.prepare("SELECT version, name, applied_at FROM schema_migrations ORDER BY version")
        .and_then(|mut stmt| {
            stmt.query_map([], |row| {
                Ok(AnalyticsDebugMigration {
                    id: row.get::<_, i64>(0).map(|v| v.to_string()).unwrap_or_default(),
                    applied_at: row.get(1).ok(),
                    status: Some("ok".to_string()),
                })
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        })
        .unwrap_or_default()
}

/// Static fallback cache-hit estimates keyed by model-name substring,
/// used when a provider under-reports cache usage. Overridable per
/// provider via `LlmProviderConfig.cache_hit_rate_estimate` in config.
fn default_cache_estimate(model: &str) -> Option<f64> {
    let m = model.to_lowercase();
    if m.contains("minimax") {
        return Some(0.65);
    }
    if m.contains("claude") {
        return Some(0.70);
    }
    if m.contains("gpt-4") || m.contains("gpt4") {
        return Some(0.50);
    }
    if m.contains("deepseek") {
        return Some(0.40);
    }
    if m.contains("qwen") {
        return Some(0.45);
    }
    None
}

/// Resolve an estimated cache hit rate for a model: prefer a provider
/// config override, else fall back to the static default map.
fn estimate_cache_for_model(model: &str, config: &GatewayConfig) -> Option<f64> {
    let m = model.to_lowercase();
    for (key, prov) in &config.llm_providers {
        if let Some(est) = prov.cache_hit_rate_estimate {
            let k = key.to_lowercase();
            let matches = m.contains(&k)
                || prov
                    .model
                    .as_deref()
                    .map(|pm| m.contains(&pm.to_lowercase()))
                    .unwrap_or(false);
            if matches {
                return Some(est);
            }
        }
    }
    default_cache_estimate(model)
}
