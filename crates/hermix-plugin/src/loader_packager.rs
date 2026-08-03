//! Neutralinojs packager — bundles custom apps using the Neutralinojs runtime.
//!
//! Two packaging modes:
//!
//! 1. **Sidecar (default, zero-dependency)**: Copies the pre-built
//!    `neutralino-win-x64.exe` binary + LLM-generated `index.html` +
//!    client library + generated `neutralino.config.json` into `dist/`.
//!    No Node.js, no `neu` CLI, no compilation — just file copying.
//!
//! 2. **neu build (optional, requires Node.js)**: Creates a proper
//!    Neutralinojs project, runs `neu build` to produce a compiled
//!    `resources.neur` bundle. Source code is hidden in the compiled
//!    bundle. Requires Node.js + `@neutralinojs/neu` CLI (one-time setup).
//!
//! ## Sidecar output layout
//! ```text
//! dist/
//!   <id>.exe                    ← renamed neutralino binary
//!   resources/
//!     index.html                 ← app UI (LLM-generated)
//!     js/
//!       neutralino.js            ← client library
//!   neutralino.config.json       ← generated from manifest
//!   icon.png                     ← app icon (optional)
//! ```

use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::{PluginEntry, PluginError};

/// Packaging mode selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageMode {
    /// Zero-dependency: copy binary + resources. Default.
    Sidecar,
    /// Professional build: `neu build` → compiled resources.neur.
    /// Requires Node.js + @neutralinojs/neu CLI.
    NeuBuild,
}

impl Default for PackageMode {
    fn default() -> Self {
        Self::Sidecar
    }
}

impl std::str::FromStr for PackageMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "sidecar" => Ok(Self::Sidecar),
            "neu-build" | "neubuild" | "neu" => Ok(Self::NeuBuild),
            _ => Err(format!(
                "unknown build mode '{s}', expected 'sidecar' or 'neu-build'"
            )),
        }
    }
}

/// Result of a packaging operation.
#[derive(Debug, Clone)]
pub struct PackageResult {
    /// Path to the output directory containing the standalone app.
    pub output_dir: PathBuf,
    /// Path to the standalone EXE.
    pub exe_path: PathBuf,
    /// Size of the output directory in bytes.
    pub total_size: u64,
    /// Which mode was used.
    pub mode: PackageMode,
}

/// RAII guard that deletes a temp directory when dropped.
struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Packs custom apps using the Neutralinojs runtime binary.
pub struct NeutralinoPackager {
    /// Path to the pre-built `neutralino-win-x64.exe` binary.
    binary_path: PathBuf,
    /// Path to the `neutralino.js` client library.
    client_lib_path: PathBuf,
    /// Path to the `esbuild.exe` binary (for TypeScript/TSX transpilation).
    esbuild_path: PathBuf,
}

impl NeutralinoPackager {
    pub fn new(binary_path: PathBuf, client_lib_path: PathBuf, esbuild_path: PathBuf) -> Self {
        Self {
            binary_path,
            client_lib_path,
            esbuild_path,
        }
    }

    /// Package a custom app using the specified mode.
    pub async fn package(
        &self,
        entry: &PluginEntry,
        mode: PackageMode,
    ) -> Result<PackageResult, PluginError> {
        match mode {
            PackageMode::Sidecar => self.package_sidecar(entry).await,
            PackageMode::NeuBuild => self.package_neu_build(entry).await,
        }
    }

    /// Sidecar mode: copy binary + resources + generated config.
    ///
    /// Zero external dependencies — just file operations.
    /// If `src/main.tsx` exists, runs esbuild to transpile TypeScript/React → app.js.
    async fn package_sidecar(&self, entry: &PluginEntry) -> Result<PackageResult, PluginError> {
        let index_html = entry
            .index_html
            .as_ref()
            .ok_or_else(|| {
                PluginError::PackagingFailed(
                    "应用缺少 index.html (Neutralinojs 应用需要自包含 index.html)".into(),
                )
            })?;

        if !self.binary_path.exists() {
            return Err(PluginError::LoaderNotFound(format!(
                "neutralino 二进制未找到: {} (请确认已随宿主打包或位于 resources/loader/)",
                self.binary_path.display()
            )));
        }

        if !self.client_lib_path.exists() {
            return Err(PluginError::LoaderNotFound(format!(
                "neutralino.js 客户端库未找到: {}",
                self.client_lib_path.display()
            )));
        }

        let output_dir = entry.dir.join("dist");
        if output_dir.exists() {
            std::fs::remove_dir_all(&output_dir).map_err(|e| {
                PluginError::PackagingFailed(format!("清理旧输出目录失败: {e}"))
            })?;
        }
        std::fs::create_dir_all(&output_dir)?;

        // ── Step 1: Copy neutralino binary → dist/<id>.exe ──
        let exe_name = if cfg!(windows) {
            format!("{}.exe", entry.manifest.id)
        } else {
            entry.manifest.id.clone()
        };
        let exe_path = output_dir.join(&exe_name);
        std::fs::copy(&self.binary_path, &exe_path).map_err(|e| {
            PluginError::PackagingFailed(format!("复制 neutralino 二进制失败: {e}"))
        })?;

        // ── Step 2: Create resources/ directory ──
        let resources_dir = output_dir.join("resources");
        let js_dir = resources_dir.join("js");
        std::fs::create_dir_all(&js_dir)?;

        // ── Step 2a: If TypeScript source exists, run esbuild ──
        if let Some(app_source) = &entry.app_source {
            if app_source.exists() {
                self.build_typescript(app_source, &js_dir, entry)?;
            }
        }

        // Copy index.html
        std::fs::copy(index_html, resources_dir.join("index.html")).map_err(|e| {
            PluginError::PackagingFailed(format!("复制 index.html 失败: {e}"))
        })?;

        // Copy neutralino.js client library
        std::fs::copy(&self.client_lib_path, js_dir.join("neutralino.js")).map_err(|e| {
            PluginError::PackagingFailed(format!("复制 neutralino.js 失败: {e}"))
        })?;

        // ── Step 3: Generate neutralino.config.json from manifest ──
        let config = crate::manifest::generate_neutralino_config(&entry.manifest);
        let config_json = serde_json::to_string_pretty(&config)?;
        std::fs::write(
            output_dir.join("neutralino.config.json"),
            config_json,
        )?;

        // ── Step 4: Copy icon.png (optional) ──
        if let Some(icon_path) = &entry.icon_path {
            if icon_path.exists() {
                let _ = std::fs::copy(icon_path, resources_dir.join("icon.png"));
            }
        }

        // ── Step 5: Patch exe icon (optional, non-fatal) ──
        #[cfg(windows)]
        {
            let icon_bytes_opt = entry
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
                });

            if icon_bytes_opt.is_some() {
                if let Ok((group, icons, _ico)) =
                    crate::icon_library::generate_icon_resources(
                        &entry.manifest.name,
                        &entry.manifest.id,
                        &entry.manifest.category,
                    )
                {
                    if let Err(e) = crate::icon_library::patch_exe_icon(&exe_path, &group, &icons)
                    {
                        tracing::warn!("为 exe 打图标补丁失败 (不影响运行): {e}");
                    }
                }
            }
        }

        let total_size = calculate_dir_size(&output_dir);

        tracing::info!(
            "packaged (sidecar) custom app '{}' → {} ({} bytes)",
            entry.manifest.id,
            output_dir.display(),
            total_size
        );

        Ok(PackageResult {
            output_dir,
            exe_path,
            total_size,
            mode: PackageMode::Sidecar,
        })
    }

    /// Run esbuild to transpile TypeScript/TSX → JavaScript.
    ///
    /// esbuild is invoked as a subprocess:
    /// ```bash
    /// esbuild src/main.tsx --bundle --outfile=resources/js/app.js \
    ///   --format=iife --jsx=transform --loader:.tsx=tsx --loader:.ts=ts \
    ///   --target=es2020
    /// ```
    ///
    /// - `--bundle` resolves relative imports (e.g. `import { Header } from './Header'`)
    /// - `--jsx=transform` converts JSX to `React.createElement(...)` (uses global React)
    /// - `--format=iife` wraps output in IIFE for browser execution
    /// - React/ReactDOM are loaded via CDN script tags in index.html (global vars)
    fn build_typescript(
        &self,
        entry_source: &Path,
        js_dir: &Path,
        entry: &PluginEntry,
    ) -> Result<(), PluginError> {
        if !self.esbuild_path.exists() {
            return Err(PluginError::LoaderNotFound(format!(
                "esbuild 二进制未找到: {} (TypeScript 转译需要 esbuild.exe，请确认位于 resources/loader/)",
                self.esbuild_path.display()
            )));
        }

        let outfile = js_dir.join("app.js");
        std::fs::create_dir_all(js_dir)?;

        // esbuild args:
        // --bundle: resolve relative imports
        // --format=iife: wrap in IIFE for browser
        // --jsx=transform: React.createElement (uses global React from CDN)
        // --loader:.tsx=tsx --loader:.ts=ts: TypeScript file loaders
        // --target=es2020: modern browsers
        let out = std::process::Command::new(&self.esbuild_path)
            .arg(entry_source)
            .arg("--bundle")
            .arg("--outfile").arg(&outfile)
            .arg("--format=iife")
            .arg("--jsx=transform")
            .arg("--loader:.tsx=tsx")
            .arg("--loader:.ts=ts")
            .arg("--target=es2020")
            .arg("--color=false")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| {
                PluginError::PackagingFailed(format!(
                    "esbuild 执行失败: {e}。路径: {}",
                    self.esbuild_path.display()
                ))
            })?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(PluginError::PackagingFailed(format!(
                "esbuild 转译失败 (exit {})。\n{}\n\
                 提示: 确保代码中不要 `import React from 'react'`，\n\
                 而是使用全局变量: `const {{ useState }} = React;`",
                out.status,
                stderr.chars().take(3000).collect::<String>()
            )));
        }

        // Verify output was created
        if !outfile.exists() {
            return Err(PluginError::PackagingFailed(
                "esbuild 执行成功但未产出 app.js — 请检查源码语法".into()
            ));
        }

        tracing::info!(
            "esbuild transpiled {} → {} ({} bytes) for app '{}'",
            entry_source.display(),
            outfile.display(),
            std::fs::metadata(&outfile).map(|m| m.len()).unwrap_or(0),
            entry.manifest.id
        );

        Ok(())
    }

    /// neu build mode: create a proper Neutralinojs project and run `neu build`.
    ///
    /// Requires Node.js + @neutralinojs/neu CLI.
    async fn package_neu_build(&self, entry: &PluginEntry) -> Result<PackageResult, PluginError> {
        let index_html = entry
            .index_html
            .as_ref()
            .ok_or_else(|| {
                PluginError::PackagingFailed(
                    "应用缺少 index.html (Neutralinojs 应用需要自包含 index.html)".into(),
                )
            })?;

        // Check environment
        let env_check = crate::node_setup::check_neu_environment();
        match env_check {
            crate::node_setup::NeuEnvironment::Ready => { /* good to go */ }
            crate::node_setup::NeuEnvironment::MissingNode => {
                return Err(PluginError::PackagingFailed(
                    "neu build 模式需要 Node.js。请安装 Node.js (https://nodejs.org) 或使用 sidecar 模式。".into(),
                ));
            }
            crate::node_setup::NeuEnvironment::MissingNeu => {
                // Try to auto-install neu CLI
                crate::node_setup::install_neu_cli()
                    .map_err(|e| PluginError::PackagingFailed(format!("安装 neu CLI 失败: {e}")))?;
            }
        }

        // Create a temporary Neutralinojs project
        let temp_dir = std::env::temp_dir().join(format!(
            "hermix-neu-build-{}-{}",
            entry.manifest.id,
            std::process::id()
        ));
        if temp_dir.exists() {
            std::fs::remove_dir_all(&temp_dir)?;
        }
        let _guard = TempDirGuard(temp_dir.clone());

        // Create project structure
        let resources_dir = temp_dir.join("resources");
        let js_dir = resources_dir.join("js");
        std::fs::create_dir_all(&js_dir)?;

        // ── If TypeScript source exists, run esbuild ──
        if let Some(app_source) = &entry.app_source {
            if app_source.exists() {
                self.build_typescript(app_source, &js_dir, entry)?;
            }
        }

        // Copy index.html
        std::fs::copy(index_html, resources_dir.join("index.html"))?;

        // Copy neutralino.js client library
        std::fs::copy(&self.client_lib_path, js_dir.join("neutralino.js"))?;

        // Generate neutralino.config.json
        let mut config = crate::manifest::generate_neutralino_config(&entry.manifest);
        // For neu build, set the CLI section
        if let Some(obj) = config.as_object_mut() {
            obj.insert(
                "cli".into(),
                serde_json::json!({
                    "binaryName": entry.manifest.id,
                    "resourcesPath": "/resources/",
                    "extensionsPath": "/extensions/",
                    "clientLibrary": "/resources/js/neutralino.js",
                }),
            );
        }
        std::fs::write(
            temp_dir.join("neutralino.config.json"),
            serde_json::to_string_pretty(&config)?,
        )?;

        // Copy icon
        if let Some(icon_path) = &entry.icon_path {
            if icon_path.exists() {
                std::fs::copy(icon_path, resources_dir.join("icon.png"))?;
            }
        }

        // Run `neu build` in the temp directory
        let out = std::process::Command::new("neu")
            .arg("build")
            .arg("--release")
            .current_dir(&temp_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| {
                PluginError::PackagingFailed(format!(
                    "neu build 执行失败: {e}。请确认 @neutralinojs/neu 已全局安装 (npm install -g @neutralinojs/neu)"
                ))
            })?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stdout = String::from_utf8_lossy(&out.stdout);
            return Err(PluginError::PackagingFailed(format!(
                "neu build 失败 (exit {}):\nstderr: {}\nstdout: {}",
                out.status,
                stderr.chars().take(2000).collect::<String>(),
                stdout.chars().take(500).collect::<String>()
            )));
        }

        // Copy output to dist/
        let output_dir = entry.dir.join("dist");
        if output_dir.exists() {
            std::fs::remove_dir_all(&output_dir)?;
        }
        std::fs::create_dir_all(&output_dir)?;

        // neu build produces: dist/<binaryName>-<platform>.exe + resources.neur
        // The exact output path depends on neu version, check common locations
        let neu_dist = temp_dir.join("dist");
        let exe_name = if cfg!(windows) {
            format!("{}-win_x64.exe", entry.manifest.id)
        } else {
            format!("{}-linux_x64", entry.manifest.id)
        };
        let neu_exe = neu_dist.join(&exe_name);

        if neu_exe.exists() {
            let final_exe = output_dir.join(if cfg!(windows) {
                format!("{}.exe", entry.manifest.id)
            } else {
                entry.manifest.id.clone()
            });
            std::fs::copy(&neu_exe, &final_exe)?;

            // Copy resources.neur if it exists
            let neur_path = neu_dist.join("resources.neur");
            if neur_path.exists() {
                std::fs::copy(&neur_path, output_dir.join("resources.neur"))?;
            }

            // Copy config
            let config_src = temp_dir.join("neutralino.config.json");
            if config_src.exists() {
                std::fs::copy(&config_src, output_dir.join("neutralino.config.json"))?;
            }

            // Copy icon
            if let Some(icon_path) = &entry.icon_path {
                if icon_path.exists() {
                    std::fs::copy(icon_path, output_dir.join("icon.png"))?;
                }
            }

            // Patch icon on the final exe
            #[cfg(windows)]
            {
                if let Ok((group, icons, _ico)) = crate::icon_library::generate_icon_resources(
                    &entry.manifest.name,
                    &entry.manifest.id,
                    &entry.manifest.category,
                ) {
                    if let Err(e) = crate::icon_library::patch_exe_icon(&final_exe, &group, &icons)
                    {
                        tracing::warn!("为 exe 打图标补丁失败 (不影响运行): {e}");
                    }
                }
            }

            let total_size = calculate_dir_size(&output_dir);

            tracing::info!(
                "packaged (neu-build) custom app '{}' → {} ({} bytes)",
                entry.manifest.id,
                output_dir.display(),
                total_size
            );

            Ok(PackageResult {
                output_dir,
                exe_path: final_exe,
                total_size,
                mode: PackageMode::NeuBuild,
            })
        } else {
            // Fallback: copy all files from neu dist
            if neu_dist.exists() {
                copy_dir_recursive(&neu_dist, &output_dir)?;
                let total_size = calculate_dir_size(&output_dir);
                let exe_path = output_dir.join(if cfg!(windows) {
                    format!("{}.exe", entry.manifest.id)
                } else {
                    entry.manifest.id.clone()
                });

                Ok(PackageResult {
                    output_dir,
                    exe_path,
                    total_size,
                    mode: PackageMode::NeuBuild,
                })
            } else {
                Err(PluginError::PackagingFailed(format!(
                    "neu build 未产出预期文件。dist 目录: {} 存在={} ",
                    neu_dist.display(),
                    neu_dist.exists()
                )))
            }
        }
    }
}

/// Recursively copy a directory.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), PluginError> {
    if !dst.exists() {
        std::fs::create_dir_all(dst)?;
    }
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
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
