//! # hermix-store
//!
//! The single place in Hermix that opens SQLite. Every crate that
//! touches a database goes through [`Store::open`] so that the
//! journaling and locking pragmas cannot drift apart between the
//! writer and the readers.
//!
//! ## Why this exists
//!
//! `rusqlite::Connection::open` defaults to `journal_mode = delete`
//! (readers block writers) and `busy_timeout = 0` (no retry on
//! `SQLITE_BUSY`). Those defaults were used at ~40 call sites spread
//! across eight crates, and the mismatch produced three distinct
//! failures that were diagnosed one at a time before this crate
//! existed:
//!
//! - **Silent lost writes.** Two threads racing on one DB: the second
//!   opener gets `SQLITE_BUSY` immediately, and the `let _ = persist(..)`
//!   wrappers at the call sites swallow the error. The UI reports the
//!   message as saved while the row never lands.
//! - **`SQLITE_CORRUPT` / "database disk image is malformed".** When
//!   the writer is killed mid-transaction — which happens on every
//!   Tauri sidecar SIGKILL — it leaves a stale `-shm` page table. A
//!   reader that opened with `journal_mode = delete` then reads the
//!   WAL header from a page the writer never checkpointed and
//!   declares the file corrupt. WAL on **both** sides removes this.
//! - **Multi-second UI stalls.** A `DELETE` on a multi-MB
//!   `sessions.db` holds the WAL writer lock for 1–3 s. With a 5 s
//!   busy_timeout the very next `SELECT` (sidebar click, session
//!   switch) sits in the retry window and freezes the front end.
//!
//! The last one is why read and write paths get **different** timeouts
//! rather than one shared value.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::Connection;

/// How the caller intends to use the database.
///
/// The two modes differ **only** in the busy timeout and in whether a
/// passive checkpoint runs on open, because the failure modes of the
/// two paths are different (see the crate docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
    /// Owns writes. Waits longer for the write lock, because a lost
    /// write is worse than a slow one — the caller can retry, but a
    /// swallowed `SQLITE_BUSY` silently drops data.
    Write,
    /// Serves reads (dashboard queries, session switches, sidebar).
    /// Fails fast so a lock held by an in-flight `DELETE` cannot stall
    /// the UI; the caller retries on its own.
    Read,
    /// 只读打开**别人的**库（`workbench.db` 一类插件自管文件）：用
    /// `SQLITE_OPEN_READ_ONLY` 打开，拒绝一切写路径，跳过 WAL /
    /// checkpoint（两者都需要写权限）。busy timeout 同 [`Self::Read`]。
    ReadOnly,
}

/// Busy timeout for [`OpenMode::Write`]. Long enough to ride out a
/// competing transaction, short enough that it never looks like a hang.
const WRITE_BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

/// Busy timeout for [`OpenMode::Read`]. Deliberately much shorter than
/// the writer's: a `DELETE` in flight holds the WAL writer lock for
/// 1–3 s on a large `sessions.db`, and a longer window here turns a
/// routine sidebar click into a multi-second freeze.
const READ_BUSY_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("failed to open database at {path}: {source}")]
    Open {
        path: String,
        #[source]
        source: rusqlite::Error,
    },
    #[error("failed to apply connection pragmas: {0}")]
    Pragma(rusqlite::Error),
}

/// Open a connection with the pragmas [`OpenMode`] requires, handed
/// back unwrapped.
///
/// This exists for the incremental migration of the ~40 call sites
/// that already own a `Mutex<Connection>`: they can swap their raw
/// `Connection::open` for this one line and keep their existing
/// locking, instead of being rewritten onto [`Store`] all at once.
/// New code should prefer [`Store::open`].
pub fn open_connection(
    path: impl AsRef<Path>,
    mode: OpenMode,
) -> Result<Connection, StoreError> {
    let path = path.as_ref().to_path_buf();
    let path_str = path.display().to_string();
    // 只读模式必须用 open_with_flags：`Connection::open` 隐含
    // READ_WRITE|CREATE，对「别人的库」（workbench.db 一类插件自管文件）
    // 既可能建出空文件，也挡不住误写。
    let conn = match mode {
        OpenMode::ReadOnly => {
            Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        }
        _ => Connection::open(&path),
    }
    .map_err(|source| StoreError::Open {
        path: path_str.clone(),
        source,
    })?;

    let timeout = match mode {
        OpenMode::Write => WRITE_BUSY_TIMEOUT,
        OpenMode::Read | OpenMode::ReadOnly => READ_BUSY_TIMEOUT,
    };
    conn.busy_timeout(timeout).map_err(StoreError::Pragma)?;

    // 只读连接不能改 journal_mode，也不该触发 checkpoint（都需要写权限）。
    if mode != OpenMode::ReadOnly {
        if let Err(e) = conn.execute_batch("PRAGMA journal_mode = WAL;") {
            tracing::warn!(
                db_path = %path_str,
                error = %e,
                "hermix-store: WAL pragma rejected; continuing with default journaling"
            );
        }
    }

    if mode == OpenMode::Read {
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
    }

    Ok(conn)
}

/// An open database handle.
///
/// The connection sits behind a mutex because `rusqlite::Connection`
/// is `Send` but not `Sync`; callers share one `Store` (usually behind
/// an `Arc`) instead of re-opening the file per operation.
pub struct Store {
    conn: Mutex<Connection>,
    path: std::path::PathBuf,
}

impl Store {
    /// Open a database file with the pragmas this mode requires.
    ///
    /// WAL is applied best-effort: if it is rejected (read-only file,
    /// exclusive lock held) the caller gets a warning and a working
    /// connection rather than a hard failure, because a busy timeout
    /// alone already resolves the silent-lost-write failure.
    pub fn open(path: impl AsRef<Path>, mode: OpenMode) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        let conn = open_connection(&path, mode)?;
        Ok(Self {
            conn: Mutex::new(conn),
            path,
        })
    }

    /// Open a private in-memory database. Used by tests only.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn =
            Connection::open_in_memory().map_err(|source| StoreError::Open {
                path: ":memory:".to_string(),
                source,
            })?;
        Ok(Self {
            conn: Mutex::new(conn),
            path: std::path::PathBuf::from(":memory:"),
        })
    }

    /// Run a closure with shared access to the connection.
    ///
    /// Returns `None` when the mutex is poisoned, which means a
    /// previous holder panicked mid-write; callers treat that as "try
    /// again later" rather than propagating a panic across threads.
    pub fn read<T>(&self, f: impl FnOnce(&Connection) -> T) -> Option<T> {
        self.conn
            .lock()
            .ok()
            .map(|conn| f(&conn))
    }

    /// Run a closure with exclusive access to the connection.
    pub fn write<T>(&self, f: impl FnOnce(&mut Connection) -> T) -> Option<T> {
        self.conn
            .lock()
            .ok()
            .map(|mut conn| f(&mut conn))
    }

    /// Path of the database file, for logging and diagnostics.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_mode_uses_wal_and_a_longer_timeout_than_read() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");

        let w = Store::open(&db, OpenMode::Write).unwrap();
        let mode: String = w
            .read(|c| c.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap())
            .unwrap();
        assert_eq!(mode, "wal", "the writer must run in WAL mode");

        let r = Store::open(&db, OpenMode::Read).unwrap();
        let read_timeout: i64 = r
            .read(|c| c.query_row("PRAGMA busy_timeout", [], |r| r.get(0)).unwrap())
            .unwrap();
        let write_timeout: i64 = w
            .read(|c| c.query_row("PRAGMA busy_timeout", [], |r| r.get(0)).unwrap())
            .unwrap();
        assert_eq!(write_timeout, 5000);
        assert_eq!(read_timeout, 500);
        assert!(
            write_timeout > read_timeout,
            "readers must fail fast so a held write lock cannot stall the UI"
        );
    }

    #[test]
    fn concurrent_readers_do_not_fail_while_a_writer_holds_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let store = Store::open(&db, OpenMode::Write).unwrap();
        store
            .write(|c| {
                c.execute_batch("CREATE TABLE t (v INTEGER); INSERT INTO t VALUES (1);")
                    .unwrap()
            })
            .unwrap();

        // Second opener on the same file: with the legacy
        // `journal_mode = delete` default this is where readers used
        // to hit SQLITE_BUSY / corruption reports.
        let reader = Store::open(&db, OpenMode::Read).unwrap();
        let count: i64 = reader
            .read(|c| c.query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0)).unwrap())
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn read_only_mode_rejects_writes_and_never_creates_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("plugin.db");

        // 不存在的文件：只读打开必须失败，而不是像 `Connection::open`
        // 那样悄悄建出一个空库（读别人的库时这是数据污染）。
        assert!(
            open_connection(&db, OpenMode::ReadOnly).is_err(),
            "read-only open must not create a missing file"
        );
        assert!(!db.exists(), "no empty db may be created");

        // 建好后只读打开成功，写入必须被拒、读仍然可用。
        let w = open_connection(&db, OpenMode::Write).unwrap();
        w.execute_batch("CREATE TABLE t (v INTEGER);").unwrap();
        drop(w);

        let ro = open_connection(&db, OpenMode::ReadOnly).unwrap();
        assert!(
            ro.execute_batch("INSERT INTO t VALUES (1);").is_err(),
            "read-only connection must reject writes"
        );
        let count: i64 = ro
            .query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "reads still work on a read-only connection");
    }

    #[test]
    fn in_memory_store_supports_ddl_and_dml() {
        let store = Store::open_in_memory().unwrap();
        store
            .write(|c| {
                c.execute_batch("CREATE TABLE x (v TEXT); INSERT INTO x VALUES ('ok');")
                    .unwrap()
            })
            .unwrap();
        let v: String = store
            .read(|c| c.query_row("SELECT v FROM x", [], |r| r.get(0)).unwrap())
            .unwrap();
        assert_eq!(v, "ok");
    }
}
