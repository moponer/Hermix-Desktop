//! Plugin manifest parsing and validation.
//!
//! Each plugin has a `manifest.json` that defines its metadata, window
//! configuration, and permissions. This module handles parsing and
//! validation of these manifests.

use serde::{Deserialize, Serialize};

/// Plugin manifest parsed from manifest.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Unique plugin identifier (e.g., "pdf-converter").
    pub id: String,
    /// Human-readable name (e.g., "PDF 转换器").
    pub name: String,
    /// Semantic version (e.g., "1.0.0"). Defaults to "1.0.0".
    #[serde(default = "default_version")]
    pub version: String,
    /// Short description shown in the plugin store. Defaults to empty.
    #[serde(default)]
    pub description: String,
    /// Longer description shown in the detail modal.
    #[serde(default)]
    pub long_description: Option<String>,
    /// Plugin author (e.g., "LLM-generated" or user name). Defaults to "Hermix".
    #[serde(default = "default_author")]
    pub author: String,
    /// Emoji icon for display. Defaults to "📦".
    #[serde(default = "default_icon")]
    pub icon: String,
    /// Custom icon file name (e.g., "icon.png") stored in the app directory.
    /// When present, this file is used as the window/taskbar icon instead of
    /// the default shell icon. Supported formats: PNG (recommended), ICO.
    #[serde(default)]
    pub icon_file: Option<String>,
    /// Category for grouping (e.g., "file-tools", "media", "productivity").
    #[serde(default = "default_category")]
    pub category: String,
    /// Tags for search/filtering.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Window configuration for the plugin's independent window.
    /// Defaults to 600×480 resizable if omitted.
    #[serde(default)]
    pub window_config: WindowConfig,
    /// Permissions required by the plugin (e.g., "fs-read", "fs-write", "net").
    #[serde(default)]
    pub permissions: Vec<String>,
    /// External dependencies.
    #[serde(default)]
    pub dependencies: Option<PluginDependencies>,
}

fn default_category() -> String {
    "other".to_string()
}

fn default_version() -> String {
    "1.0.0".to_string()
}

fn default_author() -> String {
    "Hermix".to_string()
}

fn default_icon() -> String {
    "📦".to_string()
}

/// Window configuration for the plugin's independent window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowConfig {
    /// Window width in logical pixels.
    #[serde(default = "default_width")]
    pub width: u32,
    /// Window height in logical pixels.
    #[serde(default = "default_height")]
    pub height: u32,
    /// Whether the window is resizable.
    #[serde(default = "default_true")]
    pub resizable: bool,
    /// Window title (defaults to plugin name if not set).
    #[serde(default)]
    pub title: Option<String>,
}

/// Manual `Default` so `#[serde(default)]` on `PluginManifest.window_config`
/// produces 600×480 resizable (matching the serde field defaults), NOT
/// 0×0 (which `#[derive(Default)]` would give and validation would reject).
impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            width: default_width(),
            height: default_height(),
            resizable: default_true(),
            title: None,
        }
    }
}

fn default_width() -> u32 { 600 }
fn default_height() -> u32 { 480 }
fn default_true() -> bool { true }

/// External dependencies required by the plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginDependencies {
    /// npm packages to install.
    #[serde(default)]
    pub npm: Option<Vec<String>>,
}

impl PluginManifest {
    /// Validate the manifest for required fields and valid values.
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty() {
            return Err("plugin id cannot be empty".into());
        }
        if self.name.is_empty() {
            return Err("plugin name cannot be empty".into());
        }
        if !self.id.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
            return Err("plugin id must contain only alphanumeric characters, hyphens, or underscores".into());
        }
        if self.version.is_empty() {
            return Err("plugin version cannot be empty".into());
        }
        if self.window_config.width == 0 || self.window_config.height == 0 {
            return Err("window dimensions must be positive".into());
        }
        Ok(())
    }
}
