//! Context model — `@`-reference parser for Task context.
//!
//! Parses `@`-references from user input text, classifying each as
//! a file, directory, URL, or session reference. The parsed references
//! populate [`TaskContext::references`] which the agent loop can use
//! to preload files or resolve context.
//!
//! (Phase 1 — P1-3)

use hermix_core::{TaskContext, TaskContextRef, TaskContextRefKind};

/// Parse `@`-references from user input text.
///
/// Recognizes:
/// - `@path/to/file.rs` → `File`
/// - `@path/to/dir/` → `Directory` (trailing slash)
/// - `@http://example.com` / `@https://...` → `Url`
/// - `@session:abc123` → `Session`
/// - `@anything_else` → `Unknown`
///
/// References are extracted by scanning for `@` followed by
/// non-whitespace characters. The `@` itself is not included in
/// the `target` field.
///
/// # Example
/// ```
/// use hermix_task::context::parse_at_references;
///
/// let refs = parse_at_references("Fix @src/main.rs and check @https://example.com");
/// assert_eq!(refs.len(), 2);
/// assert_eq!(refs[0].target, "src/main.rs");
/// assert_eq!(refs[0].kind, hermix_core::TaskContextRefKind::File);
/// ```
pub fn parse_at_references(text: &str) -> Vec<TaskContextRef> {
    let mut refs = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'@' {
            // Start collecting the reference target
            let start = i + 1;
            let mut end = start;

            // Skip the `@` and collect until whitespace or end of string
            while end < bytes.len() && !bytes[end].is_ascii_whitespace() {
                end += 1;
            }

            if end > start {
                let target = &text[start..end];
                let kind = classify_reference(target);
                refs.push(TaskContextRef {
                    target: target.to_string(),
                    kind,
                });
            }

            i = end;
        } else {
            i += 1;
        }
    }

    refs
}

/// Classify a `@`-reference target into a [`TaskContextRefKind`].
fn classify_reference(target: &str) -> TaskContextRefKind {
    // URLs
    if target.starts_with("http://") || target.starts_with("https://") {
        return TaskContextRefKind::Url;
    }

    // Session references
    if target.starts_with("session:") {
        return TaskContextRefKind::Session;
    }

    // Directory (trailing slash or known directory patterns)
    if target.ends_with('/') || target.ends_with('\\') {
        return TaskContextRefKind::Directory;
    }

    // Check if it looks like a file path (contains a dot in the last
    // segment, or contains path separators)
    let has_separator = target.contains('/') || target.contains('\\');
    let last_segment = target.rsplit(['/', '\\']).next().unwrap_or(target);
    let has_extension = last_segment.contains('.') && !last_segment.starts_with('.');

    if has_separator || has_extension {
        return TaskContextRefKind::File;
    }

    // Could be a bare filename like "README" — treat as file
    if !target.is_empty() {
        return TaskContextRefKind::File;
    }

    TaskContextRefKind::Unknown
}

/// Parse `@`-references from text and merge them into an existing
/// [`TaskContext`]'s `references` field (deduplicating by target).
pub fn merge_references_into(context: &mut TaskContext, text: &str) {
    let new_refs = parse_at_references(text);
    for new_ref in new_refs {
        // Deduplicate by target string
        if !context.references.iter().any(|r| r.target == new_ref.target) {
            context.references.push(new_ref);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_file_reference() {
        let refs = parse_at_references("Fix @src/main.rs");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].target, "src/main.rs");
        assert_eq!(refs[0].kind, TaskContextRefKind::File);
    }

    #[test]
    fn test_parse_directory_reference() {
        let refs = parse_at_references("Look at @src/");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].target, "src/");
        assert_eq!(refs[0].kind, TaskContextRefKind::Directory);
    }

    #[test]
    fn test_parse_url_reference() {
        let refs = parse_at_references("Check @https://example.com");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].target, "https://example.com");
        assert_eq!(refs[0].kind, TaskContextRefKind::Url);
    }

    #[test]
    fn test_parse_session_reference() {
        let refs = parse_at_references("Review @session:abc123");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].target, "session:abc123");
        assert_eq!(refs[0].kind, TaskContextRefKind::Session);
    }

    #[test]
    fn test_parse_multiple_references() {
        let refs = parse_at_references("Fix @src/main.rs and @src/lib.rs then check @https://docs.rs");
        assert_eq!(refs.len(), 3);
    }

    #[test]
    fn test_parse_no_references() {
        let refs = parse_at_references("Just a regular message with no refs");
        assert!(refs.is_empty());
    }

    #[test]
    fn test_parse_bare_at() {
        let refs = parse_at_references("Email me @");
        assert!(refs.is_empty());
    }

    #[test]
    fn test_parse_windows_path() {
        let refs = parse_at_references("Fix @src\\main.rs");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].kind, TaskContextRefKind::File);
    }

    #[test]
    fn test_parse_adjacent_references() {
        let refs = parse_at_references("@file1.rs @file2.rs");
        assert_eq!(refs.len(), 2);
    }

    #[test]
    fn test_merge_references_into_context() {
        let mut ctx = TaskContext::default();
        merge_references_into(&mut ctx, "Fix @src/main.rs");
        assert_eq!(ctx.references.len(), 1);
        // Deduplicate
        merge_references_into(&mut ctx, "Fix @src/main.rs and @src/lib.rs");
        assert_eq!(ctx.references.len(), 2);
    }

    #[test]
    fn test_classify_bare_filename() {
        assert_eq!(classify_reference("README"), TaskContextRefKind::File);
    }

    #[test]
    fn test_classify_empty() {
        assert_eq!(classify_reference(""), TaskContextRefKind::Unknown);
    }
}
