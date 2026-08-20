//! API types for the custom app system.
//!
//! These types are shared between the HTTP API layer (hermix-server),
//! the Tauri IPC layer (desktop-tauri), and the LLM tool
//! (custom_app_manager).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::manifest::PluginManifest;

/// Request to create a new custom app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateAppRequest {
    /// App manifest (id, name, version, description, etc.)
    pub manifest: PluginManifest,
    /// Self-contained index.html source code (plain HTML mode).
    ///
    /// In TypeScript/React mode, this is optional — if not provided, an
    /// HTML shell is auto-generated. If provided alongside `app_source`,
    /// it's used as the custom HTML shell.
    #[serde(default)]
    pub index_html: Option<String>,
    /// TypeScript/React source code (the app entry point, typically `main.tsx`).
    ///
    /// When provided, the system writes it to `src/main.tsx` and the packager
    /// runs esbuild to transpile it into `resources/js/app.js`.
    ///
    /// React and ReactDOM must be used as global variables (loaded via CDN in
    /// index.html), not via `import React from 'react'`.
    #[serde(default)]
    pub app_source: Option<String>,
    /// Additional TypeScript source files (relative path → content).
    ///
    /// Keys are paths relative to `src/`, e.g. `"components/Header.tsx"`,
    /// `"utils/helpers.ts"`. Values are the file contents.
    #[serde(default)]
    pub source_files: Option<HashMap<String, String>>,
}

/// Request to update an existing custom app's code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateAppRequest {
    /// New self-contained index.html source code (optional, plain HTML mode)
    #[serde(default)]
    pub index_html: Option<String>,
    /// New TypeScript/React source code (optional, TypeScript mode)
    #[serde(default)]
    pub app_source: Option<String>,
    /// Additional TypeScript source files (optional)
    #[serde(default)]
    pub source_files: Option<HashMap<String, String>>,
    /// Updated manifest (optional, e.g. to add missing permissions)
    #[serde(default)]
    pub manifest: Option<PluginManifest>,
}

/// Response containing app metadata and source code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppResponse {
    pub manifest: PluginManifest,
    /// Self-contained index.html source (either LLM-provided or auto-generated shell)
    #[serde(default)]
    pub index_html: Option<String>,
    /// TypeScript/React source code (if app uses TypeScript mode)
    #[serde(default)]
    pub app_source: Option<String>,
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
    /// Tile background color (e.g. "#FF6B6B") — null falls back to neutral
    /// gray for in-app views and to a transparent icon for the packaged EXE.
    #[serde(default)]
    pub tile_color: Option<String>,
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
            tile_color: entry.manifest.tile_color.clone(),
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
        let app_source = entry
            .app_source
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok());
        let dist_dir = entry.dir.join("dist");
        let is_packaged = dist_dir.exists();

        Self {
            manifest: entry.manifest.clone(),
            index_html,
            app_source,
            is_packaged,
            dist_path: if is_packaged {
                Some(dist_dir.to_string_lossy().to_string())
            } else {
                None
            },
        }
    }
}
