//! — auto-patch a skill when it underperforms.
//!
//! When [`SkillLoader`] matches a skill but the user reports it
//! failed (tool result \u2192 negative rating, or repeated failure code),
//! the agent may call `skill_patch` to update the SKILL.md with a
//! better body. The patch step:\//!
//! - Refuses to overwrite an **auto** skill ( owns that).
//! - Backs up the existing SKILL.md to a timestamped `.bak` file
//!   before writing the new one, so a bad patch is one undo away.
//! - Validates the slug the same way [`skill_auto_create::skill_slug`]
//!   does, so a hostile / accidental name cannot escape the root.
//!
//! ## Soft-fail
//!
//! Every entry returns [`Result`]; callers should swallow the error
//! and log it. A bad patch must never crash a session.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::skill_auto_create::{skill_slug, AUTO_SKILL_DIR};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum SkillPatchError {
    SkillNotFound(PathBuf),
    RefusingToPatchAutoSkill(PathBuf),
    BackupFailed { primary: PathBuf, cause: io::Error },
    WriteFailed { target: PathBuf, cause: io::Error },
    EmptyBody,
}

impl std::fmt::Display for SkillPatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SkillNotFound(p) => write!(f, "skill not found: {}", p.display()),
            Self::RefusingToPatchAutoSkill(p) => write!(
                f,
                "auto skills are read-only here; use skill_auto_create instead: {}",
                p.display()
            ),
            Self::BackupFailed { primary, cause } => write!(
                f,
                "could not back up {}: {}",
                primary.display(),
                cause
            ),
            Self::WriteFailed { target, cause } => write!(
                f,
                "could not write {}: {}",
                target.display(),
                cause
            ),
            Self::EmptyBody => write!(f, "empty patch body"),
        }
    }
}

impl std::error::Error for SkillPatchError {}

// ---------------------------------------------------------------------------
// Backup naming
// ---------------------------------------------------------------------------

/// Build a `.bak.<unix_ts>` style filename. Pure function so tests
/// don't depend on the wall clock. Caller supplies the timestamp if
/// they want determinism (default: now).
pub fn backup_name(base: &str, ts: u64) -> String {
    format!("{base}.bak.{ts}")
}

/// Quick unix timestamp in seconds. Wrapped because [`SystemTime`]
/// arithmetic can panic pre-epoch; we floor to 0 in that case.
pub fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Locate
// ---------------------------------------------------------------------------

/// Walk `root` looking for `<dir>/<slug>/SKILL.md`. Returns the path
/// on hit, [`SkillPatchError::SkillNotFound`] on miss. Does not
/// cross the boundary into `auto/`\u2014those are managed by and
/// are reported as [`SkillPatchError::RefusingToPatchAutoSkill`] when
/// they are explicitly addressed.
pub fn locate_skill(root: &Path, slug_raw: &str) -> Result<PathBuf, SkillPatchError> {
    let slug = skill_slug(slug_raw);
    let p = root.join(&slug).join("SKILL.md");
    if !p.exists() {
        return Err(SkillPatchError::SkillNotFound(p));
    }
    // Refuse to touch a skill whose slug itself is `auto` — that
    // would resolve to <root>/auto/SKILL.md which is the auto-root
    // marker, not a regular skill. We do NOT walk into <root>/auto/
    // subdirs here; a `slug` lookup never descends into them, so the
    // interior of an auto skill is unreachable through this API.
    if slug == AUTO_SKILL_DIR {
        return Err(SkillPatchError::RefusingToPatchAutoSkill(p));
    }
    Ok(p)
}

// ---------------------------------------------------------------------------
// Patch
// ---------------------------------------------------------------------------

/// Patch the named skill by replacing its body. Returns the path of
/// the new file. The original is preserved as a `.bak.<ts>` sibling.
///
/// `body` should be the **plain markdown body** (frontmatter will be
/// rewritten to keep `kind: patch` so an audit can find patched
/// skills later).
pub fn patch_skill(
    root: &Path,
    slug: &str,
    body: &str,
) -> Result<PathBuf, SkillPatchError> {
    if body.trim().is_empty() {
        return Err(SkillPatchError::EmptyBody);
    }
    let target = locate_skill(root, slug)?;

    // Backup. We do it BEFORE the write so a write failure is
    // impossible without preserving the original.
    let ts = now_unix_secs();
    let backup = target.with_file_name(backup_name(
        target.file_name().unwrap().to_str().unwrap_or("SKILL.md"),
        ts,
    ));
    if let Err(e) = fs::copy(&target, &backup) {
        return Err(SkillPatchError::BackupFailed { primary: target, cause: e });
    }

    // Rewrite with kind=patch and a tiny audit line. We deliberately
    // re-render instead of mutating in place so the file is always
    // self-consistent.
    let rendered = render_patched_md(slug, body);
    if let Err(e) = fs::write(&target, rendered) {
        return Err(SkillPatchError::WriteFailed { target, cause: e });
    }
    Ok(target)
}

/// Render a SKILL.md body after a patch. `kind` flips from whatever
/// it was to `patch` (recognised by [`is_patched_skill`]).
pub fn render_patched_md(name: &str, body: &str) -> String {
    let mut out = String::with_capacity(body.len() + 192);
    out.push_str("---\nname: ");
    out.push_str(name);
    out.push_str("\nkind: patch\ndescription: |-\n");
    // 块标量内容必须缩进：顶格的 `## 标题` 会被 YAML 当成键，
    // 整份 frontmatter 解析失败（与 auto skill 同一个坑，见
    // `skill_auto_create::push_indented_block`）。
    crate::skill_auto_create::push_indented_block(&mut out, body, "  ");
    out.push_str("---\n\n");
    out.push_str(body);
    out
}

/// Cheap recogniser: a file is "patched" iff its first ~256 bytes
/// contain `kind: patch`. We do not parse YAML here \u2014 the frontmatter
/// format is intentionally liberal.
pub fn is_patched_skill(content: &str) -> bool {
    let head = &content[..content.len().min(512)];
    head.lines().take(20).any(|l| l.trim() == "kind: patch")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a bare SKILL.md at `root/<slug>/SKILL.md`. We skip the
    /// full frontmatter helper because these tests only need a
    /// placeholder file that locator can find.
    fn seed_manual(root: &Path, slug: &str, body: &str) -> PathBuf {
        let dir = root.join(slug);
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("SKILL.md");
        fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn backup_name_format() {
        assert_eq!(backup_name("SKILL.md", 1234567890), "SKILL.md.bak.1234567890");
    }

    #[test]
    fn backup_name_preserves_extension() {
        // .bak.<ts> stays consistent so multiple backups stack
        // monotonically by timestamp.
        assert!(backup_name("note.md", 1).starts_with("note.md.bak."));
    }

    #[test]
    fn locate_skill_finds_manual_seed() {
        let dir = tempfile::tempdir().unwrap();
        seed_manual(dir.path(), "cool-skill", "do cool stuff");
        let p = locate_skill(dir.path(), "Cool Skill!").unwrap();
        assert!(p.ends_with("cool-skill/SKILL.md"));
    }

    #[test]
    fn locate_skill_returns_not_found_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        match locate_skill(dir.path(), "nope") {
            Err(SkillPatchError::SkillNotFound(_)) => (),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn patch_skill_creates_backup_and_replaces_body() {
        let dir = tempfile::tempdir().unwrap();
        seed_manual(dir.path(), "thing", "old body\n");
        let target = patch_skill(dir.path(), "thing", "new body\n").unwrap();
        let now = std::fs::read_to_string(&target).unwrap();
        assert!(now.contains("kind: patch"));
        assert!(now.contains("new body"));
        assert!(!now.contains("old body"));
        // At least one .bak.<ts> sibling exists.
        let mut found_bak = false;
        for entry in std::fs::read_dir(dir.path().join("thing")).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("SKILL.md.bak.") {
                let bak = std::fs::read_to_string(entry.path()).unwrap();
                assert!(bak.contains("old body"));
                found_bak = true;
            }
        }
        assert!(found_bak, "expected a .bak.<ts> backup file");
    }

    #[test]
    fn patch_skill_rejects_empty_body() {
        let dir = tempfile::tempdir().unwrap();
        seed_manual(dir.path(), "thing", "old");
        match patch_skill(dir.path(), "thing", "   \n  ") {
            Err(SkillPatchError::EmptyBody) => (),
            other => panic!("expected EmptyBody, got {other:?}"),
        }
    }

    #[test]
    fn patch_skill_refuses_to_touch_auto_root_marker() {
        let dir = tempfile::tempdir().unwrap();
        // Plant a SKILL.md at <root>/auto/SKILL.md so the locator
        // finds a file but the guard still has to refuse.
        let auto_dir = dir.path().join(AUTO_SKILL_DIR);
        std::fs::create_dir_all(&auto_dir).unwrap();
        std::fs::write(auto_dir.join("SKILL.md"), "old").unwrap();
        let err = patch_skill(dir.path(), AUTO_SKILL_DIR, "new").unwrap_err();
        match err {
            SkillPatchError::RefusingToPatchAutoSkill(_) => (),
            other => panic!("expected RefusingToPatchAutoSkill, got {other:?}"),
        }
    }

    #[test]
    fn patch_skill_missing_target_returns_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let err = patch_skill(dir.path(), "ghost", "new").unwrap_err();
        match err {
            SkillPatchError::SkillNotFound(_) => (),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn render_patched_md_has_patch_kind() {
        let out = render_patched_md("foo", "do thing");
        assert!(out.contains("kind: patch"));
        assert!(out.contains("do thing"));
    }

    #[test]
    fn is_patched_skill_recognises_marker() {
        let raw = "---\nname: foo\nkind: patch\n---\n\nbody";
        assert!(is_patched_skill(raw));
        let raw2 = "---\nname: foo\nkind: auto\n---\n\nbody";
        assert!(!is_patched_skill(raw2));
    }

    #[test]
    fn is_patched_skill_handles_no_frontmatter() {
        let raw = "no frontmatter at all\n";
        assert!(!is_patched_skill(raw));
    }
}
