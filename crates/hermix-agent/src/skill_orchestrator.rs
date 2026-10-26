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
    /// `allowed-tools` from frontmatter (already kebab→snake normalized).
    /// `None` / empty = no restriction. Enforced per-turn by the loop when
    /// the skill is auto-activated (subtract-only: never expands the
    /// session toolset).
    pub allowed_tools: Option<Vec<String>>,
}

/// Parsed SKILL.md frontmatter.
/// extended with the agentskills.io open-standard
/// fields so we can ingest community SKILL.md files verbatim. Only
/// `name` and `description` are still required to be present — the
/// others are optional and gracefully degrade to `None`.
#[derive(Debug, Clone, Default)]
pub struct SkillFrontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
    pub platform: Option<Vec<String>>,
    /// agentskills.io standard fields. None when the source
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
/// extended to ingest the agentskills.io open
/// standard. We handle these shapes:
/// ```yaml
/// name: foo                       # scalar
/// description: bar                # scalar (no quotes required)
/// tags: [a, b, c]                 # inline YAML list
/// tags:                           # block YAML list
/// - a
/// - b
/// allowed-tools:                  # kebab-case alias
/// - bash
/// - read_file
/// ```
/// Comments (`# ...`) and blank lines are skipped. Empty values
/// become `None` rather than `Some("")` so callers can detect the
/// missing case without a separate `is_empty()` check.
fn parse_yaml_frontmatter(yaml: &str) -> SkillFrontmatter {
    let mut fm = SkillFrontmatter::default();

    // Block-list accumulator: when the parser sees a `key:` with an
    // empty value, it switches to "block list" mode and reads
    // following `- item` lines until a non-indented key appears.
    let mut block_list_key: Option<String> = None;

    // Folded-scalar accumulator (YAML `>` / `|`). Multi-line
    // descriptions in SKILL.md frontmatter use `description: >`
    // followed by indented lines. Without folding support those lines
    // are skipped and the description reduces to the literal ">" —
    // the skill's activation triggers are lost and health scoring
    // only grants partial description credit.
    let mut folded_key: Option<String> = None;
    let mut folded_value = String::new();

    for line in yaml.lines() {
        // Strip trailing comments — but only outside quoted values.
        // Simple heuristic: split on " #" or "  #" so URLs and
        // punctuation aren't mistaken for comments.
        let (line, _) = match line.find(" #") {
            Some(idx) => (line[..idx].to_string(), true),
            None => (line.to_string(), false),
        };
        let line = line.trim_end();

        // Folded-scalar continuation: indented lines (or blank lines
        // inside the block) accumulate into the pending key's value.
        if let Some(key) = folded_key.as_ref() {
            let is_indented = line.starts_with(' ') || line.starts_with('\t');
            if is_indented || line.is_empty() {
                if !line.is_empty() {
                    if !folded_value.is_empty() && !folded_value.ends_with('\n') {
                        folded_value.push(' ');
                    }
                    folded_value.push_str(line.trim());
                } else {
                    // Blank line inside a folded block = paragraph break.
                    folded_value.push('\n');
                }
                continue;
            }
            // First non-indented line ends the folded block — commit.
            assign_frontmatter_scalar(&mut fm, key, folded_value.trim());
            folded_key = None;
            folded_value.clear();
            // Fall through: this line is parsed as a fresh key below.
        }

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

            // YAML folded (`>`) / literal (`|`) block start — subsequent
            // indented lines belong to this key.
            if value == ">" || value == "|" {
                folded_key = Some(canonical_frontmatter_key(key));
                folded_value = String::new();
                block_list_key = None;
                continue;
            }

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
            assign_frontmatter_scalar(&mut fm, key, value);
        }
    }

    // Flush any folded block that ran to the end of the file without a
    // terminating non-indented line.
    if let Some(key) = folded_key {
        assign_frontmatter_scalar(&mut fm, &key, folded_value.trim());
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

/// Assign a scalar frontmatter value to the matching field. Centralized so
/// both the plain `key: value` path and the folded-block (`key: >` / `|`)
/// commit path write through the same mapping.
fn assign_frontmatter_scalar(fm: &mut SkillFrontmatter, key: &str, value: &str) {
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
// Scan memo
// ---------------------------------------------------------------------------

/// Fingerprint of the skill files seen by the last full scan, plus the
/// resulting command map. `prompt_assembly` builds a skills block on
/// every turn; without this the whole library is re-read and
/// re-health-scored per turn (69 SKILL.md files here), which is both
/// hot-path I/O and a WARN flood in the operator log.
///
/// Keyed on `(path, mtime, len)` so an install / removal / edit
/// rebuilds and an unchanged library does not.
static SKILL_SCAN_MEMO: std::sync::Mutex<
    Option<(Vec<(PathBuf, u64, u64)>, HashMap<String, SkillCommandInfo>)>,
> = std::sync::Mutex::new(None);

fn skill_scan_memo_get(
    fingerprint: &[(PathBuf, u64, u64)],
) -> Option<HashMap<String, SkillCommandInfo>> {
    let guard = SKILL_SCAN_MEMO.lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        Some((fp, cmds)) if fp == fingerprint => Some(cmds.clone()),
        _ => None,
    }
}

fn skill_scan_memo_put(
    fingerprint: Vec<(PathBuf, u64, u64)>,
    commands: &HashMap<String, SkillCommandInfo>,
) {
    let mut guard = SKILL_SCAN_MEMO.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some((fingerprint, commands.clone()));
}

/// Drop the memo — for tests that mutate skill files in place and
/// expect the next scan to re-read them.
#[cfg(test)]
pub fn clear_scan_memo() {
    let mut guard = SKILL_SCAN_MEMO.lock().unwrap_or_else(|e| e.into_inner());
    *guard = None;
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
    /// secondary roots scanned after `skills_dir` for
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
    /// Delegates to `hermix_config::paths::hermix_home()` so
    /// `$HERMIX_HOME` and portable mode both work. The previous
    /// implementation only read `$HOME`, which on Windows is often
    /// unset (cmd.exe), causing skills to be silently loaded from
    /// CWD instead of the user's hermix home.
    pub fn default_dir() -> Self {
        Self::new(hermix_config::paths::hermix_home().join("skills"))
    }

    /// register an additional skills root to scan in
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
    ///
    /// Memoized per skill-file fingerprint: prompt assembly calls
    /// this on **every turn**, and re-reading + re-scoring every
    /// SKILL.md there was pure hot-path I/O (69 files on this
    /// machine). The fingerprint is `(path, mtime, len)` — stat
    /// only, no file reads — so any install / removal / edit
    /// rebuilds, and a steady backlog rebuilds nothing.
    pub fn scan_skill_commands(&mut self) -> &HashMap<String, SkillCommandInfo> {
        self.commands.clear();

        // Snapshot the directories so we can drop the immutable
        // borrow on `self.extra_dirs` before mutably borrowing
        // `self` for the recursive scan.
        let primary = self.skills_dir.clone();
        let extras: Vec<PathBuf> = self.extra_dirs.clone();

        // Primary root first (commands from here always win).
        let mut files = Vec::new();
        if primary.exists() {
            Self::collect_skill_files(&primary, true, &mut files);
        }
        for extra in &extras {
            if extra.exists() {
                Self::collect_skill_files(extra, true, &mut files);
            }
        }

        let fingerprint: Vec<(PathBuf, u64, u64)> = files
            .iter()
            .map(|p| {
                let (mtime, len) = std::fs::metadata(p)
                    .ok()
                    .and_then(|m| {
                        let t = m.modified().ok()?;
                        Some((t.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos() as u64, m.len()))
                    })
                    .unwrap_or((0, 0));
                (p.clone(), mtime, len)
            })
            .collect();

        if let Some(cached) = skill_scan_memo_get(&fingerprint) {
            self.commands = cached;
            return &self.commands;
        }

        // While we have every SKILL.md in hand we also compute its
        // health score and warn on anything below 0.5. The score is
        // NOT used to gate loading — we never want a stale skill to
        // silently disappear — but the log line helps operators
        // triage their library.
        let mut seen_names = std::collections::HashSet::new();
        for skill_md in &files {
            if let Ok(content) = std::fs::read_to_string(skill_md) {
                crate::skill_health::score_skill_md(skill_md, &content);
            }
            self.register_skill(skill_md, &mut seen_names);
        }

        skill_scan_memo_put(fingerprint, &self.commands);
        &self.commands
    }


    /// Recursively collect `<dir>/<skill>/SKILL.md` paths in
    /// registration order (a directory's own SKILL.md before its
    /// children's), so first-write-wins name collisions keep the same
    /// outcome as the pre-memo depth-first walk.
    ///
    /// Hidden subdirectories are skipped below the root so a malicious
    /// / junk sibling like `.git/` can't smuggle in stray SKILL.md
    /// files. The root's own children are exempt: a root may itself
    /// live under `.agents/skills/`.
    fn collect_skill_files(dir: &Path, is_root: bool, out: &mut Vec<PathBuf>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if !is_root && name.starts_with('.') {
                    continue;
                }
            }
            if path.is_dir() {
                let skill_md = path.join("SKILL.md");
                if skill_md.is_file() {
                    out.push(skill_md);
                }
                Self::collect_skill_files(&path, false, out);
            }
        }
    }

    /// Register a single SKILL.md file.
    fn register_skill(
        &mut self,
        skill_md_path: &Path,
        seen_names: &mut std::collections::HashSet<String>,
    ) {
        // hardening: path-boundary check. The scanner
        // walks `self.skills_dir` recursively, but a malicious
        // sibling within `skills_dir/` could be a symlink that
        // points outside the tree — `entry.path()` would follow
        // it and we'd then read a SKILL.md from a totally
        // unrelated location. Reject anything not lexically
        // rooted at any configured root (primary + extras; we
        // use lexical, not canonical, to keep this cheap —
        // `walkdir` / `read_dir` already de-duped realpath
        // collisions downstream). Mirrors upstream's
        //`path_security.validate_within_dir` check.
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
                allowed_tools: frontmatter.allowed_tools,
            },
        );
    }

    /// Get the current commands mapping.
    pub fn get_commands(&self) -> &HashMap<String, SkillCommandInfo> {
        &self.commands
    }

    /// Resolve a user-typed `/command` to its canonical key.
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

        // Health warning: if the skill scores below the maintenance
        // threshold, surface it directly to the model instead of only
        // logging it. Without this the warning was invisible to the LLM —
        // it would trust a stale skill's schema and repeat its mistakes.
        if let Some(warning) = skill_health_warning(&info.skill_md_path, &content) {
            parts.push(String::new());
            parts.push(warning);
        }

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
                    let mut rendered = format!("{}\n\n{}", activation, body.trim());
                    // Same health-warning injection as the slash-command
                    // path — a low-scoring skill must not be trusted blindly.
                    if let Some(warning) = skill_health_warning(&info.skill_md_path, &content) {
                        rendered.push_str("\n\n");
                        rendered.push_str(&warning);
                    }
                    prompt_parts.push(rendered);
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

/// Compute a health warning for a SKILL.md file, if it scores below
/// the maintenance threshold (0.5).
///
/// This is the *in-context* half of the health gate: the scan-time
/// `score_skill_md` only logs a WARN line that no LLM ever sees. When
/// the skill's content is actually injected into a prompt, we re-score
/// it silently and, if it's unhealthy, tell the model explicitly to
/// distrust stale schema / field names and verify against the real tool
/// definition — and to fix the SKILL.md rather than working around it.
///
/// Returns `None` for healthy skills so healthy injections are unchanged.
fn skill_health_warning(path: &std::path::Path, content: &str) -> Option<String> {
    let breakdown = crate::skill_health::score_skill_md_silent(path, content);
    let composite = breakdown.composite();
    if composite >= 0.5 {
        return None;
    }
    // NOTE on wording: do NOT phrase this as "stop and explain / fix
    // first". 较温和的措辞曾被模型理解为"用散文描述意图而非真正发出调用"，
    // 导致无 tool_call 直接收尾，Finalize 把散文当作真实答案。
    // The wording below explicitly tells the model to KEEP making the
    // real tool_call, and only uses schema cross-check + SKILL.md fix
    // as post-call cleanup.
    Some(format!(
        "[SKILL-HEALTH-WARNING] This skill's health score is {composite:.2}/1.0, below the 0.5 maintenance threshold — its content may be OUTDATED. Field names, action enums, and examples here may NOT match the real tool definitions. Required action: (1) emit the actual tool_call now with your best-guess schema — DO NOT just describe the intended call in prose. (2) If the tool returns a validation error, cross-check the error against the tool's real schema and retry once. (3) Only after the call succeeds (or is confirmed unsalvageable) consider fixing the SKILL.md file itself."
    ))
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

    #[test]
    fn parse_folded_scalar_description() {
        // Multi-line `description: >` blocks (custom-app-development,
        // i-have-adhd, etc.) must fold into a single description string —
        // previously the parser kept only the literal ">" and the
        // activation triggers were lost.
        let content = "\
---
name: custom-app-development
description: >
  自定义应用开发助手。当用户要求创建桌面应用时使用。
  务必在以下场景使用：用户说\"做个应用\"、\"打包成 exe\"。
  或者用户描述需要独立窗口运行的功能需求时。
tags:
  - desktop
---
# Body";
        let (fm, body) = parse_frontmatter(content);
        assert_eq!(fm.name.as_deref(), Some("custom-app-development"));
        let desc = fm.description.expect("folded description parsed");
        assert!(
            !desc.contains('>'),
            "folded description must not be the literal '>': {desc}"
        );
        assert!(desc.contains("自定义应用开发助手"), "got: {desc}");
        assert!(desc.contains("做个应用"), "got: {desc}");
        assert!(desc.contains("打包成 exe"), "got: {desc}");
        // The trigger phrase is now visible to the health scorer.
        assert!(crate::skill_health::description_has_trigger(&desc));
        // Folding must not swallow the block-list tags that follow it.
        let tags = fm.tags.expect("tags parsed");
        assert_eq!(tags, vec!["desktop".to_string()]);
        assert!(body.contains("Body"));
    }

    #[test]
    fn parse_folded_scalar_at_file_end() {
        // Folded block running to the end of the frontmatter (no trailing
        // key to terminate it) must still be committed.
        let content = "\
---
name: end-fold
description: |
  line one
  line two
---
# Body";
        let (fm, _body) = parse_frontmatter(content);
        let desc = fm.description.expect("folded description at file end");
        assert!(desc.contains("line one"), "got: {desc}");
        assert!(desc.contains("line two"), "got: {desc}");
    }

    //: agentskills.io open-standard frontmatter
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
                allowed_tools: None,
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

    //multi-root skill scanning ---

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

    #[test]
    fn rescan_is_memoized_but_still_sees_edits() {
        // `prompt_assembly` rescans on every turn. The memo must not
        // freeze the library: an edited SKILL.md has to be re-read.
        clear_scan_memo();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("demo");
        fs::create_dir_all(&dir).unwrap();
        let md = dir.join("SKILL.md");
        fs::write(&md, "---\nname: demo\ndescription: first desc\n---\nbody").unwrap();

        let mut first = SkillOrchestrator::new(tmp.path());
        assert_eq!(
            first.scan_skill_commands()["/demo"].description,
            "first desc"
        );

        // Unchanged tree: a fresh orchestrator gets the memoized map.
        let mut second = SkillOrchestrator::new(tmp.path());
        assert_eq!(second.scan_skill_commands().len(), 1);
        assert_eq!(second.get_commands()["/demo"].description, "first desc");

        // Edited tree: fingerprint (path, mtime, len) changes → rebuild.
        fs::write(
            &md,
            "---\nname: demo\ndescription: second descriptive text\n---\nbody",
        )
        .unwrap();
        let mut third = SkillOrchestrator::new(tmp.path());
        assert_eq!(
            third.scan_skill_commands()["/demo"].description,
            "second descriptive text"
        );
    }
}
