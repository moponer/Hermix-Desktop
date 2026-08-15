//! Gateway orchestrator: starts, stops, and routes messages to platform adapters.
//!
//! Implements the full message flow:
//! 1. Platform adapter receives a message
//! 2. Gateway looks up or creates a session via `SessionManager`
//! 3. Gateway checks DM authorization via `DmManager`
//! 4. Gateway invokes the agent loop with the session's message history
//! 5. Gateway sends the response back via the platform adapter
//!
//! Also integrates `StreamManager` for progressive message editing.

use chrono::{DateTime, Utc};
use hermix_config::env_compat;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

use hermix_core::errors::GatewayError;
use hermix_core::traits::{ParseMode, PlatformAdapter};
use hermix_core::types::{Message, MessageRole};
use hermix_tools::registry::ToolRegistry as FullToolRegistry;

use crate::background::{BackgroundTaskManager, TaskStatus};
use crate::commands::handle_command;
use crate::dm::{DmDecision, DmManager};
use crate::hook_payloads;
use crate::hooks::{HookEvent, HookRegistry};
use crate::session::SessionManager;
use crate::stream::{StreamConfig, StreamManager};

// ---------------------------------------------------------------------------
// GatewayConfig
// ---------------------------------------------------------------------------

/// Configuration for the Gateway orchestrator.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GatewayConfig {
    /// Enable SSRF protection on outbound URLs (default: true).
    #[serde(default = "default_true")]
    pub ssrf_protection: bool,

    /// Media cache directory path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_cache_dir: Option<String>,

    /// Maximum media cache size in bytes (0 = unlimited).
    #[serde(default)]
    pub media_cache_max_bytes: u64,

    /// Whether to enable streaming output (progressive message editing).
    #[serde(default)]
    pub streaming_enabled: bool,

    /// Streaming configuration.
    #[serde(default)]
    pub streaming: StreamConfig,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            ssrf_protection: true,
            media_cache_dir: None,
            media_cache_max_bytes: 0,
            streaming_enabled: false,
            streaming: StreamConfig::default(),
        }
    }
}

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// IncomingMessage (platform-agnostic)
// ---------------------------------------------------------------------------

/// A platform-agnostic incoming message for gateway routing.
#[derive(Debug, Clone)]
pub struct IncomingMessage {
    /// Platform name (e.g., "telegram", "discord").
    pub platform: String,
    /// Chat/channel identifier.
    pub chat_id: String,
    /// User identifier.
    pub user_id: String,
    /// Message text content.
    pub text: String,
    /// Platform-specific message ID (for reply threading).
    pub message_id: Option<String>,
    /// Whether this is a DM (direct message) or group message.
    pub is_dm: bool,
}

// ---------------------------------------------------------------------------
// MessageHandler callback
// ---------------------------------------------------------------------------

/// Callback type for processing messages through the agent loop.
/// Takes the session messages and returns the agent's response text.
pub type MessageHandler = Arc<
    dyn Fn(
            Vec<Message>,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send>,
        > + Send
        + Sync,
>;

/// Structured runtime context passed to V2 handlers.
#[derive(Debug, Clone, Default)]
pub struct GatewayRuntimeContext {
    pub session_key: String,
    pub platform: String,
    pub chat_id: String,
    pub user_id: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub profile: Option<String>,
    pub branch: Option<String>,
    pub personality: Option<String>,
    pub home: Option<String>,
    pub verbose: bool,
    pub yolo: bool,
    pub reasoning: bool,
    pub mcp_reload_generation: u64,
    /// Messages queued by handlers to be delivered only after the main reply.
    pub deferred_post_delivery_messages: Option<Arc<StdMutex<Vec<String>>>>,
    /// Release flag shared with handlers for post-delivery gating.
    pub deferred_post_delivery_released: Option<Arc<AtomicBool>>,
}

/// Context-aware callback type for processing messages through the agent loop.
pub type MessageHandlerWithContext = Arc<
    dyn Fn(
            Vec<Message>,
            GatewayRuntimeContext,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send>,
        > + Send
        + Sync,
>;

/// Callback type for streaming message processing.
/// Takes session messages and a chunk callback, returns the final response.
pub type StreamingMessageHandler = Arc<
    dyn Fn(
            Vec<Message>,
            Arc<dyn Fn(String) + Send + Sync>,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send>,
        > + Send
        + Sync,
>;

/// Context-aware callback type for streaming message processing.
pub type StreamingMessageHandlerWithContext = Arc<
    dyn Fn(
            Vec<Message>,
            GatewayRuntimeContext,
            Arc<dyn Fn(String) + Send + Sync>,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send>,
        > + Send
        + Sync,
>;

#[derive(Debug, Clone, Default)]
pub(crate) struct UsageStats {
    pub(crate) user_messages: u64,
    pub(crate) assistant_messages: u64,
    pub(crate) input_chars: u64,
    pub(crate) output_chars: u64,
    pub(crate) last_updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SessionRuntimeState {
    pub(crate) model: Option<String>,
    pub(crate) provider: Option<String>,
    pub(crate) profile: Option<String>,
    pub(crate) branch: Option<String>,
    pub(crate) personality: Option<String>,
    pub(crate) home: Option<String>,
    /// Optional usage budget (same units as `/budget` input; gateway displays as-is).
    pub(crate) budget: Option<f64>,
    pub(crate) verbose: bool,
    pub(crate) yolo: bool,
    pub(crate) reasoning: bool,
}


// ---------------------------------------------------------------------------
// Gateway
// ---------------------------------------------------------------------------

/// Background task that reads inbound messages from `rx` and routes them
/// through the gateway's full pipeline (DM check → session → agent → reply).
pub(crate) async fn gw_pump_inbound(gateway: Arc<Gateway>, mut rx: mpsc::Receiver<IncomingMessage>) {
    while let Some(msg) = rx.recv().await {
        if let Err(e) = gateway.route_message(&msg).await {
            warn!(
                platform = %msg.platform,
                chat_id = %msg.chat_id,
                user_id = %msg.user_id,
                error = %e,
                "inbound message routing failed"
            );
        }
    }
    debug!("inbound pump exited");
}

/// Central orchestrator for all platform adapters.
/// The `Gateway` owns a collection of named `PlatformAdapter` instances,
/// a `SessionManager`, a `DmManager`, and a `StreamManager`. It provides
/// a unified interface to start/stop adapters and route messages.

pub struct Gateway {
    pub(crate) adapters: Arc<RwLock<HashMap<String, Arc<dyn PlatformAdapter>>>>,
    pub(crate) session_manager: Arc<SessionManager>,
    pub(crate) dm_manager: Arc<RwLock<DmManager>>,
    pub(crate) stream_manager: Arc<StreamManager>,
    pub(crate) config: GatewayConfig,
    /// Optional message handler for processing messages through the agent loop.
    pub(crate) message_handler: RwLock<Option<MessageHandler>>,
    /// Optional context-aware message handler for processing incoming messages.
    pub(crate) message_handler_with_context: RwLock<Option<MessageHandlerWithContext>>,
    /// Optional streaming message handler.
    pub(crate) streaming_handler: RwLock<Option<StreamingMessageHandler>>,
    /// Optional context-aware streaming message handler.
    pub(crate) streaming_handler_with_context: RwLock<Option<StreamingMessageHandlerWithContext>>,
    /// Runtime command state for each session.
    pub(crate) runtime_state: RwLock<HashMap<String, SessionRuntimeState>>,
    /// Basic usage counters for each session.
    pub(crate) usage_stats: RwLock<HashMap<String, UsageStats>>,
    /// Tracks async `/background` and `/btw` tasks.
    pub(crate) background_tasks: Arc<BackgroundTaskManager>,
    /// MCP reload generation number.
    pub(crate) mcp_reload_generation: RwLock<u64>,
    /// Optional hook registry for runtime event emission.
    pub(crate) hook_registry: RwLock<Option<Arc<HookRegistry>>>,
    /// Tool registry for listing, enabling, and disabling tools via slash commands.
    pub(crate) tool_registry: RwLock<Option<FullToolRegistry>>,
}


pub mod lifecycle;
pub mod routing;
pub mod usage;
pub mod misc;

#[async_trait::async_trait]
impl hermix_core::PlatformSender for Gateway {
    async fn send_message(
        &self,
        platform: &str,
        chat_id: &str,
        text: &str,
    ) -> Result<(), String> {
        Gateway::send_message(self, platform, chat_id, text, None)
            .await
            .map_err(|e| e.to_string())
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook_payloads;
    use crate::hooks::{HookEvent, HookHandler, HookRegistry};
    use crate::session::SessionManager;
    use async_trait::async_trait;
    use hermix_config::session::SessionConfig;
    use std::sync::Mutex;

    struct TestAdapter {
        messages: Arc<Mutex<Vec<(String, String)>>>,
    }

    struct RecordingHook {
        seen: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    }

    #[async_trait]
    impl HookHandler for RecordingHook {
        async fn handle(&self, event: &HookEvent) -> Result<(), String> {
            self.seen
                .lock()
                .unwrap()
                .push((event.event_type.clone(), event.context.clone()));
            Ok(())
        }

        fn name(&self) -> &str {
            "recording-hook"
        }
    }

    #[async_trait]
    impl PlatformAdapter for TestAdapter {
        async fn start(&self) -> Result<(), GatewayError> {
            Ok(())
        }

        async fn stop(&self) -> Result<(), GatewayError> {
            Ok(())
        }

        async fn send_message(
            &self,
            chat_id: &str,
            text: &str,
            _parse_mode: Option<ParseMode>,
        ) -> Result<(), GatewayError> {
            self.messages
                .lock()
                .unwrap()
                .push((chat_id.to_string(), text.to_string()));
            Ok(())
        }

        async fn edit_message(
            &self,
            _chat_id: &str,
            _message_id: &str,
            _text: &str,
        ) -> Result<(), GatewayError> {
            Ok(())
        }

        async fn send_file(
            &self,
            _chat_id: &str,
            _file_path: &str,
            _caption: Option<&str>,
        ) -> Result<(), GatewayError> {
            Ok(())
        }

        fn is_running(&self) -> bool {
            true
        }

        fn platform_name(&self) -> &str {
            "test"
        }
    }

    #[test]
    fn gateway_config_default() {
        let cfg = GatewayConfig::default();
        assert!(cfg.ssrf_protection);
        assert!(cfg.media_cache_dir.is_none());
        assert_eq!(cfg.media_cache_max_bytes, 0);
        assert!(!cfg.streaming_enabled);
    }

    #[tokio::test]
    async fn gateway_register_and_list_adapters() {
        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let gw = Gateway::with_defaults(session_mgr, GatewayConfig::default());

        assert!(gw.adapter_names().await.is_empty());
    }

    #[tokio::test]
    async fn gateway_route_dm_denied() {
        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let dm_manager = DmManager::with_ignore_behavior();
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());

        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "unknown_user".into(),
            text: "hello".into(),
            message_id: None,
            is_dm: true,
        };

        // Should succeed (deny silently)
        let result = gw.route_message(&incoming).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn gateway_route_no_handler() {
        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());

        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "hello".into(),
            message_id: None,
            is_dm: true,
        };

        // Should fail because no message handler is set
        let result = gw.route_message(&incoming).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn gateway_route_group_message_skips_dm_check() {
        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let dm_manager = DmManager::with_ignore_behavior();
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());

        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "-group1".into(),
            user_id: "unknown_user".into(),
            text: "hello group".into(),
            message_id: None,
            is_dm: false, // Group message, no DM check
        };

        // Should fail because no handler, but DM check is skipped
        let result = gw.route_message(&incoming).await;
        assert!(result.is_err()); // No handler configured
    }

    #[tokio::test]
    async fn gateway_executes_status_command_without_agent_handler() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("test", adapter).await;

        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/status".into(),
            message_id: None,
            is_dm: true,
        };

        let result = gw.route_message(&incoming).await;
        assert!(result.is_ok());

        let msgs = sent.lock().unwrap();
        assert!(msgs.iter().any(|(_, text)| text.contains("Gateway status")));
    }

    #[tokio::test]
    async fn gateway_background_task_lifecycle_commands_work() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("test", adapter).await;
        gw.set_message_handler(Arc::new(|messages| {
            Box::pin(async move {
                let prompt = messages
                    .last()
                    .and_then(|m| m.content.clone())
                    .unwrap_or_else(|| "none".to_string());
                Ok(format!("done: {}", prompt))
            })
        }))
        .await;

        let start = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/background ping".into(),
            message_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&start).await.is_ok());

        let task_id = {
            let msgs = sent.lock().unwrap();
            let queued = msgs
                .iter()
                .find(|(_, text)| text.contains("Background task started"))
                .expect("queue ack should exist");
            queued
                .1
                .lines()
                .find_map(|line| line.strip_prefix("Task ID: ").map(str::trim))
                .expect("task id line")
                .to_string()
        };

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let status = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: format!("/background status {}", task_id),
            message_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&status).await.is_ok());

        let msgs = sent.lock().unwrap();
        assert!(msgs.iter().any(|(_, text)| text.contains("completed")));
    }

    #[tokio::test]
    async fn gateway_admin_approve_and_deny_affects_dm_authorization() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_ignore_behavior();
        dm_manager.add_admin("admin1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("test", adapter).await;

        let approve = IncomingMessage {
            platform: "test".into(),
            chat_id: "admin-chat".into(),
            user_id: "admin1".into(),
            text: "/approve user2".into(),
            message_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&approve).await.is_ok());

        // user2 should now pass DM authorization, then fail because no handler is configured.
        let authorized_dm = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat-u2".into(),
            user_id: "user2".into(),
            text: "hello".into(),
            message_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&authorized_dm).await.is_err());

        let deny = IncomingMessage {
            platform: "test".into(),
            chat_id: "admin-chat".into(),
            user_id: "admin1".into(),
            text: "/deny user2".into(),
            message_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&deny).await.is_ok());

        // user2 should be denied again, and route should return Ok (silently denied).
        let denied_dm = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat-u2".into(),
            user_id: "user2".into(),
            text: "hello again".into(),
            message_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&denied_dm).await.is_ok());
    }

    #[tokio::test]
    async fn gateway_reload_mcp_and_status_reflect_runtime_state() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("test", adapter).await;

        let provider = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/provider openrouter".into(),
            message_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&provider).await.is_ok());

        let profile = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/profile prod".into(),
            message_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&profile).await.is_ok());

        let reload = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/reload_mcp".into(),
            message_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&reload).await.is_ok());

        let status = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/status".into(),
            message_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&status).await.is_ok());

        let msgs = sent.lock().unwrap();
        let status_text = msgs
            .iter()
            .rev()
            .find_map(|(_, text)| {
                if text.contains("Gateway status") {
                    Some(text.clone())
                } else {
                    None
                }
            })
            .expect("status response should exist");
        assert!(status_text.contains("provider: openrouter"));
        assert!(status_text.contains("profile: prod"));
        assert!(status_text.contains("mcp generation: 1"));
    }

    #[tokio::test]
    async fn gateway_runtime_state_is_injected_into_agent_messages() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("test", adapter).await;
        gw.set_message_handler(Arc::new(|messages| {
            Box::pin(async move {
                let hint = messages
                    .iter()
                    .find(|m| {
                        m.role == MessageRole::System
                            && m.content
                                .as_deref()
                                .unwrap_or("")
                                .contains("[gateway_runtime]")
                    })
                    .and_then(|m| m.content.clone())
                    .unwrap_or_else(|| "no-runtime-hints".to_string());
                Ok(hint)
            })
        }))
        .await;

        let set_provider = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/provider openai".into(),
            message_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&set_provider).await.is_ok());

        let set_model = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/model gpt-4o".into(),
            message_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&set_model).await.is_ok());

        let set_profile = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/profile prod".into(),
            message_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&set_profile).await.is_ok());

        let set_branch = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/branch feature/parity".into(),
            message_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&set_branch).await.is_ok());

        let normal = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "hello".into(),
            message_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&normal).await.is_ok());

        let msgs = sent.lock().unwrap();
        let echoed = msgs
            .iter()
            .rev()
            .find_map(|(_, text)| {
                if text.contains("[gateway_runtime]") {
                    Some(text.clone())
                } else {
                    None
                }
            })
            .expect("runtime hint response should exist");

        assert!(echoed.contains("model=gpt-4o"));
        assert!(echoed.contains("provider=openai"));
        assert!(echoed.contains("profile=prod"));
        assert!(echoed.contains("branch=feature/parity"));
    }

    #[tokio::test]
    async fn gateway_context_handler_receives_structured_runtime_context() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("test", adapter).await;
        gw.set_message_handler_with_context(Arc::new(|messages, ctx| {
            Box::pin(async move {
                let payload = format!(
                    "ctx model={:?} provider={:?} profile={:?} branch={:?} platform={} user={} session={} has_legacy_hint={}",
                    ctx.model,
                    ctx.provider,
                    ctx.profile,
                    ctx.branch,
                    ctx.platform,
                    ctx.user_id,
                    ctx.session_key,
                    messages.iter().any(|m| m
                        .content
                        .as_deref()
                        .unwrap_or("")
                        .contains("[gateway_runtime]"))
                );
                Ok(payload)
            })
        }))
        .await;

        let setup_cmds = vec![
            "/provider openai",
            "/model gpt-4o-mini",
            "/profile prod",
            "/branch feat-123",
        ];
        for cmd in setup_cmds {
            let incoming = IncomingMessage {
                platform: "test".into(),
                chat_id: "chat1".into(),
                user_id: "user1".into(),
                text: cmd.to_string(),
                message_id: None,
                is_dm: true,
            };
            assert!(gw.route_message(&incoming).await.is_ok());
        }

        let normal = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "run".into(),
            message_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&normal).await.is_ok());

        let msgs = sent.lock().unwrap();
        let echoed = msgs
            .iter()
            .rev()
            .find_map(|(_, text)| {
                if text.starts_with("ctx model=") {
                    Some(text.clone())
                } else {
                    None
                }
            })
            .expect("context response should exist");
        assert!(echoed.contains("Some(\"gpt-4o-mini\")"));
        assert!(echoed.contains("Some(\"openai\")"));
        assert!(echoed.contains("Some(\"prod\")"));
        assert!(echoed.contains("Some(\"feat-123\")"));
        assert!(echoed.contains("platform=test"));
        assert!(echoed.contains("user=user1"));
        assert!(echoed.contains("has_legacy_hint=false"));
    }

    #[tokio::test]
    async fn gateway_deferred_post_delivery_messages_flush_after_main_reply() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("test", adapter).await;
        gw.set_message_handler_with_context(Arc::new(|_messages, ctx| {
            Box::pin(async move {
                let pending = ctx
                    .deferred_post_delivery_messages
                    .expect("deferred queue should be present");
                let released = ctx
                    .deferred_post_delivery_released
                    .expect("release flag should be present");
                assert!(
                    !released.load(std::sync::atomic::Ordering::Acquire),
                    "release must remain false before main reply delivery"
                );
                pending
                    .lock()
                    .unwrap()
                    .push("💾 deferred-memory-update".to_string());
                Ok("main-response".to_string())
            })
        }))
        .await;

        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "hello".into(),
            message_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&incoming).await.is_ok());

        let msgs = sent.lock().unwrap();
        let ordered: Vec<String> = msgs.iter().map(|(_, t)| t.clone()).collect();
        assert_eq!(
            ordered,
            vec![
                "main-response".to_string(),
                "💾 deferred-memory-update".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn gateway_status_then_main_then_deferred_order_matches_python_chain() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Arc::new(Gateway::new(
            session_mgr,
            dm_manager,
            GatewayConfig::default(),
        ));
        gw.register_adapter("test", adapter).await;

        let gw_for_handler = gw.clone();
        gw.set_message_handler_with_context(Arc::new(move |_messages, ctx| {
            let gw = gw_for_handler.clone();
            Box::pin(async move {
                let pending = ctx
                    .deferred_post_delivery_messages
                    .expect("deferred queue should be present");
                pending.lock().unwrap().push("💾 bg-review".to_string());

                // Mirrors Python's status_callback: status is forwarded immediately.
                gw.send_message(&ctx.platform, &ctx.chat_id, "⚠️ context pressure", None)
                    .await
                    .expect("status callback send should succeed");

                Ok("main-response".to_string())
            })
        }))
        .await;

        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "hello".into(),
            message_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&incoming).await.is_ok());

        let msgs = sent.lock().unwrap();
        let ordered: Vec<String> = msgs.iter().map(|(_, t)| t.clone()).collect();
        assert_eq!(
            ordered,
            vec![
                "⚠️ context pressure".to_string(),
                "main-response".to_string(),
                "💾 bg-review".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn gateway_streaming_flushes_deferred_after_stream_finishes() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let mut cfg = GatewayConfig::default();
        cfg.streaming_enabled = true;
        let gw = Arc::new(Gateway::new(session_mgr, dm_manager, cfg));
        gw.register_adapter("test", adapter).await;

        gw.set_streaming_handler_with_context(Arc::new(|_messages, ctx, _on_chunk| {
            Box::pin(async move {
                let pending = ctx
                    .deferred_post_delivery_messages
                    .expect("deferred queue should be present");
                let released = ctx
                    .deferred_post_delivery_released
                    .expect("release flag should be present");
                assert!(
                    !released.load(std::sync::atomic::Ordering::Acquire),
                    "release must stay false while stream handler is running"
                );
                pending
                    .lock()
                    .unwrap()
                    .push("💾 stream-bg-review".to_string());
                Ok("stream-final".to_string())
            })
        }))
        .await;

        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "hello".into(),
            message_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&incoming).await.is_ok());

        let msgs = sent.lock().unwrap();
        let ordered: Vec<String> = msgs.iter().map(|(_, t)| t.clone()).collect();
        assert_eq!(
            ordered,
            vec!["...".to_string(), "💾 stream-bg-review".to_string()]
        );
    }

    #[tokio::test]
    async fn gateway_emits_agent_start_and_end_hooks() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });
        let hook_seen = Arc::new(Mutex::new(Vec::new()));
        let mut hooks = HookRegistry::new();
        hooks.register_in_process(
            "agent:*",
            Arc::new(RecordingHook {
                seen: hook_seen.clone(),
            }),
        );

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.set_hook_registry(Arc::new(hooks)).await;
        gw.register_adapter("test", adapter).await;
        gw.set_message_handler(Arc::new(|_messages| {
            Box::pin(async move { Ok("main-response".to_string()) })
        }))
        .await;

        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "hello".into(),
            message_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&incoming).await.is_ok());

        let events = hook_seen.lock().unwrap();
        let names: Vec<String> = events.iter().map(|(name, _)| name.clone()).collect();
        assert_eq!(
            names,
            vec!["agent:start".to_string(), "agent:end".to_string()]
        );
        let end_payload = events
            .iter()
            .find(|(name, _)| name == "agent:end")
            .map(|(_, ctx)| ctx.clone())
            .expect("agent:end payload should exist");
        assert_eq!(end_payload["success"], serde_json::json!(true));
    }

    #[tokio::test]
    async fn gateway_hook_event_order_captures_start_status_step_end() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });
        let hook_seen = Arc::new(Mutex::new(Vec::new()));
        let mut hooks = HookRegistry::new();
        hooks.register_in_process(
            "agent:*",
            Arc::new(RecordingHook {
                seen: hook_seen.clone(),
            }),
        );

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Arc::new(Gateway::new(
            session_mgr,
            dm_manager,
            GatewayConfig::default(),
        ));
        gw.set_hook_registry(Arc::new(hooks)).await;
        gw.register_adapter("test", adapter).await;

        let gw_for_handler = gw.clone();
        gw.set_message_handler_with_context(Arc::new(move |_messages, ctx| {
            let gw = gw_for_handler.clone();
            Box::pin(async move {
                gw.emit_hook_event(
                    "agent:status",
                    hook_payloads::agent_status(
                        ctx.platform.clone(),
                        ctx.chat_id.clone(),
                        ctx.user_id.clone(),
                        ctx.session_key.clone(),
                        "lifecycle",
                        "Context pressure 85%",
                    ),
                )
                .await;
                gw.emit_hook_event(
                    "agent:step",
                    hook_payloads::agent_step(
                        ctx.platform.clone(),
                        ctx.chat_id.clone(),
                        ctx.user_id.clone(),
                        ctx.session_key.clone(),
                        1,
                        vec!["memory".into()],
                        vec![serde_json::json!({"name":"memory","result":"ok"})],
                    ),
                )
                .await;
                Ok("done".to_string())
            })
        }))
        .await;

        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "hello".into(),
            message_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&incoming).await.is_ok());

        let events = hook_seen.lock().unwrap();
        let names: Vec<String> = events.iter().map(|(name, _)| name.clone()).collect();
        assert_eq!(
            names,
            vec![
                "agent:start".to_string(),
                "agent:status".to_string(),
                "agent:step".to_string(),
                "agent:end".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn gateway_emits_session_start_and_command_hook_events() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });
        let hook_seen = Arc::new(Mutex::new(Vec::new()));
        let mut hooks = HookRegistry::new();
        hooks.register_in_process(
            "session:*",
            Arc::new(RecordingHook {
                seen: hook_seen.clone(),
            }),
        );
        hooks.register_in_process(
            "command:*",
            Arc::new(RecordingHook {
                seen: hook_seen.clone(),
            }),
        );

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.set_hook_registry(Arc::new(hooks)).await;
        gw.register_adapter("test", adapter).await;

        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/status".into(),
            message_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&incoming).await.is_ok());

        let events = hook_seen.lock().unwrap();
        let names: Vec<String> = events.iter().map(|(name, _)| name.clone()).collect();
        assert!(names.contains(&"session:start".to_string()));
        assert!(names.contains(&"command:status".to_string()));
    }

    #[tokio::test]
    async fn gateway_emits_command_hook_for_unknown_slash() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });
        let hook_seen = Arc::new(Mutex::new(Vec::new()));
        let mut hooks = HookRegistry::new();
        hooks.register_in_process(
            "command:*",
            Arc::new(RecordingHook {
                seen: hook_seen.clone(),
            }),
        );

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.set_hook_registry(Arc::new(hooks)).await;
        gw.register_adapter("test", adapter).await;

        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "  /nosuchplugincmd_xyz arg  ".into(),
            message_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&incoming).await.is_ok());

        let events = hook_seen.lock().unwrap();
        let cmd_events: Vec<_> = events
            .iter()
            .filter(|(n, _)| n.starts_with("command:"))
            .collect();
        assert_eq!(cmd_events.len(), 1);
        assert_eq!(cmd_events[0].0, "command:nosuchplugincmd_xyz");
        assert_eq!(
            cmd_events[0].1.get("text").and_then(|v| v.as_str()),
            Some("/nosuchplugincmd_xyz arg")
        );
    }

    #[tokio::test]
    async fn gateway_emits_session_end_and_reset_for_reset_command() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });
        let hook_seen = Arc::new(Mutex::new(Vec::new()));
        let mut hooks = HookRegistry::new();
        hooks.register_in_process(
            "session:*",
            Arc::new(RecordingHook {
                seen: hook_seen.clone(),
            }),
        );

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.set_hook_registry(Arc::new(hooks)).await;
        gw.register_adapter("test", adapter).await;
        gw.set_message_handler(Arc::new(|_messages| {
            Box::pin(async move { Ok("assistant".to_string()) })
        }))
        .await;

        let normal = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "hello".into(),
            message_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&normal).await.is_ok());

        let reset = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/reset".into(),
            message_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&reset).await.is_ok());

        let events = hook_seen.lock().unwrap();
        let names: Vec<String> = events.iter().map(|(name, _)| name.clone()).collect();
        assert!(names.contains(&"session:end".to_string()));
        assert!(names.contains(&"session:reset".to_string()));
    }
}

