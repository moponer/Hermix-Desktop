//! Skill health scoring (P2-T3).
//!
//! Skills can drift over time — wrong examples, dead URLs,
//! duplicated boilerplate — and we want the scanner to flag
//! suspicious ones without outright refusing to load them.
//!
//! The score is a 0.0–1.0 composite of four cheap deterministic
//! signals:
//!
//! 1. **Frontmatter completeness** (0.20 weight): every
//!    SKILL.md should expose `name` and `description`. The
//!    open-standard agentskills.io fields (`version`, `tags`,
//!    `allowed_tools`) are bonuses, not requirements.
//! 2. **Section structure** (0.30 weight): at least 3 markdown
//!    `##` headings — skills with no structure are nearly
//!    impossible for the LLM to follow.
//! 3. **Token budget** (0.30 weight): line count 50–500 is
//!    the sweet spot. Below 50 the skill is suspiciously thin;
//!    above 500 the body gets soft-truncated by the loader so
//!    the user is paying a budget penalty for little gain.
//! 4. **Code-example presence** (0.20 weight): at least one
//!    fenced code block (` ``` `) — the task description says
//!    "no API cost" so we accept any fence, even if the
//!    language tag is missing.
//!
//! Anything below 0.5 is logged at WARN with a clear message;
//! the loader still indexes the skill (we never want the
//! scanner to silently drop a skill the agent was relying on).
//!
//! Plan reference: `.workbuddy/SKILL_SYSTEM_REFACTOR_PLAN.md` §P2-T3.

/// Weighted components of the composite score. Public so tests
/// can assert individual dimensions without baking the entire
/// rubric into a single opaque number.
#[derive(Debug, Clone, Copy, Default)]
pub struct HealthBreakdown {
    pub frontmatter: f32,
    pub structure: f32,
    pub budget: f32,
    pub examples: f32,
}

impl HealthBreakdown {
    /// Weighted sum with the constants chosen so each dimension
    /// maxes out at its declared weight (`frontmatter` ≤ 0.20,
    /// etc.) and the overall composite lives in [0.0, 1.0].
    pub fn composite(&self) -> f32 {
        let c = self.frontmatter * 0.20
            + self.structure * 0.30
            + self.budget * 0.30
            + self.examples * 0.20;
        c.clamp(0.0, 1.0)
    }
}

/// Score a SKILL.md file. `path` is used for log context only
/// — all reading happens against the supplied `content`.
///
/// Returns the breakdown so callers can log per-axis warnings.
pub fn score_skill_md(path: &std::path::Path, content: &str) -> HealthBreakdown {
    let mut b = HealthBreakdown::default();

    // --- Frontmatter completeness (0.20) ---
    let (frontmatter, body) = crate::skill_orchestrator::parse_frontmatter(content);
    b.frontmatter = {
        let mut partial = 0.0f32;
        if frontmatter.name.is_some() {
            partial += 0.5;
        }
        if frontmatter.description.is_some() {
            partial += 0.5;
        }
        // Bonus for at least one open-standard extension field.
        let extras = [
            frontmatter.version.is_some(),
            frontmatter.author.is_some(),
            frontmatter.allowed_tools.is_some(),
            frontmatter.tags.is_some(),
        ]
        .iter()
        .filter(|x| **x)
        .count();
        if extras >= 1 {
            partial += 0.25;
        }
        partial.min(1.0)
    };

    // --- Section structure (0.30) ---
    let heading_count = body
        .lines()
        .filter(|l| l.trim_start().starts_with("## "))
        .count();
    b.structure = match heading_count {
        0 => 0.0,
        1 => 0.4,
        2 => 0.7,
        _ => 1.0,
    };

    // --- Token / line budget (0.30) ---
    // We count lines because chars is a poor proxy for cost; the
    // loader already uses a 500-line soft cap.
    let line_count = body.lines().count().max(1);
    b.budget = if line_count < 10 {
        // Suspiciously thin.
        0.2
    } else if line_count < 50 {
        0.6
    } else if line_count <= 500 {
        1.0
    } else {
        // Past the soft cap.
        0.5
    };

    // --- Code example (0.20) ---
    let code_fence_count = body.matches("```").count() / 2;
    b.examples = if code_fence_count >= 1 { 1.0 } else { 0.0 };

    let composite = b.composite();
    if composite < 0.5 {
        tracing::warn!(
            skill_path = %path.display(),
            composite = composite,
            breakdown = ?b,
            "skill below health threshold; will still load but consider revising"
        );
    }

    b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(tmp: &std::path::Path, body: &str) -> std::path::PathBuf {
        let p = tmp.join("skill.md");
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn health_score_perfect_skill() {
        let tmp = tempfile::tempdir().unwrap();
        // 50+ line body so the budget dimension earns its 1.0;
        // we want every axis at full to confirm the composite
        // reaches the upper end.
        let body = (0..60)
            .map(|i| format!("body line {i}\n"))
            .collect::<String>();
        let path = write_skill(
            tmp.path(),
            &format!(
                "---\nname: foo\ndescription: A skill\nversion: 1.0.0\ntags: [x]\n---\n\
                 # Title\n\n## Overview\n\nIntro.\n\n## Steps\n\n1. Do this.\n2. Do that.\n\n```bash\necho hi\n```\n\n## Notes\n\n{body}"
            ),
        );
        let content = std::fs::read_to_string(&path).unwrap();
        let b = score_skill_md(&path, &content);
        let composite = b.composite();
        assert!(
            composite >= 0.9,
            "well-structured skill should score ≥ 0.9, got {composite:?} breakdown={b:?}"
        );
    }

    #[test]
    fn health_score_below_threshold_with_just_frontmatter() {
        let tmp = tempfile::tempdir().unwrap();
        // No body, only frontmatter + one paragraph.
        let path = write_skill(
            tmp.path(),
            "---\nname: foo\ndescription: A skill\n---\nJust one line.",
        );
        let content = std::fs::read_to_string(&path).unwrap();
        let b = score_skill_md(&path, &content);
        let composite = b.composite();
        assert!(
            composite < 0.5,
            "bare-bones skill should score < 0.5, got {composite}"
        );
    }

    #[test]
    fn health_score_penalises_too_long_body() {
        let tmp = tempfile::tempdir().unwrap();
        let mut body = String::from("---\nname: foo\ndescription: A skill\n---\n");
        for i in 0..600 {
            body.push_str(&format!("line {i}\n"));
        }
        let path = write_skill(tmp.path(), &body);
        let content = std::fs::read_to_string(&path).unwrap();
        let b = score_skill_md(&path, &content);
        // Budget is capped (0.5) once past 500 lines; the other
        // three dimensions are missing too, so the composite
        // should be well below 0.7.
        assert!(
            b.composite() < 0.7,
            "600-line body should pull composite under 0.7, got {}",
            b.composite()
        );
    }

    #[test]
    fn health_score_breakdown_records_each_axis() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_skill(
            tmp.path(),
            "---\nname: foo\ndescription: A skill\n---\n## A\n\n## B\n\n## C\n",
        );
        let content = std::fs::read_to_string(&path).unwrap();
        let b = score_skill_md(&path, &content);
        // 3 headings → structure = 1.0; no code → examples = 0.0
        assert!((b.structure - 1.0).abs() < f32::EPSILON);
        assert!((b.examples - 0.0).abs() < f32::EPSILON);
    }
}
