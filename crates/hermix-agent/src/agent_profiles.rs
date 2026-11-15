//! Declarative sub-agent profiles (ZCode `core/src/subagent/*.md` analog).
//!
//! A profile is a markdown file under `<hermix_home>/agents/<name>.md`:
//!
//! ```markdown
//! ---
//! name: explore
//! description: Read-only exploration — returns findings, never edits.
//! role: explore          # explore | plan | general (default general)
//! max_turns: 8
//! tools: read_file, search_files, web_search
//! background: true       # hint: prefer background delegation
//! ---
//! You are a read-only explorer. Never modify files…
//! ```
//!
//! The profile shapes a `delegate_task` call in three ways:
//! 1. `role` picks the existing role whitelist (explore is physically
//!    read-only: blocked tools are removed from the child registry);
//! 2. `max_turns` becomes the child's iteration budget when the caller did
//!    not pass `max_iterations`;
//! 3. the body is prepended to the child's `context` (its system prompt
//!    fragment) — that is where the "how to behave" text lives.
//!
//! `tools` is an additional *declaration* (narrowing hint): the role
//! whitelist is the enforcement, and a listed tool that the role forbids
//! stays forbidden. We never widen access from a file on disk.

use std::path::{Path, PathBuf};

use crate::sub_agent_orchestrator::SubAgentRole;

/// One profile, parsed from markdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProfile {
    pub name: String,
    pub description: String,
    pub role: SubAgentRole,
    pub max_turns: Option<u32>,
    pub tools: Option<Vec<String>>,
    pub background: bool,
    /// Body (after the frontmatter) — appended to the child's context.
    pub body: String,
}

/// `<home>/agents` — created lazily by the app on first launch.
pub fn profiles_dir(home: &Path) -> PathBuf {
    home.join("agents")
}

/// Load every `*.md` profile in `dir`. Unparsable files are skipped with a
/// debug line — a broken profile must never break delegation.
pub fn load_profiles(dir: &Path) -> Vec<AgentProfile> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let fallback = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("profile")
            .to_string();
        match parse_profile(&raw, &fallback) {
            Some(profile) => out.push(profile),
            None => tracing::debug!(path = %path.display(), "skipping unparsable agent profile"),
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Look up one profile by name (case-insensitive).
pub fn find_profile(dir: &Path, name: &str) -> Option<AgentProfile> {
    let wanted = name.trim().to_ascii_lowercase();
    load_profiles(dir)
        .into_iter()
        .find(|p| p.name.to_ascii_lowercase() == wanted)
}

/// Parse `frontmatter + body`. `None` only when the file has no frontmatter
/// block at all (a missing `name` falls back to the file stem).
fn parse_profile(raw: &str, fallback_name: &str) -> Option<AgentProfile> {
    let mut lines = raw.lines();
    // Strip a UTF-8 BOM: Windows editors write one, and a BOM would make the
    // opening `---` fail to match — the profile would be silently ignored.
    if lines.next()?.trim().trim_start_matches('\u{feff}') != "---" {
        return None;
    }
    let mut name: Option<String> = None;
    let mut description = String::new();
    let mut role = SubAgentRole::General;
    let mut max_turns = None;
    let mut tools = None;
    let mut background = false;
    let mut body_start = None;

    let mut current_key = String::new();
    for (offset, line) in lines.enumerate() {
        if line.trim() == "---" {
            body_start = Some(offset);
            break;
        }
        let trimmed = line.trim();
        // Simple `key: value` frontmatter (multi-line values are not needed
        // by profiles; descriptions stay on one line).
        if let Some((key, value)) = trimmed.split_once(':') {
            let key = key.trim().to_ascii_lowercase();
            // Strip an inline YAML comment (`role: explore  # note`) before
            // parsing — otherwise the documented example profile itself
            // parses wrong (role→General, max_turns→None, background→false).
            let value = value
                .split(" #")
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches('"')
                .trim_matches('\'');
            match key.as_str() {
                "name" => name = Some(value.to_string()),
                "description" => description = value.to_string(),
                "role" => {
                    role = match value.to_ascii_lowercase().as_str() {
                        "explore" => SubAgentRole::Explore,
                        "plan" => SubAgentRole::Plan,
                        _ => SubAgentRole::General,
                    }
                }
                "max_turns" | "max-turns" | "max_iterations" => {
                    max_turns = value.parse::<u32>().ok()
                }
                "background" => background = value.eq_ignore_ascii_case("true"),
                "tools" => {
                    let list: Vec<String> = value
                        .trim_start_matches('[')
                        .trim_end_matches(']')
                        .split(',')
                        .map(|t| t.trim().to_string())
                        .filter(|t| !t.is_empty())
                        .collect();
                    if !list.is_empty() {
                        tools = Some(list);
                    }
                }
                _ => {}
            }
            current_key = key;
        } else if !trimmed.is_empty() && current_key == "description" {
            // Folded description continuation.
            description.push(' ');
            description.push_str(trimmed);
        }
    }

    let body = match body_start {
        Some(idx) => raw
            .lines()
            .skip(idx + 2) // frontmatter opening + body_start line
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string(),
        None => return None,
    };

    Some(AgentProfile {
        name: name.unwrap_or_else(|| fallback_name.to_string()),
        description,
        role,
        max_turns,
        tools,
        background,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPLORE: &str = "---\nname: explore\n\
        description: Read-only exploration — returns findings, never edits.\n\
        role: explore\nmax_turns: 8\ntools: read_file, search_files\n\
        background: true\n---\nYou are a read-only explorer. Never modify files.";

    #[test]
    fn parses_frontmatter_and_body() {
        let p = parse_profile(EXPLORE, "fallback").expect("parses");
        assert_eq!(p.name, "explore");
        assert!(p.description.starts_with("Read-only exploration"));
        assert_eq!(p.role, SubAgentRole::Explore);
        assert_eq!(p.max_turns, Some(8));
        assert_eq!(
            p.tools,
            Some(vec!["read_file".to_string(), "search_files".to_string()])
        );
        assert!(p.background);
        assert!(p.body.contains("Never modify files"));
    }

    #[test]
    fn defaults_are_safe() {
        let raw = "---\nname: plain\n---\nbody";
        let p = parse_profile(raw, "x").expect("parses");
        assert_eq!(p.role, SubAgentRole::General);
        assert_eq!(p.max_turns, None);
        assert!(!p.background);
        assert_eq!(p.body, "body");
    }

    #[test]
    fn missing_frontmatter_is_rejected() {
        assert!(parse_profile("just text", "x").is_none());
        assert!(parse_profile("---\nno name here\n---\nbody", "fallback").is_some());
    }

    #[test]
    fn find_profile_is_case_insensitive() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path()).unwrap();
        std::fs::write(tmp.path().join("explore.md"), EXPLORE).unwrap();
        assert!(find_profile(tmp.path(), "EXPLORE").is_some());
        assert!(find_profile(tmp.path(), "missing").is_none());
    }
}
