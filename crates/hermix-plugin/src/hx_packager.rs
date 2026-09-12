//! hx-shell packager — 模板壳 + PE 资源节嵌多文件归档，产单 exe
//!
//! 用户机器零编译、零 npm、零 esbuild：打包 = walk `app_dir` → 序列化裸清单
//! → 写 PE `RT_RCDATA 1002`（多文件归档）。壳启动读资源节 → `app://` 虚拟协议
//! 直喂 WebView2。
//!
//! 仅支持一种 app 形态：**始终 vanilla HTML/CSS/JS（TSX 退役）**。detect 到
//! `src/main.tsx` 即直接报错，引导 LLM 改 vanilla（spec §3.7 SKILL 护栏 + §1）。
//! 复杂 UI 用壳预置原语（`Hermix.store` / `Hermix.defineComponent` / `HermixUI`）。
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
    #[error("TSX/React 已退役 (spec §1): 检测到 {0}，请改 vanilla HTML/CSS/JS + 壳预置原语 (Hermix.store / defineComponent / HermixUI)。禁止 npm import。")]
    TsxDeprecated(String),
    #[error("启动失败: {0}")]
    LaunchFailed(String),
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
    /// 通过比较 `BUILD_PROBE_*` 标记部署壳：若目标壳缺失或比资源壳旧，
    /// 覆盖复制 `resources/loader/` 下的壳，避免旧版壳残留导致功能缺失。
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
    pub(crate) fn resolve_loader_file(rel: &str) -> PathBuf {
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

    /// 检测 `src/main.tsx` 是否存在 —— 存在则直接拒绝打包（TSX 已退役）。
    ///
    /// 历史背景：原版本通过 esbuild.exe + React UMD inline 把 TSX 编进单 HTML。
    /// spec §1 重定义为「始终 vanilla、TSX 退役」：错误路径变 fail-fast，
    /// packager 不再持有 esbuild 依赖，壳也不再依赖 React UMD bundle。
    /// LLM 想用组件 / 状态 → 走壳预置的 `Hermix.*` / Preact 全局。
    fn detect_tsx(&self, app_dir: &Path) -> Result<(), HxPackError> {
        let tsx = app_dir.join("src").join("main.tsx");
        if tsx.exists() {
            return Err(HxPackError::TsxDeprecated(tsx.display().to_string()));
        }
        // JSX/TS 痕迹也拒：src/*.jsx / src/*.ts 都按 TSX 处理（除纯 vanilla .js）
        if let Ok(read) = std::fs::read_dir(app_dir.join("src")) {
            for entry in read.flatten() {
                if let Some(ext) = entry.path().extension().and_then(|e| e.to_str()) {
                    if matches!(ext, "jsx" | "ts" | "tsx" | "mjs") {
                        return Err(HxPackError::TsxDeprecated(
                            entry.path().display().to_string(),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// 构造完整 payload bytes + 可选 icon png bytes：
    /// - 始终 vanilla HTML/CSS/JS（spec §1，TSX 已退役，detect_tsx 提前 fail-fast）
    /// - 多文件归档（Phase 0, spec §1）：
    ///   - app 目录里有除 `index.html` 外的静态资源（vendor/*.js、*.css、images...）→ 归档非空 → Some(bytes)
    ///   - 否则 None → 壳走旧单 HTML 路径（完全向后兼容）
    fn build_final_payload(
        &self,
        app_dir: &Path,
    ) -> Result<(Vec<u8>, Option<Vec<u8>>, Option<Vec<u8>>), HxPackError> {
        // TSX/JSX/TS 提前拦截（spec §1）
        self.detect_tsx(app_dir)?;

        let html_path = app_dir.join("index.html");
        let raw_html = std::fs::read(&html_path).map_err(|_| {
            HxPackError::NoIndexHtml(html_path.display().to_string())
        })?;

        // 读取可选图标，优先级链: icon_file (PNG) > icon_data (base64 PNG, 旧兼容) > icon (内置库 SVG 名)，
        // 返回 bytes + is_svg 标记 (hx-shell read_self_icon 同时支持 SVG/PNG).
        let resolved_icon = resolve_app_icon(app_dir);

        // 始终 vanilla：直接读 index.html 原样进 PE 资源 1000
        let html_vec = raw_html;

        // icon_bytes 是 (bytes, is_svg) — 塞尾部 payload 时 SVG/PNG 都行
        // (hx-shell read_self_icon 双格式解析), 直接取 bytes.
        //
        // tile_color 优先: 若用户配置了 tile_color 且图标是字形类（非 emoji、
        // 非自备 PNG），则生成「纯色圆角块 + 白色字形」PNG 作为标题栏图标，
        // 与列表/详情卡片、任务栏三处视觉一致。否则沿用既有 resolver 结果。
        let tiled = resolve_tile(app_dir)
            .and_then(|(tc, gk, cat)| {
                crate::icon_library::generate_tiled_icon_png(&tc, &gk, &cat, 256).ok()
            });
        let icon_bytes = tiled.or_else(|| resolved_icon.map(|(b, _is_svg)| b));

        // ── 多文件归档（Phase 0）：app 目录里所有静态资源 → 序列化 blob ──
        // 注意：icon_bytes 不进归档（它经 PE 资源 RT_RCDATA 1001 / 任务栏 ICO 单独走）
        let archive = match crate::app_archive::build(app_dir) {
            Ok(a) if !a.entries.is_empty() => {
                // 仅当除 index.html 外还有其它资源 → 才有意义走 app:// 多文件路径
                let non_html_count = a.entries.iter().filter(|e| e.relpath != "index.html").count();
                if non_html_count > 0 {
                    Some(a.blob)
                } else {
                    None
                }
            }
            Ok(_) => None,
            Err(e) => {
                tracing::warn!("app_archive build failed (回退单 HTML): {e}");
                None
            }
        };

        Ok((html_vec, icon_bytes, archive))
    }

    /// 打包 app 目录 → dist/<id>.exe（单文件分发）。
    /// WebView2Loader 在 MSVC 下由 webview2-com-sys 静态链接进 shell exe，
    /// 运行时无需外部 dll，故 dist 只产出这一个 exe。`mode` 参数保留接口兼容。
    pub fn package(&self, app_dir: &Path, id: &str, _mode: PackageMode) -> Result<PathBuf, HxPackError> {
        self.ensure_shell_deployed();
        let shell = std::fs::read(&self.shell_path).map_err(|_| {
            HxPackError::ShellNotFound(self.shell_path.display().to_string())
        })?;

        let (html, icon_png, archive) = self.build_final_payload(app_dir)?;
        eprintln!(
            "[hx_packager] build_final_payload result: html={}B, icon={}B, archive={:?}",
            html.len(),
            icon_png.as_ref().map(|v| v.len()).unwrap_or(0),
            archive.as_ref().map(|v| v.len())
        );

        let dist_dir = app_dir.join("dist");
        std::fs::create_dir_all(&dist_dir)?;

        let exe_path = dist_dir.join(format!("{id}.exe"));
        // 先写出原始 loader 壳，再把 HTML + 图标 PNG 作为 PE 资源 (RT_RCDATA)
        // 嵌入。取代「exe 末尾拼接 payload」：尾部附加大块数据正是 Defender
        // Behavior:Win32/DefenseEvasion.A!ml 的结构型触发点（被识别为
        // packer/cryptor 重叠数据）。改为正规资源节后结构合法。
        std::fs::write(&exe_path, &shell)?;
        if let Err(e) = crate::icon_library::embed_app_payload_both(
            &exe_path,
            &html,
            icon_png.as_deref(),
            archive.as_deref(),
        ) {
            tracing::warn!("嵌入 PE 资源失败, 回退 EOF 尾部注入 (可能触发 Defender 误报): {e}");
            // 回退：保留旧尾部拼接，保证可用（但可能误报）
            let mut out = std::fs::read(&exe_path)?;
            if let Some(icon) = icon_png.as_ref() {
                out.extend_from_slice(icon);
                out.extend_from_slice(&(icon.len() as u64).to_le_bytes());
            }
            out.extend_from_slice(&html);
            out.extend_from_slice(&(html.len() as u64).to_le_bytes());
            out.extend_from_slice(MAGIC);
            std::fs::write(&exe_path, out)?;
        }

        // 单文件分发：dist 只产出 <id>.exe 一个文件。
        // WebView2Loader 在 MSVC 构建下由 webview2-com-sys 静态链接进
        // shell exe（link kind = "static"），运行时不需要兄弟 dll；
        // 不复制 loader_dll_path，避免 dist 出现第二个文件。

        // ── 第二层: 把同一个图标嵌入 dist exe 的 Windows 资源表 ──
        //
        // Windows 任务栏图标取自 exe 的 RT_GROUP_ICON 资源节，不读 exe 尾部的
        // payload；hx-shell 编译时嵌入的默认图标永久占位，需写入 PE 资源表替换。
        //
        // 解法: 复用 `icon_library::patch_exe_icon*` (已写好但从未被 hx_packager
        //       调用!) 把 `manifest` 的 icon (统一 resolver) 缩放多尺寸、
        //       构造多帧 ICO、通过 BeginUpdateResourceW 写进 dist exe 的资源表。
        //
        // 统一性 (icon 三处同源):
        //   - icon_file (PNG) → 直接转 ICO
        //   - icon (内置库 SVG 名) → 先 SVG→PNG (resvg, 无字体依赖) → ICO
        //   - 都没有 → category 自动生成 (确定性)
        //   与窗口标题栏 (尾部 payload) 用的是同一个 resolver 结果。
        //
        // 失败安全: 仅是"任务栏图标不对", 不影响 exe 运行或尾部 payload —
        //          patch 失败只告警,不重写 dist exe。
        #[cfg(windows)]
        {
            use crate::icon_library;

            // tile_color 优先: 用户显式配置了 tile_color 且图标是字形类 → 生成
            // 「纯色圆角块 + 白色字形」多尺寸 ICO 打入 exe 资源表,
            // 与前端卡片、窗口标题栏三处视觉一致。
            if let Some((tc, gk, cat)) = resolve_tile(app_dir) {
                match icon_library::generate_tiled_icon_resources(&tc, &gk, &cat) {
                    Ok((group, icons, _ico)) => {
                        if let Err(e) = icon_library::patch_exe_icon(&exe_path, &group, &icons) {
                            tracing::warn!(
                                "tile_color 图标嵌入失败 (任务栏用 hx-shell 默认, 不影响运行): {}",
                                e
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!("tile_color 图标生成失败 (回退默认): {e}");
                    }
                }
            } else {
                // 统一图标源 (同 build_final_payload): PNG 或 SVG
                let icon_bytes = resolve_app_icon(app_dir);
                if let Some((bytes, is_svg)) = icon_bytes {
                    // SVG → PNG (任务栏需要位图): 用 256 主尺寸 (ICO 内部再缩放)
                    let png = if is_svg {
                        icon_library::render_svg_bytes_to_sized_png(&bytes, 256)
                            .map_err(|e| {
                                tracing::warn!("SVG→PNG 失败 (任务栏用 hx-shell 默认): {e}");
                            })
                            .ok()
                    } else {
                        Some(bytes)
                    };
                    if let Some(png) = png {
                        if let Err(e) = icon_library::patch_exe_icon_from_png(&exe_path, &png) {
                            tracing::warn!(
                                "为 {} 打 exe 图标资源补丁失败 (任务栏图标用 hx-shell 默认, 不影响运行): {}",
                                exe_path.display(),
                                e
                            );
                        }
                    }
                } else {
                    // 兜底: 没有任何用户图标时, 用内置 category 图标库自动生成, 至少比 hx-shell
                    //       默认♬ 更"应用专属"(每个应用根据 id 哈希得到确定性颜色 + 分类字形)
                    let manifest_name = id.to_string();
                    let category = read_icon_category(app_dir).unwrap_or_else(|| "other".to_string());
                    if let Ok((group, icons, _ico)) =
                        icon_library::generate_icon_resources(&manifest_name, id, &category)
                    {
                        if let Err(e) = icon_library::patch_exe_icon(&exe_path, &group, &icons) {
                            tracing::warn!("自动生成图标嵌入失败 (不影响运行): {e}");
                        }
                    }
                }
            }
        }

        // 可选代码签名：HERMIX_CODESIGN_* 配置驱动；未配置则跳过 + 告警。
        // 签名是消除 Defender 信誉型误报 (Behavior:Win32/DefenseEvasion.A!ml)
        // 的决定性因素——结构合法(PE 资源)只解决一半，信誉(签名)解决另一半。
        sign_exe_if_configured(&exe_path);

        Ok(exe_path)
    }
}

impl HxShellPackager {
    // 测试版 / dev 运行：构建单文件 html，写到 dist/<id>.dev.html，
    // 用 `hx-shell.exe --html <path>` 直接加载。不落单 exe、不在 exe 尾
    // 部拼数据，因此不会触发 Defender 对「单文件打包」的结构型误报。
    pub fn run_dev(&self, app_dir: &Path, id: &str) -> Result<(), HxPackError> {
        self.ensure_shell_deployed();
        // Phase 6 dev 模式（spec §8.7）：dev 走虚拟协议 host 磁盘 app_dir，
        // 与 prod 共用 app:// handler（hx-shell 内部 APP_DIR_FOR_DEV）。
        // 不再生成 .dev.html、不再走单 HTML 包装，与打包后行为一致。
        let child = std::process::Command::new(&self.shell_path)
            .arg("--app-dir")
            .arg(app_dir)
            .arg("--dev")
            .arg("--app-id")
            .arg(id)
            .current_dir(app_dir)
            .spawn()
            .map_err(|e| HxPackError::LaunchFailed(e.to_string()))?;
        drop(child);
        Ok(())
    }
}

/// 可选代码签名：若配置了 `HERMIX_CODESIGN_PFX`，调用 signtool 对打包产物签名。
///
/// 环境变量：
/// - `HERMIX_CODESIGN_PFX`：证书路径（.pfx/.p12）。空/未设 → 跳过（仅告警）。
/// - `HERMIX_CODESIGN_PASSWORD`：证书密码（默认空）。
/// - `HERMIX_CODESIGN_TIMESTAMP_URL`：RFC3161 时间戳服务（默认 DigiCert）。
/// - `HERMIX_SIGNTOOL`：signtool 路径（默认从 PATH 找）。
///
/// 失败仅告警不中断打包（exe 仍可用，只是未签名）。未配置证书时静默跳过。
fn sign_exe_if_configured(exe_path: &Path) {
    let pfx = match std::env::var("HERMIX_CODESIGN_PFX") {
        Ok(v) if !v.is_empty() => v,
        _ => {
            tracing::info!(
                "未配置 HERMIX_CODESIGN_PFX，跳过代码签名（要消除 Defender 误报请自备 OV/EV 代码签名证书并配置该变量）"
            );
            return;
        }
    };
    let password = std::env::var("HERMIX_CODESIGN_PASSWORD").unwrap_or_default();
    let timestamp = std::env::var("HERMIX_CODESIGN_TIMESTAMP_URL")
        .unwrap_or_else(|_| "http://timestamp.digicert.com".to_string());
    let signtool = std::env::var("HERMIX_SIGNTOOL").unwrap_or_else(|_| "signtool".to_string());

    // signtool 连外部 RFC3161 时间戳服务器(http://timestamp.digicert.com),
    // 服务器不可达时会**永久挂起**且 signtool 自身无内部超时。
    // 若在当前线程同步 .status() 阻塞, 会冻结调用方所在的 async worker,
    // 导致外层 60s 超时无法抢占、整个 agent loop 卡死。
    // 因此必须在独立线程跑 + 30s 超时, 超时则跳过签名、绝不阻塞主流程。
    let exe = exe_path.to_path_buf();
    let exe_for_thread = exe.clone();
    let (tx, rx) = std::sync::mpsc::channel::<Result<std::process::ExitStatus, String>>();
    let _sign_thread = std::thread::spawn(move || {
        let r = std::process::Command::new(&signtool)
            .args([
                "sign",
                "/fd",
                "SHA256",
                "/tr",
                &timestamp,
                "/td",
                "SHA256",
                "/f",
                &pfx,
                "/p",
                &password,
                &exe_for_thread.to_string_lossy(),
            ])
            .status();
        let _ = tx.send(r.map_err(|e| e.to_string()));
    });

    match rx.recv_timeout(std::time::Duration::from_secs(30)) {
        Ok(Ok(s)) if s.success() => {
            tracing::info!("代码签名成功: {}", exe.display());
        }
        Ok(Ok(s)) => {
            tracing::warn!(
                "signtool 退出码 {s}（签名失败，exe 未签名）: {}",
                exe.display()
            );
        }
        Ok(Err(e)) => {
            tracing::warn!(
                "无法启动 signtool（请确保 signtool 在 PATH 或设 HERMIX_SIGNTOOL）: {e}；exe 未签名"
            );
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            tracing::warn!(
                "signtool 30s 超时（时间戳服务器可能不可达），跳过签名以避免阻塞主流程: {}",
                exe.display()
            );
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            tracing::warn!(
                "signtool 信道断开（签名线程异常退出），跳过签名: {}",
                exe.display()
            );
        }
    }
}

/// 统一图标解析：卡片 / 窗口 / 任务栏三处使用同一图标来源。
///
/// 优先级链 (LLM 开发规范, 见 SKILL.md/README):
///   1. `icon_file` — 应用目录内 PNG 文件名 (完全自定义 logo)
///   2. `icon_data` — base64 PNG data URL (旧方式, 兼容保留)
///   3. `icon` — 内置 lucide 图标库名字符串, 如 "music" → resources/icons/music.svg
///   4. 单 emoji / 无 → None (调用方走 category 自动生成, 见 icon_library::generate_icon_resources)
///
/// 返回 (bytes, is_svg): bytes 是 PNG 或 SVG; is_svg 供任务栏 ICO 转换判断.
/// 窗口标题栏 (hx-shell read_self_icon) 与任务栏 (patch_exe_icon) 都从这里取.
fn resolve_app_icon(app_dir: &Path) -> Option<(Vec<u8>, bool)> {
    // 1. icon_file (PNG)
    if let Some(png) = read_icon_file_png(app_dir) {
        return Some((png, false));
    }
    // 2. icon_data (base64 PNG, 旧兼容)
    if let Some(png) = read_icon_data(app_dir) {
        return Some((png, false));
    }
    // 3. icon (内置库 SVG 名)
    if let Some(svg) = read_icon_library_svg(app_dir) {
        return Some((svg, true));
    }
    None
}

/// 读 `manifest.json` 的 `tile_color` 字段（如 "#1ABC9C"）。
/// 未设 / 空 / 不合法（未过 `validate_tile_color`）→ None。
fn read_tile_color(app_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(app_dir.join("manifest.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let raw = v.get("tile_color").and_then(|x| x.as_str())?.trim().to_string();
    if raw.is_empty() {
        return None;
    }
    crate::manifest::validate_tile_color(&raw).ok()?;
    Some(raw)
}

/// 读 `manifest.json` 的 `icon` 字段原始值（图标库名 / emoji / 空）。
fn read_icon_field(app_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(app_dir.join("manifest.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("icon").and_then(|x| x.as_str()).map(|s| s.to_string())
}

/// 与前端 `iconResolver.isEmoji` 同规则：单字符 emoji / 符号 → true。
/// 用于决定 tile 背景是否跳过（emoji 自身有色彩，叠方块变花，与前端一致）。
fn is_emoji(s: &str) -> bool {
    if s.is_empty() || s.chars().count() > 4 {
        return false;
    }
    for ch in s.chars() {
        let cp = ch as u32;
        if cp > 0x2bff {
            return true;
        }
        if (0x2600..=0x27bf).contains(&cp) {
            return true;
        }
    }
    false
}

/// 解析 tile_color 是否应应用到打包图标，与前端 `resolveAppIconEl` 行为对齐：
/// - tile_color 未设 / 非法 → None
/// - `icon` 是 emoji → None（前端裸 emoji 不加背景）
/// - `icon_file` / `icon_data` 存在（用户自备 PNG logo）→ None（不叠加背景）
/// - 其它（icon 是内置库名 / 无 icon 走 category）→ Some((tile_color, glyph_key, category))
///
/// glyph_key 直接取 `icon` 字段原值（大小写在 icon_library 内转小写比对 GLYPHS）；
/// 若不是 GLYPHS key，icon_library 会按 category 回退到对应字形，最终仍有合法图标。
fn resolve_tile(app_dir: &Path) -> Option<(String, String, String)> {
    let tile = read_tile_color(app_dir)?;
    if let Some(icon) = read_icon_field(app_dir) {
        if is_emoji(&icon) {
            return None;
        }
    }
    // 用户自备 PNG → 不叠背景（与前端 icon_file 跳过 bg 一致）
    if read_icon_file_png(app_dir).is_some() || read_icon_data(app_dir).is_some() {
        return None;
    }
    let glyph_key = read_icon_field(app_dir).unwrap_or_default();
    let category = read_icon_category(app_dir).unwrap_or_default();
    Some((tile, glyph_key, category))
}

/// 读 `manifest.json` 的 `icon_file` 字段（如 "icon.png"），应用目录下查找 → 读 PNG bytes。
/// 不存在返回 None (调用方回退到尾部 payload 的 icon 段)。
fn read_icon_file_png(app_dir: &Path) -> Option<Vec<u8>> {
    let manifest_path = app_dir.join("manifest.json");
    let text = std::fs::read_to_string(&manifest_path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let file_name = v.get("icon_file").and_then(|x| x.as_str())?;
    let file_name = file_name.trim();
    if file_name.is_empty() {
        return None;
    }
    std::fs::read(app_dir.join(file_name)).ok()
}

/// 读 `manifest.json` 的 `category` 字段（兜底自动图标生成时用）。
#[cfg(windows)]
fn read_icon_category(app_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(app_dir.join("manifest.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("category")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
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
