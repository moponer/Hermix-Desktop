//! Real memory backend: read/write MEMORY.md and USER.md in
//! `~/.hermix/memories/`.
//!
//! # 2026-06-07 hardening pass
//!
//! The previous version of this file had three problems that
//! produced user-visible "the memory system is buggy" symptoms in
//! the desktop app:
//!
//! 1. **`dirs_home()` only read `$HOME`.** On Windows that env
//!    var is often unset (cmd.exe doesn't set it; only `$USERPROFILE`
//!    is reliable). The backend fell back to `.` (CWD), so
//!    MEMORY.md got created in the sidecar's CWD instead of the
//!    user's home. **Fixed** — see [`FileMemoryBackend::new`],
//!    which delegates to `hermix_config::paths::hermix_home()`
//!    (reads `$HERMIX_HOME`, then `$HOME`/`$USERPROFILE`, then
//!    `~/.hermix`).
//! 2. **No write-side concurrency control.** The
//!    `dispatch_tools` path runs multiple tool calls in
//!    parallel via `JoinSet`; two `memory.add` calls
//!    interleaving their read-modify-write race and one wins,
//!    silently dropping the other. Fix: process-level
//!    `tokio::sync::Mutex` (cheap, prevents the in-process race)
//!    PLUS an OS-level `fs4::FileExt::lock_exclusive` on a sidecar
//!    `<file>.lock` file (covers the cross-process case where
//!    two sidecars are running, e.g. dev + production).
//! 3. **Non-atomic write.** Plain `tokio::fs::write` truncates
//!    first, then writes — a crash between truncate and write
//!    leaves an empty file and a 2200-char max-out. Fix:
//!    write to `<file>.tmp.<uuid>`, `fsync`, then `rename` over
//!    the destination. The rename is atomic on every
//!    filesystem we support.
//!
//! Plus three smaller but real fixes:
//!
//! 4. **External-drift detection.** Before we write, re-read the
//!    current file bytes and compare. If they differ from the
//!    bytes we read at the start of the operation, another
//!    writer (a different process, or a user editing the file
//!    in their editor) has touched it. We refuse to overwrite
//!    their changes and stash a `.bak.<unix_ts>` copy next to
//!    the file for forensics. Mirrors upstream Hermes' "first
//!    threat message" pattern but applied to *content drift*
//!    rather than prompt injection.
//! 5. **Per-entry size check.** A single entry > 80% of the
//!    total cap is suspicious (LLM fat-finger, or someone
//!    pasted a wall of text). Reject before write.
//! 6. **Profile switch support.** `set_profile_dir(PathBuf)`
//!    swaps the directory at runtime; tests use this to point
//!    the backend at a tempdir without re-instantiating.

use async_trait::async_trait;
use fs4::fs_std::FileExt;
use serde_json::json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::backends::threat_scan::{scan_and_wrap, scan_status_json};
use crate::tools::memory::MemoryBackend;
use hermix_core::path_security::{path_within_root_lexical, strip_win32_long_path_buf};
use hermix_core::ToolError;

const ENTRY_DELIMITER: &str = "\n§\n";
const MEMORY_CHAR_LIMIT: usize = 2200;
const USER_CHAR_LIMIT: usize = 1375;
/// Per-entry limit: 80% of the per-file cap. A single entry
/// bigger than this almost always means the LLM (or a user)
/// pasted something that should be a separate file.
const PER_ENTRY_FRACTION: f64 = 0.80;
/// Suffix for the OS-level lock file.
const LOCK_SUFFIX: &str = ".lock";
/// Suffix for the in-progress write file. Renamed atomically
/// over the destination on success.
const TMP_SUFFIX: &str = ".tmp";
/// Suffix for the drift-detected backup file. Includes a Unix
/// timestamp so successive drifts don't clobber each other.
const BAK_SUFFIX: &str = ".bak";
/// Soft archive trigger. When MEMORY.md exceeds
/// [`MEMORY_CHAR_LIMIT`] (or USER.md exceeds
/// [`USER_CHAR_LIMIT`]), `try_auto_archive` moves older entries
/// to `archive.md` instead of refusing new writes. The frozen
/// snapshot (which feeds the LLM's prefix cache) stays ≤
/// `char_limit_for(target)`; only `archive.md` grows unbounded.
/// The `ARCHIVE_LIMIT` is the rotation trigger for `archive.md`:
/// once it exceeds this many chars, the file is moved to
/// `archive/YYYY-MM.md` and a fresh `archive.md` is started.
const ARCHIVE_LIMIT: usize = 8000;
/// Sibling file that accumulates archived entries. Lives next
/// to MEMORY.md / USER.md inside the memory directory.
const ARCHIVE_FILE: &str = "archive.md";
/// Prefix for the archive index line embedded in MEMORY.md /
/// USER.md. [`FileMemoryBackend::parse_entries`] skips lines
/// starting with this prefix so the index doesn't pollute the
/// entry list. Mirrors the convention upstream
/// `nousresearch/hermes-agent` uses for "audit trail" markers.
const ARCHIVE_INDEX_PREFIX: &str = "<!-- archived:";

/// Real memory backend that stores entries in
/// `<hermix_home>/memories/MEMORY.md` and `<hermix_home>/memories/USER.md`.
///
/// Concurrency:
/// - **`write_lock`** (process-level): prevents two in-process
///   `add`/`replace`/`remove` calls from racing through their
///   read-modify-write sections. Cheap (no syscall). Always
///   acquired first.
/// - **OS file lock on `<file>.lock`** (cross-process): prevents
///   a second `hermix` instance (e.g. dev + production sidecar
///   running side by side, or the CLI invoked from a script
///   while the desktop app is open) from simultaneously
///   writing the same file. Acquired right after `write_lock`.
pub struct FileMemoryBackend {
    /// Directory the memory files live in. Wrapped in a
    /// `std::sync::Mutex` so `set_profile_dir` can swap it at
    /// runtime without async noise. `Arc` so the backend is
    /// still cheap to clone.
    dir: Arc<StdMutex<PathBuf>>,
    /// Process-level write mutex. `tokio::sync::Mutex` so the
    /// `.lock().await` integrates with the async dispatch loop.
    write_lock: Arc<tokio::sync::Mutex<()>>,
    /// **Frozen snapshot** of MEMORY.md and USER.md contents at
    /// construction time (or at the first call to
    /// [`Self::current_snapshot`]). The point of the snapshot
    /// is to protect the LLM's prefix cache: when the LLM's
    /// system prompt contains the memory contents, every byte
    /// of that contents contributes to the prompt's prefix
    /// hash. If `add`/`replace`/`remove` mutate the contents
    /// *in the snapshot the LLM sees*, every subsequent turn
    /// re-pays the cache-miss cost. Mirrors the upstream
    /// `nousresearch/hermes-agent` "frozen snapshot"
    /// semantics.
    ///
    /// Contract:
    /// - `read_file()` does **not** consult the snapshot; it
    ///   reads from disk (so the LLM's *next* turn sees fresh
    ///   data). The snapshot is the protected input to the
    ///   *current* turn.
    /// - `add`/`replace`/`remove` write to disk but do **not**
    ///   mutate the snapshot. The next call to
    ///   `current_snapshot()` will re-pin from disk.
    /// - `current_snapshot()` returns the current snapshot,
    ///   initializing it lazily on the first call.
    snapshot: Arc<StdMutex<MemorySnapshot>>,
}

/// Lazily-initialized frozen view of the two memory files.
/// `init` is `false` until the first `current_snapshot()` call;
/// the snapshot is then pinned for the duration of the process
/// or until the user opts in to refresh via
/// [`FileMemoryBackend::refresh_snapshot`].
#[derive(Default)]
struct MemorySnapshot {
    init: bool,
    memory: String,
    user: String,
}

impl FileMemoryBackend {
    /// Create a backend rooted at the user-level hermix home
    /// (`$HERMIX_HOME` or `~/.hermix`). Reads env at construction
    /// time — use [`Self::with_dir`] for tests or for cases
    /// where the env might change after startup.
    pub fn new() -> Self {
        // Delegate to the canonical helper. The old
        // implementation hand-rolled `env::var("HOME")`, which
        // is *not* set on most Windows shells (cmd.exe, MSYS
        // shells that have been launched without a profile,
        // etc.) — that was the root cause of "memory got
        // created in the wrong place" reports.
        let dir = hermix_config::paths::hermix_home().join("memories");
        Self::with_dir(dir)
    }

    /// Create a backend with an explicit memory directory. The
    /// directory does not need to exist yet (`add` creates it on
    /// first write) but its parent must.
    pub fn with_dir(dir: PathBuf) -> Self {
        // Normalize any `\\?\` long-path prefix on Windows so
        // lock-file / write-file operations all use the same
        // canonical form.
        let dir = strip_win32_long_path_buf(&dir);
        Self {
            dir: Arc::new(StdMutex::new(dir)),
            write_lock: Arc::new(tokio::sync::Mutex::new(())),
            // Snapshot is uninitialized — the first call to
            // `current_snapshot()` will read MEMORY.md / USER.md
            // and pin them.
            snapshot: Arc::new(StdMutex::new(MemorySnapshot::default())),
        }
    }

    /// **Frozen snapshot** mode. Returns the contents of
    /// `MEMORY.md` and `USER.md` *as they were at the first
    /// call to this method* (or at the last
    /// [`Self::refresh_snapshot`]). The LLM is expected to
    /// splice this into its system prompt as a single
    /// prefix-stable block. Subsequent `add`/`replace`/`remove`
    /// calls write to disk but do not change this snapshot;
    /// this is the property that protects the LLM's prefix
    /// cache.
    ///
    /// Returns `(memory_md, user_md)`. Either may be empty
    /// string if the file doesn't exist yet.
    pub fn current_snapshot(&self) -> (String, String) {
        self.snapshot
            .lock()
            .map(|mut g| {
                if !g.init {
                    // Lazy init. We use blocking I/O here for
                    // simplicity — this is called once at
                    // startup or once per "refresh" cycle, not
                    // in a hot path.
                    g.memory = std::fs::read_to_string(self.dir().join("MEMORY.md"))
                        .unwrap_or_default();
                    g.user = std::fs::read_to_string(self.dir().join("USER.md"))
                        .unwrap_or_default();
                    g.init = true;
                }
                (g.memory.clone(), g.user.clone())
            })
            .unwrap_or_else(|_| (String::new(), String::new()))
    }

    /// Re-pin the snapshot from disk. The next call to
    /// `current_snapshot()` will see whatever is on disk *now*.
    /// This is the escape hatch the agent loop uses when it
    /// wants to start a new session (so the new session's
    /// prefix cache starts from a fresh state, not a stale
    /// snapshot from a prior session).
    pub fn refresh_snapshot(&self) {
        if let Ok(mut g) = self.snapshot.lock() {
            g.init = false;
            g.memory.clear();
            g.user.clear();
        }
    }

    /// Swap the memory directory at runtime. The next
    /// `add`/`replace`/`remove` call will operate against the
    /// new directory. Existing data at the old directory is
    /// untouched.
    pub fn set_profile_dir(&self, dir: PathBuf) {
        let dir = strip_win32_long_path_buf(&dir);
        if let Ok(mut guard) = self.dir.lock() {
            *guard = dir;
        }
    }

    /// Snapshot the current memory directory. Cheap (single
    /// mutex lock).
    fn dir(&self) -> PathBuf {
        self.dir
            .lock()
            .map(|g| g.clone())
            .unwrap_or_else(|_| PathBuf::from("."))
    }

    fn path_for(&self, target: &str) -> Result<PathBuf, ToolError> {
        let base = self.dir();
        match target {
            "memory" => Ok(base.join("MEMORY.md")),
            "user" => Ok(base.join("USER.md")),
            _ => Err(ToolError::InvalidParams(format!(
                "Invalid target '{}'. Use 'memory' or 'user'.",
                target
            ))),
        }
    }

    /// Verify the resolved target file lives inside the current
    /// memory directory. Catches a misconfigured `set_profile_dir`
    /// that pointed at `/` or at the workspace root by accident
    /// (we never want memory writes outside the hermix data dir).
    fn ensure_within_dir(&self, path: &Path) -> Result<(), ToolError> {
        let root = self.dir();
        if let Some(err) = path_within_root_lexical(path, &root) {
            return Err(ToolError::ExecutionFailed(err));
        }
        Ok(())
    }

    async fn ensure_dir(&self) -> Result<(), ToolError> {
        let dir = self.dir();
        tokio::fs::create_dir_all(&dir).await.map_err(|e| {
            ToolError::ExecutionFailed(format!(
                "Failed to create memory dir '{}': {}",
                dir.display(),
                e
            ))
        })
    }

    async fn read_file(&self, path: &Path) -> Result<String, ToolError> {
        match tokio::fs::read_to_string(path).await {
            Ok(content) => Ok(content),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(ToolError::ExecutionFailed(format!(
                "Failed to read '{}': {}",
                path.display(),
                e
            ))),
        }
    }

    /// Atomically write `content` to `path` with the following
    /// guarantees:
    ///
    /// 1. **Process-level mutex** held for the whole
    ///    read-modify-write section (caller's responsibility).
    /// 2. **OS-level `lock_exclusive` on `<path>.lock`** held
    ///    while we write — covers the cross-process case.
    /// 3. **Pre-write drift check**: re-read `path`, compare
    ///    bytes to the bytes we read at the start. If they
    ///    differ, refuse + drop a `.bak.<ts>` snapshot.
    /// 4. **Atomic rename**: write to `<path>.tmp.<uuid>`, fsync,
    ///    rename over `path`. Crash-safe.
    /// Atomically write `content` to `path`. `pub(super)` so the
    /// `tests` submodule (and the wider crate integration tests)
    /// can exercise the drift detector directly. Production
    /// callers should use [`Self::add`] / [`Self::replace`] /
    /// [`Self::remove`] which compose `atomic_write` with their
    /// own read + parse + format logic.
    pub(super) async fn atomic_write(
        &self,
        path: &Path,
        content: &str,
        original_bytes: &[u8],
    ) -> Result<(), ToolError> {
        self.ensure_dir().await?;

        // Step 3: drift check.
        if !original_bytes.is_empty() {
            let current = tokio::fs::read(path).await.unwrap_or_default();
            if current != original_bytes {
                let ts = unix_ts();
                let bak = path.with_extension(format!(
                    "{}{}{}",
                    BAK_SUFFIX,
                    ts,
                    path.extension()
                        .and_then(|e| e.to_str())
                        .map(|e| format!(".{}", e))
                        .unwrap_or_default()
                ));
                // Best-effort backup. If this fails the user has
                // bigger problems than the missing .bak.
                let _ = tokio::fs::copy(path, &bak).await;
                return Err(ToolError::ExecutionFailed(format!(
                    "Memory file '{}' was modified by another writer \
                     since we read it (drift detected). Refusing to \
                     overwrite. A snapshot was saved to '{}'. \
                     Re-read and try again.",
                    path.display(),
                    bak.display()
                )));
            }
        }

        // Step 2: cross-process file lock.
        let lock_path = with_suffix(path, LOCK_SUFFIX);
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| {
                ToolError::ExecutionFailed(format!(
                    "Failed to open lock file '{}': {}",
                    lock_path.display(),
                    e
                ))
            })?;
        // `lock_exclusive` blocks until the lock is acquired.
        // If the operation is cancelled (e.g. the dispatch
        // timed out), the Drop on the lock file's `unlock`
        // would not fire — but `fs4`'s docs note the OS will
        // release the lock when the FD is closed, which
        // happens when `lock_file` goes out of scope at the
        // end of this function. So cancellation safety is OK
        // for the simple "we own the FD for the duration of
        // the write" pattern.
        if let Err(e) = lock_file.lock_exclusive() {
            return Err(ToolError::ExecutionFailed(format!(
                "Failed to acquire memory file lock '{}': {}",
                lock_path.display(),
                e
            )));
        }

        // Step 4: atomic write via tmp + rename.
        let tmp_path = with_suffix(
            path,
            &format!("{}{}", TMP_SUFFIX, uuid::Uuid::new_v4()),
        );
        let write_result: Result<(), ToolError> = async {
            tokio::fs::write(&tmp_path, content).await.map_err(|e| {
                ToolError::ExecutionFailed(format!(
                    "Failed to write tmp file '{}': {}",
                    tmp_path.display(),
                    e
                ))
            })?;
            // fsync the data file before rename so the rename
            // is durable across crashes.
            if let Ok(f) = tokio::fs::File::open(&tmp_path).await {
                let _ = f.sync_all().await;
            }
            tokio::fs::rename(&tmp_path, path).await.map_err(|e| {
                ToolError::ExecutionFailed(format!(
                    "Failed to rename '{}' -> '{}': {}",
                    tmp_path.display(),
                    path.display(),
                    e
                ))
            })?;
            Ok(())
        }
        .await;
        // Release the lock regardless of write success — the
        // explicit unlock keeps things tidy on Windows where
        // close-then-unlock-order matters for some FS drivers.
        let _ = lock_file.unlock();
        // Best-effort cleanup of the tmp file if rename
        // failed.
        if write_result.is_err() {
            let _ = tokio::fs::remove_file(&tmp_path).await;
        }
        write_result
    }

    fn parse_entries(content: &str) -> Vec<String> {
        if content.trim().is_empty() {
            return Vec::new();
        }
        // Strip any archive index line(s) first — they live at
        // the end of the file (or anywhere `try_auto_archive`
        // appended them) and must not bleed into the entry text.
        // Walking by line is the safest fix: the old `split on
        // ENTRY_DELIMITER + filter prefix` only worked when the
        // index line was preceded by `\n§\n`. When the kept set
        // was a single entry, no delimiter separated the entry
        // from the index line and the latter got concatenated
        // onto the entry text — which is the bug that broke
        // `file_total_limit_enforced`.
        let mut stripped = String::with_capacity(content.len());
        for line in content.lines() {
            if line.trim_start().starts_with(ARCHIVE_INDEX_PREFIX) {
                // Drop the index line entirely (no separator
                // replacement — `split(ENTRY_DELIMITER)` handles
                // the rest by trimming).
                continue;
            }
            stripped.push_str(line);
            stripped.push('\n');
        }
        stripped
            .split(ENTRY_DELIMITER)
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .map(ToString::to_string)
            .collect()
    }

    fn format_entries(entries: &[String]) -> String {
        entries.join(ENTRY_DELIMITER)
    }

    fn preview(text: &str) -> String {
        if text.chars().count() <= 80 {
            text.to_string()
        } else {
            let head: String = text.chars().take(80).collect();
            format!("{head}...")
        }
    }

    fn char_limit_for(target: &str) -> usize {
        if target == "user" {
            USER_CHAR_LIMIT
        } else {
            MEMORY_CHAR_LIMIT
        }
    }

    /// Per-entry size check. Returns an `Err` if any single
    /// entry is more than `PER_ENTRY_FRACTION` of the per-file
    /// cap. Catches LLMs that paste a wall of text instead of
    /// summarizing.
    fn check_entry_sizes(
        target: &str,
        entries: &[String],
    ) -> Result<(), ToolError> {
        let limit = Self::char_limit_for(target);
        let entry_cap =
            ((limit as f64) * PER_ENTRY_FRACTION).floor() as usize;
        for e in entries {
            if e.chars().count() > entry_cap {
                return Err(ToolError::InvalidParams(format!(
                    "Entry is too large ({} chars, max {} chars per \
                     entry, which is 80% of the {}-char {} cap). \
                     Split the content into smaller entries or save \
                     it as a file.",
                    e.chars().count(),
                    entry_cap,
                    limit,
                    target
                )));
            }
        }
        Ok(())
    }

    fn success_response(
        target: &str,
        entries: &[String],
        message: &str,
        archived_to: Option<&Path>,
    ) -> String {
        let limit = Self::char_limit_for(target);
        let current = entries.join(ENTRY_DELIMITER).chars().count();
        let pct = current
            .checked_mul(100)
            .and_then(|v| v.checked_div(limit))
            .unwrap_or(0)
            .min(100);
        let mut body = json!({
            "success": true,
            "target": target,
            "message": message,
            "entries": entries,
            "entry_count": entries.len(),
            "usage": format!("{pct}% - {current}/{limit} chars")
        });
        // Surface the archive path so the LLM (and the chat UI)
        // can tell the user "older N entries were moved to
        // archive.md" instead of the previous silent rejection.
        // Only present when an archive actually happened on
        // this call; absent on the common no-archive path so
        // existing tool consumers see no schema change.
        if let Some(p) = archived_to {
            body["archived_to"] = json!(p.display().to_string());
        }
        body.to_string()
    }

    /// Try to free space in MEMORY.md (or USER.md) by moving
    /// older entries to `archive.md`. Called from
    /// [`MemoryBackend::add`] *before* refusing a new write —
    /// if the archive trims enough room, the new entry goes
    /// through instead of being rejected.
    ///
    /// `target_kept_size` is the maximum chars (after the
    /// archive runs) the kept entries may occupy in the
    /// memory file. The caller (`add`) computes it as
    /// `limit - new_entry_chars - delimiter_cost`, which
    /// guarantees the new entry fits on top of the kept
    /// entries. External callers (tests, future ops) can pass
    /// any value ≤ `char_limit_for(target)` to control how
    /// aggressively to trim.
    ///
    /// Returns `Some(archive_path)` if any entries were moved
    /// and `None` if the kept entries already fit within
    /// `target_kept_size` (the common no-op path).
    ///
    /// # Mechanics
    ///
    /// - Keeps the **newest** entries (the ones the LLM most
    ///   likely needs to recall), trims the oldest.
    /// - The frozen snapshot (which feeds the LLM's prefix
    ///   cache) stays ≤ `char_limit_for(target)` — this is
    ///   the whole point. archive.md is **not** part of the
    ///   snapshot.
    /// - `archive.md` grows up to [`ARCHIVE_LIMIT`] (= 8000)
    ///   chars, then rotates to `archive/YYYY-MM.md` (or
    ///   `archive/YYYY-MM-<unix_ts>.md` if the month file
    ///   already exists from an earlier rotation in the same
    ///   calendar month — defensive against the case where
    ///   the LLM blasts the file system in a single session).
    /// - `archive/YYYY-MM.md` is append-only by month, so a
    ///   user can grep across months for a forgotten fact.
    ///
    /// # Concurrency
    ///
    /// Must be called **inside** `add`'s `write_lock` guard.
    /// This function does not take the lock itself — it
    /// composes with `atomic_write`'s per-file OS lock, which
    /// is enough because the process Mutex already serializes
    /// every call site.
    pub(super) async fn try_auto_archive(
        &self,
        target: &str,
        target_kept_size: usize,
    ) -> Result<Option<PathBuf>, ToolError> {
        let limit = Self::char_limit_for(target);
        let path = self.path_for(target)?;
        self.ensure_within_dir(&path)?;

        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let entries = Self::parse_entries(&content);

        let delimiter_cost = ENTRY_DELIMITER.chars().count();
        // Clamp to ≤ limit so a misbehaving caller can't ask
        // for more room than the cap allows.
        let target_keep = target_kept_size.min(limit);

        // Walk newest-to-oldest (reverse), accumulate chars
        // until we hit target_keep. Everything older than the
        // first rejected entry gets archived.
        let mut kept_rev: Vec<String> = Vec::new();
        let mut kept_size = 0usize;
        let mut archive_until = entries.len(); // exclusive
        for (i, e) in entries.iter().enumerate().rev() {
            let cost = e.chars().count() + delimiter_cost;
            if kept_size + cost <= target_keep {
                kept_rev.push(e.clone());
                kept_size += cost;
                archive_until = i;
            } else {
                break;
            }
        }

        let to_archive: Vec<String> = entries[..archive_until].to_vec();
        let kept: Vec<String> = kept_rev.into_iter().rev().collect();
        if to_archive.is_empty() {
            return Ok(None);
        }

        let dir = self.dir();
        tokio::fs::create_dir_all(&dir).await.map_err(|e| {
            ToolError::ExecutionFailed(format!(
                "Failed to create memory dir '{}': {}",
                dir.display(),
                e
            ))
        })?;

        let archive_path = dir.join(ARCHIVE_FILE);

        // Rotate archive.md → archive/YYYY-MM.md when it
        // exceeds ARCHIVE_LIMIT. Use atomic_write on the
        // rotated file too so a crash mid-rotation doesn't
        // lose the prior month's data.
        if let Ok(existing) = std::fs::read_to_string(&archive_path) {
            if existing.chars().count() > ARCHIVE_LIMIT {
                let now = chrono::Local::now();
                let month_dir = dir.join("archive");
                tokio::fs::create_dir_all(&month_dir).await.map_err(|e| {
                    ToolError::ExecutionFailed(format!(
                        "Failed to create archive dir '{}': {}",
                        month_dir.display(),
                        e
                    ))
                })?;
                let month_stem = now.format("%Y-%m").to_string();
                let mut month_file = month_dir.join(format!("{month_stem}.md"));
                if month_file.exists() {
                    // Same month, already rotated this turn
                    // (or earlier in the same session). Add a
                    // unix-ts suffix so we don't clobber.
                    month_file =
                        month_dir.join(format!("{month_stem}-{}.md", unix_ts()));
                }
                // Best-effort rename. If it fails (e.g.
                // Windows file lock from another process
                // holding the destination), we fall through
                // and append to the existing archive.md —
                // archive.md will just be bigger than the
                // soft limit, which is preferable to dropping
                // data.
                if std::fs::rename(&archive_path, &month_file).is_ok() {
                    tracing::info!(
                        archive = %month_file.display(),
                        prev_chars = existing.chars().count(),
                        "Rotated archive.md (>ARCHIVE_LIMIT) to monthly file"
                    );
                }
            }
        }

        // Append this batch to archive.md. Use atomic_write
        // so a crash mid-write doesn't truncate the entire
        // archive history. The drift check sees the
        // post-rotate baseline (empty if we just rotated).
        let existing_archive = std::fs::read_to_string(&archive_path).unwrap_or_default();
        let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let batch = to_archive.join(ENTRY_DELIMITER);
        let new_archive = if existing_archive.is_empty() {
            format!(
                "\n<!-- batch: {} ({} entries) -->\n{}\n",
                ts,
                to_archive.len(),
                batch
            )
        } else {
            format!(
                "{}\n\n<!-- batch: {} ({} entries) -->\n{}\n",
                existing_archive,
                ts,
                to_archive.len(),
                batch
            )
        };
        self.atomic_write(&archive_path, &new_archive, existing_archive.as_bytes())
            .await?;

        // Rewrite MEMORY.md (or USER.md) with kept entries +
        // a one-line index pointing at archive.md. The index
        // is human-readable so the user can find their
        // archived history without grepping.
        let index_line = format!(
            "{} {} entries to {} at {} -->",
            ARCHIVE_INDEX_PREFIX,
            to_archive.len(),
            archive_path
                .file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new(ARCHIVE_FILE))
                .to_string_lossy(),
            ts,
        );
        let new_memory = if kept.is_empty() {
            format!("{index_line}\n")
        } else {
            format!("{}\n{index_line}\n", kept.join(ENTRY_DELIMITER))
        };
        // Re-read MEMORY.md bytes for the drift baseline —
        // `content` (read at the top of this function) is
        // stale because we just wrote archive.md in the
        // meantime. Drift check against the freshest bytes
        // we can see.
        let current_bytes = std::fs::read(&path).unwrap_or_default();
        self.atomic_write(&path, &new_memory, &current_bytes).await?;

        Ok(Some(archive_path))
    }
}

impl Default for FileMemoryBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MemoryBackend for FileMemoryBackend {
    async fn add(&self, target: &str, content: &str) -> Result<String, ToolError> {
        let path = self.path_for(target)?;
        self.ensure_within_dir(&path)?;
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return Err(ToolError::InvalidParams(
                "Content cannot be empty for action='add'.".to_string(),
            ));
        }

        let _guard = self.write_lock.lock().await;
        let original = self.read_file(&path).await?;
        let mut entries = Self::parse_entries(&original);

        if entries.iter().any(|e| e == trimmed) {
            return Ok(Self::success_response(
                target,
                &entries,
                "Entry already exists (no duplicate added).",
                None,
            ));
        }

        // Per-entry check (post-trim). Run this *before* the
        // total-cap check so a single oversized entry fails
        // fast — auto-archiving can't help if one entry alone
        // is bigger than the entire cap.
        Self::check_entry_sizes(target, &[trimmed.to_string()])?;

        let mut new_entries = entries.clone();
        new_entries.push(trimmed.to_string());
        let new_total = new_entries.join(ENTRY_DELIMITER).chars().count();
        let limit = Self::char_limit_for(target);
        // `original` is the drift baseline we'll pass to
        // `atomic_write`. If `try_auto_archive` runs and
        // mutates MEMORY.md, we need to refresh this baseline
        // so the drift check on the final write doesn't
        // false-positive on our own archive write.
        let mut original_for_drift = original;
        let mut archived_to: Option<PathBuf> = None;
        if new_total > limit {
            // Try to make room by moving older entries to
            // archive.md. If the archive frees enough space,
            // the new entry fits and we continue. Otherwise
            // we surface a clear "still over the limit" error
            // so the LLM knows to split the entry.
            //
            // target_kept_size is the maximum size *after* the
            // archive runs: we leave room for the incoming
            // entry plus its delimiter, plus 1 char of safety
            // margin so a near-future write doesn't immediately
            // trigger another archive round-trip. Clamped at
            // the function entry to ≤ `limit` — see
            // `try_auto_archive` doc.
            let delimiter_cost = ENTRY_DELIMITER.chars().count();
            let target_kept_size = limit
                .saturating_sub(trimmed.chars().count())
                .saturating_sub(delimiter_cost)
                .saturating_sub(1);
            match self.try_auto_archive(target, target_kept_size).await? {
                Some(p) => {
                    // Re-read baseline + entries after
                    // archive mutated the file.
                    let post = self.read_file(&path).await?;
                    entries = Self::parse_entries(&post);
                    original_for_drift = post;
                    archived_to = Some(p);
                    let recalc_total = {
                        let mut tmp = entries.clone();
                        tmp.push(trimmed.to_string());
                        tmp.join(ENTRY_DELIMITER).chars().count()
                    };
                    if recalc_total > limit {
                        let current =
                            entries.join(ENTRY_DELIMITER).chars().count();
                        return Err(ToolError::ExecutionFailed(format!(
                            "Memory at {current}/{limit} chars after auto-archive. \
                             Adding this entry ({}) would still exceed the limit. \
                             Split the content into smaller entries or save it as a file.",
                            trimmed.chars().count()
                        )));
                    }
                }
                None => {
                    // Archive didn't run (e.g. a single entry
                    // is already at the cap — though
                    // check_entry_sizes above should have
                    // caught that). Surface the original
                    // limit error verbatim.
                    let current =
                        entries.join(ENTRY_DELIMITER).chars().count();
                    return Err(ToolError::ExecutionFailed(format!(
                        "Memory at {current}/{limit} chars. Adding this entry ({}) would exceed the limit.",
                        trimmed.chars().count()
                    )));
                }
            }
        }

        // Threat scan: if the entry contains prompt-injection
        // markers, wrap it with a `BLOCKED: ` prefix so the LLM
        // sees the marker (and the user can audit the raw text
        // in MEMORY.md) but the content is not silently let
        // through. Mirrors the upstream
        // `nousresearch/hermes-agent` "first threat message"
        // pattern. We still persist the entry — the
        // alternative ("drop the entry") is worse because the
        // user can't tell what was attempted.
        let (stored, scan) = scan_and_wrap(trimmed);

        let mut final_entries = entries.clone();
        final_entries.push(stored);
        self.atomic_write(
            &path,
            &Self::format_entries(&final_entries),
            original_for_drift.as_bytes(),
        )
        .await?;
        let mut resp = Self::success_response(
            target,
            &final_entries,
            if scan.matches.is_empty() {
                "Entry added."
            } else {
                "Entry added but flagged by threat-scan (BLOCKED: prefix). \
                 The LLM will be told to ignore the content; you can review \
                 the raw text in MEMORY.md and remove it if it's a false \
                 positive."
            },
            archived_to.as_deref(),
        );
        if !scan.matches.is_empty() {
            // Append the scan status JSON so the LLM has
            // machine-readable details if it wants to surface
            // them to the user.
            resp = format!("{}\n{}", resp, scan_status_json(&scan));
        }
        Ok(resp)
    }

    async fn replace(
        &self,
        target: &str,
        old_text: &str,
        new_content: &str,
    ) -> Result<String, ToolError> {
        let path = self.path_for(target)?;
        self.ensure_within_dir(&path)?;
        let old_text = old_text.trim();
        let new_content = new_content.trim();
        if old_text.is_empty() {
            return Err(ToolError::InvalidParams(
                "old_text cannot be empty for action='replace'.".to_string(),
            ));
        }
        if new_content.is_empty() {
            return Err(ToolError::InvalidParams(
                "content cannot be empty for action='replace'.".to_string(),
            ));
        }

        let _guard = self.write_lock.lock().await;
        let original = self.read_file(&path).await?;
        let mut entries = Self::parse_entries(&original);

        let matches: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter_map(|(idx, e)| {
                if e.contains(old_text) {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect();

        if matches.is_empty() {
            return Err(ToolError::ExecutionFailed(format!(
                "No entry matched '{}'.",
                old_text
            )));
        }

        if matches.len() > 1 {
            let distinct: HashSet<String> = matches.iter().map(|i| entries[*i].clone()).collect();
            if distinct.len() > 1 {
                let previews: Vec<String> = matches
                    .iter()
                    .map(|i| Self::preview(&entries[*i]))
                    .collect();
                return Err(ToolError::ExecutionFailed(format!(
                    "Multiple entries matched '{}'. Be more specific. Matches: {}",
                    old_text,
                    previews.join(" | ")
                )));
            }
        }

        let idx = matches[0];
        let mut candidate = entries.clone();
        // Threat scan the new content too. Same wrap-or-pass-
        // through logic as `add`.
        let (stored_new, scan) = scan_and_wrap(new_content);
        candidate[idx] = stored_new;
        let new_total = candidate.join(ENTRY_DELIMITER).chars().count();
        let limit = Self::char_limit_for(target);
        if new_total > limit {
            return Err(ToolError::ExecutionFailed(format!(
                "Replacement would put memory at {new_total}/{limit} chars."
            )));
        }
        // Per-entry check (post-trim, pre-wrap — the wrap
        // adds chars).
        Self::check_entry_sizes(target, &[new_content.to_string()])?;

        entries = candidate;
        self.atomic_write(&path, &Self::format_entries(&entries), original.as_bytes())
            .await?;
        let mut resp = Self::success_response(
            target,
            &entries,
            if scan.matches.is_empty() {
                "Entry replaced."
            } else {
                "Entry replaced but new content flagged by threat-scan \
                 (BLOCKED: prefix)."
            },
            None,
        );
        if !scan.matches.is_empty() {
            resp = format!("{}\n{}", resp, scan_status_json(&scan));
        }
        Ok(resp)
    }

    async fn remove(&self, target: &str, old_text: &str) -> Result<String, ToolError> {
        let path = self.path_for(target)?;
        self.ensure_within_dir(&path)?;
        let old_text = old_text.trim();
        if old_text.is_empty() {
            return Err(ToolError::InvalidParams(
                "old_text cannot be empty for action='remove'.".to_string(),
            ));
        }

        let _guard = self.write_lock.lock().await;
        let original = self.read_file(&path).await?;
        let mut entries = Self::parse_entries(&original);
        let matches: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter_map(|(idx, e)| {
                if e.contains(old_text) {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect();

        if matches.is_empty() {
            return Err(ToolError::ExecutionFailed(format!(
                "No entry matched '{}'.",
                old_text
            )));
        }

        if matches.len() > 1 {
            let distinct: HashSet<String> = matches.iter().map(|i| entries[*i].clone()).collect();
            if distinct.len() > 1 {
                let previews: Vec<String> = matches
                    .iter()
                    .map(|i| Self::preview(&entries[*i]))
                    .collect();
                return Err(ToolError::ExecutionFailed(format!(
                    "Multiple entries matched '{}'. Be more specific. Matches: {}",
                    old_text,
                    previews.join(" | ")
                )));
            }
        }

        let idx = matches[0];
        entries.remove(idx);
        self.atomic_write(&path, &Self::format_entries(&entries), original.as_bytes())
            .await?;

        Ok(Self::success_response(
            target,
            &entries,
            "Entry removed.",
            None,
        ))
    }
}

// --- helpers ----------------------------------------------------------------

/// Build a sibling path with a different suffix. Used for the
/// `.lock` and `.tmp.<uuid>` and `.bak.<ts>` sidecar files.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("memory.md");
    parent.join(format!("{}.{}", stem, suffix.trim_start_matches('.')))
}

/// Current Unix timestamp in seconds. Used to make `.bak.<ts>`
/// names unique.
fn unix_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_backend() -> (tempfile::TempDir, FileMemoryBackend) {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("memories");
        let backend = FileMemoryBackend::with_dir(dir);
        (tmp, backend)
    }

    #[tokio::test]
    async fn add_creates_file_and_persists() {
        let (_tmp, backend) = make_backend();
        let resp = backend
            .add("memory", "the user prefers dark mode")
            .await
            .unwrap();
        assert!(resp.contains("Entry added"));
        let on_disk = tokio::fs::read_to_string(backend.dir().join("MEMORY.md"))
            .await
            .unwrap();
        assert!(on_disk.contains("the user prefers dark mode"));
    }

    #[tokio::test]
    async fn replace_updates_entry() {
        let (_tmp, backend) = make_backend();
        backend.add("memory", "old fact").await.unwrap();
        backend
            .replace("memory", "old fact", "new fact")
            .await
            .unwrap();
        let entries = parse_entries_at(&backend, "memory").await;
        assert_eq!(entries, vec!["new fact".to_string()]);
    }

    #[tokio::test]
    async fn remove_drops_entry() {
        let (_tmp, backend) = make_backend();
        backend.add("memory", "transient note").await.unwrap();
        backend.remove("memory", "transient").await.unwrap();
        let entries = parse_entries_at(&backend, "memory").await;
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn duplicate_add_is_noop() {
        let (_tmp, backend) = make_backend();
        backend.add("memory", "dup").await.unwrap();
        let resp = backend.add("memory", "dup").await.unwrap();
        assert!(resp.contains("already exists"));
        let entries = parse_entries_at(&backend, "memory").await;
        assert_eq!(entries.len(), 1);
    }

    #[tokio::test]
    async fn oversized_entry_rejected() {
        let (_tmp, backend) = make_backend();
        // MEMORY_CHAR_LIMIT is 2200, per-entry cap is 80% = 1760
        let big = "x".repeat(2000);
        let err = backend.add("memory", &big).await.unwrap_err();
        assert!(format!("{}", err).contains("too large"));
    }

    #[tokio::test]
    async fn file_total_limit_enforced() {
        // Pre-archive behavior: adding past the cap was
        // rejected. Now (post-2026-06-21 auto-archive), the
        // limit is enforced by moving older entries to
        // archive.md rather than refusing. Verify that path:
        // three 1000-char entries push us past the 2200 cap,
        // so the third add must (a) succeed, (b) report
        // `archived_to`, (c) leave only the newest two in
        // MEMORY.md, (d) place the oldest in archive.md.
        let (_tmp, backend) = make_backend();
        backend
            .add("memory", &"a".repeat(1000))
            .await
            .unwrap();
        backend
            .add("memory", &"b".repeat(1000))
            .await
            .unwrap();
        let resp = backend
            .add("memory", &"c".repeat(1000))
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(parsed["success"], serde_json::json!(true));
        // The third add triggered an archive.
        assert!(
            parsed.get("archived_to").is_some(),
            "third add should report archived_to; got: {resp}"
        );
        let archive_path = parsed["archived_to"].as_str().unwrap();
        assert!(archive_path.ends_with("archive.md"));
        // MEMORY.md now contains only the two newest entries.
        let mem_entries = parse_entries_at(&backend, "memory").await;
        assert_eq!(mem_entries.len(), 2);
        assert_eq!(mem_entries[0], "b".repeat(1000));
        assert_eq!(mem_entries[1], "c".repeat(1000));
        // archive.md contains the evicted entry.
        let archive_content = std::fs::read_to_string(archive_path).unwrap();
        assert!(archive_content.contains(&"a".repeat(1000)));
        assert!(archive_content.contains("batch:"));
        // And MEMORY.md is back under the cap.
        let mem_size: usize = mem_entries
            .iter()
            .map(|e| e.chars().count() + ENTRY_DELIMITER.chars().count())
            .sum::<usize>()
            .saturating_sub(ENTRY_DELIMITER.chars().count());
        assert!(mem_size <= MEMORY_CHAR_LIMIT);
    }

    #[tokio::test]
    async fn try_auto_archive_no_op_when_under_limit() {
        let (_tmp, backend) = make_backend();
        backend.add("memory", "tiny").await.unwrap();
        // No archive needed; MEMORY.md is far under the cap.
        let result = backend
            .try_auto_archive("memory", FileMemoryBackend::char_limit_for("memory"))
            .await
            .unwrap();
        assert!(result.is_none(), "no archive when under limit");
        // archive.md should not have been created.
        let archive_path = backend.dir().join(ARCHIVE_FILE);
        assert!(!archive_path.exists());
    }

    #[tokio::test]
    async fn try_auto_archive_keeps_newest_when_over() {
        let (tmp, backend) = make_backend();
        // Hand-craft a MEMORY.md that exceeds the cap so
        // `try_auto_archive` sees the "over limit" condition
        // itself. Using `add()` here would be wrong because
        // `add` itself triggers an archive on overflow —
        // by the time we get back here, MEMORY.md is already
        // trimmed and `try_auto_archive` correctly no-ops.
        let path = backend.path_for("memory").unwrap();
        let entries: Vec<String> = (0..5)
            .map(|i| format!("{} {}", "x".repeat(500 - 8), i))
            .collect();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, entries.join(ENTRY_DELIMITER)).unwrap();
        // sanity: file is now 5 * ~500 chars = 2500 > 2200.
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.chars().count() > MEMORY_CHAR_LIMIT);

        let result = backend
            .try_auto_archive("memory", FileMemoryBackend::char_limit_for("memory"))
            .await
            .unwrap();
        assert!(result.is_some(), "archive must run when over the cap");
        // The four oldest entries ("0", "1", "2", "3") must
        // be in archive.md; the newest ("4") must stay in
        // MEMORY.md.
        let mem_entries = parse_entries_at(&backend, "memory").await;
        assert!(
            mem_entries.iter().any(|e| e.ends_with(" 4")),
            "newest entry must remain in MEMORY.md; got: {mem_entries:?}"
        );
        let archive_path = result.unwrap();
        let archive_content = std::fs::read_to_string(&archive_path).unwrap();
        // The oldest entry ("0") must always end up in archive.md
        // because it has the highest archive priority (the walk
        // evicts oldest first). Younger entries (1, 2, 3) stay
        // because they fit within the kept budget. We don't
        // hard-code "all of 0..4" — the kept size depends on the
        // exact entry length and the cap, which the fixture
        // tunes for "at least one entry evicted".
        assert!(
            archive_content.contains(" 0"),
            "oldest entry 0 should be in archive.md; got:\n{archive_content}"
        );
        // And MEMORY.md must no longer be over the cap.
        let mem_entries = parse_entries_at(&backend, "memory").await;
        let mem_size: usize = mem_entries
            .iter()
            .map(|e| e.chars().count() + ENTRY_DELIMITER.chars().count())
            .sum::<usize>()
            .saturating_sub(ENTRY_DELIMITER.chars().count());
        assert!(mem_size <= MEMORY_CHAR_LIMIT);
    }

    #[tokio::test]
    async fn parse_entries_skips_archive_index_line() {
        let content = format!(
            "first entry\n§\nsecond entry\n§\n{} 2 entries to archive.md at 2026-06-21 08:00:00 -->",
            ARCHIVE_INDEX_PREFIX
        );
        let entries = FileMemoryBackend::parse_entries(&content);
        assert_eq!(entries, vec!["first entry".to_string(), "second entry".to_string()]);
        // Round-trip: re-formatting and re-parsing must not
        // re-introduce the index line as an entry.
        let reformatted = FileMemoryBackend::format_entries(&entries);
        let reparsed = FileMemoryBackend::parse_entries(&reformatted);
        assert_eq!(reparsed, entries);
    }

    #[tokio::test]
    async fn try_auto_archive_rotates_when_archive_huge() {
        let (_tmp, backend) = make_backend();
        // Pre-populate archive.md to just over ARCHIVE_LIMIT
        // so the next archive run triggers rotation.
        let archive_path = backend.dir().join(ARCHIVE_FILE);
        std::fs::create_dir_all(backend.dir()).unwrap();
        std::fs::write(&archive_path, "y".repeat(ARCHIVE_LIMIT + 100)).unwrap();
        // Fill MEMORY.md past the cap.
        for i in 0..5 {
            let entry = format!("{} {}", "z".repeat(500 - 8), i);
            backend.add("memory", &entry).await.unwrap();
        }
        let _ = backend
            .try_auto_archive("memory", FileMemoryBackend::char_limit_for("memory"))
            .await
            .unwrap();
        // archive.md should have been rotated to
        // archive/YYYY-MM.md. Use the canonical month
        // directory to find it.
        let month_dir = backend.dir().join("archive");
        assert!(
            month_dir.exists(),
            "rotation target dir {} should exist",
            month_dir.display()
        );
        let mut rotated = false;
        for entry in std::fs::read_dir(&month_dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("20") && name.ends_with(".md") {
                rotated = true;
                break;
            }
        }
        assert!(rotated, "expected a rotated monthly archive file");
    }

    #[tokio::test]
    async fn add_response_omits_archived_to_when_no_archive() {
        let (_tmp, backend) = make_backend();
        let resp = backend.add("memory", "small entry").await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(parsed["success"], serde_json::json!(true));
        // No archive happened — field must be absent so the
        // schema is unchanged for the common path.
        assert!(
            parsed.get("archived_to").is_none(),
            "no archive should mean no archived_to field; got: {resp}"
        );
    }

    #[tokio::test]
    async fn add_single_huge_entry_still_rejected_after_archive_attempt() {
        // A single entry > 80% of the cap must still be
        // rejected — auto-archive can't help because the
        // entry itself is too big. Verifies that
        // `check_entry_sizes` runs *before* the archive
        // dance in `add`.
        let (_tmp, backend) = make_backend();
        backend.add("memory", &"a".repeat(1000)).await.unwrap();
        backend.add("memory", &"b".repeat(1000)).await.unwrap();
        let big = "x".repeat(2000);
        let err = backend.add("memory", &big).await.unwrap_err();
        let msg = format!("{}", err);
        assert!(
            msg.contains("too large"),
            "single oversized entry must hit per-entry cap; got: {msg}"
        );
    }

    #[tokio::test]
    async fn drift_detected_refuses_overwrite() {
        let (tmp, backend) = make_backend();
        backend.add("memory", "first").await.unwrap();
        // Simulate an external writer (user editing in their
        // editor, or a parallel hermix process).
        let target = backend.dir().join("MEMORY.md");
        // Capture the bytes we know the backend last wrote —
        // "first" followed by the ENTRY_DELIMITER footer. This
        // is the "baseline" we'll force `add` to re-read so
        // the atomic_write step detects the drift.
        let baseline = std::fs::read(&target).unwrap();
        // Tamper with the file to simulate an external writer.
        std::fs::write(&target, "TOTALLY DIFFERENT").unwrap();
        // Now manually invoke the drift check that `add` would
        // run by calling atomic_write with the baseline as
        // `original_bytes`. The drift detector compares
        // `original_bytes` (baseline) against the current
        // on-disk bytes ("TOTALLY DIFFERENT") and must
        // refuse + drop a .bak.
        let err = backend
            .atomic_write(&target, "REWRITE\n§\n", &baseline)
            .await
            .expect_err("drift detector must refuse when baseline != on-disk");
        assert!(format!("{}", err).contains("drift detected"));
        // A .bak file should exist next to MEMORY.md.
        let mut has_bak = false;
        for entry in std::fs::read_dir(backend.dir()).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name().to_string_lossy().contains(BAK_SUFFIX) {
                has_bak = true;
                break;
            }
        }
        assert!(has_bak, ".bak snapshot should be created on drift");
        // And the original user content is preserved.
        let on_disk = std::fs::read_to_string(&target).unwrap();
        assert_eq!(on_disk, "TOTALLY DIFFERENT");
        // Use tmp so it isn't dropped (would unlink the parent).
        let _ = tmp;
    }

    #[tokio::test]
    async fn set_profile_dir_swaps_target() {
        let (_tmp, backend) = make_backend();
        let new_dir = tempdir().unwrap();
        let new_memory_dir = new_dir.path().join("memories");
        backend.set_profile_dir(new_memory_dir.clone());
        backend
            .add("memory", "in the new dir")
            .await
            .unwrap();
        let on_disk =
            tokio::fs::read_to_string(new_memory_dir.join("MEMORY.md"))
                .await
                .unwrap();
        assert!(on_disk.contains("in the new dir"));
    }

    #[tokio::test]
    async fn user_and_memory_are_independent() {
        let (_tmp, backend) = make_backend();
        backend.add("memory", "agent fact").await.unwrap();
        backend.add("user", "user fact").await.unwrap();
        let mem = parse_entries_at(&backend, "memory").await;
        let user = parse_entries_at(&backend, "user").await;
        assert_eq!(mem, vec!["agent fact".to_string()]);
        assert_eq!(user, vec!["user fact".to_string()]);
    }

    #[tokio::test]
    async fn invalid_target_rejected() {
        let (_tmp, backend) = make_backend();
        let err = backend.add("notes", "x").await.unwrap_err();
        assert!(format!("{}", err).contains("Invalid target"));
    }

    async fn parse_entries_at(backend: &FileMemoryBackend, target: &str) -> Vec<String> {
        let path = backend.path_for(target).unwrap();
        let content = tokio::fs::read_to_string(&path)
            .await
            .unwrap_or_default();
        FileMemoryBackend::parse_entries(&content)
    }
}
