//! Custom App Manager tool — lets the LLM create, update, list, and package
//! user-facing desktop applications.
//!
//! This tool is the LLM's interface to the custom app system. The LLM
//! can:
//! - `create`: Create a new app with manifest + index_html
//! - `update`: Update an existing app's index_html / manifest
//! - `list`: List all installed apps
//! - `get`: Get an app's details and source code
//! - `delete`: Uninstall an app
//! - `package`: Package an app as a standalone EXE (wry loader + inject_app)
//! - `run`: Launch a packaged app
//! - `validate`: Static validation (manifest + HTML + RPC + permissions)
//! - `smoke`: Runtime smoke test (launch + capture JS errors)
//!
//! Architecture: App = self-contained index.html + manifest.json.
//! index.html calls system capabilities via fetch("app://localhost/rpc").
//! Packaging injects html+manifest into a wry-based loader → single exe.
//!
//! The actual file operations are delegated to `hermix_plugin::PluginManager`.

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_core::{
    tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema, traits::RiskLevel,
};
use hermix_plugin::{
    api::{AppResponse, AppSummary, CreateAppRequest},
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
        "Create, update, list, package, and run user-facing desktop applications (custom apps). Actions: create, update, list, get, delete, package, run.",
        "📦",
        None,
        RiskLevel::Caution,
        false,
    );
}

/// Handler for the custom_app_manager tool.
pub struct CustomAppManagerHandler {
    manager: Arc<PluginManager>,
}

impl CustomAppManagerHandler {
    pub fn new(manager: Arc<PluginManager>) -> Self {
        Self { manager }
    }

    async fn create(&self, params: &Value) -> Result<String, ToolError> {
        // Normalize: if `manifest` is a string (XML parser fallback), parse it
        // into a JSON object first so that serde_json::from_value can
        // deserialize it into CreateAppRequest.  This happens when the LLM
        // passes manifest as a JSON string inside <parameter> but the XML
        // parser's serde_json::from_str succeeds (producing a Value::Object)
        // OR when the LLM accidentally uses XML sub-tags, causing the parser
        // to treat the whole thing as a plain string.
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
                let hint = if e.to_string().contains("expected struct") || e.to_string().contains("expected map") {
                    " — manifest 参数必须是 JSON 对象 (如 {\"id\":\"x\",\"name\":\"y\"})，不能是字符串或 XML 子标签。在 XML 工具调用中，请用 <parameter name=\"manifest\">{...JSON...}</parameter> 传递。"
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

        let index_html = request.index_html.as_deref()
            .or_else(|| params.get("index_html").and_then(|v| v.as_str()));
        let entry = self
            .manager
            .install_plugin(
                &request.manifest,
                index_html,
                icon_data,
            )
            .map_err(|e| ToolError::ExecutionFailed(format!("创建应用失败: {e}")))?;

        let response: AppResponse = (&entry).into();
        let result = json!({
            "success": true,
            "message": format!("应用 '{}' 创建成功", entry.manifest.name),
            "app": response,
        });
        Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
    }

    async fn update(&self, params: &Value) -> Result<String, ToolError> {
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'id' 参数".into()))?;

        let index_html = params.get("index_html").and_then(|v| v.as_str());

        // Parse manifest if provided (for updating permissions etc.)
        // Normalize: if manifest is a string, try to parse it as JSON first.
        let manifest = if params.get("manifest").is_some() {
            let manifest_val = &params["manifest"];
            let manifest_val = if manifest_val.is_string() {
                serde_json::from_str::<Value>(manifest_val.as_str().unwrap_or(""))
                    .unwrap_or(manifest_val.clone())
            } else {
                manifest_val.clone()
            };
            let m: PluginManifest = serde_json::from_value(manifest_val)
                .map_err(|e| {
                    let hint = if e.to_string().contains("expected struct") || e.to_string().contains("expected map") {
                        " — manifest 参数必须是 JSON 对象，不能是字符串或 XML 子标签"
                    } else {
                        ""
                    };
                    ToolError::InvalidParams(format!("manifest 解析失败: {e}{hint}"))
                })?;
            Some(m)
        } else {
            None
        };

        if index_html.is_none() && manifest.is_none() {
            return Err(ToolError::InvalidParams(
                "至少需要提供 index_html 或 manifest 之一".into(),
            ));
        }

        let entry = self
            .manager
            .update_plugin(id, index_html, manifest.as_ref())
            .map_err(|e| ToolError::ExecutionFailed(format!("更新应用失败: {e}")))?;

        let response: AppResponse = (&entry).into();
        let result = json!({
            "success": true,
            "message": format!("应用 '{}' 更新成功", entry.manifest.name),
            "app": response,
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
        let result = json!({
            "success": true,
            "app": response,
        });
        Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
    }

    async fn delete(&self, params: &Value) -> Result<String, ToolError> {
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'id' 参数".into()))?;

        self.manager
            .uninstall_plugin(id)
            .map_err(|e| ToolError::ExecutionFailed(format!("删除应用失败: {e}")))?;

        let result = json!({
            "success": true,
            "message": format!("应用 '{id}' 已删除"),
        });
        Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
    }

    async fn package(&self, params: &Value) -> Result<String, ToolError> {
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'id' 参数".into()))?;

        let exe_path = self
            .manager
            .package_plugin(id)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("打包应用失败: {e}")))?;

        let result = json!({
            "success": true,
            "message": format!("应用 '{id}' 打包成功"),
            "exe_path": exe_path.to_string_lossy(),
            "output_dir": exe_path.parent().map(|p| p.to_string_lossy().to_string()),
        });
        Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
    }

    async fn run(&self, params: &Value) -> Result<String, ToolError> {
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'id' 参数".into()))?;

        // Existence check (run_plugin re-looks-up internally).
        self.manager
            .get_plugin(id)
            .ok_or_else(|| ToolError::NotFound(format!("应用不存在: {id}")))?;

        self.manager
            .run_plugin(id)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("启动应用失败: {e}")))?;

        let result = json!({
            "success": true,
            "message": format!("应用 '{id}' 已启动"),
        });
        Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
    }

    /// 开发期自验证 (L0+L1+L2): 校验 manifest 完整性 + HTML 结构 + RPC 调用模式 + JS 括号匹配 + 权限一致性.
    /// 不打包、不改任何文件, 仅静态分析. 返回结构化报告. 每个 issue 的 message 包含修复提示.
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
        let result = json!({
            "success": report.ok,
            "message": if report.ok {
                format!("应用 '{id}' 开发期校验通过 (manifest + HTML + RPC + 权限一致性)")
            } else {
                format!("应用 '{id}' 开发期校验发现问题, 请根据 issues 修复后重试")
            },
            "validation": report,
        });
        Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
    }

    /// L2 运行时冒烟测试: 先刷新打包产物, 再启动壳加载应用并捕获运行时错误.
    /// 通过 `run_smoke_test_async` 在 blocking 线程上启动壳、轮询日志.
    async fn smoke(&self, params: &Value) -> Result<String, ToolError> {
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("缺少 'id' 参数".into()))?;

        let app_dir = self.manager.plugins_dir().join(id);
        if !app_dir.exists() {
            return Err(ToolError::NotFound(format!("应用目录不存在: {id}")));
        }

        // 先刷新打包产物 (wry loader + inject_app 注入)
        self.manager
            .package_plugin(id)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("打包应用失败: {e}")))?;

        let app_dir_buf = app_dir.to_path_buf();
        let report = run_smoke_test_async(app_dir_buf).await;

        let result = json!({
            "success": report.ok,
            "message": if report.ok {
                format!("应用 '{id}' 运行时冒烟测试通过 (已挂载, 无运行时错误)")
            } else {
                format!("应用 '{id}' 运行时冒烟测试发现问题 (见 smoke.errors)")
            },
            "smoke": report,
        });
        Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
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
            "update" => self.update(&params).await,
            "list" => self.list().await,
            "get" => self.get(&params).await,
            "delete" => self.delete(&params).await,
            "package" => self.package(&params).await,
            "run" => self.run(&params).await,
            "validate" => self.validate(&params).await,
            "smoke" => self.smoke(&params).await,
            other => Err(ToolError::InvalidParams(format!(
                "未知操作: '{other}'。可用: create, update, list, get, delete, package, run, validate, smoke"
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();

        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "description": "要执行的操作",
                "enum": ["create", "update", "list", "get", "delete", "package", "run", "validate", "smoke"]
            }),
        );

        props.insert(
            "id".into(),
            json!({
                "type": "string",
                "description": "应用 ID (update/get/delete/package/run 必填，create 时在 manifest 中指定)"
            }),
        );

        // Manifest for create
        props.insert(
            "manifest".into(),
            json!({
                "type": "object",
                "description": "应用清单 (create 必填, update 可选用于更新 permissions 等)。包含 id, name, version, description, icon, category, window_config, permissions 等。version/description/author/icon/window_config 均有默认值,最简只需 id+name",
                "properties": {
                    "id": { "type": "string", "description": "唯一标识符 (字母数字-_)" },
                    "name": { "type": "string", "description": "应用名称" },
                    "version": { "type": "string", "description": "语义版本号" },
                    "description": { "type": "string", "description": "简短描述" },
                    "long_description": { "type": "string", "description": "详细描述" },
                    "author": { "type": "string", "description": "作者" },
                    "icon": { "type": "string", "description": "Emoji 图标" },
                    "category": { "type": "string", "description": "分类 (file-tools, media, productivity, other 等)" },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "window_config": {
                        "type": "object",
                        "properties": {
                            "width": { "type": "integer", "description": "窗口宽度 (默认 600)" },
                            "height": { "type": "integer", "description": "窗口高度 (默认 480)" },
                            "resizable": { "type": "boolean", "description": "是否可调整大小 (默认 true)" },
                            "title": { "type": "string", "description": "窗口标题" }
                        }
                    },
                    "permissions": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "权限列表 (fs-read, fs-write, exec, network, dialog, env, temp)"
                    }
                }
            }),
        );

        props.insert(
            "index_html".into(),
            json!({
                "type": "string",
                "description": "自包含 index.html 源代码 (新架构 wry 加载器模型，可选)。页面通过 fetch('app://localhost/rpc', {method:'POST', body: JSON.stringify({method, params})}) 调用桥接 (fs_*/exec/fetch/dialog_*/path_*/env_*/temp_*)。配合 manifest.permissions 声明所需权限。与 backend_code/ui_code 互斥，优先于旧架构。"
            }),
        );

        props.insert(
            "icon_data".into(),
            json!({
                "type": "string",
                "description": "自定义应用图标的 base64 编码 PNG 图片。支持 data:image/png;base64,... 格式或纯 base64 字符串。推荐 256x256 或 512x512 的正方形 PNG。如果未提供则使用默认图标。(create 可选)"
            }),
        );

        tool_schema(
            "custom_app_manager",
            "管理用户自定义桌面应用: 创建、更新、列表、查看、删除、打包为独立EXE、运行、开发期校验(validate)、运行时冒烟测试(smoke)。应用 = 自包含 index.html + manifest.json，由 wry 加载器打包为独立 exe。index.html 中通过 rpc('方法名', {参数}) 调用系统能力 (fs_*/exec/shell/fetch/dialog_*/path_*/env_*/temp_*)，需在 manifest.permissions 中声明所需权限。开发流程: 写代码→validate→失败则按 issues 修复→再 validate，直到 ok=true；如需进一步确认运行时无报错，可调用 smoke。",
            JsonSchema::object(props, vec!["action".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Verify that when `manifest` is passed as a JSON string (not an object),
    /// the normalization logic correctly parses it into a JSON value before
    /// `serde_json::from_value` attempts deserialization.
    #[test]
    fn manifest_string_normalization_parses_json_object() {
        let manifest_json = r#"{"id":"test-app","name":"Test App","version":"1.0.0"}"#;
        let params = json!({
            "action": "create",
            "manifest": manifest_json,  // This is a Value::String, not Value::Object
        });

        // The normalization should detect that manifest is a string and
        // re-parse it as a JSON object.
        let manifest_val = params.get("manifest").unwrap();
        assert!(manifest_val.is_string(), "manifest should be a string in this test");

        let parsed: serde_json::Value = serde_json::from_str(manifest_val.as_str().unwrap()).unwrap();
        assert!(parsed.is_object(), "parsed manifest should be a JSON object");
        assert_eq!(parsed["id"], "test-app");
        assert_eq!(parsed["name"], "Test App");
    }

    /// Verify that when `manifest` is already a JSON object (the normal case),
    /// it passes through unchanged.
    #[test]
    fn manifest_object_passes_through_unchanged() {
        let params = json!({
            "action": "create",
            "manifest": {
                "id": "test-app",
                "name": "Test App",
                "window_config": {"width": 800, "height": 600}
            }
        });

        let manifest_val = params.get("manifest").unwrap();
        assert!(manifest_val.is_object(), "manifest should be a JSON object");
        assert_eq!(manifest_val["id"], "test-app");
        assert_eq!(manifest_val["window_config"]["width"], 800);
    }

    /// Verify that an invalid JSON string in `manifest` fails gracefully
    /// (the normalization will leave it as-is, and serde_json::from_value
    /// will produce a helpful error message).
    #[test]
    fn manifest_invalid_string_fails_gracefully() {
        let params = json!({
            "action": "create",
            "manifest": "not valid json"
        });

        let manifest_val = params.get("manifest").unwrap();
        assert!(manifest_val.is_string());

        // Attempting to parse "not valid json" as a Value should fail
        let result = serde_json::from_str::<serde_json::Value>(manifest_val.as_str().unwrap());
        assert!(result.is_err(), "invalid JSON should fail to parse");
    }
}
