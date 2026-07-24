//! Cross-process advisory file lock for the cron scheduler.
//!
//! ## Why we need this
//!
//! The scheduler loop in `scheduler::CronScheduler::start` polls
//! every `cron_poll_interval()` seconds, opens the SQLite
//! persistence, and reads/writes `cron_jobs` rows. If two
//! `hermix-cron` processes are running concurrently — e.g. the
//! desktop app's embedded scheduler plus a CLI invocation
//! `hermix cron list` — both will call
//! `SqliteJobPersistence::save_job` at the same time and the
//! underlying `rusqlite` connection will return
//! `SQLITE_BUSY` (or worse, silently corrupt the WAL).
//!
//! `fs4::FileExt::lock_exclusive` gives us an OS-level advisory
//! lock that maps to:
//!
//!   - Windows: `LockFileEx(LOCKFILE_EXCLUSIVE_LOCK)` (via
//!     `fs4`'s `windows` backend)
//!   - Linux/macOS: `flock(LOCK_EX)` (via `fs4`'s `unix` backend)
//!
//! The lock is **advisory** — both sides have to take it for
//! the protection to be effective. The cron module takes the
//! lock around every SQLite write so the desktop app, the CLI
//! `cron add/remove/list/run` commands, and the dashboard REST
//! endpoints can all coexist.
//!
//! ## Usage
//!
//! ```ignore
//! use hermix_cron::file_lock::CronFileLock;
//!
//! let lock = CronFileLock::new("/path/to/cron/.tick.lock");
//! let _guard = lock.acquire(Duration::from_secs(3))?;
//! // ... critical section ...
//! // _guard drops the lock when it goes out of scope
//! ```
//!
//! ## Failure modes
//!
//! - **Lock held by another process**: `acquire` retries with
//!   a 100ms backoff up to the caller's `timeout`.
//! - **Stale lock from a crashed process**: the OS releases
//!   the lock on process exit, so the next `acquire` will
//!   succeed immediately. We do NOT add a "lock is older than
//!   X hours, break it" heuristic — that's a footgun for
//!   processes that genuinely run for hours.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fs4::fs_std::FileExt;

/// Default retry interval while waiting for the lock.
const RETRY_INTERVAL: Duration = Duration::from_millis(100);

/// Cross-process advisory file lock for the cron scheduler.
/// /// Construct with [`CronFileLock::new`] or
/// [`CronFileLock::at`]. Call [`Self::acquire`] to get a
/// [`LockGuard`] that releases the lock on drop.
#[derive(Clone)]
pub struct CronFileLock {
    path: PathBuf,
}

impl CronFileLock {
    /// Create a lock tied to the given path. The path is opened
    /// (or created) lazily on the first `acquire()` call so
    /// constructing the struct never touches the filesystem.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Convenience: lock at `<cron_dir>/.tick.lock`. `cron_dir`
    /// is the directory containing the SQLite `cron_jobs.db`.
    pub fn at(cron_dir: impl AsRef<Path>) -> Self {
        Self::new(cron_dir.as_ref().join(".tick.lock"))
    }

    /// Acquire the lock, blocking for up to `timeout`. Returns
    /// a `LockGuard` that releases on drop.
    /// /// Returns an error if:
    /// - the lock file can't be opened/created (I/O error)
    /// - the timeout elapses while waiting for the lock
    pub fn acquire(&self, timeout: Duration) -> Result<LockGuard, LockError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| LockError::Io {
                path: self.path.clone(),
                source: e,
            })?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.path)
            .map_err(|e| LockError::Io {
                path: self.path.clone(),
                source: e,
            })?;

        // For zero timeout, use non-blocking try_lock to avoid
        // hanging on platforms where lock_exclusive blocks.
        if timeout.is_zero() {
            match file.try_lock_exclusive() {
                Ok(true) => {
                    return Ok(LockGuard {
                        file: Some(file),
                        path: self.path.clone(),
                    });
                }
                Ok(false) | Err(_) => {
                    return Err(LockError::Timeout {
                        path: self.path.clone(),
                        waited: Duration::ZERO,
                        source: std::io::Error::new(
                            std::io::ErrorKind::WouldBlock,
                            "lock is held by another process",
                        ),
                    });
                }
            }
        }

        let start = Instant::now();
        loop {
            match file.try_lock_exclusive() {
                Ok(true) => {
                    return Ok(LockGuard {
                        file: Some(file),
                        path: self.path.clone(),
                    });
                }
                Ok(false) if start.elapsed() >= timeout => {
                    return Err(LockError::Timeout {
                        path: self.path.clone(),
                        waited: start.elapsed(),
                        source: std::io::Error::new(
                            std::io::ErrorKind::WouldBlock,
                            "lock is held by another process",
                        ),
                    });
                }
                Err(e) if start.elapsed() >= timeout => {
                    return Err(LockError::Timeout {
                        path: self.path.clone(),
                        waited: start.elapsed(),
                        source: e,
                    });
                }
                _ => {
                    // Another process holds the lock.
                    // Back off and retry.
                    std::thread::sleep(RETRY_INTERVAL);
                }
            }
        }
    }

    /// Try to acquire the lock without blocking. Returns
    /// `None` if the lock is currently held by another
    /// process.
    pub fn try_acquire(&self) -> Result<Option<LockGuard>, LockError> {
        match self.acquire(Duration::from_millis(0)) {
            Ok(g) => Ok(Some(g)),
            Err(LockError::Timeout { .. }) => Ok(None),
            Err(other) => Err(other),
        }
    }

    /// Return the lock file path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// RAII guard returned by [`CronFileLock::acquire`]. Releases
/// the lock on drop.
pub struct LockGuard {
    file: Option<File>,
    path: PathBuf,
}

impl LockGuard {
    /// Explicitly release the lock before the guard goes out
    /// of scope. Idempotent. After calling this, `is_held`
    /// returns false.
    pub fn release(mut self) {
        if let Some(file) = self.file.take() {
            let _ = file.unlock();
        }
    }

    /// Whether the guard is still holding the lock.
    pub fn is_held(&self) -> bool {
        self.file.is_some()
    }

    /// Path of the underlying lock file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            // Errors during unlock are not actionable
            // (the file may have been closed by the OS on
            // shutdown). We log and move on.
            if let Err(e) = file.unlock() {
                tracing::debug!(
                    path = %self.path.display(),
                    error = %e,
                    "CronFileLock: unlock on drop failed (likely benign)"
                );
            }
        }
    }
}

/// Errors that can occur while taking or holding a
/// [`CronFileLock`].
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("cron file lock: failed to open lock file at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "cron file lock: timed out waiting for exclusive lock at {path} after {:.2}s",
        waited.as_secs_f64()
    )]
    Timeout {
        path: PathBuf,
        waited: Duration,
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn acquire_release_round_trip() {
        let tmp = TempDir::new().unwrap();
        let lock = CronFileLock::at(tmp.path());
        let guard = lock.acquire(Duration::from_millis(500)).unwrap();
        assert!(guard.is_held());
        assert_eq!(guard.path().parent().unwrap(), tmp.path());
        guard.release();
    }

    #[test]
    fn drop_releases_lock_for_next_acquire() {
        let tmp = TempDir::new().unwrap();
        let lock = CronFileLock::at(tmp.path());
        {
            let _guard = lock.acquire(Duration::from_millis(500)).unwrap();
        } // guard drops here
        // Second acquire must succeed immediately because the
        // OS released the lock when the first guard dropped.
        let _second = lock.acquire(Duration::from_millis(500)).unwrap();
    }

    #[test]
    fn try_acquire_returns_none_when_held() {
        let tmp = TempDir::new().unwrap();
        let lock = CronFileLock::at(tmp.path());
        let _held = lock.acquire(Duration::from_millis(500)).unwrap();
        let result = lock.try_acquire().unwrap();
        assert!(result.is_none(), "try_acquire should return None when lock is held");
    }

    #[test]
    fn try_acquire_returns_some_when_free() {
        let tmp = TempDir::new().unwrap();
        let lock = CronFileLock::at(tmp.path());
        let result = lock.try_acquire().unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn concurrent_processes_serialise() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let tmp = TempDir::new().unwrap();
        let lock_path = tmp.path().join(".tick.lock");
        let lock_a = Arc::new(CronFileLock::new(&lock_path));
        let lock_b = Arc::new(CronFileLock::new(&lock_path));
        let barrier = Arc::new(Barrier::new(2));

        let barrier_a = barrier.clone();
        let handle_a = thread::spawn(move || {
            let g = lock_a.acquire(Duration::from_secs(2)).unwrap();
            barrier_a.wait();
            // Hold the lock until the barrier signals
            // the other thread has tried to acquire.
            std::thread::sleep(Duration::from_millis(200));
            g.release();
        });

        // Give thread A a head-start so it definitely
        // gets the lock first.
        std::thread::sleep(Duration::from_millis(50));
        let barrier_b = barrier.clone();
        let start = Instant::now();
        let handle_b = thread::spawn(move || {
            barrier_b.wait();
            let g = lock_b.acquire(Duration::from_secs(2)).unwrap();
            let waited = start.elapsed();
            g.release();
            waited
        });

        handle_a.join().unwrap();
        let waited_b = handle_b.join().unwrap();
        // B must have waited at least 100ms (the time A held
        // the lock after the barrier). We allow some slack
        // for OS scheduling.
        assert!(
            waited_b >= Duration::from_millis(100),
            "B acquired too quickly ({:?}) — lock not serialising?",
            waited_b
        );
    }

    #[test]
    fn lock_creates_parent_directory() {
        let tmp = TempDir::new().unwrap();
        let nested = tmp.path().join("a").join("b").join("c");
        let lock = CronFileLock::at(&nested);
        let _g = lock.acquire(Duration::from_millis(500)).unwrap();
        assert!(nested.exists());
    }

    #[test]
    fn lock_file_path_matches_constructor() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("custom.lock");
        let lock = CronFileLock::new(&p);
        assert_eq!(lock.path(), p);
    }
}
