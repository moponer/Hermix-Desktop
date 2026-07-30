//! New-loader packager — bundles a self-contained custom app via the
//! standalone wry loader (`hermix-app-loader.exe`) + `inject_app.exe`.
//!
//! - The app is a **self-contained `index.html`** (talks to the bridge via
//!   `fetch("app://localhost/rpc", { method: "POST" })`) + a `manifest.json`
//!   declaring `permissions`.
//! - Packaging is just: `inject_app <loader.exe> <index.html> <manifest.json>
//!   <output.exe>` — it appends a payload trailer (manifest + html) to a copy
//!   of the loader, producing one standalone `.exe`. No TypeScript compile,
//!   no esbuild, no Tauri toolchain.
//! - At runtime the produced exe reads its own tail, serves the html over the
//!   `app://` protocol, and gates bridge calls by the manifest's permissions.

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

/// Packs a new-model custom app into a standalone EXE using the loader.
pub struct NewLoaderPackager {
    /// Path to the prebuilt `hermix-app-loader.exe`.
    loader_path: PathBuf,
    /// Path to the `inject_app.exe` packer.
    inject_path: PathBuf,
}

impl NewLoaderPackager {
    pub fn new(loader_path: PathBuf, inject_path: PathBuf) -> Self {
        Self {
            loader_path,
            inject_path,
        }
    }

    /// Package a new-model plugin (`index.html` + `manifest.json`) into a
    /// standalone EXE under `dist/`.
    pub async fn package(&self, entry: &PluginEntry) -> Result<PackageResult, PluginError> {
        let index_html = entry
            .index_html
            .as_ref()
            .ok_or_else(|| {
                PluginError::PackagingFailed(
                    "新架构应用缺少 index.html (new-loader 模型需要自包含 index.html + manifest.json)"
                        .into(),
                )
            })?;

        let manifest_path = entry.dir.join("manifest.json");
        if !manifest_path.exists() {
            return Err(PluginError::PackagingFailed(format!(
                "应用缺少 manifest.json: {}",
                manifest_path.display()
            )));
        }

        if !self.loader_path.exists() {
            return Err(PluginError::LoaderNotFound(format!(
                "app loader 未找到: {} (请确认已随宿主打包或位于 apps/custom-app-loader/target/release/)",
                self.loader_path.display()
            )));
        }
        if !self.inject_path.exists() {
            return Err(PluginError::LoaderNotFound(format!(
                "inject_app 未找到: {}",
                self.inject_path.display()
            )));
        }

        let output_dir = entry.dir.join("dist");
        if output_dir.exists() {
            std::fs::remove_dir_all(&output_dir).map_err(|e| {
                PluginError::PackagingFailed(format!("清理旧输出目录失败: {e}"))
            })?;
        }
        std::fs::create_dir_all(&output_dir)?;

        let exe_name = if cfg!(windows) {
            format!("{}.exe", entry.manifest.id)
        } else {
            entry.manifest.id.clone()
        };
        let exe_path = output_dir.join(&exe_name);

        let out = std::process::Command::new(&self.inject_path)
            .arg(&self.loader_path)
            .arg(index_html)
            .arg(&manifest_path)
            .arg(&exe_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| PluginError::PackagingFailed(format!("inject_app 执行失败: {e}")))?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stdout = String::from_utf8_lossy(&out.stdout);
            return Err(PluginError::PackagingFailed(format!(
                "inject_app 打包失败 (exit {}):\nstderr: {}\nstdout: {}",
                out.status,
                stderr.chars().take(1000).collect::<String>(),
                stdout.chars().take(300).collect::<String>()
            )));
        }

        // Cosmetic: copy + patch an embedded icon so Explorer/taskbar shows
        // the app icon, not the loader's default. Failures are non-fatal.
        #[cfg(windows)]
        if let Some(icon_bytes) = entry
            .icon_path
            .as_ref()
            .filter(|p| p.exists())
            .and_then(|p| std::fs::read(p).ok())
            .or_else(|| {
                crate::icon_library::generate_icon_png(
                    &entry.manifest.name,
                    &entry.manifest.id,
                    &entry.manifest.category,
                )
                .ok()
            })
        {
            let _ = std::fs::write(output_dir.join("icon.png"), &icon_bytes);
            if let Ok((group, icons, _ico)) = crate::icon_library::generate_icon_resources(
                &entry.manifest.name,
                &entry.manifest.id,
                &entry.manifest.category,
            ) {
                if let Err(e) =
                    crate::icon_library::patch_exe_icon(&exe_path, &group, &icons)
                {
                    tracing::warn!("为 new-loader 应用打补丁 exe 图标失败 (不影响运行): {e}");
                }
            }
        }

        let total_size = calculate_dir_size(&output_dir);

        tracing::info!(
            "packaged (new-loader) custom app '{}' → {} ({} bytes)",
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
}

fn calculate_dir_size(path: &Path) -> u64 {
    let mut total = 0;
    if path.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                let entry_path = entry.path();
                if entry_path.is_dir() {
                    total += calculate_dir_size(&entry_path);
                } else if let Ok(meta) = entry_path.metadata() {
                    total += meta.len();
                }
            }
        }
    } else if let Ok(meta) = path.metadata() {
        total += meta.len();
    }
    total
}
