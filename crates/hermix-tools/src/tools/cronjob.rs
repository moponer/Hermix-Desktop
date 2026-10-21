//! Cron job management tool

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema, traits::RiskLevel};

use crate::ToolRegistry;
use std::sync::Arc;

/// Register this tool with the registry (P1.2 per-tool migration).
///
/// 默认挂 Signal 占位后端——它只会返回一段没人消费的请求 JSON。宿主有
/// 真实调度面时（hermix-server 的 dashboard 自动化存储）应改用
/// [`register_with_backend`] 覆盖注册，否则对话里创建的定时任务只是
/// 空头支票。
pub fn register(registry: &ToolRegistry) {
    register_with_backend(
        registry,
        Arc::new(crate::backends::cronjob::SignalCronjobBackend::new()),
    );
}

/// 用指定后端注册 cronjob 工具。
///
/// `override_existing = true`：宿主在 `register_all` 之后用真实后端覆盖
/// Signal 占位实现是预期路径（插件用 false 注册，覆盖不了内置工具——
/// "插件不能遮蔽内置"的保证不受影响）。
pub fn register_with_backend(registry: &ToolRegistry, backend: Arc<dyn CronjobBackend>) {
    let handler = Arc::new(CronjobHandler::new(backend));
    let schema = handler.schema();
    registry.register_with_risk(
        "cronjob",
        "cronjob",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        false,
        "Manage scheduled automations (cron jobs): create, list, update, pause, resume, remove, or run. 定时任务/自动化（如「每天早上自动…」「定时提醒…」）一律用此工具，不要为此创建自定义应用。\
        create 的 `anchor_session_id` 决定结果投到哪：不传=当前会话；空串=不锚定（结果落在任务自己的会话，不打扰任何对话）；具体 id=投到那个会话。",
        "⏰",
        None,
        RiskLevel::Safe,
        true,
    );
}

// ---------------------------------------------------------------------------
// CronjobBackend trait
// ---------------------------------------------------------------------------

/// Backend for cron job management operations.
#[async_trait]
pub trait CronjobBackend: Send + Sync {
    /// Create a new cron job.
    ///
    /// `anchor_session_id` is the conversation the job was created in, if
    /// any. The registry stamps `_session_id` into tool params
    /// (`ToolRegistry::inject_session_id`), so a `cronjob` call made inside
    /// a chat carries its origin here and the created automation can be
    /// anchored to it. `None` for UI-created jobs (and for headless
    /// dispatch with no session).
    async fn create(
        &self,
        name: &str,
        schedule: &str,
        task: &str,
        toolset: Option<&str>,
        anchor_session_id: Option<&str>,
    ) -> Result<String, ToolError>;
    /// List all cron jobs.
    async fn list(&self) -> Result<String, ToolError>;
    /// Update a cron job.
    async fn update(
        &self,
        id: &str,
        schedule: Option<&str>,
        task: Option<&str>,
        enabled: Option<bool>,
    ) -> Result<String, ToolError>;
    /// Pause a cron job.
    async fn pause(&self, id: &str) -> Result<String, ToolError>;
    /// Resume a cron job.
    async fn resume(&self, id: &str) -> Result<String, ToolError>;
    /// Remove a cron job.
    async fn remove(&self, id: &str) -> Result<String, ToolError>;
    /// Run a cron job immediately.
    async fn run(&self, id: &str) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// CronjobHandler
// ---------------------------------------------------------------------------

/// Tool for managing cron jobs: create, list, update, pause, resume, remove, run.
pub struct CronjobHandler {
    backend: Arc<dyn CronjobBackend>,
}

impl CronjobHandler {
    pub fn new(backend: Arc<dyn CronjobBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for CronjobHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'action' parameter".into()))?;

        match action {
            "create" => {
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;
                let schedule =
                    params
                        .get("schedule")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            ToolError::InvalidParams("Missing 'schedule' parameter".into())
                        })?;
                let task = params
                    .get("task")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'task' parameter".into()))?;
                let toolset = params.get("toolset").and_then(|v| v.as_str());
                // 投递目标（会话锚点）三档：
                //   不传 `anchor_session_id` → 用当前会话（注册表注入的
                //     `_session_id`，空串视为无锚点，headless 派发就是这种）
                //   传空串 ""              → 显式不锚定：结果落在任务自己的
                //     会话里，不往任何人的对话里灌
                //   传具体 id              → 投到那个会话（跨会话转投，或把
                //     常驻任务钉在固定对话上）
                // 显式参数优先于注入值——否则调用方无法覆盖默认行为。
                let anchor = match params
                    .get("anchor_session_id")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                {
                    Some(explicit) if explicit.is_empty() => None,
                    Some(explicit) => Some(explicit),
                    None => params
                        .get("_session_id")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty()),
                };
                self.backend
                    .create(name, schedule, task, toolset, anchor)
                    .await
            }
            "list" => self.backend.list().await,
            "update" => {
                let id = params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'id' parameter".into()))?;
                let schedule = params.get("schedule").and_then(|v| v.as_str());
                let task = params.get("task").and_then(|v| v.as_str());
                // `opt_bool_param`, not `.and_then(as_bool)`: a
                // string-encoded `"false"` used to read as None, which
                // this API means "leave enabled unchanged" — so the
                // update was silently dropped.
                let enabled = crate::param_coercion::opt_bool_param(&params, "enabled");
                self.backend.update(id, schedule, task, enabled).await
            }
            "pause" => {
                let id = params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'id' parameter".into()))?;
                self.backend.pause(id).await
            }
            "resume" => {
                let id = params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'id' parameter".into()))?;
                self.backend.resume(id).await
            }
            "remove" => {
                let id = params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'id' parameter".into()))?;
                self.backend.remove(id).await
            }
            "run" => {
                let id = params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'id' parameter".into()))?;
                self.backend.run(id).await
            }
            other => Err(ToolError::InvalidParams(format!(
                "Unknown action: '{}'. Use create, list, update, pause, resume, remove, or run.",
                other
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "description": "Action to perform",
                "enum": ["create", "list", "update", "pause", "resume", "remove", "run"]
            }),
        );
        props.insert(
            "id".into(),
            json!({
                "type": "string",
                "description": "Cron job ID (for update, pause, resume, remove, run)"
            }),
        );
        props.insert(
            "anchor_session_id".into(),
            json!({
                "type": "string",
                "description": "create 时的投递目标会话 id。不传=当前会话；传空串=不锚定（结果落在任务自己的会话，不打扰任何对话）；传具体 id=投到那个会话"
            }),
        );
        props.insert(
            "name".into(),
            json!({
                "type": "string",
                "description": "Cron job name (for create)"
            }),
        );
        props.insert(
            "schedule".into(),
            json!({
                "type": "string",
                "description": "Cron schedule expression (e.g. '0 9 * * *' for 9am daily)"
            }),
        );
        props.insert(
            "task".into(),
            json!({
                "type": "string",
                "description": "Task description for the cron job"
            }),
        );
        props.insert(
            "toolset".into(),
            json!({
                "type": "string",
                "description": "Toolset to assign to the cron job's agent"
            }),
        );
        props.insert(
            "enabled".into(),
            json!({
                "type": "boolean",
                "description": "Whether the cron job is enabled (for update)"
            }),
        );

        tool_schema(
            "cronjob",
            "Manage scheduled automations (cron jobs): create, list, update, pause, resume, remove, or run. 定时任务/自动化（如「每天早上自动…」「定时提醒…」）一律用此工具，不要为此创建自定义应用。\
            create 的 `anchor_session_id` 决定结果投到哪：不传=当前会话；空串=不锚定；具体 id=投到那个会话。",
            JsonSchema::object(props, vec!["action".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct MockCronBackend;
    #[async_trait]
    impl CronjobBackend for MockCronBackend {
        async fn create(
            &self,
            name: &str,
            _schedule: &str,
            _task: &str,
            _toolset: Option<&str>,
            _anchor_session_id: Option<&str>,
        ) -> Result<String, ToolError> {
            Ok(format!("Created cronjob: {}", name))
        }
        async fn list(&self) -> Result<String, ToolError> {
            Ok("[]".to_string())
        }
        async fn update(
            &self,
            id: &str,
            _schedule: Option<&str>,
            _task: Option<&str>,
            _enabled: Option<bool>,
        ) -> Result<String, ToolError> {
            Ok(format!("Updated cronjob: {}", id))
        }
        async fn pause(&self, id: &str) -> Result<String, ToolError> {
            Ok(format!("Paused: {}", id))
        }
        async fn resume(&self, id: &str) -> Result<String, ToolError> {
            Ok(format!("Resumed: {}", id))
        }
        async fn remove(&self, id: &str) -> Result<String, ToolError> {
            Ok(format!("Removed: {}", id))
        }
        async fn run(&self, id: &str) -> Result<String, ToolError> {
            Ok(format!("Ran: {}", id))
        }
    }

    #[tokio::test]
    async fn test_cronjob_create() {
        let handler = CronjobHandler::new(Arc::new(MockCronBackend));
        let result = handler.execute(json!({"action": "create", "name": "test", "schedule": "0 9 * * *", "task": "Say hello"})).await.unwrap();
        assert!(result.contains("Created"));
    }

    #[tokio::test]
    async fn test_cronjob_list() {
        let handler = CronjobHandler::new(Arc::new(MockCronBackend));
        let result = handler.execute(json!({"action": "list"})).await.unwrap();
        assert_eq!(result, "[]");
    }

    #[tokio::test]
    async fn test_cronjob_schema() {
        let handler = CronjobHandler::new(Arc::new(MockCronBackend));
        assert_eq!(handler.schema().name, "cronjob");
    }

    /// Records what `create` actually received, so the `_session_id`
    /// → `anchor_session_id` handoff can be asserted instead of
    /// inferred from the response text.
    struct AnchorCapturingBackend(Mutex<Option<Option<String>>>);

    #[async_trait]
    impl CronjobBackend for AnchorCapturingBackend {
        async fn create(
            &self,
            _name: &str,
            _schedule: &str,
            _task: &str,
            _toolset: Option<&str>,
            anchor_session_id: Option<&str>,
        ) -> Result<String, ToolError> {
            *self.0.lock().expect("anchor mutex") = Some(anchor_session_id.map(str::to_string));
            Ok("ok".into())
        }
        async fn list(&self) -> Result<String, ToolError> {
            Ok("[]".into())
        }
        async fn update(
            &self,
            _id: &str,
            _schedule: Option<&str>,
            _task: Option<&str>,
            _enabled: Option<bool>,
        ) -> Result<String, ToolError> {
            Ok("ok".into())
        }
        async fn pause(&self, _id: &str) -> Result<String, ToolError> {
            Ok("ok".into())
        }
        async fn resume(&self, _id: &str) -> Result<String, ToolError> {
            Ok("ok".into())
        }
        async fn remove(&self, _id: &str) -> Result<String, ToolError> {
            Ok("ok".into())
        }
        async fn run(&self, _id: &str) -> Result<String, ToolError> {
            Ok("ok".into())
        }
    }

    fn anchored_backend() -> (CronjobHandler, Arc<AnchorCapturingBackend>) {
        let backend = Arc::new(AnchorCapturingBackend(Mutex::new(None)));
        (CronjobHandler::new(backend.clone()), backend)
    }

    #[tokio::test]
    async fn test_cronjob_create_forwards_injected_session_as_anchor() {
        let (handler, backend) = anchored_backend();
        handler
            .execute(json!({
                "action": "create",
                "name": "daily",
                "schedule": "0 9 * * *",
                "task": "digest",
                "_session_id": "sess-abc",
            }))
            .await
            .unwrap();
        assert_eq!(
            *backend.0.lock().expect("anchor mutex"),
            Some(Some("sess-abc".to_string())),
            "registry 注入的 `_session_id` 必须原样传给 backend 作为 anchor"
        );
    }

    #[tokio::test]
    async fn test_cronjob_create_without_session_has_no_anchor() {
        let (handler, backend) = anchored_backend();
        // 无 `_session_id`（UI 创建 / 裸 execute 调用）。
        handler
            .execute(json!({"action": "create", "name": "ui", "schedule": "0 9 * * *", "task": "x"}))
            .await
            .unwrap();
        assert_eq!(*backend.0.lock().expect("anchor mutex"), Some(None));
    }

    #[tokio::test]
    async fn test_cronjob_create_treats_empty_session_as_no_anchor() {
        let (handler, backend) = anchored_backend();
        // 空串锚点等于"有个叫空串的会话"，必须被过滤掉。
        handler
            .execute(json!({
                "action": "create",
                "name": "headless",
                "schedule": "0 9 * * *",
                "task": "x",
                "_session_id": "",
            }))
            .await
            .unwrap();
        assert_eq!(*backend.0.lock().expect("anchor mutex"), Some(None));
    }

    /// 显式 `anchor_session_id` 必须压过注册表注入的 `_session_id`：
    /// 否则调用方无法把任务钉到别的会话，「跨会话转投」就做不了。
    #[tokio::test]
    async fn test_cronjob_explicit_anchor_overrides_injected_session() {
        let (handler, backend) = anchored_backend();
        handler
            .execute(json!({
                "action": "create",
                "name": "pinned",
                "schedule": "0 9 * * *",
                "task": "x",
                "_session_id": "sess-here",
                "anchor_session_id": "sess-target",
            }))
            .await
            .unwrap();
        assert_eq!(
            *backend.0.lock().expect("anchor mutex"),
            Some(Some("sess-target".to_string())),
            "显式锚点应覆盖注入值"
        );
    }

    /// 显式传空串 = 主动要求不锚定（即使当前会话存在）。
    /// 语义差别很关键：注入空串是"没有会话"，显式空串是"不要投给任何人"。
    #[tokio::test]
    async fn test_cronjob_explicit_empty_anchor_suppresses_injected_session() {
        let (handler, backend) = anchored_backend();
        handler
            .execute(json!({
                "action": "create",
                "name": "quiet",
                "schedule": "0 9 * * *",
                "task": "x",
                "_session_id": "sess-here",
                "anchor_session_id": "",
            }))
            .await
            .unwrap();
        assert_eq!(*backend.0.lock().expect("anchor mutex"), Some(None));
    }
}
