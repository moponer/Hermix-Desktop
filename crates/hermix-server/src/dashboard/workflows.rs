//! 工作流端点：对话内由 `workflow` 工具保存的再跑流程。
//!
//! 存储本体在 hermix-tools（`tools::workflow`，`<hermix_home>/workflows.json`），
//! 这里只做 HTTP 包装 + 运行调度：run 走 `agent_service.send_message()`
//! （铁律 DD），会话 id = `workflow:{id}`——与对话同路径，跑完的会话在
//! 对话列表可见、可继续聊。

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use hermix_core::traits::AgentOverrides;
use hermix_tools::tools::workflow as store;
use serde::Deserialize;

use crate::state::HttpServerState;

/// GET /api/workflows — 全量列表（全局 + 各项目，前端按 workspace 分组）。
pub async fn list_workflows() -> Response {
    Json(serde_json::json!({ "items": store::list_workflows() })).into_response()
}

#[derive(Deserialize)]
pub struct WorkflowUpsert {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub workspace: Option<String>,
    pub prompt: String,
}

/// POST /api/workflows — 新增 / 更新。缺 id 时按「名称 + 作用域」去重。
pub async fn upsert_workflow(Json(body): Json<WorkflowUpsert>) -> Response {
    match store::upsert_workflow(
        body.id.as_deref(),
        &body.name,
        &body.description,
        body.workspace.as_deref(),
        &body.prompt,
    ) {
        Ok(entry) => Json(serde_json::json!({ "ok": true, "workflow": entry })).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

/// DELETE /api/workflows/{id}
pub async fn remove_workflow(Path(id): Path<String>) -> Response {
    match store::remove_workflow(&id) {
        Ok(_) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e).into_response(),
    }
}

#[derive(Deserialize, Default)]
pub struct WorkflowRunRequest {
    /// 替换 prompt 里的 `{{参数}}` 占位符（key = 参数名）。
    #[serde(default)]
    pub params: BTreeMap<String, String>,
}

/// POST /api/workflows/{id}/run — 再跑一次。
///
/// 在请求线程上只做参数替换 + 标记 running，真正的 agent 运行 spawn 到
/// 后台；完成后状态回写进工作流条目（卡片上可见 运行中 / 失败）。
pub async fn run_workflow(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
    Json(body): Json<WorkflowRunRequest>,
) -> Response {
    let Some(wf) = store::get_workflow(&id) else {
        return (
            StatusCode::NOT_FOUND,
            format!("没有 id 为 '{id}' 的工作流"),
        )
            .into_response();
    };

    let mut prompt = wf.prompt.clone();
    for (k, v) in &body.params {
        prompt = prompt.replace(&format!("{{{{{}}}}}", k), v);
    }

    store::update_run_status(&id, "running");
    let agent = state.agent_service.clone();
    tokio::spawn(async move {
        let mut overrides = AgentOverrides::default();
        if let Some(ws) = &wf.workspace {
            overrides.workspace = Some(ws.clone());
        }
        let session_id = format!("workflow:{}", wf.id);
        let result = agent.send_message(&session_id, &prompt, overrides).await;
        store::update_run_status(&wf.id, if result.is_ok() { "ok" } else { "failed" });
    });

    Json(serde_json::json!({
        "ok": true,
        "hint": "已开始运行，完成后可在对话列表查看 workflow 会话。",
    }))
    .into_response()
}
