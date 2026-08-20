//! 文档管理模块: 每个自定义应用必须自带 4 个 md 文档作为源码的一部分.
//!
//! 设计原则:
//! - 4 个 md 文件与 index.html / src/ 平级, 落盘到 `<custom-apps>/<id>/docs/`
//! - create 后由 `init_docs` 自动生成模板, 后续由 agent 用工具维护
//! - update / 修 bug 前必须先 `read_doc` + `search_iterations`, 避免重复触发
//! - `更新迭代.md` 强制 4 节结构: 【问题】【根因】【修复】【验证】

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::manifest::PluginManifest;
use crate::PluginError;

/// 自定义应用必备的 4 个 md 文件名 (顺序 = 创建顺序).
pub const DOC_NAMES: [&str; 4] = [
    "设计方案.md",
    "开发计划进度.md",
    "更新迭代.md",
    "README.md",
];

/// 文档标识 (用于工具参数解析).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Doc {
    Design,
    Plan,
    Iterations,
    Readme,
}

impl Doc {
    pub fn filename(&self) -> &'static str {
        match self {
            Doc::Design => "设计方案.md",
            Doc::Plan => "开发计划进度.md",
            Doc::Iterations => "更新迭代.md",
            Doc::Readme => "README.md",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "设计方案" | "design" | "design.md" | "设计方案.md" => Some(Doc::Design),
            "开发计划进度" | "plan" | "plan.md" | "开发计划进度.md" => Some(Doc::Plan),
            "更新迭代" | "iterations" | "iterations.md" | "更新迭代.md" => Some(Doc::Iterations),
            "README" | "readme" | "README.md" => Some(Doc::Readme),
            _ => None,
        }
    }
}

/// 应用文档目录路径: `<app_dir>/docs/`.
pub fn docs_dir(app_dir: &Path) -> PathBuf {
    app_dir.join("docs")
}

/// 创建 docs 目录 (不存在则建), 已存在不报错.
pub fn ensure_docs_dir(app_dir: &Path) -> Result<PathBuf, PluginError> {
    let dir = docs_dir(app_dir);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// 从结构化 proposal 生成 设计方案.md 内容 (markdown).
pub fn render_design_md(name: &str, id: &str, proposal: &Value) -> String {
    let status = proposal.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
    let section = |key: &str| -> String {
        proposal
            .get(key)
            .map(|v| match v {
                Value::String(s) => s.clone(),
                Value::Array(arr) => arr
                    .iter()
                    .map(|x| x.as_str().unwrap_or("").to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
                other => other.to_string(),
            })
            .unwrap_or_else(|| "(待补充)".to_string())
    };

    format!(
        "# {name} 设计方案\n\n\
         > 由 `custom_app_manager` `propose` 生成。\n\
         > 用户确认后状态 = approved 才能 create。\n\n\
         ## 1. 名称\n\
         - 应用名称: {name}\n\
         - 应用 ID: {id}\n\
         - 分类: {}\n\
         - 图标: {}\n\
         - 状态: {status}\n\n\
         ## 2. 功能\n\
         {}\n\n\
         ## 3. 界面布局\n\
         {}\n\n\
         ## 4. 交互效果\n\
         {}\n\n\
         ## 5. 自定义应用专属 (强制拍板)\n\
         {}\n\n\
         ## 6. 复用来源\n\
         {}\n\n\
         ---\n\
         更新日志: 本文件由 propose 初始化；create 后可通过 `custom_app_manager update_doc` action 修改。\n",
        proposal.get("category").and_then(|v| v.as_str()).unwrap_or("other"),
        proposal.get("icon").and_then(|v| v.as_str()).unwrap_or("(未指定)"),
        section("功能"),
        section("界面布局"),
        section("交互效果"),
        section("自定义应用专属"),
        section("复用来源"),
    )
}

/// 开发计划进度.md 模板.
pub fn render_plan_md(name: &str) -> String {
    format!(
        "# {name} 开发计划进度\n\n\
         > checkbox 维护: agent 每次 `update` 可通过 `plan_note` 自动勾选/勾掉。\n\n\
         ## 总览\n\
         - 状态: 已 create, 待编码\n\
         - 阶段: ① 设计闸门已过 → ② 编码 → ③ 验证 → ④ 打包\n\n\
         ## 阶段任务\n\n\
         ### 阶段 0 — 设计闸门\n\
         - [x] propose 输出设计方案\n\
         - [x] 用户确认 (approve)\n\n\
         ### 阶段 1 — 编码\n\
         - [ ] 写 manifest.json (含 permissions / window_config)\n\
         - [ ] 写 index.html / app_source\n\
         - [ ] 配套 source_files (若 TS/React 模式)\n\
         - [ ] 自实现标题栏 (V7 壳无边框)\n\n\
         ### 阶段 2 — 验证\n\
         - [ ] validate (L0 manifest / L1 HTML / L2 权限 / L3 前端质量 / L4 文档一致性)\n\
         - [ ] 修复 validate 报告直到 ok=true\n\
         - [ ] run 测试版预览\n\
         - [ ] (如需) smoke 运行时验证\n\n\
         ### 阶段 3 — 打包分发\n\
         - [ ] package (sidecar)\n\
         - [ ] README.md 写完 (用户视角)\n\
         - [ ] 交付说明\n\n\
         ## 下一步行动\n\
         (agent 在每次 update 后追加具体下一动作)\n"
    )
}

/// 更新迭代.md 模板 (append-only, 初始空).
pub fn render_iterations_md(name: &str) -> String {
    format!(
        "# {name} 更新迭代日志\n\n\
         > append-only, 每次 `update` 或 `log_iteration` 追加一条。\n\
         > 修任何 bug 前先 grep 本文件, 避免重复触发。\n\
         > 每条强制四节: 【问题】【根因】【修复】【验证】, 缺一不可。\n\n\
         ---\n\n\
         <!-- 每次 append 在此分割线下追加, 由 log_iteration 自动写入 -->\n"
    )
}

/// README.md 模板 (用户视角).
pub fn render_readme_md(manifest: &PluginManifest) -> String {
    let permissions = if manifest.permissions.is_empty() {
        "(未声明)".to_string()
    } else {
        manifest.permissions.join(", ")
    };

    format!(
        "# {name}\n\n\
         > {description}\n\n\
         ## 功能\n\
         (agent 在 create / update 后通过 `custom_app_manager update_doc` 补全)\n\n\
         ## 使用方法\n\
         1. 在 Hermix Desktop 「自定义应用」卡片点 「运行」(或双击)\n\
         2. 测试版: hx-shell --html 直接加载, 不生成单 exe\n\
         3. 打包版: 走 `custom_app_manager package` 出的 `dist/<id>.exe`\n\n\
         ## 更新方法\n\
         1. 在 Hermix Desktop 对话框说 「更新 {name} 应用」 → agent 调 `custom_app_manager update`\n\
         2. agent 会先读 `docs/` 4 个 md, 再动手\n\
         3. 修 bug 时会先 grep `docs/更新迭代.md` 查历史, 避免重复触发\n\
         4. 每次 update 自动 log_iteration (【问题/根因/修复/验证】)\n\n\
         ## 权限\n\
         {permissions}\n\n\
         ## 分发方法\n\
         ```bash\n\
         # 通过 custom_app_manager 打包 (sidecar 模式, 单文件 exe)\n\
         custom_app_manager action=package id={id}\n\
         # 产出: <custom-apps>/{id}/dist/{id}.exe\n\
         ```\n\n\
         ## 文件结构\n\
         ```\n\
         {id}/\n\
         ├── manifest.json       # 应用清单\n\
         ├── index.html          # 纯 HTML 模式入口\n\
         │   (或)\n\
         ├── src/main.tsx        # TS/React 模式入口\n\
         │   src/components/...  # 额外组件\n\
         ├── docs/\n\
         │   ├── 设计方案.md       # propose 生成的方案\n\
         │   ├── 开发计划进度.md   # agent 维护的 checkbox\n\
         │   ├── 更新迭代.md       # append-only 历史\n\
         │   └── README.md         # 本文件\n\
         └── dist/\n\
             ├── {id}.dev.html    # run 测试版产物\n\
             └── {id}.exe          # package 产物 (单文件)\n\
         ```\n\n\
         ## 故障排查\n\
         1. 看 `docs/更新迭代.md` 历史\n\
         2. 跑 `custom_app_manager action=validate id={id}` 静态校验\n\
         3. 跑 `custom_app_manager action=smoke id={id}` 运行时验证\n",
        name = manifest.name,
        description = manifest.description.as_str(),
        id = manifest.id,
        permissions = permissions,
    )
}

/// 初始化 4 个 md 模板 (已存在不覆盖). 在 create 后调用.
pub fn init_docs(app_dir: &Path, proposal: &Value, manifest: &PluginManifest) -> Result<Vec<PathBuf>, PluginError> {
    let dir = ensure_docs_dir(app_dir)?;
    let mut paths = Vec::with_capacity(DOC_NAMES.len());

    let write_if_absent = |file: &str, content: &str| -> Result<PathBuf, PluginError> {
        let p = dir.join(file);
        if !p.exists() {
            fs::write(&p, content)?;
        }
        Ok(p)
    };

    paths.push(write_if_absent(
        Doc::Design.filename(),
        &render_design_md(&manifest.name, &manifest.id, proposal),
    )?);
    paths.push(write_if_absent(Doc::Plan.filename(), &render_plan_md(&manifest.name))?);
    paths.push(write_if_absent(Doc::Iterations.filename(), &render_iterations_md(&manifest.name))?);
    paths.push(write_if_absent(Doc::Readme.filename(), &render_readme_md(manifest))?);

    Ok(paths)
}

/// 读取单个 md 全文.
pub fn read_doc(app_dir: &Path, doc: Doc) -> Result<String, PluginError> {
    let path = docs_dir(app_dir).join(doc.filename());
    if !path.exists() {
        return Err(PluginError::NotFound(format!(
            "文档不存在: {} (可用: {})",
            doc.filename(),
            DOC_NAMES.join(", ")
        )));
    }
    Ok(fs::read_to_string(&path)?)
}

/// 覆盖写入单个 md. 不存在则创建 docs/ 目录.
pub fn update_doc(app_dir: &Path, doc: Doc, content: &str) -> Result<PathBuf, PluginError> {
    let dir = ensure_docs_dir(app_dir)?;
    let path = dir.join(doc.filename());
    fs::write(&path, content)?;
    Ok(path)
}

/// 追加一条迭代记录到 更新迭代.md.
///
/// `iteration` JSON 必含 `问题`/`根因`/`修复`/`验证` 四节, 否则报错.
pub fn append_iteration(app_dir: &Path, iteration: &Value) -> Result<PathBuf, PluginError> {
    let obj = iteration.as_object().ok_or_else(|| {
        PluginError::InvalidManifest(
            "iteration 必须是 JSON 对象 (含【问题】【根因】【修复】【验证】四节)".into(),
        )
    })?;

    let required = ["问题", "根因", "修复", "验证"];
    for key in required {
        if obj.get(key).and_then(|v| v.as_str()).map(str::trim).unwrap_or("").is_empty() {
            return Err(PluginError::InvalidManifest(format!(
                "iteration 缺少必填节: {key} (需含【问题】【根因】【修复】【验证】)"
            )));
        }
    }

    let now = chrono_like_now();
    let block = format!(
        "\n---\n\n## {now}\n\n\
         **问题**: {}\n\n\
         **根因**: {}\n\n\
         **修复**: {}\n\n\
         **验证**: {}\n",
        obj.get("问题").and_then(|v| v.as_str()).unwrap_or(""),
        obj.get("根因").and_then(|v| v.as_str()).unwrap_or(""),
        obj.get("修复").and_then(|v| v.as_str()).unwrap_or(""),
        obj.get("验证").and_then(|v| v.as_str()).unwrap_or(""),
    );

    let path = docs_dir(app_dir).join(Doc::Iterations.filename());
    ensure_docs_dir(app_dir)?;
    let mut existing = if path.exists() {
        fs::read_to_string(&path)?
    } else {
        render_iterations_md(&obj.get("name").and_then(|v| v.as_str()).unwrap_or("应用").to_string())
    };
    existing.push_str(&block);
    fs::write(&path, existing)?;
    Ok(path)
}

/// 在 更新迭代.md 中按关键词搜索历史记录 (大小写不敏感, 命中任一节即可).
///
/// 返回命中的条目 (含时间戳 + 4 节).
pub fn search_iterations(app_dir: &Path, query: &str) -> Result<Vec<Value>, PluginError> {
    let path = docs_dir(app_dir).join(Doc::Iterations.filename());
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(&path)?;
    let query_lc = query.to_lowercase();

    let mut hits = Vec::new();
    for chunk in content.split("\n---\n") {
        let chunk_lc = chunk.to_lowercase();
        if chunk_lc.contains(&query_lc) {
            // 解析每节的 4 行内容
            let mut entry = serde_json::Map::new();
            for key in ["问题", "根因", "修复", "验证"] {
                if let Some(start) = chunk.find(&format!("**{key}**")) {
                    let after = &chunk[start..];
                    if let Some(colon) = after.find(':') {
                        let body = after[colon + 1..].trim();
                        // 取到下一个 ** 为止 (如有)
                        let end = body.find("\n**").unwrap_or(body.len());
                        entry.insert(key.into(), json!(body[..end].trim()));
                    }
                }
            }
            // 第一行通常是时间戳 "## YYYY-MM-DD HH:MM:SS"
            if let Some(h2_start) = chunk.find("## ") {
                let after = &chunk[h2_start + 3..];
                let end = after.find('\n').unwrap_or(after.len());
                entry.insert("时间".into(), json!(after[..end].trim()));
            }
            if !entry.is_empty() {
                hits.push(Value::Object(entry));
            }
        }
    }
    Ok(hits)
}

/// 文档一致性检查: 4 个 md 是否齐全 + 更新迭代每条是否含 4 节.
pub fn check_docs_consistency(app_dir: &Path) -> DocsReport {
    let mut report = DocsReport::default();

    for name in DOC_NAMES {
        let p = docs_dir(app_dir).join(name);
        if p.exists() {
            report.present.push(name.to_string());
        } else {
            report.missing.push(name.to_string());
        }
    }

    if !report.missing.is_empty() {
        report.issues.push(format!("缺少文档: {}", report.missing.join(", ")));
    }

    // 检查 更新迭代.md 每条是否含 4 节 (跳过仅有模板注释的块)
    let iter_path = docs_dir(app_dir).join(Doc::Iterations.filename());
    if iter_path.exists() {
        if let Ok(content) = fs::read_to_string(&iter_path) {
            for chunk in content.split("\n---\n").skip(1) {
                if chunk.trim().is_empty() {
                    continue;
                }
                // 没时间戳标题 (## ...) 的块视为模板注释, 不检查
                if !chunk.contains("## ") {
                    continue;
                }
                for key in ["**问题**", "**根因**", "**修复**", "**验证**"] {
                    if !chunk.contains(key) {
                        report.issues.push(format!(
                            "更新迭代.md 缺 {key} 节 (请用 log_iteration 追加标准格式)"
                        ));
                    }
                }
            }
        }
    }

    report.ok = report.missing.is_empty() && report.issues.is_empty();
    report
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct DocsReport {
    pub ok: bool,
    pub present: Vec<String>,
    pub missing: Vec<String>,
    pub issues: Vec<String>,
}

/// 不引入 chrono 依赖, 轻量 RFC3339 时间戳.
fn chrono_like_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}") // 秒级时间戳, agent 用时自己解析; 不阻塞主流程
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_tmp(suffix: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("docs_{}_{}", std::process::id(), suffix))
    }

    fn make_manifest() -> PluginManifest {
        PluginManifest {
            id: "test-app".into(),
            name: "测试应用".into(),
            version: "1.0.0".into(),
            description: "测试".into(),
            long_description: None,
            author: "".into(),
            icon: "".into(),
            icon_file: None,
            tile_color: None,
            category: "other".into(),
            tags: vec![],
            permissions: vec!["fs-read".into(), "fs-write".into()],
            window_config: Default::default(),
            dependencies: Default::default(),
        }
    }

    #[test]
    fn init_docs_creates_all_four() {
        let tmp = make_tmp("x");
        let proposal = json!({"功能": "测试功能", "界面布局": "单页", "交互效果": "点击"});
        let m = make_manifest();
        let paths = init_docs(tmp.as_path(), &proposal, &m).unwrap();
        assert_eq!(paths.len(), 4);
        for p in &paths {
            assert!(p.exists());
        }
    }

    #[test]
    fn init_docs_does_not_overwrite_existing() {
        let tmp = make_tmp("x");
        let proposal = json!({"功能": "测试"});
        let m = make_manifest();
        init_docs(tmp.as_path(), &proposal, &m).unwrap();

        // 改 README 后再 init 不应被覆盖
        let readme = docs_dir(tmp.as_path()).join("README.md");
        std::fs::write(&readme, "CUSTOM").unwrap();
        init_docs(tmp.as_path(), &proposal, &m).unwrap();
        assert_eq!(std::fs::read_to_string(&readme).unwrap(), "CUSTOM");
    }

    #[test]
    fn append_iteration_requires_four_sections() {
        let tmp = make_tmp("x");
        let m = make_manifest();
        init_docs(tmp.as_path(), &json!({}), &m).unwrap();

        // 缺 验证 节 → 报错
        let bad = json!({"问题": "x", "根因": "y", "修复": "z"});
        assert!(append_iteration(tmp.as_path(), &bad).is_err());

        // 完整 4 节 → 成功
        let good = json!({"问题": "x", "根因": "y", "修复": "z", "验证": "ok"});
        assert!(append_iteration(tmp.as_path(), &good).is_ok());
    }

    #[test]
    fn search_iterations_finds_keywords() {
        let tmp = make_tmp("x");
        let m = make_manifest();
        init_docs(tmp.as_path(), &json!({}), &m).unwrap();

        append_iteration(
            tmp.as_path(),
            &json!({"问题": "按钮失效", "根因": "事件没绑", "修复": "加 onClick", "验证": "可点"}),
        )
        .unwrap();

        let hits = search_iterations(tmp.as_path(), "按钮").unwrap();
        assert!(!hits.is_empty(), "应能搜到「按钮」");
    }

    #[test]
    fn check_consistency_reports_missing() {
        let tmp = make_tmp("x");
        let report = check_docs_consistency(tmp.as_path());
        assert!(!report.ok);
        assert_eq!(report.missing.len(), 4);
    }

    #[test]
    fn check_consistency_passes_after_init() {
        let tmp = make_tmp("x");
        let m = make_manifest();
        init_docs(tmp.as_path(), &json!({}), &m).unwrap();
        let report = check_docs_consistency(tmp.as_path());
        assert!(report.ok, "应通过, issues={:?}", report.issues);
    }
}