//! Persistent user "Always Allow" list.
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use regex::Regex;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserAllowlistEntry {
    /// Regex pattern. Validated at `add` time; recompiled at `load`
    /// time (silently dropped on compile error so a hand-edited file
    /// with one bad entry doesn't break the rest).
    pub pattern: String,
    /// Example command the user approved (informational).
    #[serde(default)]
    pub command_hint: String,
    /// Unix timestamp (seconds since epoch) when the entry was added.
    #[serde(default)]
    pub added_at_unix: u64,
}

/// Persistent user "Always Allow" list.
/// Holds the regex patterns the user has blessed for the `always`
/// tier of the 4-state approval (`once` / `session` / `always` /
/// `deny`). Loaded from disk at agent startup via [`Self::load`] and
/// re-saved on every modification. A missing or corrupt file yields
/// an empty allowlist — load **must not** fail the agent start, by
/// design.
/// The `compiled` cache is kept in sync with `entries` at all times
/// (skip-serialized so the JSON only stores the source patterns;
/// recompiled lazily on `load`).
#[derive(Debug, Clone, Default)]
pub struct UserAllowlist {
    entries: Vec<UserAllowlistEntry>,
    /// Compiled patterns, parallel to `entries`. Recomputed by
    /// `recompile()` after `load()`. `#[serde(skip)]` would be nice
    /// but `UserAllowlist` itself isn't `Serialize` — only the
    /// `entries` are written to disk.
    compiled: Vec<Regex>,
}

impl UserAllowlist {
    /// Create an empty allowlist. Use [`Self::load`] for the
    /// disk-backed constructor.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a regex pattern. Compiles it; returns an error if invalid
    /// (the entry is **not** added in that case — the caller can
    /// decide to drop, log, or surface to the user).
    /// `command_hint` is informational (e.g. the example command
    /// that triggered the prompt); it shows up in `--allowlist list`
    /// output and is **not** consulted at match time.
    pub fn add(
        &mut self,
        pattern: &str,
        command_hint: &str,
    ) -> Result<(), regex::Error> {
        let re = Regex::new(pattern)?;
        self.entries.push(UserAllowlistEntry {
            pattern: pattern.to_string(),
            command_hint: command_hint.to_string(),
            added_at_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        });
        self.compiled.push(re);
        Ok(())
    }

    /// Convenience: add a literal command (escaped to exact-match
    /// regex `^<literal>$`). This is what the WS "Always Allow" UI
    /// will call.
    pub fn add_literal(&mut self, command: &str) -> Result<(), regex::Error> {
        self.add(&format!("^{}$", regex::escape(command)), command)
    }

    /// Remove by exact pattern match. Returns `true` if an entry was
    /// removed.
    pub fn remove(&mut self, pattern: &str) -> bool {
        if let Some(pos) = self.entries.iter().position(|e| e.pattern == pattern) {
            self.entries.remove(pos);
            self.compiled.remove(pos);
            true
        } else {
            false
        }
    }

    /// Iterate entries (read-only).
    pub fn iter(&self) -> impl Iterator<Item = &UserAllowlistEntry> {
        self.entries.iter()
    }

    /// Number of entries currently held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if no entries are present.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Check if any compiled pattern matches `command`.
    pub fn matches(&self, command: &str) -> bool {
        self.compiled.iter().any(|re| re.is_match(command))
    }

    /// Load from a JSON file. Behavior matrix:
    /// | File state             | Result                              |
    /// |------------------------|-------------------------------------|
    /// | Missing                | `Self::new()` (no warning)          |
    /// | Empty                  | `Self::new()` (no warning)          |
    /// | Valid JSON array       | populated, recompiled               |
    /// | Corrupt JSON           | `Self::new()` + warning to stderr   |
    /// | Permission denied      | `Self::new()` + warning to stderr   |
    /// Load is **never** fatal: a corrupt or unreadable file must
    /// not stop the agent from starting. The user can rebuild the
    /// allowlist by re-clicking "Always Allow" on the relevant
    /// prompts.
    pub fn load(path: &Path) -> Self {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::new(),
            Err(e) => {
                eprintln!(
                    "[hermix-approval] allowlist read error ({}); starting with empty allowlist",
                    e
                );
                return Self::new();
            }
        };
        if bytes.is_empty() {
            return Self::new();
        }
        match serde_json::from_slice::<Vec<UserAllowlistEntry>>(&bytes) {
            Ok(entries) => {
                let mut al = Self {
                    entries,
                    compiled: Vec::new(),
                };
                al.recompile();
                al
            }
            Err(e) => {
                eprintln!(
                    "[hermix-approval] allowlist parse error ({}); starting with empty allowlist",
                    e
                );
                Self::new()
            }
        }
    }

    /// Persist to a JSON file (atomic via tmp + rename). Creates the
    /// parent directory if missing. On Windows, the target file is
    /// removed before rename because `std::fs::rename` fails if the
    /// target exists (POSIX rename atomically replaces).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(&self.entries)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes)?;
        // On Windows, std::fs::rename fails if the target exists.
        // POSIX rename atomically replaces, so the remove is
        // Windows-only.
        #[cfg(windows)]
        {
            if let Err(e) = std::fs::remove_file(path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "failed to remove file before rename (Windows)"
                    );
                }
            }
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Recompile all entry patterns. Silently drops entries that
    /// fail to compile — both from `compiled` (so they don't match)
    /// and from `entries` (so the next `save` doesn't write them
    /// back into the file). Entries were validated at `add` time,
    /// so this only matters for hand-edited files; a corrupt entry
    /// should not break the whole allowlist.
    fn recompile(&mut self) {
        let mut kept_entries = Vec::with_capacity(self.entries.len());
        let mut compiled = Vec::with_capacity(self.entries.len());
        for e in self.entries.drain(..) {
            match Regex::new(&e.pattern) {
                Ok(re) => {
                    compiled.push(re);
                    kept_entries.push(e);
                }
                Err(err) => eprintln!(
                    "[hermix-approval] dropping uncompilable allowlist entry \
                     `{}` ({}): {}",
                    e.pattern, e.command_hint, err
                ),
            }
        }
        self.entries = kept_entries;
        self.compiled = compiled;
    }
}
