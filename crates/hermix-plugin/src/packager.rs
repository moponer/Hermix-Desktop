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
    ///
    /// NOTE: retained for API compatibility with `PluginManager::new`, but
    /// the effective path is re-resolved per call in `package()` so that
    /// first-launch extraction / manual drops take effect without a restart.
    #[allow(dead_code)]
    shell_path: PathBuf,
    /// Path to the esbuild executable (see `shell_path` note above).
    #[allow(dead_code)]
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
        // Resolve shell/esbuild paths at *call time* (not cached at
        // construction). The desktop app extracts them on first launch and
        // the user may also drop them in manually; re-resolving here means
        // neither requires restarting the server to take effect.
        let shell_path = crate::resolve_shell_path();
        let esbuild_path = crate::resolve_esbuild_path();

        // Validate shell exists
        if !shell_path.exists() {
            return Err(PluginError::ShellNotFound(format!(
                "app shell not found at {}",
                shell_path.display()
            )));
        }

        // Validate esbuild exists
        if !esbuild_path.exists() {
            return Err(PluginError::EsbuildNotFound(format!(
                "esbuild not found at {}",
                esbuild_path.display()
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
        std::fs::copy(&shell_path, &exe_path).map_err(|e| {
            PluginError::PackagingFailed(format!("复制 app shell 失败: {e}"))
        })?;

        // Create resources directory
        let resources_dir = output_dir.join("resources");
        std::fs::create_dir_all(&resources_dir)?;

        // Resolve the app icon: prefer an existing icon_file (user/agent supplied),
        // otherwise auto-generate a per-app icon from the built-in library.
        // This guarantees the shell never falls back to the default (main-program)
        // icon.
        let existing_icon = entry
            .icon_path
            .as_ref()
            .filter(|p| p.exists())
            .and_then(|p| std::fs::read(p).ok());
        let icon_bytes: Option<Vec<u8>> = existing_icon.or_else(|| {
            crate::icon_library::generate_icon_png(
                &entry.manifest.name,
                &entry.manifest.id,
                &entry.manifest.category,
            )
            .map_err(|e| {
                tracing::warn!("打包时自动生成图标失败 (使用默认壳图标): {e}");
                e
            })
            .ok()
        });

        if let Some(bytes) = icon_bytes {
            std::fs::write(resources_dir.join("icon.png"), &bytes).map_err(|e| {
                PluginError::PackagingFailed(format!("写入图标失败: {e}"))
            })?;
            // Persist to the app dir for stability across re-packages.
            let _ = std::fs::write(entry.dir.join("icon.png"), &bytes);
            // Ensure the manifest references the icon (for the runtime shell).
            if entry.manifest.icon_file.as_deref() != Some("icon.png") {
                let mut m = entry.manifest.clone();
                m.icon_file = Some("icon.png".to_string());
                let mj = serde_json::to_string_pretty(&m)?;
                std::fs::write(resources_dir.join("manifest.json"), &mj)?;
                let _ = std::fs::write(entry.dir.join("manifest.json"), &mj);
            } else {
                // Write manifest.json to resources
                let manifest_json = serde_json::to_string_pretty(&entry.manifest)?;
                std::fs::write(resources_dir.join("manifest.json"), manifest_json)?;
            }

            // Layer 2: patch the copied app-shell.exe's embedded icon so the
            // Explorer/taskbar file icon matches the app (not the main program).
            // Failures are non-fatal — the .exe still runs fine.
            #[cfg(windows)]
            if let Ok((group, icons, _ico)) = crate::icon_library::generate_icon_resources(
                &entry.manifest.name,
                &entry.manifest.id,
                &entry.manifest.category,
            ) {
                if let Err(e) =
                    crate::icon_library::patch_exe_icon(&exe_path, &group, &icons)
                {
                    tracing::warn!("打补丁 exe 图标失败 (不影响运行): {e}");
                } else {
                    tracing::debug!("已为 {} 打补丁内嵌图标", exe_path.display());
                }
            }
        } else {
            // Write manifest.json to resources (no icon available)
            let manifest_json = serde_json::to_string_pretty(&entry.manifest)?;
            std::fs::write(resources_dir.join("manifest.json"), manifest_json)?;
        }

        // Bundle backend.ts
        if entry.backend_path.exists() {
            self.bundle_with_esbuild(
                &esbuild_path,
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
                    &esbuild_path,
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
        esbuild_path: &Path,
        entry_path: &Path,
        output_path: &Path,
        format: &str,
        bundle_deps: bool,
    ) -> Result<(), PluginError> {
        let mut cmd = std::process::Command::new(esbuild_path);

        cmd.arg(entry_path)
            .arg("--bundle")
            .arg(format!("--format={format}"))
            .arg("--platform=browser")
            .arg("--target=es2020")
            // IMPORTANT: esbuild 0.21+ rejects the space-separated
            // `--outfile <path>` form and reports "Invalid build flag".
            // The `--outfile=<path>` (equals) form is required and works
            // across all versions.
            .arg(format!("--outfile={}", output_path.display()))
            .arg("--log-level=warning")
            .arg("--color=false");

        if bundle_deps {
            // UI bundles run inside the app-shell loader, which provides
            // React/ReactDOM via an import map (esm.sh). Externalizing them
            // avoids "Could not resolve 'react'" during esbuild and prevents
            // duplicate React instances at runtime.
            cmd.arg("--external:react")
                .arg("--external:react-dom/client")
                .arg("--external:react/jsx-runtime");
            // Use automatic JSX runtime so that esbuild produces `_jsx()` calls
            // instead of `React.createElement()`. This eliminates the need for
            // `React` to be in scope (the classic runtime requires a default
            // import of React which esbuild --external:react doesn't inject).
            // The app-shell loader's `rewriteBareImports` rewrites
            // `react/jsx-runtime` bare imports to the full esm.sh URL.
            cmd.arg("--jsx=automatic")
                .arg("--jsx-import-source=react");
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
import {{ createElement as e }} from "react";
function DefaultApp() {{
  return e('div', {{ style: {{ padding: '40px', textAlign: 'center', fontFamily: 'system-ui' }} }},
    e('h1', {{ style: {{ color: '#232328' }} }}, '{app_name}'),
    e('p', {{ style: {{ color: '#888' }} }}, '此应用没有自定义 UI。后端结果将在控制台显示。'),
  );
}}
export default DefaultApp;
"#
    )
}
