//! Skill orchestrator and slash command system.
//!
//! Scans `~/.hermix/skills/` for SKILL.md files, builds a `/command` → skill
//! mapping, and provides helpers for invoking skills via slash commands and
//! preloading skills for session-wide use.
//!
//! Corresponds to Python `agent/skill_commands.py`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use regex::Regex;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Information about a registered skill command.
#[derive(Debug, Clone)]
pub struct SkillCommandInfo {
    /// Original skill name from frontmatter.
    pub name: String,
    /// Short description.
    pub description: String,
    /// Path to the SKILL.md file.
    pub skill_md_path: PathBuf,
    /// Directory containing the skill.
    pub skill_dir: PathBuf,
    /// Category from frontmatter.
    pub category: String,
    /// Tags from frontmatter.
    pub tags: Vec<String>,
}

/// Parsed SKILL.md frontmatter.
///
/// P2-8 (2026-06-27): extended with the agentskills.io open-standard
/// fields so we can ingest community SKILL.md files verbatim. Only
/// `name` and `description` are still required to be present — the
/// others are optional and gracefully degrade to `None`.
#[derive(Debug, Clone, Default)]
pub struct SkillFrontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
    pub platform: Option<Vec<String>>,
    /// P2-8: agentskills.io standard fields. None when the source
    /// SKILL.md predates the standard or uses a private format.
    pub version: Option<String>,
    pub author: Option<String>,
    pub license: Option<String>,
    pub tags: Option<Vec<String>>,
    pub allowed_tools: Option<Vec<String>>,
    pub category: Option<String>,
}

// ---------------------------------------------------------------------------
// Frontmatter parsing
// ---------------------------------------------------------------------------

/// Parse YAML frontmatter from a SKILL.md file.
///
/// Expects the file to start with `---\n...\n---\n` followed by the body.
/// Returns (frontmatter, body).
pub fn parse_frontmatter(content: &str) -> (SkillFrontmatter, &str) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (SkillFrontmatter::default(), content);
    }

    // Find the closing ---
    let after_first = &trimmed[3..];
    let after_first = after_first.trim_start_matches(['\r', '\n']);

    if let Some(end_idx) = after_first.find("\n---") {
        let yaml_block = &after_first[..end_idx];
        let body_start = end_idx + 4; // skip \n---
        let body = after_first[body_start..].trim_start_matches(['\r', '\n']);

        let fm = parse_yaml_frontmatter(yaml_block);
        (fm, body)
    } else {
        (SkillFrontmatter::default(), content)
    }
}

/// Minimal YAML frontmatter parser (no full YAML dependency needed).
///
/// P2-8 (2026-06-27): extended to ingest the agentskills.io open
/// standard. We handle these shapes:
///
/// ```yaml
/// name: foo                       # scalar
/// description: bar                # scalar (no quotes required)
/// tags: [a, b, c]                 # inline YAML list
/// tags:                           # block YAML list
///   - a
///   - b
/// allowed-tools:                  # kebab-case alias
///   - bash
///   - read_file
/// ```
///
/// Comments (`# ...`) and blank lines are skipped. Empty values
/// become `None` rather than `Some("")` so callers can detect the
/// missing case without a separate `is_empty()` check.
fn parse_yaml_frontmatter(yaml: &str) -> SkillFrontmatter {
    let mut fm = SkillFrontmatter::default();

    // Block-list accumulator: when the parser sees a `key:` with an
    // empty value, it switches to "block list" mode and reads
    // following `- item` lines until a non-indented key appears.
    let mut block_list_key: Option<String> = None;

    for line in yaml.lines() {
        // Strip trailing comments — but only outside quoted values.
        // Simple heuristic: split on " #" or "  #" so URLs and
        // punctuation aren't mistaken for comments.
        let (line, _) = match line.find(" #") {
            Some(idx) => (line[..idx].to_string(), true),
            None => (line.to_string(), false),
        };
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Block-list continuation: `- value` or indented `  - value`.
        // The raw YAML frontmatter we accept is usually indented by
        // 2 spaces (`  - foo`), but we tolerate no indent too.
        let trimmed_for_list = line.trim_start();
        if trimmed_for_list.starts_with("- ")
            || trimmed_for_list.starts_with("-\t")
            || trimmed_for_list == "-"
        {
            if let Some(key) = block_list_key.as_deref() {
                let value = trimmed_for_list[1..]
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'');
                push_block_list(&mut fm, key, value);
                continue;
            }
        }

        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim().trim_matches('"').trim_matches('\'');

            // Empty value → expect block-list mode on subsequent
            // indented lines.
            if value.is_empty() {
                block_list_key = Some(canonical_frontmatter_key(key));
                // Initialize the Vec slot so subsequent `-` lines
                // have somewhere to push.
                ensure_block_list(&mut fm, &block_list_key.clone().unwrap());
                continue;
            }

            // Inline list `[a, b, c]` for short lists.
            if value.starts_with('[') && value.ends_with(']') {
                let inner = &value[1..value.len() - 1];
                let items: Vec<String> = inner
                    .split(',')
                    .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                assign_block_list(&mut fm, key, items);
                block_list_key = None;
                continue;
            }

            // Scalar value.
            block_list_key = None;
            match canonical_frontmatter_key(key).as_str() {
                "name" => fm.name = Some(value.to_string()),
                "description" => fm.description = Some(value.to_string()),
                "version" => fm.version = Some(value.to_string()),
                "author" => fm.author = Some(value.to_string()),
                "license" => fm.license = Some(value.to_string()),
                "category" => fm.category = Some(value.to_string()),
                _ => {} // unknown keys are tolerated (forward-compat)
            }
        }
    }

    fm
}

/// Normalize kebab-case / snake_case keys to a single canonical
/// form. The agentskills.io standard uses `allowed-tools`, the
/// existing hermix skill code uses `platform` — both should land
/// in `allowed_tools`.
fn canonical_frontmatter_key(key: &str) -> String {
    key.replace('-', "_")
}

fn ensure_block_list(fm: &mut SkillFrontmatter, key: &str) {
    match key {
        "platform" if fm.platform.is_none() => fm.platform = Some(Vec::new()),
        "tags" if fm.tags.is_none() => fm.tags = Some(Vec::new()),
        "allowed_tools" if fm.allowed_tools.is_none() => fm.allowed_tools = Some(Vec::new()),
        _ => {}
    }
}

fn push_block_list(fm: &mut SkillFrontmatter, key: &str, value: &str) {
    match key {
        "platform" => fm
            .platform
            .get_or_insert_with(Vec::new)
            .push(value.to_string()),
        "tags" => fm.tags.get_or_insert_with(Vec::new).push(value.to_string()),
        "allowed_tools" => fm
            .allowed_tools
            .get_or_insert_with(Vec::new)
            .push(value.to_string()),
        _ => {}
    }
}

fn assign_block_list(fm: &mut SkillFrontmatter, key: &str, items: Vec<String>) {
    match canonical_frontmatter_key(key).as_str() {
        "platform" => fm.platform = Some(items),
        "tags" => fm.tags = Some(items),
        "allowed_tools" => fm.allowed_tools = Some(items),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Slug normalization
// ---------------------------------------------------------------------------

lazy_static::lazy_static! {
    static ref SKILL_INVALID_CHARS: Regex = Regex::new(r"[^a-z0-9-]").unwrap();
    static ref SKILL_MULTI_HYPHEN: Regex = Regex::new(r"-{2,}").unwrap();
}

/// Normalize a skill name into a clean hyphen-separated slug.
fn slugify_skill_name(name: &str) -> String {
    let lower = name.to_lowercase().replace([' ', '_'], "-");
    let cleaned = SKILL_INVALID_CHARS.replace_all(&lower, "");
    let deduped = SKILL_MULTI_HYPHEN.replace_all(&cleaned, "-");
    deduped.trim_matches('-').to_string()
}

// ---------------------------------------------------------------------------
// SkillOrchestrator
// ---------------------------------------------------------------------------

/// Manages skill discovery, slash command resolution, and skill invocation.
pub struct SkillOrchestrator {
    /// Mapping from `/command-name` to skill info.
    commands: HashMap<String, SkillCommandInfo>,
    /// Primary skills directory (typically `~/.hermix/skills/`).
    /// Always scanned first; commands from here win name conflicts
    /// against secondary dirs (first-write-wins, deterministic).
    skills_dir: PathBuf,
    /// P2-T5: secondary roots scanned after `skills_dir` for
    /// cross-tool compatibility (e.g. Open-Plugins / agentskills.io
    /// standard `.agents/skills/`, `.cursor/skills/`,
    /// `.claude/skills/`). Optional; empty by default. Duplicates
    /// by `name` are silently dropped — the same
    /// deterministic-first-wins rule as the primary scan.
    extra_dirs: Vec<PathBuf>,
}

impl SkillOrchestrator {
    /// Create a new orchestrator with the given skills directory.
    pub fn new(skills_dir: impl Into<PathBuf>) -> Self {
        Self {
            commands: HashMap::new(),
            skills_dir: skills_dir.into(),
            extra_dirs: Vec::new(),
        }
    }

    /// Create an orchestrator using the default `<HERMIX_HOME>/skills/` directory.
    ///
    /// Delegates to `hermix_config::paths::hermix_home()` so
    /// `$HERMIX_HOME` and portable mode both work. The previous
    /// implementation only read `$HOME`, which on Windows is often
    /// unset (cmd.exe), causing skills to be silently loaded from
    /// CWD instead of the user's hermix home.
    pub fn default_dir() -> Self {
        Self::new(hermix_config::paths::hermix_home().join("skills"))
    }

    /// P2-T5: register an additional skills root to scan in
    /// addition to `skills_dir`. Used by the Open-Plugins /
    /// agentskills.io compatibility layer (`~/.agents/skills/`,
    /// `~/.cursor/skills/`, `~/.claude/skills/`). Order matters:
    /// primary `skills_dir` always wins name collisions.
    pub fn with_extra_dirs<I, P>(mut self, dirs: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.extra_dirs = dirs.into_iter().map(Into::into).collect();
        self
    }

    /// Return the secondary directories that will be scanned in
    /// addition to the primary `skills_dir`.
    pub fn extra_dirs(&self) -> &[PathBuf] {
        &self.extra_dirs
    }

    /// Scan all configured roots (primary, then extras) and build
    /// the slash-command mapping. Duplicates by `name` are
    /// dropped with first-write-wins, mirroring the prior
    /// single-dir behavior exactly (no behavioral regression
    /// for callers that only used `skills_dir`).
    pub fn scan_skill_commands(&mut self) -> &HashMap<String, SkillCommandInfo> {
        self.commands.clear();

        // P2-T3: while we have every SKILL.md in hand for the scan,
        // we also compute its health score and warn on anything
        // below 0.5. The score is NOT used to gate loading — we
        // never want a stale skill to silently disappear — but the
        // log line helps operators triage their library.
        let mut seen_names = std::collections::HashSet::new();

        // Snapshot the directories so we can drop the immutable
        // borrow on `self.extra_dirs` before mutably borrowing
        // `self` for the recursive scan.
        let primary = self.skills_dir.clone();
        let extras: Vec<PathBuf> = self.extra_dirs.clone();

        // Primary root first (commands from here always win).
        if primary.exists() {
            self.scan_directory_with_health(&primary, &mut seen_names);
        }
        for extra in &extras {
            if extra.exists() {
                self.scan_directory_with_health(extra, &mut seen_names);
            }
        }

        &self.commands
    }

    /// Recursively scan a directory for SKILL.md files, scoring
    /// each one's health for log diagnostics.
    ///
    /// `dir` is the directory to scan now (we process it
    /// unconditionally, regardless of its name). Subsequent
    /// recursion skips hidden subdirectories so a malicious /
    /// junk sibling like `.git/` can't smuggle in stray SKILL.md
    /// files.
    fn scan_directory_with_health(
        &mut self,
        dir: &Path,
        seen_names: &mut std::collections::HashSet<String>,
    ) {
        self.scan_directory_inner(dir, seen_names, true);
    }

    fn scan_directory_inner(
        &mut self,
        dir: &Path,
        seen_names: &mut std::collections::HashSet<String>,
        is_root: bool,
    ) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };

            for entry in entries.flatten() {
                let path = entry.path();

                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    // Skip hidden directories at any recursion depth —
                    // we never want a `.git/` or `.trash/` to leak in
                    // SKILL.md files. The only exception is the very
                    // first level we were called with, which may
                    // legitimately live under `.agents/skills/` etc.
                    if !is_root && name.starts_with('.') {
                        continue;
                    }
                }

                if path.is_dir() {
                    let skill_md = path.join("SKILL.md");
                    if skill_md.exists() {
                        if let Ok(content) = std::fs::read_to_string(&skill_md) {
                            crate::skill_health::score_skill_md(&skill_md, &content);
                        }
                        self.register_skill(&skill_md, seen_names);
                    }
                    self.scan_directory_inner(&path, seen_names, false);
                }
            }
    }

    /// Recursively scan a directory for SKILL.md files.
    /// NOTE: superseded by `scan_directory_with_health` / `scan_directory_inner`
    /// but retained as a simpler fallback for future use.
    #[allow(dead_code)]
    fn scan_directory(&mut self, dir: &Path, seen_names: &mut std::collections::HashSet<String>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();

            // Skip hidden directories
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with('.') {
                    continue;
                }
            }

            if path.is_dir() {
                // Check for SKILL.md in this directory
                let skill_md = path.join("SKILL.md");
                if skill_md.exists() {
                    self.register_skill(&skill_md, seen_names);
                }
                // Also recurse into subdirectories
                self.scan_directory(&path, seen_names);
            }
        }
    }

    /// Register a single SKILL.md file.
    fn register_skill(
        &mut self,
        skill_md_path: &Path,
        seen_names: &mut std::collections::HashSet<String>,
    ) {
        // 2026-06-07 hardening: path-boundary check. The scanner
        // walks `self.skills_dir` recursively, but a malicious
        // sibling within `skills_dir/` could be a symlink that
        // points outside the tree — `entry.path()` would follow
        // it and we'd then read a SKILL.md from a totally
        // unrelated location. Reject anything not lexically
        // rooted at any configured root (primary + extras; we
        // use lexical, not canonical, to keep this cheap —
        // `walkdir` / `read_dir` already de-duped realpath
        // collisions downstream). Mirrors upstream's
        // `path_security.validate_within_dir` check. P2-T5
        // extended the test from primary-only to all roots so
        // Open-Plugins secondary directories register too.
        let mut allowed_root = Some(self.skills_dir.as_path());
        let extras_snapshot: Vec<&Path> =
            self.extra_dirs.iter().map(|p| p.as_path()).collect();
        let mut allowed_roots: Vec<&Path> =
            extras_snapshot.iter().copied().collect();
        allowed_roots.insert(0, allowed_root.take().unwrap());

        let mut in_any_root = false;
        for root in &allowed_roots {
            if hermix_core::path_security::path_within_root_lexical(skill_md_path, root)
                .is_none()
            {
                in_any_root = true;
                break;
            }
        }
        if !in_any_root {
            tracing::warn!(
                skill_md = %skill_md_path.display(),
                skills_dir = %self.skills_dir.display(),
                extra_dirs = ?self.extra_dirs,
                "rejecting skill: path escaped all configured roots (likely symlink attack)"
            );
            return;
        }

        let content = match std::fs::read_to_string(skill_md_path) {
            Ok(c) => c,
            Err(_) => return,
        };

        let (frontmatter, body) = parse_frontmatter(&content);

        let name = frontmatter.name.unwrap_or_else(|| {
            skill_md_path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string()
        });

        if seen_names.contains(&name) {
            return;
        }
        seen_names.insert(name.clone());

        let description = frontmatter.description.unwrap_or_else(|| {
            // Use first non-heading, non-empty line from body
            body.lines()
                .map(|l| l.trim())
                .find(|l| !l.is_empty() && !l.starts_with('#'))
                .map(|l| l.chars().take(80).collect())
                .unwrap_or_else(|| format!("Invoke the {} skill", name))
        });

        let cmd_name = slugify_skill_name(&name);
        if cmd_name.is_empty() {
            return;
        }

        let skill_dir = skill_md_path
            .parent()
            .unwrap_or(skill_md_path)
            .to_path_buf();

        let category = frontmatter.category.unwrap_or_default();
        let tags = frontmatter.tags.unwrap_or_default();

        self.commands.insert(
            format!("/{cmd_name}"),
            SkillCommandInfo {
                name,
                description,
                skill_md_path: skill_md_path.to_path_buf(),
                skill_dir,
                category,
                tags,
            },
        );
    }

    /// Get the current commands mapping.
    pub fn get_commands(&self) -> &HashMap<String, SkillCommandInfo> {
        &self.commands
    }

    /// Resolve a user-typed `/command` to its canonical key.
    ///
    /// Hyphens and underscores are treated interchangeably.
    pub fn resolve_skill_command_key(&self, command: &str) -> Option<String> {
        if command.is_empty() {
            return None;
        }
        let normalized = format!("/{}", command.trim_start_matches('/').replace('_', "-"));
        if self.commands.contains_key(&normalized) {
            Some(normalized)
        } else {
            None
        }
    }

    /// Build the user message content for a skill slash command invocation.
    pub fn build_skill_invocation_message(
        &self,
        cmd_key: &str,
        user_instruction: &str,
    ) -> Option<String> {
        let info = self.commands.get(cmd_key)?;

        let content = match std::fs::read_to_string(&info.skill_md_path) {
            Ok(c) => c,
            Err(_) => return Some(format!("[Failed to load skill: {}]", info.name)),
        };

        let (_, body) = parse_frontmatter(&content);

        let mut parts = vec![
            format!(
                "[SYSTEM: The user has invoked the \"{}\" skill, indicating they want \
                 you to follow its instructions. The full skill content is loaded below.]",
                info.name
            ),
            String::new(),
            body.trim().to_string(),
        ];

        if !user_instruction.is_empty() {
            parts.push(String::new());
            parts.push(format!(
                "The user has provided the following instruction alongside the skill invocation: {}",
                user_instruction
            ));
        }

        Some(parts.join("\n"))
    }

    /// Load multiple skills for session-wide preloading.
    ///
    /// Returns (prompt_text, loaded_skill_names, missing_identifiers).
    pub fn build_preloaded_skills_prompt(
        &self,
        identifiers: &[String],
    ) -> (String, Vec<String>, Vec<String>) {
        let mut prompt_parts = Vec::new();
        let mut loaded_names = Vec::new();
        let mut missing = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for identifier in identifiers {
            let identifier = identifier.trim();
            if identifier.is_empty() || seen.contains(identifier) {
                continue;
            }
            seen.insert(identifier.to_string());

            // Try to resolve as a command key
            let cmd_key = self.resolve_skill_command_key(identifier);
            let info = cmd_key.as_ref().and_then(|k| self.commands.get(k));

            match info {
                Some(info) => {
                    let content = match std::fs::read_to_string(&info.skill_md_path) {
                        Ok(c) => c,
                        Err(_) => {
                            missing.push(identifier.to_string());
                            continue;
                        }
                    };
                    let (_, body) = parse_frontmatter(&content);

                    let activation = format!(
                        "[SYSTEM: The user launched this CLI session with the \"{}\" skill \
                         preloaded. Treat its instructions as active guidance for the duration \
                         of this session unless the user overrides them.]",
                        info.name
                    );
                    prompt_parts.push(format!("{}\n\n{}", activation, body.trim()));
                    loaded_names.push(info.name.clone());
                }
                None => {
                    missing.push(identifier.to_string());
                }
            }
        }

        (prompt_parts.join("\n\n"), loaded_names, missing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_parse_frontmatter_with_yaml() {
        let content = "---\nname: my-skill\ndescription: A test skill\n---\n# Body\nHello world";
        let (fm, body) = parse_frontmatter(content);
        assert_eq!(fm.name.as_deref(), Some("my-skill"));
        assert_eq!(fm.description.as_deref(), Some("A test skill"));
        assert!(body.contains("Hello world"));
    }

    #[test]
    fn test_parse_frontmatter_without_yaml() {
        let content = "# Just a body\nNo frontmatter here.";
        let (fm, body) = parse_frontmatter(content);
        assert!(fm.name.is_none());
        assert!(body.contains("Just a body"));
    }

    // P2-8 (2026-06-27): agentskills.io open-standard frontmatter
    // fields. We test the new scalars + block-list shapes in isolation
    // so a refactor can't silently drop a field that community skills
    // depend on.

    #[test]
    fn p2_8_parse_agentskills_scalar_fields() {
        let content = "\
---
name: commit-message
description: Format a Conventional Commit message
version: 1.2.0
author: alice@example.com
license: MIT
category: git
---
# Body";
        let (fm, body) = parse_frontmatter(content);
        assert_eq!(fm.name.as_deref(), Some("commit-message"));
        assert_eq!(
            fm.description.as_deref(),
            Some("Format a Conventional Commit message")
        );
        assert_eq!(fm.version.as_deref(), Some("1.2.0"));
        assert_eq!(fm.author.as_deref(), Some("alice@example.com"));
        assert_eq!(fm.license.as_deref(), Some("MIT"));
        assert_eq!(fm.category.as_deref(), Some("git"));
        assert!(body.contains("Body"));
        // These came in as scalars so the list-shaped fields stay None.
        assert!(fm.tags.is_none());
        assert!(fm.allowed_tools.is_none());
        assert!(fm.platform.is_none());
    }

    #[test]
    fn p2_8_parse_block_list_tags() {
        let content = "\
---
name: cli-tools
description: A meta-skill
tags:
  - cli
  - rust
  - productivity
---
# Body";
        let (fm, _body) = parse_frontmatter(content);
        let expected: Vec<String> = ["cli", "rust", "productivity"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(fm.tags.as_deref(), Some(expected.as_slice()));
    }

    #[test]
    fn p2_8_parse_inline_list_tags() {
        let content = "---\nname: a\ndescription: b\ntags: [x, y, z]\n---\n# body";
        let (fm, _body) = parse_frontmatter(content);
        let expected: Vec<String> = ["x", "y", "z"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(fm.tags.as_deref(), Some(expected.as_slice()));
    }

    #[test]
    fn p2_8_parse_allowed_tools_with_kebab_alias() {
        // The agentskills.io standard uses `allowed-tools` (kebab).
        // We canonicalise to `allowed_tools` (snake) but the parser
        // must accept the kebab form verbatim.
        let content = "\
---
name: safe-shell
description: constrained shell
allowed-tools:
  - terminal
  - read_file
---
# Body";
        let (fm, _body) = parse_frontmatter(content);
        let expected: Vec<String> = ["terminal", "read_file"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(fm.allowed_tools.as_deref(), Some(expected.as_slice()));
    }

    #[test]
    fn p2_8_parse_strips_trailing_comments() {
        // A comment after a value must not bleed into the parsed
        // content. We accept " #" as the comment marker (leading
        // space) so URLs and paths that contain "#" don't get
        // truncated.
        let content = "---\nname: foo # the foo skill\ndescription: bar\n---\nbody";
        let (fm, _body) = parse_frontmatter(content);
        assert_eq!(fm.name.as_deref(), Some("foo"));
        assert_eq!(fm.description.as_deref(), Some("bar"));
    }

    #[test]
    fn p2_8_unknown_fields_are_tolerated() {
        // Forward-compat: a future agentskills.io field shouldn't
        // crash the parser. We just leave it on the floor.
        let content = "---\nname: x\ndescription: y\nfuture_field: 42\n---\nbody";
        let (fm, _body) = parse_frontmatter(content);
        assert_eq!(fm.name.as_deref(), Some("x"));
    }

    #[test]
    fn test_slugify_skill_name() {
        assert_eq!(slugify_skill_name("My Cool Skill"), "my-cool-skill");
        assert_eq!(slugify_skill_name("claude_code"), "claude-code");
        assert_eq!(slugify_skill_name("gif+search"), "gifsearch");
        assert_eq!(slugify_skill_name("---test---"), "test");
    }

    #[test]
    fn test_resolve_skill_command_key() {
        let mut orch = SkillOrchestrator::new("/tmp/nonexistent");
        orch.commands.insert(
            "/my-skill".to_string(),
            SkillCommandInfo {
                name: "my-skill".to_string(),
                description: "test".to_string(),
                skill_md_path: PathBuf::from("/tmp/skill/SKILL.md"),
                skill_dir: PathBuf::from("/tmp/skill"),
                category: "test".to_string(),
                tags: vec![],
            },
        );

        // Exact match
        assert_eq!(
            orch.resolve_skill_command_key("my-skill"),
            Some("/my-skill".to_string())
        );
        // Underscore → hyphen normalization
        assert_eq!(
            orch.resolve_skill_command_key("my_skill"),
            Some("/my-skill".to_string())
        );
        // With leading slash
        assert_eq!(
            orch.resolve_skill_command_key("/my-skill"),
            Some("/my-skill".to_string())
        );
        // Non-existent
        assert!(orch.resolve_skill_command_key("nonexistent").is_none());
        // Empty
        assert!(orch.resolve_skill_command_key("").is_none());
    }

    #[test]
    fn test_scan_skill_commands() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("test-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: test-skill\ndescription: A test\n---\n# Test\nDo the thing.",
        )
        .unwrap();

        let mut orch = SkillOrchestrator::new(tmp.path());
        orch.scan_skill_commands();

        assert!(orch.commands.contains_key("/test-skill"));
        let info = &orch.commands["/test-skill"];
        assert_eq!(info.name, "test-skill");
        assert_eq!(info.description, "A test");
    }

    #[test]
    fn test_build_skill_invocation_message() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("greet");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: greet\ndescription: Greeting skill\n---\nSay hello to the user.",
        )
        .unwrap();

        let mut orch = SkillOrchestrator::new(tmp.path());
        orch.scan_skill_commands();

        let msg = orch
            .build_skill_invocation_message("/greet", "in French")
            .unwrap();
        assert!(msg.contains("greet"));
        assert!(msg.contains("Say hello to the user."));
        assert!(msg.contains("in French"));
    }

    #[test]
    fn test_build_preloaded_skills_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("coder");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: coder\ndescription: Coding assistant\n---\nWrite clean code.",
        )
        .unwrap();

        let mut orch = SkillOrchestrator::new(tmp.path());
        orch.scan_skill_commands();

        let (prompt, loaded, missing) =
            orch.build_preloaded_skills_prompt(&["coder".to_string(), "nonexistent".to_string()]);

        assert_eq!(loaded, vec!["coder"]);
        assert_eq!(missing, vec!["nonexistent"]);
        assert!(prompt.contains("Write clean code."));
        assert!(prompt.contains("preloaded"));
    }

    // --- P2-T5: multi-root skill scanning ---

    #[test]
    fn p2_t5_scan_merges_primary_and_extra_dirs_with_first_wins() {
        // Simulates the open-plugins layout: skills live under
        // both `~/.hermix/skills/` AND `~/.agents/skills/` and we
        // want both to come through, with the primary taking
        // precedence on name collisions.
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("primary");
        let extra = tmp.path().join(".agents").join("skills");
        fs::create_dir_all(&primary).unwrap();
        fs::create_dir_all(&extra).unwrap();

        // Primary has "shared" with description "First instance".
        let shared_dir = primary.join("shared");
        fs::create_dir_all(&shared_dir).unwrap();
        fs::write(
            shared_dir.join("SKILL.md"),
            "---\nname: shared\ndescription: First instance\n---\nfirst body",
        )
        .unwrap();

        // Extra has its own "shared" (later-suffix) plus a unique
        // "only-in-extra" skill.
        let shared_extra = extra.join("shared");
        let only_extra = extra.join("only-in-extra");
        fs::create_dir_all(&shared_extra).unwrap();
        fs::create_dir_all(&only_extra).unwrap();
        fs::write(
            shared_extra.join("SKILL.md"),
            "---\nname: shared\ndescription: Second instance\n---\nsecond body",
        )
        .unwrap();
        fs::write(
            only_extra.join("SKILL.md"),
            "---\nname: only-in-extra\ndescription: Lives in extras\n---\nextra body",
        )
        .unwrap();

        let mut orch = SkillOrchestrator::new(&primary)
            .with_extra_dirs([extra.clone()]);
        orch.scan_skill_commands();

        // Both skills are registered.
        assert!(orch.get_commands().contains_key("/shared"));
        assert!(orch.get_commands().contains_key("/only-in-extra"));

        // First-write-wins: the primary `shared` description
        // survives; the extra's duplicate is dropped silently so
        // the deterministic rule never depends on filesystem order.
        let shared_info = &orch.get_commands()["/shared"];
        assert_eq!(
            shared_info.description, "First instance",
            "extra-dir duplicate must not overwrite the primary skill"
        );
        // And the primary root is the recorded `skills_dir`.
        assert_eq!(orch.extra_dirs(), &[extra]);
    }

    #[test]
    fn p2_t5_scan_skips_missing_extra_dirs() {
        // Operator opted an extra directory in that hasn't been
        // created yet — we silently skip it instead of failing.
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("primary");
        fs::create_dir_all(&primary).unwrap();
        let primary_skill = primary.join("lonely");
        fs::create_dir_all(&primary_skill).unwrap();
        fs::write(
            primary_skill.join("SKILL.md"),
            "---\nname: lonely\ndescription: Primary-only\n---\nbody",
        )
        .unwrap();

        let nonexistent = tmp.path().join("does-not-exist-yet");
        let mut orch = SkillOrchestrator::new(&primary)
            .with_extra_dirs([nonexistent.clone()]);
        orch.scan_skill_commands();

        assert_eq!(orch.get_commands().len(), 1);
        assert!(orch.get_commands().contains_key("/lonely"));
        assert_eq!(orch.extra_dirs(), &[nonexistent]);
    }
}
