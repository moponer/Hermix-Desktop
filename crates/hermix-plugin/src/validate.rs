//! 自定义应用「开发期自验证」(shift-left validation).
//!
//! 目标: 让 LLM 在 *开发* 自定义应用时就能拦截打包/运行期错误。
//! hx-shell 架构下应用为静态 `index.html` + `window.hx.*` 绑定（WebView2 原生桥）。
//! 校验聚焦:
//!
//! - **L0 静态契约**: manifest 完整性 + permissions 声明
//! - **L1 HTML/JS 预检**: 结构完整性、hx API 调用模式、括号匹配
//! - **L2 权限一致性**: HTML 中的 hx API 调用是否在 permissions 中声明
//! - **L3 前端质量关卡**: 离线可用 (无 CDN 外链)、桌面定位 (禁 input file)、
//!   标题栏 (V7 壳必填)、无 emoji、禁 custom scheme fetch。把「前端默认标准」变成可强制硬指标。
//!
//! 调用方 (agent 开发循环) 应在生成/修改应用代码后调用
//! [`validate_custom_app`], 看到 `ok == false` 就继续修复, 直到通过
//! 才视为「开发完成」。
//!
//! 设计: 纯同步、零 panic、无外部依赖。

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// 单条校验问题。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ValidationIssue {
    /// `"error"` 阻断打包; `"warning"` 仅提示。
    pub level: String,
    /// 问题类别: `manifest` / `html` / `rpc` / `js`。
    pub code: String,
    /// 人类可读描述。
    pub message: String,
    /// 出错行 (1-based, 相对被校验文件)。JS/TS 语法错误时填充, 便于 agent 精准定位。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// 出错列 (1-based)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub col: Option<u32>,
    /// 出错点附近代码上下文 (esbuild 提供的片段), 同样为了 agent 免读全文即可 patch。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

/// 应用校验报告。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct ValidationReport {
    /// 是否通过 (无 `error` 级问题)。
    pub ok: bool,
    /// 所有问题 (error + warning)。
    pub issues: Vec<ValidationIssue>,
}

fn issue(level: &str, code: &str, message: impl Into<String>) -> ValidationIssue {
    ValidationIssue {
        level: level.to_string(),
        code: code.to_string(),
        message: message.into(),
        line: None,
        col: None,
        snippet: None,
    }
}

/// hx-shell 绑定 API → 所需权限的映射。
///
/// hx-shell 架构下，应用通过 `window.hx.*`（WebView2 原生桥 chrome.webview.postMessage）
/// 调用系统能力。当 HTML 中出现 `hx.writeFile(` 等调用时，需要对应权限。
/// 这里按 API 前缀匹配。
const HX_API_PERMISSIONS: &[(&str, &str)] = &[
    // 文件系统
    ("hx.readFile", "fs-read"),
    ("hx.readDir", "fs-read"),
    ("hx.stat", "fs-read"),
    ("hx.writeFile", "fs-write"),
    ("hx.appendFile", "fs-write"),
    ("hx.removeFile", "fs-write"),
    ("hx.createDir", "fs-write"),
    ("hx.removeDir", "fs-write"),
    // 进程/系统
    ("hx.exec", "exec"),
    ("hx.getInfo", "app"),
    // 对话框
    ("hx.openDialog", "dialog"),
    ("hx.saveDialog", "dialog"),
    ("hx.showMessage", "dialog"),
    // 剪贴板
    ("hx.clipboardGet", "clipboard"),
    ("hx.clipboardSet", "clipboard"),
];

/// 对单个自定义应用做开发期自验证 (L0 + L1 + L2)。
///
/// `app_dir` 为该应用的根目录 (含 `manifest.json` / `index.html`)。
/// 返回结构化报告, 调用方据此决定是否需要继续修复。
pub fn validate_custom_app(app_dir: &Path) -> ValidationReport {
    let mut issues: Vec<ValidationIssue> = Vec::new();

    // ── L0: manifest ──
    let manifest_path = app_dir.join("manifest.json");
    if !manifest_path.exists() {
        return ValidationReport {
            ok: false,
            issues: vec![issue("error", "manifest", "缺少 manifest.json")],
        };
    }
    let manifest_text = match std::fs::read_to_string(&manifest_path) {
        Ok(t) => t,
        Err(e) => {
            return ValidationReport {
                ok: false,
                issues: vec![issue("error", "manifest", format!("读取 manifest.json 失败: {e}"))],
            };
        }
    };
    let manifest: serde_json::Value = match serde_json::from_str(&manifest_text) {
        Ok(v) => v,
        Err(e) => {
            return ValidationReport {
                ok: false,
                issues: vec![issue(
                    "error",
                    "manifest",
                    format!("manifest.json 解析失败 (JSON 格式错误): {e}"),
                )],
            };
        }
    };
    for f in ["name", "id"] {
        if manifest.get(f).and_then(|v| v.as_str()).is_none() {
            issues.push(issue(
                "error",
                "manifest",
                format!("manifest 缺少必填字段 `{f}`"),
            ));
        }
    }
    if manifest.get("category").and_then(|v| v.as_str()).is_none() {
        issues.push(issue(
            "warning",
            "manifest",
            "manifest 建议声明 `category` 字段",
        ));
    }

    // 收集已声明的权限
    let declared_perms: Vec<String> = manifest
        .get("permissions")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    // 应用模型: 支持两种模式
    // 1. TypeScript/React: src/main.tsx (可选 + index.html 壳)
    // 2. Plain HTML: index.html (自包含)
    let index_html = app_dir.join("index.html");
    let src_main_tsx = app_dir.join("src").join("main.tsx");
    let has_tsx = src_main_tsx.exists();

    if !index_html.exists() && !has_tsx {
        return ValidationReport {
            ok: false,
            issues: vec![issue(
                "error",
                "manifest",
                "缺少 index.html 和 src/main.tsx (应用需要至少一个)",
            )],
        };
    }

    // ── L1: HTML 结构预检 (仅当 index.html 存在时) ──
    let html = if index_html.exists() {
        std::fs::read_to_string(&index_html).unwrap_or_default()
    } else {
        String::new()
    };

    // 如果是 TypeScript 模式，也读取 TSX 源码用于校验
    let tsx_code = if has_tsx {
        std::fs::read_to_string(&src_main_tsx).unwrap_or_default()
    } else {
        String::new()
    };

    // 合并 HTML + TSX 代码用于 API 检查
    let combined_code = format!("{html}\n{tsx_code}");

    // 检查基本 HTML 结构 (仅在纯 HTML 模式下严格检查)
    if !has_tsx && !html.is_empty() {
        let has_html_tag = html.contains("<html") || html.contains("<!DOCTYPE");
        let has_body = html.contains("<body") || html.contains("<body>");
        let has_script = html.contains("<script");

        if !has_html_tag && !html.trim().starts_with('<') {
            issues.push(issue(
                "error",
                "html",
                "index.html 不是有效的 HTML 文档: 缺少 <html> 或 <!DOCTYPE> 标签",
            ));
        }
        if !has_body {
            issues.push(issue(
                "warning",
                "html",
                "index.html 缺少 <body> 标签, 页面可能无法正确渲染",
            ));
        }
        if !has_script {
            issues.push(issue(
                "warning",
                "html",
                "index.html 缺少 <script> 标签, 应用可能没有交互逻辑",
            ));
        }

        // 检查 <script> 标签闭合
        let script_opens = html.matches("<script").count();
        let script_closes = html.matches("</script>").count();
        if script_opens > script_closes {
            issues.push(issue(
                "error",
                "html",
                format!(
                    "<script> 标签未闭合: 发现 {script_opens} 个 <script> 但只有 {script_closes} 个 </script>"
                ),
            ));
        }
    }

    // 检查 hx API 调用 (在 HTML 或 TSX 中) — hx-shell 桥通过 WebView2 原生桥注入，无需外部库
    let has_hx_api = combined_code.contains("hx.")
        || combined_code.contains("window.hx")
        || combined_code.contains("chrome.webview.postMessage");

    if !has_hx_api {
        issues.push(issue(
            "warning",
            "rpc",
            "未检测到 hx API 调用。\
             提示: 先调 await window.hx.getInfo() 发现可用 API，\
             然后使用 window.hx.writeFile(path, content) 等调用系统能力，\
             或 window.chrome.webview.postMessage 与壳通信。\
             二进制文件用 window.hx.fileUrl(path) 生成 hxfile:// URL。",
        ));
    }

    // TypeScript 模式: 检查不要用 import React from 'react'
    if has_tsx && !tsx_code.is_empty() {
        if tsx_code.contains("import React") || tsx_code.contains("from 'react'") || tsx_code.contains("from \"react\"") {
            issues.push(issue(
                "error",
                "tsx",
                "src/main.tsx 中使用了 `import ... from 'react'`。\
             React 是全局变量 (由 index.html 通过 CDN 加载)，不要 import。\
             修复: 使用 `const {{ useState }} = React;` 代替 import。",
            ));
        }
        // 检查 JSX 语法基本有效性
        if !tsx_code.contains("React") && !tsx_code.contains("ReactDOM") {
            issues.push(issue(
                "warning",
                "tsx",
                "src/main.tsx 中未引用 React 或 ReactDOM。\
             提示: 使用 `const {{ useState }} = React;` 和 `const {{ createRoot }} = ReactDOM;`。",
            ));
        }
    }

    // ── L1.5: JS/TS 语法校验 (内置 esbuild 真 parser, 缺失时退回手写括号匹配) ──
    // esbuild 报的行/列精确到括号, agent 拿到即可精准 patch, 不必再读 28KB 全文。
    if !tsx_code.is_empty() {
        check_js_with_esbuild(&tsx_code, "tsx", "src/main.tsx", 0, &mut issues);
    } else if html.contains("<script") {
        check_js_in_html(&html, &mut issues);
    }

    // ── L2: 权限一致性 (在 HTML + TSX 中检查) ──
    if !declared_perms.is_empty() {
        // 检查 HTML + TSX 中使用的 hx API 调用是否都有对应的 permission
        let used_apis = extract_hx_apis(&combined_code);
        for api in &used_apis {
            if let Some(required_perm) = HX_API_PERMISSIONS
                .iter()
                .find(|(m, _)| api.starts_with(m))
                .map(|(_, p)| *p)
            {
                if !declared_perms.iter().any(|p| p == required_perm) {
                    issues.push(issue(
                        "error",
                        "rpc",
                        format!(
                            "代码中调用了 `{api}`, 需要权限 `{required_perm}`, \
                             但 manifest.permissions 中未声明。\
                             修复: 在 manifest.json 的 permissions 数组中添加 \"{required_perm}\""
                        ),
                    ));
                }
            }
        }
    } else if has_hx_api {
        // 有 API 调用但没有声明任何权限
        issues.push(issue(
            "warning",
            "rpc",
            "index.html 中有 hx API 调用但 manifest 未声明 permissions。\
             提示: 在 manifest.json 中添加 permissions, 如 [\"fs-read\", \"fs-write\", \"exec\", \"dialog\"]",
        ));
    }

    // L1: 建议在 manifest 中声明 permissions
    if declared_perms.is_empty() {
        issues.push(issue(
            "warning",
            "manifest",
            "建议在 manifest 中声明 `permissions` (如 [\"fs-read\",\"exec\",\"dialog\"])。\
             未声明的 hx API 调用会在运行时被拒绝。",
        ));
    }

    // ── L3: 前端质量关卡 (交付前强制) ──
    // 把 custom-app-development SKILL 的「前端默认标准」变成可强制的硬指标，
    // 让 agent 开发循环在声明「开发完成」前拦截违规，实现「一次性到位」。
    check_frontend_quality(&combined_code, &mut issues);

    let ok = !issues.iter().any(|i| i.level == "error");
    ValidationReport { ok, issues }
}

/// L3 前端质量关卡: 离线可用 + 桌面定位 + 标题栏 + 无 emoji。
fn check_frontend_quality(code: &str, issues: &mut Vec<ValidationIssue>) {
    let lower = code.to_lowercase();

    // 1) 远程运行时外链 → 断网/内网白屏 (error, 阻断交付, spec §3.7)
    //
    // 禁：<script src="http..."> / <link href="http..."> / <img src="http..."> /
    //     <iframe src="http..."> / CSS @import url("http...")
    //     常见 CDN 域名黑名单兜底（unpkg / jsdelivr / cdnjs 等）
    // 放：本地相对 ./vendor/x.js、styles.css、assets/*；
    //     data: / blob: / 协议相对 //（用本机 webview host 时已分情况）
    //
    // 替代之前的「禁一切 CDN」实现 —— 现在允许本地相对外联（vendor + 资源），
    // 因为多文件归档已在 Phase 0 落地（spec §8.1），app 可以 vendor 外部库走相对外联。
    let remote_hits = detect_remote_runtime_assets(code);
    let cdn_hosts = [
        "unpkg.com",
        "jsdelivr",
        "cdnjs.cloudflare",
        "cdn.tailwindcss",
        "ajax.googleapis",
        "cdn.bootcdn",
    ];
    let cdn_hit = cdn_hosts.iter().find_map(|h| {
        let idx = lower.find(h)?;
        // 必须是在属性值或 URL 上下文（避免误中注释/字符串里的描述）
        // 粗判定：命中的前后 80 字符内至少一个 < 或者 src=/href=
        let ctx_start = idx.saturating_sub(80);
        let ctx_end = (idx + h.len() + 80).min(lower.len());
        let ctx = &lower[ctx_start..ctx_end];
        if ctx.contains("src=") || ctx.contains("href=") || ctx.contains("@import") {
            Some(*h)
        } else {
            None
        }
    });
    if !remote_hits.is_empty() || cdn_hit.is_some() {
        let why = if !remote_hits.is_empty() {
            format!(
                "检测到运行时远程资源: {}（这些地址用户断网/内网时会白屏）",
                remote_hits.join(", ")
            )
        } else {
            format!("检测到 CDN 域名 ({}) 在 src=/href= 属性上下文中", cdn_hit.unwrap())
        };
        issues.push(issue("error", "frontend", format!(
            "{why}。自定义应用必须离线可用: 框架/状态/组件/图标走壳预置原语 \
             (window.Hermix.store / defineComponent / icon / route / storage / Preact)，\
             外部库通过创作期 vendor 到 app_dir/vendor/ 后用相对外联 \
             (<script src=\"./vendor/x.js\">、<link href=\"styles.css\">), \
             运行时禁止 http(s)://。"
        )));
    }

    // 2) 浏览器 input file / webkitdirectory → 桌面定位违例 (error)
    if code.contains("type=\"file\"")
        || code.contains("type='file'")
        || code.contains("webkitdirectory")
        || lower.contains("input type=file")
    {
        issues.push(issue(
            "error",
            "frontend",
            "检测到 <input type=\"file\"> / webkitdirectory。桌面应用必须用壳层原生对话框: \
             window.hx.openDialog({directory:true}) / openDialog({filters:[...]}) / saveDialog(...), \
             禁止浏览器文件选择 (会触发 WebView2 上传确认弹窗, 与桌面工具定位不符)。",
        ));
    }

    // 3) 标题栏缺失 → V7 壳无原生标题栏 (warning)
    let has_titlebar = code.contains("hx.win.startDrag")
        || code.contains("win.startDrag")
        || code.contains("hx-titlebar")
        || code.contains("class=\"hx-titlebar\"");
    if !has_titlebar {
        issues.push(issue(
            "warning",
            "frontend",
            "未检测到自定义标题栏 (hx.win.startDrag / hx-titlebar)。\
             V7 壳窗口无原生标题栏 (decorations:false), 必须在 HTML 顶部自行实现标题栏 \
             (拖动区 + 最小化/最大化/关闭按钮), 否则用户无法移动或关闭窗口 (只能 Alt+F4)。",
        ));
    }

    // 4) custom scheme fetch → 用 window.hx.* (warning)
    if lower.contains("fetch(") && (lower.contains("app://") || lower.contains("app.localhost") || lower.contains("/hx/"))
    {
        issues.push(issue(
            "warning",
            "frontend",
            "检测到对 custom scheme 的 fetch (app:// / app.localhost / /hx/)。\
             Chromium 对自定义 scheme 的 fetch 有 CORS 限制会失败。改用 window.hx.* (WebView2 原生桥) 或 fetch 到 https URL。",
        ));
    }

    // 5) emoji → 界面禁止 (warning)
    if let Some(em) = detect_emoji(code) {
        issues.push(issue(
            "warning",
            "frontend",
            format!(
                "界面包含 emoji 字符 (如 {em})。自定义应用界面禁止 emoji (标题/按钮/状态/装饰均不可用), \
                 改用内联 SVG 图标 (lucide 风格, stroke=currentColor)。"
            ),
        ));
    }
}

/// 检测代码里是否含 emoji (返回第一个命中的字符用于提示)。
/// 覆盖常见 emoji 平面: Misc Symbols / Dingbats / Emoji。
fn detect_emoji(s: &str) -> Option<String> {
    for c in s.chars() {
        let cp = c as u32;
        let is_emoji = (0x1F300..=0x1FAFF).contains(&cp)
            || (0x2600..=0x27BF).contains(&cp)
            || (0x2B00..=0x2BFF).contains(&cp);
        if is_emoji {
            return Some(c.to_string());
        }
    }
    None
}

/// 检测所有 `<tag src|href="http(s)://...">` 与 CSS `@import url(http://...)`，
/// 返回人类可读命中标签列表。
///
/// 设计要点（spec §3.7）：
/// - 正则：`<(?:script|link|img|iframe|video|audio|source)\\b[^>]*?\\b(?:src|href)\\s*=\\s*["']https?://[^"']+["']`
/// - `@import\\s+url\\(\\s*["']?https?://[^"')]+["']?\\s*\\)`
/// - 放行：`./` `/` `data:` `blob:` `mailto:` `#` 协议相对 `//`。
///
/// 返回简短人类标签（如 `script src=https://...` / `@import https://...`），
/// 不超过 3 条（避免错误信息太长）。
fn detect_remote_runtime_assets(code: &str) -> Vec<String> {
    use regex_lite::Regex;
    let tag_re = Regex::new(
        r#"(?i)<(?:script|link|img|iframe|video|audio|source)\b[^>]*?\b(?:src|href)\s*=\s*["']\s*(https?://[^"']+)["']"#,
    )
    .expect("regexp valid");
    let import_re =
        Regex::new(r#"(?i)@import\s+url\(\s*["']?\s*(https?://[^"')]+)\s*["']?\s*\)"#)
            .expect("regexp valid");

    let mut hits: Vec<String> = Vec::new();
    for cap in tag_re.captures_iter(code) {
        let url = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        // 截短展示
        let display = if url.len() > 60 {
            format!("{}…", &url[..59])
        } else {
            url.to_string()
        };
        hits.push(display);
        if hits.len() >= 3 {
            break;
        }
    }
    if hits.len() < 3 {
        for cap in import_re.captures_iter(code) {
            let url = cap.get(1).map(|m| m.as_str()).unwrap_or("");
            let display = if url.len() > 60 {
                format!("{}…", &url[..59])
            } else {
                url.to_string()
            };
            hits.push(format!("@import {}", display));
            if hits.len() >= 3 {
                break;
            }
        }
    }
    hits
}

/// 逐段抽取 HTML 中的 `<script>` 内容, 用 esbuild 真 parser 校验,
/// 并把 esbuild 报的行号映射回 HTML 文件行号 (script 起始行 + 偏移)。
fn check_js_in_html(html: &str, issues: &mut Vec<ValidationIssue>) {
    let mut pos = 0;
    let mut idx = 0usize;
    while let Some(open) = html[pos..].find("<script") {
        let abs_open = pos + open;
        // 找到 <script ...> 的 '>' 结束标签
        let tag_end = match html[abs_open..].find('>') {
            Some(t) => t,
            None => break,
        };
        let js_start = abs_open + tag_end + 1;
        // 该 script 块在原始 HTML 中的起始行 (1-based)
        let html_start_line = html[..js_start].matches('\n').count() as u32 + 1;
        let end = match html[js_start..].find("</script>") {
            Some(e) => e,
            None => break,
        };
        let js = &html[js_start..js_start + end];
        idx += 1;
        if !js.trim().is_empty() {
            check_js_with_esbuild(
                js,
                "js",
                &format!("index.html <script#{idx}>"),
                html_start_line,
                issues,
            );
        }
        pos = js_start + end + 9; // 跳过 </script>
    }
}

/// 用内置 esbuild 做真 JS/TS 语法校验 (替代手写括号匹配)。
///
/// `source_label` 用于报错展示; `html_line_offset` 是代码段在原始 HTML 中的
/// 起始行, esbuild 报的行号需加上此偏移才能映射回 HTML 文件行 (TSX 模式传 0)。
/// esbuild 不可用时退回手写 `check_js_brackets` 兜底, 保证校验永不硬失败。
fn check_js_with_esbuild(
    code: &str,
    loader: &str,
    source_label: &str,
    html_line_offset: u32,
    issues: &mut Vec<ValidationIssue>,
) {
    match run_esbuild_check(code, loader, source_label) {
        Some((stderr, success)) => {
            if success {
                return; // esbuild 确认无语法错误
            }
            let parsed = parse_esbuild_errors(&stderr);
            if parsed.is_empty() {
                // esbuild 报了错但解析不出结构 (极端情况) → 退回手写检查兜底
                check_js_brackets(code, issues);
            } else {
                for (line, col, msg, snippet) in parsed {
                    let html_line = if html_line_offset > 0 {
                        html_line_offset + line.saturating_sub(1)
                    } else {
                        line
                    };
                    let loc = if html_line_offset > 0 {
                        format!("{source_label} 第 {html_line} 行第 {col} 列 (HTML 文件)")
                    } else {
                        format!("{source_label} 第 {line} 行第 {col} 列")
                    };
                    let mut message = format!("JS/TS 语法错误: {loc}: {msg}");
                    if !snippet.is_empty() {
                        message.push_str(&format!("\n  代码: {snippet}"));
                    }
                    issues.push(ValidationIssue {
                        level: "error".to_string(),
                        code: "js".to_string(),
                        message,
                        line: Some(html_line),
                        col: Some(col),
                        snippet: if snippet.is_empty() {
                            None
                        } else {
                            Some(snippet)
                        },
                    });
                }
            }
        }
        None => {
            // esbuild 不可用 → 手写兜底
            check_js_brackets(code, issues);
        }
    }
}

/// 调内置 esbuild (stdin transform 模式) 校验单段 JS/TS 语法。
/// 返回 (stderr 文本, esbuild 是否退出成功); esbuild 缺失返回 None。
fn run_esbuild_check(code: &str, loader: &str, sourcefile: &str) -> Option<(String, bool)> {
    let esbuild = crate::hx_packager::HxShellPackager::resolve_loader_file("esbuild.exe");
    if !esbuild.exists() {
        return None;
    }
    let mut child = Command::new(&esbuild)
        .arg(format!("--loader={loader}"))
        .arg("--format=esm")
        .arg(format!("--sourcefile={sourcefile}"))
        .arg("--log-level=verbose")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    {
        let mut stdin = child.stdin.take()?;
        let _ = stdin.write_all(code.as_bytes());
        // stdin 离开作用域 drop → EOF, esbuild 开始解析
    }
    let out = child.wait_with_output().ok()?;
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    Some((stderr, out.status.success()))
}

/// 解析 esbuild 的 stderr, 抽出每条错误的 (行, 列, 消息, 代码片段)。
///
/// esbuild 单错误格式 (transform 模式):
/// ```text
/// X [ERROR] Expected ")" but found ";"
///
///     index.html#script1:1:14:
///       1 │ const x = (1+2;
///         │               ^
///         ╵               )
/// ```
fn parse_esbuild_errors(stderr: &str) -> Vec<(u32, u32, String, String)> {
    let mut out: Vec<(u32, u32, String, String)> = Vec::new();
    let mut cur_msg: Option<String> = None;
    let mut cur_line: Option<u32> = None;
    let mut cur_col: Option<u32> = None;
    let mut cur_snippet: Option<String> = None;

    let flush = |out: &mut Vec<(u32, u32, String, String)>,
                     cur_msg: &mut Option<String>,
                     cur_line: &mut Option<u32>,
                     cur_col: &mut Option<u32>,
                     cur_snippet: &mut Option<String>| {
        if let (Some(msg), Some(l), Some(c), Some(snip)) =
            (cur_msg.take(), cur_line.take(), cur_col.take(), cur_snippet.take())
        {
            out.push((l, c, msg, snip));
        }
    };

    for raw in stderr.lines() {
        let line = raw.trim_end();
        // 错误消息行
        if let Some(m) = line.trim().strip_prefix("X [ERROR] ") {
            flush(
                &mut out,
                &mut cur_msg,
                &mut cur_line,
                &mut cur_col,
                &mut cur_snippet,
            );
            cur_msg = Some(m.to_string());
            continue;
        }
        if cur_msg.is_some() {
            // 位置行: "    index.html#script1:1:14:"
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_suffix(':') {
                if let Some((a, b)) = rest.rsplit_once(':') {
                    if let Some((_, l)) = a.rsplit_once(':') {
                        if let (Ok(lineno), Ok(colno)) = (l.parse::<u32>(), b.parse::<u32>()) {
                            cur_line = Some(lineno);
                            cur_col = Some(colno);
                        }
                    }
                }
            }
            // 代码片段行: "      1 │ const x = (1+2;"
            if cur_line.is_some() {
                if let Some(idx) = line.find('│') {
                    if cur_snippet.is_none() {
                        cur_snippet = Some(line[idx + '│'.len_utf8()..].trim_start().to_string());
                    }
                }
            }
        }
    }
    flush(
        &mut out,
        &mut cur_msg,
        &mut cur_line,
        &mut cur_col,
        &mut cur_snippet,
    );
    out
}

/// 基本 JS 括号匹配检查。
fn check_js_brackets(js: &str, issues: &mut Vec<ValidationIssue>) {
    let mut stack: Vec<(char, usize)> = Vec::new();
    let chars: Vec<char> = js.chars().collect();
    let mut in_string: Option<char> = None;
    let mut in_template = false;
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];

        // 处理字符串
        if in_string.is_some() {
            if c == '\\' && i + 1 < chars.len() {
                i += 2; // 跳过转义字符
                continue;
            }
            if Some(c) == in_string {
                in_string = None;
            }
            i += 1;
            continue;
        }

        // 处理模板字符串
        if in_template {
            if c == '\\' && i + 1 < chars.len() {
                i += 2;
                continue;
            }
            if c == '`' {
                in_template = false;
            }
            i += 1;
            continue;
        }

        match c {
            '\'' | '"' => in_string = Some(c),
            '`' => in_template = true,
            '/' if i + 1 < chars.len() && chars[i + 1] == '/' => {
                // 行注释, 跳到行尾
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            '/' if i + 1 < chars.len() && chars[i + 1] == '*' => {
                // 块注释, 跳到 */
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
                continue;
            }
            '(' | '[' | '{' => stack.push((c, i)),
            ')' => {
                if let Some((open, _)) = stack.last() {
                    if *open == '(' {
                        stack.pop();
                    } else {
                        issues.push(issue(
                            "error",
                            "js",
                            format!(
                                "JS 括号不匹配: 第 {} 字符处发现 ')' 但期望 '}}' 或 ']'",
                                i
                            ),
                        ));
                        break;
                    }
                } else {
                    issues.push(issue(
                        "error",
                        "js",
                        format!("JS 括号不匹配: 第 {} 字符处发现 ')' 但没有匹配的 '('", i),
                    ));
                    break;
                }
            }
            ']' => {
                if let Some((open, _)) = stack.last() {
                    if *open == '[' {
                        stack.pop();
                    } else {
                        issues.push(issue(
                            "error",
                            "js",
                            format!(
                                "JS 括号不匹配: 第 {} 字符处发现 ']' 但期望 ')' 或 '}}'",
                                i
                            ),
                        ));
                        break;
                    }
                } else {
                    issues.push(issue(
                        "error",
                        "js",
                        format!("JS 括号不匹配: 第 {} 字符处发现 ']' 但没有匹配的 '['", i),
                    ));
                    break;
                }
            }
            '}' => {
                if let Some((open, _)) = stack.last() {
                    if *open == '{' {
                        stack.pop();
                    } else {
                        issues.push(issue(
                            "error",
                            "js",
                            format!(
                                "JS 括号不匹配: 第 {} 字符处发现 '}}' 但期望 ')' 或 ']'",
                                i
                            ),
                        ));
                        break;
                    }
                } else {
                    issues.push(issue(
                        "error",
                        "js",
                        format!("JS 括号不匹配: 第 {} 字符处发现 '}}' 但没有匹配的 '{{'", i),
                    ));
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }

    // 检查未闭合的括号 (最多报前 3 个)
    let unclosed: Vec<_> = stack.iter().take(3).collect();
    for (open, pos) in unclosed {
        issues.push(issue(
            "error",
            "js",
            format!(
                "JS 括号不匹配: 第 {} 字符处的 '{}' 未闭合",
                pos,
                match *open {
                    '(' => "( ... )",
                    '[' => "[ ... ]",
                    '{' => "{ ... }",
                    _ => "unknown",
                }
            ),
        ));
    }
}

/// 从 HTML 中提取 hx API 调用。
///
/// 匹配模式: `hx.methodName(`
/// 例如: `hx.readFile(`, `hx.writeFile(`
fn extract_hx_apis(html: &str) -> Vec<String> {
    let mut apis = Vec::new();
    let known_prefixes: Vec<&str> = HX_API_PERMISSIONS
        .iter()
        .map(|(m, _)| *m)
        .collect();

    // 匹配 hx.<methodName>(
    for cap in regex_lite::Regex::new(r"\bhx\.(\w+)\s*\(")
        .unwrap()
        .captures_iter(html)
    {
        if let Some(m) = cap.get(1) {
            let api = format!("hx.{}", m.as_str());
            // Keep it if it matches a known prefix
            let is_known = known_prefixes
                .iter()
                .any(|p| api.starts_with(p));
            if is_known && !apis.contains(&api) {
                apis.push(api);
            }
        }
    }

    apis
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good_manifest() -> String {
        r#"{"name":"测试","id":"test-app","category":"developer"}"#.to_string()
    }

    #[test]
    fn tsx_app_passes() {
        let tmp = std::env::temp_dir().join(format!("va_tsx_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("src")).unwrap();
        std::fs::write(
            tmp.join("manifest.json"),
            r#"{"name":"测试","id":"test-tsx","category":"developer","permissions":["fs-read","exec"]}"#,
        )
        .unwrap();
        // Auto-generated HTML shell (TypeScript mode)
        std::fs::write(
            tmp.join("index.html"),
            r#"<html><head></head><body><div id="root"></div><script src="/js/app.js"></script></body></html>"#,
        )
        .unwrap();
        std::fs::write(
            tmp.join("src/main.tsx"),
            r#"
const { useState } = React;
const { createRoot } = ReactDOM;

function App() {
    const [count, setCount] = useState(0);
    async function readFile() {
        const r = await window.hx.readFile('test.txt');
        const r2 = await window.hx.exec('python a.py');
    }
    return <div onClick={() => setCount(c => c + 1)}>{count}</div>;
}

createRoot(document.getElementById('root')!).render(<App />);
"#,
        )
        .unwrap();
        let r = validate_custom_app(&tmp);
        assert!(r.ok, "TypeScript/React 应用应无 error: {:?}", r.issues);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn tsx_app_import_react_fails() {
        let tmp = std::env::temp_dir().join(format!("va_tsx_import_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("src")).unwrap();
        std::fs::write(
            tmp.join("manifest.json"),
            r#"{"name":"测试","id":"test-tsx-import","permissions":["fs-read"]}"#,
        )
        .unwrap();
        std::fs::write(
            tmp.join("index.html"),
            r#"<html><head></head><body></body></html>"#,
        )
        .unwrap();
        std::fs::write(
            tmp.join("src/main.tsx"),
            r#"
import React from 'react';
function App() { return <div>hello</div>; }
"#,
        )
        .unwrap();
        let r = validate_custom_app(&tmp);
        assert!(!r.ok, "import React 应报 error: {:?}", r.issues);
        assert!(r.issues.iter().any(|i| i.code == "tsx"), "应有 tsx 类 error");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn good_app_passes() {
        let tmp = std::env::temp_dir().join(format!("va_good_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("manifest.json"),
            r#"{"name":"测试","id":"test-good","category":"developer","permissions":["fs-read"]}"#,
        )
        .unwrap();
        std::fs::write(
            tmp.join("index.html"),
            r#"<html><head></head><body><script>window.hx.readFile('test.txt').then(c => console.log(c));</script></body></html>"#,
        )
        .unwrap();
        let r = validate_custom_app(&tmp);
        assert!(r.ok, "hx-shell 应用应无 error: {:?}", r.issues);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn hx_app_passes() {
        let tmp = std::env::temp_dir().join(format!("va_new_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("manifest.json"),
            r#"{"name":"测试","id":"test-new","category":"developer","permissions":["fs-read","exec"]}"#,
        )
        .unwrap();
        std::fs::write(
            tmp.join("index.html"),
            r#"<html><head></head><body><script>
async function run() {
    const r = await window.hx.readFile('test.txt');
    const r2 = await window.hx.exec('python a.py');
}
</script></body></html>"#,
        )
        .unwrap();
        let r = validate_custom_app(&tmp);
        assert!(r.ok, "hx-shell 应用应无 error: {:?}", r.issues);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn missing_index_html_fails() {
        let tmp = std::env::temp_dir().join(format!("va_nohtml_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("manifest.json"), &good_manifest()).unwrap();
        let r = validate_custom_app(&tmp);
        assert!(!r.ok);
        assert!(r.issues.iter().any(|i| i.code == "manifest"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn missing_manifest_fails() {
        let tmp = std::env::temp_dir().join(format!("va_noman_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("index.html"), "<html></html>").unwrap();
        let r = validate_custom_app(&tmp);
        assert!(!r.ok);
        assert!(r.issues.iter().any(|i| i.code == "manifest"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn bad_manifest_json_fails() {
        let tmp = std::env::temp_dir().join(format!("va_badjson_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("manifest.json"), "{ this is not json ").unwrap();
        std::fs::write(tmp.join("index.html"), "<html></html>").unwrap();
        let r = validate_custom_app(&tmp);
        assert!(!r.ok);
        assert!(r.issues.iter().any(|i| i.code == "manifest"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn unclosed_script_tag_fails() {
        let tmp = std::env::temp_dir().join(format!("va_unclosed_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("manifest.json"),
            r#"{"name":"测试","id":"test-u","permissions":["fs-read"]}"#,
        )
        .unwrap();
        std::fs::write(
            tmp.join("index.html"),
            "<html><head></head><body><script>window.hx.readFile()</body></html>",
        )
        .unwrap();
        let r = validate_custom_app(&tmp);
        assert!(!r.ok, "未闭合 script 标签应报 error: {:?}", r.issues);
        assert!(r.issues.iter().any(|i| i.code == "html"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn api_without_permission_fails() {
        let tmp = std::env::temp_dir().join(format!("va_noperm_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("manifest.json"),
            r#"{"name":"测试","id":"test-np","permissions":["fs-read"]}"#,
        )
        .unwrap();
        // 使用 execCommand 但只声明了 fs-read 权限
        std::fs::write(
            tmp.join("index.html"),
            r#"<html><head></head><body><script>window.hx.exec('python')</script></body></html>"#,
        )
        .unwrap();
        let r = validate_custom_app(&tmp);
        assert!(!r.ok, "缺少 exec 权限应报 error: {:?}", r.issues);
        assert!(r.issues.iter().any(|i| i.code == "rpc" && i.level == "error"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn js_bracket_mismatch_fails() {
        let tmp = std::env::temp_dir().join(format!("va_bracket_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("manifest.json"),
            r#"{"name":"测试","id":"test-b","permissions":["fs-read"]}"#,
        )
        .unwrap();
        std::fs::write(
            tmp.join("index.html"),
            r#"<html><body><script>function test( { }</script></body></html>"#,
        )
        .unwrap();
        let r = validate_custom_app(&tmp);
        assert!(!r.ok, "JS 括号不匹配应报 error: {:?}", r.issues);
        assert!(r.issues.iter().any(|i| i.code == "js"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn extract_hx_apis_works() {
        let html = r#"<script>window.hx.readFile('a'); window.hx.exec('x');</script>"#;
        let apis = extract_hx_apis(html);
        assert!(apis.contains(&"hx.readFile".to_string()), "应检测到 readFile: {apis:?}");
        assert!(apis.contains(&"hx.exec".to_string()), "应检测到 exec: {apis:?}");
    }

    #[test]
    fn parse_esbuild_errors_extracts_line_col_snippet() {
        // 模拟 esbuild transform 模式的真实 stderr 格式
        let stderr = "\
X [ERROR] Expected \")\" but found \";\"

    index.html#script1:1:14:
      1 │ const x = (1+2;
        │               ^
        ╵               )";
        let parsed = parse_esbuild_errors(stderr);
        assert_eq!(parsed.len(), 1, "应解析出 1 条错误");
        let (line, col, msg, snippet) = &parsed[0];
        assert_eq!(*line, 1);
        assert_eq!(*col, 14);
        assert!(msg.contains("Expected"), "消息应含 Expected: {msg}");
        assert!(
            snippet.contains("const x = (1+2;"),
            "片段应含出错代码: {snippet}"
        );
    }

    // 端到端验证 esbuild 真 parser 路径能报出精确 HTML 行号。
    // 仅在仓库内置 esbuild.exe 存在时校验行号; 否则退回手写兜底 (仍有 js 问题)。
    #[test]
    fn validate_reports_precise_js_location_via_esbuild() {
        let repo_esbuild = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../apps/desktop-tauri/src-tauri/resources/loader/esbuild.exe");
        let has_esbuild = repo_esbuild.exists();

        let tmp = std::env::temp_dir().join(format!("va_esbuild_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("loader")).unwrap();
        if has_esbuild {
            std::fs::copy(&repo_esbuild, tmp.join("loader").join("esbuild.exe")).unwrap();
        }
        // 指向临时 HERMIX_HOME, 让 resolve_loader_file 找到 esbuild
        std::env::set_var("HERMIX_HOME", &tmp);

        let app = tmp.join("app");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("manifest.json"), r#"{"name":"t","id":"t"}"#).unwrap();
        // <script> 在第4行, 其内 JS 第1行(HTML第5行)有语法错: 缺 ')'
        std::fs::write(
            app.join("index.html"),
            "<!DOCTYPE html>\n<html>\n<body>\n<script>\nconst x = (1+2;\n</script>\n</body>\n</html>\n",
        )
        .unwrap();

        let r = validate_custom_app(&app);
        let _ = std::fs::remove_dir_all(&tmp);

        let js_issue = r.issues.iter().find(|i| i.code == "js");
        assert!(js_issue.is_some(), "应报告 js 语法错误, issues={:?}", r.issues);
        let issue = js_issue.unwrap();
        if has_esbuild {
            assert_eq!(
                issue.line,
                Some(5),
                "esbuild 路径: 错误应在 HTML 第5行, 实际={:?} msg={}",
                issue.line,
                issue.message
            );
            assert!(issue.col.is_some(), "应有精确列号");
            assert!(issue.snippet.is_some(), "应有代码片段");
        }
    }
}
