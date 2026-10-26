//! Write protection for files that decide what the agent may do.
//!
//! `approval_allowlist.json` and `session_approvals.json` answer one question:
//! "may this command run without asking the user?". A tool that can write them
//! is therefore a privilege-escalation path — one `write_file`, or one
//! `echo ... > <home>\approval_allowlist.json`, converts "the user approved
//! `cargo build` once" into "every future command is pre-approved", and the
//! next turn's `rm -rf` never reaches a human.
//!
//! `config.yaml` and `enabled_toolsets.json` are included for the same reason
//! in weaker form: they steer provider endpoints, approval settings and which
//! tools exist at all.
//!
//! ## Hard deny, not an approval prompt
//!
//! An approval card is the wrong control here. The model asking for it is the
//! party that benefits, and the card reads like any other legitimate request —
//! the user cannot tell a real reconfiguration from a self-grant. Refusing
//! outright costs nothing (the user can edit the file themselves) and removes
//! the decision from the model's reach.
//!
//! ## What this is not
//!
//! It is not a sandbox. The shell screen is a substring check on the command
//! text, so obfuscation (variable assembly, an encoded payload, a script file
//! written elsewhere) can get around it. It closes the obvious shapes, the
//! same way `DENIED_PATTERNS` does; the real boundary remains the ACL sandbox.

use std::path::Path;

use hermix_core::ToolError;

/// Names specific to Hermix — protected wherever they appear.
///
/// Single source of truth lives in `hermix_core::path_security` so the workbench
/// gateway (which cannot depend on this crate — the dependency runs the other
/// way) enforces the same list instead of drifting from it.
use hermix_core::path_security::HERMIX_PERMISSION_CONFIG_NAMES as HERMIX_CONFIG_NAMES;

/// Generic name: only protected inside the app's own home directory, because
/// `config.yaml` is a normal file in thousands of unrelated projects.
const GENERIC_CONFIG_NAME: &str = "config.yaml";

/// Harness components the agent **is** allowed to edit, but every overwrite is
/// snapshotted first.
///
/// ## Why editable rather than denied
///
/// Harness² (google-research) defines a harness as the model-external system —
/// prompt, guardrails, memory, tool guidance, subagents, skills, scripts,
/// plugins — and its whole premise is that an optimiser **edits** those
/// components while model weights stay fixed. Hard-denying them (the treatment
/// `approval_allowlist.json` needs) would make the framework's core loop
/// impossible.
///
/// The risk that made a deny tempting is real but different in kind: rewriting
/// `SOUL.md` does not grant new permissions, it only changes behaviour — and
/// behaviour changes are visible on the next turn. What is missing is not a
/// wall but a **way back**. So: allow the write, keep a snapshot.
const HARNESS_ROOT_FILES: &[&str] = &["SOUL.md", "MEMORY.md", "USER.md"];

/// Directories (relative to the app home) whose files are harness components.
const HARNESS_SUBDIRS: &[&str] = &[
    "personalities",
    "agents",
    "guidance",
    "scripts",
    "skills",
    "memories",
];

/// How many snapshots per file to keep before the oldest are pruned.
const HARNESS_SNAPSHOT_KEEP: usize = 40;

fn normalise(path: &Path) -> String {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    abs.to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase()
}

fn hermix_home_prefix() -> String {
    let home = normalise(&hermix_config::paths::hermix_home());
    format!("{}\\", home.trim_end_matches('\\'))
}

fn is_under_hermix_home(path: &Path) -> bool {
    normalise(path).starts_with(&hermix_home_prefix())
}

/// `true` when writing `path` would change what the agent is allowed to do.
pub fn is_permission_config_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    if HERMIX_CONFIG_NAMES.contains(&name.as_str()) {
        return true;
    }
    name == GENERIC_CONFIG_NAME && is_under_hermix_home(path)
}

/// Reject a write to a permission-governing file.
pub fn check_write(path: &Path) -> Result<(), ToolError> {
    if is_permission_config_path(path) {
        return Err(ToolError::ExecutionFailed(format!(
            "Write denied: '{}' 决定 agent 的权限与全局配置，不允许由工具改写。\
             如果确实需要修改，请让用户自己编辑该文件（这是防止 agent 自我提权的硬约束）。",
            path.display()
        )));
    }
    Ok(())
}

/// `true` when `path` is a harness component (see [`HARNESS_ROOT_FILES`] /
/// [`HARNESS_SUBDIRS`]) living under the app home.
pub fn is_harness_component(path: &Path) -> bool {
    if !is_under_hermix_home(path) {
        return false;
    }
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if HARNESS_ROOT_FILES
        .iter()
        .any(|n| n.eq_ignore_ascii_case(name))
    {
        return true;
    }
    let rel = relative_to_home(path);
    let first = rel.split(['\\', '/']).next().unwrap_or("");
    HARNESS_SUBDIRS
        .iter()
        .any(|d| d.eq_ignore_ascii_case(first))
}

/// Path relative to the app home, using the normalised separator.
fn relative_to_home(path: &Path) -> String {
    let full = normalise(path);
    let prefix = hermix_home_prefix();
    full.strip_prefix(&prefix).unwrap_or(&full).to_string()
}

/// Snapshot a harness component's current contents before it is overwritten.
///
/// Called immediately before a write lands. Deliberately **best-effort**: a
/// snapshot failure must never block the edit itself (the user's change matters
/// more than our ability to record its predecessor), so errors are logged and
/// swallowed.
///
/// Only existing files are snapshotted — a new file has no predecessor. The
/// snapshot name carries a millisecond timestamp so repeated edits in one turn
/// do not collide, and each file keeps at most [`HARNESS_SNAPSHOT_KEEP`]
/// versions so `harness-history/` cannot grow without bound.
pub fn snapshot_harness_component(path: &Path) {
    if !is_harness_component(path) || !path.is_file() {
        return;
    }
    let Ok(previous) = std::fs::read(path) else {
        return;
    };
    let history = hermix_config::paths::hermix_home().join("harness-history");
    if std::fs::create_dir_all(&history).is_err() {
        return;
    }
    let rel = relative_to_home(path);
    let stem: String = rel
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
        .collect();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let target = history.join(format!("{stem}-{stamp}.bak"));
    if let Err(e) = std::fs::write(&target, &previous) {
        tracing::warn!(path = %path.display(), error = %e, "harness snapshot failed (write continues)");
        return;
    }
    tracing::debug!(path = %path.display(), snapshot = %target.display(), "harness component snapshotted");

    // Prune: keep the newest N for this stem.
    let prefix = format!("{stem}-");
    let Ok(entries) = std::fs::read_dir(&history) else {
        return;
    };
    let mut same: Vec<std::path::PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".bak"))
        })
        .collect();
    if same.len() > HARNESS_SNAPSHOT_KEEP {
        same.sort();
        let drop_count = same.len() - HARNESS_SNAPSHOT_KEEP;
        for old in same.into_iter().take(drop_count) {
            let _ = std::fs::remove_file(old);
        }
    }
}

/// Screen a shell command for a reference to a permission-governing file.
///
/// The shell can reach these without ever passing a path argument to a file
/// tool (`echo ... > $HOME/.hermix/approval_allowlist.json`), so the command
/// text is checked too. Best-effort by design — see the module note.
pub fn command_touches_permission_config(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    if HERMIX_CONFIG_NAMES.iter().any(|n| lower.contains(n)) {
        return true;
    }
    lower.contains(GENERIC_CONFIG_NAME)
        && (lower.contains(".hermix") || lower.contains(&hermix_home_prefix()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hermix_specific_names_are_protected_anywhere() {
        assert!(is_permission_config_path(Path::new(
            "C:/work/proj/approval_allowlist.json"
        )));
        assert!(is_permission_config_path(Path::new(
            "/tmp/session_approvals.json"
        )));
        // Case-insensitive (Windows filesystems are).
        assert!(is_permission_config_path(Path::new(
            "C:/x/APPROVAL_ALLOWLIST.JSON"
        )));
    }

    #[test]
    fn generic_config_is_only_protected_inside_the_app_home() {
        // A project's own config.yaml must stay writable.
        assert!(!is_permission_config_path(Path::new("C:/work/proj/config.yaml")));
        let home = hermix_config::paths::hermix_home();
        assert!(is_permission_config_path(&home.join("config.yaml")));
    }

    #[test]
    fn ordinary_files_stay_writable() {
        assert!(!is_permission_config_path(Path::new("C:/work/proj/main.rs")));
        assert!(!is_permission_config_path(Path::new("MEMORY.md")));
        assert!(check_write(Path::new("C:/work/proj/out.txt")).is_ok());
    }

    /// Harness components are recognised **under the app home only**, and
    /// permission configs are explicitly *not* among them — those stay hard
    /// denied (a snapshot implies "the write went through", which is exactly
    /// what must not happen for an allowlist).
    #[test]
    fn harness_components_are_recognised_under_the_home() {
        let home = hermix_config::paths::hermix_home();
        for rel in [
            "SOUL.md",
            "MEMORY.md",
            "USER.md",
            "personalities/ops.md",
            "agents/reviewer.md",
            "guidance/web_automation.md",
            "scripts/summarize.py",
            "skills/code/review/SKILL.md",
        ] {
            assert!(
                is_harness_component(&home.join(rel)),
                "{rel} must count as a harness component"
            );
        }
        // Same names outside the home are ordinary project files.
        assert!(!is_harness_component(Path::new("C:/work/SOUL.md")));
        assert!(!is_harness_component(Path::new("C:/work/guidance/x.md")));
        // Permission configs are hard-denied, never snapshotted.
        assert!(!is_harness_component(&home.join("approval_allowlist.json")));
        assert!(!is_harness_component(&home.join("config.yaml")));
    }

    #[test]
    fn shell_commands_reaching_the_allowlist_are_flagged() {
        assert!(command_touches_permission_config(
            r#"echo '{"x":1}' > $HOME/.hermix/approval_allowlist.json"#
        ));
        assert!(command_touches_permission_config(
            "Remove-Item C:\\Users\\me\\.hermix\\session_approvals.json"
        ));
        // A project config.yaml without the app home is normal work.
        assert!(!command_touches_permission_config("cat config.yaml"));
        assert!(!command_touches_permission_config("cargo build --release"));
    }
}
