//! Packager — bundles TypeScript code with esbuild and copies the precompiled shell.
//!
//! This module replaces the old DenoExecutor. Instead of running backend.ts
//! in a Deno subprocess, it:
//!   1. Uses esbuild to bundle backend.ts → backend.bundle.js
//!   2. Uses esbuild to bundle ui.tsx → ui.bundle.js (with React inlined)
//!   3. Copies the precompiled app-shell.exe to the output directory
//!   4. Writes resources/ (manifest.json, backend.bundle.js, ui.bundle.js)
//!
//! The result is a standalone directory that can be zipped and distributed.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::{PluginEntry, PluginError};

/// Result of a packaging operation.
#[derive(Debug, Clone)]
pub struct PackageResult {
    /// Path to the output directory containing the standalone app.
    pub output_dir: PathBuf,
    /// Path to the standalone EXE.
    pub exe_path: PathBuf,
    /// Size of the output directory in bytes.
    pub total_size: u64,
}

/// Packs plugins using esbuild + precompiled shell.
pub struct EsbuildPackager {
    /// Path to the precompiled app-shell.exe.
    shell_path: PathBuf,
    /// Path to the esbuild executable.
    esbuild_path: PathBuf,
}

impl EsbuildPackager {
    /// Create a new packager.
    pub fn new(shell_path: PathBuf, esbuild_path: PathBuf) -> Self {
        Self {
            shell_path,
            esbuild_path,
        }
    }

    /// Package a plugin into a standalone directory.
    pub async fn package(&self, entry: &PluginEntry) -> Result<PackageResult, PluginError> {
        // Validate shell exists
        if !self.shell_path.exists() {
            return Err(PluginError::EsbuildNotFound(format!(
                "app shell not found at {}",
                self.shell_path.display()
            )));
        }

        // Validate esbuild exists
        if !self.esbuild_path.exists() {
            return Err(PluginError::EsbuildNotFound(format!(
                "esbuild not found at {}",
                self.esbuild_path.display()
            )));
        }

        // Create output directory
        let output_dir = entry.dir.join("dist");
        if output_dir.exists() {
            std::fs::remove_dir_all(&output_dir).map_err(|e| {
                PluginError::PackagingFailed(format!("清理旧输出目录失败: {e}"))
            })?;
        }
        std::fs::create_dir_all(&output_dir)?;

        // Copy app shell
        let exe_name = if cfg!(windows) {
            format!("{}.exe", entry.manifest.id)
        } else {
            entry.manifest.id.clone()
        };
        let exe_path = output_dir.join(&exe_name);
        std::fs::copy(&self.shell_path, &exe_path).map_err(|e| {
            PluginError::PackagingFailed(format!("复制 app shell 失败: {e}"))
        })?;

        // Create resources directory
        let resources_dir = output_dir.join("resources");
        std::fs::create_dir_all(&resources_dir)?;

        // Copy custom icon file to resources if present
        if let Some(icon_path) = &entry.icon_path {
            if icon_path.exists() {
                let icon_name = icon_path.file_name().unwrap_or_default();
                std::fs::copy(icon_path, resources_dir.join(icon_name)).map_err(|e| {
                    PluginError::PackagingFailed(format!("复制图标文件失败: {e}"))
                })?;
                tracing::debug!("copied icon: {} → {}", icon_path.display(), resources_dir.join(icon_name).display());
            }
        }

        // Write manifest.json to resources
        let manifest_json = serde_json::to_string_pretty(&entry.manifest)?;
        std::fs::write(resources_dir.join("manifest.json"), manifest_json)?;

        // Bundle backend.ts
        if entry.backend_path.exists() {
            self.bundle_with_esbuild(
                &entry.backend_path,
                &resources_dir.join("backend.bundle.js"),
                "esm",
                false,
            )
            .await?;
        }

        // Bundle ui.tsx (if exists)
        if let Some(ui_path) = &entry.ui_path {
            if ui_path.exists() {
                self.bundle_with_esbuild(
                    ui_path,
                    &resources_dir.join("ui.bundle.js"),
                    "esm",
                    true,
                )
                .await?;
            }
        } else {
            // Generate a default UI if none provided
            let default_ui = generate_default_ui(&entry.manifest.name);
            std::fs::write(resources_dir.join("ui.bundle.js"), default_ui)?;
        }

        // Calculate total size
        let total_size = calculate_dir_size(&output_dir);

        tracing::info!(
            "packaged custom app '{}' → {} ({} bytes)",
            entry.manifest.id,
            output_dir.display(),
            total_size
        );

        Ok(PackageResult {
            output_dir,
            exe_path,
            total_size,
        })
    }

    /// Run esbuild to bundle a TypeScript file.
    async fn bundle_with_esbuild(
        &self,
        entry_path: &Path,
        output_path: &Path,
        format: &str,
        bundle_deps: bool,
    ) -> Result<(), PluginError> {
        let mut cmd = std::process::Command::new(&self.esbuild_path);

        cmd.arg(entry_path)
            .arg("--bundle")
            .arg(format!("--format={format}"))
            .arg("--platform=browser")
            .arg("--target=es2020")
            .arg("--outfile")
            .arg(output_path)
            .arg("--log-level=warning")
            .arg("--color=false");

        if bundle_deps {
            // For UI, we want to externalize React only if it's
            // imported from the shell. Otherwise, bundle everything.
            // Default: bundle everything for self-contained output.
        }

        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = cmd
            .output()
            .map_err(|e| PluginError::PackagingFailed(format!("esbuild 执行失败: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            return Err(PluginError::PackagingFailed(format!(
                "esbuild 打包失败 (exit {}):\nstderr: {}\nstdout: {}",
                output.status,
                stderr.chars().take(1000).collect::<String>(),
                stdout.chars().take(500).collect::<String>()
            )));
        }

        tracing::debug!(
            "esbuild bundled {} → {}",
            entry_path.display(),
            output_path.display()
        );
        Ok(())
    }
}

/// Calculate the total size of a directory.
fn calculate_dir_size(path: &Path) -> u64 {
    let mut total = 0;
    if path.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                let entry_path = entry.path();
                if entry_path.is_dir() {
                    total += calculate_dir_size(&entry_path);
                } else {
                    if let Ok(meta) = entry.metadata() {
                        total += meta.len();
                    }
                }
            }
        }
    } else if let Ok(meta) = std::fs::metadata(path) {
        total += meta.len();
    }
    total
}

/// Generate a minimal default UI for apps without ui.tsx.
fn generate_default_ui(app_name: &str) -> String {
    format!(
        r#"// Auto-generated default UI for "{app_name}"
const e = React.createElement;
function DefaultApp() {{
  return e('div', {{ style: {{ padding: '40px', textAlign: 'center', fontFamily: 'system-ui' }} }},
    e('h1', {{ style: {{ color: '#6c63ff' }} }}, '{app_name}'),
    e('p', {{ style: {{ color: '#888' }} }}, '此应用没有自定义 UI。后端结果将在控制台显示。'),
  );
}}
export default DefaultApp;
"#
    )
}
