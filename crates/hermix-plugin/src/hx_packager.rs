//! hx-shell packager — 尾部注入模板壳生成单文件 exe
//!
//! 用户机器零编译：打包 = 纯文件拼接（HTML + len:8LE + MAGIC）。
//! 壳启动时读自身尾部 payload 加载页面，否则加载外部 resources/ 目录（开发模式）。
//!
//! 支持两种 app 形态：
//! 1. **纯 HTML**：`index.html` 直接作为 payload（内联 CSS+JS）
//! 2. **TSX/React**：`src/main.tsx` → esbuild 编译 → React UMD + app.js
//!    inline 进 index.html（Hermix 内置 esbuild.exe + React UMD，完全离线）
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
    #[error("esbuild 未找到: {0}（需要 Hermix 内置 resources/loader/esbuild.exe）")]
    EsbuildNotFound(String),
    #[error("esbuild 编译失败: {0}")]
    EsbuildFailed(String),
    #[error("React UMD 缺失: {0}")]
    ReactUmMissing(String),
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

    /// 若模板壳/加载器缺失，尝试从程序资源目录懒部署到目标位置。
    ///
    /// Hermix 是 portable 应用：新机器 / 更新后 `HERMIX_HOME/loader/`
    /// 可能没有 hx-shell.exe。此时从 `current_exe` 旁边的
    /// `resources/loader/`（Tauri bundle 内置）复制一份过去，保证打包可用。
    /// 复制失败不 panic —— 后续 `package()` 会给出明确的 ShellNotFound。
    fn ensure_shell_deployed(&self) {
        let missing: Vec<(&PathBuf, &str)> = vec![
            (&self.shell_path, "hx-shell.exe"),
            (&self.loader_dll_path, "WebView2Loader.dll"),
        ]
        .into_iter()
        .filter(|(p, _)| !p.exists())
        .collect();
        if missing.is_empty() {
            return;
        }

        // 找资源源目录：current_exe 旁 resources/loader/
        let Some(exe_dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.to_path_buf())) else {
            return;
        };
        for (target, file_name) in &missing {
            // 直接位于 exe 旁
            let mut src = exe_dir.join("resources").join("loader").join(file_name);
            if !src.exists() {
                // 或 exe 旁平级（无 resources/ 嵌套）
                src = exe_dir.join(file_name);
            }
            if src.exists() {
                if let Some(parent) = target.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::copy(&src, target);
            }
        }
    }

    /// 解析 Hermix 内置工具路径（esbuild.exe / React UMD），支持
    /// HERMIX_HOME/loader/ 与 current_exe 旁 resources/loader/ 两种布局。
    fn resolve_loader_file(rel: &str) -> PathBuf {
        let home = hermix_config::paths::hermix_home();
        let cands = [
            Some(home.join("loader").join(rel)),
            std::env::current_exe().ok().and_then(|e| {
                e.parent().map(|d| d.join("resources").join("loader").join(rel))
            }),
            std::env::current_exe().ok().and_then(|e| {
                e.parent().map(|d| d.join(rel))
            }),
        ]
        .into_iter()
        .flatten();
        for c in cands {
            if c.exists() {
                return c;
            }
        }
        home.join("loader").join(rel)
    }

    /// 用 esbuild 编译 `src/main.tsx` → 返回 inline 的 `<script>...</script>` 块。
    ///
    /// React/ReactDOM 是全局变量（由 UMD inline 注入），esbuild 只负责
    /// TSX→JS 转换 + bundle，`--external:react --external:react-dom`
    /// 跳过它们；`--jsx=transform` 生成 `React.createElement` 调用
    /// （匹配 UMD 全局）。产物是 IIFE，直接内嵌 HTML。
    fn compile_tsx(&self, app_dir: &Path) -> Result<String, HxPackError> {
        let entry = app_dir.join("src").join("main.tsx");
        if !entry.exists() {
            return Ok(String::new()); // 无 TSX 源码，纯 HTML 模式
        }

        let esbuild = Self::resolve_loader_file("esbuild.exe");
        if !esbuild.exists() {
            return Err(HxPackError::EsbuildNotFound(esbuild.display().to_string()));
        }

        // 临时输出文件（同目录，避免跨盘）
        let out_js = app_dir.join("dist").join("_app.bundle.js");
        std::fs::create_dir_all(out_js.parent().unwrap())?;
        let _ = std::fs::remove_file(&out_js);

        let out = std::process::Command::new(&esbuild)
            .arg("--bundle")
            .arg("--format=iife")
            .arg("--external:react")
            .arg("--external:react-dom")
            .arg("--jsx=transform")
            .arg("--log-level=error")
            .arg(format!("--outfile={}", out_js.display()))
            .arg(entry)
            .output()
            .map_err(|e| HxPackError::EsbuildFailed(e.to_string()))?;

        if !out.status.success() {
            let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(HxPackError::EsbuildFailed(
                if msg.is_empty() { "unknown esbuild error".into() } else { msg },
            ));
        }

        let js = std::fs::read_to_string(&out_js)?;
        let _ = std::fs::remove_file(&out_js);
        Ok(format!("<script>\n{js}\n</script>"))
    }

    /// 构造完整 HTML：纯 HTML 模式直接用 index.html；
    /// TSX 模式把 React UMD + ReactDOM UMD + esbuild 产物 inline 进模板。
    fn build_final_html(&self, app_dir: &Path) -> Result<Vec<u8>, HxPackError> {
        let html_path = app_dir.join("index.html");
        let raw = std::fs::read(&html_path).map_err(|_| {
            HxPackError::NoIndexHtml(html_path.display().to_string())
        })?;

        // 是否有 TSX 源码？
        let has_tsx = app_dir.join("src").join("main.tsx").exists();
        if !has_tsx {
            return Ok(raw);
        }

        // TSX 模式：React UMD + 编译产物 inline
        let react_umd = std::fs::read_to_string(Self::resolve_loader_file("react/react.production.min.js"))
            .map_err(|_| HxPackError::ReactUmMissing("react.production.min.js".into()))?;
        let react_dom_umd = std::fs::read_to_string(Self::resolve_loader_file("react/react-dom.production.min.js"))
            .map_err(|_| HxPackError::ReactUmMissing("react-dom.production.min.js".into()))?;
        let compiled = self.compile_tsx(app_dir)?;

        // 模板：<div id="root"> + React UMD + ReactDOM UMD + app.js
        // 若 index.html 已有 <div id="root">，保留标题/样式；否则用标准模板
        let title = "Hermix App";
        let html = String::from_utf8_lossy(&raw);
        let has_root = html.contains("id=\"root\"") || html.contains("id='root'");
        let body = if has_root {
            // 用原始 html 作为壳，把 </body> 前注入脚本
            let mut s = html.to_string();
            let scripts = format!(
                "<script>\n{react_umd}\n</script>\n<script>\n{react_dom_umd}\n</script>\n{compiled}"
            );
            if let Some(pos) = s.rfind("</body>") {
                s.insert_str(pos, &scripts);
            } else {
                s.push_str(&scripts);
            }
            s
        } else {
            format!(
                "<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head><meta charset=\"utf-8\" /><title>{title}</title></head>\n<body>\n<div id=\"root\"></div>\n<script>\n{react_umd}\n</script>\n<script>\n{react_dom_umd}\n</script>\n{compiled}\n</body>\n</html>"
            )
        };

        Ok(body.into_bytes())
    }

    /// 打包 app 目录 → dist/<id>.exe（单文件）+ WebView2Loader.dll
    /// `mode` 参数保留接口兼容（Sidecar/NeuBuild 结果相同）。
    pub fn package(&self, app_dir: &Path, id: &str, _mode: PackageMode) -> Result<PathBuf, HxPackError> {
        self.ensure_shell_deployed();
        let shell = std::fs::read(&self.shell_path).map_err(|_| {
            HxPackError::ShellNotFound(self.shell_path.display().to_string())
        })?;

        let html = self.build_final_html(app_dir)?;

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
