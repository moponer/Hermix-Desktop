//! # hermix-plugin
//!
//! Custom app system — user-facing desktop window applications.
//!
//! Uses **hx-shell** (Rust + wry/WebView2) as the runtime: a lightweight
//! (~2-3MB) single-file exe that provides a WebView2 window + built-in
//! `window.hx.*` bindings (writeFile/readFile/getInfo) via WebView2's
//! native postMessage bridge. Apps are self-contained HTML/CSS/JS.
//!
//! ## Architecture (hx-shell)
//!
//! - **PluginManager**: scans app directories, manages lifecycle
//! - **PluginManifest**: defines app metadata, window config, permissions
//! - **HxShellPackager**: packs app index.html + hx-shell.exe → single
//!   distributable exe (HTML tail-injected). User machine needs zero
//!   toolchain — packaging is a pure file concatenation.

pub mod manifest;
pub mod scanner;
pub mod loader_packager; // legacy (unused, kept for compile stability)
pub mod icon_library;
pub mod validate;
pub use validate::validate_custom_app;
pub mod smoke;
pub use smoke::{run_smoke_test, run_smoke_test_async, SmokeError, SmokeReport};
pub mod api;
pub mod hx_packager;
pub use hx_packager::{HxShellPackager, PackageMode, HxPackError};
pub mod node_setup; // 保留：loader_packager.rs 依赖（neu build 路径的 env 检查）

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Generate an HTML shell for TypeScript/React apps.
///
/// hx-shell 架构：React + ReactDOM 由 esbuild 打包进 `app.js`（无 CDN、无网络依赖），
/// 系统能力通过 `window.hx.*`（WebView2 原生桥）调用，无需外部客户端库。
fn generate_html_shell(manifest: &PluginManifest) -> String {
    let title = manifest.window_config
        .title
        .as_deref()
        .unwrap_or(&manifest.name);
    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>{title}</title>
  </head>
  <body>
    <div id="root"></div>
    <script src="/js/app.js"></script>
  </body>
</html>"#
    )
}

pub use manifest::{PluginDependencies, PluginManifest, WindowConfig};

pub use scanner::PluginScanner;

/// A loaded plugin entry with its manifest and file paths.
#[derive(Debug, Clone)]
pub struct PluginEntry {
    /// Plugin manifest parsed from manifest.json.
    pub manifest: PluginManifest,
    /// Path to self-contained index.html (either LLM-provided or auto-generated shell)
    pub index_html: Option<PathBuf>,
    /// Path to TypeScript/React source (src/main.tsx), if app uses TypeScript mode.
    /// When this is set, the packager runs esbuild to transpile → resources/js/app.js.
    pub app_source: Option<PathBuf>,
    /// Path to the `src/` directory containing additional TypeScript files.
    pub source_dir: Option<PathBuf>,
    /// Path to custom icon file, e.g. icon.png (optional)
    pub icon_path: Option<PathBuf>,
    /// Path to README.md (optional)
    pub readme_path: Option<PathBuf>,
    /// Path to the plugin directory.
    pub dir: PathBuf,
}

/// Errors that can occur in the plugin system.
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("plugin not found: {0}")]
    NotFound(String),

    #[error("invalid manifest: {0}")]
    InvalidManifest(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("app loader not found: {0}")]
    LoaderNotFound(String),

    #[error("packaging failed: {0}")]
    PackagingFailed(String),

    #[error("execution failed: {0}")]
    ExecutionFailed(String),
}

/// Heuristic: does `s` look like a file path rather than HTML content?
fn looks_like_file_path(s: &str) -> bool {
    if s.contains('<') {
        return false;
    }
    let s = s.trim();
    if s.len() >= 3 {
        let bytes = s.as_bytes();
        if bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/') {
            return true;
        }
    }
    if s.starts_with('/') || s.starts_with("~/") {
        return true;
    }
    false
}

/// Walk up from `start` looking for `relative` to exist.
fn walk_up_for(start: &Path, relative: &str) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    for _ in 0..15 {
        let candidate = current.join(relative);
        if candidate.exists() {
            return Some(candidate);
        }
        if !current.pop() {
            break;
        }
    }
    None
}

/// Resolve the path to the hx-shell template binary ().
///
/// Candidate priority:
///   1. `HERMIX_HOME/loader/hx-shell.exe`
///   2. `current_exe/resources/loader/hx-shell.exe`
///   3. Walk up from `current_exe()` for `resources/loader/hx-shell.exe`
///   4. default `HERMIX_HOME/loader/hx-shell.exe`
pub fn resolve_hx_shell_path() -> PathBuf {
    let home = hermix_config::paths::hermix_home();
    let candidates = [
        Some(home.join("loader").join("hx-shell.exe")),
        std::env::current_exe().ok().and_then(|e| {
            e.parent().map(|d| d.join("resources").join("loader").join("hx-shell.exe"))
        }),
        std::env::current_exe().ok().and_then(|e| {
            e.parent().and_then(|d| walk_up_for(d, "resources/loader/hx-shell.exe"))
        }),
    ]
    .into_iter().flatten();
    for c in candidates { if c.exists() { return c; } }
    home.join("loader").join("hx-shell.exe")
}

/// Resolve the path to the `WebView2Loader.dll` (wry runtime dependency).
pub fn resolve_webview2_loader_path() -> PathBuf {
    let home = hermix_config::paths::hermix_home();
    let candidates = [
        Some(home.join("loader").join("WebView2Loader.dll")),
        std::env::current_exe().ok().and_then(|e| {
            e.parent().map(|d| d.join("resources").join("loader").join("WebView2Loader.dll"))
        }),
        std::env::current_exe().ok().and_then(|e| {
            e.parent().and_then(|d| walk_up_for(d, "resources/loader/WebView2Loader.dll"))
        }),
    ]
    .into_iter().flatten();
    for c in candidates { if c.exists() { return c; } }
    home.join("loader").join("WebView2Loader.dll")
}

/// The main plugin manager that coordinates all plugin operations.
pub struct PluginManager {
    /// Directory where plugins are stored.
    plugins_dir: PathBuf,
    /// Loaded plugins indexed by ID.
    plugins: RwLock<HashMap<String, PluginEntry>>,
    /// hx-shell packager.
    packager: Arc<HxShellPackager>,
}

impl PluginManager {
    /// Create a new plugin manager.
    pub fn new(plugins_dir: PathBuf) -> Self {
        let shell_path = resolve_hx_shell_path();
        let loader_dll_path = resolve_webview2_loader_path();
        let packager = Arc::new(HxShellPackager::new(shell_path, loader_dll_path));
        Self {
            plugins_dir,
            plugins: RwLock::new(HashMap::new()),
            packager,
        }
    }

    /// Scan the plugins directory and load all valid plugins.
    pub fn scan_plugins(&self) -> Result<Vec<PluginEntry>, PluginError> {
        let scanner = PluginScanner::new(self.plugins_dir.clone());
        let entries = scanner.scan()?;

        let mut plugins = self.plugins.write().expect("plugin lock poisoned");
        plugins.clear();
        for entry in &entries {
            plugins.insert(entry.manifest.id.clone(), entry.clone());
        }

        tracing::info!(
            "loaded {} custom apps from {}",
            entries.len(),
            self.plugins_dir.display()
        );
        Ok(entries)
    }

    /// List all loaded plugins.
    pub fn list_plugins(&self) -> Vec<PluginManifest> {
        let plugins = self.plugins.read().expect("plugin lock poisoned");
        plugins.values().map(|e| e.manifest.clone()).collect()
    }

    /// Get a specific plugin by ID.
    pub fn get_plugin(&self, id: &str) -> Option<PluginEntry> {
        let plugins = self.plugins.read().expect("plugin lock poisoned");
        plugins.get(id).cloned()
    }

    /// Install a new plugin from LLM-generated code.
    ///
    /// Two modes:
    /// - **TypeScript/React mode**: provide `app_source` (TSX code). The system
    ///   generates an HTML shell and the packager runs esbuild to transpile.
    /// - **Plain HTML mode** (backward compatible): provide `index_html` directly.
    ///
    /// If both `app_source` and `index_html` are provided, `app_source` takes
    /// precedence — the LLM's `index_html` is used as the custom shell, and
    /// `app_source` is transpiled via esbuild into `app.js`.
    pub fn install_plugin(
        &self,
        manifest: &PluginManifest,
        index_html: Option<&str>,
        app_source: Option<&str>,
        source_files: Option<&HashMap<String, String>>,
        icon_data: Option<&str>,
    ) -> Result<PluginEntry, PluginError> {
        manifest
            .validate()
            .map_err(PluginError::InvalidManifest)?;

        // Validate that at least one source is provided
        let has_html = index_html.map(|s| !s.trim().is_empty()).unwrap_or(false);
        let has_source = app_source.map(|s| !s.trim().is_empty()).unwrap_or(false);

        if !has_html && !has_source {
            return Err(PluginError::InvalidManifest(
                "either app_source (TypeScript/React) or index_html (plain HTML) is required".into(),
            ));
        }

        let plugin_dir = self.plugins_dir.join(&manifest.id);

        if plugin_dir.exists() {
            std::fs::remove_dir_all(&plugin_dir)?;
        }

        std::fs::create_dir_all(&plugin_dir)?;

        let manifest_json = serde_json::to_string_pretty(manifest)?;
        std::fs::write(plugin_dir.join("manifest.json"), manifest_json)?;

        // ── TypeScript/React mode ──
        let (app_source_path, source_dir) = if has_source {
            let src_dir = plugin_dir.join("src");
            std::fs::create_dir_all(&src_dir)?;

            // Write main entry file
            let main_path = src_dir.join("main.tsx");
            std::fs::write(&main_path, app_source.unwrap())?;

            // Write additional source files
            if let Some(files) = source_files {
                for (rel_path, content) in files {
                    let target = src_dir.join(rel_path);
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&target, content)?;
                }
            }

            (Some(main_path), Some(src_dir))
        } else {
            (None, None)
        };

        // ── index.html ──
        let index_html_path = {
            let path = plugin_dir.join("index.html");
            let html_content = if has_html {
                // LLM provided HTML (either plain HTML mode or custom shell for TS mode)
                let raw = index_html.unwrap().trim();
                if looks_like_file_path(raw) {
                    std::fs::read_to_string(raw).map_err(|e| {
                        PluginError::InvalidManifest(format!(
                            "index_html looks like a file path but read failed: {raw} -- {e}"
                        ))
                    })?
                } else {
                    raw.to_string()
                }
            } else {
                // Auto-generate HTML shell for TypeScript mode
                generate_html_shell(manifest)
            };
            std::fs::write(&path, &html_content)?;
            Some(path)
        };

        // ── Validate HTML content looks valid (only for plain HTML mode) ──
        if has_html && !has_source {
            let html_code = std::fs::read_to_string(index_html_path.as_ref().unwrap())?;
            let html_lower = html_code.to_lowercase();
            if !html_lower.contains("<html")
                && !html_lower.contains("<!doctype")
                && !html_lower.contains("<div")
                && !html_lower.contains("<script")
            {
                return Err(PluginError::InvalidManifest(format!(
                    "index_html content does not look like valid HTML -- \
                     no <html>, <!DOCTYPE>, <div> or <script> tag found."
                )));
            }
        }

        let icon_path = if let Some(data) = icon_data {
            let bytes = base64_decode_icon(data)?;
            let path = plugin_dir.join("icon.png");
            std::fs::write(&path, bytes)?;
            Some(path)
        } else {
            match icon_library::generate_icon_png(
                &manifest.name,
                &manifest.id,
                &manifest.category,
            ) {
                Ok(bytes) => {
                    let path = plugin_dir.join("icon.png");
                    if std::fs::write(&path, &bytes).is_ok() {
                        Some(path)
                    } else {
                        None
                    }
                }
                Err(e) => {
                    tracing::warn!("auto-generate app icon failed: {e}");
                    None
                }
            }
        };

        let final_manifest = if icon_path.is_some() && manifest.icon_file.is_none() {
            let updated = PluginManifest {
                icon_file: Some("icon.png".to_string()),
                ..manifest.clone()
            };
            let manifest_json = serde_json::to_string_pretty(&updated)?;
            std::fs::write(plugin_dir.join("manifest.json"), manifest_json)?;
            updated
        } else {
            manifest.clone()
        };

        let entry = PluginEntry {
            manifest: final_manifest,
            index_html: index_html_path,
            app_source: app_source_path,
            source_dir,
            icon_path,
            readme_path: None,
            dir: plugin_dir.clone(),
        };

        let mut plugins = self.plugins.write().expect("plugin lock poisoned");
        plugins.insert(manifest.id.clone(), entry.clone());

        tracing::info!("installed custom app: {} (mode: {})",
            manifest.id,
            if has_source { "TypeScript/React" } else { "plain HTML" }
        );
        Ok(entry)
    }

    /// Update an existing plugin's code and/or manifest.
    ///
    /// Can update `index_html` (plain HTML), `app_source` (TypeScript/React),
    /// or `manifest` independently.
    pub fn update_plugin(
        &self,
        id: &str,
        index_html: Option<&str>,
        app_source: Option<&str>,
        source_files: Option<&HashMap<String, String>>,
        manifest: Option<&PluginManifest>,
    ) -> Result<PluginEntry, PluginError> {
        let mut entry = self
            .get_plugin(id)
            .ok_or_else(|| PluginError::NotFound(id.to_string()))?;

        // Update TypeScript source
        if let Some(source) = app_source {
            let raw = source.trim();
            if !raw.is_empty() {
                let src_dir = entry.dir.join("src");
                std::fs::create_dir_all(&src_dir)?;
                let main_path = src_dir.join("main.tsx");
                std::fs::write(&main_path, raw)?;
                entry.app_source = Some(main_path);
                entry.source_dir = Some(src_dir.clone());

                // Write additional source files
                if let Some(files) = source_files {
                    for (rel_path, content) in files {
                        let target = src_dir.join(rel_path);
                        if let Some(parent) = target.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        std::fs::write(&target, content)?;
                    }
                }

                // Auto-generate HTML shell if index.html doesn't exist or is missing
                let idx_path = entry.dir.join("index.html");
                if !idx_path.exists() {
                    let shell = if let Some(m) = manifest {
                        generate_html_shell(m)
                    } else {
                        generate_html_shell(&entry.manifest)
                    };
                    std::fs::write(&idx_path, &shell)?;
                    entry.index_html = Some(idx_path);
                }
            }
        }

        // Update plain HTML (only if app_source is not being updated)
        if let Some(code) = index_html {
            let raw = code.trim();
            if !raw.is_empty() {
                let resolved = if looks_like_file_path(raw) {
                    std::fs::read_to_string(raw).map_err(|e| {
                        PluginError::InvalidManifest(format!(
                            "index_html looks like a file path but read failed: {raw} -- {e}"
                        ))
                    })?
                } else {
                    raw.to_string()
                };
                let idx_path = entry.dir.join("index.html");
                std::fs::write(&idx_path, &resolved)?;
                entry.index_html = Some(idx_path);
            }
        }

        if let Some(m) = manifest {
            m.validate()
                .map_err(PluginError::InvalidManifest)?;
            let manifest_json = serde_json::to_string_pretty(m)?;
            std::fs::write(entry.dir.join("manifest.json"), manifest_json)?;
            entry.manifest = m.clone();
        }

        let mut plugins = self.plugins.write().expect("plugin lock poisoned");
        plugins.insert(id.to_string(), entry.clone());

        tracing::info!("updated custom app: {}", id);
        Ok(entry)
    }

    /// Uninstall a plugin.
    pub fn uninstall_plugin(&self, id: &str) -> Result<(), PluginError> {
        let plugin_dir = self.plugins_dir.join(id);

        if !plugin_dir.exists() {
            return Err(PluginError::NotFound(id.to_string()));
        }

        std::fs::remove_dir_all(&plugin_dir)?;

        let mut plugins = self.plugins.write().expect("plugin lock poisoned");
        plugins.remove(id);

        tracing::info!("uninstalled custom app: {}", id);
        Ok(())
    }

    /// Package a plugin with the given mode (default: sidecar).
    pub async fn package_plugin(&self, id: &str) -> Result<PathBuf, PluginError> {
        self.package_plugin_with_mode(id, PackageMode::default()).await
    }

    /// Package a plugin with an explicit mode.
    pub async fn package_plugin_with_mode(
        &self,
        id: &str,
        mode: PackageMode,
    ) -> Result<PathBuf, PluginError> {
        let entry = self
            .get_plugin(id)
            .ok_or_else(|| PluginError::NotFound(id.to_string()))?;

        if entry.index_html.is_none() {
            return Err(PluginError::PackagingFailed(
                "app missing index.html, cannot package".into(),
            ));
        }

        let result = self.packager.package(&entry.dir, &entry.manifest.id, mode)
            .map_err(|e| PluginError::PackagingFailed(e.to_string()))?;
        Ok(result)
    }

    /// Launch a packaged app (spawns `dist/<id>.exe`).
    pub async fn run_plugin(&self, id: &str) -> Result<(), PluginError> {
        let entry = self
            .get_plugin(id)
            .ok_or_else(|| PluginError::NotFound(id.to_string()))?;
        let exe_name = if cfg!(windows) {
            format!("{}.exe", entry.manifest.id)
        } else {
            entry.manifest.id.clone()
        };
        let exe_path = entry.dir.join("dist").join(&exe_name);
        if !exe_path.exists() {
            return Err(PluginError::ExecutionFailed(format!(
                "app not packaged, exe not found: {}",
                exe_path.display()
            )));
        }
        let child = std::process::Command::new(&exe_path)
            .spawn()
            .map_err(|e| PluginError::ExecutionFailed(format!("launch app failed: {e}")))?;
        // 窗口居中由 hx-shell 内部完成 (main.rs WindowBuilder::with_outer_position,
        // 2026-08-05 已在 apps/hx-shell/src/main.rs 修复) — spawn 侧不做额外处理。
        drop(child);
        Ok(())
    }

    /// Get the plugins directory path.
    pub fn plugins_dir(&self) -> &Path {
        &self.plugins_dir
    }

    /// Get a reference to the packager.
    pub fn packager(&self) -> &Arc<HxShellPackager> {
        &self.packager
    }
}

/// Decode a base64-encoded icon Image (PNG/ICO).
pub(crate) fn base64_decode_icon(data: &str) -> Result<Vec<u8>, PluginError> {
    let b64 = if data.starts_with("data:") {
        data.split(',')
            .next_back()
            .unwrap_or("")
            .trim()
    } else {
        data.trim()
    };
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| PluginError::InvalidManifest(format!("icon_data base64 decode failed: {e}")))
}
