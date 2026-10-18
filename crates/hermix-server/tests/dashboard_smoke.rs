//! Smoke tests for the dashboard management API endpoints.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

/// Helper: build a default test app.
async fn test_app() -> axum::Router {
    let _ = tracing_subscriber::fmt::try_init();
    let cfg = hermix_config::GatewayConfig::default();
    let state = hermix_server::HttpServerState::build(cfg).await.unwrap();
    hermix_server::router(state)
}

/// Isolated Hermes home (no reliance on `HERMIX_HOME` env).
async fn test_app_with_hermes_home(home: std::path::PathBuf) -> axum::Router {
    let _ = tracing_subscriber::fmt::try_init();
    let cfg = hermix_config::GatewayConfig::default();
    let state = hermix_server::HttpServerState::build_with_hermes_home(cfg, home)
        .await
        .unwrap();
    hermix_server::router(state)
}

/// oneshot 不经过真实监听器，axum 不会自动注入 `ConnectInfo`；缺了它
/// `request_guard::client_ip` 返回 None → `derive_origin` 判 `Unpaired`
/// → T1 关口 403（本文件全部测试因此挂红，属 T1 关口落地时的
/// pre-existing 集成测试破损，CI gate 只跑 `--lib` 未覆盖）。注入
/// loopback 即 `Local` 来源，恢复测试本意。
fn local(req: Request<Body>) -> Request<Body> {
    let mut req = req;
    req.extensions_mut().insert(axum::extract::ConnectInfo(
        "127.0.0.1:41000"
            .parse::<std::net::SocketAddr>()
            .unwrap(),
    ));
    req
}

/// Helper: GET request and return (status, body_json).
async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let res = app
        .oneshot(local(
            Request::builder().uri(uri).body(Body::empty()).unwrap()))
        .await
        .unwrap();
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// PUT JSON body to `uri`, return (status, parsed JSON).
async fn put_json(
    app: axum::Router,
    uri: &str,
    payload: &serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let bytes = serde_json::to_vec(payload).unwrap();
    let res = app
        .oneshot(local(
            Request::builder()
                .method("PUT")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(bytes))
                .unwrap(),
        ))
        .await
        .unwrap();
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// POST with empty body, return (status, parsed JSON).
async fn post_json(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let res = app
        .oneshot(local(
            Request::builder()
                .method("POST")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    (status, json)
}

// ── /api/dashboard/plugins ───────────────────────────────────────────

#[tokio::test]
async fn dashboard_plugins_get_returns_shape() {
    let app = test_app().await;
    let (status, body) = get_json(app, "/api/dashboard/plugins").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["plugins"].is_object());
    assert!(body["persisted"].is_boolean());
    let plugins = body["plugins"].as_object().unwrap();
    assert!(plugins.contains_key("mcp_filesystem"));
    assert!(plugins.contains_key("tool_code_exec"));
}

#[tokio::test]
async fn dashboard_plugins_put_then_get_roundtrip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().to_path_buf();
    let app = test_app_with_hermes_home(home.clone()).await;

    let (s0, b0) = get_json(app.clone(), "/api/dashboard/plugins").await;
    assert_eq!(s0, StatusCode::OK);
    assert_eq!(b0["persisted"], false);

    let payload = serde_json::json!({
        "plugins": {
            "mcp_filesystem": false,
            "mcp_terminal": true,
            "mcp_database": false,
            "tool_code_exec": false
        }
    });
    let (s1, b1) = put_json(app.clone(), "/api/dashboard/plugins", &payload).await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(b1["ok"], true);
    assert_eq!(b1["persisted"], true);

    let plugin_file = home.join("dashboard_plugins.json");
    assert!(plugin_file.is_file(), "expected {}", plugin_file.display());

    let (s2, b2) = get_json(app, "/api/dashboard/plugins").await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(b2["persisted"], true);
    let plugins = b2["plugins"].as_object().unwrap();
    assert_eq!(plugins["mcp_filesystem"], false);
    assert_eq!(plugins["mcp_terminal"], true);
    assert_eq!(plugins["mcp_database"], false);
    assert_eq!(plugins["tool_code_exec"], false);
}

// ── /api/dashboard/themes & theme ───────────────────────────────────

#[tokio::test]
#[ignore = "pre-existing：/api/dashboard/themes 已被 P0 死端点砍除（b51a32a1：researcher / consciousness WS / themes），测试未同步更新；恢复路由会推翻 P0 砍除决策，待主题功能重立项时一并恢复测试"]
async fn dashboard_themes_get_returns_builtin_list() {
    let app = test_app().await;
    let (status, body) = get_json(app, "/api/dashboard/themes").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["active"].as_str().unwrap(), "default");
    assert!(body["persisted"].is_boolean());
    let themes = body["themes"].as_array().unwrap();
    assert!(themes.len() >= 7);
    assert!(themes.iter().any(|t| t["name"] == "midnight"));
}

#[tokio::test]
#[ignore = "pre-existing：/api/dashboard/theme 已被 P0 死端点砍除（b51a32a1），测试未同步更新；恢复路由会推翻 P0 砍除决策，待主题功能重立项时一并恢复测试"]
async fn dashboard_theme_put_then_get_roundtrip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().to_path_buf();
    let app = test_app_with_hermes_home(home.clone()).await;

    let (s1, b1) = put_json(
        app.clone(),
        "/api/dashboard/theme",
        &serde_json::json!({ "name": "midnight" }),
    )
    .await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(b1["ok"], true);
    assert_eq!(b1["theme"].as_str().unwrap(), "midnight");

    let theme_file = home.join("dashboard_theme.json");
    assert!(theme_file.is_file(), "expected {}", theme_file.display());

    let (s2, b2) = get_json(app, "/api/dashboard/themes").await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(b2["active"].as_str().unwrap(), "midnight");
    assert_eq!(b2["persisted"], true);
}

#[tokio::test]
#[ignore = "pre-existing：/api/dashboard/theme 已被 P0 死端点砍除（b51a32a1），测试未同步更新；恢复路由会推翻 P0 砍除决策，待主题功能重立项时一并恢复测试"]
async fn dashboard_theme_put_unknown_returns_400() {
    let app = test_app().await;
    let (status, body) = put_json(
        app,
        "/api/dashboard/theme",
        &serde_json::json!({ "name": "not-a-real-theme" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["ok"], false);
}

#[tokio::test]
async fn dashboard_plugin_manifests_returns_array() {
    let app = test_app().await;
    let (status, body) = get_json(app, "/api/dashboard/plugin-manifests").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_array());
    // The 18 official manifests are seeded on first read; we expect a
    // non-empty array. The list-and-rescan test below pins the exact
    // count when the test uses a hermix_home fixture with bundles on
    // disk. Here we just assert that seeding populated the endpoint.
    assert!(
        body.as_array().unwrap().len() >= 1,
        "expected at least one seeded manifest, got {}",
        body.as_array().unwrap().len()
    );
}

#[tokio::test]
async fn dashboard_plugin_manifests_and_rescan_use_disk_bundles() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().to_path_buf();
    let demo = home.join("dashboard-plugins").join("demo");
    std::fs::create_dir_all(&demo).expect("mkdir");
    std::fs::write(
        demo.join("manifest.json"),
        r#"{"name":"demo","label":"Demo","description":"test","icon":"🔗","version":"0.0.1","tab":{"path":"/","position":"sidebar"},"entry":"main.js","has_api":false,"source":"local"}"#,
    )
    .expect("write manifest");

    let app = test_app_with_hermes_home(home).await;

    let (st, body) = get_json(app.clone(), "/api/dashboard/plugin-manifests").await;
    assert_eq!(st, StatusCode::OK);
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"].as_str().unwrap(), "demo");

    let (st2, b2) = post_json(app.clone(), "/api/dashboard/plugins/rescan").await;
    assert_eq!(st2, StatusCode::OK);
    assert_eq!(b2["ok"], true);
    assert_eq!(b2["count"], 1);

    let (st3, mf) = get_json(app, "/dashboard-plugins/demo/manifest.json").await;
    assert_eq!(st3, StatusCode::OK);
    assert_eq!(mf["name"].as_str().unwrap(), "demo");
}

// ── /api/sessions ───────────────────────────────────────────────────

#[tokio::test]
async fn sessions_list_returns_paginated() {
    let app = test_app().await;
    let (status, body) = get_json(app, "/v1/sessions").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["sessions"].is_array());
    assert!(body["total"].is_number());
}

#[tokio::test]
async fn session_messages_returns_empty_for_unknown() {
    let app = test_app().await;
    let (status, body) = get_json(app, "/v1/sessions/unknown-session/messages").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["session_id"].as_str().unwrap(), "unknown-session");
    assert!(body["messages"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn session_search_returns_results_array() {
    let app = test_app().await;
    let (status, body) = get_json(app, "/v1/sessions/search?q=hello").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["results"].is_array());
}

// ── /api/config ─────────────────────────────────────────────────────

#[tokio::test]
async fn config_get_returns_json() {
    let app = test_app().await;
    let (status, body) = get_json(app, "/api/config").await;
    assert_eq!(status, StatusCode::OK);
    // Should be a JSON object (the serialized GatewayConfig)
    assert!(body.is_object());
}

// ── /api/model/info ─────────────────────────────────────────────────

#[tokio::test]
async fn model_info_returns_provider_and_model() {
    let app = test_app().await;
    let (status, body) = get_json(app, "/api/model/info").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["model"].is_string());
    assert!(body["provider"].is_string());
    assert!(body["effective_context_length"].is_number());
    assert!(body["capabilities"].is_object());
}

// ── /api/env ────────────────────────────────────────────────────────

#[tokio::test]
async fn env_vars_returns_known_keys() {
    let app = test_app().await;
    let (status, body) = get_json(app, "/api/env").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_object());
    // Should contain at least OPENAI_API_KEY
    assert!(body["OPENAI_API_KEY"].is_object());
    assert!(body["ANTHROPIC_API_KEY"].is_object());
    // Each entry should have expected fields
    let openai = &body["OPENAI_API_KEY"];
    assert!(openai["is_set"].is_boolean());
    assert!(openai["description"].is_string());
    assert!(openai["category"].is_string());
}

// ── /api/skills ─────────────────────────────────────────────────────

#[tokio::test]
async fn skills_list_returns_array() {
    let app = test_app().await;
    let (status, body) = get_json(app, "/api/skills").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_array());
}

// ── /api/tools/toolsets ─────────────────────────────────────────────

#[tokio::test]
async fn toolsets_list_returns_array() {
    let app = test_app().await;
    let (status, body) = get_json(app, "/api/tools/toolsets").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_array());
    // Should have registered builtin tools
    let arr = body.as_array().unwrap();
    assert!(
        !arr.is_empty(),
        "toolsets should not be empty after builtin registration"
    );
}

// ── /api/analytics/usage ────────────────────────────────────────────

#[tokio::test]
async fn analytics_usage_returns_structure() {
    let app = test_app().await;
    let (status, body) = get_json(app, "/api/analytics/usage").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["daily"].is_array());
    assert!(body["by_model"].is_array());
    assert!(body["totals"].is_object());
    assert!(body["skills"].is_object());
}

// ── DELETE /api/sessions/{id} ───────────────────────────────────────

#[tokio::test]
async fn delete_session_returns_ok() {
    let app = test_app().await;
    let res = app
        .oneshot(local(
            Request::builder()
                .method("DELETE")
                .uri("/api/sessions/nonexistent-session")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    // Even for non-existent session, delete should succeed gracefully
    // (the DB may not exist, but the handler should not panic)
    let status = res.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::NOT_FOUND,
        "unexpected status: {}",
        status
    );
}

async fn post_json_body(
    app: axum::Router,
    uri: &str,
    payload: &serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let bytes = serde_json::to_vec(payload).unwrap();
    let res = app
        .oneshot(local(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(bytes))
                .unwrap(),
        ))
        .await
        .unwrap();
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// DELETE and return (status, parsed JSON).
async fn delete_json(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let res = app
        .oneshot(local(
            Request::builder()
                .method("DELETE")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn mcp_servers_list_empty_on_fresh_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = test_app_with_hermes_home(dir.path().to_path_buf()).await;
    let (status, body) = get_json(app, "/api/mcp/servers").await;
    assert_eq!(status, StatusCode::OK);
    // Official seeds were removed: a fresh install starts with no entries.
    let servers = body["servers"].as_array().expect("servers array");
    assert!(servers.is_empty(), "fresh state must have no servers");
    assert_eq!(body["seeded"], true);
}

#[tokio::test]
async fn mcp_servers_create_then_get() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = test_app_with_hermes_home(dir.path().to_path_buf()).await;
    let payload = serde_json::json!({
        "id": "user-notion",
        "name": "Notion (User)",
        "transport": "http",
        "url": "https://mcp.notion.so/sse",
        "enabled": true,
        "description": "Custom user-added Notion bridge",
        "created_at": "2026-06-02T00:00:00Z",
    });
    let (status, body) = post_json_body(app.clone(), "/api/mcp/servers", &payload).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "user-notion");
    assert_eq!(body["name"], "Notion (User)");
    assert_eq!(body["transport"], "http");

    let (_, list) = get_json(app, "/api/mcp/servers").await;
    let servers = list["servers"].as_array().unwrap();
    assert!(servers.iter().any(|s| s["id"] == "user-notion"));
    assert_eq!(servers.len(), 1); // no seeds + 1 user entry
}

#[tokio::test]
async fn mcp_servers_create_rejects_duplicate_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = test_app_with_hermes_home(dir.path().to_path_buf()).await;
    let payload = serde_json::json!({
        "id": "user-notion",
        "name": "Notion (User)",
        "transport": "http",
        "url": "https://mcp.notion.so/sse",
        "enabled": true,
        "created_at": "2026-06-02T00:00:00Z",
    });
    let (status, _body) = post_json_body(app.clone(), "/api/mcp/servers", &payload).await;
    assert_eq!(status, StatusCode::OK);
    // Same id again must not overwrite the existing entry.
    let (status, _body) = post_json_body(app, "/api/mcp/servers", &payload).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn mcp_servers_update_then_get() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = test_app_with_hermes_home(dir.path().to_path_buf()).await;
    let payload = serde_json::json!({
        "id": "user-notion",
        "name": "Notion (User)",
        "transport": "http",
        "url": "https://mcp.notion.so/sse",
        "enabled": true,
        "created_at": "2026-06-02T00:00:00Z",
    });
    let (status, _) = post_json_body(app.clone(), "/api/mcp/servers", &payload).await;
    assert_eq!(status, StatusCode::OK);

    let patch = serde_json::json!({ "enabled": false, "description": "Disabled for testing" });
    let (status, body) = put_json(app.clone(), "/api/mcp/servers/user-notion", &patch).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], false);
    assert_eq!(body["description"], "Disabled for testing");

    let (_, list) = get_json(app, "/api/mcp/servers").await;
    let entry = list["servers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "user-notion")
        .unwrap();
    assert_eq!(entry["enabled"], false);
}

#[tokio::test]
async fn mcp_servers_delete_then_get() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = test_app_with_hermes_home(dir.path().to_path_buf()).await;
    let payload = serde_json::json!({
        "id": "user-temp",
        "name": "Temp (User)",
        "transport": "http",
        "url": "https://example.com/sse",
        "enabled": true,
        "created_at": "2026-06-02T00:00:00Z",
    });
    let (status, _) = post_json_body(app.clone(), "/api/mcp/servers", &payload).await;
    assert_eq!(status, StatusCode::OK);

    let (status, _body) = delete_json(app.clone(), "/api/mcp/servers/user-temp").await;
    assert_eq!(status, StatusCode::OK);
    let (_, list) = get_json(app, "/api/mcp/servers").await;
    let servers = list["servers"].as_array().unwrap();
    assert!(servers.is_empty());
    assert!(!servers.iter().any(|s| s["id"] == "user-temp"));
}

#[tokio::test]
async fn mcp_servers_delete_unknown_returns_404() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = test_app_with_hermes_home(dir.path().to_path_buf()).await;
    let (status, _body) = delete_json(app, "/api/mcp/servers/does-not-exist").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn mcp_servers_reset_clears_user_entries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = test_app_with_hermes_home(dir.path().to_path_buf()).await;
    let payload = serde_json::json!({
        "id": "user-temp",
        "name": "Temp (User)",
        "transport": "http",
        "url": "https://example.com/sse",
        "enabled": true,
        "created_at": "2026-06-02T00:00:00Z",
    });
    let (status, _) = post_json_body(app.clone(), "/api/mcp/servers", &payload).await;
    assert_eq!(status, StatusCode::OK);

    // Reset wipes user entries; official seeds no longer exist, so the
    // state returns to an empty list.
    let (status, body) = post_json(app.clone(), "/api/mcp/servers/reset").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["seeded"], true);
    let servers = body["servers"].as_array().unwrap();
    assert!(servers.is_empty());
}

// ── /api/automation/tasks ───────────────────────────────────────────

#[tokio::test]
async fn automation_tasks_seeds_six_demos_on_first_list() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = test_app_with_hermes_home(dir.path().to_path_buf()).await;
    let (status, body) = get_json(app, "/api/automation/tasks").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["seeded"], true);
    let tasks = body["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 6, "expected 6 demo templates");
    // Each demo should have category + schedule set; enabled should default false.
    for t in tasks {
        assert!(t["category"].is_string(), "demo must carry category");
        assert!(t["schedule"].is_string(), "demo must carry cron schedule");
        assert_eq!(t["enabled"], false, "demos must default to disabled");
        // Disabled tasks correctly have no `next_run_at` — the scheduler
        // only arms enabled tasks.
        assert_eq!(
            t["next_run_at"],
            serde_json::Value::Null,
            "disabled demo must not have next_run_at"
        );
    }
}

#[tokio::test]
async fn automation_tasks_create_validates_schedule() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = test_app_with_hermes_home(dir.path().to_path_buf()).await;
    let bad = serde_json::json!({
        "id": "test-bad",
        "name": "无效 schedule",
        "prompt": "hi",
        "schedule": "not a cron expr",
        "enabled": true,
    });
    let (status, _) = post_json_body(app, "/api/automation/tasks", &bad).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn automation_tasks_create_validates_required_fields() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = test_app_with_hermes_home(dir.path().to_path_buf()).await;
    let no_name = serde_json::json!({
        "id": "t1",
        "name": "",
        "prompt": "hi",
        "schedule": "0 8 * * *",
        "enabled": true,
    });
    let (status, _) = post_json_body(app.clone(), "/api/automation/tasks", &no_name).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let no_prompt = serde_json::json!({
        "id": "t2",
        "name": "n",
        "prompt": "",
        "schedule": "0 8 * * *",
        "enabled": true,
    });
    let (status, _) = post_json_body(app, "/api/automation/tasks", &no_prompt).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn automation_tasks_create_then_get_then_update_then_delete() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = test_app_with_hermes_home(dir.path().to_path_buf()).await;
    let payload = serde_json::json!({
        "id": "roundtrip",
        "name": "测试任务",
        "description": "test",
        "prompt": "请 echo hello",
        "schedule": "*/15 * * * *",
        "schedule_label": "每 15 分钟",
        "enabled": true,
        "category": "工作",
        "icon": "🧪",
        "model": "openai:gpt-4o",
    });
    let (status, body) = post_json_body(app.clone(), "/api/automation/tasks", &payload).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "roundtrip");
    assert_eq!(body["name"], "测试任务");
    assert_eq!(body["run_count"], 0);
    assert!(body["next_run_at"].is_string(), "enabled task must compute next_run_at");

    // Duplicate id should conflict
    let (status, _) = post_json_body(app.clone(), "/api/automation/tasks", &payload).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Update
    let updated = serde_json::json!({
        "id": "roundtrip",
        "name": "改后的名字",
        "prompt": "请 echo world",
        "schedule": "0 12 * * *",
        "enabled": false,
    });
    let (status, body) = put_json(app.clone(), "/api/automation/tasks/roundtrip", &updated).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "改后的名字");
    assert_eq!(body["enabled"], false);
    assert_eq!(body["next_run_at"], serde_json::Value::Null, "disabled task should clear next_run_at");

    // Delete
    let (status, _) = delete_json(app.clone(), "/api/automation/tasks/roundtrip").await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // List should not contain roundtrip anymore
    let (_, list) = get_json(app, "/api/automation/tasks").await;
    let tasks = list["tasks"].as_array().unwrap();
    assert!(!tasks.iter().any(|t| t["id"] == "roundtrip"));
}

#[tokio::test]
async fn automation_tasks_update_unknown_returns_404() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = test_app_with_hermes_home(dir.path().to_path_buf()).await;
    let payload = serde_json::json!({
        "id": "ghost",
        "name": "x",
        "prompt": "y",
        "schedule": "0 8 * * *",
        "enabled": true,
    });
    let (status, _) = put_json(app, "/api/automation/tasks/ghost", &payload).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn automation_tasks_delete_unknown_returns_404() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = test_app_with_hermes_home(dir.path().to_path_buf()).await;
    let (status, _) = delete_json(app, "/api/automation/tasks/ghost").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn automation_tasks_reset_restores_demos_keeps_user() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = test_app_with_hermes_home(dir.path().to_path_buf()).await;
    // Add a custom task
    let custom = serde_json::json!({
        "id": "mine",
        "name": "我自己加的",
        "prompt": "ping",
        "schedule": "0 8 * * *",
        "enabled": true,
    });
    let (status, _) = post_json_body(app.clone(), "/api/automation/tasks", &custom).await;
    assert_eq!(status, StatusCode::OK);
    // Delete a demo to dirty the seed set
    let (status, _) = delete_json(app.clone(), "/api/automation/tasks/demo-morning-brief").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // Reset
    let (status, body) = post_json(app.clone(), "/api/automation/reset").await;
    assert_eq!(status, StatusCode::OK);
    let tasks = body["tasks"].as_array().unwrap();
    assert!(tasks.iter().any(|t| t["id"] == "mine"), "user task preserved");
    assert!(tasks.iter().any(|t| t["id"] == "demo-morning-brief"), "demo restored");
    assert_eq!(tasks.len(), 7, "6 demos + 1 user");
}

// ── /api/dashboard/plugin-inspect ───────────────────────────

#[tokio::test]
async fn plugin_inspect_returns_empty_for_unknown_id() {
    let app = test_app().await;
    let (status, body) = get_json(app, "/api/dashboard/plugin-inspect/nonexistent").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "nonexistent");
    assert_eq!(body["availability"], "seeded");
    assert_eq!(body["total"], 0);
    assert!(body["tools"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn plugin_inspect_all_returns_at_least_one_ready_toolset() {
    let app = test_app().await;
    let (status, body) = get_json(app, "/api/dashboard/plugin-inspect").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_object(), "expected map of plugin_id -> inspect");
    // `register_builtin_tools` registers at least: web, terminal, files,
    // image_gen, video, audio, media, skills, ... — so the response
    // should have at least one entry.
    let map = body.as_object().unwrap();
    let n = map.len();
    assert!(n >= 1, "expected at least one registered toolset, got {n}");
    // Every entry should have availability=ready, since we only emit
    // entries for toolsets that have at least one tool.
    for (_id, v) in map {
        assert_eq!(v["availability"], "ready");
        assert!(v["total"].as_u64().unwrap() >= 1);
    }
}

#[tokio::test]
async fn plugin_inspect_web_returns_web_search_and_extract() {
    let app = test_app().await;
    let (status, body) = get_json(app, "/api/dashboard/plugin-inspect/web").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["availability"], "ready");
    let tools = body["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|v| v.as_str().unwrap()).collect();
    assert!(
        names.contains(&"web_search"),
        "expected web_search in web toolset, got {names:?}"
    );
    assert!(
        names.contains(&"web_extract"),
        "expected web_extract in web toolset, got {names:?}"
    );
}

// ── /api/sessions/{id}/undo + /restore ──────────────────────────────

#[tokio::test]
async fn session_undo_default_count_soft_deletes_last_turn() {
    use hermix_agent::session_persistence::SessionPersistence;
    use hermix_core::Message;

    let dir = tempfile::tempdir().expect("tempdir");
    let app = test_app_with_hermes_home(dir.path().to_path_buf()).await;

    // Seed a 3-turn conversation straight into SQLite.
    let sp = SessionPersistence::new(dir.path());
    let messages = vec![
        Message::user("turn-1 user"),
        Message::assistant("turn-1 assistant"),
        Message::user("turn-2 user"),
        Message::assistant("turn-2 assistant"),
        Message::user("turn-3 user"),
        Message::assistant("turn-3 assistant"),
    ];
    sp.persist_session("undo-http", &messages, None, None, None, None, None, None, None)
        .unwrap();

    // /undo (no count param) → default 1 turn.
    let (status, body) = post_json(app.clone(), "/v1/sessions/undo-http/undo").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert_eq!(body["deactivated"], 2);
    assert_eq!(body["turns_requested"], 1);

    // /api/sessions/undo-http/messages should now return only the first 4
    // messages (turn-1 and turn-2).
    let (_, list) = get_json(app, "/v1/sessions/undo-http/messages").await;
    let msgs = list["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 4);
    assert_eq!(msgs[3]["content"], "turn-2 assistant");
}

#[tokio::test]
async fn session_undo_with_count_query_string() {
    use hermix_agent::session_persistence::SessionPersistence;
    use hermix_core::Message;

    let dir = tempfile::tempdir().expect("tempdir");
    let app = test_app_with_hermes_home(dir.path().to_path_buf()).await;

    let sp = SessionPersistence::new(dir.path());
    sp.persist_session(
        "undo-http-2",
        &[
            Message::user("t1u"),
            Message::assistant("t1a"),
            Message::user("t2u"),
            Message::assistant("t2a"),
            Message::user("t3u"),
            Message::assistant("t3a"),
        ],
        None,
        None,
        None,
        None,
        None,
        // workspace / expert_id: no binding for this undo smoke test.
        None,
        None,
    )
    .unwrap();

    let (status, body) = post_json(
        app.clone(),
        "/v1/sessions/undo-http-2/undo?count=2",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["deactivated"], 4);
    assert_eq!(body["turns_requested"], 2);

    let (_, list) = get_json(app, "/v1/sessions/undo-http-2/messages").await;
    assert_eq!(list["messages"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn session_restore_brings_back_most_recent_block() {
    use hermix_agent::session_persistence::SessionPersistence;
    use hermix_core::Message;

    let dir = tempfile::tempdir().expect("tempdir");
    let app = test_app_with_hermes_home(dir.path().to_path_buf()).await;

    let sp = SessionPersistence::new(dir.path());
    sp.persist_session(
        "undo-http-3",
        &[
            Message::user("a"),
            Message::assistant("b"),
            Message::user("c"),
            Message::assistant("d"),
        ],
        None,
        None,
        None,
        None,
        None,
        // workspace / expert_id: no binding for this restore smoke test.
        None,
        None,
    )
    .unwrap();

    post_json(app.clone(), "/v1/sessions/undo-http-3/undo").await;
    let (_, list1) = get_json(app.clone(), "/v1/sessions/undo-http-3/messages").await;
    assert_eq!(list1["messages"].as_array().unwrap().len(), 2);

    let (status, body) = post_json(app.clone(), "/v1/sessions/undo-http-3/restore").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["restored"], 2);

    let (_, list2) = get_json(app, "/v1/sessions/undo-http-3/messages").await;
    assert_eq!(list2["messages"].as_array().unwrap().len(), 4);
}

#[tokio::test]
async fn session_undo_on_empty_session_returns_zero() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = test_app_with_hermes_home(dir.path().to_path_buf()).await;
    let (status, body) = post_json(app, "/v1/sessions/never-existed/undo").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["deactivated"], 0);
}

// ── Builtin skill seeding ────────────────────────────────────────────

#[test]
fn seed_builtin_skills_writes_all_builtins_to_fresh_dir() {
    let dir = tempfile::tempdir().expect("tempdir");
    let skills_dir = dir.path().to_path_buf();

    let written = hermix_server::dashboard::skills::seed_builtin_skills(&skills_dir)
        .expect("seed should succeed on a fresh dir");

    // 期望值从 BUILTIN_SKILLS 取，不写死数字——加技能时这里自动跟着走。
    let expected = hermix_server::dashboard::skills::builtin_skills_len();
    assert_eq!(written, expected);

    // 每一份都应落盘到 <category>/<name>/SKILL.md
    let entries: Vec<_> = walkdir::WalkDir::new(&skills_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name() == "SKILL.md")
        .collect();
    assert_eq!(entries.len(), expected);

    // Spot-check: the plan-writing skill lands in productivity/
    let planning = skills_dir.join("productivity").join("plan-writing").join("SKILL.md");
    assert!(planning.exists(), "plan-writing should be seeded at {:?}", planning);
    let content = std::fs::read_to_string(&planning).unwrap();
    assert!(content.contains("plan-writing"), "frontmatter should be intact");
}

#[test]
fn seed_builtin_skills_is_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let skills_dir = dir.path().to_path_buf();

    let first = hermix_server::dashboard::skills::seed_builtin_skills(&skills_dir)
        .expect("first seed should succeed");
    assert_eq!(first, hermix_server::dashboard::skills::builtin_skills_len());

    // Mutate one file to prove the second call doesn't clobber it.
    let target = skills_dir.join("productivity").join("plan-writing").join("SKILL.md");
    std::fs::write(&target, "USER CUSTOM CONTENT").unwrap();

    let second = hermix_server::dashboard::skills::seed_builtin_skills(&skills_dir)
        .expect("second seed should succeed");
    assert_eq!(second, 0, "idempotent: nothing should be written the second time");

    let content = std::fs::read_to_string(&target).unwrap();
    assert_eq!(content, "USER CUSTOM CONTENT", "user edits must be preserved");
}

// ── seed_one upgrade semantics ──────────────────────────────────────
// The hash sidecar (.seed-meta.json) distinguishes a stock copy (safe to
// upgrade in place when the bundled builtin changes) from user content
// (preserved; the new builtin is staged as SKILL.md.new). These tests pin
// the four SeedOutcome branches plus the legacy-install adoption path.

use hermix_server::dashboard::skills::{seed_one, SeedOutcome};

const CAT: &str = "design";
const NAME: &str = "probe-skill";
const V1: &str = "---\nname: probe-skill\n---\nversion one body\n";
const V2: &str = "---\nname: probe-skill\n---\nversion two body\n";

fn probe_dir(skills_dir: &std::path::Path) -> std::path::PathBuf {
    skills_dir.join(CAT).join(NAME)
}

#[test]
fn seed_one_upgrades_stock_copy_when_builtin_changes() {
    let dir = tempfile::tempdir().unwrap();
    let skills = dir.path().to_path_buf();

    assert_eq!(seed_one(&skills, CAT, NAME, V1).unwrap(), SeedOutcome::Fresh);
    // Same content again → AlreadyCurrent, no rewrite.
    assert_eq!(seed_one(&skills, CAT, NAME, V1).unwrap(), SeedOutcome::AlreadyCurrent);
    // Bundled content changes in a release → stock copy refreshes in place.
    assert_eq!(seed_one(&skills, CAT, NAME, V2).unwrap(), SeedOutcome::Updated);
    let upgraded = std::fs::read_to_string(probe_dir(&skills).join("SKILL.md")).unwrap();
    assert_eq!(upgraded, V2, "unmodified stock copy must follow the builtin");
    // No stray staged file on the clean-upgrade path.
    assert!(!probe_dir(&skills).join("SKILL.md.new").exists());
}

#[test]
fn seed_one_preserves_user_edit_and_stages_new() {
    let dir = tempfile::tempdir().unwrap();
    let skills = dir.path().to_path_buf();

    seed_one(&skills, CAT, NAME, V1).unwrap();
    let path = probe_dir(&skills).join("SKILL.md");
    std::fs::write(&path, "USER CUSTOM CONTENT").unwrap();

    assert_eq!(seed_one(&skills, CAT, NAME, V2).unwrap(), SeedOutcome::PreservedUserEdit);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "USER CUSTOM CONTENT",
        "user edits must never be clobbered"
    );
    assert_eq!(
        std::fs::read_to_string(probe_dir(&skills).join("SKILL.md.new")).unwrap(),
        V2,
        "new builtin must be staged next to the user copy"
    );
    // Re-seeding with the same staged content must not rewrite the file
    // (idempotent PreservedUserEdit).
    let staged = probe_dir(&skills).join("SKILL.md.new");
    let mtime = std::fs::metadata(&staged).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert_eq!(seed_one(&skills, CAT, NAME, V2).unwrap(), SeedOutcome::PreservedUserEdit);
    assert_eq!(std::fs::metadata(&staged).unwrap().modified().unwrap(), mtime);
}

#[test]
fn seed_one_adopts_legacy_install_without_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let skills = dir.path().to_path_buf();

    // Pre-sidecar install: SKILL.md exists with exactly the builtin
    // content but no .seed-meta.json (older app version wrote it).
    let probe = probe_dir(&skills);
    std::fs::create_dir_all(&probe).unwrap();
    std::fs::write(probe.join("SKILL.md"), V1).unwrap();

    assert_eq!(seed_one(&skills, CAT, NAME, V1).unwrap(), SeedOutcome::AlreadyCurrent);
    // Adoption records the hash — a future changed builtin can now upgrade.
    assert_eq!(seed_one(&skills, CAT, NAME, V2).unwrap(), SeedOutcome::Updated);

    // Legacy install that was hand-edited (differs from builtin, no
    // sidecar) must be treated as user content, not upgraded over.
    let dir2 = tempfile::tempdir().unwrap();
    let skills2 = dir2.path().to_path_buf();
    let probe2 = probe_dir(&skills2);
    std::fs::create_dir_all(&probe2).unwrap();
    std::fs::write(probe2.join("SKILL.md"), "hand written").unwrap();
    assert_eq!(seed_one(&skills2, CAT, NAME, V1).unwrap(), SeedOutcome::PreservedUserEdit);
    assert_eq!(std::fs::read_to_string(probe2.join("SKILL.md")).unwrap(), "hand written");
}

#[test]
fn seed_one_fresh_after_delete_restores_builtin() {
    let dir = tempfile::tempdir().unwrap();
    let skills = dir.path().to_path_buf();

    seed_one(&skills, CAT, NAME, V1).unwrap();
    std::fs::remove_file(probe_dir(&skills).join("SKILL.md")).unwrap();
    // Deleting the directory remains the documented "restore builtin" path.
    assert_eq!(seed_one(&skills, CAT, NAME, V2).unwrap(), SeedOutcome::Fresh);
    assert_eq!(std::fs::read_to_string(probe_dir(&skills).join("SKILL.md")).unwrap(), V2);
}

#[tokio::test]
async fn list_skills_endpoint_returns_seeded_skills() {
    let dir = tempfile::tempdir().expect("tempdir");
    let skills_dir = dir.path().join("skills");
    hermix_server::dashboard::skills::seed_builtin_skills(&skills_dir).unwrap();

    // Build a server state whose skill store points at the temp dir.
    use hermix_skills::FileSkillStore;
    use std::sync::Arc;
    let store: Arc<dyn hermix_skills::SkillStore> = Arc::new(FileSkillStore::new(skills_dir.clone()));
    let cfg = hermix_config::GatewayConfig::default();
    let mut state = hermix_server::HttpServerState::build_with_hermes_home(cfg, dir.path().to_path_buf())
        .await
        .unwrap();
    state.skill_store = Some(store);

    let app = hermix_server::router(state);
    let (status, body) = get_json(app, "/api/skills").await;
    assert_eq!(status, StatusCode::OK);
    let arr = body.as_array().expect("response should be an array");
    assert_eq!(
        arr.len(),
        hermix_server::dashboard::skills::builtin_skills_len(),
        "落盘的内置技能都该出现在 /api/skills"
    );

    // Spot-check：取一条确实还在内置清单里的技能。
    // （原来还断言了 `web-novel`，那是已下线的内置技能，全仓只剩这一处引用。）
    let names: Vec<&str> = arr.iter().map(|s| s["name"].as_str().unwrap_or("")).collect();
    assert!(names.contains(&"plan-writing"));
}

// ── /api/persona/* — file-backed persona management ─────────────────
// Each test uses a fresh temp HERMIX_HOME so file writes don't leak
// between tests. The test app is a one-shot Router clone, so each
// `oneshot` call is independent. The shared `delete_json` helper above
// is reused for delete requests.

#[tokio::test]
async fn persona_soul_get_returns_builtin_when_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = test_app_with_hermes_home(dir.path().to_path_buf()).await;
    let (status, body) = get_json(app, "/api/persona/soul").await;
    assert_eq!(status, StatusCode::OK);
    // 文件缺失时回退到内置中文身份,不再返回 null
    assert_eq!(
        body["content"].as_str(),
        Some(hermix_agent::context::DEFAULT_AGENT_IDENTITY)
    );
    assert!(body["path"].as_str().unwrap().ends_with("SOUL.md"));
}

#[tokio::test]
async fn persona_soul_put_then_get_roundtrip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().to_path_buf();
    let app = test_app_with_hermes_home(home.clone()).await;

    let payload = serde_json::json!({ "content": "# My Soul\nBe kind, be brief, be useful." });
    let (s1, b1) = put_json(app.clone(), "/api/persona/soul", &payload).await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(b1["content"], "# My Soul\nBe kind, be brief, be useful.");
    let written = home.join("SOUL.md");
    assert!(written.is_file(), "expected {}", written.display());

    let (s2, b2) = get_json(app, "/api/persona/soul").await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(b2["content"], "# My Soul\nBe kind, be brief, be useful.");
    assert!(b2["updated_at"].is_number());
}

#[tokio::test]
async fn persona_memory_and_user_roundtrip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().to_path_buf();
    let app = test_app_with_hermes_home(home.clone()).await;

    // memory
    let mem = serde_json::json!({ "content": "user prefers concise answers" });
    let (s1, _) = put_json(app.clone(), "/api/persona/memory", &mem).await;
    assert_eq!(s1, StatusCode::OK);
    let (s2, b2) = get_json(app.clone(), "/api/persona/memory").await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(b2["content"], "user prefers concise answers");

    // user
    let user = serde_json::json!({ "content": "Name: Spiker\nRole: Logistics" });
    let (s3, _) = put_json(app.clone(), "/api/persona/user", &user).await;
    assert_eq!(s3, StatusCode::OK);
    let (s4, b4) = get_json(app, "/api/persona/user").await;
    assert_eq!(s4, StatusCode::OK);
    assert_eq!(b4["content"], "Name: Spiker\nRole: Logistics");

    assert!(home.join("memories").join("MEMORY.md").is_file());
    assert!(home.join("memories").join("USER.md").is_file());
}

#[tokio::test]
async fn persona_personalities_list_includes_builtins() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = test_app_with_hermes_home(dir.path().to_path_buf()).await;
    let (status, body) = get_json(app, "/api/persona/personalities").await;
    assert_eq!(status, StatusCode::OK);
    let arr = body.as_array().expect("array");
    let names: Vec<&str> = arr.iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"coder"));
    assert!(names.contains(&"writer"));
    assert!(names.contains(&"analyst"));
    for p in arr {
        if ["coder", "writer", "analyst"].contains(&p["name"].as_str().unwrap()) {
            assert_eq!(p["is_builtin"], true);
            assert!(p["content"].is_null() || p["summary"].is_string());
        }
    }
}

#[tokio::test]
async fn persona_personality_get_builtin() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = test_app_with_hermes_home(dir.path().to_path_buf()).await;
    let (status, body) = get_json(app, "/api/persona/personalities/coder").await;
    assert_eq!(status, StatusCode::OK);
    let content = body["content"].as_str().expect("content");
    assert!(content.contains("工程师"));
}

#[tokio::test]
async fn persona_personality_create_update_delete_roundtrip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().to_path_buf();
    let app = test_app_with_hermes_home(home.clone()).await;

    // create
    let payload = serde_json::json!({ "content": "# finance planner\nYou are a careful financial advisor." });
    let (s1, b1) = put_json(app.clone(), "/api/persona/personalities/finance-planner", &payload).await;
    assert_eq!(s1, StatusCode::OK);
    assert!(b1["content"].as_str().unwrap().contains("financial advisor"));
    assert!(home.join("personalities").join("finance-planner.md").is_file());

    // list should now include it
    let (s2, b2) = get_json(app.clone(), "/api/persona/personalities").await;
    assert_eq!(s2, StatusCode::OK);
    let arr = b2.as_array().unwrap();
    let names: Vec<&str> = arr.iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"finance-planner"));

    // update
    let updated = serde_json::json!({ "content": "# finance planner v2" });
    let (s3, _) = put_json(app.clone(), "/api/persona/personalities/finance-planner", &updated).await;
    assert_eq!(s3, StatusCode::OK);
    let (s4, b4) = get_json(app.clone(), "/api/persona/personalities/finance-planner").await;
    assert_eq!(s4, StatusCode::OK);
    assert_eq!(b4["content"], "# finance planner v2");

    // delete
    let (s5, b5) = delete_json(app.clone(), "/api/persona/personalities/finance-planner").await;
    assert_eq!(s5, StatusCode::OK);
    assert_eq!(b5["deleted"], "finance-planner");
    assert!(!home.join("personalities").join("finance-planner.md").exists());

    // delete again should 404
    let (s6, _) = delete_json(app, "/api/persona/personalities/finance-planner").await;
    assert_eq!(s6, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn persona_personality_builtin_cannot_be_overwritten_or_deleted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = test_app_with_hermes_home(dir.path().to_path_buf()).await;

    // PUT builtin should be 403
    let payload = serde_json::json!({ "content": "hacked" });
    let (s1, _) = put_json(app.clone(), "/api/persona/personalities/coder", &payload).await;
    assert_eq!(s1, StatusCode::FORBIDDEN);

    // DELETE builtin should be 403
    let (s2, _) = delete_json(app, "/api/persona/personalities/coder").await;
    assert_eq!(s2, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn persona_personality_unsafe_slug_is_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = test_app_with_hermes_home(dir.path().to_path_buf()).await;

    // axum decodes path params before they reach the handler, so we URL-
    // encode the bad slugs and rely on the server's `is_safe_slug` check
    // to reject them. A path with `..` or `/` that the router can't match
    // would 404 at the routing layer instead — those are caught by
    // axum's path normalization, not by us.
    let bad_slugs = ["with%20space", "with.dot", "中文"];
    for slug in bad_slugs {
        let payload = serde_json::json!({ "content": "x" });
        let (s, _) = put_json(
            app.clone(),
            &format!("/api/persona/personalities/{}", slug),
            &payload,
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "slug should be rejected: {slug}");
    }

    // Sanity: a clean slug still works.
    let ok_payload = serde_json::json!({ "content": "x" });
    let (s, _) = put_json(
        app,
        "/api/persona/personalities/clean-slug-1",
        &ok_payload,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn persona_overrides_roundtrip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().to_path_buf();
    let app = test_app_with_hermes_home(home.clone()).await;

    // initial — missing file → defaults
    let (s0, b0) = get_json(app.clone(), "/api/persona/overrides").await;
    assert_eq!(s0, StatusCode::OK);
    assert_eq!(b0["system_prompt"], "");
    assert_eq!(b0["preferences"], serde_json::Value::Null);

    // write
    let payload = serde_json::json!({
        "system_prompt": "You are a careful assistant.",
        "preferences": {
            "default_language": "zh-CN",
            "verbosity": "concise"
        }
    });
    let (s1, _) = put_json(app.clone(), "/api/persona/overrides", &payload).await;
    assert_eq!(s1, StatusCode::OK);
    assert!(home.join("persona_overrides.json").is_file());

    // read back
    let (s2, b2) = get_json(app, "/api/persona/overrides").await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(b2["system_prompt"], "You are a careful assistant.");
    assert_eq!(b2["preferences"]["default_language"], "zh-CN");
    assert_eq!(b2["preferences"]["verbosity"], "concise");
}

// ── DELETE /v1/sessions/{id} — cascade to compression_* + fts ────────
// Regression for the bug: `delete_session` only deleted
// `messages` + `sessions`, so any session with a `compression_archives`
// row (which every long conversation accumulates after the first
// context-window overflow) failed with HTTP 500
// `FOREIGN KEY constraint failed` and the session was stuck in the
// Sidebar forever — the frontend's `handleDeleteSession` had no
// `try/catch`, so the user just saw a dead click.
// These tests use `test_app_with_hermes_home` so we can plant rows in
// the real `compression_archives` / `compression_summary_history` /
// `messages_fts` tables (which all have FKs to `sessions.id`) and then
// verify the DELETE truly cascades.

/// Insert a session row + 1 row in *every* related table that
/// FK-anchors back to `sessions.id`. Returns the session id.
fn plant_session_with_all_children(home: &std::path::Path) -> String {
    use rusqlite::{params, Connection};
    let db_path = hermix_config::paths::data_dir_in(home).join("sessions.db");
    // Use the real SessionPersistence::new so the schema (FK + FTS5
    // triggers) is laid out exactly the way production sees it.
    let _ = hermix_agent::session_persistence::SessionPersistence::new(home);
    let conn = Connection::open(&db_path).expect("open sessions.db");
    let sid = "d498b63f-c764-4f10-93ce-f6fd07120f5d";

    conn.execute(
        "INSERT INTO sessions (id, model, platform, created_at, updated_at, title, \
                              message_count) VALUES (?1, ?2, 'cli', ?3, ?3, ?4, 0)",
        params![sid, "minimax:MiniMax-M2.7", "2026-06-25T05:02:39+00:00", "你好"],
    )
    .expect("insert session");

    // 1 message (so messages_fts has something to clean up)
    conn.execute(
        "INSERT INTO messages (session_id, role, content, created_at) \
         VALUES (?1, 'user', 'hello world', ?2)",
        params![sid, "2026-06-25T05:02:40+00:00"],
    )
    .expect("insert message");
    let msg_id: i64 = conn
        .query_row(
            "SELECT id FROM messages WHERE session_id = ?1",
            params![sid],
            |r| r.get(0),
        )
        .expect("query msg id");
    // Manually feed the FTS5 index (the production trigger does this
    // in the same transaction; we're not running through
    // SessionPersistence::persist_message here because we want the
    // test to be self-contained).
    conn.execute(
        "INSERT INTO messages_fts(rowid, content, session_id, role) \
         VALUES (?1, 'hello world', ?2, 'user')",
        params![msg_id, sid],
    )
    .expect("insert messages_fts");

    // The actual culprit — compression_archives has FK(session_id) → sessions(id)
    conn.execute(
        "INSERT INTO compression_archives (session_id, content, original_turns, \
         compressed_turns, original_tokens, compressed_tokens, salience, source, created_at) \
         VALUES (?1, 'fake-compressed-content', 5, 1, 100, 20, 0.5, 'compression', ?2)",
        params![sid, "2026-06-25T06:00:00+00:00"],
    )
    .expect("insert compression_archives");

    // compression_summary_history also FK-anchored
    conn.execute(
        "INSERT INTO compression_summary_history (session_id, summary, \
         compression_count, original_turns, compressed_turns, original_tokens, \
         compressed_tokens, tokens_saved, created_at) \
         VALUES (?1, 'summary', 1, 5, 1, 100, 20, 80, ?2)",
        params![sid, "2026-06-25T06:00:00+00:00"],
    )
    .expect("insert compression_summary_history");

    sid.to_string()
}

#[tokio::test]
async fn delete_session_cascades_to_all_related_tables() {
    // Regression for the "无法删除 你好" bug. Pre-fix this
    // returned 500 FK constraint failed because `compression_archives`
    // / `compression_summary_history` were never deleted.
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().to_path_buf();
    let app = test_app_with_hermes_home(home.clone()).await;
    let sid = plant_session_with_all_children(&home);

    // Sanity: rows exist before delete
    let db_path = hermix_config::paths::data_dir_in(&home).join("sessions.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let cnt = |table: &str| -> i64 {
            conn.query_row(
                &format!("SELECT count(*) FROM {table} WHERE session_id = ?1"),
                rusqlite::params![sid],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(cnt("compression_archives"), 1, "compression_archives planted");
        assert_eq!(cnt("compression_summary_history"), 1, "compression_summary_history planted");
        assert_eq!(cnt("messages"), 1, "messages planted");
        let fts_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM messages_fts WHERE session_id = ?1",
                rusqlite::params![sid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fts_count, 1, "messages_fts planted");
    }

    // ── The actual DELETE that used to return 500 ──
    let res = app
        .oneshot(local(
            Request::builder()
                .method("DELETE")
                .uri(format!("/v1/sessions/{sid}"))
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "DELETE must succeed (was 500 FK before fix)"
    );

    // Post-delete: every related row must be gone, not orphaned.
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let cnt = |table: &str| -> i64 {
        // sessions 表主键是 id，级联表才用 session_id 外键列。
        let col = if table == "sessions" { "id" } else { "session_id" };
        conn.query_row(
            &format!("SELECT count(*) FROM {table} WHERE {col} = ?1"),
            rusqlite::params![sid],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(cnt("sessions"), 0, "sessions row deleted");
    assert_eq!(cnt("messages"), 0, "messages cascaded");
    assert_eq!(cnt("compression_archives"), 0, "compression_archives cascaded (the bug)");
    assert_eq!(
        cnt("compression_summary_history"),
        0,
        "compression_summary_history cascaded"
    );
    let fts_count: i64 = conn
        .query_row(
            "SELECT count(*) FROM messages_fts WHERE session_id = ?1",
            rusqlite::params![sid],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(fts_count, 0, "messages_fts cascaded (FTS5 'delete' cleanup)");
}

#[tokio::test]
async fn delete_session_returns_404_for_unknown_id() {
    // New behaviour on deleting a nonexistent id used to
    // return 200 OK with `{"ok":true}` — silently doing nothing. Now
    // it returns 404 so the client can show a clear "session not
    // found" instead of pretending the operation succeeded.
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().to_path_buf();
    let app = test_app_with_hermes_home(home).await;

    let res = app
        .oneshot(local(
            Request::builder()
                .method("DELETE")
                .uri("/v1/sessions/00000000-0000-0000-0000-000000000000")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_session_is_atomic_under_foreign_key_violation() {
    // Defensive: if the cascade ever regresses and a child row
    // survives, the transaction must roll back so the session stays
    // deletable on retry — not half-deleted (FK violation in step 5
    // would leak the whole tx without `transaction()`).
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().to_path_buf();
    let app = test_app_with_hermes_home(home.clone()).await;
    let sid = plant_session_with_all_children(&home);

    // Manually break the cascade: re-plant a `compression_archives`
    // row AFTER the function has finished — that simulates a race
    // where some other writer re-creates
    // a child row between our pre-flight and the DELETE. We then
    // assert the DELETE itself either succeeds (preferred) or 500s
    // without leaving the session half-deleted.
    let db_path = hermix_config::paths::data_dir_in(&home).join("sessions.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO compression_archives (session_id, content, original_turns, \
             compressed_turns, original_tokens, compressed_tokens, salience, source, created_at) \
             VALUES (?1, 'late-arrival', 5, 1, 100, 20, 0.5, 'compression', ?2)",
            rusqlite::params![sid, "2026-06-25T07:00:00+00:00"],
        )
        .unwrap();
    }

    let res = app
        .oneshot(local(
            Request::builder()
                .method("DELETE")
                .uri(format!("/v1/sessions/{sid}"))
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    // With the cascade in place this must succeed even with the
    // late-arrival row, because step 2 (DELETE FROM compression_archives)
    // nukes ALL rows for the session, including the late arrival.
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "DELETE must cascade through late-arrival compression rows"
    );

    // And post-delete: nothing left
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let survivors: i64 = conn
        .query_row(
            "SELECT count(*) FROM compression_archives WHERE session_id = ?1",
            rusqlite::params![sid],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(survivors, 0, "no compression_archives orphans after delete");
}
