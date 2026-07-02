use std::sync::Arc;

use futures::stream::BoxStream;
use hermix_agent::local_agent_service::{LocalAgentService, ProviderFactory};
use hermix_agent::session_persistence::SessionPersistence;
use hermix_core::traits::{AgentOverrides, AgentService};
use hermix_core::{AgentError, LlmProvider, LlmResponse, Message, StreamChunk, ToolSchema};
use hermix_tools::ToolRegistry;
use proptest::prelude::*;
use tempfile::tempdir;

struct EchoProvider;

#[async_trait::async_trait]
impl LlmProvider for EchoProvider {
    async fn chat_completion(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> Result<LlmResponse, AgentError> {
        Ok(LlmResponse {
            message: Message::assistant("mock-assistant-reply"),
            usage: None,
            model: "mock:model".to_string(),
            finish_reason: Some("stop".to_string()),
        })
    }

    fn chat_completion_stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
        Box::pin(futures::stream::empty())
    }
}

fn make_service() -> LocalAgentService {
    let tmp = tempdir().expect("tempdir");
    let home = tmp.keep();
    // Point HERMIX_HOME at the per-test tempdir so the shared
    // MemoryRouter built inside `LocalAgentService::new` operates
    // against an isolated memory.db. Without this, the router
    // reads from the developer's real `~/.hermix` and injects
    // cross-session recall hits into the loaded session — which
    // then breaks length-based invariants like `messages.len() >= 2`
    // and triggers proptest regression seeds (see
    // `prop_agent_service.proptest-regressions`).
    //
    // The guard restores `HERMIX_HOME` (or clears it) on drop so
    // subsequent test cases run in a clean environment.
    let _home_guard = HermixHomeGuard::set(&home);

    let session = Arc::new(SessionPersistence::new(&home));
    let _ = session.ensure_db();

    let mut cfg = hermix_config::GatewayConfig::default();
    cfg.home_dir = Some(home.to_string_lossy().to_string());
    cfg.model = Some("mock:model".to_string());
    let factory: ProviderFactory = Arc::new(|_, _| Arc::new(EchoProvider));
    LocalAgentService::new_with_provider_factory(
        Arc::new(cfg),
        Arc::new(ToolRegistry::new()),
        session,
        factory,
    )
}

/// RAII guard that scopes a `HERMIX_HOME` override to one
/// `make_service` call. `Drop` restores the previous value (or
/// removes the var if it was unset) so successive proptest cases
/// don't leak env state into each other.
struct HermixHomeGuard {
    previous: Option<String>,
}

impl HermixHomeGuard {
    fn set(path: &std::path::Path) -> Self {
        let previous = std::env::var("HERMIX_HOME").ok();
        // Safety: proptest runs each generated case on the same
        // thread, the guard is constructed inline by `make_service`,
        // and `Drop` restores on the same thread. See the matching
        // discussion in `local_agent_service_unit.rs::HermixHomeGuard`.
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

proptest! {
    // Feature: unified-runtime-architecture, Property 3: send_message appends user message and assistant reply
    //
    // Invariant: after a successful `send_message`, the *last* user
    // message in the returned message stream equals the caller's input,
    // and the *last* assistant message equals the model's reply. We
    // locate by role rather than by position because the runtime
    // interleaves System messages (system prompt, memory prefetch,
    // gathered context — see `agent_loop::prepare_run_context` lines
    // ~4149 / 4209 / 4223) before the user message, so position-based
    // assertions like `messages[len-2]` are fragile.
    #[test]
    fn send_message_appends_user_and_assistant(
        sid in "[a-z0-9]{1,20}",
        text in ".{1,40}",
    ) {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let (reply, messages) = rt.block_on(async {
            let service = make_service();
            let reply = service
                .send_message(&sid, &text, AgentOverrides::default())
                .await
                .expect("send_message");

            let messages = service
                .get_session_messages(&sid)
                .await
                .expect("session messages");
            (reply, messages)
        });
        prop_assert_eq!(reply.text, "mock-assistant-reply");
        prop_assert!(messages.len() >= 2, "no messages returned for sid={sid:?}");

        // Locate last user + last assistant by role (not position).
        let last_user_idx = messages
            .iter()
            .rposition(|m| m.role == hermix_core::MessageRole::User)
            .expect("at least one user message must be present");
        let last_assistant_idx = messages
            .iter()
            .rposition(|m| m.role == hermix_core::MessageRole::Assistant)
            .expect("at least one assistant message must be present");
        prop_assert!(
            last_user_idx < last_assistant_idx,
            "last user (idx={last_user_idx}) must come before last assistant (idx={last_assistant_idx})"
        );
        prop_assert_eq!(
            messages[last_user_idx].content.as_deref(),
            Some(text.as_str()),
            "last user message must equal the input `text`"
        );
        prop_assert_eq!(
            messages[last_assistant_idx].content.as_deref(),
            Some("mock-assistant-reply"),
            "last assistant message must equal the echo reply"
        );
    }
}
