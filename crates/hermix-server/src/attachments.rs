//! Attached non-image documents → disk.
//!
//! Clients send raw file bytes on the wire (`SendMessageRequest.attached_files`,
//! mirroring the `images` channel) because browsers/WebView2 never expose a
//! filesystem path on the JS `File` object. This module is the seam where
//! bytes become paths: each file is written under `<hermix_home>/attachments/`
//! and the resolved absolute paths ride into the turn via
//! `AgentOverrides.attached_file_paths`, so the agent can read them with
//! `read_file` / `read_pdf` like any local file.

use hermix_transport::FileAttachment;
use std::path::{Path, PathBuf};

/// Persist every attachment under `dir`; returns the paths that were
/// written successfully (failed writes are logged and skipped — a broken
/// attachment must not fail the whole turn).
pub fn persist_attached_files_in(dir: &Path, files: &[FileAttachment]) -> Vec<String> {
    if files.is_empty() {
        return Vec::new();
    }
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::warn!(dir = %dir.display(), error = %e, "persist_attached_files: mkdir failed");
        return Vec::new();
    }
    let mut paths = Vec::with_capacity(files.len());
    for (i, f) in files.iter().enumerate() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let path: PathBuf = dir.join(format!("{stamp}-{i}-{}", sanitize_file_name(&f.name)));
        match std::fs::write(&path, &f.bytes) {
            Ok(()) => paths.push(path.to_string_lossy().to_string()),
            Err(e) => tracing::warn!(name = %f.name, error = %e, "persist_attached_files: write failed"),
        }
    }
    paths
}

/// `<hermix_home>/attachments/` — the server process resolves the portable
/// data dir exactly like the session store, so paths stay valid across
/// restarts.
pub fn attachments_dir() -> PathBuf {
    hermix_config::paths::hermix_home().join("attachments")
}

/// Persist into the default home; see [`persist_attached_files_in`].
pub fn persist_attached_files(files: &[FileAttachment]) -> Vec<String> {
    persist_attached_files_in(&attachments_dir(), files)
}

/// Strip anything that could escape the attachments dir or upset Windows:
/// control chars and `<>:"/\|?*` become `_`, trailing dots/spaces are cut
/// (Windows strips them on open, enabling name-collision tricks), length is
/// capped. The result is display/extension info only — the millis-index
/// prefix guarantees uniqueness, so the original name is never load-bearing.
pub(crate) fn sanitize_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '\u{0}'..='\u{1f}' | '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            _ => c,
        })
        .collect();
    let trimmed = cleaned.trim_matches(|c| c == '.' || c == ' ');
    let cut: String = trimmed.chars().take(120).collect();
    if cut.is_empty() {
        "attachment".to_string()
    } else {
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fa(name: &str, bytes: &[u8]) -> FileAttachment {
        FileAttachment {
            name: name.to_string(),
            mime_type: None,
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn sanitize_strips_path_traversal_and_windows_invalids() {
        assert_eq!(sanitize_file_name("..\\..\\evil.pdf"), "_.._evil.pdf");
        assert_eq!(sanitize_file_name("a/b/c.txt"), "a_b_c.txt");
        assert_eq!(sanitize_file_name("col:on.pdf"), "col_on.pdf");
        assert_eq!(sanitize_file_name("  ..hidden.  "), "hidden");
    }

    #[test]
    fn sanitize_empty_or_dotdot_falls_back() {
        assert_eq!(sanitize_file_name(""), "attachment");
        assert_eq!(sanitize_file_name(".."), "attachment");
    }

    #[test]
    fn sanitize_keeps_unicode_names() {
        assert_eq!(sanitize_file_name("报告.pdf"), "报告.pdf");
    }

    #[test]
    fn sanitize_caps_length() {
        let long = "a".repeat(300) + ".pdf";
        let out = sanitize_file_name(&long);
        // Extension loss on absurdly long names is fine — the millis-index
        // prefix, not the name, guarantees uniqueness.
        assert_eq!(out.chars().count(), 120);
        assert!(out.chars().all(|c| c == 'a'));
    }

    #[test]
    fn persist_writes_files_and_returns_paths() {
        let tmp = std::env::temp_dir().join(format!("hermix-att-test-{}", std::process::id()));
        let dir = tmp.join("attachments");
        let paths = persist_attached_files_in(
            &dir,
            &[fa("report.pdf", b"%PDF-1.4"), fa("notes.md", b"# hi")],
        );
        assert_eq!(paths.len(), 2);
        for p in &paths {
            let content = std::fs::read(p).expect("file must exist on disk");
            assert!(!content.is_empty());
        }
        assert!(paths[0].ends_with("report.pdf"));
        assert!(paths[1].ends_with("notes.md"));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn persist_empty_slice_returns_empty_without_mkdir() {
        let missing = std::env::temp_dir().join("hermix-att-nonexistent-dir");
        assert!(persist_attached_files_in(&missing, &[]).is_empty());
        assert!(!missing.exists());
    }

    #[test]
    fn persist_same_name_twice_keeps_both() {
        let tmp = std::env::temp_dir().join(format!("hermix-att-skip-{}", std::process::id()));
        // The millis-index prefix keeps two same-named files distinct, so
        // both survive instead of the second overwriting the first.
        let paths = persist_attached_files_in(&tmp, &[fa("same.txt", b"1"), fa("same.txt", b"2")]);
        assert_eq!(paths.len(), 2);
        assert_ne!(
            std::fs::read_to_string(&paths[0]).unwrap(),
            std::fs::read_to_string(&paths[1]).unwrap()
        );
        std::fs::remove_dir_all(&tmp).ok();
    }
}
