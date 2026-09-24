//! Durable sink backed by SQLite, via `hermix-store`.
//!
//! This is what turns [`crate::Draft::commit`] from an in-memory
//! protocol into an actual durability guarantee: the entries are
//! written inside one transaction, so a crash either leaves all of
//! them or none of them.

use hermix_store::{OpenMode, Store, StoreError};

use crate::Entry;

/// Writes session entries into a `session_entries` table.
pub struct SqliteSink {
    store: Store,
    session_id: String,
}

impl SqliteSink {
    /// Open (or create) the table and bind the sink to one session.
    pub fn new(store: Store, session_id: impl Into<String>) -> Result<Self, StoreError> {
        let session_id = session_id.into();
        store.write(|conn| {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS session_entries (
                    session_id      TEXT NOT NULL,
                    seq             INTEGER NOT NULL,
                    kind            TEXT NOT NULL,
                    payload         TEXT NOT NULL,
                    by_task_id      TEXT,
                    conversation_id TEXT,
                    PRIMARY KEY (session_id, seq)
                );",
            )
        });
        Ok(Self { store, session_id })
    }

    /// Open the sink directly on a database file.
    pub fn open(path: impl AsRef<std::path::Path>, session_id: impl Into<String>) -> Result<Self, StoreError> {
        let store = Store::open(path, OpenMode::Write)?;
        Self::new(store, session_id)
    }

    /// Write a batch atomically. Pass this to
    /// [`crate::Draft::commit`].
    pub fn write(&self, entries: &[Entry]) -> Result<(), String> {
        if entries.is_empty() {
            return Ok(());
        }
        let session_id = self.session_id.clone();
        let Some(result) = self.store.write(|conn| {
            let tx = match conn.transaction() {
                Ok(t) => t,
                Err(e) => return Err(e.to_string()),
            };
            {
                let mut stmt = match tx.prepare(
                    "INSERT OR REPLACE INTO session_entries
                     (session_id, seq, kind, payload, by_task_id, conversation_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                ) {
                    Ok(s) => s,
                    Err(e) => return Err(e.to_string()),
                };
                for e in entries {
                    let payload = match serde_json::to_string(&e.payload) {
                        Ok(p) => p,
                        Err(err) => return Err(err.to_string()),
                    };
                    if let Err(err) = stmt.execute(rusqlite::params![
                        &session_id,
                        e.seq as i64,
                        &e.kind,
                        &payload,
                        e.by_task_id.as_deref(),
                        e.conversation_id.as_deref()
                    ]) {
                        return Err(err.to_string());
                    }
                }
            }
            tx.commit().map_err(|e| e.to_string())
        }) else {
            return Err("session sink: connection lock poisoned".to_string());
        };
        result
    }

    /// Read every persisted entry for this session, in sequence order.
    /// Used to rebuild a session after a restart.
    pub fn load(&self) -> Result<Vec<Entry>, String> {
        let session_id = self.session_id.clone();
        let Some(result) = self.store.read(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT seq, kind, payload, by_task_id, conversation_id
                     FROM session_entries
                     WHERE session_id = ?1 ORDER BY seq",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(rusqlite::params![&session_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                })
                .map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            for r in rows {
                let (seq, kind, payload, by_task_id, conversation_id) =
                    r.map_err(|e| e.to_string())?;
                let payload: serde_json::Value =
                    serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null);
                out.push(Entry {
                    seq: seq.max(0) as u64,
                    kind,
                    payload,
                    by_task_id,
                    conversation_id,
                });
            }
            Ok(out)
        }) else {
            return Err("session sink: connection lock poisoned".to_string());
        };
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Session;

    #[test]
    fn committed_entries_survive_in_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let sink = SqliteSink::open(dir.path().join("s.db"), "sess-1").unwrap();

        let s = Session::new();
        let mut d = s.begin().unwrap();
        d.append("user_text", serde_json::json!({"text": "hello"}));
        d.append("assistant_text", serde_json::json!({"text": "hi"}));
        d.commit(|entries| sink.write(entries)).unwrap();

        let loaded = sink.load().unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].seq, 1);
        assert_eq!(loaded[0].kind, "user_text");
        assert_eq!(loaded[1].payload["text"], "hi");
    }

    #[test]
    fn a_session_can_be_rebuilt_from_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("s.db");

        let sink = SqliteSink::open(&db, "sess-1").unwrap();
        let s = Session::new();
        let mut d = s.begin().unwrap();
        d.append("a", serde_json::json!({"n": 1}));
        d.append("b", serde_json::json!({"n": 2}));
        d.commit(|entries| sink.write(entries)).unwrap();
        drop(sink);

        // Restart: same file, new process, new Session object.
        let sink2 = SqliteSink::open(&db, "sess-1").unwrap();
        let restored = Session::from_entries(sink2.load().unwrap());
        assert_eq!(restored.committed().len(), 2);

        let mut d = restored.begin().unwrap();
        d.append("c", serde_json::json!({"n": 3}));
        let rel = d.commit(|entries| sink2.write(entries)).unwrap();
        assert_eq!(rel.entries[0].seq, 3, "sequence must continue after a restart");
    }

    #[test]
    fn two_sessions_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let sink_a = SqliteSink::open(dir.path().join("s.db"), "sess-a").unwrap();
        let sink_b = SqliteSink::open(dir.path().join("s.db"), "sess-b").unwrap();

        let sa = Session::new();
        let mut da = sa.begin().unwrap();
        da.append("only_a", serde_json::json!({}));
        da.commit(|e| sink_a.write(e)).unwrap();

        assert_eq!(sink_b.load().unwrap().len(), 0);
        assert_eq!(sink_a.load().unwrap().len(), 1);
    }

    // ------------------------------------------------------------------
    // P2 EntryRecord 对齐：by_task_id / conversation_id 持久化 round-trip
    // ------------------------------------------------------------------

    #[test]
    fn task_attribution_survives_the_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let sink = SqliteSink::open(dir.path().join("s.db"), "sess-1").unwrap();

        let s = Session::new();
        let mut d = s.begin().unwrap();
        // 用户消息无任务归属。
        d.append("user_text", serde_json::json!({"text": "hi"}));
        // 工具 entry 归属到 task-42（append_with_task）。
        d.append_with_task("tool_call", serde_json::json!({"tool": "read_file"}), Some("task-42".into()));
        d.set_conversation("conv-7");
        d.commit(|entries| sink.write(entries)).unwrap();

        let loaded = sink.load().unwrap();
        assert_eq!(loaded.len(), 2);
        // entry 0：无任务归属 + 有会话归属。
        assert_eq!(loaded[0].by_task_id, None, "用户消息 by_task_id 应为 None");
        assert_eq!(loaded[0].conversation_id.as_deref(), Some("conv-7"));
        // entry 1：任务归属 + 会话归属。
        assert_eq!(loaded[1].by_task_id.as_deref(), Some("task-42"));
        assert_eq!(loaded[1].conversation_id.as_deref(), Some("conv-7"));
    }

    #[test]
    fn entries_without_attribution_default_to_none() {
        // 用普通 append（不填 by_task_id / conversation_id）→ load 回 None。
        let dir = tempfile::tempdir().unwrap();
        let sink = SqliteSink::open(dir.path().join("s.db"), "sess-1").unwrap();

        let s = Session::new();
        let mut d = s.begin().unwrap();
        d.append("system", serde_json::json!({}));
        d.commit(|entries| sink.write(entries)).unwrap();

        let loaded = sink.load().unwrap();
        assert_eq!(loaded[0].by_task_id, None);
        assert_eq!(loaded[0].conversation_id, None);
    }
}
