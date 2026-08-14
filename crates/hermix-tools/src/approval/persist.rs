//! Session-approval persistence (JSON file round-trip).
use std::path::Path;
// ---------------------------------------------------------------------------
//Session-approval persistence
// ---------------------------------------------------------------------------

/// Load a previously-persisted session-approval set from `path`.
/// Failure matrix (mirrors `UserAllowlist::load`):
/// | File state        | Result                                          |
/// |-------------------|-------------------------------------------------|
/// | Missing           | empty set (no warning)                          |
/// | Empty             | empty set (no warning)                          |
/// | Valid JSON array  | populated set                                   |
/// | Corrupt JSON      | empty set + warning                             |
/// | Permission denied | empty set + warning                             |
/// Load is **never** fatal — a corrupt or unreadable file must not
/// stop the agent from starting.
pub fn load_session_approvals(path: &Path) -> std::collections::HashSet<String> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Default::default(),
        Err(e) => {
            eprintln!(
                "[hermix-approval] session_approvals read error ({}); starting with empty set",
                e
            );
            return Default::default();
        }
    };
    if bytes.is_empty() {
        return Default::default();
    }
    match serde_json::from_slice::<Vec<String>>(&bytes) {
        Ok(entries) => entries.into_iter().collect(),
        Err(e) => {
            eprintln!(
                "[hermix-approval] session_approvals parse error ({}); starting with empty set",
                e
            );
            Default::default()
        }
    }
}

/// Persist the session-approval set to `path` (atomic via tmp + rename).
/// Creates the parent directory if missing. Windows needs the target
/// removed before rename (`std::fs::rename` fails if target exists on
/// Windows); POSIX rename atomically replaces.
pub fn persist_session_approvals(
    path: &Path,
    set: &std::collections::HashSet<String>,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Stable order so re-saving the same set produces byte-identical
    // output (makes file diffs / change-detection trivial in tests).
    let mut entries: Vec<&String> = set.iter().collect();
    entries.sort();
    let bytes = serde_json::to_vec_pretty(&entries)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &bytes)?;
    #[cfg(windows)]
    {
        // On Windows, remove_file before rename is needed because
        // rename fails if target exists. Ignore NotFound errors (file
        // doesn't exist yet) but log other errors for diagnostics.
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
