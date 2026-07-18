//! Path security helpers for tool implementations.
//!
//! Three concerns that every tool that touches the filesystem shares:
//!
//! 1. **Win32 long-path prefix stripping.** Windows accepts
//!    `\\?\C:\foo\bar` as a way to opt out of `MAX_PATH` (260 char)
//!    limits. Most user-facing programs (cmd.exe, FINDSTR, etc.) can't
//!    handle that form, so when the LLM hands us a `workdir` or `path`
//!    that includes the prefix we transparently strip it back to the
//!    standard form. Mirrors the explicit comment in
//!    `hermix-environments::local::LocalBackend::execute_command`.
//!
//! 2. **Traversal-component detection.** Cheap substring/`Path::parts`
//!    check that catches obvious `..` attempts before doing the
//!    (relatively expensive) `canonicalize() + relative_to()` round
//!    trip.
//!
//! 3. **Boundary enforcement (`validate_within_dir`).** The
//!    "is `path` inside `root`?" question, normalized through
//!    `std::fs::canonicalize` (which on Windows resolves `\\?\` and
//!    follows symlinks). Returns `Some(error_message)` on failure,
//!    `None` on success — matches the convention used by every tool
//!    handler (`ToolError::ExecutionFailed(msg)` at the call site).
//!
//! These helpers do **not** try to be a full path-policy layer; that
//! lives in `hermix-tools::approval::ApprovalManager` and Tirith.
//! The job here is the boring "is this string even a well-formed
//! path that points somewhere we want to touch?" check that every
//! tool would otherwise re-implement differently.

use std::path::{Path, PathBuf};

/// The Windows long-path namespace prefix, in both its raw and
/// post-shell-escape forms.
///
/// - `\\?\C:\foo` — the canonical form
/// - `\\?\UNC\server\share\foo` — UNC variant
/// - `\\.\COM1` — device namespace (also blocked separately)
pub const WIN32_LONG_PATH_PREFIX: &str = r"\\?\";
pub const WIN32_LONG_PATH_UNC_PREFIX: &str = r"\\?\UNC\";
pub const WIN32_DEVICE_PREFIX: &str = r"\\.\";

/// Strip the `\\?\` (or `\\?\UNC\`) Win32 long-path prefix from a path
/// string. Returns the input unchanged if the prefix is not present.
///
/// This is **not** a security check — it normalizes forms that the
/// Win32 API accepts but most user-facing programs reject. Use
/// [`forbidden_win32_prefix`] to reject the prefix outright.
pub fn strip_win32_long_path(input: &str) -> String {
    if let Some(rest) = input.strip_prefix(WIN32_LONG_PATH_UNC_PREFIX) {
        // \\?\UNC\server\share → \\server\share
        format!(r"\\{}", rest)
    } else if let Some(rest) = input.strip_prefix(WIN32_LONG_PATH_PREFIX) {
        rest.to_string()
    } else {
        input.to_string()
    }
}

/// Strip the `\\?\` (or `\\.\`) Win32 prefix from a `Path`-like value.
/// Returns a fresh `PathBuf` in standard form.
pub fn strip_win32_long_path_buf(input: &Path) -> PathBuf {
    let s = input.to_string_lossy();
    PathBuf::from(strip_win32_long_path(&s))
}

/// Return the offending Win32 prefix if `s` contains a forbidden
/// `\\?\` or `\\.\` namespace marker. Used by the terminal handler
/// to reject LLM-fabricated paths like `ls \\?\D:\data` that have no
/// business appearing in a user-facing command.
pub fn forbidden_win32_prefix(s: &str) -> Option<&'static str> {
    if s.contains(WIN32_LONG_PATH_UNC_PREFIX) || s.contains(r"\?\UNC\") {
        Some(WIN32_LONG_PATH_UNC_PREFIX)
    } else if s.contains(WIN32_LONG_PATH_PREFIX) || s.contains(r"\?\") {
        Some(WIN32_LONG_PATH_PREFIX)
    } else if s.contains(WIN32_DEVICE_PREFIX) || s.contains(r"\.\") {
        Some(WIN32_DEVICE_PREFIX)
    } else {
        None
    }
}

/// Detect shell metacharacters that could be used for command chaining
/// or injection. Returns `Some(description)` if the command contains
/// potentially dangerous shell operators.
///
/// This is a defense-in-depth layer — the approval system catches known
/// dangerous patterns, but this catches generic shell injection vectors
/// that might not be in the pattern list.
pub fn detect_shell_metacharacters(command: &str) -> Option<&'static str> {
    // Command chaining operators
    if command.contains("&&") {
        return Some("command chaining via &&");
    }
    if command.contains("||") {
        return Some("command chaining via ||");
    }
    // Command separator (not inside quotes — simplified check)
    if command.contains(';') && !command.contains("'\";'\"'") {
        return Some("command separator via ;");
    }
    // Pipe to shell (already caught by approval patterns, but defense-in-depth)
    if command.contains("|") {
        // Check for pipe-to-shell patterns like `... | sh` or `... | bash`
        let lower = command.to_lowercase();
        if lower.contains("| sh") || lower.contains("| bash")
            || lower.contains("| powershell") || lower.contains("| cmd")
            || lower.contains("| /bin/") {
            return Some("pipe to shell interpreter");
        }
    }
    // Backtick command substitution
    if command.contains('`') {
        return Some("command substitution via backticks");
    }
    // $() command substitution (simplified — doesn't handle escaped parens)
    if command.contains("$(") {
        return Some("command substitution via $()");
    }
    // Process substitution (bash-specific)
    if command.contains("<(") || command.contains(">(") {
        return Some("process substitution");
    }
    None
}

/// Cheap `..` component check. Returns true if any path component
/// is exactly `..` (i.e. the path tries to escape its parent).
///
/// This is intentionally *not* a full traversal detector — a path
/// like `foo/../bar` will have `..` as a part only after lexing; the
/// function name says "component" for a reason. For full
/// `resolve() + relative_to()` enforcement, see
/// [`validate_within_dir`].
pub fn has_traversal_component(path_str: &str) -> bool {
    Path::new(path_str).components().any(|c| {
        matches!(c, std::path::Component::ParentDir)
    })
}

/// Ensure `path` resolves to a location within `root`.
///
/// Returns `None` on success, `Some(error_message)` on failure.
/// Uses `std::fs::canonicalize` to follow symlinks and normalize
/// `..` components. On Windows, `canonicalize` returns the `\\?\`
/// long-path form, which is fine for the relative-to comparison
/// (both sides will have the same prefix if they're in the same
/// tree) but is wrapped in `Some(_)` errors if a path doesn't
/// exist.
pub fn validate_within_dir(path: &Path, root: &Path) -> Option<String> {
    let resolved = match std::fs::canonicalize(path) {
        Ok(p) => p,
        Err(e) => {
            return Some(format!(
                "Path '{}' could not be resolved: {}",
                path.display(),
                e
            ))
        }
    };
    let root_resolved = match std::fs::canonicalize(root) {
        Ok(p) => p,
        Err(e) => {
            return Some(format!(
                "Root '{}' could not be resolved: {}",
                root.display(),
                e
            ))
        }
    };
    match resolved.strip_prefix(&root_resolved) {
        Ok(_) => None,
        Err(e) => Some(format!(
            "Path '{}' is outside allowed root '{}': {}",
            resolved.display(),
            root_resolved.display(),
            e
        )),
    }
}

/// Validate that `path` (which may be absolute or relative to
/// `root`) is inside `root`. Unlike [`validate_within_dir`], this
/// does not require `path` to exist on disk — useful for
/// pre-write checks where the destination file doesn't exist yet.
///
/// The relative path is computed lexically (not via `canonicalize`)
/// so the check is purely a string/boundary guard.
pub fn path_within_root_lexical(path: &Path, root: &Path) -> Option<String> {
    // If `path` is absolute, treat it as-is; otherwise join with root.
    let candidate: PathBuf = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    // Try to strip the root prefix lexically. `Path::starts_with` is
    // byte-based and platform-aware enough for this purpose.
    if candidate.starts_with(root) {
        None
    } else {
        Some(format!(
            "Path '{}' is outside allowed root '{}'",
            candidate.display(),
            root.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn strip_long_path() {
        assert_eq!(strip_win32_long_path(r"\\?\C:\foo\bar"), r"C:\foo\bar");
        assert_eq!(
            strip_win32_long_path(r"\\?\UNC\server\share\foo"),
            r"\\server\share\foo"
        );
        // Already standard — unchanged.
        assert_eq!(strip_win32_long_path(r"C:\foo\bar"), r"C:\foo\bar");
        // Already standard UNC.
        assert_eq!(
            strip_win32_long_path(r"\\server\share\foo"),
            r"\\server\share\foo"
        );
    }

    #[test]
    fn forbidden_prefix_detects_all_variants() {
        assert_eq!(forbidden_win32_prefix(r"\\?\D:\data"), Some(WIN32_LONG_PATH_PREFIX));
        assert_eq!(
            forbidden_win32_prefix(r"\\?\UNC\evil\share"),
            Some(WIN32_LONG_PATH_UNC_PREFIX)
        );
        assert_eq!(forbidden_win32_prefix(r"\\.\COM1"), Some(WIN32_DEVICE_PREFIX));
        assert_eq!(forbidden_win32_prefix(r"ls -la"), None);
    }

    #[test]
    fn traversal_component_detected() {
        assert!(has_traversal_component("../etc/passwd"));
        assert!(has_traversal_component("foo/../bar"));
        assert!(has_traversal_component("foo/.."));
        // No traversal
        assert!(!has_traversal_component("foo/bar"));
        assert!(!has_traversal_component("foo/.bar"));
    }

    #[test]
    fn validate_within_dir_accepts_in_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let inner = tmp.path().join("subdir");
        std::fs::create_dir_all(&inner).unwrap();
        let file = inner.join("file.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(validate_within_dir(&file, tmp.path()).is_none());
    }

    #[test]
    fn validate_within_dir_rejects_outside() {
        let tmp_a = tempfile::tempdir().unwrap();
        let tmp_b = tempfile::tempdir().unwrap();
        let file_b = tmp_b.path().join("file.txt");
        std::fs::write(&file_b, "x").unwrap();
        let err = validate_within_dir(&file_b, tmp_a.path());
        assert!(err.is_some(), "should reject path outside root");
    }

    #[test]
    fn lexical_within_root_works_for_nonexistent() {
        let root = PathBuf::from("/tmp/imaginary");
        let inner = root.join("nested/file.txt");
        assert!(path_within_root_lexical(&inner, &root).is_none());
        let outside = PathBuf::from("/etc/passwd");
        assert!(path_within_root_lexical(&outside, &root).is_some());
    }
}
