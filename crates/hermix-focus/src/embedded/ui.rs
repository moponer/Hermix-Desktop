//! UI read surface for the memory-management page.
//!
//! Read-only listing / search / soft-delete / pin over the same
//! `memory.db` the agent loop writes. The server dashboard layer
//! (`hermix-server/src/dashboard/memory_inspect.rs`) assembles the
//! graph from these rows; this module owns nothing but SQL and the
//! serializable row types, so the wire schema lives in one place.

use serde::Serialize;

use crate::memory_layer::Result;

use super::EmbeddedMemoryBackend;

/// One graph node row as returned by the listing queries. The `kind`
/// is the node category the UI colors by: for frames it is the write
/// provenance (`turn` / `memory_tool` / `session` / `delegation` /
/// `compression_backfill` / `focus`), for facts `fact`, plus
/// `reflection` rows. `daily` days and `md` entries are synthesized
/// by the server layer, not by SQL.
#[derive(Debug, Clone, Serialize)]
pub struct UiMemoryRow {
    pub id: String,
    pub kind: String,
    /// Payload text as stored (JSON string for frames/facts). The
    /// server layer truncates — this module returns the raw text so
    /// the detail view can render it in full.
    pub text: String,
    pub salience: f32,
    pub created_at: String,
    /// `frames.parent_id` (delegation hierarchy), None elsewhere.
    pub parent_id: Option<String>,
    /// `frames.state` / reflections `kind` — kept raw for the UI.
    pub state: Option<String>,
    /// `facts.canonical_key` — lets the server layer link promoted
    /// md entries to their fact nodes. None for frames/reflections.
    pub key: Option<String>,
    pub session_id: Option<String>,
    pub pin_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UiMemoryStats {
    pub frames: i64,
    pub facts: i64,
    pub reflections: i64,
    /// Approximate on-disk size of memory.db in bytes.
    pub db_bytes: i64,
    /// Visible rows created per local calendar day, newest first.
    pub per_day: Vec<(String, i64)>,
}

impl EmbeddedMemoryBackend {
    /// Newest-first listing of visible frames, capped. `limit == 0`
    /// returns an empty vec (same contract as the other listings).
    pub fn ui_list_frames(&self, limit: u32) -> Result<Vec<UiMemoryRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, source, payload, salience, created_at, parent_id, state, tags, pin_count
             FROM frames
             WHERE visibility = 1
             ORDER BY created_at DESC, id DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit as i64], |row| {
            let tags_s: String = row.get(7)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, f64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                tags_s,
                row.get::<_, i64>(8)?,
            ))
        })?;
        Ok(rows
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(|(id, source, text, sal, created, parent, state, tags, pin)| UiMemoryRow {
                id,
                kind: source,
                text,
                salience: sal as f32,
                created_at: created,
                parent_id: parent,
                state,
                session_id: session_tag(&tags),
                pin_count: pin,
                key: None,
            })
            .collect())
    }

    /// Newest-first listing of visible facts, capped.
    pub fn ui_list_facts(&self, limit: u32) -> Result<Vec<UiMemoryRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, canonical_key, payload, salience, created_at, tags, 0
             FROM facts
             WHERE visibility = 1
             ORDER BY created_at DESC, id DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit as i64], |row| {
            let tags_s: String = row.get(5)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, f64>(3)?,
                row.get::<_, String>(4)?,
                tags_s,
                row.get::<_, i64>(6)?,
            ))
        })?;
        Ok(rows
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(|(id, key, text, sal, created, tags, pin)| UiMemoryRow {
                text,
                id,
                kind: "fact".to_string(),
                salience: sal as f32,
                created_at: created,
                parent_id: None,
                state: None,
                session_id: session_tag(&tags),
                pin_count: pin,
                key: Some(key),
            })
            .collect())
    }

    /// Newest-first listing of visible reflections (rowid ids).
    pub fn ui_list_reflections(&self, limit: u32) -> Result<Vec<UiMemoryRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, kind, content, salience, created_at, session_id
             FROM reflections
             WHERE visibility = 1
             ORDER BY id DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit as i64], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, f64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        Ok(rows
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(|(id, kind, text, sal, created, session)| UiMemoryRow {
                id: id.to_string(),
                kind: format!("reflection:{kind}"),
                text,
                salience: sal as f32,
                created_at: created,
                parent_id: None,
                state: None,
                session_id: Some(session),
                pin_count: 0,
                key: None,
            })
            .collect())
    }

    /// Search across facts (FTS5, bigram-segmented like the write
    /// path), frames and reflections (LIKE fallback). Returns the
    /// same row shape as the listings so the UI can render one list.
    pub fn ui_search(&self, query: &str, limit: u32) -> Result<Vec<UiMemoryRow>> {
        let q = query.trim();
        if q.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock();
        let mut out = search_facts(&conn, q, limit)?;
        out.extend(search_frames(&conn, q, limit)?);
        out.extend(search_reflections(&conn, q, limit)?);
        drop(conn);
        out.truncate(limit as usize);
        Ok(out)
    }

    /// Whether a visible fact with this canonical key exists. The
    /// session-end promotion uses it to distinguish "new memory"
    /// (worth a chat event) from a re-assert of an already-promoted
    /// entry (silent salience bump).
    pub fn has_fact(&self, canonical_key: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let found: Option<String> = conn
            .query_row(
                "SELECT id FROM facts WHERE canonical_key = ?1 AND visibility = 1 LIMIT 1",
                rusqlite::params![canonical_key],
                |r| r.get(0),
            )
            .ok();
        Ok(found.is_some())
    }

    /// Soft-hide a fact (reversible via SQL, mirrors
    /// `soft_hide_frame`). Also drops the FTS row so search stops
    /// surfacing it.
    pub fn ui_soft_hide_fact(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE facts SET visibility = 0 WHERE id = ?1 AND visibility = 1",
            rusqlite::params![id],
        )?;
        let _ = conn.execute("DELETE FROM facts_fts WHERE id = ?1", rusqlite::params![id]);
        Ok(n > 0)
    }

    /// Soft-hide a reflection row.
    pub fn ui_soft_hide_reflection(&self, id: i64) -> Result<bool> {
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE reflections SET visibility = 0 WHERE id = ?1 AND visibility = 1",
            rusqlite::params![id],
        )?;
        Ok(n > 0)
    }

    /// Pin / unpin a frame (`pin_count` drives the salience pin
    /// weight). Returns whether any row changed.
    pub fn ui_set_frame_pin(&self, id: &str, pinned: bool) -> Result<bool> {
        let conn = self.conn.lock();
        let n = if pinned {
            conn.execute(
                "UPDATE frames SET pin_count = pin_count + 1 WHERE id = ?1 AND visibility = 1",
                rusqlite::params![id],
            )?
        } else {
            conn.execute(
                "UPDATE frames SET pin_count = MAX(pin_count - 1, 0) WHERE id = ?1",
                rusqlite::params![id],
            )?
        };
        Ok(n > 0)
    }

    /// Aggregate counts for the memory-management overview row.
    pub fn ui_stats(&self) -> Result<UiMemoryStats> {
        let conn = self.conn.lock();
        let count = |sql: &str| -> i64 {
            conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0)
        };
        let frames = count("SELECT COUNT(*) FROM frames WHERE visibility = 1");
        let facts = count("SELECT COUNT(*) FROM facts WHERE visibility = 1");
        let reflections = count("SELECT COUNT(*) FROM reflections WHERE visibility = 1");
        let db_bytes = count("SELECT COALESCE(page_count * page_size, 0) FROM pragma_page_count(), pragma_page_size()");
        let mut per_day: Vec<(String, i64)> = Vec::new();
        if let Ok(mut stmt) = conn.prepare(
            "SELECT substr(created_at, 1, 10) AS day, COUNT(*) AS n
             FROM frames WHERE visibility = 1
             GROUP BY day ORDER BY day DESC LIMIT 30",
        ) {
            if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))) {
                for r in rows.flatten() {
                    per_day.push(r);
                }
            }
        }
        Ok(UiMemoryStats {
            frames,
            facts,
            reflections,
            db_bytes,
            per_day,
        })
    }
}

/// Extract the `session:<id>` tag from a tags JSON array, if present.
fn session_tag(tags_json: &str) -> Option<String> {
    let tags: Vec<String> = serde_json::from_str(tags_json).ok()?;
    tags.into_iter()
        .find(|t| t.starts_with("session:"))
        .map(|t| t["session:".len()..].to_string())
}

/// SQL LIKE pattern with `%`/`_`/`\` escaped.
fn like_pattern(q: &str) -> String {
    format!(
        "%{}%",
        q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
    )
}

/// FTS5 facts search. The facts_fts index is bigram-segmented on the
/// write path, so CJK substrings match without wildcards.
fn search_facts(
    conn: &rusqlite::Connection,
    q: &str,
    limit: u32,
) -> Result<Vec<UiMemoryRow>> {
    let mut out = Vec::new();
    let segmented = super::cjk_bigram_segment(q);
    let Ok(mut stmt) = conn.prepare(
        "SELECT f.id, f.canonical_key, f.payload, f.salience, f.created_at, f.tags
         FROM facts_fts t JOIN facts f ON f.id = t.id
         WHERE facts_fts MATCH ?1 AND f.visibility = 1
         ORDER BY f.salience DESC
         LIMIT ?2",
    ) else {
        return Ok(out);
    };
    let Ok(rows) = stmt.query_map(rusqlite::params![segmented, limit as i64], |row| {
        let tags_s: String = row.get(5)?;
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, f64>(3)?,
            row.get::<_, String>(4)?,
            tags_s,
        ))
    }) else {
        return Ok(out);
    };
    for r in rows.flatten() {
        out.push(UiMemoryRow {
            text: r.2,
            id: r.0,
            kind: "fact".to_string(),
            salience: r.3 as f32,
            created_at: r.4,
            parent_id: None,
            state: None,
            session_id: session_tag(&r.5),
            pin_count: 0,
            key: Some(r.1),
        });
    }
    Ok(out)
}

/// Substring search over visible frame payloads.
fn search_frames(
    conn: &rusqlite::Connection,
    q: &str,
    limit: u32,
) -> Result<Vec<UiMemoryRow>> {
    let mut out = Vec::new();
    let like = like_pattern(q);
    let Ok(mut stmt) = conn.prepare(
        "SELECT id, source, payload, salience, created_at, parent_id, state, tags, pin_count
         FROM frames
         WHERE visibility = 1 AND payload LIKE ?1 ESCAPE '\\'
         ORDER BY created_at DESC
         LIMIT ?2",
    ) else {
        return Ok(out);
    };
    let Ok(rows) = stmt.query_map(rusqlite::params![like, limit as i64], |row| {
        let tags_s: String = row.get(7)?;
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, f64>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<String>>(6)?,
            tags_s,
            row.get::<_, i64>(8)?,
        ))
    }) else {
        return Ok(out);
    };
    for r in rows.flatten() {
        out.push(UiMemoryRow {
            text: r.2,
            id: r.0,
            kind: r.1,
            salience: r.3 as f32,
            created_at: r.4,
            parent_id: r.5,
            state: r.6,
            session_id: session_tag(&r.7),
            pin_count: r.8,
            key: None,
        });
    }
    Ok(out)
}

/// Substring search over visible reflections.
fn search_reflections(
    conn: &rusqlite::Connection,
    q: &str,
    limit: u32,
) -> Result<Vec<UiMemoryRow>> {
    let mut out = Vec::new();
    let like = like_pattern(q);
    let Ok(mut stmt) = conn.prepare(
        "SELECT id, kind, content, salience, created_at, session_id
         FROM reflections
         WHERE visibility = 1 AND content LIKE ?1 ESCAPE '\\'
         ORDER BY id DESC
         LIMIT ?2",
    ) else {
        return Ok(out);
    };
    let Ok(rows) = stmt.query_map(rusqlite::params![like, limit as i64], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, f64>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
        ))
    }) else {
        return Ok(out);
    };
    for r in rows.flatten() {
        out.push(UiMemoryRow {
            id: r.0.to_string(),
            kind: format!("reflection:{}", r.1),
            text: r.2,
            salience: r.3 as f32,
            created_at: r.4,
            parent_id: None,
            state: None,
            session_id: Some(r.5),
            pin_count: 0,
            key: None,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::EmbeddedMemoryBackend;

    fn sample_rows(backend: &EmbeddedMemoryBackend) {
        backend
            .record_turn("你好", "你好！", "sess-a", 1)
            .unwrap();
        backend
            .record_turn("Rust 生命周期", "讲一下", "sess-a", 2)
            .unwrap();
        backend
            .upsert_fact(
                "user.name:张三",
                serde_json::json!({ "text": "张三" }),
                0.8,
                &["session:sess-a".to_string()],
                "curation",
            )
            .unwrap();
        backend
            .add_reflection("sess-a", "what_went_well", "用户喜欢简洁", 0.4, &[], 2, "template")
            .unwrap();
    }

    #[test]
    fn list_rows_shapes_kinds() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        sample_rows(&b);
        let facts = b.ui_list_facts(100).unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].kind, "fact");
        assert_eq!(facts[0].key.as_deref(), Some("user.name:张三"));
        assert_eq!(facts[0].session_id.as_deref(), Some("sess-a"));

        let frames = b.ui_list_frames(100).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].kind, "turn");

        let refl = b.ui_list_reflections(100).unwrap();
        assert_eq!(refl.len(), 1);
        assert!(refl[0].kind.starts_with("reflection:"));
    }

    #[test]
    fn limit_zero_is_empty_and_cap_applies() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        sample_rows(&b);
        assert!(b.ui_list_frames(0).unwrap().is_empty());
        assert_eq!(b.ui_list_frames(1).unwrap().len(), 1);
    }

    #[test]
    fn search_hits_facts_frames_reflections() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        sample_rows(&b);
        let hits = b.ui_search("张三", 10).unwrap();
        assert!(hits.iter().any(|h| h.kind == "fact"));
        // LIKE fallback over frame payloads.
        let hits = b.ui_search("生命周期", 10).unwrap();
        assert!(hits.iter().any(|h| h.kind == "turn"));
        let hits = b.ui_search("简洁", 10).unwrap();
        assert!(hits.iter().any(|h| h.kind.starts_with("reflection")));
    }

    #[test]
    fn soft_hide_fact_removes_from_list_and_search() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        sample_rows(&b);
        let facts = b.ui_list_facts(10).unwrap();
        let id = facts[0].id.clone();
        assert!(b.ui_soft_hide_fact(&id).unwrap());
        assert!(b.ui_list_facts(10).unwrap().is_empty());
        assert!(b.ui_search("张三", 10).unwrap().is_empty());
    }

    #[test]
    fn has_fact_matches_canonical_key() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        sample_rows(&b);
        assert!(b.has_fact("user.name:张三").unwrap());
        assert!(!b.has_fact("user.name:李四").unwrap());
        // Soft-hidden fact no longer counts.
        let facts = b.ui_list_facts(10).unwrap();
        b.ui_soft_hide_fact(&facts[0].id).unwrap();
        assert!(!b.has_fact("user.name:张三").unwrap());
    }

    #[test]
    fn frame_pin_roundtrip() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        sample_rows(&b);
        let frames = b.ui_list_frames(10).unwrap();
        let id = frames[0].id.clone();
        assert!(b.ui_set_frame_pin(&id, true).unwrap());
        assert!(b.ui_list_frames(10).unwrap()[0].pin_count >= 1);
        assert!(b.ui_set_frame_pin(&id, false).unwrap());
        assert_eq!(b.ui_list_frames(10).unwrap()[0].pin_count, 0);
    }

    #[test]
    fn stats_counts_and_per_day() {
        let b = EmbeddedMemoryBackend::open_in_memory().unwrap();
        sample_rows(&b);
        let s = b.ui_stats().unwrap();
        assert_eq!(s.frames, 2);
        assert_eq!(s.facts, 1);
        assert_eq!(s.reflections, 1);
        assert!(!s.per_day.is_empty());
    }
}
