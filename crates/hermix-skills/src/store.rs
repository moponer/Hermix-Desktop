//! Local skill storage: the `SkillStore` trait and `FileSkillStore` implementation.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tracing::{debug, instrument};

use hermix_core::types::{Skill, SkillMeta};

use crate::skill::SkillError;

// ---------------------------------------------------------------------------
// SkillStore trait
// ---------------------------------------------------------------------------

/// Abstraction over skill persistence backends.
#[async_trait]
pub trait SkillStore: Send + Sync {
    /// Persist a skill. Creates or overwrites.
    async fn save(&self, skill: &Skill) -> Result<(), SkillError>;

    /// Load a skill by name. Returns `None` if not found.
    async fn load(&self, name: &str) -> Result<Option<Skill>, SkillError>;

    /// List metadata for all stored skills.
    async fn list(&self) -> Result<Vec<SkillMeta>, SkillError>;

    /// Delete a skill by name. Succeeds even if the skill didn't exist.
    async fn delete(&self, name: &str) -> Result<(), SkillError>;

    /// Disable a skill by moving its directory to a `.disabled/` sibling.
    async fn disable(&self, name: &str) -> Result<(), SkillError>;

    /// Enable a skill by moving it back from `.disabled/`.
    async fn enable(&self, name: &str) -> Result<(), SkillError>;
}

// ---------------------------------------------------------------------------
// YAML frontmatter
// ---------------------------------------------------------------------------

/// The frontmatter we write into / parse from `SKILL.md`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct SkillFrontmatter {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<String>,
}

// ---------------------------------------------------------------------------
// FileSkillStore
// ---------------------------------------------------------------------------

/// File-system backed skill store.
/// Skills are stored as `<skills_dir>/<category>/<name>/SKILL.md`.
/// Each file contains a YAML frontmatter block followed by the skill
/// content in Markdown.
pub struct FileSkillStore {
    skills_dir: PathBuf,
}

impl FileSkillStore {
    /// Create a new store rooted at `skills_dir`.
    pub fn new(skills_dir: PathBuf) -> Self {
        Self { skills_dir }
    }

    /// Return the default skills directory.
    /// Resolves to `$HERMIX_PORTABLE_DATA_DIR/skills`, `$HERMIX_HOME/skills`,
    /// or `~/.hermix/skills` — same precedence chain as `hermix_home()`.
    pub fn default_dir() -> PathBuf {
        hermix_config::paths::hermix_home().join("skills")
    }

    /// Compute the directory path for a given skill name and optional category.
    fn skill_dir(&self, name: &str, category: Option<&str>) -> PathBuf {
        match category {
            Some(cat) => self.skills_dir.join(cat).join(name),
            None => self.skills_dir.join(name),
        }
    }

    /// Write a `SKILL.md` file with frontmatter + content.
    fn render_skill_file(fm: &SkillFrontmatter, content: &str) -> String {
        let yaml = serde_yaml::to_string(fm).unwrap_or_default();
        // serde_yaml adds a leading "---\n" we need to strip the first line
        // and add our own delimiters.
        let yaml_body = yaml.trim_start_matches("---\n").trim_end();
        format!("---\n{}\n---\n{}", yaml_body, content)
    }

    /// Parse a `SKILL.md` file, extracting frontmatter and body.
    fn parse_skill_file(raw: &str) -> Result<(SkillFrontmatter, String), SkillError> {
        // We expect the file to start with "---\n" and end the frontmatter
        // with another "---\n".
        if !raw.starts_with("---") {
            return Err(SkillError::Parse(
                "Skill file must start with YAML frontmatter".to_string(),
            ));
        }

        let rest = &raw[3..]; // skip first "---"
                              // Find the closing "---"
        let end = rest
            .find("\n---")
            .ok_or_else(|| SkillError::Parse("Missing closing --- in frontmatter".to_string()))?;

        let yaml_str = &rest[..end];
        let body_start = end + 4; // skip "\n---"
        let body = rest[body_start..].trim_start_matches('\n').to_string();

        let fm: SkillFrontmatter =
            serde_yaml::from_str(yaml_str).map_err(|e| SkillError::Parse(e.to_string()))?;

        Ok((fm, body))
    }
}

#[async_trait]
impl SkillStore for FileSkillStore {
    #[instrument(skip(self, skill), fields(name = %skill.name))]
    async fn save(&self, skill: &Skill) -> Result<(), SkillError> {
        let dir = self.skill_dir(&skill.name, skill.category.as_deref());
        fs::create_dir_all(&dir)
            .await
            .map_err(|e| SkillError::Io(e.to_string()))?;

        let fm = SkillFrontmatter {
            name: skill.name.clone(),
            category: skill.category.clone(),
            description: skill.description.clone(),
            version: Some(crate::version::compute_version(&skill.content)),
        };

        let content = Self::render_skill_file(&fm, &skill.content);
        let path = dir.join("SKILL.md");

        debug!("Writing skill file: {}", path.display());
        fs::write(&path, content)
            .await
            .map_err(|e| SkillError::Io(e.to_string()))?;

        Ok(())
    }

    #[instrument(skip(self), fields(name = %name))]
    async fn load(&self, name: &str) -> Result<Option<Skill>, SkillError> {
        // Search in all category subdirectories and the root.
        let candidates = self.candidate_dirs(name).await?;

        for dir in candidates {
            let path = dir.join("SKILL.md");
            if path.exists() {
                let raw = fs::read_to_string(&path)
                    .await
                    .map_err(|e| SkillError::Io(e.to_string()))?;

                let (fm, content) = Self::parse_skill_file(&raw)?;

                return Ok(Some(Skill {
                    name: fm.name,
                    content,
                    category: fm.category,
                    description: fm.description,
                }));
            }
        }

        Ok(None)
    }

    #[instrument(skip(self))]
    async fn list(&self) -> Result<Vec<SkillMeta>, SkillError> {
        let mut metas = Vec::new();

        // Ensure root exists.
        if !self.skills_dir.exists() {
            return Ok(metas);
        }

        self.collect_metas(&self.skills_dir, &mut metas).await?;
        Ok(metas)
    }

    #[instrument(skip(self), fields(name = %name))]
    async fn delete(&self, name: &str) -> Result<(), SkillError> {
        let candidates = self.candidate_dirs(name).await?;

        for dir in candidates {
            let path = dir.join("SKILL.md");
            if path.exists() {
                // Remove the whole skill directory.
                fs::remove_dir_all(&dir)
                    .await
                    .map_err(|e| SkillError::Io(e.to_string()))?;
                return Ok(());
            }
        }

        // Idempotent: deleting a non-existent skill is fine.
        Ok(())
    }

    /// Disable a skill by moving its directory to a `.disabled/` sibling.
    /// This preserves the skill files so they can be re-enabled later.
    async fn disable(&self, name: &str) -> Result<(), SkillError> {
        let candidates = self.candidate_dirs(name).await?;

        for dir in candidates {
            let path = dir.join("SKILL.md");
            if path.exists() {
                // Build the disabled path: <parent>/.disabled/<name>/
                let parent = dir.parent().unwrap_or(&self.skills_dir);
                let disabled_dir = parent.join(".disabled").join(
                    dir.file_name().and_then(|n| n.to_str()).unwrap_or(name),
                );

                // Create the .disabled directory if it doesn't exist
                fs::create_dir_all(&disabled_dir)
                    .await
                    .map_err(|e| SkillError::Io(e.to_string()))?;

                // Move the skill directory to .disabled/
                let dest = disabled_dir.join(
                    dir.file_name().and_then(|n| n.to_str()).unwrap_or(name),
                );
                fs::rename(&dir, &dest)
                    .await
                    .map_err(|e| SkillError::Io(e.to_string()))?;

                tracing::info!("Disabled skill '{}' (moved to {})", name, dest.display());
                return Ok(());
            }
        }

        // Idempotent: disabling a non-existent skill is fine.
        Ok(())
    }

    /// Enable a skill by moving it back from `.disabled/`.
    async fn enable(&self, name: &str) -> Result<(), SkillError> {
        // Search for the skill in .disabled directories
        if self.skills_dir.exists() {
            let mut entries = fs::read_dir(&self.skills_dir)
                .await
                .map_err(|e| SkillError::Io(e.to_string()))?;

            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|e| SkillError::Io(e.to_string()))?
            {
                let path = entry.path();
                if !path.is_dir() || !path.file_name().map_or(false, |n| n == ".disabled") {
                    continue;
                }

                // Check for the skill in this .disabled directory
                let disabled_skill_dir = path.join(name);
                let skill_md = disabled_skill_dir.join("SKILL.md");
                if skill_md.exists() {
                    // Move back to parent directory
                    let parent = path.parent().unwrap_or(&self.skills_dir);
                    let dest = parent.join(
                        disabled_skill_dir.file_name().and_then(|n| n.to_str()).unwrap_or(name),
                    );

                    fs::rename(&disabled_skill_dir, &dest)
                        .await
                        .map_err(|e| SkillError::Io(e.to_string()))?;

                    tracing::info!("Enabled skill '{}' (moved from {})", name, disabled_skill_dir.display());
                    return Ok(());
                }
            }
        }

        // Idempotent: enabling a non-existent skill is fine.
        Ok(())
    }
}

impl FileSkillStore {
    /// Build a list of candidate directories where a skill named `name`
    /// might live (root + any category subdirectory).
    async fn candidate_dirs(&self, name: &str) -> Result<Vec<PathBuf>, SkillError> {
        let mut dirs = vec![self.skills_dir.join(name)];

        if self.skills_dir.exists() {
            let mut entries = fs::read_dir(&self.skills_dir)
                .await
                .map_err(|e| SkillError::Io(e.to_string()))?;

            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|e| SkillError::Io(e.to_string()))?
            {
                let path = entry.path();
                if path.is_dir() {
                    let candidate = path.join(name);
                    dirs.push(candidate);
                }
            }
        }

        Ok(dirs)
    }

    /// Recursively collect [`SkillMeta`] from all `SKILL.md` files under
    /// `dir`. The `relative` prefix is used to reconstruct categories.
    fn collect_metas<'a>(
        &'a self,
        dir: &'a Path,
        metas: &'a mut Vec<SkillMeta>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), SkillError>> + Send + 'a>>
    {
        Box::pin(async move {
            if !dir.exists() {
                return Ok(());
            }

            let mut entries = fs::read_dir(dir)
                .await
                .map_err(|e| SkillError::Io(e.to_string()))?;

            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|e| SkillError::Io(e.to_string()))?
            {
                let path = entry.path();

                // If we find a SKILL.md, parse its frontmatter for meta.
                if path.is_dir() {
                    let skill_file = path.join("SKILL.md");
                    if skill_file.exists() {
                        let raw = fs::read_to_string(&skill_file)
                            .await
                            .map_err(|e| SkillError::Io(e.to_string()))?;

                        match Self::parse_skill_file(&raw) {
                            Ok((fm, _)) => {
                                // Derive category from the path relative to skills_dir.
                                let category = fm.category.or_else(|| {
                                    path.parent()
                                        .and_then(|p| p.strip_prefix(&self.skills_dir).ok())
                                        .and_then(|rel| {
                                            let s = rel.to_string_lossy().to_string();
                                            if s.is_empty() {
                                                None
                                            } else {
                                                Some(s)
                                            }
                                        })
                                });

                                metas.push(SkillMeta {
                                    name: fm.name,
                                    category,
                                    description: fm.description,
                                });
                            }
                            Err(e) => {
                                tracing::warn!("Failed to parse {}: {}", skill_file.display(), e);
                            }
                        }
                    } else {
                        // Recurse into subdirectories.
                        self.collect_metas(&path, metas).await?;
                    }
                }
            }

            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// SkillWatcher: file system watcher for SKILL.md changes
// ---------------------------------------------------------------------------

/// Event emitted when a SKILL.md file is created, modified, or deleted.
#[derive(Debug, Clone)]
pub enum SkillEvent {
    /// A skill was created or modified.
    Changed {
        /// The skill name (derived from the directory name).
        name: String,
        /// The category (parent directory name), if any.
        category: Option<String>,
        /// The full path to the SKILL.md file.
        path: PathBuf,
    },
    /// A skill was deleted.
    Deleted {
        /// The skill name.
        name: String,
        /// The category, if any.
        category: Option<String>,
    },
}

/// Callback type for skill events.
pub type SkillEventCallback = Box<dyn Fn(SkillEvent) + Send + Sync>;

/// Watches a skills directory for SKILL.md changes and emits events.
/// Uses the `notify` crate for cross-platform file system watching.
/// The watcher runs in a background thread and emits events via a callback.
pub struct SkillWatcher {
    _watcher: notify::RecommendedWatcher,
}

impl SkillWatcher {
    /// Create a new watcher for the given skills directory.
    /// The `callback` is called whenever a SKILL.md file is created,
    /// modified, or deleted. The watcher runs in a background thread.
    pub fn new(
        skills_dir: PathBuf,
        callback: SkillEventCallback,
    ) -> Result<Self, SkillError> {
        use notify::{Event, RecursiveMode, Watcher};

        let skills_dir_clone = skills_dir.clone();
        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            match res {
                Ok(event) => {
                    Self::handle_event(&skills_dir_clone, &event, &callback);
                }
                Err(e) => {
                    tracing::warn!("File watch error: {}", e);
                }
            }
        })
        .map_err(|e| SkillError::Io(format!("Failed to create file watcher: {}", e)))?;

        watcher
            .watch(&skills_dir, RecursiveMode::Recursive)
            .map_err(|e| SkillError::Io(format!("Failed to watch skills directory: {}", e)))?;

        tracing::info!("Watching skills directory: {}", skills_dir.display());
        Ok(Self { _watcher: watcher })
    }

    /// Handle a file system event, filtering for SKILL.md changes.
    fn handle_event(
        skills_dir: &Path,
        event: &notify::Event,
        callback: &SkillEventCallback,
    ) {
        use notify::EventKind;

        // Only process create, modify, and remove events
        let is_relevant = matches!(
            event.kind,
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
        );

        if !is_relevant {
            return;
        }

        for path in &event.paths {
            // Check if this is a SKILL.md file
            if path.file_name().and_then(|n| n.to_str()) != Some("SKILL.md") {
                continue;
            }

            // Extract skill name and category from path
            // Path format: <skills_dir>/<category>/<name>/SKILL.md
            // or: <skills_dir>/<name>/SKILL.md
            let relative = match path.strip_prefix(skills_dir) {
                Ok(r) => r,
                Err(_) => continue,
            };

            let components: Vec<_> = relative.components().collect();
            if components.len() < 2 {
                continue; // Need at least <name>/SKILL.md
            }

            let skill_name = components[components.len() - 2]
                .as_os_str()
                .to_string_lossy()
                .to_string();

            let category = if components.len() >= 3 {
                Some(components[components.len() - 3].as_os_str().to_string_lossy().to_string())
            } else {
                None
            };

            let event = match event.kind {
                EventKind::Create(_) | EventKind::Modify(_) => {
                    tracing::debug!(name = %skill_name, "Skill file changed");
                    SkillEvent::Changed {
                        name: skill_name,
                        category,
                        path: path.clone(),
                    }
                }
                EventKind::Remove(_) => {
                    tracing::debug!(name = %skill_name, "Skill file removed");
                    SkillEvent::Deleted {
                        name: skill_name,
                        category,
                    }
                }
                _ => continue,
            };

            callback(event);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_render_and_parse_frontmatter() {
        let fm = SkillFrontmatter {
            name: "my-skill".to_string(),
            category: Some("general".to_string()),
            description: Some("A test skill".to_string()),
            version: Some("0.1.0".to_string()),
        };
        let content = "# My Skill\nDo the thing.";
        let rendered = FileSkillStore::render_skill_file(&fm, content);

        let (parsed_fm, parsed_body) = FileSkillStore::parse_skill_file(&rendered).unwrap();
        assert_eq!(parsed_fm.name, "my-skill");
        assert_eq!(parsed_fm.category, Some("general".to_string()));
        assert_eq!(parsed_fm.description, Some("A test skill".to_string()));
        assert!(parsed_body.contains("Do the thing"));
    }

    #[tokio::test]
    async fn test_save_and_load_skill() {
        let dir = tempdir().unwrap();
        let store = FileSkillStore::new(dir.path().to_path_buf());

        let skill = Skill {
            name: "greet".to_string(),
            content: "# Greet\nSay hello.".to_string(),
            category: Some("social".to_string()),
            description: Some("Greets people".to_string()),
        };

        store.save(&skill).await.unwrap();
        let loaded = store.load("greet").await.unwrap().unwrap();

        assert_eq!(loaded.name, "greet");
        assert_eq!(loaded.category, Some("social".to_string()));
        assert!(loaded.content.contains("Say hello."));
    }

    #[tokio::test]
    async fn test_list_skills() {
        let dir = tempdir().unwrap();
        let store = FileSkillStore::new(dir.path().to_path_buf());

        let skill_a = Skill {
            name: "skill-a".to_string(),
            content: "Content A".to_string(),
            category: Some("cat1".to_string()),
            description: None,
        };
        let skill_b = Skill {
            name: "skill-b".to_string(),
            content: "Content B".to_string(),
            category: None,
            description: None,
        };

        store.save(&skill_a).await.unwrap();
        store.save(&skill_b).await.unwrap();

        let metas = store.list().await.unwrap();
        assert_eq!(metas.len(), 2);
    }

    #[tokio::test]
    async fn test_delete_skill() {
        let dir = tempdir().unwrap();
        let store = FileSkillStore::new(dir.path().to_path_buf());

        let skill = Skill {
            name: "temp".to_string(),
            content: "Temporary".to_string(),
            category: None,
            description: None,
        };

        store.save(&skill).await.unwrap();
        store.delete("temp").await.unwrap();

        let result = store.load("temp").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_load_nonexistent() {
        let dir = tempdir().unwrap();
        let store = FileSkillStore::new(dir.path().to_path_buf());
        let result = store.load("nope").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_skill_watcher_detects_changes() {
        use std::sync::{Arc, Mutex};

        let dir = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();

        let _watcher = SkillWatcher::new(
            dir.path().to_path_buf(),
            Box::new(move |event| {
                events_clone.lock().unwrap().push(event);
            }),
        )
        .unwrap();

        // Create a skill file
        let skill_dir = dir.path().join("test-skill");
        fs::create_dir_all(&skill_dir).await.unwrap();
        fs::write(skill_dir.join("SKILL.md"), "---\nname: test-skill\n---\nContent")
            .await
            .unwrap();

        // Give the watcher time to detect the change
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        let events = events.lock().unwrap();
        assert!(!events.is_empty(), "Watcher should have detected the change");
        assert!(
            matches!(&events[0], SkillEvent::Changed { name, .. } if name == "test-skill"),
            "First event should be a Changed event for test-skill"
        );
    }
}
