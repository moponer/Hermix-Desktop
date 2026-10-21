//! `watch` —— 「盯着这件事」的语义封装。
//!
//! 为什么不新增一套机制：引擎 A（`automation_tasks.json` + 30s tick）已经
//! 具备「定时唤醒 → 跑一轮 agent → 回投到锚定会话」的全部能力，且面板里
//! 可见、可暂停、可删除。守望缺的不是调度，是**语义**：盯什么、为什么盯、
//! 怎么算完成。
//!
//! 所以这里只做两件事：
//!   1. 把结构化的守望意图渲染成一段明确的检查指令；
//!   2. 调同一个 `CronjobBackend::create` 建一条普通定时任务。
//!
//! 代价与边界（写进工具描述，不藏着）：触发粒度是「每 N 分钟检查一次」，
//! 不是真事件驱动。看不到中间过程，只能看结果是否满足完成判据。

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_core::traits::RiskLevel;
use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use super::cronjob::CronjobBackend;
use crate::ToolRegistry;

/// 默认检查间隔：30 分钟。够密以发现异常，又不至于把用户吵醒。
const DEFAULT_CHECK_EVERY: &str = "*/30 * * * *";

pub struct WatchHandler {
    backend: Arc<dyn CronjobBackend>,
}

impl WatchHandler {
    pub fn new(backend: Arc<dyn CronjobBackend>) -> Self {
        Self { backend }
    }
}

fn str_param<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// 渲染成一段检查指令。措辞即行为：这里写「无事就不要说话」，
/// 否则每一轮都会回投一条「一切正常」，三次之后就没人看这个任务了。
fn render_check_prompt(
    target: &str,
    intent: Option<&str>,
    done_contract: Option<&str>,
) -> String {
    let mut prompt = format!("你在守望一件事：{target}\n");
    if let Some(intent) = intent {
        prompt.push_str(&format!("盯它的原因：{intent}\n"));
    }
    prompt.push_str(&format!(
        "完成判据：{}\n",
        done_contract.unwrap_or("（未给出——那就以「明显达成目标」为准，拿不准时问一句，不要猜）")
    ));
    prompt.push_str(
        "\n这一轮只做三件事：\n\
         1. 用最少的动作确认上面的完成判据是否已经满足；\n\
         2. 已满足 → 一句话说结果，并提醒可以用 cronjob(action=remove) 撤掉这条守望；\n\
         3. 未满足 → 只在出现异常（失败、卡住、方向明显错了）时用一句话报警；\n\
         一切正常就什么都不说。不要重复上一轮已经说过的内容。\n",
    );
    prompt
}

fn default_name(target: &str) -> String {
    let head: String = target.chars().take(24).collect();
    if target.chars().count() > 24 {
        format!("守望：{head}…")
    } else {
        format!("守望：{head}")
    }
}

#[async_trait]
impl ToolHandler for WatchHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidParams("缺少 'action' 参数".into()))?;

        match action {
            "create" => {
                let target = str_param(&params, "target")
                    .ok_or_else(|| ToolError::InvalidParams("缺少 'target'（要盯什么）".into()))?;
                let intent = str_param(&params, "intent");
                let done_contract = str_param(&params, "done_contract");
                let schedule =
                    str_param(&params, "check_every").unwrap_or(DEFAULT_CHECK_EVERY);
                let name = str_param(&params, "name")
                    .map(str::to_string)
                    .unwrap_or_else(|| default_name(target));

                // 三档锚点语义与 cronjob 一致：显式 > 注入 > 无。
                let anchor = match str_param(&params, "anchor_session_id") {
                    Some(explicit) => Some(explicit.to_string()),
                    None => str_param(&params, "_session_id").map(str::to_string),
                };

                let prompt = render_check_prompt(target, intent, done_contract);
                let out = self
                    .backend
                    .create(&name, schedule, &prompt, None, anchor.as_deref())
                    .await?;

                Ok(json!({
                    "created": true,
                    "name": name,
                    "check_every": schedule,
                    "done_contract": done_contract,
                    "backend": out,
                    "hint": "已建成一条定时检查任务（自动化页可见、可暂停）。它只在异常或完成时说话。",
                })
                .to_string())
            }
            "list" => self.backend.list().await,
            "remove" => {
                let id = params
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ToolError::InvalidParams("缺少 'id' 参数".into()))?;
                self.backend.remove(id).await
            }
            other => Err(ToolError::InvalidParams(format!(
                "未知 action: '{other}'。可选 create / list / remove。"
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
                "enum": ["create", "list", "remove"]
            }),
        );
        props.insert(
            "target".into(),
            json!({
                "type": "string",
                "description": "要盯什么（一句话，具体到可判定：「auth 模块的单测是否全过」）"
            }),
        );
        props.insert(
            "intent".into(),
            json!({
                "type": "string",
                "description": "为什么盯它（可选）。用户回看时靠这句判断要不要继续盯"
            }),
        );
        props.insert(
            "done_contract".into(),
            json!({
                "type": "string",
                "description": "怎么算完成（可选但强烈建议）：「pnpm tsc 退出码 0」这种可验证的条件。不给就只能靠「明显达成」判断"
            }),
        );
        props.insert(
            "check_every".into(),
            json!({
                "type": "string",
                "description": format!("检查间隔，5 段 cron。默认 {DEFAULT_CHECK_EVERY}（每 30 分钟）")
            }),
        );
        props.insert(
            "name".into(),
            json!({
                "type": "string",
                "description": "任务名（可选，默认由 target 生成）"
            }),
        );
        props.insert(
            "anchor_session_id".into(),
            json!({
                "type": "string",
                "description": "结果投到哪个会话。不传=当前会话；空串=不锚定"
            }),
        );
        props.insert(
            "id".into(),
            json!({ "type": "string", "description": "任务 id（remove 时必填）" }),
        );

        tool_schema(
            "watch",
            "盯住一件事：建一条定时检查任务，只在异常或完成时说话。\
             适合「帮我盯着 X，完了告诉我」。\
             触发粒度是每 N 分钟查一次（不是真事件流），看不到中间过程——\
             所以必须给可判定的完成判据，否则它会猜。",
            JsonSchema::object(props, vec!["action".into()]),
        )
    }
}

/// 注册 `watch`。需要宿主提供 cronjob 后端（与 `cronjob` 工具共用同一个实例，
/// 这样两条路写的是同一张任务表）。
pub fn register_with_backend(registry: &ToolRegistry, backend: Arc<dyn CronjobBackend>) {
    let handler = Arc::new(WatchHandler::new(backend));
    let schema = handler.schema();
    registry.register_with_risk(
        "watch",
        "watch",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        false,
        "盯住一件事：把结构化的守望意图渲染成定时检查任务，只在异常/完成时说话。\
        与 cronjob 共用同一张任务表，因此面板可见、可暂停、可删除。",
        "👁",
        None,
        RiskLevel::Safe,
        false,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 只记录 create 收到的参数，不碰真实存储。
    struct CapturingBackend(Mutex<Vec<(String, String, String, Option<String>)>>);

    #[async_trait]
    impl CronjobBackend for CapturingBackend {
        async fn create(
            &self,
            name: &str,
            schedule: &str,
            task: &str,
            _toolset: Option<&str>,
            anchor_session_id: Option<&str>,
        ) -> Result<String, ToolError> {
            self.0.lock().expect("mutex").push((
                name.to_string(),
                schedule.to_string(),
                task.to_string(),
                anchor_session_id.map(str::to_string),
            ));
            Ok("{}".into())
        }
        async fn list(&self) -> Result<String, ToolError> {
            Ok("[]".into())
        }
        async fn update(
            &self,
            _id: &str,
            _s: Option<&str>,
            _t: Option<&str>,
            _e: Option<bool>,
        ) -> Result<String, ToolError> {
            Ok("{}".into())
        }
        async fn pause(&self, _id: &str) -> Result<String, ToolError> {
            Ok("{}".into())
        }
        async fn resume(&self, _id: &str) -> Result<String, ToolError> {
            Ok("{}".into())
        }
        async fn remove(&self, _id: &str) -> Result<String, ToolError> {
            Ok("{}".into())
        }
        async fn run(&self, _id: &str) -> Result<String, ToolError> {
            Ok("{}".into())
        }
    }

    fn harness() -> (WatchHandler, Arc<CapturingBackend>) {
        let backend = Arc::new(CapturingBackend(Mutex::new(Vec::new())));
        (WatchHandler::new(backend.clone()), backend)
    }

    #[tokio::test]
    async fn create_renders_done_contract_and_defaults_to_30min() {
        let (handler, backend) = harness();
        handler
            .execute(json!({
                "action": "create",
                "target": "auth 模块单测",
                "intent": "明天要合并",
                "done_contract": "cargo test -p auth 退出码 0",
                "_session_id": "sess-1",
            }))
            .await
            .unwrap();

        let calls = backend.0.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let (name, schedule, prompt, anchor) = &calls[0];
        assert!(name.starts_with("守望："), "名称默认由 target 生成");
        assert_eq!(schedule, DEFAULT_CHECK_EVERY);
        assert_eq!(anchor.as_deref(), Some("sess-1"));
        assert!(prompt.contains("cargo test -p auth 退出码 0"), "完成判据必须进指令");
        assert!(prompt.contains("一切正常就什么都不说"), "必须压住噪音");
    }

    /// 没给完成判据时，不能假装判定得了 —— 指令里要明确要求「拿不准先问」。
    #[tokio::test]
    async fn missing_done_contract_asks_instead_of_guessing() {
        let (handler, backend) = harness();
        handler
            .execute(json!({ "action": "create", "target": "重构进展" }))
            .await
            .unwrap();
        let calls = backend.0.lock().unwrap();
        assert!(calls[0].2.contains("拿不准时问一句"), "{}", calls[0].2);
    }

    #[tokio::test]
    async fn target_is_required() {
        let (handler, _backend) = harness();
        assert!(handler.execute(json!({ "action": "create" })).await.is_err());
    }
}
