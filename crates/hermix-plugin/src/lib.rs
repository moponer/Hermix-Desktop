//! # hermix-plugin
//!
//! 自定义应用系统 — 面向终端用户的桌面窗口应用。
//!
//! 与 LLM 驱动的插件 (hermix-tools) 不同，自定义应用是用户自己使用的
//! 独立桌面窗口应用。用户通过对话让 LLM 生成应用代码，系统自动打包
//! 为独立可执行文件。
//!
//! ## 架构 (WebView-Native 方案)
//!
//! - **PluginManager**: 扫描应用目录、管理生命周期
//! - **PluginManifest**: 定义应用元数据、窗口配置、权限
//! - **EsbuildPackager**: 用 esbuild 打包 TypeScript，复制预编译壳
//! - **PluginWindowManager**: 在主窗口内预览运行应用
//! - **无 Deno / QuickJS**: 后端代码编译后在 WebView2 (V8) 中运行

pub mod manifest;
pub mod scanner;
pub mod packager;
pub mod window_manager;
pub mod api;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

pub use manifest::{PluginDependencies, PluginManifest, WindowConfig};
pub use packager::{EsbuildPackager, PackageResult};
pub use scanner::PluginScanner;
pub use window_manager::PluginWindowManager;

/// A loaded plugin entry with its manifest and file paths.
#[derive(Debug, Clone)]
pub struct PluginEntry {
    /// Plugin manifest parsed from manifest.json.
    pub manifest: PluginManifest,
    /// Path to backend.ts
    pub backend_path: PathBuf,
    /// Path to ui.tsx (optional)
    pub ui_path: Option<PathBuf>,
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

    #[error("app shell not found: {0}")]
    ShellNotFound(String),

    #[error("esbuild not found: {0}")]
    EsbuildNotFound(String),

    #[error("packaging failed: {0}")]
    PackagingFailed(String),

    #[error("execution failed: {0}")]
    ExecutionFailed(String),
}

/// Resolve the path to the precompiled app shell (`app-shell.exe`).
///
/// Candidate priority (mirrors the Tauri custom-app commands so the
/// HTTP/WS server and the desktop UI agree on where the binary lives):
///   1. `HERMIX_HOME/shell/app-shell.exe` — extracted on first launch by the
///      desktop app's `ensure_app_shell_and_esbuild`.
///   2. `current_exe/resources/shell/app-shell.exe` — portable/installed build.
///   3. `current_exe/shell/app-shell.exe` — adjacent-layout fallback.
///   4. `HERMIX_HOME/shell/app-shell.exe` — default (may not exist yet; the
///      packager returns a clear `ShellNotFound` error in that case).
pub fn resolve_shell_path() -> PathBuf {
    let home = hermix_config::paths::hermix_home();
    let candidates = [
        Some(home.join("shell").join("app-shell.exe")),
        std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.join("resources").join("shell").join("app-shell.exe"))),
        std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.join("shell").join("app-shell.exe"))),
    ]
    .into_iter()
    .flatten();
    for c in candidates {
        if c.exists() {
            return c;
        }
    }
    home.join("shell").join("app-shell.exe")
}

/// Resolve the path to the bundled esbuild executable (`esbuild.exe`).
/// Same candidate priority as [`resolve_shell_path`], with `esbuild` substituted.
pub fn resolve_esbuild_path() -> PathBuf {
    let home = hermix_config::paths::hermix_home();
    let candidates = [
        Some(home.join("esbuild").join("esbuild.exe")),
        std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.join("resources").join("esbuild").join("esbuild.exe"))),
        std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.join("esbuild").join("esbuild.exe"))),
    ]
    .into_iter()
    .flatten();
    for c in candidates {
        if c.exists() {
            return c;
        }
    }
    home.join("esbuild").join("esbuild.exe")
}

/// The main plugin manager that coordinates all plugin operations.
pub struct PluginManager {
    /// Directory where plugins are stored.
    plugins_dir: PathBuf,
    /// Loaded plugins indexed by ID.
    plugins: RwLock<HashMap<String, PluginEntry>>,
    /// Packager for bundling plugins as standalone EXEs.
    packager: Arc<EsbuildPackager>,
    /// Path to the precompiled app shell (app-shell.exe).
    shell_path: PathBuf,
}

impl PluginManager {
    /// Create a new plugin manager.
    /// /// `shell_path` is the path to the precompiled `app-shell.exe`.
    /// `esbuild_path` is the path to the bundled `esbuild.exe`.
    pub fn new(plugins_dir: PathBuf, shell_path: PathBuf, esbuild_path: PathBuf) -> Self {
        let packager = Arc::new(EsbuildPackager::new(shell_path.clone(), esbuild_path));
        Self {
            plugins_dir,
            plugins: RwLock::new(HashMap::new()),
            packager,
            shell_path,
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
    /// /// `icon_data` is an optional base64-encoded PNG image that will be
    /// saved as `icon.png` in the plugin directory and referenced from
    /// the manifest's `icon_file` field.
    pub fn install_plugin(
        &self,
        manifest: &PluginManifest,
        backend_code: &str,
        ui_code: Option<&str>,
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

        // Write backend.ts
        std::fs::write(plugin_dir.join("backend.ts"), backend_code)?;

        // Write ui.tsx if provided
        let ui_path = if let Some(code) = ui_code {
            let path = plugin_dir.join("ui.tsx");
            std::fs::write(&path, code)?;
            Some(path)
        } else {
            None
        };

        // Write icon.png from base64 data if provided
        let icon_path = if let Some(data) = icon_data {
            let bytes = base64_decode_icon(data)?;
            let path = plugin_dir.join("icon.png");
            std::fs::write(&path, bytes)?;
            Some(path)
        } else {
            None
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
            backend_path: plugin_dir.join("backend.ts"),
            ui_path,
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

    /// Update an existing plugin's code.
    pub fn update_plugin(
        &self,
        id: &str,
        backend_code: Option<&str>,
        ui_code: Option<&str>,
    ) -> Result<PluginEntry, PluginError> {
        let entry = self
            .get_plugin(id)
            .ok_or_else(|| PluginError::NotFound(id.to_string()))?;

        if let Some(code) = backend_code {
            std::fs::write(&entry.backend_path, code)?;
        }

        if let Some(code) = ui_code {
            let ui_path = entry.dir.join("ui.tsx");
            std::fs::write(&ui_path, code)?;
        }

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
    /// /// This uses esbuild to bundle the TypeScript code and copies the
    /// precompiled app shell — no Rust/Tauri toolchain needed.
    pub async fn package_plugin(&self, id: &str) -> Result<PathBuf, PluginError> {
        let entry = self
            .get_plugin(id)
            .ok_or_else(|| PluginError::NotFound(id.to_string()))?;

        let result = self.packager.package(&entry).await?;
        Ok(result.exe_path)
    }

    /// Get the plugins directory path.
    pub fn plugins_dir(&self) -> &Path {
        &self.plugins_dir
    }

    /// Get the shell path.
    pub fn shell_path(&self) -> &Path {
        &self.shell_path
    }

    /// Get a reference to the packager.
    pub fn packager(&self) -> &Arc<EsbuildPackager> {
        &self.packager
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
