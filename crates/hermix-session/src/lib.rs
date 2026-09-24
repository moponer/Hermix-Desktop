//! # hermix-session
//!
//! The session change line and the commit protocol.
//!
//! ## The invariant this crate enforces
//!
//! **Committed state is the only observable state.** Nothing that has
//! not been committed may be published to a client.
//!
//! Today the transport layer is the state carrier: progress is pushed
//! to the WebSocket as it happens and persistence is a side effect
//! that runs afterwards. That inversion is the root cause of the
//! whole family of "it got stuck / it vanished / it doesn't match"
//! reports — when the socket dies, or the process is killed, or a
//! retry lands on a busy session, the in-flight state simply
//! disappears, because it never existed anywhere except the wire.
//!
//! This crate inverts it back. Writing state is a two-phase act:
//!
//! 1. **Hold the line** — [`Session::begin`] grants exclusive access
//!    to the change line. Only one holder at a time.
//! 2. **Draft** — [`Draft::append`] stages entries. Staged entries are
//!    not visible to anyone.
//! 3. **Commit** — [`Draft::commit`] writes through the caller's
//!    persistence sink first; only if that succeeds do the entries
//!    become part of the session and a [`Release`] is issued.
//!
//! Publication requires a [`Release`], which can only be obtained by
//! committing. That is a compile-time guarantee rather than a
//! convention: there is no way to ask a `Session` to publish
//! something that was not committed.
//!
//! Dropping a [`Draft`] without committing discards it and releases
//! the line, so a panicking or early-returning caller cannot leave
//! half-applied state behind.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

pub mod sqlite_sink;

pub use sqlite_sink::SqliteSink;

/// One entry in the session's append-only log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    /// Monotonic sequence number assigned at commit time. `0` before
    /// the entry is committed.
    pub seq: u64,
    pub kind: String,
    pub payload: serde_json::Value,
    /// P2 EntryRecord 对齐（设计 §3.3 `by_task_id`）：发起本条 entry 的
    /// 任务 id。重连/对账时靠它把 entry 归属回崩溃前未完成的 task。
    /// `None` = 无任务归属（用户消息 / 系统 entry）。
    #[serde(default)]
    pub by_task_id: Option<String>,
    /// P2 EntryRecord 对齐（设计 §3.3 `conversation_id`）：所属会话。
    /// SqliteSink 另用表外 `session_id` 列做物理隔离；此字段是 entry
    /// 自身的逻辑归属（多会话合并日志 / 跨会话重放时区分用）。
    #[serde(default)]
    pub conversation_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineState {
    Free,
    Held,
}

struct Inner {
    entries: Vec<Entry>,
    line: LineState,
    next_seq: u64,
    /// I8: set to true when a commit fails at the persistence layer.
    /// Once poisoned, `begin` refuses new drafts; the session must be
    /// reconstructed from a known-good snapshot.
    poisoned: bool,
}

/// A session, identified by its append-only log.
///
/// Cheap to clone: all clones share the same log and the same change
/// line.
#[derive(Clone)]
pub struct Session {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// Another holder is drafting. The caller decides whether to wait
    /// or give up, so this is reported instead of blocking.
    #[error("session change line is held by another writer")]
    LineHeld,
    #[error("persistence failed: {0}")]
    Persist(String),
    /// I8: An uncertain storage failure has poisoned the session.
    /// The sink's state is no longer trusted (it may have partially
    /// written), so further commits are refused. The caller must
    /// reconstruct the session from a known-good snapshot
    /// (e.g. `Session::from_entries` on a reloaded log) and start
    /// over; retrying on this instance is undefined.
    #[error("session poisoned after an uncertain storage failure")]
    Poisoned,
}

impl Session {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                entries: Vec::new(),
                line: LineState::Free,
                next_seq: 1,
                poisoned: false,
            })),
        }
    }

    /// Rebuild a session from a persisted log. Used on restart and
    /// reconnect, which is what makes a session resumable.
    pub fn from_entries(entries: Vec<Entry>) -> Self {
        let next_seq = entries.last().map(|e| e.seq + 1).unwrap_or(1);
        Self {
            inner: Arc::new(Mutex::new(Inner {
                entries,
                line: LineState::Free,
                next_seq,
                poisoned: false,
            })),
        }
    }

    /// Take the change line and start drafting.
    pub fn begin(&self) -> Result<Draft, SessionError> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.poisoned {
            // I8: an uncertain storage failure is fatal. Refuse new
            // drafts; the caller must reconstruct from a known-good
            // snapshot instead of retrying.
            return Err(SessionError::Poisoned);
        }
        if inner.line == LineState::Held {
            return Err(SessionError::LineHeld);
        }
        inner.line = LineState::Held;
        Ok(Draft {
            session: self.clone(),
            staged: Vec::new(),
        })
    }

    /// Everything committed so far, in order.
    pub fn committed(&self) -> Vec<Entry> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .clone()
    }

    /// Publish committed entries.
    ///
    /// Requires a [`Release`], which only [`Draft::commit`] can
    /// produce — this signature is what prevents publishing state
    /// that was never committed.
    pub fn publish(&self, release: &Release) -> Vec<Entry> {
        release.entries.clone()
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

/// Exclusive holder of the change line.
///
/// Entries staged here are invisible until [`Draft::commit`]
/// succeeds. Dropping the draft releases the line without applying
/// anything.
pub struct Draft {
    session: Session,
    staged: Vec<Entry>,
}

impl Draft {
    /// Stage an entry. Not visible to readers until commit.
    /// 无任务归属（用户消息 / 系统 entry）。
    pub fn append(&mut self, kind: impl Into<String>, payload: serde_json::Value) {
        self.staged.push(Entry {
            seq: 0,
            kind: kind.into(),
            payload,
            by_task_id: None,
            conversation_id: None,
        });
    }

    /// Stage an entry with a task attribution. 工具 / LLM 产出的 entry
    /// 用它把 entry 归属到发起它的 task（重连/对账时按 `by_task_id`
    /// 把 entry 拼回未完成的 task）。
    pub fn append_with_task(
        &mut self,
        kind: impl Into<String>,
        payload: serde_json::Value,
        by_task_id: Option<String>,
    ) {
        self.staged.push(Entry {
            seq: 0,
            kind: kind.into(),
            payload,
            by_task_id,
            conversation_id: None,
        });
    }

    /// 给所有已 staged 的 entry 统一打会话归属标记（多会话合并日志用）。
    /// 须在 commit 前调用；commit 后 entry 已不可变（I5）。
    pub fn set_conversation(&mut self, conversation_id: impl Into<String>) {
        let cid = conversation_id.into();
        for e in &mut self.staged {
            e.conversation_id = Some(cid.clone());
        }
    }

    /// Entries staged so far, before they are committed.
    pub fn staged(&self) -> &[Entry] {
        &self.staged
    }

    /// Commit through `persist`, which stands in for the durable
    /// sink (SQLite today, whatever `hermix-store` fronts later).
    ///
    /// Order matters: the durable write happens **first**, and the
    /// in-memory log is only extended once it has succeeded. A failing
    /// sink therefore leaves the session exactly as it was and yields
    /// no [`Release`], so there is nothing to publish.
    pub fn commit(
        mut self,
        persist: impl FnOnce(&[Entry]) -> Result<(), String>,
    ) -> Result<Release, SessionError> {
        let mut inner = self
            .session
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        // Sequence numbers are assigned *before* the durable write so
        // the sink receives complete entries (a sink that persists
        // them needs a real seq, not the placeholder 0). `next_seq` is
        // only advanced once the write has succeeded, so a rejected
        // commit leaves the session — including its sequence counter —
        // exactly as it was.
        let start = inner.next_seq;
        for (i, entry) in self.staged.iter_mut().enumerate() {
            entry.seq = start + i as u64;
        }

        if let Err(e) = persist(&self.staged) {
            // I8: a failed durable write is an *uncertain* storage
            // failure — the sink may have partially persisted. The
            // session is now poisoned: refuse further commits. The
            // line is still released so an in-flight `Drop` is clean,
            // but `begin` will return `Poisoned` from here on.
            tracing::warn!(error = %e, "session poisoned by persistence failure");
            inner.poisoned = true;
            inner.line = LineState::Free;
            return Err(SessionError::Poisoned);
        }

        let mut committed = Vec::with_capacity(self.staged.len());
        for entry in self.staged.drain(..) {
            inner.next_seq = entry.seq + 1;
            committed.push(entry.clone());
            inner.entries.push(entry);
        }
        inner.line = LineState::Free;

        Ok(Release {
            seq: inner.next_seq - 1,
            entries: committed,
        })
    }
}

impl Drop for Draft {
    fn drop(&mut self) {
        // Normal completion already cleared the flag in `commit`;
        // this covers every other exit path, including panic.
        if let Ok(mut inner) = self.session.inner.lock() {
            if !self.staged.is_empty() {
                tracing::debug!(
                    discarded = self.staged.len(),
                    "session: draft dropped without commit; staged entries discarded"
                );
            }
            inner.line = LineState::Free;
        }
    }
}

/// Proof that a commit succeeded.
///
/// The only way to obtain one is [`Draft::commit`], and the only way
/// to publish is to hand one to [`Session::publish`]. Uncommitted
/// state is therefore unpublishable by construction.
#[derive(Debug, Clone)]
pub struct Release {
    /// Sequence number of the last committed entry.
    pub seq: u64,
    pub entries: Vec<Entry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(_: &[Entry]) -> Result<(), String> {
        Ok(())
    }

    fn fail(_: &[Entry]) -> Result<(), String> {
        Err("disk on fire".to_string())
    }

    #[test]
    fn staged_entries_are_invisible_until_commit() {
        let s = Session::new();
        let mut d = s.begin().unwrap();
        d.append("assistant_text", serde_json::json!({"text": "hi"}));
        assert!(s.committed().is_empty(), "staged state must not be observable");
        drop(d);
        assert!(s.committed().is_empty(), "dropping must not apply the draft");
    }

    #[test]
    fn commit_makes_entries_observable_and_assigns_seq() {
        let s = Session::new();
        let mut d = s.begin().unwrap();
        d.append("assistant_text", serde_json::json!({"text": "hi"}));
        d.append("tool_call", serde_json::json!({"tool": "read_file"}));
        let rel = d.commit(ok).unwrap();

        assert_eq!(rel.entries.len(), 2);
        assert_eq!(rel.entries[0].seq, 1);
        assert_eq!(rel.entries[1].seq, 2);
        assert_eq!(s.committed().len(), 2);
    }

    #[test]
    fn failed_commit_changes_nothing_and_yields_no_release() {
        let s = Session::new();
        // A first, committed entry so we can prove it survives.
        let mut d0 = s.begin().unwrap();
        d0.append("user_text", serde_json::json!({"text": "hello"}));
        d0.commit(ok).unwrap();

        let mut d = s.begin().unwrap();
        d.append("assistant_text", serde_json::json!({"text": "doomed"}));
        let result = d.commit(fail);

        assert!(
            matches!(result, Err(SessionError::Poisoned)),
            "a rejected commit must not yield a Release, and must poison the session"
        );
        assert_eq!(
            s.committed().len(),
            1,
            "state must be byte-identical to before the failed commit"
        );
        // I8: after an uncertain storage failure the session is fatal.
        assert!(
            matches!(s.begin(), Err(SessionError::Poisoned)),
            "a poisoned session must refuse further drafts"
        );
    }

    #[test]
    fn poison_persists_across_clones() {
        // All clones share the same log + change line, so a poison
        // taken on one clone is visible on every other clone.
        let s = Session::new();
        let s2 = s.clone();
        let mut d = s.begin().unwrap();
        d.append("x", serde_json::json!({}));
        assert!(d.commit(fail).is_err());
        assert!(matches!(s2.begin(), Err(SessionError::Poisoned)));
    }

    #[test]
    fn successful_commits_leave_the_session_unpoisoned() {
        // The poison flag must be set ONLY on failure; a session that
        // commits repeatedly stays usable.
        let s = Session::new();
        for _ in 0..5 {
            let mut d = s.begin().unwrap();
            d.append("k", serde_json::json!({}));
            d.commit(ok).unwrap();
        }
        assert_eq!(s.committed().len(), 5);
        assert!(s.begin().is_ok(), "repeated success must not poison");
    }

    #[test]
    fn only_one_holder_can_draft_at_a_time() {
        let s = Session::new();
        let _held = s.begin().unwrap();
        assert!(
            matches!(s.begin(), Err(SessionError::LineHeld)),
            "a second writer must not be able to take the change line"
        );
    }

    #[test]
    fn line_is_released_after_commit_and_after_drop() {
        let s = Session::new();
        let mut d = s.begin().unwrap();
        d.append("k", serde_json::json!({}));
        d.commit(ok).unwrap();
        assert!(s.begin().is_ok(), "commit must release the line");

        let d2 = s.begin().unwrap();
        drop(d2);
        assert!(s.begin().is_ok(), "drop must release the line");
    }

    #[test]
    fn rebuild_from_log_continues_the_sequence() {
        let s = Session::new();
        let mut d = s.begin().unwrap();
        d.append("a", serde_json::json!({}));
        d.append("b", serde_json::json!({}));
        d.commit(ok).unwrap();

        // Simulates a restart: same log, new process.
        let restored = Session::from_entries(s.committed());
        let mut d = restored.begin().unwrap();
        d.append("c", serde_json::json!({}));
        let rel = d.commit(ok).unwrap();
        assert_eq!(rel.entries[0].seq, 3, "seq must continue, not restart at 1");
    }

    #[test]
    fn publish_requires_a_release() {
        let s = Session::new();
        let mut d = s.begin().unwrap();
        d.append("assistant_text", serde_json::json!({"text": "hi"}));
        let rel = d.commit(ok).unwrap();
        // The only way to reach `publish` is with a Release obtained
        // from a successful commit — there is no API that publishes
        // staged state.
        assert_eq!(s.publish(&rel).len(), 1);
    }

    // ------------------------------------------------------------------
    // I1: 一次 Session commit 在所有记录写入上是原子的（变更线 + 单事务）。
    // 1000 个并发 begin/commit 批次不得交错：每批 N 条 entry 要么整批
    // 进入日志（连续 N 个 seq），要么整批缺席。
    // 设计 §3.3 I1 明写"并发测试：1000 并发写无交错"。
    // ------------------------------------------------------------------

    #[test]
    fn i1_concurrent_commits_do_not_interleave() {
        use std::sync::Arc;
        use std::thread;

        const BATCHES: u32 = 1000;
        const PER_BATCH: u32 = 2;

        let s = Arc::new(Session::new());
        let mut handles = Vec::with_capacity(BATCHES as usize);
        for _b in 0..BATCHES {
            let s = Arc::clone(&s);
            handles.push(thread::spawn(move || {
                // `begin` is deliberately non-blocking: it reports
                // `LineHeld` instead of waiting. In a 1000-thread
                // stampede the caller must retry (with a short sleep)
                // until the line frees.
                let mut d = loop {
                    match s.begin() {
                        Ok(draft) => break draft,
                        Err(SessionError::LineHeld) => {
                            thread::sleep(std::time::Duration::from_millis(1));
                        }
                        Err(e) => panic!("unexpected session error: {e}"),
                    }
                };
                for _ in 0..PER_BATCH {
                    d.append("i1", serde_json::json!({}));
                }
                d.commit(ok).expect("commit 必须成功")
            }));
        }
        for h in handles {
            h.join().expect("thread panic");
        }

        let log = s.committed();
        assert_eq!(
            log.len(),
            (BATCHES * PER_BATCH) as usize,
            "1000 并发批 × 2 条 → 2000 条全在"
        );
        // seq 严格连续 1..=2000（commit 原子，无跳号）。
        for (i, e) in log.iter().enumerate() {
            assert_eq!(e.seq, i as u64 + 1, "seq 必须连续无跳号");
        }
    }
}
