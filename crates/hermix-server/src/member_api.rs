//! 任务台 REST 面（总管方案 P4.1）。
//!
//! 桌面端「任务台」视图的数据源：成员工单列表/详情/中止、资源巡检快照、
//! 角色库。执行侧（MemberRunner）由会话构建时安装（全局槽），未安装时
//! 列表类端点降级为空集 + 提示（桌面冷启动、尚无会话），不是错误——
//! 空任务台是合法状态。

use axum::extract::{Path, State, Extension};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::state::HttpServerState;

fn runner_or_unavailable() -> Result<
    std::sync::Arc<hermix_agent::member::runner::MemberRunner>,
    Response,
> {
    hermix_agent::member::runner::current_member_runner().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "member_runner_unavailable",
                "message": "成员运行器未安装（服务刚启动、尚无会话构建）。开一个对话后即可。"
            })),
        )
            .into_response()
    })
}

/// `GET /api/members` — 工单列表（轻量，不含报告正文）。
pub(crate) async fn list_members(State(_state): State<HttpServerState>) -> Response {
    match runner_or_unavailable() {
        Ok(runner) => Json(json!({ "members": runner.list().await })).into_response(),
        Err(resp) => resp,
    }
}

/// `GET /api/members/{member_id}` — 工单详情（含步骤日志/最终报告/评审）。
pub(crate) async fn get_member(
    State(_state): State<HttpServerState>,
    Path(member_id): Path<String>,
) -> Response {
    match runner_or_unavailable() {
        Ok(runner) => Json(runner.get(&member_id).await).into_response(),
        Err(resp) => resp,
    }
}

/// `POST /api/members/{member_id}/stop` — 中止工单（看板按钮）。
pub(crate) async fn stop_member(
    State(_state): State<HttpServerState>,
    Path(member_id): Path<String>,
) -> Response {
    match runner_or_unavailable() {
        Ok(runner) => {
            let stopped = runner.stop(&member_id).await;
            Json(json!({
                "member_id": member_id,
                "status": if stopped { "cancelled" } else { "not_running" },
            }))
            .into_response()
        }
        Err(resp) => resp,
    }
}

/// `GET /api/resource/patrol` — 资源巡检快照（C 盘水位 / scratch 配额 /
/// 停派门）。巡检未启动 = 服务未起 with_resource_patrol（理论只有测试态）。
pub(crate) async fn resource_patrol(State(state): State<HttpServerState>) -> Response {
    let Some(patrol) = &state.resource_patrol else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "resource_patrol_not_running",
                "dispatch_blocked": false,
            })),
        )
            .into_response();
    };
    let snap = patrol.snapshot();
    Json(json!({
        "dispatch_blocked": snap.dispatch_blocked(),
        "c_drive_low_free_bytes": snap.c_drive_low_free_bytes,
        "over_quota": snap.over_quota.iter().map(|(path, used, cap)| json!({
            "path": path.display().to_string(),
            "used_bytes": used,
            "cap_bytes": cap,
        })).collect::<Vec<_>>(),
    }))
    .into_response()
}

/// `GET /api/agent-roles` — 角色库（与 agent_role_list 工具同形）。
/// 不依赖 MemberRunner：角色库是纯文件面。
pub(crate) async fn list_roles(State(state): State<HttpServerState>) -> Response {
    let dir = state.hermix_home.join("agents");
    let _ = hermix_agent::member::roles::ensure_builtin_roles(&dir);
    let library = hermix_agent::member::roles::load_role_library(&dir);
    Json(json!({
        "roles": library.roles.iter().map(|r| json!({
            "name": r.name,
            "description": r.description,
            "worker": match &r.worker {
                hermix_agent::member::roles::MemberWorker::Internal => "internal",
                hermix_agent::member::roles::MemberWorker::External(d) => d,
                hermix_agent::member::roles::MemberWorker::Browser => "browser",
            },
            "toolset": r.toolset,
            "model": r.model,
            "version": r.version,
            "file": r.source_path.display().to_string(),
        })).collect::<Vec<_>>(),
        "errors": library.errors.iter().map(|(p, e)| json!({
            "file": p.display().to_string(),
            "error": e,
        })).collect::<Vec<_>>(),
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_state::empty_state;
    use axum::extract::State;
    use futures::StreamExt;

    #[tokio::test]
    async fn patrol_handler_degrades_without_patrol() {
        let state = empty_state();
        let resp = resource_patrol(State(state)).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn patrol_handler_reports_snapshot_shape() {
        let mut state = empty_state();
        let bus = crate::orchestration_bus::OrchestrationEventBus::new();
        let patrol = std::sync::Arc::new(
            crate::resource_patrol::ResourcePatrol::default_c_drive(bus),
        );
        patrol.tick_once().await;
        state.resource_patrol = Some(patrol);
        let resp = resource_patrol(State(state)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.expect("body");
        let v: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert!(v.get("dispatch_blocked").is_some());
        assert!(v.get("over_quota").is_some());
    }

    #[tokio::test]
    async fn roles_handler_lists_builtins_from_home() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = crate::test_state::test_state_with_home(tmp.path());
        let resp = list_roles(State(state)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.expect("body");
        let v: serde_json::Value = serde_json::from_slice(&body).expect("json");
        let names: Vec<&str> = v["roles"]
            .as_array()
            .expect("roles")
            .iter()
            .map(|r| r["name"].as_str().expect("name"))
            .collect();
        assert!(names.contains(&"reviewer") && names.contains(&"coder"), "names={names:?}");
    }

    #[tokio::test]
    async fn members_handlers_degrade_without_runner() {
        // 不安装 runner：冷启动语义 = 503 而非 500/panic。
        // 注意全局槽可能被其他测试装过 —— 这里只测「确实没有」的分支
        // 不可行（全局态），改为装一个临时 runner 后测空列表与 not_found。
        let tmp = tempfile::tempdir().expect("tempdir");
        let runner = std::sync::Arc::new(
            hermix_agent::member::runner::MemberRunner::new(
                hermix_agent::member::runner::MemberRunnerConfig {
                    base_config: hermix_agent::agent_config::AgentConfig::default(),
                    tool_registry: std::sync::Arc::new(hermix_agent::agent_loop::ToolRegistry::new()),
                    llm_provider: std::sync::Arc::new(NoopProvider),
                    hermix_home: tmp.path().to_path_buf(),
                    chunk_emitter: None,
                    roles_dir: tmp.path().join("agents"),
                },
            ),
        );
        hermix_agent::member::runner::install_member_runner(runner, false);
        let state = empty_state();
        let resp = list_members(State(state)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.expect("body");
        let v: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(v["members"].as_array().map(Vec::len), Some(0));

        let state2 = empty_state();
        let resp2 = get_member(State(state2), axum::extract::Path("member-none".into())).await;
        assert_eq!(resp2.status(), StatusCode::OK);
        let body2 = axum::body::to_bytes(resp2.into_body(), 64 * 1024).await.expect("body");
        let v2: serde_json::Value = serde_json::from_slice(&body2).expect("json");
        assert_eq!(v2["status"], "not_found");
    }

    /// 最小假 provider：成员列表/详情不触发 LLM 调用，永远用不上。
    struct NoopProvider;

    #[async_trait::async_trait]
    impl hermix_core::LlmProvider for NoopProvider {
        async fn chat_completion(
            &self,
            _m: &[hermix_core::Message],
            _t: &[hermix_core::ToolSchema],
            _mt: Option<u32>,
            _tp: Option<f64>,
            _mo: Option<&str>,
            _e: Option<&serde_json::Value>,
        ) -> Result<hermix_core::LlmResponse, hermix_core::AgentError> {
            Err(hermix_core::AgentError::LlmApi("noop".into()))
        }
        fn chat_completion_stream(
            &self,
            _m: &[hermix_core::Message],
            _t: &[hermix_core::ToolSchema],
            _mt: Option<u32>,
            _tp: Option<f64>,
            _mo: Option<&str>,
            _e: Option<&serde_json::Value>,
        ) -> futures::stream::BoxStream<'static, Result<hermix_core::StreamChunk, hermix_core::AgentError>> {
            futures::stream::empty().boxed()
        }
    }
}

/// `GET /api/member-stats` — 角色评分（P6）。
pub(crate) async fn member_stats(State(_state): State<HttpServerState>) -> Response {
    match runner_or_unavailable() {
        Ok(runner) => Json(json!({ "stats": runner.role_stats().await })).into_response(),
        Err(resp) => resp,
    }
}

/// 已知的本地缓存/膨胀目录（P0 散件：清单化治理）。只读扫描、只报不删
/// —— 删除是破坏性动作，由用户在资源管理器里决策。
fn known_cache_dirs() -> Vec<(&'static str, std::path::PathBuf)> {
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let user = std::env::var("USERPROFILE").unwrap_or_default();
    [
        ("npm 缓存", format!("{local}/npm-cache")),
        ("pnpm store", format!("{local}/pnpm/store")),
        ("pip 缓存", format!("{local}/pip/cache")),
        ("cargo registry", format!("{user}/.cargo/registry")),
        ("cargo git", format!("{user}/.cargo/git")),
        ("pnpm 缓存(家目录)", format!("{user}/AppData/Local/pnpm-cache")),
        ("npm 缓存(家目录)", format!("{user}/AppData/Local/npm-cache")),
        ("Temp", std::env::temp_dir().to_string_lossy().to_string()),
    ]
    .into_iter()
    .map(|(label, p)| (label, std::path::PathBuf::from(p)))
    .filter(|(_, p)| p.exists())
    .collect()
}

/// `GET /api/resource/caches` — 按需扫描缓存目录体积（30s 上限，
/// 大目录可能遍历秒级——这就是不放进 60s 巡检的原因）。
pub(crate) async fn scan_caches(State(_state): State<HttpServerState>) -> Response {
    let dirs = known_cache_dirs();
    let scan = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::task::spawn_blocking(move || {
            dirs.into_iter()
                .map(|(label, path)| {
                    let bytes = hermix_core::resource_governor::dir_size_bytes(&path);
                    json!({
                        "label": label,
                        "path": path.display().to_string(),
                        "bytes": bytes,
                    })
                })
                .collect::<Vec<_>>()
        }),
    )
    .await;
    match scan {
        Ok(Ok(items)) => Json(json!({ "caches": items })).into_response(),
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("cache scan panicked: {e}") })),
        )
            .into_response(),
        Err(_) => (
            StatusCode::REQUEST_TIMEOUT,
            Json(json!({ "error": "cache_scan_timeout", "message": "目录过大，30 秒内扫不完；请手动查看" })),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// 手机面（P4.5）：经 relay 的配对设备访问成员工单。能力门与 /v1/tasks
// 同款（Capability::Chat + 本设备可见性）；桌面 Local 也放行供调试。
// ---------------------------------------------------------------------------

/// `GET /v1/members` — 手机端工单列表。
pub(crate) async fn mobile_list_members(
    Extension(origin): Extension<hermix_core::Origin>,
    Extension(device_scopes): Extension<Option<std::collections::HashSet<hermix_core::Capability>>>,
    State(state): State<HttpServerState>,
) -> Response {
    if let Err(resp) = crate::routes::check_capability_from_extensions(
        hermix_core::Capability::Chat,
        &origin,
        &device_scopes,
    ) {
        return resp.into_response();
    }
    // 复用桌面端点的能力检查后走同一条数据路（成员数据无设备隔离语义
    // —— 工单是主机资源，配对设备皆可见，与任务列表的「只看自己」不同）。
    let _ = state;
    match runner_or_unavailable() {
        Ok(runner) => Json(json!({ "members": runner.list().await })).into_response(),
        Err(resp) => resp,
    }
}

/// `POST /v1/members/{member_id}/stop` — 手机端中止工单。
pub(crate) async fn mobile_stop_member(
    Extension(origin): Extension<hermix_core::Origin>,
    Extension(device_scopes): Extension<Option<std::collections::HashSet<hermix_core::Capability>>>,
    State(_state): State<HttpServerState>,
    Path(member_id): Path<String>,
) -> Response {
    if let Err(resp) = crate::routes::check_capability_from_extensions(
        hermix_core::Capability::Chat,
        &origin,
        &device_scopes,
    ) {
        return resp.into_response();
    }
    match runner_or_unavailable() {
        Ok(runner) => {
            let stopped = runner.stop(&member_id).await;
            Json(json!({
                "member_id": member_id,
                "status": if stopped { "cancelled" } else { "not_running" },
            }))
            .into_response()
        }
        Err(resp) => resp,
    }
}
