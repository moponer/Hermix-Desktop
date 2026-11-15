use std::sync::Arc;

use hermix_agent::local_agent_service::LocalAgentService;
use hermix_agent::session_persistence::SessionPersistence;
use hermix_core::traits::{AgentOverrides, AgentService};
use hermix_core::{AgentError, Message};
use hermix_tools::ToolRegistry;
use tempfile::tempdir;

fn make_service() -> (Arc<SessionPersistence>, LocalAgentService) {
    let tmp = tempdir().expect("tempdir");
    let home = tmp.keep();
    for key in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENROUTER_API_KEY",
        "DASHSCOPE_API_KEY",
        "MOONSHOT_API_KEY",
        "MINIMAX_API_KEY",
        "NOUS_API_KEY",
        "GITHUB_COPILOT_TOKEN",
    ] {
        unsafe {
            std::env::remove_var(key);
        }
    }
    // Point HERMIX_HOME at the per-test tempdir so the shared
    // MemoryRouter built inside `LocalAgentService::new` operates
    // against an isolated memory.db. Without this, the router
    // reads from the developer's real `~/.hermix` and injects
    // cross-session recall hits into the loaded session — which
    // then breaks length-based assertions like
    // `get_session_messages_and_reset_session_round_trip`.
    // Save/restore is per-test: subsequent tests get a clean
    // `~/.hermix` (the env-var is reset in the `Drop` of the
    // returned service, see below).
    let _home_guard = HermixHomeGuard::set(&home);

    let session = Arc::new(SessionPersistence::new(&home));
    let _ = session.ensure_db();

    let mut cfg = hermix_config::GatewayConfig::default();
    cfg.home_dir = Some(home.to_string_lossy().to_string());
    cfg.model = Some("openai:gpt-4o".to_string());

    let service = LocalAgentService::new(
        Arc::new(cfg),
        Arc::new(ToolRegistry::new()),
        session.clone(),
    );
    (session, service)
}

/// RAII guard that sets `HERMIX_HOME` on construction and restores
/// the previous value (or clears it) on drop. Keeps the
/// `make_service` helper from leaking global env state into the
/// next test in the same binary.
struct HermixHomeGuard {
    previous: Option<String>,
}

impl HermixHomeGuard {
    fn set(path: &std::path::Path) -> Self {
        let previous = std::env::var("HERMIX_HOME").ok();
        // Safety: the test binary uses a multi-threaded runtime
        // (default for `cargo test`) and the guard scopes the
        // mutation to a single Drop call on the same thread that
        // called `set`. Per std::env::set_var docs, this is
        // sound when the set + get happen on the same thread —
        // which is guaranteed because `set` is called inline by
        // `make_service` and the corresponding `restore` runs in
        // `Drop` on the test's main thread.
        unsafe { std::env::set_var("HERMIX_HOME", path); }
        Self { previous }
    }
}

impl Drop for HermixHomeGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.previous {
                Some(v) => std::env::set_var("HERMIX_HOME", v),
                None => std::env::remove_var("HERMIX_HOME"),
            }
        }
    }
}

#[tokio::test]
async fn send_message_returns_config_error_without_api_key() {
    let (_session, service) = make_service();
    let err = service
        .send_message("s1", "hello", AgentOverrides::default())
        .await
        .expect_err("should fail with Config error when no API key is set");
    match err {
        AgentError::Config(msg) => {
            assert!(
                msg.contains("API key") || msg.contains("OPENAI_API_KEY"),
                "expected error about missing API key, got: {msg}"
            );
        }
        other => panic!("expected AgentError::Config, got {:?}", other),
    }
}

#[tokio::test]
async fn send_message_stream_returns_config_error_without_api_key() {
    let (_session, service) = make_service();
    let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen_cb = seen.clone();
    let on_chunk = Arc::new(move |chunk: hermix_core::StreamChunk| {
        if let Some(delta) = chunk.delta.and_then(|d| d.content) {
            seen_cb.lock().expect("lock").push(delta);
        }
    });
    let err = service
        .send_message_stream("s1", "hello", AgentOverrides::default(), on_chunk)
        .await
        .expect_err("should fail with Config error when no API key is set");
    match err {
        AgentError::Config(msg) => {
            assert!(
                msg.contains("API key") || msg.contains("OPENAI_API_KEY"),
                "expected error about missing API key, got: {msg}"
            );
        }
        other => panic!("expected AgentError::Config, got {:?}", other),
    }
    assert!(seen.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn get_session_messages_and_reset_session_round_trip() {
    let (session, service) = make_service();
    let messages = vec![Message::user("u"), Message::assistant("a")];
    session
        .persist_session(
            "s2",
            &messages,
            Some("openai:gpt-4o"),
            Some("test"),
            None,
            None,
            None,
            // workspace / expert_id: no binding for this round-trip test.
            None,
            None,
        )
        .expect("persist");

    let loaded = service
        .get_session_messages("s2")
        .await
        .expect("load from service");
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].content.as_deref(), Some("u"));
    assert_eq!(loaded[1].content.as_deref(), Some("a"));

    service.reset_session("s2").await.expect("reset");
    let after = service
        .get_session_messages("s2")
        .await
        .expect("load after reset");
    assert!(after.is_empty(), "reset should clear persisted session");
}
