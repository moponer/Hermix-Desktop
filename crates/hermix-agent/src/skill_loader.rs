//! Skill loader — auto-matches user messages to skills and injects instructions.
//!
//! (Phase 6 — P6-3)
//!
//! The existing [`SkillOrchestrator`] handles slash commands (`/skill-name`)
//! and session-wide preloading. This module adds **automatic skill matching**:
//!
//! 1. **Keyword matching**: When a user message contains keywords that match
//!    a skill's name, description, or tags, the skill's instructions are
//!    injected into the system prompt for that turn.
//!
//! 2. **Capability gap fallback** (`find-skills`): When the LLM encounters a
//!    task it can't complete with the available tools, it can call
//!    `find-skills` to discover skills that might help. This is the
//!    "能力缺口兜底" (capability gap backstop) from the 铁律 agent_skills.
//!
//! ## Integration
//!
//! The orchestration core calls [`SkillLoader::match_message`] at the start
//! of each turn with the user's message text. If a skill matches, its
//! instructions are returned and injected into the system prompt alongside
//! the existing `<available_skills>` block.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::skill_orchestrator::{SkillCommandInfo, SkillOrchestrator};

// ---------------------------------------------------------------------------
// SkillMatch
// ---------------------------------------------------------------------------

/// A skill that matched a user message.
#[derive(Debug, Clone)]
pub struct SkillMatch {
    /// The slash command key (e.g. `/commit-message`).
    pub command_key: String,
    /// The skill name.
    pub name: String,
    /// The skill description.
    pub description: String,
    /// The relevance score (0.0 to 1.0).
    pub score: f32,
    /// The full skill body (instructions to inject).
    pub body: String,
    /// Which field matched: "name", "description", or "tags".
    pub matched_on: String,
}

// ---------------------------------------------------------------------------
// SkillLoader
// ---------------------------------------------------------------------------

/// Auto-matches user messages to skills and produces injection text.
///
/// Wraps a [`SkillOrchestrator`] and adds keyword-based matching on top of
/// the slash-command resolution. The orchestrator handles discovery (scanning
/// `~/.hermix/skills/` for SKILL.md files); the loader adds the matching
/// layer.
pub struct SkillLoader {
    orchestrator: SkillOrchestrator,
    /// Minimum score threshold for a match (default 0.3).
    min_score: f32,
    /// Maximum number of auto-matched skills to inject per turn (default 3).
    max_matches: usize,
}

impl SkillLoader {
    /// Create a new loader using the default skills directory.
    pub fn new() -> Self {
        Self {
            orchestrator: SkillOrchestrator::default_dir(),
            min_score: 0.3,
            max_matches: 3,
        }
    }

    /// Create a new loader with a specific skills directory.
    pub fn with_dir(skills_dir: impl Into<PathBuf>) -> Self {
        Self {
            orchestrator: SkillOrchestrator::new(skills_dir),
            min_score: 0.3,
            max_matches: 3,
        }
    }

    /// Set the minimum score threshold.
    pub fn with_min_score(mut self, score: f32) -> Self {
        self.min_score = score;
        self
    }

    /// Set the maximum number of auto-matched skills per turn.
    pub fn with_max_matches(mut self, max: usize) -> Self {
        self.max_matches = max;
        self
    }

    /// Scan for skills (delegates to the orchestrator).
    pub fn scan(&mut self) -> &HashMap<String, SkillCommandInfo> {
        self.orchestrator.scan_skill_commands()
    }

    /// Get the underlying orchestrator.
    pub fn orchestrator(&self) -> &SkillOrchestrator {
        &self.orchestrator
    }

    /// Match a user message to skills.
    ///
    /// Returns up to `max_matches` skills whose name, description, or tags
    /// contain keywords from the message. The score is based on the ratio
    /// of matched keywords to total message keywords.
    pub fn match_message(&self, message: &str) -> Vec<SkillMatch> {
        let commands = self.orchestrator.get_commands();
        if commands.is_empty() {
            return Vec::new();
        }

        let msg_lower = message.to_lowercase();
        let msg_tokens: Vec<&str> = msg_lower
            .split_whitespace()
            .filter(|t| t.len() >= 3) // skip short tokens
            .collect();

        if msg_tokens.is_empty() {
            return Vec::new();
        }

        let mut scored: Vec<SkillMatch> = Vec::new();

        for (cmd_key, info) in commands {
            let name_lower = info.name.to_lowercase();
            let desc_lower = info.description.to_lowercase();
            let tags_lower: Vec<String> = info.tags.iter().map(|t| t.to_lowercase()).collect();

            // Score: how many message tokens appear in the skill's fields
            let mut matched_tokens = 0;
            let mut matched_on = String::new();

            for token in &msg_tokens {
                let in_name = name_lower.contains(token);
                let in_desc = desc_lower.contains(token);
                let in_tags = tags_lower.iter().any(|t| t.contains(token));

                if in_name || in_desc || in_tags {
                    matched_tokens += 1;
                    if in_name && matched_on.is_empty() {
                        matched_on = "name".into();
                    } else if in_desc && matched_on.is_empty() {
                        matched_on = "description".into();
                    } else if in_tags && matched_on.is_empty() {
                        matched_on = "tags".into();
                    }
                }
            }

            if matched_tokens == 0 {
                continue;
            }

            let score = matched_tokens as f32 / msg_tokens.len() as f32;
            if score < self.min_score {
                continue;
            }

            // Load the skill body
            let body = match std::fs::read_to_string(&info.skill_md_path) {
                Ok(content) => {
                    let (_, body) = crate::skill_orchestrator::parse_frontmatter(&content);
                    body.trim().to_string()
                }
                Err(_) => continue,
            };

            scored.push(SkillMatch {
                command_key: cmd_key.clone(),
                name: info.name.clone(),
                description: info.description.clone(),
                score,
                body,
                matched_on,
            });
        }

        // Sort by score descending, take top N
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(self.max_matches);
        scored
    }

    /// Build the injection text for matched skills.
    ///
    /// Returns `None` if no skills matched. The returned text is suitable
    /// for appending to the system prompt.
    pub fn build_injection(&self, message: &str) -> Option<String> {
        let matches = self.match_message(message);
        if matches.is_empty() {
            return None;
        }

        let mut parts = Vec::new();
        parts.push(
            "## Auto-Matched Skills (system-injected)\n\
             The following skills were automatically matched to the user's message. \
             Review their instructions and apply them if relevant to the current task.\n"
                .to_string(),
        );

        for m in &matches {
            parts.push(format!(
                "### {} (score: {:.2}, matched on: {})\n{}\n",
                m.name, m.score, m.matched_on, m.body
            ));
        }

        Some(parts.join("\n"))
    }

    /// Find skills for a capability gap.
    ///
    /// When the LLM can't complete a task with the available tools, it can
    /// describe what it needs and this function returns skills that might
    /// help. This is the "find-skills 能力缺口兜底" from 铁律 agent_skills.
    pub fn find_skills_for_capability_gap(&self, description: &str) -> Vec<SkillMatch> {
        // Reuse the same matching logic — the "capability gap description"
        // is treated like a user message for matching purposes.
        self.match_message(description)
    }

    /// Build the find-skills response text for the LLM.
    ///
    /// Returns a JSON string listing matching skills with their names,
    /// descriptions, and command keys. The LLM can then use `skill_view`
    /// to inspect a skill's full content.
    pub fn find_skills_response(&self, description: &str) -> String {
        let matches = self.find_skills_for_capability_gap(description);
        if matches.is_empty() {
            return serde_json::json!({
                "found": false,
                "message": "No skills found matching the description. Try rephrasing or use skills_list to see all available skills.",
            })
            .to_string();
        }

        let results: Vec<serde_json::Value> = matches
            .iter()
            .map(|m| {
                serde_json::json!({
                    "name": m.name,
                    "command": m.command_key,
                    "description": m.description,
                    "score": m.score,
                    "matched_on": m.matched_on,
                })
            })
            .collect();

        serde_json::json!({
            "found": true,
            "count": results.len(),
            "skills": results,
            "hint": "Use skill_view with the skill name to see full instructions.",
        })
        .to_string()
    }
}

impl Default for SkillLoader {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_test_skills() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        // Create a few test skills
        let skills = [
            (
                "commit-message",
                "Format a Conventional Commit message",
                "git commit conventional message format",
                "# Commit Message Skill\n\nWhen writing a commit message, use the Conventional Commits format.",
            ),
            (
                "code-review",
                "Review code for quality and best practices",
                "code review quality best-practices",
                "# Code Review Skill\n\nReview the code for quality, security, and best practices.",
            ),
            (
                "pdf-extract",
                "Extract text and data from PDF files",
                "pdf extract text data",
                "# PDF Extract Skill\n\nUse read_pdf to extract text from PDF files.",
            ),
        ];

        for (name, desc, tags, body) in &skills {
            let skill_dir = dir.join(name);
            fs::create_dir_all(&skill_dir).unwrap();
            let content = format!(
                "---\nname: {name}\ndescription: {desc}\ntags: [{tags}]\n---\n{body}"
            );
            fs::write(skill_dir.join("SKILL.md"), content).unwrap();
        }

        tmp
    }

    #[test]
    fn test_match_message_finds_relevant_skill() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let matches = loader.match_message("help me write a git commit message");
        assert!(!matches.is_empty());
        // The commit-message skill should be in the top matches
        let names: Vec<&str> = matches.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"commit-message"));
    }

    #[test]
    fn test_match_message_returns_empty_for_irrelevant() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let matches = loader.match_message("hello world");
        // "hello" and "world" don't match any skill
        assert!(matches.is_empty());
    }

    #[test]
    fn test_match_message_respects_max_matches() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path()).with_max_matches(1);
        loader.scan();

        let matches = loader.match_message("code review commit message");
        assert!(!matches.is_empty());
        assert!(matches.len() <= 1);
    }

    #[test]
    fn test_build_injection_returns_none_when_no_match() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let injection = loader.build_injection("hello world");
        assert!(injection.is_none());
    }

    #[test]
    fn test_build_injection_returns_text_when_matched() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let injection = loader.build_injection("help me write a commit message");
        assert!(injection.is_some());
        let text = injection.unwrap();
        assert!(text.contains("Auto-Matched Skills"));
        assert!(text.contains("commit-message"));
        assert!(text.contains("Conventional Commits"));
    }

    #[test]
    fn test_find_skills_for_capability_gap() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let matches = loader.find_skills_for_capability_gap("I need to extract text from a PDF");
        assert!(!matches.is_empty());
        let names: Vec<&str> = matches.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"pdf-extract"));
    }

    #[test]
    fn test_find_skills_response_json() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let response = loader.find_skills_response("I need to review code quality");
        let parsed: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed["found"], true);
        assert!(parsed["count"].as_u64().unwrap() > 0);
        assert!(parsed["skills"].is_array());
    }

    #[test]
    fn test_find_skills_response_empty() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let response = loader.find_skills_response("xyzzy qwerty");
        let parsed: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed["found"], false);
    }

    #[test]
    fn test_match_on_tags() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        // "best-practices" is a tag on the code-review skill
        let matches = loader.match_message("show me best-practices for coding");
        assert!(!matches.is_empty());
        let names: Vec<&str> = matches.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"code-review"));
    }

    #[test]
    fn test_score_threshold_filters_low_relevance() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path()).with_min_score(0.99);
        loader.scan();

        // Very high threshold — should filter out most matches
        let matches = loader.match_message("help me write a commit message about code review");
        // The score would be low because only a few tokens match each skill
        assert!(matches.is_empty() || matches.iter().all(|m| m.score >= 0.99));
    }

    #[test]
    fn test_empty_skills_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let matches = loader.match_message("anything at all");
        assert!(matches.is_empty());
    }

    #[test]
    fn test_nonexistent_skills_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let nonexistent = tmp.path().join("nonexistent");
        let mut loader = SkillLoader::with_dir(&nonexistent);
        loader.scan();

        let matches = loader.match_message("anything");
        assert!(matches.is_empty());
    }
}
