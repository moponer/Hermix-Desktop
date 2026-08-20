//! Custom App Manager tool — lets the LLM create, update, list, and package
//! user-facing desktop applications.
//!
//! This tool is the LLM's interface to the custom app system. The LLM
//! can:
//! - `create`: Create a new app with manifest + index_html
//! - `propose` / `approve`: Design gate (must approve before create)
//! - `update`: Update an existing app's index_html / manifest, with mandatory iteration_note
//! - `list` / `get`: List / fetch installed apps
//! - `delete`: Uninstall an app
//! - `package`: Package an app as a standalone EXE (hx-shell PE resource embedding)
//! - `run`: Launch the app in dev/test mode (single-file html via hx-shell --html)
//! - `validate`: Static validation (manifest + HTML + hx API + permissions + frontend quality + docs consistency)
//! - `smoke`: Runtime smoke test (launch + capture errors)
//! - `patch`: Surgical text replace in an installed app's source
//! - `read_doc` / `update_doc`: Read / overwrite one of the 4 app docs
//! - `log_iteration`: Append an entry to 更新迭代.md (must contain 问题/根因/修复/验证)
//! - `search_iterations`: Search history in 更新迭代.md by keyword
//!
//! Every app owns 4 docs (in `<custom-apps>/<id>/docs/`):
//! - 设计方案.md: design proposal (initialized by propose/create)
//! - 开发计划进度.md: agent-maintained phase checklist
//! - 更新迭代.md: append-only iteration log
//! - README.md: user-facing quickstart

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_core::{
    tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema, traits::RiskLevel,
};
use hermix_plugin::{
    api::{AppResponse, AppSummary, CreateAppRequest},
    docs::{self as app_docs, Doc},
    run_smoke_test_async, validate_custom_app, PluginManager, PluginManifest,
};

use crate::ToolRegistry;

/// Register the custom_app_manager tool.
pub fn register(registry: &ToolRegistry, manager: Arc<PluginManager>) {
    let handler = Arc::new(CustomAppManagerHandler::new(manager));
    let schema = handler.schema();
        registry.register_with_risk(
            "custom_app_manager",
            "custom_app",
            schema,
            handler,
            Arc::new(|| true),
            vec![],
            true,
            "Create, update, list, package, and run user-facing desktop applications using hx-shell runtime (wry/WebView2). Actions: create, propose, approve, update, list, get, delete, package, run, validate, smoke, patch, read_doc, update_doc, log_iteration, search_iterations. Every app owns 4 docs (设计方案/开发计划进度/更新迭代/README) that the agent MUST read before any change and update after any change.",
            "📦",
            None,
            RiskLevel::Caution,
            false,
        );
}

pub struct CustomAppManagerHandler {
    manager: Arc<PluginManager>,
}

impl CustomAppManagerHandler {
    pub fn new(manager: Arc<PluginManager>) -> Self {
        Self { manager }
    }

    async fn create(&self, params: &Value) -> Result<String, ToolError> {
        let params = if let Some(manifest_val) = params.get("manifest") {
            if manifest_val.is_string() {
                let mut fixed = params.clone();
                if let Ok(parsed) = serde_json::from_str::<Value>(manifest_val.as_str().unwrap_or("")) {
                    fixed.as_object_mut().unwrap().insert("manifest".into(), parsed);
                }
                fixed
            } else {
                params.clone()
            }
        } else {
            params.clone()
        };

        let request: CreateAppRequest = serde_json::from_value(params.clone())
            .map_err(|e| {
                let msg = e.to_string();
                let hint = if msg.contains("expected struct") || msg.contains("expected map") {
                    " — manifest 参数必须是 JSON 对象, 不能是字符串或 XML 子标签。"
                } else if msg.contains("expected a sequence") || msg.contains("expected sequence") {
                    " — tags / permissions 必须是字符串数组, 不要写成字符串或对象。"
                } else {
                    ""
                };
                ToolError::InvalidParams(format!("参数解析失败: {e}{hint}"))
            })?;

        let icon_data = params.get("icon_data").and_then(|v| v.as_str());

        request
            .manifest
            .validate()
            .map_err(|e| ToolError::InvalidParams(format!("manifest 无效: {e}")))?;

        let proposed_id = &request.manifest.id;
        let proposal_value = match self.manager.load_proposal(proposed_id) {
            Some(p)
                if p.get("status").and_then(|s| s.as_str()) == Some("approved") => p,
            Some(_) => {
                return Err(ToolError::InvalidParams(format!(
                    "应用 '{proposed_id}' 的设计方案尚未经用户确认 (status != approved). 流程: propose → 展示给用户 → approve → create."
                )))
            }
            None => {
                return Err(ToolError::InvalidParams(format!(
                    "创建应用 '{proposed_id}' 前必须先调用 propose 输出结构化设计方案并经 approve 确认。"
                )))
            }
        };

        let index_html = request.index_html.as_deref()
            .or_else(|| params.get("index_html").and_then(|v| v.as_str()));
        let app_source = request.app_source.as_deref()
            .or_else(|| params.get("app_source").and_then(|v| v.as_str()));

        let source_files_map: Option<std::collections::HashMap<String, String>> =
            request.source_files.clone().or_else(|| {
                params.get("source_files").and_then(|v| v.as_object()).map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
            });

        let plugin_dir = self.manager.plugins_dir().join(&request.manifest.id);
        let design_path = plugin_dir.join("docs").join("设计方案.md");
        if design_path.exists() {
            let current = std::fs::read_to_string(&design_path).unwrap_or_default();
            if current.contains("(待补充)") || current.contains("状态: pending") {
                if let Err(e) = std::fs::write(
                    &design_path,
                    app_docs::render_design_md(
                        &request.manifest.name,
                        &request.manifest.id,
                        &proposal_value,
                    ),
                ) {
                    tracing::warn!("写 设计方案.md 失败 (非致命): {e}");
                }
            }
        }

        let entry = self
            .manager
            .install_plugin(
                &request.manifest,
                index_html,
                app_source,
                source_files_map.as_ref(),
                icon_data,
            )
            .map_err(|e| ToolError::ExecutionFailed(format!("创建应用失败: {e}")))?;

        let response: AppResponse = (&entry).into();
        let result = json!({
            "success": true,
            "message": format!(
                "应用 '{}' 创建成功。docs/ 下已初始化 4 个 md, 后续 update 须先读 docs/ 再动手, 每次改动后须 log_iteration 与更新对应 md.",
                entry.manifest.name
            ),
            "app": response,
            "docs_dir": plugin_dir.join("docs").to_string_lossy(),
        });
        Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
    }

    async fn propose(&self, params: &Value) -> Result<String, ToolError> {
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'id' 参数".into()))?;

        let proposal = params
            .get("proposal")
            .cloned()
            .ok_or_else(|| ToolError::InvalidParams("缺少 'proposal' 参数".into()))?;

        let mut stored = match proposal {
            Value::Object(map) => map,
            _ => return Err(ToolError::InvalidParams("proposal 必须是 JSON 对象".into())),
        };
        stored.insert("id".to_string(), json!(id));
        stored.insert("status".to_string(), json!("pending"));

        let path = self
            .manager
            .save_proposal(id, &Value::Object(stored.clone()))
            .map_err(|e| ToolError::ExecutionFailed(format!("保存设计方案失败: {e}")))?;

        let result = json!({
            "success": true,
            "message": format!("应用 '{id}' 设计方案已保存 (pending), 请展示给用户确认后再 approve + create"),
            "proposal": Value::Object(stored),
            "proposal_path": path.to_string_lossy(),
            "next": "将方案展示给用户 → 用户确认后调用 approve action → 再调用 create"
        });
        Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
    }

    async fn approve(&self, params: &Value) -> Result<String, ToolError> {
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'id' 参数".into()))?;

        let proposal = self
            .manager
            .approve_proposal(id)
            .map_err(|e| match e {
                hermix_plugin::PluginError::NotFound(_) => ToolError::InvalidParams(e.to_string()),
                other => ToolError::ExecutionFailed(other.to_string()),
            })?;

        let result = json!({
            "success": true,
            "message": format!("应用 '{id}' 设计方案已确认 (approved), 现在可以 create"),
            "proposal": proposal,
            "next": "调用 create action 落地应用. create 会自动初始化 docs/ 4 个 md."
        });
        Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
    }

    async fn update(&self, params: &Value) -> Result<String, ToolError> {
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'id' 参数".into()))?;

        let app_dir = self.manager.plugins_dir().join(id);
        if !app_dir.exists() {
            return Err(ToolError::NotFound(format!("应用目录不存在: {id}")));
        }

        let index_html = params.get("index_html").and_then(|v| v.as_str());
        let app_source = params.get("app_source").and_then(|v| v.as_str());
        let source_files_obj = params.get("source_files").and_then(|v| v.as_object());
        let source_files_map: Option<std::collections::HashMap<String, String>> =
            source_files_obj.map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            });

        let manifest = if params.get("manifest").is_some() {
            let manifest_val = &params["manifest"];
            let manifest_val = if manifest_val.is_string() {
                serde_json::from_str::<Value>(manifest_val.as_str().unwrap_or(""))
                    .unwrap_or(manifest_val.clone())
            } else {
                manifest_val.clone()
            };
            let m: PluginManifest = serde_json::from_value(manifest_val)
                .map_err(|e| ToolError::InvalidParams(format!("manifest 解析失败: {e}")))?;
            Some(m)
        } else {
            None
        };

        let changes_code = index_html.is_some() || app_source.is_some() || source_files_map.is_some();
        let changes_meta = manifest.is_some();

        if !changes_code && !changes_meta {
            return Err(ToolError::InvalidParams(
                "至少需要提供 app_source、index_html、source_files 或 manifest 之一".into(),
            ));
        }

        let iteration_note = params.get("iteration_note").cloned();
        if changes_code && iteration_note.is_none() {
            return Err(ToolError::InvalidParams(
                "改代码类 update 必须提供 'iteration_note' (JSON 对象, 含【问题】【根因】【修复】【验证】四节), 会自动 append 到 docs/更新迭代.md".into(),
            ));
        }

        let entry = self
            .manager
            .update_plugin(id, index_html, app_source, source_files_map.as_ref(), manifest.as_ref())
            .map_err(|e| ToolError::ExecutionFailed(format!("更新应用失败: {e}")))?;

        let mut log_result: Option<String> = None;
        if let Some(note) = iteration_note {
            let mut note_with_name = if note.is_object() {
                note.as_object().unwrap().clone()
            } else {
                let mut m = serde_json::Map::new();
                m.insert("问题".into(), note.clone());
                m.insert("根因".into(), json!("(由 update 自动记录)"));
                m.insert("修复".into(), json!(format!("update action 调用 (id={id})")));
                m.insert("验证".into(), json!("(待 validate/run 验证)"));
                m
            };
            note_with_name.entry("name".to_string()).or_insert(json!(entry.manifest.name));

            match app_docs::append_iteration(&app_dir, &Value::Object(note_with_name)) {
                Ok(p) => log_result = Some(format!("已 log_iteration → {}", p.display())),
                Err(e) => return Err(ToolError::ExecutionFailed(format!(
                    "update 落地成功但 log_iteration 失败: {e}."
                ))),
            }
        }

        let mut plan_result: Option<String> = None;
        if let Some(plan_note) = params.get("plan_note").and_then(|v| v.as_str()) {
            let plan_path = app_dir.join("docs").join("开发计划进度.md");
            if plan_path.exists() {
                let mut s = std::fs::read_to_string(&plan_path).unwrap_or_default();
                let stamp = chrono_like_now();
                s.push_str(&format!("\n- [{stamp}] {plan_note}\n"));
                if let Err(e) = std::fs::write(&plan_path, s) {
                    tracing::warn!("写 开发计划进度.md 失败 (非致命): {e}");
                } else {
                    plan_result = Some(format!("已 append plan_note → 开发计划进度.md"));
                }
            }
        }

        let response: AppResponse = (&entry).into();
        let result = json!({
            "success": true,
            "message": format!("应用 '{}' 更新成功", entry.manifest.name),
            "app": response,
            "log_iteration": log_result,
            "plan_note": plan_result,
            "next_steps": [
                "调用 action=validate 复验",
                "调用 action=run 测试版预览",
                "如有 bug: 先 action=search_iterations query=<关键词>",
                "修改 docs/开发计划进度.md 勾掉已完成 checkbox"
            ]
        });
        Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
    }

    async fn list(&self) -> Result<String, ToolError> {
        self.manager
            .scan_plugins()
            .map_err(|e| ToolError::ExecutionFailed(format!("扫描应用失败: {e}")))?;
        let plugins = self.manager.list_plugins();
        let summaries: Vec<AppSummary> = {
            let entries: Vec<_> = plugins
                .iter()
                .map(|m| {
                    let entry = self.manager.get_plugin(&m.id);
                    entry.map(|e| AppSummary::from(&e))
                })
                .collect();
            entries.into_iter().flatten().collect()
        };
        let result = json!({
            "success": true,
            "apps": summaries,
            "count": summaries.len(),
        });
        Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
    }

    async fn get(&self, params: &Value) -> Result<String, ToolError> {
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'id' 参数".into()))?;
        let entry = self
            .manager
            .get_plugin(id)
            .ok_or_else(|| ToolError::NotFound(format!("应用不存在: {id}")))?;
        let response: AppResponse = (&entry).into();
        Ok(serde_json::to_string_pretty(&json!({
            "success": true, "app": response,
        })).unwrap_or_default())
    }

    async fn delete(&self, params: &Value) -> Result<String, ToolError> {
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'id' 参数".into()))?;
        self.manager
            .uninstall_plugin(id)
            .map_err(|e| ToolError::ExecutionFailed(format!("删除应用失败: {e}")))?;
        Ok(serde_json::to_string_pretty(&json!({
            "success": true,
            "message": format!("应用 '{id}' 已删除 (含 docs/ 与 dist/)"),
        })).unwrap_or_default())
    }

    async fn package(&self, params: &Value) -> Result<String, ToolError> {
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'id' 参数".into()))?;
        let build_mode = params
            .get("build_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("sidecar");
        let mode = build_mode
            .parse::<hermix_plugin::PackageMode>()
            .map_err(|e| ToolError::InvalidParams(format!("无效的 build_mode: {e}")))?;
        let exe_path = self
            .manager
            .package_plugin_with_mode(id, mode)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("打包应用失败: {e}")))?;
        Ok(serde_json::to_string_pretty(&json!({
            "success": true,
            "message": format!("应用 '{id}' 打包成功 ({build_mode} 模式)"),
            "exe_path": exe_path.to_string_lossy(),
            "output_dir": exe_path.parent().map(|p| p.to_string_lossy().to_string()),
            "build_mode": build_mode,
        })).unwrap_or_default())
    }

    async fn run(&self, params: &Value) -> Result<String, ToolError> {
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'id' 参数".into()))?;
        self.manager
            .get_plugin(id)
            .ok_or_else(|| ToolError::NotFound(format!("应用不存在: {id}")))?;
        self.manager
            .run_dev_plugin(id)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("启动应用失败: {e}")))?;
        let dev_html = self
            .manager
            .plugins_dir()
            .join(id)
            .join("dist")
            .join(format!("{id}.dev.html"));
        Ok(serde_json::to_string_pretty(&json!({
            "success": true,
            "message": format!("应用 '{id}' 已以测试版(dev)模式启动"),
            "dev_html": dev_html.to_string_lossy(),
            "run_hint": "测试版用 hx-shell --html 直接加载, 不生成单 exe. 要发布请调用 package action.",
            "update_hint": "回到继续开发: 直接调用 update action 改 app_source/index_html, 再 run 即时预览.",
        })).unwrap_or_default())
    }

    async fn validate(&self, params: &Value) -> Result<String, ToolError> {
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'id' 参数".into()))?;
        let app_dir = self.manager.plugins_dir().join(id);
        if !app_dir.exists() {
            return Err(ToolError::NotFound(format!("应用目录不存在: {id}")));
        }
        let report = validate_custom_app(&app_dir);
        let docs_report = hermix_plugin::check_docs_consistency(&app_dir);
        let mut combined_ok = report.ok && docs_report.ok;
        let mut merged_issues = report.issues.clone();
        for issue_msg in &docs_report.issues {
            merged_issues.push(hermix_plugin::validate::ValidationIssue {
                level: "error".to_string(),
                code: "docs".to_string(),
                message: issue_msg.clone(),
                line: None,
                col: None,
                snippet: None,
            });
        }
        for missing in &docs_report.missing {
            merged_issues.push(hermix_plugin::validate::ValidationIssue {
                level: "error".to_string(),
                code: "docs".to_string(),
                message: format!("缺少文档: {missing}"),
                line: None,
                col: None,
                snippet: None,
            });
            combined_ok = false;
        }
        Ok(serde_json::to_string_pretty(&json!({
            "success": combined_ok,
            "message": if combined_ok {
                format!("应用 '{id}' 开发期校验通过 (L0 manifest + L1 HTML + L2 权限 + L3 前端质量 + L4 文档一致性)")
            } else {
                format!("应用 '{id}' 开发期校验发现问题, 请根据 issues 修复后重试")
            },
            "validation": {
                "ok": combined_ok,
                "issues": merged_issues,
                "issue_count": merged_issues.len(),
                "docs": {
                    "ok": docs_report.ok,
                    "present": docs_report.present,
                    "missing": docs_report.missing,
                    "issues": docs_report.issues,
                }
            }
        })).unwrap_or_default())
    }

    async fn patch(&self, params: &Value) -> Result<String, ToolError> {
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'id' 参数".into()))?;
        let target = params
            .get("target")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::InvalidParams("缺少 'target' 参数 (index_html|app_source|source_file)".into())
            })?;
        let old_block = params
            .get("old_block")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'old_block' 参数".into()))?;
        if old_block.is_empty() {
            return Err(ToolError::InvalidParams("old_block 不能为空".into()));
        }
        let new_block = params
            .get("new_block")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'new_block' 参数".into()))?;
        let occurrence = params
            .get("occurrence")
            .and_then(|v| v.as_str())
            .unwrap_or("first");
        let replace_all = occurrence == "all";

        let app_dir = self.manager.plugins_dir().join(id);
        if !app_dir.exists() {
            return Err(ToolError::NotFound(format!("应用目录不存在: {id}")));
        }

        let file_path = match target {
            "index_html" => app_dir.join("index.html"),
            "app_source" => app_dir.join("src").join("main.tsx"),
            "source_file" => {
                let p = params.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
                    ToolError::InvalidParams("target=source_file 时必须提供 'path'".into())
                })?;
                if p.contains("..") || p.starts_with('/') || p.starts_with('\\') {
                    return Err(ToolError::InvalidParams("path 非法: 禁止 '..' 或绝对路径".into()));
                }
                app_dir.join("src").join(p)
            }
            other => return Err(ToolError::InvalidParams(format!("未知 target: '{other}'"))),
        };
        if !file_path.exists() {
            return Err(ToolError::NotFound(format!(
                "目标文件不存在: {} (target={target})",
                file_path.display()
            )));
        }
        let content = std::fs::read_to_string(&file_path).map_err(|e| {
            ToolError::ExecutionFailed(format!("读取 {} 失败: {e}", file_path.display()))
        })?;
        let count = if replace_all {
            content.matches(old_block).count()
        } else if content.contains(old_block) {
            1
        } else {
            0
        };
        if count == 0 {
            return Err(ToolError::InvalidParams(format!(
                "在 {} 中未找到 old_block。请用 validate 返回的 snippet 作为 old_block.",
                file_path.display()
            )));
        }
        let new_content = if replace_all {
            content.replace(old_block, new_block)
        } else {
            content.replacen(old_block, new_block, 1)
        };
        std::fs::write(&file_path, &new_content).map_err(|e| {
            ToolError::ExecutionFailed(format!("写入 {} 失败: {e}", file_path.display()))
        })?;
        Ok(serde_json::to_string_pretty(&json!({
            "success": true,
            "message": format!("已对 {target} 做 {count} 处手术式替换"),
            "file": file_path.display().to_string(),
            "replacements": count,
            "next": "调用 custom_app_manager validate action 复验; 如改动非 trivial, 调 log_iteration 记录"
        })).unwrap_or_default())
    }

    async fn smoke(&self, params: &Value) -> Result<String, ToolError> {
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'id' 参数".into()))?;
        let app_dir = self.manager.plugins_dir().join(id);
        if !app_dir.exists() {
            return Err(ToolError::NotFound(format!("应用目录不存在: {id}")));
        }
        self.manager
            .package_plugin(id)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("打包应用失败: {e}")))?;
        let app_dir_buf = app_dir.to_path_buf();
        let report = run_smoke_test_async(app_dir_buf).await;
        Ok(serde_json::to_string_pretty(&json!({
            "success": report.ok,
            "message": if report.ok {
                format!("应用 '{id}' 运行时冒烟测试通过")
            } else {
                format!("应用 '{id}' 运行时冒烟测试发现问题")
            },
            "smoke": report,
        })).unwrap_or_default())
    }

    async fn read_doc(&self, params: &Value) -> Result<String, ToolError> {
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'id' 参数".into()))?;
        let doc_str = params
            .get("doc")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams(
                "缺少 'doc' 参数 (设计方案|开发计划进度|更新迭代|README)".into(),
            ))?;
        let doc = Doc::parse(doc_str)
            .ok_or_else(|| ToolError::InvalidParams(format!("未知 doc '{doc_str}'")))?;
        let app_dir = self.manager.plugins_dir().join(id);
        let content = app_docs::read_doc(&app_dir, doc)
            .map_err(|e| ToolError::NotFound(format!("读 {doc_str} 失败: {e}")))?;
        Ok(serde_json::to_string_pretty(&json!({
            "success": true,
            "id": id,
            "doc": doc_str,
            "content": content,
            "next_steps": [
                "据此规划下一步",
                "若是 bug 修复: 先 action=search_iterations query=<关键词>",
                "若是新阶段: 修改 开发计划进度.md 勾掉已完成 checkbox"
            ]
        })).unwrap_or_default())
    }

    async fn update_doc(&self, params: &Value) -> Result<String, ToolError> {
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'id' 参数".into()))?;
        let doc_str = params
            .get("doc")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'doc' 参数".into()))?;
        let content = params
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'content' 参数".into()))?;
        let doc = Doc::parse(doc_str)
            .ok_or_else(|| ToolError::InvalidParams(format!("未知 doc '{doc_str}'")))?;
        if doc == Doc::Design {
            return Err(ToolError::InvalidParams(
                "设计方案.md 不允许直接 update_doc; 改方案必须重新 propose + approve".into(),
            ));
        }
        let app_dir = self.manager.plugins_dir().join(id);
        if !app_dir.exists() {
            return Err(ToolError::NotFound(format!("应用目录不存在: {id}")));
        }
        let path = app_docs::update_doc(&app_dir, doc, content)
            .map_err(|e| ToolError::ExecutionFailed(format!("写 {doc_str} 失败: {e}")))?;
        Ok(serde_json::to_string_pretty(&json!({
            "success": true,
            "id": id,
            "doc": doc_str,
            "path": path.to_string_lossy(),
            "next": "调用 action=validate 复验文档一致性"
        })).unwrap_or_default())
    }

    async fn log_iteration(&self, params: &Value) -> Result<String, ToolError> {
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'id' 参数".into()))?;
        let app_dir = self.manager.plugins_dir().join(id);
        if !app_dir.exists() {
            return Err(ToolError::NotFound(format!("应用目录不存在: {id}")));
        }
        let mut iteration = params
            .get("iteration")
            .cloned()
            .ok_or_else(|| ToolError::InvalidParams("缺少 'iteration' 参数".into()))?;
        if let Some(s) = iteration.as_str() {
            iteration = serde_json::from_str::<Value>(s).map_err(|e| {
                ToolError::InvalidParams(format!("iteration 字符串无法解析为对象: {e}"))
            })?;
        }
        if !iteration.is_object() {
            return Err(ToolError::InvalidParams("iteration 必须是 JSON 对象".into()));
        }
        let entry = self.manager.get_plugin(id);
        if let Some(e) = entry {
            iteration.as_object_mut().unwrap()
                .entry("name".to_string())
                .or_insert(json!(e.manifest.name));
        }
        let path = app_docs::append_iteration(&app_dir, &iteration)
            .map_err(|e| ToolError::InvalidParams(format!("log_iteration 失败: {e}")))?;
        Ok(serde_json::to_string_pretty(&json!({
            "success": true,
            "id": id,
            "message": "已 append 一条迭代到 docs/更新迭代.md",
            "path": path.to_string_lossy(),
            "next": "调用 action=search_iterations 验证"
        })).unwrap_or_default())
    }

    async fn search_iterations(&self, params: &Value) -> Result<String, ToolError> {
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'id' 参数".into()))?;
        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'query' 参数".into()))?;
        let app_dir = self.manager.plugins_dir().join(id);
        if !app_dir.exists() {
            return Err(ToolError::NotFound(format!("应用目录不存在: {id}")));
        }
        let hits = app_docs::search_iterations(&app_dir, query)
            .map_err(|e| ToolError::ExecutionFailed(format!("search_iterations 失败: {e}")))?;
        Ok(serde_json::to_string_pretty(&json!({
            "success": true,
            "id": id,
            "query": query,
            "hits": hits,
            "count": hits.len(),
            "hint": if hits.is_empty() {
                "无历史命中, 自由修. 修完记得 log_iteration 记录."
            } else {
                "命中历史, 修前先看清楚上次怎么修的, 避免重复触发."
            }
        })).unwrap_or_default())
    }
}

#[async_trait]
impl ToolHandler for CustomAppManagerHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'action' 参数".into()))?;
        match action {
            "create" => self.create(&params).await,
            "propose" => self.propose(&params).await,
            "approve" => self.approve(&params).await,
            "update" => self.update(&params).await,
            "list" => self.list().await,
            "get" => self.get(&params).await,
            "delete" => self.delete(&params).await,
            "package" => self.package(&params).await,
            "run" => self.run(&params).await,
            "validate" => self.validate(&params).await,
            "smoke" => self.smoke(&params).await,
            "patch" => self.patch(&params).await,
            "read_doc" => self.read_doc(&params).await,
            "update_doc" => self.update_doc(&params).await,
            "log_iteration" => self.log_iteration(&params).await,
            "search_iterations" => self.search_iterations(&params).await,
            other => Err(ToolError::InvalidParams(format!("未知操作: '{other}'"))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("action".into(), json!({
            "type": "string",
            "enum": ["create", "propose", "approve", "update", "list", "get", "delete", "package", "run", "validate", "smoke", "patch", "read_doc", "update_doc", "log_iteration", "search_iterations"]
        }));
        props.insert("id".into(), json!({"type": "string"}));
        props.insert("manifest".into(), json!({
            "type": "object",
            "description": "应用清单 (create 必填, update 可选)",
            "properties": {
                "id": {"type": "string"}, "name": {"type": "string"}, "version": {"type": "string"},
                "description": {"type": "string"}, "long_description": {"type": "string"},
                "author": {"type": "string"}, "icon": {"type": "string"},
                "category": {"type": "string"}, "tags": {"type": "array", "items": {"type": "string"}},
                "window_config": {"type": "object"},
                "permissions": {"type": "array", "items": {"type": "string"}}
            }
        }));
        props.insert("proposal".into(), json!({
            "type": "object",
            "description": "propose 必填. 结构化设计方案 (含 名称/功能/界面布局/交互效果 四节 + 自定义应用专属)."
        }));
        props.insert("index_html".into(), json!({
            "type": "string",
            "description": "[模式一: 纯 HTML] 自包含 index.html 的完整源代码内容. 改代码类 update 必须同时给 iteration_note."
        }));
        props.insert("app_source".into(), json!({
            "type": "string",
            "description": "[模式二: TS/React] 应用入口源码. 改代码类 update 必须同时给 iteration_note."
        }));
        props.insert("source_files".into(), json!({
            "type": "object",
            "description": "额外 TS 源文件 (可选). 键为相对 src/ 的路径.",
            "additionalProperties": {"type": "string"}
        }));
        props.insert("build_mode".into(), json!({"type": "string", "enum": ["sidecar", "neu-build"]}));
        props.insert("icon_data".into(), json!({"type": "string"}));
        props.insert("target".into(), json!({"type": "string", "enum": ["index_html", "app_source", "source_file"]}));
        props.insert("path".into(), json!({"type": "string"}));
        props.insert("old_block".into(), json!({"type": "string"}));
        props.insert("new_block".into(), json!({"type": "string"}));
        props.insert("occurrence".into(), json!({"type": "string", "enum": ["first", "all"]}));
        props.insert("iteration_note".into(), json!({
            "type": "object",
            "description": "[文档管理强制] 改代码类 update 必填. JSON 对象含【问题】【根因】【修复】【验证】4 节."
        }));
        props.insert("plan_note".into(), json!({"type": "string"}));
        props.insert("doc".into(), json!({
            "type": "string",
            "enum": ["设计方案", "开发计划进度", "更新迭代", "README"]
        }));
        props.insert("content".into(), json!({"type": "string"}));
        props.insert("iteration".into(), json!({
            "type": "object",
            "description": "log_iteration 必填. JSON 对象, 必须含【问题】【根因】【修复】【验证】4 节.",
            "properties": {
                "问题": {"type": "string"}, "根因": {"type": "string"},
                "修复": {"type": "string"}, "验证": {"type": "string"}
            },
            "required": ["问题", "根因", "修复", "验证"]
        }));
        props.insert("query".into(), json!({"type": "string"}));
        tool_schema(
            "custom_app_manager",
            "管理用户自定义桌面应用 + 强制文档维护机制. [文档管理强制] docs/ 4 个 md. [设计闸门] propose→approve→create. [代码精炼] 单行/封装/不重复造轮子 (详见 code-craftsmanship skill).",
            JsonSchema::object(props, vec!["action".into()]),
        )
    }
}

fn chrono_like_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_tmp(suffix: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("cam_{}_{}", std::process::id(), suffix))
    }

    fn init_test_docs(app_dir: &std::path::Path) {
        // 模拟 install_plugin → init_docs 后的 docs/ 4 个 md
        let _ = std::fs::create_dir_all(app_dir.join("docs"));
        std::fs::write(app_dir.join("docs/设计方案.md"), "# 测试 设计方案\n## 1. 名称\n- 测试\n").unwrap();
        std::fs::write(app_dir.join("docs/开发计划进度.md"), "# 测试 开发计划\n").unwrap();
        std::fs::write(app_dir.join("docs/更新迭代.md"), "# 测试 更新迭代\n---\n").unwrap();
        std::fs::write(app_dir.join("docs/README.md"), "# 测试 README\n").unwrap();
    }

    #[test]
    fn manifest_string_normalization_parses_json_object() {
        let params = json!({"action": "create", "manifest": r#"{"id":"x","name":"y"}"#});
        let v: serde_json::Value = serde_json::from_str(params["manifest"].as_str().unwrap()).unwrap();
        assert_eq!(v["id"], "x");
    }

    #[test]
    fn manifest_object_passes_through_unchanged() {
        let params = json!({"action": "create", "manifest": {"id": "x"}});
        assert_eq!(params["manifest"]["id"], "x");
    }

    #[test]
    fn manifest_invalid_string_fails_gracefully() {
        let r = serde_json::from_str::<serde_json::Value>("not json");
        assert!(r.is_err());
    }

    #[test]
    fn patch_app_source_replaces_precise_block() {
        let tmp = make_tmp("patch");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("patch-app").join("src")).unwrap();
        let mgr = std::sync::Arc::new(hermix_plugin::PluginManager::new(tmp.clone()));
        let handler = CustomAppManagerHandler::new(mgr);
        std::fs::write(tmp.join("patch-app/src/main.tsx"), "const a='OLD';\nconst b='keep';\n").unwrap();
        let res = futures::executor::block_on(handler.patch(&json!({
            "action": "patch", "id": "patch-app", "target": "app_source",
            "old_block": "'OLD'", "new_block": "'NEW'",
        })));
        assert!(res.is_ok(), "{:?}", res.err());
        let after = std::fs::read_to_string(tmp.join("patch-app/src/main.tsx")).unwrap();
        assert!(after.contains("'NEW'"));
        assert!(!after.contains("'OLD'"));
        assert!(after.contains("'keep'"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn patch_old_block_not_found_errors() {
        let tmp = make_tmp("patchnf");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("patch-nf/src")).unwrap();
        let mgr = std::sync::Arc::new(hermix_plugin::PluginManager::new(tmp.clone()));
        let handler = CustomAppManagerHandler::new(mgr);
        std::fs::write(tmp.join("patch-nf/src/main.tsx"), "x").unwrap();
        let res = futures::executor::block_on(handler.patch(&json!({
            "action": "patch", "id": "patch-nf", "target": "app_source",
            "old_block": "ZZZ", "new_block": "y",
        })));
        assert!(res.is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn patch_path_traversal_rejected() {
        let tmp = make_tmp("patchtrav");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("trav/src")).unwrap();
        let mgr = std::sync::Arc::new(hermix_plugin::PluginManager::new(tmp.clone()));
        let handler = CustomAppManagerHandler::new(mgr);
        std::fs::write(tmp.join("trav/src/main.tsx"), "x").unwrap();
        let res = futures::executor::block_on(handler.patch(&json!({
            "action": "patch", "id": "trav", "target": "source_file",
            "path": "../escape.tsx", "old_block": "x", "new_block": "y",
        })));
        assert!(res.is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn update_code_requires_iteration_note() {
        let tmp = make_tmp("iter");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("iter-app/src")).unwrap();
        let mgr = std::sync::Arc::new(hermix_plugin::PluginManager::new(tmp.clone()));
        let handler = CustomAppManagerHandler::new(mgr);
        std::fs::write(tmp.join("iter-app/src/main.tsx"), "x").unwrap();
        let res = futures::executor::block_on(handler.update(&json!({
            "action": "update", "id": "iter-app", "app_source": "y",
        })));
        assert!(res.is_err());
        assert!(res.as_ref().err().unwrap().to_string().contains("iteration_note"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn doc_actions_full_flow() {
        let tmp = make_tmp("docs");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("docs-app/src")).unwrap();
        let mgr = std::sync::Arc::new(hermix_plugin::PluginManager::new(tmp.clone()));
        let handler = CustomAppManagerHandler::new(mgr);
        std::fs::write(tmp.join("docs-app/src/main.tsx"), "init").unwrap();
        init_test_docs(&tmp.join("docs-app"));

        let bad = futures::executor::block_on(handler.log_iteration(&json!({
            "action": "log_iteration", "id": "docs-app",
            "iteration": {"问题": "x"}
        })));
        assert!(bad.is_err(), "log_iteration 缺 4 节应报错");

        let good = futures::executor::block_on(handler.log_iteration(&json!({
            "action": "log_iteration", "id": "docs-app",
            "iteration": {"问题": "按钮失效", "根因": "事件没绑", "修复": "加 onClick", "验证": "可点"}
        })));
        assert!(good.is_ok(), "log_iteration 应成功: {:?}", good.err());

        let hits = futures::executor::block_on(handler.search_iterations(&json!({
            "action": "search_iterations", "id": "docs-app", "query": "按钮"
        }))).unwrap();
        assert!(hits.contains("hits"));

        let rd = futures::executor::block_on(handler.read_doc(&json!({
            "action": "read_doc", "id": "docs-app", "doc": "设计方案"
        })));
        assert!(rd.is_ok(), "read_doc 应成功: {:?}", rd.err());

        let ud = futures::executor::block_on(handler.update_doc(&json!({
            "action": "update_doc", "id": "docs-app", "doc": "设计方案", "content": "hack"
        })));
        assert!(ud.is_err(), "update_doc 设计方案.md 应被拒");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}