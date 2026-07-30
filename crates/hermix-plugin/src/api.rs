//! API types for the custom app system.
//!
//! These types are shared between the HTTP API layer (hermix-server),
//! the Tauri IPC layer (desktop-tauri), and the LLM tool
//! (custom_app_manager).

use serde::{Deserialize, Serialize};

use crate::manifest::PluginManifest;

/// Request to create a new custom app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateAppRequest {
    /// App manifest (id, name, version, description, etc.)
    pub manifest: PluginManifest,
    /// Self-contained index.html source code
    #[serde(default)]
    pub index_html: Option<String>,
}

/// Request to update an existing custom app's code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateAppRequest {
    /// New self-contained index.html source code (optional)
    #[serde(default)]
    pub index_html: Option<String>,
    /// Updated manifest (optional, e.g. to add missing permissions)
    #[serde(default)]
    pub manifest: Option<PluginManifest>,
}

/// Response containing app metadata and source code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppResponse {
    pub manifest: PluginManifest,
    /// Self-contained index.html source
    #[serde(default)]
    pub index_html: Option<String>,
    /// Whether the app has been packaged (dist/ exists)
    pub is_packaged: bool,
    /// Path to the dist directory (if packaged)
    #[serde(default)]
    pub dist_path: Option<String>,
}

/// Response for listing apps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListAppsResponse {
    pub apps: Vec<AppSummary>,
}

/// Summary of an app for list views.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSummary {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub icon: String,
    /// Custom icon file name (e.g. "icon.png") — null if no custom icon.
    #[serde(default)]
    pub icon_file: Option<String>,
    pub category: String,
    pub author: String,
    pub is_packaged: bool,
    /// Path to the dist directory (if packaged).
    #[serde(default)]
    pub dist_path: Option<String>,
}

/// Response for packaging an app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageResponse {
    pub success: bool,
    pub exe_path: String,
    pub output_dir: String,
    pub total_size: u64,
    pub error: Option<String>,
}

/// Response for running an app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunAppResponse {
    pub success: bool,
    pub error: Option<String>,
}

impl From<&crate::PluginEntry> for AppSummary {
    fn from(entry: &crate::PluginEntry) -> Self {
        let dist_dir = entry.dir.join("dist");
        let is_packaged = dist_dir.exists();
        Self {
            id: entry.manifest.id.clone(),
            name: entry.manifest.name.clone(),
            version: entry.manifest.version.clone(),
            description: entry.manifest.description.clone(),
            icon: entry.manifest.icon.clone(),
            icon_file: entry.manifest.icon_file.clone(),
            category: entry.manifest.category.clone(),
            author: entry.manifest.author.clone(),
            is_packaged,
            dist_path: if is_packaged {
                Some(dist_dir.to_string_lossy().to_string())
            } else {
                None
            },
        }
    }
}

impl From<&crate::PluginEntry> for AppResponse {
    fn from(entry: &crate::PluginEntry) -> Self {
        let index_html = entry
            .index_html
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok());
        let dist_dir = entry.dir.join("dist");
        let is_packaged = dist_dir.exists();

        Self {
            manifest: entry.manifest.clone(),
            index_html,
            is_packaged,
            dist_path: if is_packaged {
                Some(dist_dir.to_string_lossy().to_string())
            } else {
                None
            },
        }
    }
}
