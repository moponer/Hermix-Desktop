//! Test helpers shared by the dashboard endpoint tests.
//!
//! The dashboard endpoints all take `State<HttpServerState>`, so
//! each test needs a state. Building one from scratch is tedious
//! (it needs a config, a tool registry, a session persistence, …)
//! and we don't want to drag the whole of `lib.rs`'s production
//! constructors into the test scope. The helpers here build a
//! minimal state with the fields each test actually cares about
//! (today, just `hermix_home`).

#![cfg(any(test, feature = "test-helpers"))]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::collections::HashMap;

use hermix_agent::session_persistence::SessionPersistence;
use hermix_config::GatewayConfig;
use hermix_core::traits::{AgentOverrides, AgentReply, AgentService};
use hermix_core::{AgentError, Message, StreamChunk};
use hermix_tools::ToolRegistry;

use crate::HttpServerState;

/// Build an `HttpServerState` whose `hermix_home` is a fresh
/// tempdir nested inside its own user-root tempdir. The tempdirs
/// are leaked (intentionally — `HttpServerState` owns the path,
/// and a Drop impl would race with the test ending in a
/// non-deterministic way).
///
/// Why the nested layout: many dashboard endpoints compute
/// "user home" from `hermix_home.parent()`. If every test shared
/// the same `%TEMP%` parent, parallel tests would race on the
/// global temp dir. Nesting `hermix_home` under a per-test
/// `<uuid>/hermix_home` gives each test a unique parent so
/// `hermix_home.parent()` is also a fresh, isolated directory
/// the test can scribble legacy `.hermes` artifacts into.
pub async fn test_state_with_temp_home() -> HttpServerState {
    let user_root = tempfile::tempdir().expect("user-root tempdir");
    let user_root_path = user_root.path().to_path_buf();
    // Leak the TempDir handle so its Drop doesn't `remove_dir_all`
    // the path while HttpServerState still owns it.
    std::mem::forget(user_root);
    let hermix_home = user_root_path.join("hermix_home");
    std::fs::create_dir_all(&hermix_home).expect("hermix_home mkdir");
    build_state_with_home(hermix_home)
}

/// Like [`test_state_with_temp_home`] but with a caller-chosen
/// path. The path is **not** created — the caller does that. Used
/// by tests that want to pre-populate the target before the
/// handler runs.
pub fn test_state_with_home(home: impl AsRef<Path>) -> HttpServerState {
    let home = home.as_ref().to_path_buf();
    build_state_with_home(home)
}

fn build_state_with_home(home: PathBuf) -> HttpServerState {
    use hermix_tools::ClarifyDispatcher;

    let config = Arc::new(GatewayConfig::default());
    let tool_registry = Arc::new(ToolRegistry::new());
    let session_persistence = Arc::new(SessionPersistence::new(&home));
    let clarify_dispatcher = ClarifyDispatcher::new();
    let (clarify_broadcast, _) = tokio::sync::broadcast::channel::<crate::ClarifyBroadcast>(64);
    let agent_service: Arc<dyn AgentService> = Arc::new(NullAgentService);

    HttpServerState {
        config,
        tool_registry,
        hermix_home: home,
        session_persistence,
        cron_scheduler: None,
        skill_store: None,
        runtime_gateway_running: None,
        agent_service,
        approval_registry: Default::default(),
        clarify_dispatcher,
        clarify_broadcast,
        expert_registry: hermix_skills::ExpertRegistry::new(),
        automation_scheduler: None,
        user_allowlist: Arc::new(std::sync::Mutex::new(None)),
        session_bypass: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        consciousness_tracker: hermix_agent::ConsciousnessTracker::new(),
        tickloop_manager: Default::default(),
        groupchat_mgr: Arc::new(hermix_gateway::groupchat::GroupChatManager::new(
            Arc::new(hermix_gateway::groupchat::in_memory::InMemoryRoomPersistence::new()),
        )),
        active_sessions: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
        // Test-state helper: an MCP manager wired to its own empty
        // tool registry. Tests that need populated MCP state can
        // override via `Arc::get_mut` or by constructing a fresh
        // helper; the rest don't touch the manager.
        mcp_manager: hermix_mcp::ArcMcpManager::new(Arc::new(ToolRegistry::new())),
        task_engine: None,
        // 2026-07-06: production build path added this; mirror
        // here so the dashboard test fixtures stay in sync.
        recent_sends: Arc::new(std::sync::Mutex::new(HashMap::new())),
        // 2026-07-17: mirror production `active_handles` registry so
        // tests compile against the expanded `HttpServerState`.
        active_handles: Arc::new(std::sync::Mutex::new(HashMap::new())),
        // 2026-07-07: Todo event bus. Test fixtures don't exercise
        // the WS forwarder, so None is fine.
        todo_event_tx: None,
        orchestration_bus: crate::OrchestrationEventBus::new(),
        task_service: None,
        automation_service: None,
        automation_engine_scheduler: None,
        custom_app_manager: None,
    }
}

/// No-op `AgentService` used by tests. The migration endpoints
/// never call into the agent; this exists only because
/// `HttpServerState` requires a value of that type. Every method
/// returns a clearly-labeled error so any test that accidentally
/// *does* call one fails fast and loud instead of hanging.
struct NullAgentService;

#[async_trait::async_trait]
impl AgentService for NullAgentService {
    async fn send_message(
        &self,
        _session_id: &str,
        _text: &str,
        _overrides: AgentOverrides,
    ) -> Result<AgentReply, AgentError> {
        Err(AgentError::Config(
            "NullAgentService: not for production use".into(),
        ))
    }

    async fn send_message_stream(
        &self,
        _session_id: &str,
        _text: &str,
        _overrides: AgentOverrides,
        _on_chunk: Arc<dyn Fn(StreamChunk) + Send + Sync>,
    ) -> Result<AgentReply, AgentError> {
        Err(AgentError::Config(
            "NullAgentService: not for production use".into(),
        ))
    }

    async fn get_session_messages(
        &self,
        _session_id: &str,
    ) -> Result<Vec<Message>, AgentError> {
        Err(AgentError::Config(
            "NullAgentService: not for production use".into(),
        ))
    }

    async fn reset_session(&self, _session_id: &str) -> Result<(), AgentError> {
        Err(AgentError::Config(
            "NullAgentService: not for production use".into(),
        ))
    }
}
