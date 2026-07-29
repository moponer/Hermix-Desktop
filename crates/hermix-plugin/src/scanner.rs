//! Plugin directory scanner.
//!
//! Scans the plugins directory for valid plugins by looking for
//! subdirectories containing a `manifest.json` file.

use std::path::PathBuf;
use walkdir::WalkDir;

use crate::{PluginEntry, PluginError, PluginManifest};

/// Scans a directory for plugins.
pub struct PluginScanner {
    plugins_dir: PathBuf,
}

impl PluginScanner {
    pub fn new(plugins_dir: PathBuf) -> Self {
        Self { plugins_dir }
    }

    /// Scan the plugins directory and return all valid plugin entries.
    pub fn scan(&self) -> Result<Vec<PluginEntry>, PluginError> {
        let mut entries = Vec::new();

        if !self.plugins_dir.exists() {
            tracing::info!("plugins directory does not exist: {}", self.plugins_dir.display());
            return Ok(entries);
        }

        for entry in WalkDir::new(&self.plugins_dir)
            .max_depth(1)  // Only immediate subdirectories
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_dir() {
                continue;
            }

            let dir_path = entry.path();
            if dir_path == self.plugins_dir {
                continue;  // Skip the root plugins dir itself
            }

            match self.load_plugin(dir_path) {
                Ok(plugin) => {
                    tracing::debug!("loaded plugin: {}", plugin.manifest.id);
                    entries.push(plugin);
                }
                Err(e) => {
                    tracing::warn!(
                        "skipping invalid plugin at {}: {}",
                        dir_path.display(),
                        e
                    );
                }
            }
        }

        Ok(entries)
    }

    /// Load a single plugin from a directory.
    fn load_plugin(&self, dir_path: &std::path::Path) -> Result<PluginEntry, PluginError> {
        let manifest_path = dir_path.join("manifest.json");
        if !manifest_path.exists() {
            return Err(PluginError::InvalidManifest(
                "manifest.json not found".into()
            ));
        }

        let manifest_str = std::fs::read_to_string(&manifest_path)?;
        let manifest: PluginManifest = serde_json::from_str(&manifest_str)?;

        manifest.validate().map_err(PluginError::InvalidManifest)?;

        let backend_path = dir_path.join("backend.ts");
        let ui_path = dir_path.join("ui.tsx");
        let ui_path = if ui_path.exists() { Some(ui_path) } else { None };

        let index_html_path = dir_path.join("index.html");
        let index_html = if index_html_path.exists() {
            Some(index_html_path)
        } else {
            None
        };

        // Require at least one app model: legacy (backend.ts) or new-loader
        // (self-contained index.html).
        if !backend_path.exists() && index_html.is_none() {
            return Err(PluginError::InvalidManifest(
                "backend.ts 与 index.html 均不存在 (自定义应用需要其一)".into(),
            ));
        }

        // Resolve icon_path from manifest.icon_file
        let icon_path = manifest.icon_file.as_ref().and_then(|name| {
            let p = dir_path.join(name);
            if p.exists() { Some(p) } else { None }
        });

        let readme_path = dir_path.join("README.md");
        let readme_path = if readme_path.exists() { Some(readme_path) } else { None };

        Ok(PluginEntry {
            manifest,
            backend_path,
            ui_path,
            index_html,
            icon_path,
            readme_path,
            dir: dir_path.to_path_buf(),
        })
    }
}
