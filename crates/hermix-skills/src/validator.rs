//! Skill document structure validator .
//!
//! Validates SKILL.md files against the context-engineering documentation
//! standard. Checks frontmatter fields, required/optional sections, and
//! trigger-word presence in descriptions.
//!
//! This is separate from `guard.rs` which handles security (dangerous
//! patterns, URL blocking). This module handles **structural quality**
//! — making sure every skill document follows a consistent schema so
//! the LLM can route to it effectively.

use std::path::Path;

// ---------------------------------------------------------------------------
// Validation result types
// ---------------------------------------------------------------------------

/// Severity of a validation finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Informational — no action needed.
    Info,
    /// Warning — should fix, does not block loading.
    Warn,
    /// Error — must fix before the skill can be considered valid.
    Error,
}

/// A single validation finding.
#[derive(Debug, Clone)]
pub struct ValidationFinding {
    /// What was checked.
    pub rule: &'static str,
    /// How severe the finding is.
    pub severity: Severity,
    /// Human-readable message.
    pub message: String,
}

/// Aggregate result of validating a SKILL.md.
#[derive(Debug, Clone)]
pub struct ValidationResult {
    /// Path of the validated file (for log context).
    pub path: String,
    /// All findings, ordered by severity (Error > Warn > Info).
    pub findings: Vec<ValidationFinding>,
}

impl ValidationResult {
    /// Returns `true` if there are no Error-severity findings.
    pub fn is_pass(&self) -> bool {
        !self.findings.iter().any(|f| f.severity == Severity::Error)
    }

    /// Returns only the error-severity findings.
    pub fn errors(&self) -> Vec<&ValidationFinding> {
        self.findings.iter().filter(|f| f.severity == Severity::Error).collect()
    }

    /// Returns only the warn-severity findings.
    pub fn warnings(&self) -> Vec<&ValidationFinding> {
        self.findings.iter().filter(|f| f.severity == Severity::Warn).collect()
    }
}

// ---------------------------------------------------------------------------
// Frontmatter parsing (lightweight, no serde-yaml dependency needed here)
// ---------------------------------------------------------------------------

/// Minimal parsed frontmatter for validation purposes.
#[derive(Debug, Clone, Default)]
struct Frontmatter {
    name: Option<String>,
    description: Option<String>,
}

/// Extract YAML frontmatter from SKILL.md content.
/// Returns (frontmatter, body).
fn parse_frontmatter(content: &str) -> (Frontmatter, &str) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (Frontmatter::default(), content);
    }
    let after_first = &trimmed[3..];
    let after_first = after_first.trim_start_matches(['\r', '\n']);
    if let Some(end_idx) = after_first.find("\n---") {
        let yaml_block = &after_first[..end_idx];
        let body_start = end_idx + 4;
        let body = after_first[body_start..].trim_start_matches(['\r', '\n']);
        let fm = parse_yaml_simple(yaml_block);
        (fm, body)
    } else {
        (Frontmatter::default(), content)
    }
}

/// Ultra-minimal YAML parser — only extracts `name` and `description`
/// scalar values. Good enough for validation; the full parser lives
/// in `hermix-agent/src/skill_orchestrator.rs`.
fn parse_yaml_simple(yaml: &str) -> Frontmatter {
    let mut fm = Frontmatter::default();
    for line in yaml.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("name:") {
            let val = val.trim().trim_matches('"').trim_matches('\'');
            if !val.is_empty() {
                fm.name = Some(val.to_string());
            }
        } else if let Some(val) = line.strip_prefix("description:") {
            let val = val.trim().trim_matches('"').trim_matches('\'');
            if !val.is_empty() {
                fm.description = Some(val.to_string());
            }
        }
    }
    fm
}

// ---------------------------------------------------------------------------
// Section presence helpers
// ---------------------------------------------------------------------------

/// Check if a markdown body contains a `## Section Name` heading.
pub fn has_section(body: &str, heading: &str) -> bool {
    body.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("## ") && trimmed[3..].trim().eq_ignore_ascii_case(heading)
    })
}

/// Trigger-word patterns that indicate a description tells the LLM
/// *when* to activate the skill.
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
];

/// Check if the description contains a trigger phrase.
pub fn description_has_trigger(desc: &str) -> bool {
    let lower = desc.to_lowercase();
    TRIGGER_PHRASES.iter().any(|phrase| lower.contains(phrase))
}

// ---------------------------------------------------------------------------
// Main validation function
// ---------------------------------------------------------------------------

/// Validate a SKILL.md file's structure.
/// `path` is used for log/diagnostic context only.
/// `content` is the full text of the SKILL.md file.
pub fn validate_skill_md(path: &Path, content: &str) -> ValidationResult {
    let mut findings = Vec::new();
    let (fm, body) = parse_frontmatter(content);

    // --- Frontmatter: name (required) ---
    if fm.name.is_none() {
        findings.push(ValidationFinding {
            rule: "frontmatter.name",
            severity: Severity::Error,
            message: "SKILL.md frontmatter must contain a `name` field".to_string(),
        });
    }

    // --- Frontmatter: description (required) ---
    match &fm.description {
        None => {
            findings.push(ValidationFinding {
                rule: "frontmatter.description",
                severity: Severity::Error,
                message: "SKILL.md frontmatter must contain a `description` field".to_string(),
            });
        }
        Some(desc) => {
            if !description_has_trigger(desc) {
                findings.push(ValidationFinding {
                    rule: "frontmatter.description.trigger",
                    severity: Severity::Warn,
                    message: format!(
                        "description should contain a trigger phrase (e.g. 'Use when ...'); \
                         found: {:?}",
                        desc
                    ),
                });
            }
        }
    }

    // --- Required sections ---
    if !has_section(body, "When to Activate") {
        findings.push(ValidationFinding {
            rule: "section.when_to_activate",
            severity: Severity::Error,
            message: "SKILL.md body must contain `## When to Activate` section".to_string(),
        });
    }

    if !has_section(body, "Integration") {
        findings.push(ValidationFinding {
            rule: "section.integration",
            severity: Severity::Error,
            message: "SKILL.md body must contain `## Integration` section".to_string(),
        });
    }

    // --- Recommended sections (warn, not block) ---
    if !has_section(body, "Gotchas") {
        findings.push(ValidationFinding {
            rule: "section.gotchas",
            severity: Severity::Warn,
            message: "SKILL.md body should contain `## Gotchas` section".to_string(),
        });
    }

    if !has_section(body, "Examples") && !has_section(body, "Practical Guidance") {
        findings.push(ValidationFinding {
            rule: "section.examples",
            severity: Severity::Warn,
            message: "SKILL.md body should contain `## Examples` or `## Practical Guidance` section"
                .to_string(),
        });
    }

    // Sort by severity (Error first, then Warn, then Info).
    findings.sort_by(|a, b| b.severity.cmp(&a.severity));

    ValidationResult {
        path: path.display().to_string(),
        findings,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn validate(content: &str) -> ValidationResult {
        validate_skill_md(&PathBuf::from("test.md"), content)
    }

    #[test]
    fn valid_skill_passes() {
        let result = validate(
            "---\nname: foo\ndescription: Use when refactoring code\n---\n\
             ## When to Activate\n\nWhen you need to refactor.\n\n\
             ## Integration\n\nWorks with debugging skill.\n\n\
             ## Gotchas\n\nNone.\n\n\
             ## Examples\n\n```bash\nrefactor\n```",
        );
        assert!(result.is_pass(), "should pass: {:?}", result.findings);
        assert!(result.errors().is_empty());
    }

    #[test]
    fn missing_name_is_error() {
        let result = validate(
            "---\ndescription: A skill\n---\n## When to Activate\n\n## Integration\n",
        );
        assert!(!result.is_pass());
        assert!(result.errors().iter().any(|f| f.rule == "frontmatter.name"));
    }

    #[test]
    fn missing_description_is_error() {
        let result = validate("---\nname: foo\n---\n## When to Activate\n\n## Integration\n");
        assert!(!result.is_pass());
        assert!(result.errors().iter().any(|f| f.rule == "frontmatter.description"));
    }

    #[test]
    fn description_without_trigger_is_warn() {
        let result = validate(
            "---\nname: foo\ndescription: A generic skill\n---\n\
             ## When to Activate\n\n## Integration\n",
        );
        assert!(result.is_pass(), "warn should not block");
        assert!(result.warnings().iter().any(|f| f.rule == "frontmatter.description.trigger"));
    }

    #[test]
    fn missing_when_to_activate_is_error() {
        let result = validate(
            "---\nname: foo\ndescription: Use when coding\n---\n## Integration\n",
        );
        assert!(!result.is_pass());
        assert!(result.errors().iter().any(|f| f.rule == "section.when_to_activate"));
    }

    #[test]
    fn missing_integration_is_error() {
        let result = validate(
            "---\nname: foo\ndescription: Use when coding\n---\n## When to Activate\n",
        );
        assert!(!result.is_pass());
        assert!(result.errors().iter().any(|f| f.rule == "section.integration"));
    }

    #[test]
    fn missing_gotchas_is_warn() {
        let result = validate(
            "---\nname: foo\ndescription: Use when coding\n---\n\
             ## When to Activate\n\n## Integration\n## Examples\n",
        );
        assert!(result.is_pass());
        assert!(result.warnings().iter().any(|f| f.rule == "section.gotchas"));
    }

    #[test]
    fn missing_examples_is_warn() {
        let result = validate(
            "---\nname: foo\ndescription: Use when coding\n---\n\
             ## When to Activate\n\n## Integration\n## Gotchas\n",
        );
        assert!(result.is_pass());
        assert!(result.warnings().iter().any(|f| f.rule == "section.examples"));
    }

    #[test]
    fn no_frontmatter_fails() {
        let result = validate("Just some text without frontmatter");
        assert!(!result.is_pass());
    }

    #[test]
    fn trigger_phrases_are_case_insensitive() {
        let result = validate(
            "---\nname: foo\ndescription: USE WHEN you need help\n---\n\
             ## When to Activate\n\n## Integration\n",
        );
        let trigger_warnings: Vec<_> = result
            .findings
            .iter()
            .filter(|f| f.rule == "frontmatter.description.trigger")
            .collect();
        assert!(
            trigger_warnings.is_empty(),
            "uppercase 'USE WHEN' should be recognized as trigger"
        );
    }
}
