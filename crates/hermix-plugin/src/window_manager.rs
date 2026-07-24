//! Window manager — opens custom apps for preview within the main application.
//!
//! In the WebView-Native architecture, "opening" a custom app means
//! launching the precompiled shell (app-shell.exe) as a child process
//! with the app's dist directory as the working directory. The shell
//! reads resources/manifest.json and loads the bundled JS.
//!
//! For in-app preview (without spawning a separate process), the
//! frontend can load the app's bundled UI in an iframe or a Webview
//! within the main Tauri window.

use std::path::PathBuf;

use crate::{PluginEntry, PluginError};

/// Manages opening custom apps in independent windows.
pub struct PluginWindowManager {
    /// Path to the precompiled app shell.
    shell_path: PathBuf,
}

impl PluginWindowManager {
    /// Create a new window manager.
    pub fn new(shell_path: PathBuf) -> Self {
        Self { shell_path }
    }

    /// Open a custom app in a standalone window by launching the shell.
    /// /// This spawns the precompiled `app-shell.exe` with the app's
    /// `dist/` directory as the working directory. The shell reads
    /// `resources/manifest.json` and renders the UI.
    pub async fn open_plugin(&self, entry: &PluginEntry) -> Result<(), PluginError> {
        let dist_dir = entry.dir.join("dist");

        if !dist_dir.exists() {
            return Err(PluginError::ExecutionFailed(format!(
                "应用尚未打包，请先执行打包操作。dist 目录不存在: {}",
                dist_dir.display()
            )));
        }

        let exe_path = if cfg!(windows) {
            dist_dir.join(format!("{}.exe", entry.manifest.id))
        } else {
            dist_dir.join(&entry.manifest.id)
        };

        if !exe_path.exists() {
            return Err(PluginError::ExecutionFailed(format!(
                "可执行文件不存在: {}。请先打包应用。",
                exe_path.display()
            )));
        }

        // Launch the standalone app
        let _child = tokio::process::Command::new(&exe_path)
            .current_dir(&dist_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| {
                PluginError::ExecutionFailed(format!("启动应用失败: {e}"))
            })?;

        tracing::info!(
            "launched custom app '{}' from {}",
            entry.manifest.id,
            exe_path.display()
        );

        Ok(())
    }

    /// Get the preview URL for in-app preview.
    /// /// Returns a file:// URL pointing to the app's bundled index.html
    /// (or the shell's index.html if the app doesn't have one).
    pub fn get_preview_url(&self, entry: &PluginEntry) -> Result<String, PluginError> {
        let dist_dir = entry.dir.join("dist");
        if !dist_dir.exists() {
            return Err(PluginError::ExecutionFailed(
                "应用尚未打包，请先执行打包操作。".into(),
            ));
        }

        // The shell's index.html is at the root of dist
        let index_path = dist_dir.join("index.html");
        let path = if index_path.exists() {
            index_path
        } else {
            // Fall back to shell's UI
            self.shell_path
                .parent()
                .map(|p| p.join("index.html"))
                .unwrap_or(index_path)
        };

        let url = format!(
            "file:///{}",
            path.to_string_lossy().replace('\\', "/")
        );
        Ok(url)
    }
}
