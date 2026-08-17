//! Usage aggregation: tools, skills, estimates.
use std::path::{Path, PathBuf};

use chrono::Utc;
use hermix_core::{AgentError, Message, MessageRole};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use super::*;

impl SessionPersistence {
    /// Aggregate real session / message / model counts over the last `days` days.
    /// also sums token usage from `token_usage` and
    /// derives a daily curve + per-tool breakdown from
    /// `messages.tool_calls` JSON. Sessions / messages that predate the
    /// `token_usage` migration contribute `0` to token totals — that's
    /// fine, the chart shows the migration date as "0 baseline".
    /// - `total_sessions`: count of sessions updated within the window
    /// - `total_messages`: count of messages created within the window
    /// - `by_model`: `(model_name, session_count, message_count)` tuples
    /// - `recent_sessions`: the 20 most recent session ids
    pub fn aggregate_usage(
        &self,
        days: u32,
    ) -> Result<UsageAggregate, AgentError> {
        self.ensure_db()?;
        let conn = open_with_wal(&self.db_path)?;

        // cutoff 时间：days 天前
        let now = Utc::now();
        let cutoff = now - chrono::Duration::days(days as i64);
        let cutoff_str = cutoff.to_rfc3339();

        // 1) total_sessions — 按 updated_at 过滤
        let total_sessions: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE updated_at >= ?1",
                rusqlite::params![cutoff_str],
                |row| row.get(0),
            )
            .map_err(|e| AgentError::Io(format!("Failed to count sessions: {e}")))?;

        // 2) total_messages — 按 messages.created_at 过滤
        let total_messages: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE created_at >= ?1",
                rusqlite::params![cutoff_str],
                |row| row.get(0),
            )
            .map_err(|e| AgentError::Io(format!("Failed to count messages: {e}")))?;

        // 3) by_model — 按 model 聚合 (now joined with token totals)
        // We LEFT JOIN `token_usage` so legacy sessions without usage
        // rows still appear (with input/output = 0). SUM treats NULLs
        // as 0 in SQLite so the COALESCE below is belt-and-suspenders.
        let mut stmt = conn
            .prepare(
                "SELECT
                    COALESCE(s.model, '(unknown)') AS m,
                    COUNT(DISTINCT s.id) AS session_count,
                    COALESCE(SUM(COALESCE(s.message_count, 0)), 0) AS message_sum,
                    COALESCE(SUM(COALESCE(t.prompt_tokens, 0)), 0) AS input_sum,
                    COALESCE(SUM(COALESCE(t.completion_tokens, 0)), 0) AS output_sum,
                    COALESCE(SUM(COALESCE(t.cache_read_tokens, 0)), 0) AS cache_read_sum,
                    COALESCE(SUM(COALESCE(t.cache_creation_tokens, 0)), 0) AS cache_creation_sum,
                    COALESCE(SUM(COALESCE(t.reasoning_tokens, 0)), 0) AS reasoning_sum,
                    COALESCE(SUM(COALESCE(t.estimated_cost, 0)), 0.0) AS cost_sum
                 FROM sessions s
                 LEFT JOIN token_usage t
                    ON t.session_id = s.id AND t.created_at >= ?1
                 WHERE s.updated_at >= ?1
                 GROUP BY m
                 ORDER BY session_count DESC",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare by_model: {e}")))?;
        let mut by_model: Vec<ModelUsageRow> = stmt
            .query_map(rusqlite::params![cutoff_str], |row| {
                Ok(ModelUsageRow {
                    model: row.get(0)?,
                    sessions: row.get(1)?,
                    message_count: row.get(2)?,
                    input_tokens: row.get::<_, i64>(3)?.max(0) as u64,
                    output_tokens: row.get::<_, i64>(4)?.max(0) as u64,
                    cache_read_tokens: row.get::<_, i64>(5)?.max(0) as u64,
                    cache_creation_tokens: row.get::<_, i64>(6)?.max(0) as u64,
                    reasoning_tokens: row.get::<_, i64>(7)?.max(0) as u64,
                    estimated_cost: row.get::<_, f64>(8).unwrap_or(0.0),
                })
            })
            .map_err(|e| AgentError::Io(format!("Failed to query by_model: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AgentError::Io(format!("Failed to read by_model: {e}")))?;

        // 4) recent_sessions — 最新 20 条
        let mut stmt = conn
            .prepare(
                "SELECT id, COALESCE(title, '(无标题)'), COALESCE(model, '(unknown)'), updated_at
                 FROM sessions
                 ORDER BY updated_at DESC
                 LIMIT 20",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare recent: {e}")))?;
        let recent_sessions: Vec<RecentSessionRow> = stmt
            .query_map([], |row| {
                Ok(RecentSessionRow {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    model: row.get(2)?,
                    updated_at: row.get(3)?,
                })
            })
            .map_err(|e| AgentError::Io(format!("Failed to query recent: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AgentError::Io(format!("Failed to read recent: {e}")))?;

        //5) : token totals. SUM with COALESCE so a NULL `estimated_cost`
        // doesn't propagate and break the f64 aggregator.
        // Bind the token-total query result directly as mutable vars so the
        //legacy fallback (`estimate_usage_from_messages`) can mutate
        // them in place. No dead `= 0` initialiser — that just trips
        // `unused_assignments` since the tuple is overwritten immediately.
        let (
            mut total_input,
            mut total_output,
            total_cache_read,
            total_cache_creation,
            total_reasoning,
            mut total_cost,
        ): (i64, i64, i64, i64, i64, f64) = conn
            .query_row(
                "SELECT
                    COALESCE(SUM(prompt_tokens), 0),
                    COALESCE(SUM(completion_tokens), 0),
                    COALESCE(SUM(cache_read_tokens), 0),
                    COALESCE(SUM(cache_creation_tokens), 0),
                    COALESCE(SUM(reasoning_tokens), 0),
                    COALESCE(SUM(COALESCE(estimated_cost, 0)), 0.0)
                 FROM token_usage
                 WHERE created_at >= ?1",
                rusqlite::params![cutoff_str],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )
            .map_err(|e| AgentError::Io(format!("Failed to sum token_usage: {e}")))?;

        //6) : daily curve. We materialise the full window first
        // (days+1 buckets ending today), then LEFT JOIN against the
        // aggregated rows so missing days show as 0 instead of being
        // absent. Doing this in Rust (not SQL) keeps the SQL
        // portable — we don't have to ship a recursive CTE on every
        // `aggregate_usage` call.
        let mut daily: Vec<DailyUsageRow> = Vec::with_capacity(days as usize + 1);
        for offset in 0..=days {
            let day_start = (now - chrono::Duration::days(offset as i64))
                .date_naive()
                .format("%Y-%m-%d")
                .to_string();
            daily.push(DailyUsageRow {
                day: day_start,
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
                reasoning_tokens: 0,
                estimated_cost: 0.0,
                sessions: 0,
                messages: 0,
            });
        }
        // Reverse so oldest is index 0, today is the last entry — matches
        // how the SVG line-chart reads left-to-right.
        daily.reverse();

        // Daily token rollup.
        let mut stmt = conn
            .prepare(
                "SELECT
                    substr(created_at, 1, 10) AS day,
                    COALESCE(SUM(prompt_tokens), 0),
                    COALESCE(SUM(completion_tokens), 0),
                    COALESCE(SUM(cache_read_tokens), 0),
                    COALESCE(SUM(cache_creation_tokens), 0),
                    COALESCE(SUM(reasoning_tokens), 0),
                    COALESCE(SUM(COALESCE(estimated_cost, 0)), 0.0)
                 FROM token_usage
                 WHERE created_at >= ?1
                 GROUP BY day
                 ORDER BY day ASC",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare daily: {e}")))?;
        let daily_token_rows: Vec<(String, i64, i64, i64, i64, i64, f64)> = stmt
            .query_map(rusqlite::params![cutoff_str], |row| {
                Ok((
                    row.get(0)?, row.get(1)?, row.get(2)?,
                    row.get(3)?, row.get(4)?, row.get(5)?,
                    row.get(6)?,
                ))
            })
            .map_err(|e| AgentError::Io(format!("Failed to query daily: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AgentError::Io(format!("Failed to read daily: {e}")))?;
        for (day, ip, op, cr, cc, rs, cost) in daily_token_rows {
            if let Some(slot) = daily.iter_mut().find(|d| d.day == day) {
                slot.input_tokens = ip.max(0) as u64;
                slot.output_tokens = op.max(0) as u64;
                slot.cache_read_tokens = cr.max(0) as u64;
                slot.cache_creation_tokens = cc.max(0) as u64;
                slot.reasoning_tokens = rs.max(0) as u64;
                slot.estimated_cost = cost;
            }
        }
        // Daily session/message rollup — separate query so we don't
        // try to fold the LEFT JOIN into the token one (SQLite planner
        // gets confused by the OR on the date predicate).
        let mut stmt = conn
            .prepare(
                "SELECT
                    substr(s.updated_at, 1, 10) AS day,
                    COUNT(DISTINCT s.id) AS session_count,
                    COUNT(DISTINCT m.id) AS message_count
                 FROM sessions s
                 LEFT JOIN messages m
                    ON m.session_id = s.id
                    AND substr(m.created_at, 1, 10) = substr(s.updated_at, 1, 10)
                 WHERE s.updated_at >= ?1
                 GROUP BY day
                 ORDER BY day ASC",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare daily sessions: {e}")))?;
        let daily_sess_rows: Vec<(String, i64, i64)> = stmt
            .query_map(rusqlite::params![cutoff_str], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(|e| AgentError::Io(format!("Failed to query daily sessions: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AgentError::Io(format!("Failed to read daily sessions: {e}")))?;
        for (day, sess, msgs) in daily_sess_rows {
            if let Some(slot) = daily.iter_mut().find(|d| d.day == day) {
                slot.sessions = sess.max(0) as u32;
                slot.messages = msgs.max(0) as u32;
            }
        }

        //7) : by_tool aggregation. Parse `messages.tool_calls` JSON
        // (which is `Vec<{function: {name}}>` per assistant message)
        // and tally by `function.name`. Done in Rust rather than
        // SQLite JSON1 because we already have the messages loaded
        // pattern in `flush_messages` — and the cost of a single
        // full-table scan over the date window is bounded by the
        // active message count, which is usually <100k.
        // We build a `HashMap<String, (total_calls, sessions_set_size)>`
        // and convert at the end. `daily_avg` uses trailing-7-day counts
        // (not the full window) so a tool that ran 500 times last week
        // but only 3 this week isn't papered over by ancient noise.
        let by_tool = self.aggregate_tools(&conn, &cutoff_str)?;

        //7b) : per-skill usage. Grouped by target
        // skill name parsed from `skill_view` / `skill_manage` calls.
        let skills = self.aggregate_skills(&conn, &cutoff_str)?;

        //7c) : legacy fallback. If `token_usage`
        // has no rows in the window but there ARE sessions/messages,
        // the pre-install never wrote usage — estimate from
        // message content so the dashboard isn't a wall of zeros.
        // We mutate the already-computed aggregates in place: only
        // fill when the real totals are empty, so modern installs
        // (which DO have token_usage) are untouched.
        let has_real_tokens = total_input > 0 || total_output > 0
            || by_model.iter().any(|m| m.input_tokens > 0 || m.output_tokens > 0);
        if !has_real_tokens && (total_sessions > 0 || total_messages > 0) {
            self.estimate_usage_from_messages(
                &conn,
                &cutoff_str,
                &mut by_model,
                &mut daily,
                &mut total_input,
                &mut total_output,
                &mut total_cost,
            );
        }

        //8) (E4): compression telemetry. Each compression
        // spawns a child session (`parent_session_id` set), so
        // counting those within the window gives "compressions
        // performed" without a synthetic counter.
        let total_compactions: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE parent_session_id IS NOT NULL AND updated_at >= ?1",
                rusqlite::params![cutoff_str],
                |row| row.get(0),
            )
            .map_err(|e| AgentError::Io(format!("Failed to count compressions: {e}")))?;

        // Archived turns = the messages stored inside compression-child
        // sessions. The summary text itself lives in
        // `sessions.compressed_summary` (not as a message row), so every
        // non-system child message is exactly one folded-over turn.
        let total_archived_turns: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages
                 WHERE session_id IN (
                     SELECT id FROM sessions
                     WHERE parent_session_id IS NOT NULL AND updated_at >= ?1
                 )
                 AND role != 'system'",
                rusqlite::params![cutoff_str],
                |row| row.get(0),
            )
            .map_err(|e| AgentError::Io(format!("Failed to count archived turns: {e}")))?;

        Ok(UsageAggregate {
            total_sessions,
            total_messages,
            by_model,
            recent_sessions,
            total_input_tokens: total_input.max(0) as u64,
            total_output_tokens: total_output.max(0) as u64,
            total_cache_read_tokens: total_cache_read.max(0) as u64,
            total_cache_creation_tokens: total_cache_creation.max(0) as u64,
            total_reasoning_tokens: total_reasoning.max(0) as u64,
            total_compactions: total_compactions.max(0) as u64,
            total_archived_turns: total_archived_turns.max(0) as u64,
            total_estimated_cost: total_cost,
            daily,
            by_tool,
            skills,
        })
    }

    /// helper: parse `messages.tool_calls` JSON within `cutoff_str`
    /// window, aggregate by tool name. Kept private so the public API
    /// stays a single `aggregate_usage(days)`.
    /// `tool_calls` JSON shape (mirrors `Message::tool_calls` — flattened
    /// `FunctionCall`):
    /// ```json
    /// [{"id": "call_xxx", "name": "read_file", "arguments": "..."}]
    /// ```
    /// Older OpenAI-style shape `{"function": {"name": ...}}` is also
    /// tolerated so a row written before the schema migration doesn't
    /// drop its counts.
    /// Defensive: malformed rows (no JSON, no name) are silently
    /// skipped — never bubble a JSON parse error out of an analytics
    /// query, the user just sees a slightly lower total.
    pub(crate) fn aggregate_tools(
        &self,
        conn: &rusqlite::Connection,
        cutoff_str: &str,
    ) -> Result<Vec<ToolUsageRow>, AgentError> {
        let mut stmt = conn
            .prepare(
                "SELECT session_id, tool_calls, created_at
                 FROM messages
                 WHERE role = 'assistant'
                   AND tool_calls IS NOT NULL
                   AND tool_calls != ''
                   AND created_at >= ?1",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare tools query: {e}")))?;

        use std::collections::{HashMap, HashSet};
        let mut counts: HashMap<String, u64> = HashMap::new();
        let mut sessions: HashMap<String, HashSet<String>> = HashMap::new();
        // 7-day trailing window for `daily_avg`. We re-use `cutoff_str`
        // semantics (RFC3339) but compare against `created_at` substring
        // (which is also RFC3339) so a lexicographic comparison works.
        let now = Utc::now();
        let trailing7_cutoff = (now - chrono::Duration::days(7)).to_rfc3339();
        let mut trailing_counts: HashMap<String, u64> = HashMap::new();

        let mut rows = stmt
            .query(rusqlite::params![cutoff_str])
            .map_err(|e| AgentError::Io(format!("Failed to query tool rows: {e}")))?;
        while let Some(row) = rows
            .next()
            .map_err(|e| AgentError::Io(format!("Failed to step tool rows: {e}")))?
        {
            let session_id: String = row.get(0).unwrap_or_default();
            let tool_calls_json: String = row.get(1).unwrap_or_default();
            let created_at: String = row.get(2).unwrap_or_default();
            let in_trailing7 = created_at >= trailing7_cutoff;

            let parsed: serde_json::Value = match serde_json::from_str(&tool_calls_json) {
                Ok(v) => v,
                Err(_) => continue, // skip malformed rows
            };
            let arr = match parsed.as_array() {
                Some(a) => a,
                None => continue,
            };
            for tc in arr {
                // Two accepted shapes — flattened `name` (canonical
                // after the ToolCall struct refactor) and the legacy
                // OpenAI `function.name` envelope.
                let name = tc
                    .get("name")
                    .and_then(|n| n.as_str())
                    .or_else(|| {
                        tc.get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                    })
                    .unwrap_or("unknown");
                *counts.entry(name.to_string()).or_insert(0) += 1;
                sessions
                    .entry(name.to_string())
                    .or_insert_with(HashSet::new)
                    .insert(session_id.clone());
                if in_trailing7 {
                    *trailing_counts.entry(name.to_string()).or_insert(0) += 1;
                }
            }
        }
        drop(rows);

        let mut out: Vec<ToolUsageRow> = counts
            .into_iter()
            .map(|(tool, call_count)| {
                let session_count = sessions.get(&tool).map(|s| s.len() as u32).unwrap_or(0);
                let trailing = trailing_counts.get(&tool).copied().unwrap_or(0);
                ToolUsageRow {
                    tool,
                    call_count,
                    session_count,
                    daily_avg: (trailing as f64) / 7.0,
                }
            })
            .collect();
        out.sort_by(|a, b| b.call_count.cmp(&a.call_count));
        Ok(out)
    }

    /// aggregate skill usage from `messages.tool_calls`.
    /// Counts `skill_view` calls (a "view/load") and `skill_manage` calls
    /// (an edit/create/delete) per *target* skill — the target name comes
    /// from the call's `arguments.name`, so the dashboard shows
    /// "code-review was used 47 times", not "skill_manage ran 47 times".
    pub(crate) fn aggregate_skills(
        &self,
        conn: &rusqlite::Connection,
        cutoff_str: &str,
    ) -> Result<SkillUsageAggregate, AgentError> {
        let mut stmt = conn
            .prepare(
                "SELECT tool_calls, created_at
                 FROM messages
                 WHERE role = 'assistant'
                   AND tool_calls IS NOT NULL
                   AND tool_calls != ''
                   AND created_at >= ?1",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare skills query: {e}")))?;

        use std::collections::{HashMap, HashSet};
        let mut views: HashMap<String, u64> = HashMap::new();
        let mut manages: HashMap<String, u64> = HashMap::new();
        let mut last_used: HashMap<String, String> = HashMap::new();

        let mut rows = stmt
            .query(rusqlite::params![cutoff_str])
            .map_err(|e| AgentError::Io(format!("Failed to query skill rows: {e}")))?;
        while let Some(row) = rows
            .next()
            .map_err(|e| AgentError::Io(format!("Failed to step skill rows: {e}")))?
        {
            let tool_calls_json: String = row.get(0).unwrap_or_default();
            let created_at: String = row.get(1).unwrap_or_default();
            let parsed: serde_json::Value = match serde_json::from_str(&tool_calls_json) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let arr = match parsed.as_array() {
                Some(a) => a,
                None => continue,
            };
            for tc in arr {
                let name = tc
                    .get("name")
                    .and_then(|n| n.as_str())
                    .or_else(|| {
                        tc.get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                    });
                let name = match name {
                    Some(n) => n,
                    None => continue,
                };
                let is_view = name == "skill_view";
                let is_manage = name == "skill_manage";
                if !is_view && !is_manage {
                    continue;
                }
                // `arguments` is a JSON *string* (flattened FunctionCall).
                let args_str = tc
                    .get("arguments")
                    .and_then(|a| a.as_str())
                    .or_else(|| {
                        tc.get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(|a| a.as_str())
                    })
                    .unwrap_or("");
                let args: serde_json::Value =
                    serde_json::from_str(args_str).unwrap_or(serde_json::Value::Null);
                let skill = args
                    .get("name")
                    .and_then(|n| n.as_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or("unknown")
                    .to_string();
                if is_view {
                    *views.entry(skill.clone()).or_insert(0) += 1;
                } else {
                    *manages.entry(skill.clone()).or_insert(0) += 1;
                }
                let slot = last_used.entry(skill).or_insert_with(String::new);
                if created_at > *slot {
                    *slot = created_at.clone();
                }
            }
        }
        drop(rows);

        let mut all_skills: HashSet<String> = HashSet::new();
        all_skills.extend(views.keys().cloned());
        all_skills.extend(manages.keys().cloned());
        let distinct = all_skills.len() as u64;
        let mut entries: Vec<SkillUsageRow> = all_skills
            .into_iter()
            .map(|skill| SkillUsageRow {
                skill: skill.clone(),
                view_count: *views.get(&skill).unwrap_or(&0),
                manage_count: *manages.get(&skill).unwrap_or(&0),
                last_used_at: last_used.get(&skill).cloned(),
            })
            .collect();
        entries.sort_by(|a, b| {
            (b.view_count + b.manage_count).cmp(&(a.view_count + a.manage_count))
        });
        entries.truncate(10);

        Ok(SkillUsageAggregate {
            entries,
            total_views: views.values().sum(),
            total_manages: manages.values().sum(),
            distinct_skills: distinct,
        })
    }

    /// legacy fallback for installs that predate the
    /// `token_usage` migration. When the real `token_usage` table is empty
    /// in the window but sessions/messages exist, estimate token counts from
    /// message content so the dashboard isn't a wall of zeros. Heuristic:
    /// ASCII ≈ 0.25 token/char (~4 chars/token); non-ASCII (CJK) ≈ 1.5
    /// tokens/char. Accumulates into the per-model and per-day buckets passed
    /// in by reference, and into the running totals.
    pub(crate) fn estimate_usage_from_messages(
        &self,
        conn: &rusqlite::Connection,
        cutoff_str: &str,
        by_model: &mut Vec<ModelUsageRow>,
        daily: &mut Vec<DailyUsageRow>,
        total_input: &mut i64,
        total_output: &mut i64,
        _total_cost: &mut f64,
    ) {
        let mut stmt = match conn.prepare(
            "SELECT m.content, m.role, COALESCE(s.model, '(unknown)'), substr(m.created_at, 1, 10)
             FROM messages m
             LEFT JOIN sessions s ON s.id = m.session_id
             WHERE m.created_at >= ?1",
        ) {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut rows = match stmt.query(rusqlite::params![cutoff_str]) {
            Ok(r) => r,
            Err(_) => return,
        };

        use std::collections::HashMap;
        let mut model_input: HashMap<String, u64> = HashMap::new();
        let mut model_output: HashMap<String, u64> = HashMap::new();
        let mut day_input: HashMap<String, u64> = HashMap::new();
        let mut day_output: HashMap<String, u64> = HashMap::new();

        while let Ok(Some(row)) = rows.next() {
            let content: String = row.get(0).unwrap_or_default();
            let role: String = row.get(1).unwrap_or_default();
            let model: String = row.get(2).unwrap_or_else(|_| "(unknown)".to_string());
            let day: String = row.get(3).unwrap_or_default();
            let est = estimate_tokens(&content);
            if role == "assistant" {
                *model_output.entry(model.clone()).or_insert(0) += est;
                *day_output.entry(day.clone()).or_insert(0) += est;
                *total_output += est as i64;
            } else {
                *model_input.entry(model.clone()).or_insert(0) += est;
                *day_input.entry(day.clone()).or_insert(0) += est;
                *total_input += est as i64;
            }
        }
        drop(rows);

        // Merge per-model estimates (rows already exist from the real
        // `by_model` query, just with 0 tokens — add the estimate on top).
        for (model, inp) in model_input {
            let out = model_output.get(&model).copied().unwrap_or(0);
            if let Some(slot) = by_model.iter_mut().find(|m| m.model == model) {
                slot.input_tokens += inp;
                slot.output_tokens += out;
            } else {
                by_model.push(ModelUsageRow {
                    model,
                    sessions: 0,
                    message_count: 0,
                    input_tokens: inp,
                    output_tokens: out,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                    reasoning_tokens: 0,
                    estimated_cost: 0.0,
                });
            }
        }
        for (day, inp) in day_input {
            let out = day_output.get(&day).copied().unwrap_or(0);
            if let Some(slot) = daily.iter_mut().find(|d| d.day == day) {
                slot.input_tokens += inp;
                slot.output_tokens += out;
            }
        }
    }
}
