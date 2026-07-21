//! Custom App Manager tool — lets the LLM create, update, list, and package
//! user-facing desktop applications.
//!
//! This tool is the LLM's interface to the custom app system. The LLM
//! can:
//! - `create`: Create a new app with manifest + backend.ts + ui.tsx
//! - `update`: Update an existing app's code
//! - `list`: List all installed apps
//! - `get`: Get an app's details and source code
//! - `delete`: Uninstall an app
//! - `package`: Package an app as a standalone EXE
//! - `run`: Launch a packaged app
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
    PluginManager,
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
        let request: CreateAppRequest = serde_json::from_value(params.clone())
            .map_err(|e| ToolError::InvalidParams(format!("参数解析失败: {e}")))?;

        let icon_data = params.get("icon_data").and_then(|v| v.as_str());

        request
            .manifest
            .validate()
            .map_err(|e| ToolError::InvalidParams(format!("manifest 无效: {e}")))?;

        let entry = self
            .manager
            .install_plugin(&request.manifest, &request.backend_code, request.ui_code.as_deref(), icon_data)
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

        let backend_code = params.get("backend_code").and_then(|v| v.as_str());
        let ui_code = params.get("ui_code").and_then(|v| v.as_str());

        if backend_code.is_none() && ui_code.is_none() {
            return Err(ToolError::InvalidParams(
                "至少需要提供 backend_code 或 ui_code 之一".into(),
            ));
        }

        let entry = self
            .manager
            .update_plugin(id, backend_code, ui_code)
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

        let entry = self
            .manager
            .get_plugin(id)
            .ok_or_else(|| ToolError::NotFound(format!("应用不存在: {id}")))?;

        let wm = hermix_plugin::PluginWindowManager::new(
            self.manager.shell_path().to_path_buf(),
        );
        wm.open_plugin(&entry)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("启动应用失败: {e}")))?;

        let result = json!({
            "success": true,
            "message": format!("应用 '{id}' 已启动"),
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
            other => Err(ToolError::InvalidParams(format!(
                "未知操作: '{other}'。可用: create, update, list, get, delete, package, run"
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
                "enum": ["create", "update", "list", "get", "delete", "package", "run"]
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
                "description": "应用清单 (create 必填)。包含 id, name, version, description, icon, category, window_config, permissions 等",
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
                        "description": "权限列表 (fs-read, fs-write, net, env, process)"
                    }
                }
            }),
        );

        props.insert(
            "backend_code".into(),
            json!({
                "type": "string",
                "description": "backend.ts 源代码。使用 invoke('shell_fs_*') 进行文件操作，fetch() 进行网络请求。必须导出 execute(params) 函数。(create 必填, update 可选)"
            }),
        );

        props.insert(
            "ui_code".into(),
            json!({
                "type": "string",
                "description": "ui.tsx 源代码。React 组件，使用 invoke('shell_*') 调用后端。必须默认导出一个 React 组件。(create/update 可选)"
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
            "管理用户自定义桌面应用: 创建、更新、列表、查看、删除、打包为独立EXE、运行。打包使用预编译壳+esbuild，用户无需安装Rust/Tauri。",
            JsonSchema::object(props, vec!["action".into()]),
        )
    }
}
