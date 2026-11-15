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

            // Skip hidden directories (e.g. `.proposals` design drafts) — they
            // are not plugins and would otherwise be reported as invalid.
            if dir_path
                .file_name()
                .map(|n| n.to_string_lossy().starts_with('.'))
                .unwrap_or(false)
            {
                continue;
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

        let index_html_path = dir_path.join("index.html");
        let index_html = if index_html_path.exists() {
            Some(index_html_path)
        } else {
            None
        };

        // Check for TypeScript/React source (src/main.tsx)
        let src_dir = dir_path.join("src");
        let main_tsx = src_dir.join("main.tsx");
        let app_source = if main_tsx.exists() {
            Some(main_tsx)
        } else {
            None
        };
        let source_dir = if app_source.is_some() {
            Some(src_dir)
        } else {
            None
        };

        // 已打包（`hx-packager` 产物）的自定义应用：`installed.json` 里记着
        // `exe: dist/xxx.exe`，源码入口 index.html / src/main.tsx 已经打进
        // 那个 exe 里了。此前这类应用每次启动都被判 "invalid manifest"
        // （实机 102 条 WARN + 应用在列表里消失），而它其实是装好可用的。
        let packaged_exe = std::fs::read_to_string(dir_path.join("installed.json"))
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|v| v.get("exe").and_then(|e| e.as_str()).map(str::to_string))
            .map(|rel| dir_path.join(rel))
            .filter(|p| p.exists());

        if index_html.is_none() && app_source.is_none() && packaged_exe.is_none() {
            return Err(PluginError::InvalidManifest(
                "index.html、src/main.tsx 与已打包的 exe 均不存在 (自定义应用需要至少一个)".into(),
            ));
        }

        // If app_source exists but index.html doesn't, that's OK -
        // the packager will generate the HTML shell during build.

        // Resolve icon_path from manifest.icon_file
        let icon_path = manifest.icon_file.as_ref().and_then(|name| {
            let p = dir_path.join(name);
            if p.exists() { Some(p) } else { None }
        });

        let readme_path = dir_path.join("README.md");
        let readme_path = if readme_path.exists() { Some(readme_path) } else { None };

        Ok(PluginEntry {
            manifest,
            index_html,
            app_source,
            source_dir,
            icon_path,
            readme_path,
            dir: dir_path.to_path_buf(),
        })
    }
}
