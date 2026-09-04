//! Before/after diff for approval cards.
//!
//! The approval UI shows a unified diff of what a file-mutating tool call
//! is about to do. The raw material is already on the wire
//! (`RequestApprovalItem.arguments`), but a true before/after needs the
//! target file's current contents, which only the backend can read — so
//! the diff is computed here, at approval-request time, and shipped to
//! the client as a plain string.

use similar::TextDiff;

/// Cap so a pathological `write_file` (or a huge generated file) can't
/// flood the approval card with a multi-megabyte diff.
const MAX_DIFF_CHARS: usize = 8_000;

/// Extract a unified diff for a file-mutating tool call, or `None` when
/// the call shape doesn't give us enough to work with (non-file tools,
/// relative paths this gate can't resolve, ...). The UI keeps its
/// plain-description fallback for `None` — it never fabricates a diff.
pub fn approval_diff(tool: &str, args: &serde_json::Value) -> Option<String> {
    let _ = tool; // dispatch is shape-based, not name-based, so unknown/aliased tools work too
    let obj = args.as_object()?;

    // edit-style: the call carries both sides — no filesystem needed
    // (`edit_file` / `patch` with old_string/new_string).
    let old = obj.get("old_string").and_then(|v| v.as_str());
    let new = obj.get("new_string").and_then(|v| v.as_str());
    if let (Some(old), Some(new)) = (old, new) {
        return Some(unified(old, new));
    }

    // write-style: new contents + the path of the file being replaced
    // (`write_file`).
    let content = obj
        .get("content")
        .or_else(|| obj.get("new_content"))
        .or_else(|| obj.get("text"))
        .and_then(|v| v.as_str())?;
    let path = obj
        .get("path")
        .or_else(|| obj.get("file_path"))
        .or_else(|| obj.get("file"))
        .and_then(|v| v.as_str())?;
    let p = std::path::Path::new(path);
    // Only absolute paths are safe to resolve here — the file tools
    // resolve relative paths against the session workspace, which this
    // gate doesn't know. `None` keeps the UI's fallback description.
    if !p.is_absolute() {
        return None;
    }
    let old = std::fs::read_to_string(p).unwrap_or_default();
    Some(unified(&old, content))
}

fn unified(old: &str, new: &str) -> String {
    let diff = TextDiff::from_lines(old, new);
    let text = diff.unified_diff().context_radius(3).to_string();
    if text.chars().count() <= MAX_DIFF_CHARS {
        return text;
    }
    let head: String = text.chars().take(MAX_DIFF_CHARS).collect();
    format!("{head}\n… (diff 过长，已截断)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn edit_style_uses_old_new_strings_without_fs() {
        let args = json!({
            "path": "does/not/matter.rs",
            "old_string": "fn a() {}\n",
            "new_string": "fn a() { /* changed */ }\n",
        });
        let d = approval_diff("edit_file", &args).expect("diff");
        assert!(d.contains("-fn a() {}"));
        assert!(d.contains("+fn a() { /* changed */ }"));
    }

    #[test]
    fn write_style_new_file_shows_all_additions() {
        let tmp = std::env::temp_dir().join("hermix-approval-diff-new.txt");
        let _ = std::fs::remove_file(&tmp);
        let args = json!({
            "path": tmp.to_string_lossy(),
            "content": "hello\n",
        });
        let d = approval_diff("write_file", &args).expect("diff");
        assert!(d.contains("+hello"));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn write_style_overwrite_shows_removal_and_addition() {
        let tmp = std::env::temp_dir().join("hermix-approval-diff-over.txt");
        std::fs::write(&tmp, "old line\n").unwrap();
        let args = json!({
            "path": tmp.to_string_lossy(),
            "content": "new line\n",
        });
        let d = approval_diff("write_file", &args).expect("diff");
        assert!(d.contains("-old line"));
        assert!(d.contains("+new line"));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn relative_path_yields_none() {
        let args = json!({ "path": "relative/foo.txt", "content": "x\n" });
        assert!(approval_diff("write_file", &args).is_none());
    }

    #[test]
    fn non_file_args_yield_none() {
        assert!(approval_diff("terminal", &json!({ "command": "ls" })).is_none());
    }
}
