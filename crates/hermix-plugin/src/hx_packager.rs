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

    /// 若模板壳/加载器缺失**或版本过旧**，从程序资源目录懒部署到目标位置。
    ///
    /// Hermix 是 portable 应用：新机器 / 更新后 `HERMIX_HOME/loader/`
    /// 可能没有 hx-shell.exe，或存在旧版（无 window.hx init script /
    /// 无新增 API）。此时从 `current_exe` 旁边的
    /// `resources/loader/`（Tauri bundle 内置）复制一份过去，保证打包可用。
    ///
    /// 2026-08-04 增强：**版本死锁修复**。旧逻辑只在文件缺失时复制，
    /// 导致 `HERMIX_HOME/loader/` 里一旦有旧版壳，新壳永远不被部署
    /// （主人报 "window.hx 没注入" 的直接根因）。现在通过比较
    /// `BUILD_PROBE_*` 标记：若目标壳比资源壳旧（或缺失标记），覆盖复制。
    /// 复制失败不 panic —— 后续 `package()` 会给出明确的 ShellNotFound。
    fn ensure_shell_deployed(&self) {
        // 源目录：current_exe 旁 resources/loader/
        let Some(exe_dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.to_path_buf())) else {
            return;
        };
        let src_candidates = |file_name: &str| -> Option<std::path::PathBuf> {
            let direct = exe_dir.join("resources").join("loader").join(file_name);
            if direct.exists() { return Some(direct); }
            let flat = exe_dir.join(file_name);
            if flat.exists() { return Some(flat); }
            None
        };

        // 逐个检查：缺失 → 复制；存在但版本旧 → 覆盖
        let items: [(&PathBuf, &str); 2] = [
            (&self.shell_path, "hx-shell.exe"),
            (&self.loader_dll_path, "WebView2Loader.dll"),
        ];
        for (target, file_name) in items {
            if let Some(src) = src_candidates(file_name) {
                let needs_copy = !target.exists() || shell_is_stale(target, &src);
                if needs_copy {
                    if let Some(parent) = target.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    // Windows 上直接覆盖运行中/只读文件会失败；先尝试删旧再复制
                    let _ = std::fs::remove_file(target);
                    let _ = std::fs::copy(&src, target);
                }
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

    /// 构造完整 payload bytes + 可选 icon png bytes：
    /// - 纯 HTML 模式：`[icon_png?][icon_len:8LE][html][html_len:8LE][MAGIC]`
    /// - TSX 模式：React UMD + ReactDOM UMD + esbuild 产物 inline 进 HTML 壳后再打包。
    /// icon 段可选：有就前置，没就省略（保持向后兼容旧 exe）。
    /// `icon_png_out` 仅在 manifest 有 icon_data 时为 Some(bytes)；None 时不打 icon 段。
    fn build_final_payload(
        &self,
        app_dir: &Path,
    ) -> Result<(Vec<u8>, Option<Vec<u8>>), HxPackError> {
        let html_path = app_dir.join("index.html");
        let raw_html = std::fs::read(&html_path).map_err(|_| {
            HxPackError::NoIndexHtml(html_path.display().to_string())
        })?;

        // 读取可选图标（优先级：icon_data base64 > icon 库内 SVG 名 > icon_file）
        // - `icon_data`: base64 PNG data URL（旧方式）
        // - `icon`: SVG 库内名字符串，如 "music" → 读内置图标库 resources/icons/music.svg
        // - `icon_file`: 应用目录里的图标文件名（如 icon.png）
        let icon_png = read_icon_data(app_dir).or_else(|| read_icon_library_svg(app_dir));

        // 是否有 TSX 源码？
        let has_tsx = app_dir.join("src").join("main.tsx").exists();
        let html_vec = if !has_tsx {
            raw_html
        } else {
            // TSX 模式：React UMD + 编译产物 inline 进模板
            let react_umd = std::fs::read_to_string(Self::resolve_loader_file("react/react.production.min.js"))
                .map_err(|_| HxPackError::ReactUmMissing("react.production.min.js".into()))?;
            let react_dom_umd = std::fs::read_to_string(Self::resolve_loader_file("react/react-dom.production.min.js"))
                .map_err(|_| HxPackError::ReactUmMissing("react.dom.production.min.js".into()))?;
            let compiled = self.compile_tsx(app_dir)?;

            let title = "Hermix App";
            let html = String::from_utf8_lossy(&raw_html);
            let has_root = html.contains("id=\"root\"") || html.contains("id='root'");
            let body = if has_root {
                let mut s = html.to_string();
                s = s.replace(r#"<script src="/js/app.js"></script>"#, "");
                s = s.replace(r#"<script src='/js/app.js'></script>"#, "");
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
            body.into_bytes()
        };

        Ok((html_vec, icon_png))
    }

    /// 打包 app 目录 → dist/<id>.exe（单文件）+ WebView2Loader.dll
    /// `mode` 参数保留接口兼容（Sidecar/NeuBuild 结果相同）。
    pub fn package(&self, app_dir: &Path, id: &str, _mode: PackageMode) -> Result<PathBuf, HxPackError> {
        self.ensure_shell_deployed();
        let shell = std::fs::read(&self.shell_path).map_err(|_| {
            HxPackError::ShellNotFound(self.shell_path.display().to_string())
        })?;

        let (html, icon_png) = self.build_final_payload(app_dir)?;

        let dist_dir = app_dir.join("dist");
        std::fs::create_dir_all(&dist_dir)?;

        let exe_path = dist_dir.join(format!("{id}.exe"));
        let mut out = shell;
        // payload trailer: [icon_png?][icon_len:8LE][html][html_len:8LE][MAGIC]
        // 壳 read_self_icon 会自动检测前导 icon 段
        if let Some(icon) = icon_png.as_ref() {
            out.extend_from_slice(icon);
            out.extend_from_slice(&(icon.len() as u64).to_le_bytes());
        }
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

/// 比较目标壳与资源壳的 BUILD_PROBE 版本标记。
/// 目标缺少标记、或标记比资源旧 → true（需要覆盖）。
/// 两侧都无法识别标记 → 按文件大小判断（更大的通常更新）。
fn shell_is_stale(target: &Path, src: &Path) -> bool {
    fn probe(p: &Path) -> Option<u32> {
        let bytes = std::fs::read(p).ok()?;
        let hay = String::from_utf8_lossy(&bytes);
        // 提取 BUILD_PROBE_YYYY_DC_V<N> 里的版本号
        let find = |marker: &str| {
            let idx = hay.find(marker)?;
            let rest = &hay[idx + marker.len()..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse::<u32>().ok()
        };
        find("BUILD_PROBE_2026_DC_V").or_else(|| find("BUILD_PROBE"))
    }
    match (probe(target), probe(src)) {
        (Some(t), Some(s)) => t < s,
        (None, Some(_)) => true,          // 目标无标记（旧版）→ 覆盖
        (Some(_), None) => false,         // 资源无标记 → 不动
        (None, None) => {
            // 双无标记：按大小比（旧壳更小）
            std::fs::metadata(target)
                .and_then(|t| std::fs::metadata(src).map(|s| (t.len(), s.len())))
                .map(|(t, s)| t < s)
                .unwrap_or(false)
        }
    }
}

/// 从 manifest.json 读 icon_data（base64 data URL 或纯 base64），解码为 PNG bytes。
/// 返回 None 表示没有图标或解码失败（壳会降级为无图标窗口）。
fn read_icon_data(app_dir: &Path) -> Option<Vec<u8>> {
    let manifest_path = app_dir.join("manifest.json");
    let text = std::fs::read_to_string(&manifest_path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let raw = v.get("icon_data").and_then(|x| x.as_str())?;
    crate::base64_decode_icon(raw).ok()
}

/// 从 manifest.json 读 `icon` 字段（图标库内 SVG 名，如 "music"），
/// 从内置图标库 `resources/icons/<name>.svg` 读取 SVG 内容。
/// 返回 None 表示没有 icon 字段或库中无此图标（壳降级为无图标窗口）。
fn read_icon_library_svg(app_dir: &Path) -> Option<Vec<u8>> {
    let manifest_path = app_dir.join("manifest.json");
    let text = std::fs::read_to_string(&manifest_path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let name = v.get("icon").and_then(|x| x.as_str())?;
    let name = name.trim();
    if name.is_empty() || name.contains("base64") || name.len() > 64 {
        return None; // 非法名（可能是 base64 data URL 混进 icon 字段）
    }
    // 图标库候选路径：当前 exe 旁 resources/icons/ → HERMIX_HOME/loader/icons/
    let cands = [
        std::env::current_exe().ok().and_then(|e| {
            e.parent().map(|d| d.join("resources").join("icons").join(format!("{name}.svg")))
        }),
        Some(
            hermix_config::paths::hermix_home()
                .join("loader").join("icons").join(format!("{name}.svg")),
        ),
    ]
    .into_iter()
    .flatten();
    for c in cands {
        if c.exists() {
            return std::fs::read(&c).ok();
        }
    }
    None
}
