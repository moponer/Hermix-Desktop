//! 自定义应用「开发期自验证」(shift-left validation)。
//!
//! 目标: 让 LLM 在 *开发* 自定义应用时就能拦截打包/运行期错误,
//! 而不是等用户点击打包或启动壳时才发现。覆盖两类最高频错误:
//!
//! - **L0 静态契约**: manifest 完整性 + `ui.tsx` 只允许导入白名单
//!   (`react` / `react-dom/client` 由壳 import map 提供, 其余只能是
//!   相对路径/URL)。裸导入其它 npm 包会在 esbuild 阶段 `Could not resolve`。
//! - **L1 预打包**: 用与真实打包 *完全相同* 的 esbuild 标志先跑一遍
//!   `ui.tsx` / `backend.ts` 的 bundle, 任何 esbuild 错误 (如旧的
//!   `--outfile` 空格形式、模块解析失败) 在此阶段暴露。
//!
//! 调用方 (agent 开发循环) 应在生成/修改应用代码后调用
//! [`validate_custom_app`], 看到 `ok == false` 就继续修复, 直到通过
//! 才视为「开发完成」。运行时冒烟测试 (L2, 无头加载壳捕获 console 错误)
//! 作为后续阶段, 不在本模块。
//!
//! 设计: 纯同步、零 panic、无外部依赖 (esbuild 缺失时降级为告警而非失败)。

use std::path::Path;

/// 单条校验问题。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ValidationIssue {
    /// `"error"` 阻断打包; `"warning"` 仅提示。
    pub level: String,
    /// 问题类别: `manifest` / `import` / `esbuild`。
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

/// 提取源码中的导入说明符 (import/export ... from / import() / require())。
fn extract_import_specifiers(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let triggers = ["from ", "import(", "require("];
    for trig in triggers {
        let mut start = 0;
        while let Some(pos) = src[start..].find(trig) {
            let abs = start + pos + trig.len();
            // 跳过空白
            let mut j = abs;
            while j < src.len() && (src.as_bytes()[j] == b' ' || src.as_bytes()[j] == b'\t') {
                j += 1;
            }
            if j < src.len() && (src.as_bytes()[j] == b'\'' || src.as_bytes()[j] == b'"') {
                let q = src.as_bytes()[j];
                let mut k = j + 1;
                let mut s = String::new();
                while k < src.len() && src.as_bytes()[k] != q {
                    s.push(src.as_bytes()[k] as char);
                    k += 1;
                }
                if !s.is_empty() {
                    out.push(s);
                }
            }
            start = abs + 1;
        }
    }
    out
}

/// `ui.tsx` 允许的导入说明符判定。
fn is_allowed_specifier(spec: &str) -> bool {
    if spec.starts_with('.') || spec.starts_with('/') {
        return true; // 相对/绝对路径
    }
    if spec.starts_with("http://")
        || spec.starts_with("https://")
        || spec.starts_with("data:")
        || spec.starts_with("blob:")
    {
        return true; // URL
    }
    // react 全家桶由壳的 import map 提供 (external 化)
    if spec == "react"
        || spec == "react-dom"
        || spec == "react-dom/client"
        || spec.starts_with("react/")
        || spec.starts_with("react-dom/")
    {
        return true;
    }
    false
}

/// 校验 `ui.tsx` 的导入白名单 (L0)。
fn check_imports(src: &str, issues: &mut Vec<ValidationIssue>) {
    for spec in extract_import_specifiers(src) {
        if !is_allowed_specifier(&spec) {
            issues.push(issue(
                "error",
                "import",
                format!(
                    "不允许的裸模块导入: \"{spec}\"。自定义应用打包环境无法解析该依赖; \
                     只允许 react / react-dom/client (由壳提供) 或相对路径/URL 导入。\
                     请移除该依赖或改为相对路径引用。"
                ),
            ));
        }
    }
}

/// 用真实打包的同款 esbuild 标志做预打包 (L1)。
fn esbuild_preflight(
    app_dir: &Path,
    file_name: &str,
    bundle_deps: bool,
    issues: &mut Vec<ValidationIssue>,
) {
    let entry = app_dir.join(file_name);
    if !entry.exists() {
        return; // 该文件不存在, 跳过 (例如没有 backend.ts)
    }
    let esbuild = crate::resolve_esbuild_path();
    if !esbuild.exists() {
        issues.push(issue(
            "warning",
            "esbuild",
            format!(
                "esbuild 未找到 ({}), 跳过 {} 预打包校验。请先确保 esbuild 已就绪。",
                esbuild.display(),
                file_name
            ),
        ));
        return;
    }

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let out_path = std::env::temp_dir().join(format!(
        "hermix_validate_{}_{}.js",
        stamp, file_name
    ));

    let mut cmd = std::process::Command::new(&esbuild);
    cmd.arg(&entry)
        .arg("--bundle")
        .arg("--format=esm")
        .arg("--platform=browser")
        .arg("--target=es2020")
        // 关键是 --outfile= 等号形式 (esbuild 0.21+ 拒绝空格形式)
        .arg(format!("--outfile={}", out_path.display()))
        .arg("--log-level=warning")
        .arg("--color=false");
    if bundle_deps {
        cmd.arg("--external:react").arg("--external:react-dom/client");
    }

    match cmd.output() {
        Ok(out) => {
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                issues.push(issue(
                    "error",
                    "esbuild",
                    format!(
                        "{} 预打包失败 (esbuild):\n{}",
                        file_name,
                        stderr.chars().take(1200).collect::<String>()
                    ),
                ));
            }
        }
        Err(e) => {
            issues.push(issue(
                "warning",
                "esbuild",
                format!("无法执行 esbuild 预打包 {}: {e}", file_name),
            ));
        }
    }

    let _ = std::fs::remove_file(&out_path);
}

/// 对单个自定义应用做开发期自验证 (L0 + L1)。
///
/// `app_dir` 为该应用的根目录 (含 `manifest.json` / `ui.tsx` / `backend.ts`)。
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
    for f in ["name", "id", "category"] {
        if manifest.get(f).and_then(|v| v.as_str()).is_none() {
            let level = if f == "category" { "warning" } else { "error" };
            issues.push(issue(
                level,
                "manifest",
                format!("manifest 缺少字段 `{f}` (打包时会使用默认值, 但建议补全)"),
            ));
        }
    }

    // ── L0: ui.tsx 导入白名单 ──
    if let Ok(ui) = std::fs::read_to_string(app_dir.join("ui.tsx")) {
        check_imports(&ui, &mut issues);
    }
    // backend.ts 同样做白名单 (后端也只允许相对/URL 导入, 不应依赖裸 npm 包)
    if let Ok(be) = std::fs::read_to_string(app_dir.join("backend.ts")) {
        check_imports(&be, &mut issues);
    }

    // ── L1: esbuild 预打包 ──
    esbuild_preflight(app_dir, "ui.tsx", true, &mut issues);
    esbuild_preflight(app_dir, "backend.ts", false, &mut issues);

    let ok = !issues.iter().any(|i| i.level == "error");
    ValidationReport { ok, issues }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_app(dir: &Path, manifest: &str, ui: &str, backend: &str) {
        std::fs::write(dir.join("manifest.json"), manifest).unwrap();
        std::fs::write(dir.join("ui.tsx"), ui).unwrap();
        std::fs::write(dir.join("backend.ts"), backend).unwrap();
    }

    fn good_manifest() -> String {
        r#"{"name":"测试","id":"test-app","category":"developer"}"#.to_string()
    }

    #[test]
    fn good_app_passes() {
        let tmp = std::env::temp_dir().join(format!("va_good_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        write_app(
            &tmp,
            &good_manifest(),
            "import { useState } from 'react';\nexport default function A(){return null;}\n",
            "export {};\n",
        );
        let r = validate_custom_app(&tmp);
        assert!(r.ok, "应无 error: {:?}", r.issues);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn bare_npm_import_fails() {
        let tmp = std::env::temp_dir().join(format!("va_bare_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        write_app(
            &tmp,
            &good_manifest(),
            "import _ from 'lodash';\nexport default function A(){return null;}\n",
            "export {};\n",
        );
        let r = validate_custom_app(&tmp);
        assert!(!r.ok);
        assert!(r.issues.iter().any(|i| i.code == "import"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn missing_manifest_fails() {
        let tmp = std::env::temp_dir().join(format!("va_noman_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        // 无 manifest.json
        std::fs::write(tmp.join("ui.tsx"), "export default 1;\n").unwrap();
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
        write_app(
            &tmp,
            "{ this is not json ",
            "export default 1;\n",
            "export {};\n",
        );
        let r = validate_custom_app(&tmp);
        assert!(!r.ok);
        assert!(r.issues.iter().any(|i| i.code == "manifest"));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
