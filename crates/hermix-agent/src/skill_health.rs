//! Skill health scoring (, extended ).
//!
//! Skills can drift over time — wrong examples, dead URLs,
//! duplicated boilerplate — and we want the scanner to flag
//! suspicious ones without outright refusing to load them.
//!
//! The score is a 0.0–1.0 composite of **nine** deterministic
//! signals (expanded from the original four in ):
//!
//! | # | Dimension                         | Weight | Source     |
//! |---|-----------------------------------|--------|------------|
//! | 1 | Frontmatter `name` present        | 0.10   | Original   |
//! | 2 | Frontmatter `description` + trigger| 0.15   | Extended   |
//! | 3 | `## When to Activate` section     | 0.15   | New        |
//! | 4 | `## Core Concepts` / `## Instructions` | 0.10 | Split from structure |
//! | 5 | `## Practical Guidance` / `## Examples` | 0.10 | Extended from examples |
//! | 6 | `## Gotchas` section              | 0.05   | New        |
//! | 7 | `## Integration` section          | 0.05   | New        |
//! | 8 | `## References` section           | 0.05   | New        |
//! | 9 | Body ≤ 500 lines (budget)         | 0.05   | Adjusted   |
//! |   | **Total**                         | **0.80**|           |
//!
//! Bonus dimensions (up to 0.20 extra, capping composite at 1.0):
//! - Version field present (+0.05)
//! - Tags field present (+0.05)
//! - Code-example present (+0.05)
//! - Additional section count > 3 (+0.05)
//!
//! Anything below 0.5 is logged at WARN with a clear message;
//! the loader still indexes the skill (we never want the
//! scanner to silently drop a skill the agent was relying on).
//!
//! Plan reference: `.workbuddy/SKILL_SYSTEM_REFACTOR_PLAN.md` §.

/// Weighted components of the composite score. Public so tests
/// can assert individual dimensions without baking the entire
/// rubric into a single opaque number.
#[derive(Debug, Clone, Copy, Default)]
pub struct HealthBreakdown {
    //Core 9 dimensions ---
    /// Frontmatter `name` present (0.10 weight).
    pub frontmatter_name: f32,
    /// Frontmatter `description` present and contains trigger phrase (0.15 weight).
    pub frontmatter_description: f32,
    /// `## When to Activate` section present (0.15 weight).
    pub when_to_activate: f32,
    /// `## Core Concepts` or `## Instructions` present (0.10 weight).
    pub core_concepts: f32,
    /// `## Practical Guidance` or `## Examples` present (0.10 weight).
    pub practical_guidance: f32,
    /// `## Gotchas` section present (0.05 weight).
    pub gotchas: f32,
    /// `## Integration` section present (0.05 weight).
    pub integration: f32,
    /// `## References` section present (0.05 weight).
    pub references: f32,
    /// Body line count within budget (≤ 500 lines) (0.05 weight).
    pub budget: f32,

    // --- Bonus dimensions (up to 0.20 extra) ---
    /// Version field in frontmatter (+0.05 bonus).
    pub bonus_version: f32,
    /// Tags field in frontmatter (+0.05 bonus).
    pub bonus_tags: f32,
    /// At least one fenced code block (+0.05 bonus).
    pub bonus_code_example: f32,
    /// More than 3 `##` headings (+0.05 bonus).
    pub bonus_section_count: f32,
}

impl HealthBreakdown {
    /// Weighted sum with the constants chosen so each core dimension
    /// maxes out at its declared weight and the overall composite
    /// lives in [0.0, 1.0]. Bonus dimensions can push the raw
    /// composite above 1.0, which is then clamped.
    pub fn composite(&self) -> f32 {
        let core = self.frontmatter_name * 0.10
            + self.frontmatter_description * 0.15
            + self.when_to_activate * 0.15
            + self.core_concepts * 0.10
            + self.practical_guidance * 0.10
            + self.gotchas * 0.05
            + self.integration * 0.05
            + self.references * 0.05
            + self.budget * 0.05;
        let bonus = self.bonus_version * 0.05
            + self.bonus_tags * 0.05
            + self.bonus_code_example * 0.05
            + self.bonus_section_count * 0.05;
        (core + bonus).clamp(0.0, 1.0)
    }
}

// ---------------------------------------------------------------------------
// Section detection helpers
// ---------------------------------------------------------------------------

/// Check if a markdown body contains a `## Section Name` heading
/// (case-insensitive).
fn has_section(body: &str, heading: &str) -> bool {
    body.lines().any(|line| {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("## ") {
            return false;
        }
        let title = trimmed[3..].trim();
        // Exact match, or the canonical heading is used as a prefix
        // followed by a separator — real skills write `## 实践指南：数据与
        // 本机能力` / `## Core Concepts: Foo` and stranding those at 0.0
        // punished the skill for adding a clarifying subtitle.
        if title.eq_ignore_ascii_case(heading) {
            return true;
        }
        let Some(rest) = title.get(heading.len()..) else {
            return false;
        };
        title
            .get(..heading.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(heading))
            && rest.starts_with([':', '：', '(', '（', '-', '—', ' '])
    })
}

/// Like [`has_section`] but matches *any* of several candidate headings.
/// Used to support both English and Chinese section titles so the health
/// gate works for the product's Chinese-language skills (e.g. `## When to
/// Activate` and `## 何时激活` are treated identically).
fn has_any_section(body: &str, headings: &[&str]) -> bool {
    headings.iter().any(|h| has_section(body, h))
}

/// Trigger-word patterns that indicate a description tells the LLM
/// *when* to activate the skill. Covers both English and Chinese so the
/// product's Chinese-language skills get full description credit.
const TRIGGER_PHRASES: &[&str] = &[
    "use when",
    "use this when",
    "activate when",
    "use for",
    "use this for",
    "applies when",
    "relevant when",
    "use in",
    "use case",
    // Chinese
    "用于",
    "适用",
    "当你",
    "使用时",
    "激活时",
    "以下场景",
    "在以下情况",
    "涉及",
    "触发词",
    "相关任务",
];

/// Check if the description contains a trigger phrase.
pub(crate) fn description_has_trigger(desc: &str) -> bool {
    let lower = desc.to_lowercase();
    TRIGGER_PHRASES.iter().any(|phrase| lower.contains(phrase))
}

/// Count the number of `##` headings in the body.
fn count_headings(body: &str) -> usize {
    body.lines()
        .filter(|l| l.trim_start().starts_with("## "))
        .count()
}

// ---------------------------------------------------------------------------
// Corpus scoring
// ---------------------------------------------------------------------------

/// Aggregate health report for a corpus of skills.
#[derive(Debug, Clone)]
pub struct SkillHealthReport {
    /// Total number of skills scored.
    pub total: usize,
    /// Number of skills with composite < 0.5 (flagged).
    pub flagged: usize,
    /// Average composite score across all skills.
    pub avg_composite: f32,
    /// Per-skill breakdowns: (name, composite, breakdown).
    pub entries: Vec<(String, f32, HealthBreakdown)>,
}

/// Score all SKILL.md files in a directory. Each immediate subdirectory
/// is expected to contain a `SKILL.md` file.
pub fn score_corpus(skills_dir: &std::path::Path) -> SkillHealthReport {
    let mut entries = Vec::new();
    let mut sum = 0.0f32;
    let mut flagged = 0usize;

    if let Ok(read_dir) = std::fs::read_dir(skills_dir) {
        for entry in read_dir.flatten() {
            let skill_md = entry.path().join("SKILL.md");
            if skill_md.is_file() {
                if let Ok(content) = std::fs::read_to_string(&skill_md) {
                    let b = score_skill_md(&skill_md, &content);
                    let composite = b.composite();
                    let name = entry
                        .file_name()
                        .to_string_lossy()
                        .to_string();
                    if composite < 0.5 {
                        flagged += 1;
                    }
                    sum += composite;
                    entries.push((name, composite, b));
                }
            }
        }
    }

    let total = entries.len();
    let avg_composite = if total > 0 { sum / total as f32 } else { 0.0 };

    SkillHealthReport {
        total,
        flagged,
        avg_composite,
        entries,
    }
}

// ---------------------------------------------------------------------------
// Main scoring function
// ---------------------------------------------------------------------------

/// Score a SKILL.md file. `path` is used for log context only
/// — all reading happens against the supplied `content`.
///
/// Returns the breakdown so callers can log per-axis warnings.
pub fn score_skill_md(path: &std::path::Path, content: &str) -> HealthBreakdown {
    score_skill_md_inner(path, content, true)
}

/// Same scoring as [`score_skill_md`] but without the WARN log line.
///
/// Used by hot paths that re-score a skill every time its content is
/// injected into the LLM prompt (e.g. `build_skill_invocation_message`,
/// `build_preloaded_skills_prompt`). Logging there would spam the same
/// warning on every invocation — the scan-time `score_skill_md` call
/// already reported it once.
pub fn score_skill_md_silent(path: &std::path::Path, content: &str) -> HealthBreakdown {
    score_skill_md_inner(path, content, false)
}

fn score_skill_md_inner(
    path: &std::path::Path,
    content: &str,
    log_warning: bool,
) -> HealthBreakdown {
    let mut b = HealthBreakdown::default();
    let (frontmatter, body) = crate::skill_orchestrator::parse_frontmatter(content);

    // --- 1. Frontmatter name (0.10) ---
    b.frontmatter_name = if frontmatter.name.is_some() { 1.0 } else { 0.0 };

    // --- 2. Frontmatter description + trigger (0.15) ---
    b.frontmatter_description = match &frontmatter.description {
        None => 0.0,
        Some(desc) if desc.trim().is_empty() => 0.0,
        Some(desc) => {
            if description_has_trigger(desc) {
                1.0
            } else {
                // Description present but no trigger phrase: partial credit.
                0.5
            }
        }
    };

    // --- 3. ## When to Activate (0.15) --- (English + Chinese aliases)
    b.when_to_activate = if has_any_section(
        body,
        &[
            "When to Activate",
            "When to Use",
            "何时激活",
            "适用场景",
            "使用场景",
            "激活时机",
            "触发条件",
            "激活条件",
            "何时使用",
            "适用范围",
        ],
    ) {
        1.0
    } else {
        0.0
    };

    // --- 4. ## Core Concepts or ## Instructions (0.10) ---
    b.core_concepts = if has_any_section(
        body,
        &[
            "Core Concepts",
            "Instructions",
            "核心概念",
            "核心方法",
            "核心实践",
            "核心框架",
            "核心原则",
            "核心立场",
            "核心理念",
            "指令",
            "原则",
            "执行步骤",
            "工作流",
            "操作步骤",
        ],
    ) {
        1.0
    } else {
        0.0
    };

    // --- 5. ## Practical Guidance or ## Examples (0.10) ---
    b.practical_guidance = if has_any_section(
        body,
        &[
            "Practical Guidance",
            "Examples",
            "实践指南",
            "示例",
            "例子",
            "实践",
            "实战",
            "用法",
            "调用方式",
            "完整示例",
        ],
    ) {
        1.0
    } else {
        0.0
    };

    // --- 6. ## Gotchas (0.05) ---
    b.gotchas = if has_any_section(
        body,
        &[
            "Gotchas",
            "注意事项",
            "常见错误",
            "避坑",
            "陷阱",
            "硬性规则",
            "硬规则",
            "约束",
            "禁止事项",
        ],
    ) {
        1.0
    } else {
        0.0
    };

    // --- 7. ## Integration (0.05) ---
    b.integration = if has_any_section(
        body,
        &[
            "Integration",
            "集成",
            "整合",
            "衔接",
            "协作",
            "与用户对话",
            "与其他模块的配合",
        ],
    ) {
        1.0
    } else {
        0.0
    };

    // --- 8. ## References (0.05) ---
    b.references = if has_any_section(
        body,
        &[
            "References",
            "参考",
            "参考资料",
            "引用",
            "来源",
            "相关文档",
            "延伸阅读",
        ],
    ) {
        1.0
    } else {
        0.0
    };

    // --- 9. Body line budget (0.05) ---
    let line_count = body.lines().count().max(1);
    b.budget = if line_count <= 500 { 1.0 } else { 0.0 };

    // --- Bonus dimensions ---
    b.bonus_version = if frontmatter.version.is_some() { 1.0 } else { 0.0 };
    b.bonus_tags = if frontmatter.tags.is_some() { 1.0 } else { 0.0 };
    b.bonus_code_example = if body.matches("```").count() / 2 >= 1 { 1.0 } else { 0.0 };
    b.bonus_section_count = if count_headings(body) > 3 { 1.0 } else { 0.0 };

    let composite = b.composite();
    if log_warning && composite < 0.5 && should_warn_once(path, composite) {
        tracing::warn!(
            skill_path = %path.display(),
            composite = composite,
            breakdown = ?b,
            "skill below health threshold; will still load but consider revising"
        );
    }

    b
}

/// Emit each `(skill path, score)` WARN at most once per process.
///
/// The scan it runs on is memoized (see
/// `skill_orchestrator::SKILL_SCAN_MEMO`), but a rebuild — every skill
/// install or edit — would otherwise re-print the same advisory line
/// for every unhealthy skill. On this machine that was ~36 lines per
/// turn and 83% of `agent.log`, which buried the lines that matter.
/// Keyed on the score too, so a skill that regresses warns again.
fn should_warn_once(path: &std::path::Path, composite: f32) -> bool {
    static WARNED: std::sync::Mutex<Option<std::collections::HashSet<(std::path::PathBuf, u32)>>> =
        std::sync::Mutex::new(None);
    let mut guard = WARNED.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .get_or_insert_with(std::collections::HashSet::new)
        .insert((path.to_path_buf(), composite.to_bits()))
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
        let body = (0..60)
            .map(|i| format!("body line {i}\n"))
            .collect::<String>();
        let path = write_skill(
            tmp.path(),
            &format!(
                "---\nname: foo\ndescription: Use when refactoring code\nversion: 1.0.0\ntags: [x]\n---\n\
                 # Title\n\n## When to Activate\n\nWhen refactoring.\n\n## Core Concepts\n\nConcepts.\n\n\
                 ## Practical Guidance\n\n```bash\necho hi\n```\n\n## Gotchas\n\nWatch out.\n\n\
                 ## Integration\n\nWorks with debugging.\n\n## References\n\n- context.rs\n\n## Notes\n\n{body}"
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
        let mut body = String::from("---\nname: foo\ndescription: Use when coding\n---\n");
        for i in 0..600 {
            body.push_str(&format!("line {i}\n"));
        }
        let path = write_skill(tmp.path(), &body);
        let content = std::fs::read_to_string(&path).unwrap();
        let b = score_skill_md(&path, &content);
        // Budget should be 0.0 for > 500 lines; the composite should
        // be below 0.7 since many other dimensions are missing too.
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
            "---\nname: foo\ndescription: Use when testing\n---\n## A\n\n## B\n\n## C\n\n## D\n",
        );
        let content = std::fs::read_to_string(&path).unwrap();
        let b = score_skill_md(&path, &content);
        // 4+ headings → bonus_section_count = 1.0
        assert!((b.bonus_section_count - 1.0).abs() < f32::EPSILON);
        // No When to Activate → when_to_activate = 0.0
        assert!((b.when_to_activate - 0.0).abs() < f32::EPSILON);
        // Description has trigger → frontmatter_description = 1.0
        assert!((b.frontmatter_description - 1.0).abs() < f32::EPSILON);
    }

    //New dimension tests ---

    #[test]
    fn new_dimensions_when_to_activate() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_skill(
            tmp.path(),
            "---\nname: foo\ndescription: Use when coding\n---\n## When to Activate\n\nActivate me.",
        );
        let content = std::fs::read_to_string(&path).unwrap();
        let b = score_skill_md(&path, &content);
        assert!((b.when_to_activate - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn new_dimensions_gotchas() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_skill(
            tmp.path(),
            "---\nname: foo\ndescription: Use when coding\n---\n## Gotchas\n\nBe careful.",
        );
        let content = std::fs::read_to_string(&path).unwrap();
        let b = score_skill_md(&path, &content);
        assert!((b.gotchas - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn new_dimensions_integration() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_skill(
            tmp.path(),
            "---\nname: foo\ndescription: Use when coding\n---\n## Integration\n\nWorks with bar.",
        );
        let content = std::fs::read_to_string(&path).unwrap();
        let b = score_skill_md(&path, &content);
        assert!((b.integration - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn new_dimensions_references() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_skill(
            tmp.path(),
            "---\nname: foo\ndescription: Use when coding\n---\n## References\n\n- context.rs",
        );
        let content = std::fs::read_to_string(&path).unwrap();
        let b = score_skill_md(&path, &content);
        assert!((b.references - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn description_without_trigger_gets_partial_credit() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_skill(
            tmp.path(),
            "---\nname: foo\ndescription: A generic skill\n---\n## When to Activate\n\nWhen needed.",
        );
        let content = std::fs::read_to_string(&path).unwrap();
        let b = score_skill_md(&path, &content);
        // Description present but no trigger → 0.5 partial credit
        assert!(
            (b.frontmatter_description - 0.5).abs() < f32::EPSILON,
            "expected 0.5 partial credit, got {}",
            b.frontmatter_description
        );
    }

    /// A canonical heading followed by a clarifying subtitle must still
    /// score. Real skills write `## 实践指南：数据与本机能力` — stranding
    /// that at 0.0 punished the skill for being MORE specific.
    #[test]
    fn heading_with_subtitle_still_scores() {
        for title in [
            "## 实践指南：数据与本机能力",
            "## 核心概念：目录与骨架",
            "## Core Concepts: Directory Layout",
            "## Gotchas (read this first)",
            "## References — internal",
        ] {
            let body = format!("{title}\n\ncontent\n");
            let canonical = title
                .trim_start_matches("## ")
                .split([':', '：', '(', '—'])
                .next()
                .unwrap()
                .trim()
                .to_string();
            assert!(
                has_section(&body, &canonical),
                "subtitle form must match: {title} (canonical={canonical})"
            );
        }
    }

    /// A longer heading that merely STARTS with the canonical words but
    /// has no separator is a different section — must not match.
    #[test]
    fn heading_prefix_without_separator_does_not_match() {
        assert!(!has_section("## 实践指南针\n", "实践指南"));
        assert!(!has_section("## ReferencesAndNotes\n", "References"));
    }

    #[test]
    fn corpus_scoring_works() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std::fs::create_dir_all(skills_dir.join("good-skill")).unwrap();
        std::fs::create_dir_all(skills_dir.join("bad-skill")).unwrap();

        std::fs::write(
            skills_dir.join("good-skill").join("SKILL.md"),
            "---\nname: good\ndescription: Use when testing\nversion: 1.0\ntags: [x]\n---\n\
             ## When to Activate\n\nWhen testing.\n\n## Core Concepts\n\nStuff.\n\n\
             ## Practical Guidance\n\n```bash\necho\n```\n\n## Gotchas\n\nNone.\n\n\
             ## Integration\n\nNone.\n\n## References\n\n- mod.rs",
        )
        .unwrap();

        std::fs::write(
            skills_dir.join("bad-skill").join("SKILL.md"),
            "---\nname: bad\n---\nNo structure.",
        )
        .unwrap();

        let report = score_corpus(&skills_dir);
        assert_eq!(report.total, 2);
        assert_eq!(report.flagged, 1); // bad-skill should be flagged
        assert!(report.avg_composite > 0.0);
    }

    // --- Bilingual (Chinese) support ---

    #[test]
    fn chinese_section_aliases_score_full() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_skill(
            tmp.path(),
            "---\nname: foo\ndescription: 用于协助用户起草或润色各类文本内容\nversion: 1.0.0\ntags: [x]\n---\n\
             # 标题\n\n## 何时激活\n\n起草或润色时。\n\n## 核心概念\n\n先定读者。\n\n\
             ## 实践指南\n\n```bash\necho hi\n```\n\n## 注意事项\n\n别替用户做判断。\n\n\
             ## 集成\n\n与摘要任务衔接。\n\n## 参考\n\n- ctx.rs\n\n## 其它\n\n补充。",
        );
        let content = std::fs::read_to_string(&path).unwrap();
        let b = score_skill_md(&path, &content);
        assert!((b.when_to_activate - 1.0).abs() < f32::EPSILON);
        assert!((b.core_concepts - 1.0).abs() < f32::EPSILON);
        assert!((b.practical_guidance - 1.0).abs() < f32::EPSILON);
        assert!((b.gotchas - 1.0).abs() < f32::EPSILON);
        assert!((b.integration - 1.0).abs() < f32::EPSILON);
        assert!((b.references - 1.0).abs() < f32::EPSILON);
        // Chinese trigger phrase → full description credit
        assert!((b.frontmatter_description - 1.0).abs() < f32::EPSILON);
        // All dims present (+ version/tags/code/section bonuses) → ≥ 0.9
        assert!(
            b.composite() >= 0.9,
            "chinese skill should score ≥ 0.9, got {} breakdown={:?}",
            b.composite(),
            b
        );
    }

    #[test]
    fn chinese_trigger_phrase_gives_full_description_credit() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_skill(
            tmp.path(),
            "---\nname: foo\ndescription: 用于协助用户重构代码\n---\n## 适用场景\n\n重构时。",
        );
        let content = std::fs::read_to_string(&path).unwrap();
        let b = score_skill_md(&path, &content);
        assert!((b.frontmatter_description - 1.0).abs() < f32::EPSILON);
        // `## 适用场景` is a Chinese alias for When to Activate
        assert!((b.when_to_activate - 1.0).abs() < f32::EPSILON);
    }
}
