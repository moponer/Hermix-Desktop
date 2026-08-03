//! 自定义应用「开发期自验证」(shift-left validation).
//!
//! 目标: 让 LLM 在 *开发* 自定义应用时就能拦截打包/运行期错误。
//! hx-shell 架构下应用为静态 `index.html` + `window.hx.*` 绑定（WebView2 原生桥）。
//! 校验聚焦:
//!
//! - **L0 静态契约**: manifest 完整性 + permissions 声明
//! - **L1 HTML/JS 预检**: 结构完整性、hx API 调用模式、括号匹配
//! - **L2 权限一致性**: HTML 中的 hx API 调用是否在 permissions 中声明
//!
//! 调用方 (agent 开发循环) 应在生成/修改应用代码后调用
//! [`validate_custom_app`], 看到 `ok == false` 就继续修复, 直到通过
//! 才视为「开发完成」。
//!
//! 设计: 纯同步、零 panic、无外部依赖。

use std::path::Path;

/// 单条校验问题。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ValidationIssue {
    /// `"error"` 阻断打包; `"warning"` 仅提示。
    pub level: String,
    /// 问题类别: `manifest` / `html` / `rpc` / `js`。
    pub code: String,
    /// 人类可读描述。
    pub message: String,
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
             提示: 使用 window.hx.writeFile(path, content) 等调用系统能力，\
             或 window.chrome.webview.postMessage 与壳通信。",
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

    // ── L1.5: JS/TS 括号匹配预检 ──
    if !tsx_code.is_empty() {
        check_js_brackets(&tsx_code, &mut issues);
    } else if html.contains("<script") {
        let js_code = extract_js_from_html(&html);
        if !js_code.is_empty() {
            check_js_brackets(&js_code, &mut issues);
        }
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
    let ok = !issues.iter().any(|i| i.level == "error");
    ValidationReport { ok, issues }
}

/// 从 HTML 中提取 <script> 标签内的 JS 代码。
fn extract_js_from_html(html: &str) -> String {
    let mut js_parts = Vec::new();
    let mut pos = 0;
    while let Some(start) = html[pos..].find("<script") {
        let after_tag = pos + start;
        // 找到 > 结束标签
        if let Some(tag_end) = html[after_tag..].find('>') {
            let js_start = after_tag + tag_end + 1;
            if let Some(end) = html[js_start..].find("</script>") {
                js_parts.push(html[js_start..js_start + end].to_string());
                pos = js_start + end + 9; // skip </script>
            } else {
                break; // 未闭合, 前面已经报错了
            }
        } else {
            break;
        }
    }
    js_parts.join("\n")
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
}
