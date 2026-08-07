//! Plugin manifest parsing and validation.
//!
//! Each plugin has a `manifest.json` that defines its metadata, window
//! configuration, and permissions. This module handles parsing and
//! validation of these manifests.

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    #[serde(default, deserialize_with = "deserialize_string_or_seq")]
    pub tags: Vec<String>,
    /// Window configuration for the plugin's independent window.
    /// Defaults to 600×480 resizable if omitted.
    #[serde(default)]
    pub window_config: WindowConfig,
    /// Permissions required by the plugin (e.g., "fs-read", "fs-write", "net").
    #[serde(default, deserialize_with = "deserialize_string_or_seq")]
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

/// Tolerant deserializer for `Vec<String>` fields that the LLM may shape wrong.
///
/// Accepts any of:
/// - absent / null  -> empty vec (matches `#[serde(default)]`)
/// - a single string -> `[string]`
/// - an array -> each element coerced to string
/// - an object -> its keys collected as strings
///
/// This prevents the raw `invalid type: map, expected a sequence` serde error
/// when the LLM writes `permissions`/`tags` as a string or as an object
/// (e.g. `{"fs-read": true}`) instead of an array.
fn deserialize_string_or_seq<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Null => Ok(Vec::new()),
        Value::String(s) => Ok(vec![s]),
        Value::Array(arr) => Ok(arr
            .into_iter()
            .map(|v| match v {
                Value::String(s) => s,
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                other => other.to_string(),
            })
            .collect()),
        Value::Object(map) => Ok(map.keys().cloned().collect()),
        other => Err(serde::de::Error::custom(format!(
            "expected a string, array, or object for a string list; got {other:?}"
        ))),
    }
}

/// Optional variant for `Option<Vec<String>>` fields (e.g. `npm`).
fn deserialize_opt_string_or_seq<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(vec![s])),
        Value::Array(arr) => Ok(Some(
            arr.into_iter()
                .map(|v| match v {
                    Value::String(s) => s,
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    other => other.to_string(),
                })
                .collect(),
        )),
        Value::Object(map) => Ok(Some(map.keys().cloned().collect())),
        other => Err(serde::de::Error::custom(format!(
            "expected a string, array, or object for an optional string list; got {other:?}"
        ))),
    }
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
    #[serde(default, deserialize_with = "deserialize_opt_string_or_seq")]
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

/// Map Hermix permission names to Neutralinojs nativeAllowList patterns.
///
/// Hermix uses semantic permission names (fs-read, fs-write, exec, etc.),
/// while Neutralinojs uses glob patterns matching API namespaces.
pub fn permissions_to_neutralino_allowlist(permissions: &[String]) -> Vec<String> {
    let mut allow: Vec<String> = Vec::new();
    for p in permissions {
        match p.as_str() {
            "fs-read" => {
                allow.push("filesystem.readFile".into());
                allow.push("filesystem.readdir".into());
                allow.push("filesystem.stats".into());
            }
            "fs-write" => {
                allow.push("filesystem.writeFile".into());
                allow.push("filesystem.createDirectory".into());
                allow.push("filesystem.removeFile".into());
                allow.push("filesystem.removeDirectory".into());
                allow.push("filesystem.writeFileBinary".into());
            }
            "exec" => {
                allow.push("os.execCommand".into());
                allow.push("os.spawnProcess".into());
                allow.push("os.getSpawnedProcesses".into());
                allow.push("os.killProcess".into());
                allow.push("os.updateStoredValue".into());
            }
            "dialog" => {
                allow.push("dialog.openDialog".into());
                allow.push("dialog.saveDialog".into());
                allow.push("dialog.showMessageBox".into());
                allow.push("os.showMessageBox".into());
            }
            "env" => {
                allow.push("os.getEnv".into());
                allow.push("os.getEnvs".into());
            }
            "temp" => {
                allow.push("os.getPath".into());
            }
            "clipboard" => {
                allow.push("clipboard.get".into());
                allow.push("clipboard.set".into());
            }
            "network" => {
                // Neutralino doesn't gate fetch() — JS native fetch works
                // without permission. No nativeAllowList entry needed.
            }
            _ => {}
        }
    }
    // Always allow app.* (exit, etc.) and events.*
    allow.push("app.*".into());
    allow.push("events.*".into());
    allow.push("window.*".into());
    allow
}

/// Generate a Neutralinojs config (`neutralino.config.json`) from a
/// Hermix plugin manifest.
///
/// This maps our manifest fields to the Neutralinojs config format,
/// including window settings, permissions (nativeAllowList), and
/// application metadata.
pub fn generate_neutralino_config(manifest: &PluginManifest) -> serde_json::Value {
    let window_title = manifest
        .window_config
        .title
        .clone()
        .unwrap_or_else(|| manifest.name.clone());

    let icon_path = if manifest.icon_file.is_some() {
        "/resources/icon.png"
    } else {
        ""
    };

    let allow_list = permissions_to_neutralino_allowlist(&manifest.permissions);

    serde_json::json!({
        "applicationId": manifest.id,
        "version": manifest.version,
        "defaultMode": "window",
        "port": 0,
        "documentRoot": "/resources/",
        "url": "/",
        "enableServer": true,
        "enableNativeAPI": true,
        "tokenSecurity": "none",
        "logging": {
            "enabled": true,
            "writeToLogFile": false
        },
        "nativeAllowList": allow_list,
        "globalVariables": {
            "APP_NAME": manifest.name,
            "APP_VERSION": manifest.version,
            "APP_DESCRIPTION": manifest.description
        },
        "modes": {
            "window": {
                "title": window_title,
                "width": manifest.window_config.width,
                "height": manifest.window_config.height,
                "center": true,
                "fullScreen": false,
                "alwaysOnTop": false,
                "icon": icon_path,
                "enableInspector": true,
                "borderless": false,
                "maximize": false,
                "hidden": false,
                "resizable": manifest.window_config.resizable,
                "exitProcessOnClose": true
            }
        }
    })
}


#[cfg(test)]
mod tests {
    use super::*;

    /// `tags`/`permissions` must accept a single string instead of an array.
    #[test]
    fn string_tags_permissions_normalized() {
        let m: PluginManifest = serde_json::from_str(
            r#"{"id":"a","name":"A","tags":"onetag","permissions":"fs-read"}"#,
        )
        .expect("string list fields should be tolerated");
        assert_eq!(m.tags, vec!["onetag"]);
        assert_eq!(m.permissions, vec!["fs-read"]);
    }

    /// `tags`/`permissions` must accept an object whose keys become the list
    /// (the typical LLM mistake of writing `{"fs-read": true}`).
    #[test]
    fn object_tags_permissions_normalized_to_keys() {
        let m: PluginManifest = serde_json::from_str(
            r#"{"id":"a","name":"A","permissions":{"fs-read":true,"exec":true}}"#,
        )
        .expect("object list fields should be tolerated");
        // serde_json::Map is BTreeMap-backed → keys iterate in sorted order;
        // assert order-agnostically since permissions semantics don't care.
        let mut got = m.permissions;
        got.sort();
        assert_eq!(got, vec!["exec", "fs-read"]);
    }

    /// `npm` (Option<Vec<String>>) must accept a single string.
    #[test]
    fn npm_string_normalized_to_some() {
        let m: PluginManifest =
            serde_json::from_str(r#"{"id":"a","name":"A","dependencies":{"npm":"lodash"}}"#)
                .expect("string npm should be tolerated");
        assert_eq!(m.dependencies.and_then(|d| d.npm), Some(vec!["lodash".to_string()]));
    }
}
