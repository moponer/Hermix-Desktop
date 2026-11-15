//! — Agent self-created skills.
//!
//! When a session demonstrates a useful, repeatable tool sequence
//! (>= `[AUTO_SKILL_MIN_TOOL_STEPS]` distinct tool calls), the agent
//! may capture it as a skill so future sessions can re-use the
//! sequence with `/<slug>`.
//!
//! ## Storage
//!
//! Auto-created skills live at:
//!
//! ```text
//! ~/.hermix/skills/auto/<slug>/SKILL.md
//! ```
//!
//! `~/.hermix/skills/auto/` is one of the registered skill roots
//! (see `skill_orchestrator`), so the next session picks the new
//! skill up automatically.
//!
//! ## Safety
//!
//! - **Slug sanitisation**: every dynamic segment is run through
//!   [`skill_slug`] which strips path separators, control chars, and
//!   unicode word characters down to ASCII a-z 0-9 -.
//! - **No overwrites**: [`write_auto_skill`] refuses to replace an
//!   existing non-auto skill (manual skills stay authoritative).
//! - **Pre-existing seed**: refuse to seed a skill if a *manual* one
//!   already lives at that slug; auto skills share the namespace.
//! - **Soft fail**: every public entry returns `Result` and the agent
//!   loop is expected to swallow the error with a debug-level log so
//!   a permissions hiccup never breaks the session.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Minimum number of distinct tool invocations before auto-creation
/// is considered worth surfacing. Below this we stay silent — the
/// example is too small to be a reusable pattern.
pub const AUTO_SKILL_MIN_TOOL_STEPS: usize = 5;

/// Cap on a single auto-skill body. Auto skills are small by design:
/// the prompt budget is precious, and a 30 KB auto skill is
/// invariably noise. Pull detailed docs from elsewhere instead.
pub const AUTO_SKILL_MAX_BYTES: usize = 16_000;

/// Subdirectory under the user skills root reserved for auto skills.
pub const AUTO_SKILL_DIR: &str = "auto";

// ---------------------------------------------------------------------------
// Slug
// ---------------------------------------------------------------------------

/// Convert a free-form name (or joined tool sequence) to a path-safe
/// slug: lowercased ASCII a-z 0-9 and `-`, no leading or trailing
/// separators, every internal separator collapsed to one `-`.
///
/// * `My Function!` → `my-function`
/// * `compose_email` → `compose-email`
/// * `   spaces   ` → `spaces`
/// * Empty / all-punctuation input → `auto` (the fallback slug).
pub fn skill_slug(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_dash = true;
    for ch in input.chars() {
        let lc = ch.to_ascii_lowercase();
        if lc.is_ascii_alphanumeric() {
            out.push(lc);
            last_dash = false;
        } else if lc.is_whitespace() || matches!(lc, '-' | '_' | '.') {
            if !last_dash {
                out.push('-');
                last_dash = true;
            }
        }
        // Drop everything else (punctuation, control, non-ASCII, etc.)
    }
    let trimmed = out.trim_end_matches('-').to_string();
    if trimmed.is_empty() {
        return "auto".to_string();
    }
    if trimmed.len() > 64 {
        // Length cap keeps directory listings sane. 64 chars is
        // comfortable for both filesystem and tool args.
        trimmed.chars().take(64).collect()
    } else {
        trimmed
    }
}

// ---------------------------------------------------------------------------
// Heuristic gate
// ---------------------------------------------------------------------------

/// Decide whether the recorded tool sequence deserves to be turned
/// into a skill. A sequence qualifies when it:
///
/// - has **>= [`AUTO_SKILL_MIN_TOOL_STEPS`] distinct calls** — below
/// that the "pattern" is just a single answer.
/// - has **>= 2 distinct tool names** — a single-tool count of 5 is
/// not a sequence, it's just repeated retrying.
/// - the dominant tool does not exceed 80 % of the calls — extreme
/// repetition usually means the agent loop is stuck.
pub fn should_auto_create_skill(tool_names: &[String]) -> bool {
    if tool_names.len() < AUTO_SKILL_MIN_TOOL_STEPS {
        return false;
    }
    let distinct: std::collections::HashSet<&String> = tool_names.iter().collect();
    if distinct.len() < 2 {
        return false;
    }
    // Worst-case dominance check (linear, O(n)).
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for name in tool_names {
        *counts.entry(name.as_str()).or_insert(0) += 1;
    }
    let max = counts.values().copied().max().unwrap_or(0);
    max * 5 <= tool_names.len() * 4 // i.e. max <= 0.8 * total
}

#[derive(Debug, PartialEq, Eq)]
pub enum AutoSkillReject {
    TooFewSteps,
    SingleToolOnly,
    DominantTool,
}

impl std::fmt::Display for AutoSkillReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooFewSteps => write!(f, "below min tool steps"),
            Self::SingleToolOnly => write!(f, "single-tool sequence"),
            Self::DominantTool => write!(f, "single tool dominates sequence"),
        }
    }
}

impl std::error::Error for AutoSkillReject {}

/// Same predicate as [`should_auto_create_skill`] but returns the
/// *reason* for rejection instead of a bool, so the audit log can
/// say *why* we skipped instead of just *that* we skipped.
pub fn auto_create_decision(tool_names: &[String]) -> Result<(), AutoSkillReject> {
    if tool_names.len() < AUTO_SKILL_MIN_TOOL_STEPS {
        return Err(AutoSkillReject::TooFewSteps);
    }
    let distinct: std::collections::HashSet<&String> = tool_names.iter().collect();
    if distinct.len() < 2 {
        return Err(AutoSkillReject::SingleToolOnly);
    }
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for name in tool_names {
        *counts.entry(name.as_str()).or_insert(0) += 1;
    }
    let max = counts.values().copied().max().unwrap_or(0);
    if max * 5 > tool_names.len() * 4 {
        return Err(AutoSkillReject::DominantTool);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// I/O
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum AutoSkillError {
    Io(io::Error),
    BodyTooLarge { bytes: usize, limit: usize },
    NameNotAllowed(PathBuf),
}

impl std::fmt::Display for AutoSkillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "i/o error: {e}"),
            Self::BodyTooLarge { bytes, limit } => {
                write!(f, "body {bytes}B exceeds limit {limit}B")
            }
            Self::NameNotAllowed(p) => {
                write!(f, "slug escapes auto root: {}", p.display())
            }
        }
    }
}

impl std::error::Error for AutoSkillError {}

impl From<io::Error> for AutoSkillError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Resolve the auto-skill directory for a given user skills root,
/// creating it (and intermediate parents) on demand. Idempotent.
///
/// `root` is the *full* skills root, e.g.
/// `~/.hermix/skills`. The auto subdirectory lives under it.
pub fn auto_skill_dir(root: &Path) -> io::Result<PathBuf> {
    let dir = root.join(AUTO_SKILL_DIR);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Render a complete SKILL.md body given the LLM-produced markdown
/// frontmatter + content. The function is liberal: anything goes,
/// as long as it doesn't exceed [`AUTO_SKILL_MAX_BYTES`]. The auto
/// author is itself an LLM, so we trust it (and still defensive-cap).
pub fn render_skill_md(name: &str, body: &str) -> String {
    let mut out = String::with_capacity(body.len() + 128);
    out.push_str("---\nname: ");
    out.push_str(name);
    out.push_str("\nkind: auto\ndescription: |-\n");
    push_indented_block(&mut out, body, "  ");
    out.push_str("---\n\n");
    out.push_str(body);
    out
}

/// 把一段 markdown 写成 YAML 块标量内容（每行缩进 `indent`）。
///
/// 此前是直接 `push_str(body)`：正文顶格 ⇒ 块标量在第一个顶格行结束，
/// `## 标题` 被 YAML 当成键，**整份 frontmatter 解析失败**。2026-11-06
/// 实机：`~/.hermix/skills/auto` 下 55 份技能因此报废，`GET /api/skills`
/// 每次刷新刷 271 条 `Failed to parse`。
pub(crate) fn push_indented_block(out: &mut String, text: &str, indent: &str) {
    let normalized = text.replace("\r\n", "\n");
    for line in normalized.split('\n') {
        if line.is_empty() {
            out.push('\n');
        } else {
            out.push_str(indent);
            out.push_str(line);
            out.push('\n');
        }
    }
}

/// Persist a new auto skill. Refuses to overwrite an existing file
/// — even another auto skill — so the agent can't thrash skills via
/// repeated calls during a single session. (Use
/// `skill_manage patch` for intentional updates; see .)
///
/// Returns the absolute path of the new file on success.
pub fn write_auto_skill(
    skills_root: &Path,
    name_raw: &str,
    body: &str,
) -> Result<PathBuf, AutoSkillError> {
    let slug = skill_slug(name_raw);
    if slug.is_empty() {
        return Err(AutoSkillError::NameNotAllowed(PathBuf::from(name_raw)));
    }
    if body.len() > AUTO_SKILL_MAX_BYTES {
        return Err(AutoSkillError::BodyTooLarge {
            bytes: body.len(),
            limit: AUTO_SKILL_MAX_BYTES,
        });
    }
    let dir = auto_skill_dir(skills_root)?;
    let target = dir.join(&slug).join("SKILL.md");
    if target.exists() {
        // Refuse to overwrite. Caller should rename or use patch.
        return Err(AutoSkillError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("auto skill already exists: {}", target.display()),
        )));
    }
    fs::create_dir_all(target.parent().unwrap())?;
    let rendered = render_skill_md(&slug, body);
    fs::write(&target, rendered)?;
    warn_if_auto_skills_pile_up(&dir);
    Ok(target.canonicalize().unwrap_or(target))
}

/// 沉淀技能数量超过这个数就提示一次。
///
/// 每个沉淀技能都会进路由匹配池（受 `AUTO_SKILL_SCORE_FACTOR` 降权与
/// `AUTO_SKILL_MIN_SCORE` 门槛约束，但仍会参与）。降权解决了"抢名额"，
/// 数量本身只影响扫描成本（每个 ~2KB）与目录可读性。
///
/// **不自动删除**：删掉的可能正是下次要复用的那条，而磁盘代价很小。
/// 提示一次（进程内 `OnceLock`）足够——反复刷日志没有额外价值。
pub const AUTO_SKILL_SOFT_LIMIT: usize = 80;

fn warn_if_auto_skills_pile_up(dir: &Path) {
    static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if WARNED.get().is_some() {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let count = entries.flatten().filter(|e| e.path().is_dir()).count();
    if count > AUTO_SKILL_SOFT_LIMIT {
        let _ = WARNED.set(());
        tracing::warn!(
            count,
            limit = AUTO_SKILL_SOFT_LIMIT,
            dir = %dir.display(),
            "自动沉淀的技能数量已超过软上限：它们参与路由匹配（已降权 + 更高的门槛），\
             但数量过多会拖慢扫描。可手动清理不再需要的目录。"
        );
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_lowercases_and_strips() {
        assert_eq!(skill_slug("My Function!"), "my-function");
        assert_eq!(skill_slug("compose_email"), "compose-email");
        assert_eq!(skill_slug("   spaces   "), "spaces");
        assert_eq!(skill_slug("Tabs\tand\nNewlines"), "tabs-and-newlines");
        assert_eq!(skill_slug("中文"), "auto"); // non-ASCII → fallback
        assert_eq!(skill_slug(""), "auto");
        assert_eq!(skill_slug("!!!"), "auto");
    }

    #[test]
    fn slug_collapses_runs_of_separators() {
        assert_eq!(skill_slug("a___b"), "a-b");
        assert_eq!(skill_slug("a - - b"), "a-b");
        assert_eq!(skill_slug(".a.b.c."), "a-b-c");
    }

    #[test]
    fn slug_length_capped_to_64() {
        let s: String = "a".repeat(200);
        let out = skill_slug(&s);
        assert_eq!(out.len(), 64);
    }

    #[test]
    fn auto_create_decision_rejects_short_sequences() {
        let v = vec!["a".to_string(); 3];
        assert_eq!(
            auto_create_decision(&v),
            Err(AutoSkillReject::TooFewSteps)
        );
        assert!(!should_auto_create_skill(&v));
    }

    #[test]
    fn auto_create_decision_rejects_single_tool() {
        let v = vec!["a".to_string(); 8];
        assert_eq!(
            auto_create_decision(&v),
            Err(AutoSkillReject::SingleToolOnly)
        );
        assert!(!should_auto_create_skill(&v));
    }

    #[test]
    fn auto_create_decision_rejects_dominant_tool() {
        // 8 calls, 7 of them the same tool. 7/8 = 87.5% > 80%.
        let mut v = vec!["dom".to_string(); 7];
        v.push("other".to_string());
        assert_eq!(
            auto_create_decision(&v),
            Err(AutoSkillReject::DominantTool)
        );
    }

    #[test]
    fn auto_create_decision_accepts_balanced_sequence() {
        // 8 calls, 4 + 4. 50% ≤ 80% → ok.
        let mut v = vec!["dom".to_string(); 4];
        v.extend(std::iter::repeat_with(|| "other".to_string()).take(4));
        assert!(auto_create_decision(&v).is_ok());
        assert!(should_auto_create_skill(&v));
    }

    #[test]
    fn auto_create_accepts_three_distinct_tools() {
        let v = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "a".to_string(),
            "b".to_string(),
        ];
        assert!(auto_create_decision(&v).is_ok());
    }

    #[test]
    fn render_skill_md_round_trip() {
        let body = "do the thing.\n1. step one\n2. step two";
        let out = render_skill_md("doer", body);
        assert!(out.starts_with("---\nname: doer\n"));
        assert!(out.contains("kind: auto"));
        assert!(out.contains(body));
        assert!(out.ends_with(body));
    }

    #[test]
    fn write_auto_skill_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_auto_skill(
            dir.path(),
            "MyTool",
            "## Step\nDo the bar.",
        )
        .unwrap();
        assert!(path.exists());
        let s = std::fs::read_to_string(&path).unwrap();
        assert!(s.contains("kind: auto"));
        assert!(s.contains("Do the bar."));
    }

    #[test]
    fn write_auto_skill_refuses_to_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        write_auto_skill(dir.path(), "dup", "first").unwrap();
        let err = write_auto_skill(dir.path(), "dup", "second").unwrap_err();
        match err {
            AutoSkillError::Io(e) => assert_eq!(e.kind(), io::ErrorKind::AlreadyExists),
            other => panic!("expected Io(AlreadyExists), got {other:?}"),
        }
    }

    #[test]
    fn write_auto_skill_rejects_oversized_body() {
        let dir = tempfile::tempdir().unwrap();
        let big = "x".repeat(AUTO_SKILL_MAX_BYTES + 1);
        let err = write_auto_skill(dir.path(), "big", &big).unwrap_err();
        match err {
            AutoSkillError::BodyTooLarge { bytes, limit } => {
                assert_eq!(bytes, AUTO_SKILL_MAX_BYTES + 1);
                assert_eq!(limit, AUTO_SKILL_MAX_BYTES);
            }
            other => panic!("expected BodyTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn auto_skill_dir_creates_intermediate() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("skills");
        let auto = auto_skill_dir(&root).unwrap();
        assert!(auto.is_dir());
        assert!(auto.ends_with("auto"));
    }
}
