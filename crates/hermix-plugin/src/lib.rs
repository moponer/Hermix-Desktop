//! # hermix-plugin
//!
//! 自定义应用系统 — 面向终端用户的桌面窗口应用。
//!
//! 与 LLM 驱动的插件 (hermix-tools) 不同，自定义应用是用户自己使用的
//! 独立桌面窗口应用。用户通过对话让 LLM 生成应用代码，系统自动打包
//! 为独立可执行文件。
//!
//! ## 架构 (standalone wry loader 方案)
//!
//! - **PluginManager**: 扫描应用目录、管理生命周期
//! - **PluginManifest**: 定义应用元数据、窗口配置、权限
//! - **NewLoaderPackager**: 用 `inject_app` 把自包含 `index.html` + `manifest.json`
//!   注入 `hermix-app-loader.exe` 尾部，产出零 Tauri 运行时的独立 `.exe`
//! - **无 esbuild / Deno / QuickJS**: 应用即静态 HTML，桥接走 `app://localhost/rpc`

pub mod manifest;
pub mod scanner;
pub mod loader_packager;
pub mod icon_library;
pub mod validate;
pub use validate::validate_custom_app;
pub mod smoke;
pub use smoke::{run_smoke_test, run_smoke_test_async, SmokeError, SmokeReport};
pub mod api;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

pub use manifest::{PluginDependencies, PluginManifest, WindowConfig};
pub use loader_packager::{NewLoaderPackager, PackageResult};
pub use scanner::PluginScanner;

/// A loaded plugin entry with its manifest and file paths.
#[derive(Debug, Clone)]
pub struct PluginEntry {
    /// Plugin manifest parsed from manifest.json.
    pub manifest: PluginManifest,
    /// Path to self-contained index.html
    pub index_html: Option<PathBuf>,
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

/// Resolve the path to the standalone app loader (`hermix-app-loader.exe`).
///
/// Candidate priority (mirrors the shell/esbuild resolvers):
///   1. `HERMIX_HOME/loader/hermix-app-loader.exe` — extracted on first launch
///      by the desktop app's resource unpacking.
///   2. `current_exe/resources/loader/hermix-app-loader.exe` — installed build.
///   3. `apps/custom-app-loader/target/release/hermix-app-loader.exe` —
///      repo-dev fallback (so the host works straight from the source tree
///      without a separate copy step).
///   4. default `HERMIX_HOME/loader/hermix-app-loader.exe`.
pub fn resolve_loader_path() -> PathBuf {
    let home = hermix_config::paths::hermix_home();
    let candidates = [
        Some(home.join("loader").join("hermix-app-loader.exe")),
        std::env::current_exe().ok().and_then(|e| {
            e.parent()
                .map(|d| d.join("resources").join("loader").join("hermix-app-loader.exe"))
        }),
        Some(
            std::env::current_dir()
                .ok()
                .map(|d| d.join("apps").join("custom-app-loader").join("target").join("release").join("hermix-app-loader.exe"))
                .unwrap_or_default(),
        ),
    ]
    .into_iter()
    .flatten();
    for c in candidates {
        if c.exists() {
            return c;
        }
    }
    home.join("loader").join("hermix-app-loader.exe")
}

/// Resolve the path to the `inject_app.exe` packer (companion to the loader).
/// Same candidate priority as [`resolve_loader_path`].
pub fn resolve_inject_path() -> PathBuf {
    let home = hermix_config::paths::hermix_home();
    let candidates = [
        Some(home.join("loader").join("inject_app.exe")),
        std::env::current_exe().ok().and_then(|e| {
            e.parent()
                .map(|d| d.join("resources").join("loader").join("inject_app.exe"))
        }),
        Some(
            std::env::current_dir()
                .ok()
                .map(|d| d.join("apps").join("custom-app-loader").join("target").join("release").join("inject_app.exe"))
                .unwrap_or_default(),
        ),
    ]
    .into_iter()
    .flatten();
    for c in candidates {
        if c.exists() {
            return c;
        }
    }
    home.join("loader").join("inject_app.exe")
}

/// The main plugin manager that coordinates all plugin operations.
pub struct PluginManager {
    /// Directory where plugins are stored.
    plugins_dir: PathBuf,
    /// Loaded plugins indexed by ID.
    plugins: RwLock<HashMap<String, PluginEntry>>,
    /// New packager (standalone wry loader + inject_app) for new-model apps.
    loader_packager: Arc<NewLoaderPackager>,
}

impl PluginManager {
    /// Create a new plugin manager.
    ///
    /// The loader paths (`hermix-app-loader.exe` + `inject_app.exe`) are
    /// resolved internally via [`resolve_loader_path`]/[`resolve_inject_path`]
    /// so callers (server, desktop) don't need to change their construction.
    pub fn new(plugins_dir: PathBuf) -> Self {
        let loader_path = resolve_loader_path();
        let inject_path = resolve_inject_path();
        let loader_packager = Arc::new(NewLoaderPackager::new(loader_path, inject_path));
        Self {
            plugins_dir,
            plugins: RwLock::new(HashMap::new()),
            loader_packager,
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
    /// `icon_data` is an optional base64-encoded PNG image that will be
    /// saved as `icon.png` in the plugin directory and referenced from
    /// the manifest's `icon_file` field.
    pub fn install_plugin(
        &self,
        manifest: &PluginManifest,
        index_html: Option<&str>,
        icon_data: Option<&str>,
    ) -> Result<PluginEntry, PluginError> {
        manifest
            .validate()
            .map_err(PluginError::InvalidManifest)?;

        let plugin_dir = self.plugins_dir.join(&manifest.id);

        // Remove existing if present
        if plugin_dir.exists() {
            std::fs::remove_dir_all(&plugin_dir)?;
        }

        // Create plugin directory
        std::fs::create_dir_all(&plugin_dir)?;

        // Write manifest.json
        let manifest_json = serde_json::to_string_pretty(manifest)?;
        std::fs::write(plugin_dir.join("manifest.json"), manifest_json)?;

        // Write self-contained index.html if provided
        let index_html_path = if let Some(code) = index_html {
            if !code.trim().is_empty() {
                let path = plugin_dir.join("index.html");
                std::fs::write(&path, code)?;
                Some(path)
            } else {
                None
            }
        } else {
            None
        };

        // Write icon.png from base64 data if provided; otherwise auto-generate
        // a per-app icon from the built-in library (deterministic by id/category).
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
                    tracing::warn!("自动生成应用图标失败 (使用默认壳图标): {e}");
                    None
                }
            }
        };

        // If icon was saved but manifest doesn't reference it, update the manifest
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
            icon_path,
            readme_path: None,
            dir: plugin_dir.clone(),
        };

        // Register in memory
        let mut plugins = self.plugins.write().expect("plugin lock poisoned");
        plugins.insert(manifest.id.clone(), entry.clone());

        tracing::info!("installed custom app: {}", manifest.id);
        Ok(entry)
    }

    /// Update an existing plugin's code and/or manifest.
    pub fn update_plugin(
        &self,
        id: &str,
        index_html: Option<&str>,
        manifest: Option<&PluginManifest>,
    ) -> Result<PluginEntry, PluginError> {
        let mut entry = self
            .get_plugin(id)
            .ok_or_else(|| PluginError::NotFound(id.to_string()))?;

        if let Some(code) = index_html {
            if !code.trim().is_empty() {
                let idx_path = entry.dir.join("index.html");
                std::fs::write(&idx_path, code)?;
                entry.index_html = Some(idx_path);
            }
        }

        // Update manifest if provided (e.g. to add missing permissions).
        // Writes manifest.json and updates the in-memory entry.
        if let Some(m) = manifest {
            m.validate()
                .map_err(PluginError::InvalidManifest)?;
            let manifest_json = serde_json::to_string_pretty(m)?;
            std::fs::write(entry.dir.join("manifest.json"), manifest_json)?;
            entry.manifest = m.clone();
        }

        // Re-register the updated entry in memory so get_plugin reflects it
        // until the next scan_plugins.
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

        // Remove directory
        std::fs::remove_dir_all(&plugin_dir)?;

        // Remove from memory
        let mut plugins = self.plugins.write().expect("plugin lock poisoned");
        plugins.remove(id);

        tracing::info!("uninstalled custom app: {}", id);
        Ok(())
    }

    /// Package a plugin as a standalone EXE.
    ///
    /// Apps (self-contained `index.html` + `manifest.json`) are
    /// packed via the standalone wry loader (`inject_app`). Emits
    /// `dist/<id>.exe`.
    pub async fn package_plugin(&self, id: &str) -> Result<PathBuf, PluginError> {
        let entry = self
            .get_plugin(id)
            .ok_or_else(|| PluginError::NotFound(id.to_string()))?;

        if entry.index_html.is_none() {
            return Err(PluginError::PackagingFailed(
                "应用缺少 index.html，无法打包".into(),
            ));
        }

        let result = self.loader_packager.package(&entry).await?;
        Ok(result.exe_path)
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
                "应用尚未打包, 找不到可执行文件: {}",
                exe_path.display()
            )));
        }
        std::process::Command::new(&exe_path)
            .spawn()
            .map_err(|e| PluginError::ExecutionFailed(format!("启动应用失败: {e}")))?;
        Ok(())
    }

    /// Get the plugins directory path.
    pub fn plugins_dir(&self) -> &Path {
        &self.plugins_dir
    }

    /// Get a reference to the new-loader packager.
    pub fn loader_packager(&self) -> &Arc<NewLoaderPackager> {
        &self.loader_packager
    }
}

/// Decode a base64-encoded icon image (PNG/ICO).
///
/// Accepts both raw base64 and data-URI format
/// (`data:image/png;base64,...`). Returns the raw bytes.
fn base64_decode_icon(data: &str) -> Result<Vec<u8>, PluginError> {
    let b64 = if data.starts_with("data:") {
        // data:image/png;base64,iVBOR...
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
        .map_err(|e| PluginError::InvalidManifest(format!("icon_data base64 解码失败: {e}")))
}
