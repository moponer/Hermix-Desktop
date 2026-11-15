//! Declarative permission profile (— P6 direction 1).
//!
//! The agent runs directly on the real Windows host with **no container /
//! VM sandbox**, so OS-level `access denied` (EDR blocks, protected
//! directories, sandboxed virtual FS) is a real, frequent failure.
//!
//! Open-source agents (OpenHands, OpenClaw, Hermes, Grok Build) solve this
//! not by asking the host OS for elevation at runtime, but by a
//! **declarative permission profile**: the agent declares, up front, which
//! directories / commands it is allowed to touch. Anything outside that
//! scope triggers an **approval prompt** — the agent *asks the user to
//! grant access*, instead of silently failing or fabricating a generic
//! "here's a cleanup checklist" reply.
//!
//! `PermissionProfile` is that declaration. `decide_pre_tool_dispatch`
//! consults it before dispatching a `file`/`terminal` tool: if the target
//! resource is outside `granted_paths` / `granted_commands`, it returns
//! `ToolDispatchDecision::AwaitApproval` — reusing the exact same WS
//! approval channel that `Dangerous` tools use — so the desktop UI pops a
//! "资源未授权，是否允许访问？" card the user can one-click approve.
//!
//! This is the "主动申请权限审批" behaviour the product was missing.

use serde::{Deserialize, Serialize};
use std::path::{Component, Path};

/// Normalise a path for case-/separator-insensitive prefix matching AND
/// collapse `.` / `..` segments so a granted prefix of `c:/users/alice`
/// cannot be escaped via `c:/users/alice/../../windows/...`.
///
/// Windows paths are case-insensitive and accept both `\` and `/`; Git
/// Bash also feeds `/c/Users/...` style roots. We collapse all of that
/// into a single canonical lower-cased `/`-separated form so a granted
/// prefix like `C:\Users` matches a request for `/c/users/alice/file`,
/// while `..` segments are physically removed so a path-traversal request
/// resolves to its real on-disk parent before the prefix comparison runs.
fn normalize_path(p: &str) -> String {
    let mut s = p.replace('\\', "/");
    // Git Bash style root: `/c/foo` → `c:/foo` (so it matches the
    // `C:\Users` grant form). Only triggers for `/<letter>/...`.
    if s.starts_with('/')
        && s.len() >= 3
        && s.as_bytes()[1].is_ascii_alphabetic()
        && s.as_bytes()[2] == b'/'
    {
        s = format!("{}:/{}", &s[1..2], &s[3..]);
    }
    s = s.to_lowercase();

    // Collapse `.` / `..` segments with a stack so we never compare
    // `c:/users/alice/../foo` against `c:/users/alice` (which would
    // otherwise pass the prefix check and let the agent escape the
    // granted directory root).
    // We canonicalise against the literal string form (no
    // `fs::canonicalize`) because the agent must be able to ask about
    // paths that don't yet exist on disk — creating `c:/new/proj/...`
    // should not require the directory to exist first.
    let path = Path::new(&s);
    let mut prefix: Option<String> = None;
    let mut unc_root: Option<String> = None;
    let mut rooted = false;
    let mut stack: Vec<String> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(p) => {
                let raw = p.as_os_str().to_string_lossy().to_lowercase();
                // UNC paths (`\\server\share\...`) get reported by
                // Rust as a single `Prefix` whose `raw` already
                // contains the `\\server\share` part. Store that as
                // the root verbatim and skip the trailing `RootDir`
                // we get on Windows — otherwise we'd emit
                // `server/share:/file.txt`.
                if raw.starts_with(r"\\") || raw.starts_with("//") {
                    let cleaned = raw
                        .trim_start_matches(r"\\")
                        .trim_start_matches("//");
                    unc_root = Some(cleaned.to_string());
                    rooted = true;
                } else {
                    // Windows drive prefix looks like `c:` — keep it
                    // without the trailing colon because we re-add
                    // `/` below when rooted.
                    prefix = Some(raw.trim_end_matches(':').to_string());
                }
            }
            Component::RootDir => {
                if unc_root.is_some() {
                    // Already captured in `unc_root`; skip the
                    // standalone RootDir Rust inserts after the UNC
                    // prefix so we don't double the slashes.
                } else {
                    stack.clear();
                    rooted = true;
                }
            }
            Component::CurDir => {
                // `.` — drop
            }
            Component::ParentDir => {
                if let Some(top) = stack.last() {
                    if top == ".." {
                        // We're already escaping relative-style; keep the `..`.
                        stack.push("..".to_string());
                    } else {
                        stack.pop();
                    }
                } else if !rooted && prefix.is_none() && unc_root.is_none() {
                    // Relative path that escapes — preserve `..` so the
                    // caller sees the unresolved form. Should never
                    // bypass a granted absolute prefix.
                    stack.push("..".to_string());
                } else if rooted || prefix.is_some() {
                    // At the drive / root — `..` cannot go above; drop it.
                } else {
                    stack.push("..".to_string());
                }
            }
            Component::Normal(seg) => {
                stack.push(seg.to_string_lossy().to_lowercase());
            }
        }
    }

    let mut out = String::new();
    if let Some(ref p) = prefix {
        out.push_str(p);
        if rooted {
            out.push(':');
        }
    }
    if let Some(ref unc) = unc_root {
        out.push_str(unc);
    }
    if rooted {
        out.push('/');
    }
    out.push_str(&stack.join("/"));
    out
}

/// Normalise a command for prefix matching (lowercase, trimmed).
fn normalize_command(c: &str) -> String {
    c.trim().to_lowercase()
}

/// Strip trailing `/` from a normalised path while preserving the
/// drive-root form `c:/`. Without this, a granted `c:/users` wouldn't
/// match a request for `c:/users` (one has the slash, the other
/// doesn't) and `c:/` would lose its root form.
fn trim_trailing(mut s: String) -> String {
    if s.len() > 3 && s.ends_with('/') {
        // `c:/` (3 chars) is preserved as the drive root.
        s.pop();
    }
    s
}

/// Scan `args` (the portion of a command **after** a granted prefix)
/// for shell chaining operators, ignoring anything inside `"..."` or
/// `'...'` pairs. Catches `;`, `&&`, `||`, lone `|`, `` ` ``,
/// `$(...)`, and `\n` / `\r` line breaks. A `false` result means the
/// post-prefix payload is a benign argument list.
fn contains_shell_chain_operator(args: &str) -> bool {
    let bytes = args.as_bytes();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                } else if b == b'\\' && i + 1 < bytes.len() {
                    // Skip escaped char inside double-quotes only;
                    // single quotes are literal in POSIX shell.
                    if q == b'"' {
                        i += 1;
                    }
                }
            }
            None => {
                match b {
                    b'"' | b'\'' => quote = Some(b),
                    b';' | b'\n' | b'\r' | b'`' => return true,
                    b'&' if bytes.get(i + 1) == Some(&b'&') => return true,
                    b'|' if bytes.get(i + 1) == Some(&b'|') => return true,
                    b'|' if bytes.get(i + 1) != Some(&b'|') => return true,
                    b'$' if bytes.get(i + 1) == Some(&b'(') => return true,
                    _ => {}
                }
            }
        }
        i += 1;
    }
    false
}

/// Declarative set of resources the agent is allowed to touch without
/// prompting. Empty fields mean "nothing pre-granted" — every file/terminal
/// access outside an explicit grant goes through the approval prompt, which
/// is the safe default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionProfile {
    /// Directory roots the agent may read/write without prompting.
    /// Matching is case-insensitive, separator-insensitive prefix.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub granted_paths: Vec<String>,

    /// Command prefixes (e.g. `git`, `npm`, `python`) the agent may run
    /// without prompting. Empty = every command is subject to approval.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub granted_commands: Vec<String>,
}

impl PermissionProfile {
    /// True if `path` lives under one of the granted directory roots.
    /// Both `path` and the granted prefix are normalised through
    /// [`normalize_path`] (which collapses `.`/`..`), so a granted
    /// `c:/users/alice` cannot be bypassed via
    /// `c:/users/alice/../../windows/...` — that request canonicalises
    /// to `c:/windows` and falls outside the prefix.
    pub fn allows_path(&self, path: &str) -> bool {
        let norm = trim_trailing(normalize_path(path));
        self.granted_paths.iter().any(|g| {
            let prefix = normalize_path(g);
            let prefix = trim_trailing(prefix);
            norm == prefix || norm.starts_with(&format!("{prefix}/"))
        })
    }

    /// True if `command` begins with one of the granted command prefixes.
    /// A prefix only matches at a **word boundary** — so `git` matches
    /// `git status` but not `gitx init`. After the prefix matches we
    /// also reject any command that would let the agent chain
    /// additional shell actions past the granted prefix (`;`, `&&`,
    /// `||`, lone `|`, command substitution `` ` `` / `$(...)`, or
    /// newline). Quoted forms (`"…;…"`) are tolerated by the
    /// tokenizer-style scanner, so legitimate args like
    /// `git log --grep="foo && bar"` are still allowed.
    pub fn allows_command(&self, command: &str) -> bool {
        let norm = normalize_command(command);
        self.granted_commands.iter().any(|g| {
            let prefix = normalize_command(g);
            let hits_prefix = norm == prefix
                || (norm.starts_with(&prefix)
                    && !norm[prefix.len()..]
                        .chars()
                        .next()
                        .map(|c| c.is_alphanumeric())
                        .unwrap_or(false));
            if !hits_prefix {
                return false;
            }
            // Past the prefix we only allow arguments — no shell
            // chaining. Quoted segments are exempt from the operator
            // scan because users routinely write args containing `;`,
            // `|`, etc.
            let after = &norm[prefix.len()..];
            !contains_shell_chain_operator(after)
        })
    }

    /// Add `path` to the granted set (idempotent).
    pub fn grant_path(&mut self, path: &str) {
        let norm = normalize_path(path);
        if !self.granted_paths.iter().any(|p| normalize_path(p) == norm) {
            self.granted_paths.push(norm);
        }
    }

    /// Add `command` prefix to the granted set (idempotent).
    pub fn grant_command(&mut self, command: &str) {
        let norm = normalize_command(command);
        if !self.granted_commands.iter().any(|c| normalize_command(c) == norm) {
            self.granted_commands.push(norm);
        }
    }

    /// True if `granted_paths` / `granted_commands` are non-empty
    /// (i.e. the user has already declared any pre-granted scope).
    pub fn has_any_grant(&self) -> bool {
        !self.granted_paths.is_empty() || !self.granted_commands.is_empty()
    }
}

/// A single resource a user has just approved. Captured at approval
/// request time and consumed when the approval resolves to `Approved`,
/// so the agent doesn't have to re-parse the tool arguments.
///
/// Path is stored as the **directory root** the user is granting
/// (typically the first 1-2 path components) so future access to any
/// file under that root is auto-allowed. Command is stored as the
/// first whitespace-separated token (e.g. `git`, `cargo`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingGrant {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

impl PendingGrant {
    /// Extract a sensible grant from a tool call's parsed arguments.
    /// For file tools we strip the file name and keep only the leading
    /// directory components — granting access to
    /// `C:\Users\alice\file.txt` should not implicitly grant
    /// `C:\Windows\System32`, but should grant everything under
    /// `C:\Users\alice`. Same rationale for Git Bash roots.
    /// For terminal/code_execution we keep the first whitespace
    /// token as the command prefix.
    /// Bulk-path tools (`disk_cleanup`, etc.) accept a `paths` array
    /// instead of a single `path`. We grant the directory root of the
    /// **first** non-empty entry — refusing to grant the whole array
    /// would be too narrow (the user clearly approved cleaning those
    /// paths) but granting a path we never saw would be unsafe.
    pub fn from_tool_call(tool_name: &str, params: &serde_json::Value) -> Self {
        let _ = tool_name;
        if let Some(path) = params.get("path").and_then(|v| v.as_str()) {
            if !path.is_empty() {
                return Self {
                    path: Some(extract_path_root(path)),
                    command: None,
                };
            }
        }
        if let Some(arr) = params.get("paths").and_then(|v| v.as_array()) {
            if let Some(first) = arr.iter().find_map(|v| v.as_str()) {
                if !first.is_empty() {
                    return Self {
                        path: Some(extract_path_root(first)),
                        command: None,
                    };
                }
            }
        }
        if let Some(cmd) = params.get("command").and_then(|v| v.as_str()) {
            let prefix = cmd.split_whitespace().next().unwrap_or(cmd).to_string();
            return Self {
                path: None,
                command: Some(prefix),
            };
        }
        Self::default()
    }

    /// Apply this grant to `profile`. Idempotent.
    pub fn apply_to(&self, profile: &mut PermissionProfile) {
        if let Some(p) = &self.path {
            profile.grant_path(p);
        }
        if let Some(c) = &self.command {
            profile.grant_command(c);
        }
    }
}

/// Strip the file name from `path` and keep up to 2 leading directory
/// components. Normalises Git Bash roots (`/c/foo` → `C:/foo`) first
/// so the result is in canonical Windows form.
///
/// Rule: drop any trailing segment that looks like a file (contains a
/// dot not at position 0), then take the first 2 remaining segments.
/// If nothing remains, fall back to the leading segment.
///
/// The result is then run through [`normalize_path`] so it carries
/// the same `.`/`..` collapse as the prefix matcher — that way
/// `extract_path_root` and `allows_path` cannot drift apart.
fn extract_path_root(path: &str) -> String {
    // 1. Apply the same case-/separator-/Git-Bash normalisation
    // `allows_path` uses, so the trailing segment we look at is
    // in the canonical form the matcher expects.
    let normalised = normalize_path(path);
    if normalised.is_empty() {
        return path.to_string();
    }

    // 2. Pull the leading components as `[prefix?, root?, seg..., seg]`
    // using std::path::components so we get the right Windows
    // drive handling for free.
    let comps: Vec<Component> = Path::new(&normalised).components().collect();
    let mut drive: Option<String> = None;
    let mut unc_root: Option<String> = None;
    let mut rooted = false;
    let mut segs: Vec<String> = Vec::new();
    for c in comps {
        match c {
            Component::Prefix(p) => {
                let raw = p.as_os_str().to_string_lossy().to_lowercase();
                // UNC paths (`\\server\share\...`) get reported by
                // Rust as a single `Prefix` whose `raw` already
                // contains the `\\server\share` part. Store that as
                // the root verbatim so the canonical prefix matches
                // a fresh request via `\\server\share\file.txt` the
                // same way (after `replace('\\', '/')` in
                // `normalize_path`).
                if raw.starts_with(r"\\") || raw.starts_with("//") {
                    let cleaned = raw
                        .trim_start_matches(r"\\")
                        .trim_start_matches("//");
                    unc_root = Some(cleaned.to_string());
                    rooted = true;
                } else {
                    drive = Some(raw.trim_end_matches(':').to_string());
                }
            }
            Component::RootDir => {
                if unc_root.is_some() {
                    // Rust inserts a `RootDir` after the UNC prefix;
                    // skip it so we don't double the slashes.
                } else {
                    rooted = true;
                }
            }
            Component::Normal(s) => segs.push(s.to_string_lossy().to_string()),
            _ => {}
        }
    }

    // 3. Drop trailing file-like segments so we keep directories only.
    while let Some(last) = segs.last() {
        if last.contains('.') && !last.starts_with('.') {
            segs.pop();
        } else {
            break;
        }
    }

    // 4. Assemble up to 2 leading directory segments. UNC paths
    // keep the `\\server\share` part intact as the root, then
    // append one additional child segment for symmetry with the
    // drive case (so the share name + one directory below it
    // becomes the granted root).
    if let Some(unc) = unc_root {
        let take = segs.len().min(1);
        let extra = if segs.is_empty() {
            String::new()
        } else {
            format!("/{}", segs[..take].join("/"))
        };
        return format!("{}{}", unc, extra);
    }
    if segs.is_empty() {
        return match drive {
            Some(d) => {
                if rooted {
                    format!("{}:/", d)
                } else {
                    d
                }
            }
            None => normalised,
        };
    }
    let take = segs.len().min(2);
    let dir_part = segs[..take].join("/");
    match drive {
        Some(d) if rooted => format!("{}:/{}", d, dir_part),
        Some(d) => format!("{}:/{}", d, dir_part),
        None => dir_part,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_profile_allows_nothing() {
        let p = PermissionProfile::default();
        assert!(!p.allows_path("C:\\Users\\alice\\file.txt"));
        assert!(!p.allows_command("git status"));
    }

    #[test]
    fn path_prefix_is_case_and_separator_insensitive() {
        let mut p = PermissionProfile::default();
        p.grant_path("C:\\Users");
        assert!(p.allows_path("C:\\Users\\alice\\file.txt"));
        // Git Bash style root
        assert!(p.allows_path("/c/users/alice/file.txt"));
        // sibling that only shares a prefix string must NOT match
        assert!(!p.allows_path("C:\\Users2\\file.txt"));
        // exact root match allowed
        assert!(p.allows_path("C:\\Users"));
    }

    #[test]
    fn command_prefix_matches_at_word_boundary_only() {
        let mut p = PermissionProfile::default();
        p.grant_command("git");
        assert!(p.allows_command("git status"));
        assert!(p.allows_command("  GIT pull"));
        assert!(!p.allows_command("gitx init"));
        assert!(!p.allows_command("gitx"));
    }

    #[test]
    fn grant_is_idempotent() {
        let mut p = PermissionProfile::default();
        p.grant_path("C:\\Users");
        p.grant_path("c:/users"); // same after normalization
        assert_eq!(p.granted_paths.len(), 1);
        p.grant_command("git");
        p.grant_command("GIT");
        assert_eq!(p.granted_commands.len(), 1);
    }

    #[test]
    fn has_any_grant_detects_state() {
        let mut p = PermissionProfile::default();
        assert!(!p.has_any_grant());
        p.grant_path("C:\\Users");
        assert!(p.has_any_grant());
    }

    #[test]
    fn pending_grant_strips_file_from_path() {
        let v = serde_json::json!({ "path": "C:\\Users\\alice\\file.txt" });
        let g = PendingGrant::from_tool_call("read_file", &v);
        assert_eq!(g.path.as_deref(), Some("c:/users/alice"));
        assert!(g.command.is_none());
    }

    #[test]
    fn pending_grant_strips_command_to_first_token() {
        let v = serde_json::json!({ "command": "git status" });
        let g = PendingGrant::from_tool_call("terminal", &v);
        assert!(g.path.is_none());
        assert_eq!(g.command.as_deref(), Some("git"));
    }

    #[test]
    fn pending_grant_apply_to_grants_profile() {
        let v = serde_json::json!({ "path": "/c/Users/alice/x.txt" });
        let g = PendingGrant::from_tool_call("read_file", &v);
        let mut p = PermissionProfile::default();
        g.apply_to(&mut p);
        assert!(p.allows_path("/c/users/alice/other.txt"));
        assert!(!p.allows_path("/c/Windows/x.txt"));
    }

    #[test]
    fn extract_path_root_handles_drive_only() {
        assert_eq!(extract_path_root("C:\\file.txt"), "c:/");
        assert_eq!(extract_path_root("c:/file.txt"), "c:/");
    }

    #[test]
    fn extract_path_root_handles_unc() {
        assert_eq!(
            extract_path_root("\\\\server\\share\\file.txt"),
            "server/share"
        );
    }

    #[test]
    fn path_traversal_cannot_escape_granted_prefix() {
        let mut p = PermissionProfile::default();
        p.grant_path("c:/users/alice");
        // Direct child: allowed
        assert!(p.allows_path("c:/users/alice/file.txt"));
        // Sibling: blocked
        assert!(!p.allows_path("c:/users/bob/file.txt"));
        // **Traversal attempt — must be blocked.** A naive prefix
        // match would let this through because the literal string
        // starts with `c:/users/alice/`, but normalisation collapses
        // the `..` so the resolved path is `c:/windows`.
        assert!(
            !p.allows_path("c:/users/alice/../windows/system32/cmd.exe"),
            "path-traversal must not bypass the granted prefix"
        );
        // Triple-step traversal — same.
        assert!(!p.allows_path("c:/users/alice/a/b/../../../foo"));
        // Self-references must not change the answer.
        assert!(p.allows_path("c:/users/alice/./sub/file"));
    }

    #[test]
    fn command_injection_cannot_bypass_granted_prefix() {
        let mut p = PermissionProfile::default();
        p.grant_command("git");
        // Benign: allowed
        assert!(p.allows_command("git status"));
        assert!(p.allows_command("git log --grep=\"foo && bar\""));
        // Quoted `;` inside args: still allowed (user-style escaping).
        assert!(p.allows_command("git log --grep=\"v1.0; hotfix\""));
        // **Shell-chain attempts — must be blocked.** A naive prefix
        // match accepts these because the next character after `git`
        // is whitespace; the operator scan in the rest of the args
        // catches the chaining intent.
        assert!(!p.allows_command("git status; rm -rf /"));
        assert!(!p.allows_command("git status && rm -rf /"));
        assert!(!p.allows_command("git status || rm -rf /"));
        assert!(!p.allows_command("git log $(rm -rf /)"));
        assert!(!p.allows_command("git log `rm -rf /`"));
        assert!(!p.allows_command("git log\nrm -rf /"));
    }

    #[test]
    fn pending_grant_accepts_paths_array() {
        // Bulk-path tools (disk_cleanup) use `paths: [String]` rather
        // than a single `path` field. We must still capture the
        // directory root of the first non-empty entry so the user
        // gets a meaningful grant instead of nothing.
        let v = serde_json::json!({
            "paths": ["C:\\Users\\alice\\AppData\\Local\\Temp\\a.tmp", "C:\\foo"]
        });
        let g = PendingGrant::from_tool_call("disk_cleanup", &v);
        // `extract_path_root` keeps the first 2 directory segments —
        // `users/alice` here (after `AppData\Local\Temp\a.tmp`'s
        // file-like `a.tmp` is dropped, the dir count is 5 and we
        // take min(5, 2) = 2).
        assert_eq!(g.path.as_deref(), Some("c:/users/alice"));
        assert!(g.command.is_none());
        // Empty array → no grant.
        let v = serde_json::json!({ "paths": [] });
        let g = PendingGrant::from_tool_call("disk_cleanup", &v);
        assert!(g.path.is_none());
        // Single `path` wins over an empty `paths`.
        let v = serde_json::json!({
            "path": "C:\\Users\\alice\\file.txt",
            "paths": []
        });
        let g = PendingGrant::from_tool_call("read_file", &v);
        assert_eq!(g.path.as_deref(), Some("c:/users/alice"));
    }
}
