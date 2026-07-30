//! 自定义应用「开发期自验证」(shift-left validation).
//!
//! 目标: 让 LLM 在 *开发* 自定义应用时就能拦截打包/运行期错误,
//! 而不是等用户点击打包或启动 loader 时才发现。新架构 (standalone wry
//! loader) 下应用即静态 `index.html`, 校验聚焦:
//!
//! - **L0 静态契约**: manifest 完整性 + permissions 声明
//! - **L1 HTML/JS 预检**: 结构完整性、RPC 调用模式、括号匹配
//! - **L2 权限一致性**: HTML 中的 RPC 方法是否在 permissions 中声明
//!
//! 调用方 (agent 开发循环) 应在生成/修改应用代码后调用
//! [`validate_custom_app`], 看到 `ok == false` 就继续修复, 直到通过
//! 才视为「开发完成」。运行时冒烟测试 (L3, loader 无头模式捕获 console 错误)
//! 作为后续阶段, 不在本模块。
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

/// 已知 RPC 方法 → 所需权限的映射。
const RPC_METHOD_PERMISSIONS: &[(&str, &str)] = &[
    ("fs_read", "fs-read"),
    ("fs_read_bytes", "fs-read"),
    ("fs_list", "fs-read"),
    ("fs_exists", "fs-read"),
    ("fs_write", "fs-write"),
    ("fs_write_bytes", "fs-write"),
    ("fs_mkdir", "fs-write"),
    ("fs_move", "fs-write"),
    ("fs_copy", "fs-write"),
    ("fs_delete", "fs-write"),
    ("exec", "exec"),
    ("shell", "exec"),
    ("fetch", "network"),
    ("dialog_open", "dialog"),
    ("dialog_save", "dialog"),
    ("env_get", "env"),
    ("env_list", "env"),
    ("temp_dir", "temp"),
    ("temp_file", "temp"),
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

    // 应用模型: 只支持新架构 (self-contained index.html). 旧 esbuild 壳模型已退役.
    let index_html = app_dir.join("index.html");

    if !index_html.exists() {
        return ValidationReport {
            ok: false,
            issues: vec![issue(
                "error",
                "manifest",
                "缺少 index.html (新架构应用需要自包含 index.html + manifest.json)",
            )],
        };
    }

    // ── L1: HTML 结构预检 ──
    let html = std::fs::read_to_string(&index_html).unwrap_or_default();

    // 检查基本 HTML 结构
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

    // 检查 RPC 调用
    let has_rpc = html.contains("app://localhost/rpc") || html.contains("rpc(");

    if !has_rpc {
        issues.push(issue(
            "warning",
            "rpc",
            "index.html 未发现对 `app://localhost/rpc` 的调用, 应用可能无法与桥接通信。\
             提示: 使用 rpc('方法名', {参数}) 调用桥接, 例如: rpc('fs_read', {path: '/tmp/a.txt'})",
        ));
    }

    // ── L1.5: JS 括号匹配预检 ──
    // 提取 <script> 标签内的内容进行基本检查
    if has_script {
        let js_code = extract_js_from_html(&html);
        if !js_code.is_empty() {
            check_js_brackets(&js_code, &mut issues);
        }
    }

    // ── L2: 权限一致性 ──
    if !declared_perms.is_empty() {
        // 检查 HTML 中使用的 RPC 方法是否都有对应的 permission
        let used_methods = extract_rpc_methods(&html);
        for method in &used_methods {
            if let Some(required_perm) = RPC_METHOD_PERMISSIONS.iter().find(|(m, _)| *m == method).map(|(_, p)| *p) {
                if !declared_perms.iter().any(|p| p == required_perm) {
                    issues.push(issue(
                        "error",
                        "rpc",
                        format!(
                            "HTML 中调用了 RPC 方法 `{method}`, 需要权限 `{required_perm}`, \
                             但 manifest.permissions 中未声明。\
                             修复: 在 manifest.json 的 permissions 数组中添加 \"{required_perm}\""
                        ),
                    ));
                }
            }
        }
    } else if has_rpc {
        // 有 RPC 调用但没有声明任何权限
        issues.push(issue(
            "warning",
            "rpc",
            "index.html 中有桥接调用但 manifest 未声明 permissions。\
             提示: 在 manifest.json 中添加 permissions, 如 [\"fs-read\", \"fs-write\", \"exec\", \"dialog\", \"network\"]",
        ));
    }

    // L1: 新架构应用建议在 manifest 中声明 permissions
    if declared_perms.is_empty() {
        issues.push(issue(
            "warning",
            "manifest",
            "新架构应用建议在 manifest 中声明 `permissions` (如 [\"fs-read\",\"exec\",\"dialog\"])。\
             未声明则桥接调用会被默认拒绝。",
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

/// 从 HTML 中提取 RPC 方法名。
/// 匹配模式: rpc('method_name' 或 rpc("method_name" 或 "method":"method_name"
fn extract_rpc_methods(html: &str) -> Vec<String> {
    let mut methods = Vec::new();
    let known_methods: Vec<&str> = RPC_METHOD_PERMISSIONS.iter().map(|(m, _)| *m).collect();

    // 模式1: rpc('method' 或 rpc("method"
    for cap in regex_lite::Regex::new(r#"rpc\(\s*['"](\w+)['"]"#)
        .unwrap()
        .captures_iter(html)
    {
        if let Some(m) = cap.get(1) {
            let method = m.as_str().to_string();
            methods.push(method);
        }
    }

    // 模式2: "method":"method_name" (在 fetch body 中)
    for cap in regex_lite::Regex::new(r#""method"\s*:\s*"(\w+)""#)
        .unwrap()
        .captures_iter(html)
    {
        if let Some(m) = cap.get(1) {
            let method = m.as_str().to_string();
            if !methods.contains(&method) {
                methods.push(method);
            }
        }
    }

    // 过滤: 只保留已知方法 (避免误报)
    methods.retain(|m| known_methods.contains(&m.as_str()));
    methods
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good_manifest() -> String {
        r#"{"name":"测试","id":"test-app","category":"developer"}"#.to_string()
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
            "<html><body><script>fetch('app://localhost/rpc',{method:'POST'})</script></body></html>",
        )
        .unwrap();
        let r = validate_custom_app(&tmp);
        assert!(r.ok, "新架构应用应无 error: {:?}", r.issues);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn new_model_app_passes() {
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
            r#"<html><body><script>
async function run() {
    let r = await rpc('fs_read', {path: 'test.txt'});
    let r2 = await rpc('exec', {cmd: 'python', args: ['a.py']});
}
</script></body></html>"#,
        )
        .unwrap();
        let r = validate_custom_app(&tmp);
        assert!(r.ok, "新架构应用应无 error: {:?}", r.issues);
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
            "<html><body><script>fetch('app://localhost/rpc')</body></html>",
        )
        .unwrap();
        let r = validate_custom_app(&tmp);
        assert!(!r.ok, "未闭合 script 标签应报 error: {:?}", r.issues);
        assert!(r.issues.iter().any(|i| i.code == "html"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn rpc_method_without_permission_fails() {
        let tmp = std::env::temp_dir().join(format!("va_noperm_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("manifest.json"),
            r#"{"name":"测试","id":"test-np","permissions":["fs-read"]}"#,
        )
        .unwrap();
        // 使用 exec 方法但只声明了 fs-read 权限
        std::fs::write(
            tmp.join("index.html"),
            r#"<html><body><script>rpc('exec', {cmd: 'python'})</script></body></html>"#,
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
    fn extract_rpc_methods_works() {
        let html = r#"<script>rpc('fs_read', {path: 'a'}); rpc("exec", {cmd: 'x'});</script>"#;
        let methods = extract_rpc_methods(html);
        assert!(methods.contains(&"fs_read".to_string()), "应检测到 fs_read: {methods:?}");
        assert!(methods.contains(&"exec".to_string()), "应检测到 exec: {methods:?}");
    }
}
