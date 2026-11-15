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
    // Defense-in-depth for shell *injection* vectors only.
    // We intentionally do NOT flag ordinary shell syntax such as `&&`,
    // `||`, `;`, backticks, `$(...)`, or `<(...)`/`>(...)` — those are
    // everyday command construction (e.g. `npm run build && npm test`,
    // `echo "today is $(date)"`) and confirming on every one would fire
    // on almost every real command, destroying terminal UX.
    //
    // Piping a STRING into a shell interpreter is only a genuine
    // "data becomes code" vector when the source is REMOTE (a
    // `curl`/`wget` download) — that is the classic install/poison
    // supply-chain footgun, and it stays gated (see `fetch-run-script`
    // in the approval engine). A LOCAL script piped into a shell
    // interpreter (`./build.sh | sh`, `python -c "..."`, `echo "x" | cmd`,
    // `powershell -enc <base64>`) is routine automation work, NOT an
    // injection vector: the data is local and already known to the user.
    // Gating those forced constant false-positive prompts on ordinary
    // scripted work (the user's 2026-09-26 ruling: "命令执行（设置
    // 环境变量或命令编码的）不用审批，只有删除等高危才审批").
    // Catastrophic *commands* (rm -rf, mkfs, del /s /q …) are still
    // caught independently by the approval pattern list, so nothing is
    // lost by narrowing here.
    //
    // So: flag only when a URL (http:// or https://, or a remote
    // fetch command `curl`/`wget`) is piped into a shell interpreter.
    let lower = command.to_lowercase();
    let has_remote_source = lower.contains("http://")
        || lower.contains("https://")
        || lower.contains("curl ")
        || lower.contains("wget ");
    let pipes_to_interpreter = lower.contains("| sh")
        || lower.contains("| bash")
        || lower.contains("| zsh")
        || lower.contains("| fish")
        || lower.contains("| powershell")
        || lower.contains("| cmd")
        || lower.contains("| /bin/");
    if has_remote_source && pipes_to_interpreter {
        return Some("pipe remote script to shell interpreter");
    }
    None
}

// ---------------------------------------------------------------------------
// 权限治理文件名（跨 crate 单一真相源）
// ---------------------------------------------------------------------------

/// 决定「下一次工具调用能不能不弹卡就执行」的文件名。
///
/// 出现在任意路径都必须硬拒写：一个 `write_file`、一次
/// `echo ... > <home>\approval_allowlist.json`，就把「用户批准过 cargo build
/// 一次」变成「以后所有命令都免审批」。工具侧
/// （`hermix_tools::permission_guard`）与工作台网关
/// （`hermix_plugin::workbench_gateway`）必须共用这一份名单 —— 两侧各写一份时，
/// 能力弱的那一侧就是提权入口。
pub const HERMIX_PERMISSION_CONFIG_NAMES: [&str; 3] = [
    "approval_allowlist.json",
    "session_approvals.json",
    "enabled_toolsets.json",
];

/// 文件名（不含目录）是否为权限治理文件。
///
/// 只判名字、不判位置：这些名字是 Hermix 专属的，出现在任何目录都不是用户的
/// 普通项目文件（与 `config.yaml` 不同 —— 它只在 app home 下才是治理文件，
/// 判定留在 `hermix_tools::permission_guard`，那里能拿到 home 路径）。
pub fn is_permission_config_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    HERMIX_PERMISSION_CONFIG_NAMES
        .iter()
        .any(|n| *n == lower.as_str())
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
