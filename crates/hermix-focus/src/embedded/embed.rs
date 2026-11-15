//! Embedding storage and cosine-vector retrieval.
use chrono::{DateTime, Utc};
use rusqlite::params;
use serde_json::Value;

use super::*;

use crate::focus_stack::FocusFrame;
use crate::memory_layer::{MemoryError, MemoryHit, Result};

impl EmbeddedMemoryBackend {
    pub fn upsert_embedding(
        &self,
        id: &str,
        embedding: &[f32],
        model: &str,
    ) -> Result<()> {
        if embedding.is_empty() {
            return Err(MemoryError::Other("empty embedding".into()));
        }
        let bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
        let dim = embedding.len() as i64;
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE frames SET embedding = ?1, embedding_dim = ?2, embedding_model = ?3
             WHERE id = ?4",
            params![bytes, dim, model, id],
        )?;
        if n == 0 {
            return Err(MemoryError::Other(format!("frame {id} not found")));
        }
        Ok(())
    }

    /// L1.3: one-shot convenience — write the frame + its embedding
    /// in a single SQLite transaction. Prefer this over separate
    /// `upsert_frame` + `upsert_embedding` calls so a frame is
    /// never observed without its vector.
    pub fn upsert_frame_with_embedding<T: serde::Serialize + Clone>(
        &self,
        frame: &FocusFrame<T>,
        embedding: &[f32],
        model: &str,
    ) -> Result<()> {
        if embedding.is_empty() {
            return Err(MemoryError::Other("empty embedding".into()));
        }
        let payload = serde_json::to_string(&frame.payload)?;
        let tags_json = serde_json::to_string(&frame.tags)?;
        let state_str = state_to_str(frame.state);
        let created_at = frame.created_at.to_rfc3339();
        let updated_at = frame.updated_at.to_rfc3339();
        let parent_id = frame.parent_id.map(|p| p.to_string());
        let bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
        let dim = embedding.len() as i64;
        let conn = self.conn.lock();
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            r#"INSERT OR REPLACE INTO frames
               (id, payload, state, salience, created_at, updated_at,
                parent_id, tags, pin_count, touch_count,
                embedding, embedding_dim, embedding_model)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"#,
            params![
                frame.id.to_string(),
                payload,
                state_str,
                frame.salience,
                created_at,
                updated_at,
                parent_id,
                tags_json,
                frame.pin_count as i64,
                frame.touch_count as i64,
                bytes,
                dim,
                model,
            ],
        )?;
        let payload_for_index = serde_json::to_value(&frame.payload)?;
        let payload_text = cjk_bigram_segment(&extract_text(&payload_for_index));
        let tags_text = cjk_bigram_segment(&frame.tags.join(" "));
        let _ = tx.execute(
            "INSERT OR REPLACE INTO frames_fts (id, payload_text, tags_text) VALUES (?1, ?2, ?3)",
            params![frame.id.to_string(), payload_text, tags_text],
        );
        tx.commit()?;
        Ok(())
    }

    /// L1.3: cosine top-k. O(N) scan over frames whose stored
    /// `embedding_dim` matches the query; rows from a different
    /// model are skipped when `model_filter` is non-empty.
    /// `min_score` is the cosine floor in [-1, 1].
    pub(crate) fn cosine_topk_impl(
        &self,
        query: &[f32],
        k: usize,
        min_score: f32,
        model_filter: &str,
        session_id: Option<&str>,
    ) -> Result<Vec<(MemoryHit, f32)>> {
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let qdim = query.len() as i64;
        let q_norm = l2_normalize(query);
        let conn = self.conn.lock();
        let (sql, has_sid) = if session_id.is_some() {
            (
                "SELECT id, payload, state, salience, created_at, updated_at,
                        parent_id, tags, pin_count, touch_count,
                        embedding, embedding_dim, embedding_model
                 FROM frames
                 WHERE embedding IS NOT NULL
                   AND embedding_dim = ?1
                   AND source = ?2",
                true,
            )
        } else {
            (
                "SELECT id, payload, state, salience, created_at, updated_at,
                        parent_id, tags, pin_count, touch_count,
                        embedding, embedding_dim, embedding_model
                 FROM frames
                 WHERE embedding IS NOT NULL
                   AND embedding_dim = ?1",
                false,
            )
        };
        let mut stmt = conn.prepare(sql)?;
        let mut rows = if has_sid {
            let sid = session_id.unwrap_or("");
            stmt.query(params![qdim, sid])?
        } else {
            stmt.query(params![qdim])?
        };
        let mut scored: Vec<(MemoryHit, f32)> = Vec::new();
        while let Some(row) = rows.next()? {
            let model_s: Option<String> = row.get(12)?;
            if !model_filter.is_empty() {
                match model_s.as_deref() {
                    Some(m) if m == model_filter => {}
                    _ => continue,
                }
            }
            let blob: Option<Vec<u8>> = row.get(10)?;
            let Some(blob) = blob else { continue };
            // Decode f32 little-endian. We know `blob.len() == 4 * qdim`
            // because we filtered on embedding_dim = qdim, but a
            // corrupt row would silently read past the end. Check
            // and skip if mismatch.
            if blob.len() != (qdim as usize) * 4 {
                continue;
            }
            let vec: Vec<f32> = blob
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            let v_norm = l2_normalize(&vec);
            // cosine = q · v / (||q|| * ||v||) — but we pre-normalized
            // q, so this is just q · v_normalized (||q|| = 1).
            let score = dot(&q_norm, &v_norm);
            if score < min_score {
                continue;
            }
            let id_s: String = row.get(0)?;
            let payload_s: String = row.get(1)?;
            let state_s: String = row.get(2)?;
            // Column 3 = stored salience. We re-rank with cosine score
            // (line 421) and use that as the surfaced salience, so the
            // stored value is intentionally consumed-but-ignored.
            let _stored_salience: f64 = row.get(3)?;
            let created_s: String = row.get(4)?;
            let updated_s: String = row.get(5)?;
            let _parent_id_s: Option<String> = row.get(6)?;
            let tags_s: String = row.get(7)?;
            let pin_count: i64 = row.get(8)?;
            let touch_count: i64 = row.get(9)?;
            let payload_v: serde_json::Value =
                serde_json::from_str(&payload_s).unwrap_or(Value::Null);
            let created_at = DateTime::parse_from_rfc3339(&created_s)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let _ = updated_s; // not surfaced in MemoryHit
            scored.push((
                MemoryHit {
                    id: id_s,
                    content: extract_text(&payload_v),
                    salience: score as f32,
                    created_at,
                    metadata: serde_json::json!({
                        "tags": serde_json::from_str::<Vec<String>>(&tags_s)
                            .unwrap_or_default(),
                        "state": state_s,
                        "pin_count": pin_count,
                        "touch_count": touch_count,
                        "cosine_score": score,
                    }),
                    source: self.name().to_string(),
                },
                score,
            ));
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(k);
        Ok(scored)
    }
}
