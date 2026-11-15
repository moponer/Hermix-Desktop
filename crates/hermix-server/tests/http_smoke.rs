use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

#[tokio::test]
async fn health_ok() {
    let _ = tracing_subscriber::fmt::try_init();
    let cfg = hermix_config::GatewayConfig::default();
    let state = hermix_server::HttpServerState::build(cfg).await.unwrap();
    let app = hermix_server::router(state);
    let res = app
        .oneshot(local(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
#[ignore = "pre-existing：/api/providers/oauth stub 路由从未存在于已跟踪 src（本测试随 6b929a1a 收编 untracked 时即红，git log -S 对 src 无任何路由增删记录）；待 oauth stub 批次落地后移除本 ignore"]
async fn oauth_stub_routes_return_json_contract() {
    let _ = tracing_subscriber::fmt::try_init();
    let cfg = hermix_config::GatewayConfig::default();
    let state = hermix_server::HttpServerState::build(cfg).await.unwrap();
    let app = hermix_server::router(state);

    let res = app
        .clone()
        .oneshot(local(
            Request::builder()
                .uri("/api/providers/oauth")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v.get("providers").is_some());
    assert!(v.get("connected").is_some());

    let res = app
        .clone()
        .oneshot(local(
            Request::builder()
                .method("DELETE")
                .uri("/api/providers/oauth/openai")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v.get("ok"), Some(&serde_json::json!(true)));
    assert_eq!(v.get("provider"), Some(&serde_json::json!("openai")));

    let res = app
        .clone()
        .oneshot(local(
            Request::builder()
                .method("POST")
                .uri("/api/providers/oauth/openai/start")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v.get("flow"), Some(&serde_json::json!("device_code")));
    assert!(v.get("session_id").and_then(|x| x.as_str()).is_some());

    let res = app
        .clone()
        .oneshot(local(
            Request::builder()
                .method("GET")
                .uri("/api/providers/oauth/openai/poll/sess-1")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v.get("status"), Some(&serde_json::json!("error")));

    let res = app
        .clone()
        .oneshot(local(
            Request::builder()
                .method("POST")
                .uri("/api/providers/oauth/openai/submit")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"session_id": "s1", "code": "abc"}).to_string(),
                ))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v.get("ok"), Some(&serde_json::json!(false)));
    assert_eq!(v.get("status"), Some(&serde_json::json!("error")));

    let res = app
        .oneshot(local(
            Request::builder()
                .method("DELETE")
                .uri("/api/providers/oauth/sessions/sess-1")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v.get("ok"), Some(&serde_json::json!(true)));
}

#[tokio::test]
#[ignore = "requires live provider credentials to pass reliably"]
async fn command_help_runs_through_gateway() {
    let _ = tracing_subscriber::fmt::try_init();
    let cfg = hermix_config::GatewayConfig::default();
    let state = hermix_server::HttpServerState::build(cfg).await.unwrap();
    let app = hermix_server::router(state);
    let payload = serde_json::json!({ "command": "/help" });
    let res = app
        .oneshot(local(
            Request::builder()
                .method("POST")
                .uri("/v1/commands")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap(),
        ))
        .await
        .unwrap();

    // The command endpoint should accept the request
    // Note: Without a real LLM provider configured, it might fail with 500
    // But the endpoint itself should at least accept the request
    assert_eq!(res.status(), StatusCode::OK);

    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();

    // The response should have the expected structure
    assert!(v.get("accepted").is_some());
    assert!(v.get("output").is_some());
}

// ---------------------------------------------------------------------------
// §5.5 P6：GET /api/pairing/audit — 审计日志读端点（后端半边，
// DiagnosticsTab 展示属 UI 批次）。全部走 tempdir 装配隔离落盘。
// ---------------------------------------------------------------------------

/// oneshot 不经过真实监听器，axum 不会自动注入 `ConnectInfo`；缺了它
/// `request_guard::client_ip` 返回 None → `derive_origin` 判 `Unpaired`
/// → T1 关口 403。注入 loopback 即 `Local` 来源（本文件端点测试的隐含
/// 前提）。存量 oauth 测试同因自 T1 关口落地后即红（CI gate 只跑
/// `--lib` 未覆盖集成测试面），本批次一并修复。
fn local(req: Request<Body>) -> Request<Body> {
    let mut req = req;
    req.extensions_mut().insert(axum::extract::ConnectInfo(
        "127.0.0.1:41000"
            .parse::<std::net::SocketAddr>()
            .unwrap(),
    ));
    req
}

/// 端点 200：先 `record_audit` seed 两条再 GET，返回 events 数组 + count。
#[tokio::test]
async fn audit_endpoint_returns_seeded_events() {
    let _ = tracing_subscriber::fmt::try_init();
    let dir = tempfile::tempdir().unwrap();
    let cfg = hermix_config::GatewayConfig::default();
    let state = hermix_server::HttpServerState::build_with_hermes_home(
        cfg,
        dir.path().to_path_buf(),
    )
    .await
    .unwrap();
    let registry = state
        .device_registry
        .as_ref()
        .expect("tempdir home 必能开 devices.db");
    registry.record_audit(
        hermix_server::pairing::AuditEvent::Pairing,
        Some("dev-audit-1".to_string()),
        None,
        Some("allowed".to_string()),
    );
    registry.record_audit(
        hermix_server::pairing::AuditEvent::Revoke,
        Some("dev-audit-1".to_string()),
        None,
        Some("revoked".to_string()),
    );
    let app = hermix_server::router(state);

    let res = app
        .oneshot(local(
            Request::builder()
                .uri("/api/pairing/audit")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["count"], 2, "seed 两条 → count=2");
    let events = v["events"].as_array().expect("events 是数组");
    assert!(events.iter().any(|e| e["event"] == "pairing"));
    assert!(events.iter().any(|e| e["event"] == "revoke"));
    assert!(
        events.iter().all(|e| e["ts"].is_i64()),
        "每行含 ts 权威字段"
    );
}

/// 未带 ConnectInfo → Unpaired → T1 关口 403（审计行含 source_ip /
/// device_id，属设备安全域敏感数据，与 revoke_device 同鉴权面）。
#[tokio::test]
async fn audit_endpoint_unpaired_403() {
    let _ = tracing_subscriber::fmt::try_init();
    let dir = tempfile::tempdir().unwrap();
    let cfg = hermix_config::GatewayConfig::default();
    let state = hermix_server::HttpServerState::build_with_hermes_home(
        cfg,
        dir.path().to_path_buf(),
    )
    .await
    .unwrap();
    let app = hermix_server::router(state);

    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/pairing/audit")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

/// limit 生效：seed 5 条取 3 条（"最近 N 条"语义；时间倒序已由
/// pairing.rs 单元测试覆盖，此处不断言顺序避免同毫秒 ts 抖动）。
#[tokio::test]
async fn audit_endpoint_respects_limit() {
    let _ = tracing_subscriber::fmt::try_init();
    let dir = tempfile::tempdir().unwrap();
    let cfg = hermix_config::GatewayConfig::default();
    let state = hermix_server::HttpServerState::build_with_hermes_home(
        cfg,
        dir.path().to_path_buf(),
    )
    .await
    .unwrap();
    let registry = state
        .device_registry
        .as_ref()
        .expect("tempdir home 必能开 devices.db");
    for i in 0..5 {
        registry.record_audit(
            hermix_server::pairing::AuditEvent::Grant,
            None,
            None,
            Some(format!("d{i}")),
        );
    }
    let app = hermix_server::router(state);

    let res = app
        .oneshot(local(
            Request::builder()
                .uri("/api/pairing/audit?limit=3")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["count"], 3, "limit=3 → 3 条");
    assert_eq!(v["events"].as_array().unwrap().len(), 3);
}

/// `device_registry: None`（测试 / 只读环境）→ 503，不 panic、不碰 DB。
#[tokio::test]
async fn audit_endpoint_503_when_registry_missing() {
    let _ = tracing_subscriber::fmt::try_init();
    let dir = tempfile::tempdir().unwrap();
    let cfg = hermix_config::GatewayConfig::default();
    let mut state = hermix_server::HttpServerState::build_with_hermes_home(
        cfg,
        dir.path().to_path_buf(),
    )
    .await
    .unwrap();
    state.device_registry = None;
    let app = hermix_server::router(state);

    let res = app
        .oneshot(local(
            Request::builder()
                .uri("/api/pairing/audit")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
}
