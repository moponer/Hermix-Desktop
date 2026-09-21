//! 工作台代码 lint 引擎。
//!
//! 规则全部来自 `workbench_rules.json` —— 同一份源同时生成 SKILL.md（让 AI 一次写对）
//! 与本引擎（落盘前拦截），杜绝「文档说 200 行、lint 查 300 行」的漂移。
//!
//! 违规必须带「文件:行号 + 规则 ID + 修法」返回，否则 agent 无从自修，只会陷入重写死循环。

use std::collections::HashMap;

use regex_lite::Regex;
use serde::Deserialize;

const RULES_JSON: &str = include_str!("workbench_rules.json");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuleSet {
    #[allow(dead_code)]
    version: u32,
    limits: Limits,
    #[allow(dead_code)]
    locked_files: Vec<String>,
    rules: Vec<Rule>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Limits {
    js: usize,
    css: usize,
    html: usize,
    duplicate_block_lines: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Rule {
    id: String,
    #[allow(dead_code)]
    title: String,
    applies: Vec<String>,
    pattern: String,
    #[serde(default)]
    exclude_pattern: Option<String>,
    /// 该规则只在「非指定文件」上生效（如 W009：只有非 api.js 才禁用 wb.*）。
    #[serde(default)]
    only_outside: Option<String>,
    /// 白名单文件：规则在这些文件上不生效（如 tokens.css 必须能写裸色值）。
    #[serde(default)]
    except_files: Vec<String>,
    /// 整文件匹配而非逐行。
    #[serde(default)]
    whole_file: bool,
    /// 反转判定：整文件「未命中」才算违规（如 W011 缺 import/export）。
    #[serde(default)]
    invert: bool,
    /// 整文件「必须命中」（如 W022 必须引用宿主 SDK）。
    #[serde(default)]
    must_match: bool,
    /// 命中 pattern 时，整文件还必须命中本模式 —— 用于「成对出现」的规则
    /// （如 W010：绑了 addEventListener 就必须有 removeEventListener）。
    #[serde(default)]
    requires_pattern: Option<String>,
    message: String,
    fix: String,
}

#[derive(Debug, Clone)]
pub struct Violation {
    pub file: String,
    pub line: usize,
    pub rule: String,
    pub message: String,
    pub fix: String,
}

impl Violation {
    /// 回传给 agent 的格式：可定位、可自修，不含模糊描述。
    pub fn format(&self) -> String {
        format!(
            "{}:{}  {}  {}\n  → {}",
            self.file, self.line, self.rule, self.message, self.fix
        )
    }
}

fn rule_set() -> Result<RuleSet, String> {
    serde_json::from_str(RULES_JSON).map_err(|e| format!("parse workbench_rules.json: {}", e))
}

fn ext_of(rel: &str) -> &str {
    match rel.rsplit('.').next() {
        Some(e) if rel.contains('.') => e,
        _ => "",
    }
}

/// 对一批文件做全量校验。`pending` 用于「落盘前」：用待写入内容替换同名文件后检查，
/// 使跨文件规则（W009 / W013）在写入前就能拦住。
pub fn lint_files(files: &[(String, String)], pending: Option<(&str, &str)>) -> Vec<Violation> {
    let set = match rule_set() {
        Ok(s) => s,
        Err(e) => {
            return vec![Violation {
                file: "-".to_string(),
                line: 0,
                rule: "W000".to_string(),
                message: e,
                fix: "检查 workbench_rules.json 结构".to_string(),
            }]
        }
    };

    let mut merged: Vec<(String, String)> = files.to_vec();
    if let Some((rel, content)) = pending {
        match merged.iter_mut().find(|(r, _)| r == rel) {
            Some(slot) => slot.1 = content.to_string(),
            None => merged.push((rel.to_string(), content.to_string())),
        }
    }

    let mut out = Vec::new();
    for (rel, content) in &merged {
        let ext = ext_of(rel);
        let limit = match ext {
            "css" => set.limits.css,
            "html" => set.limits.html,
            _ => set.limits.js,
        };
        let lines = content.lines().count();
        if lines > limit {
            out.push(Violation {
                file: rel.clone(),
                line: limit + 1,
                rule: "W004".to_string(),
                message: format!("{} 行超出单文件上限 {}", lines, limit),
                fix: "拆成多个模块，每个模块职责单一".to_string(),
            });
        }

        for rule in &set.rules {
            if !rule.applies.iter().any(|a| a == ext) {
                continue;
            }
            if let Some(f) = &rule.only_outside {
                if rel == f {
                    continue;
                }
            }
            if rule.except_files.iter().any(|f| f == rel) {
                continue;
            }
            let re = match Regex::new(&rule.pattern) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let exclude = rule
                .exclude_pattern
                .as_ref()
                .and_then(|p| Regex::new(p).ok());
            // 成对规则：整文件缺另一半才算违规，缺的那半由首处命中行定位。
            let missing_pair = match &rule.requires_pattern {
                Some(p) => match Regex::new(p) {
                    Ok(r) => !r.is_match(content),
                    Err(_) => false,
                },
                None => false,
            };

            if rule.whole_file && rule.requires_pattern.is_none() {
                let hit = re.is_match(content);
                // invert 与 mustMatch 语义一致：整文件未命中即违规。
                if (rule.invert || rule.must_match) && !hit {
                    out.push(violation_at(rel, 1, rule));
                }
                continue;
            }

            for (idx, line) in content.lines().enumerate() {
                if !re.is_match(line) {
                    continue;
                }
                if let Some(ex) = &exclude {
                    if ex.is_match(line) {
                        continue;
                    }
                }
                // 成对规则：缺另一半才违规，且只报首处，避免同一根因刷一屏。
                if rule.requires_pattern.is_some() {
                    if missing_pair {
                        out.push(violation_at(rel, idx + 1, rule));
                    }
                    break;
                }
                out.push(violation_at(rel, idx + 1, rule));
            }
        }
    }

    out.extend(detect_duplicates(
        &merged,
        set.limits.duplicate_block_lines,
    ));
    out
}

fn violation_at(rel: &str, line: usize, rule: &Rule) -> Violation {
    Violation {
        file: rel.to_string(),
        line,
        rule: rule.id.clone(),
        message: rule.message.clone(),
        fix: rule.fix.clone(),
    }
}

/// 跨文件重复块检测：同一段连续行出现在两个文件里，说明该抽公共模块了。
fn detect_duplicates(files: &[(String, String)], block: usize) -> Vec<Violation> {
    if block == 0 {
        return Vec::new();
    }
    let mut seen: HashMap<String, (String, usize)> = HashMap::new();
    let mut out = Vec::new();
    for (rel, content) in files {
        let lines: Vec<&str> = content
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect();
        if lines.len() < block {
            continue;
        }
        for start in 0..=(lines.len() - block) {
            let key = lines[start..start + block].join("\n");
            match seen.get(&key) {
                Some((first_file, first_line)) if first_file != rel => {
                    out.push(Violation {
                        file: rel.clone(),
                        line: start + 1,
                        rule: "W013".to_string(),
                        message: format!(
                            "与 {} 存在 {} 行重复代码",
                            first_file, block
                        ),
                        fix: "抽到 src/components 或 src/utils 作为公共模块，两边 import".to_string(),
                    });
                }
                _ => {
                    seen.insert(key, (rel.clone(), start + 1));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK_JS: &str = "import { a } from './b.js';\nexport function f() {\n  return a;\n}\n";

    #[test]
    fn clean_module_passes() {
        let files = vec![("src/views/a.js".to_string(), OK_JS.to_string())];
        let v = lint_files(&files, None);
        assert!(v.is_empty(), "{:?}", v.iter().map(|x| x.format()).collect::<Vec<_>>());
    }

    #[test]
    fn inline_style_is_rejected() {
        let files = vec![(
            "src/views/a.js".to_string(),
            "export const t = `<div style=\"color:#333\"></div>`;\n".to_string(),
        )];
        let v = lint_files(&files, None);
        assert!(v.iter().any(|x| x.rule == "W001"), "{:?}", v);
    }

    #[test]
    fn wb_access_outside_api_is_rejected() {
        let files = vec![(
            "src/views/a.js".to_string(),
            "export async function go() {\n  return window.wb.db.query('x');\n}\n".to_string(),
        )];
        let v = lint_files(&files, None);
        assert!(v.iter().any(|x| x.rule == "W009"), "{:?}", v);
    }

    #[test]
    fn wb_access_inside_api_is_allowed() {
        let files = vec![(
            "src/api.js".to_string(),
            "export async function go() {\n  return window.wb.db.query('x');\n}\n".to_string(),
        )];
        let v = lint_files(&files, None);
        assert!(!v.iter().any(|x| x.rule == "W009"), "{:?}", v);
    }

    #[test]
    fn tokens_css_may_contain_raw_colors() {
        let files = vec![(
            "styles/tokens.css".to_string(),
            ":root {\n  --wb-bg: #1c1c20;\n}\n".to_string(),
        )];
        let v = lint_files(&files, None);
        assert!(!v.iter().any(|x| x.rule == "W004a"), "{:?}", v);
    }

    #[test]
    fn other_css_raw_color_is_rejected() {
        let files = vec![(
            "styles/components.css".to_string(),
            ".wb-card {\n  color: #333333;\n}\n".to_string(),
        )];
        let v = lint_files(&files, None);
        assert!(v.iter().any(|x| x.rule == "W004a"), "{:?}", v);
    }

    #[test]
    fn missing_esm_marker_is_rejected() {
        let files = vec![("src/utils/x.js".to_string(), "function helper() {\n  return 1;\n}\n".to_string())];
        let v = lint_files(&files, None);
        assert!(v.iter().any(|x| x.rule == "W011"), "{:?}", v);
    }

    #[test]
    fn cdn_import_is_rejected() {
        let files = vec![(
            "src/views/a.js".to_string(),
            "import { Grid } from 'https://cdn.example.com/grid.js';\nexport const x = Grid;\n".to_string(),
        )];
        let v = lint_files(&files, None);
        assert!(v.iter().any(|x| x.rule == "W019"), "{:?}", v);
    }

    /// 成对规则：绑了事件却没解绑 —— 反复挂载卸载会累积监听器。
    #[test]
    fn listener_without_unbind_is_rejected() {
        let files = vec![(
            "src/views/a.js".to_string(),
            "export function mount(root) {\n  root.addEventListener('click', () => {});\n}\n".to_string(),
        )];
        let v = lint_files(&files, None);
        let hit = v.iter().find(|x| x.rule == "W015").expect("W015");
        assert_eq!(hit.line, 2, "{:?}", hit);
    }

    #[test]
    fn paired_listener_passes() {
        let src = "export function mount(root) {\n  root.addEventListener('click', onClick);\n}\nexport function unmount() {\n  document.removeEventListener('click', onClick);\n}\n";
        let files = vec![("src/views/a.js".to_string(), src.to_string())];
        let v = lint_files(&files, None);
        assert!(!v.iter().any(|x| x.rule == "W015"), "{:?}", v);
    }

    /// 代码层不得绕过宿主直连网络与本地存储 —— 否则权限关口形同虚设。
    #[test]
    fn host_channel_bypass_is_rejected() {
        for (src, rule) in [
            ("export const g = () => fetch('https://x');\n", "W024"),
            ("export const s = () => localStorage.getItem('k');\n", "W025"),
            ("export const e = () => eval('1+1');\n", "W023"),
        ] {
            let files = vec![("src/views/a.js".to_string(), src.to_string())];
            let v = lint_files(&files, None);
            assert!(v.iter().any(|x| x.rule == rule), "{} {:?}", rule, v);
        }
    }

    #[test]
    fn pending_content_is_checked_before_write() {
        let files = vec![("src/views/a.js".to_string(), OK_JS.to_string())];
        let bad = "export const t = `<div style=\"\"></div>`;\n";
        let v = lint_files(&files, Some(("src/views/a.js", bad)));
        assert!(v.iter().any(|x| x.rule == "W001"), "{:?}", v);
    }

    #[test]
    fn duplicate_blocks_are_rejected() {
        let block: String = (0..15).map(|i| format!("  const v{} = {};\n", i, i)).collect();
        let files = vec![
            ("src/views/a.js".to_string(), format!("export function a() {{\n{}}}\n", block)),
            ("src/views/b.js".to_string(), format!("export function b() {{\n{}}}\n", block)),
        ];
        let v = lint_files(&files, None);
        assert!(v.iter().any(|x| x.rule == "W013"), "{:?}", v);
    }

    #[test]
    fn violation_format_is_actionable() {
        let v = Violation {
            file: "src/views/overview.js".to_string(),
            line: 47,
            rule: "W001".to_string(),
            message: "禁止内联样式 style= 属性".to_string(),
            fix: "移到 styles/components.css".to_string(),
        };
        assert_eq!(
            v.format(),
            "src/views/overview.js:47  W001  禁止内联样式 style= 属性\n  → 移到 styles/components.css"
        );
    }
}
