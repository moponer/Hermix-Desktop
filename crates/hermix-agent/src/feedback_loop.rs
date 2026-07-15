//! P4-T5 — per-turn harness feedback loop.
//!
//! Every agent turn leaves a one-line trace of:
//! - what action was attempted,
//! - whether it succeeded,
//! - what the agent learnt for next time.
//!
//! The log is the **harness-level feedback layer**: independent of
//! the in-prompt reflection (which is the *agent-level* feedback).
//! Downstream consumers (P4-T6 self-improvement, P7 reflection
//! generators) read from this file.
//!
//! ## Storage
//!
//! ```text
//! ~/.hermix/feedback/<session_id>.log.jsonl
//! ```
//!
//! Append-only JSONL: one [`FeedbackEntry`] per line. Cheap to read,
//! cheap to write, and survives across crashes because every entry
//! is durably flushed on insert.
//!
//! ## Soft-fail
//!
//! Every entry returns [`Result`]. The agent loop MUST swallow the
//! error and log it \u2014 a faulty feedback disk must never crash the
//! run.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

/// One observation from a single turn. Fields are deliberately
/// short so the JSONL stays compact (and so the LLM doesn't learn
/// to pad the rationale column).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedbackEntry {
    pub turn: u32,
    pub action: String,
    pub success: bool,
    pub learnings: String,
}

/// Resolve the feedback directory (and create it on demand).
pub fn feedback_root() -> PathBuf {
    let home = hermix_config::paths::hermix_home();
    let dir = home.join("feedback");
    let _ = fs::create_dir_all(&dir); // soft-fail; create_dir_all
    dir
}

/// Build the file path for a session's log without touching the FS.
pub fn feedback_path(session_id: &str) -> PathBuf {
    feedback_root().join(format!("{}.log.jsonl", sanitize_session_id(session_id)))
}

/// Strip any character that isn't safe in a filename (Windows is
/// strict about `<>:"/\\|?*`).
fn sanitize_session_id(session_id: &str) -> String {
    let mut out = String::with_capacity(session_id.len());
    for ch in session_id.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else if ch == ' ' {
            out.push('_');
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push_str("default");
    }
    if out.len() > 64 {
        out.truncate(64);
    }
    out
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

/// Append-only feedback log. Cheap to clone (Arc in), expensive to
/// share across threads (Mutex in). The interface is intentionally
/// minimal: append, length, snapshot.
pub struct FeedbackLog {
    path: PathBuf,
    inner: Mutex<()>,
}

impl FeedbackLog {
    /// Open (and lazily create) a session's log. The parent
    /// directory is created on demand; subsequent writes append.
    pub fn open(session_id: &str) -> std::io::Result<Self> {
        let path = feedback_path(session_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(Self {
            path,
            inner: Mutex::new(()),
        })
    }

    /// Open against an explicit path (used by tests).
    pub fn open_at(path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(Self {
            path: path.to_path_buf(),
            inner: Mutex::new(()),
        })
    }

    /// Append one entry. The write is flushed before returning so a
    /// crash mid-session does not lose the entry.
    pub fn record(&self, entry: &FeedbackEntry) -> std::io::Result<()> {
        let _guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let line = serde_json::to_string(entry)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        writeln!(file, "{}", line)?;
        file.flush()?;
        Ok(())
    }

    /// Read all entries from the underlying file. Returns an empty
    /// vec if the file doesn't exist yet.
    pub fn snapshot(&self) -> std::io::Result<Vec<FeedbackEntry>> {
        let _guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = fs::File::open(&self.path)?;
        let reader = BufReader::new(file);
        let mut out = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<FeedbackEntry>(&line) {
                Ok(e) => out.push(e),
                Err(_e) => {
                    // Skip lines we cannot parse \u2014 a partially
                    // written line is recoverable; we never want to
                    // blow the loop on a corrupt trace.
                }
            }
        }
        Ok(out)
    }

    /// Most-recent `n` entries (oldest within the slice first).
    pub fn tail(&self, n: usize) -> std::io::Result<Vec<FeedbackEntry>> {
        let all = self.snapshot()?;
        Ok(all.into_iter().rev().take(n).collect::<Vec<_>>().into_iter().rev().collect())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> usize {
        self.snapshot().map(|v| v.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_entry(turn: u32, success: bool) -> FeedbackEntry {
        FeedbackEntry {
            turn,
            action: format!("tool:shell:{turn}"),
            success,
            learnings: format!("turn {turn} observation"),
        }
    }

    #[test]
    fn sanitize_strips_path_separators() {
        assert_eq!(sanitize_session_id("a/b\\c"), "a_b_c");
        assert_eq!(sanitize_session_id("../etc/passwd"), ".._etc_passwd");
        assert_eq!(sanitize_session_id("safe-id_1.0"), "safe-id_1.0");
        assert_eq!(sanitize_session_id(""), "default");
    }

    #[test]
    fn sanitize_truncates_long_ids() {
        let long = "a".repeat(200);
        assert_eq!(sanitize_session_id(&long).len(), 64);
    }

    #[test]
    fn record_round_trips_via_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let log = FeedbackLog::open_at(&dir.path().join("s.log.jsonl")).unwrap();
        log.record(&fake_entry(1, true)).unwrap();
        log.record(&fake_entry(2, false)).unwrap();
        let entries = log.snapshot().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].turn, 1);
        assert!(entries[1].success == false);
    }

    #[test]
    fn tail_returns_most_recent_n() {
        let dir = tempfile::tempdir().unwrap();
        let log = FeedbackLog::open_at(&dir.path().join("s.log.jsonl")).unwrap();
        for i in 0..5 {
            log.record(&fake_entry(i, true)).unwrap();
        }
        let tail = log.tail(2).unwrap();
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].turn, 3);
        assert_eq!(tail[1].turn, 4);
    }

    #[test]
    fn record_appends_not_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.log.jsonl");
        let log = FeedbackLog::open_at(&path).unwrap();
        log.record(&fake_entry(1, true)).unwrap();
        drop(log);
        // Re-open, append, verify both entries survive.
        let log2 = FeedbackLog::open_at(&path).unwrap();
        log2.record(&fake_entry(2, false)).unwrap();
        let entries = log2.snapshot().unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn snapshot_skips_corrupt_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.log.jsonl");
        // Manually plant: 1 good, 1 corrupt, 1 good.
        let s = serde_json::to_string(&fake_entry(1, true)).unwrap();
        std::fs::write(&path, format!("{s}\nNOT_JSON\n{s}\n")).unwrap();
        let log = FeedbackLog::open_at(&path).unwrap();
        let entries = log.snapshot().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].turn, 1);
        assert_eq!(entries[1].turn, 1);
    }

    #[test]
    fn snapshot_on_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let log = FeedbackLog::open_at(&dir.path().join("missing.log.jsonl")).unwrap();
        assert!(log.snapshot().unwrap().is_empty());
        assert_eq!(log.len(), 0);
        assert!(log.is_empty());
    }

    #[test]
    fn tail_with_zero_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let log = FeedbackLog::open_at(&dir.path().join("s.log.jsonl")).unwrap();
        assert!(log.tail(0).unwrap().is_empty());
    }
}
