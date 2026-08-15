//! Retrieval paths: FTS / LIKE / facts search and hit hydration.
use chrono::{DateTime, Utc};

use rusqlite::params;

use super::*;

use crate::memory_layer::{MemoryHit, MemoryQuery, Result};
use crate::salience::salience;

impl EmbeddedMemoryBackend {
    pub(crate) fn fts_search_visible(&self, text: &str, limit: usize, session_id: Option<&str>) -> Result<Vec<String>> {
        let conn = self.conn.lock();
        let (sql, session_param) = if session_id.is_some() {
            (
                "SELECT f.id FROM frames_fts f
                 JOIN frames fr ON fr.id = f.id
                 WHERE frames_fts MATCH ?1 AND fr.visibility = 1 AND fr.source = ?2
                 LIMIT ?3",
                true,
            )
        } else {
            (
                "SELECT f.id FROM frames_fts f
                 JOIN frames fr ON fr.id = f.id
                 WHERE frames_fts MATCH ?1 AND fr.visibility = 1
                 LIMIT ?2",
                false,
            )
        };
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => return Ok(Vec::new()),
        };
        // Build an FTS5 MATCH expression from the text. We split on
        // whitespace and quote each token; tokens that look like FTS5
        // operators are skipped.
        //CJK query expansion — pre-segment the query with the same
        // bigram splitter used at write time so single-word queries
        // like "喜欢" still hit "我喜欢 Rust" (which is stored as
        // "我 喜欢 Rust" after bigram preprocessing).
        let bigrammed = cjk_bigram_segment(text);
        let tokens: Vec<String> = bigrammed
            .split_whitespace()
            .filter(|t| t.chars().count() >= 1)
            .filter(|t| !["AND", "OR", "NOT", "NEAR"].contains(t))
            .map(|t| {
                // strip non-word chars (keep CJK — alphanumeric filter
                // would strip them)
                t.chars()
                    .filter(|c| c.is_alphanumeric() || *c == '_' || is_cjk(*c))
                    .collect::<String>()
            })
            .filter(|t| !t.is_empty())
            .map(|t| format!("\"{t}\""))
            .collect();
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        let q = tokens.join(" OR ");
        let mut rows = if session_param {
            let sid = session_id.unwrap_or("");
            match stmt.query(params![q, sid, limit as i64]) {
                Ok(r) => r,
                Err(_) => return Ok(Vec::new()),
            }
        } else {
            match stmt.query(params![q, limit as i64]) {
                Ok(r) => r,
                Err(_) => return Ok(Vec::new()),
            }
        };
        let mut ids = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            ids.push(id);
        }
        Ok(ids)
    }


    pub(crate) fn like_search_visible(&self, text: &str, limit: usize, session_id: Option<&str>) -> Result<Vec<String>> {
        let conn = self.conn.lock();
        let (sql, session_param) = if session_id.is_some() {
            (
                "SELECT id FROM frames WHERE visibility = 1 AND source = ?1
                 AND (payload LIKE ?2 OR tags LIKE ?2)
                 LIMIT ?3",
                true,
            )
        } else {
            (
                "SELECT id FROM frames WHERE visibility = 1
                 AND (payload LIKE ?1 OR tags LIKE ?1)
                 LIMIT ?2",
                false,
            )
        };
        let mut stmt = conn.prepare(sql)?;
        let pattern = format!("%{}%", text);
        let mut rows = if session_param {
            let sid = session_id.unwrap_or("");
            stmt.query(params![sid, pattern, limit as i64])?
        } else {
            stmt.query(params![pattern, limit as i64])?
        };
        let mut ids = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            ids.push(id);
        }
        Ok(ids)
    }

    /// L2 Semantic: search the `facts` table for keyword hits.
    /// Same MATCH expression builder as `fts_search_visible` but
    /// against `facts_fts`.
    pub(crate) fn fts_facts_visible(&self, text: &str, limit: usize, session_id: Option<&str>) -> Result<Vec<String>> {
        let conn = self.conn.lock();
        let now = chrono::Utc::now().to_rfc3339();
        let (sql, session_param) = if session_id.is_some() {
            (
                "SELECT fa.id FROM facts_fts fa
                 JOIN facts f ON f.id = fa.id
                 WHERE facts_fts MATCH ?1 AND f.visibility = 1 AND f.source = ?2
                   AND (f.expires_at IS NULL OR f.expires_at >= ?3)
                 LIMIT ?4",
                true,
            )
        } else {
            (
                "SELECT fa.id FROM facts_fts fa
                 JOIN facts f ON f.id = fa.id
                 WHERE facts_fts MATCH ?1 AND f.visibility = 1
                   AND (f.expires_at IS NULL OR f.expires_at >= ?2)
                 LIMIT ?3",
                false,
            )
        };
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => return Ok(Vec::new()),
        };
        let bigrammed = cjk_bigram_segment(text);
        let tokens: Vec<String> = bigrammed
            .split_whitespace()
            .filter(|t| t.chars().count() >= 1)
            .filter(|t| !["AND", "OR", "NOT", "NEAR"].contains(t))
            .map(|t| {
                t.chars()
                    .filter(|c| c.is_alphanumeric() || *c == '_' || is_cjk(*c))
                    .collect::<String>()
            })
            .filter(|t| !t.is_empty())
            .map(|t| format!("\"{t}\""))
            .collect();
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        let q = tokens.join(" OR ");
        let mut rows = if session_param {
            let sid = session_id.unwrap_or("");
            match stmt.query(params![q, sid, &now, limit as i64]) {
                Ok(r) => r,
                Err(_) => return Ok(Vec::new()),
            }
        } else {
            match stmt.query(params![q, &now, limit as i64]) {
                Ok(r) => r,
                Err(_) => return Ok(Vec::new()),
            }
        };
        let mut ids = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            ids.push(id);
        }
        Ok(ids)
    }

    /// Convert an id (which may point at either `frames` or
    /// `facts`) to a `MemoryHit`. Returns `Ok(None)` when the id is
    /// unknown or the row is hidden.
    pub(crate) fn hydrate_visible(
        &self,
        id_s: &str,
        query: &MemoryQuery,
        now: DateTime<Utc>,
    ) -> Result<Option<MemoryHit>> {
        let conn = self.conn.lock();
        // Try frames first.
        let frame_row: Option<(String, String, String, f64, String, String, Option<String>, String, i64, i64)> = conn
            .query_row(
                "SELECT id, payload, state, salience, created_at, updated_at,
                        parent_id, tags, pin_count, touch_count
                 FROM frames WHERE id = ?1 AND visibility = 1",
                params![id_s],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                        r.get(9)?,
                    ))
                },
            )
            .ok();
        if let Some((id, payload, _state, salience_field, created_at, updated_at, _parent, tags, pin_count, touch_count)) = frame_row {
            let created_at = parse_dt(&created_at);
            let updated_at = parse_dt(&updated_at);
            if let Some(tw) = &query.time_window {
                if !tw.contains(created_at) {
                    return Ok(None);
                }
            }
            let payload_json: serde_json::Value = serde_json::from_str(&payload).unwrap_or(serde_json::Value::String(payload));
            let s = salience(
                updated_at,
                touch_count as u32,
                pin_count as u32,
                now,
                &self.salience_cfg,
            );
            return Ok(Some(MemoryHit {
                id,
                content: extract_text(&payload_json),
                salience: salience_field as f32 * 0.6 + s.total * 0.4,
                created_at,
                metadata: serde_json::json!({
                    "tags": serde_json::from_str::<Vec<String>>(&tags).unwrap_or_default(),
                    "kind": "frame",
                    "pin_count": pin_count,
                    "touch_count": touch_count,
                }),
                source: self.name().to_string(),
            }));
        }
        // Try facts next.
        let fact_row: Option<(String, String, f64, String, String, String, i64, Option<String>)> = conn
            .query_row(
                "SELECT id, payload, salience, created_at, updated_at, tags, access_count, expires_at
                 FROM facts WHERE id = ?1 AND visibility = 1",
                params![id_s],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                    ))
                },
            )
            .ok();
        if let Some((id, payload, salience_field, created_at, updated_at, tags, access_count, expires_at)) = fact_row {
            let created_at = parse_dt(&created_at);
            if let Some(tw) = &query.time_window {
                if !tw.contains(created_at) {
                    return Ok(None);
                }
            }
            // Skip expired facts (expires_at < now). This is a
            // defensive check — the janitor should have already
            // soft-hidden them, but a race is possible.
            if let Some(ref exp) = expires_at {
                let exp_dt = parse_dt(exp);
                if exp_dt <= now {
                    return Ok(None);
                }
            }
            let payload_json: serde_json::Value = serde_json::from_str(&payload).unwrap_or(serde_json::Value::String(payload));
            // Apply salience time decay using the same formula as
            // frames: `stored * 0.6 + computed * 0.4` where
            // `computed` uses recency × freq × pin. For facts,
            // `freq` is derived from `access_count` (every recall
            // bumps it) and `pin` defaults to 0 (facts are not
            // pinned today). This means old, rarely-recalled facts
            // naturally sink below the salience floor and get
            // filtered out at recall time.
            let updated_at_dt = parse_dt(&updated_at);
            let computed = salience(
                updated_at_dt,
                access_count.clamp(0, i64::MAX) as u32,
                0, // facts have no pin_count
                now,
                &self.salience_cfg,
            );
            // Access-count boost: log-saturating so extreme
            // counts don't dominate. Applied after decay so
            // a frequently-cited fact still floats up even
            // when its recency has partially decayed.
            let access_boost = (access_count as f32).log10().max(0.0) * 0.05;
            let final_salience = (salience_field as f32 * 0.6 + computed.total * 0.4 + access_boost).min(1.0);
            return Ok(Some(MemoryHit {
                id,
                content: extract_text(&payload_json),
                salience: final_salience,
                created_at,
                metadata: serde_json::json!({
                    "tags": serde_json::from_str::<Vec<String>>(&tags).unwrap_or_default(),
                    "kind": "fact",
                    "access_count": access_count,
                }),
                source: self.name().to_string(),
            }));
        }
        Ok(None)
    }

    /// Soft-hide any visible `memory_write` row whose JSON payload
    /// `content` matches the given substring. Returns the number of
    /// rows hidden. Used by the `remove` action of the
    /// `on_memory_write` hook.
    pub(crate) fn soft_hide_matching_memory_write(&self, target: &str, content_substr: &str) -> usize {
        let substr = content_substr.trim();
        if substr.is_empty() {
            return 0;
        }
        let conn = self.conn.lock();
        let pattern = format!("%{}%", substr);
        let now = Utc::now().to_rfc3339();
        // Match on payload LIKE + tags LIKE. The agent loop passes the
        // user-supplied `target` parameter verbatim. For memory_tool
        // rows the tag is `target:<name>`, for turn rows it is
        // `session:<sid>`, for session_end rows it is `session`.
        // We accept any of those forms without prepending another
        // prefix (the agent already knows the convention).
        let n = conn
            .execute(
                "UPDATE frames
                 SET visibility = 0, hidden_at = ?1
                 WHERE visibility = 1
                   AND tags LIKE ?2
                   AND payload LIKE ?3",
                params![now, format!("%{}%", target), pattern],
            )
            .unwrap_or(0);
        n
    }
}
