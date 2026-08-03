//! hx-shell packager — 尾部注入模板壳生成单文件 exe
//!
//! 用户机器零编译：打包 = 纯文件拼接（HTML + len:8LE + MAGIC）。
//! 壳启动时读自身尾部 payload 加载页面，否则加载外部 resources/ 目录（开发模式）。
//!
//! 保留 `PackageMode` 枚举是为了与历史 API 兼容（hermix-tools/commands/dashboard
//! 都引用 `hermix_plugin::PackageMode::Sidecar/NeuBuild`）。本打包器两种模式等价。

use std::path::{Path, PathBuf};

/// 打包模式（兼容枚举）。hx-shell 只有一种打包路径，两种模式结果相同。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageMode {
    /// 单文件打包（壳尾部注入 HTML，零依赖）
    Sidecar,
    /// 预留：同 Sidecar（兼容历史枚举名）。
    NeuBuild,
}

impl Default for PackageMode {
    fn default() -> Self { Self::Sidecar }
}

impl std::str::FromStr for PackageMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "sidecar" => Ok(Self::Sidecar),
            "neu-build" | "neubuild" | "neu" => Ok(Self::NeuBuild),
            _ => Err(format!("unknown package mode '{s}', expected 'sidecar' or 'neu-build'")),
        }
    }
}

const MAGIC: &[u8] = b"HXAPP1";

#[derive(Debug, thiserror::Error)]
pub enum HxPackError {
    #[error("模板壳未找到: {0}")]
    ShellNotFound(String),
    #[error("应用缺少 index.html: {0}")]
    NoIndexHtml(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct HxShellPackager {
    shell_path: PathBuf,
    loader_dll_path: PathBuf,
}

impl HxShellPackager {
    pub fn new(shell_path: PathBuf, loader_dll_path: PathBuf) -> Self {
        Self { shell_path, loader_dll_path }
    }

    /// 打包 app 目录 → dist/<id>.exe（单文件）+ WebView2Loader.dll
    /// `mode` 参数保留接口兼容（Sidecar/NeuBuild 结果相同）。
    pub fn package(&self, app_dir: &Path, id: &str, _mode: PackageMode) -> Result<PathBuf, HxPackError> {
        let shell = std::fs::read(&self.shell_path).map_err(|_| {
            HxPackError::ShellNotFound(self.shell_path.display().to_string())
        })?;

        let html_path = app_dir.join("index.html");
        let html = std::fs::read(&html_path).map_err(|_| {
            HxPackError::NoIndexHtml(html_path.display().to_string())
        })?;

        let dist_dir = app_dir.join("dist");
        std::fs::create_dir_all(&dist_dir)?;

        let exe_path = dist_dir.join(format!("{id}.exe"));
        let mut out = shell;
        out.extend_from_slice(&html);
        out.extend_from_slice(&(html.len() as u64).to_le_bytes());
        out.extend_from_slice(MAGIC);
        std::fs::write(&exe_path, out)?;

        if self.loader_dll_path.exists() {
            let dst = dist_dir.join("WebView2Loader.dll");
            let _ = std::fs::copy(&self.loader_dll_path, dst);
        }

        Ok(exe_path)
    }
}