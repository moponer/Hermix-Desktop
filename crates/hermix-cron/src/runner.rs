//! Job runner for the cron scheduler.
//!
//! The `CronRunner` is responsible for executing a cron job by creating a fresh
//! agent loop context, loading the job's skills, and delivering results to
//! the configured target platform.
//!
//! Safety: cron jobs **cannot** recursively schedule more cron jobs. The runner
//! runs the agent with a restricted tool set that excludes the cronjob tool.

use std::sync::Arc;

use hermix_agent::agent_loop::ToolRegistry;
use hermix_agent::{AgentConfig, AgentLoop};
use hermix_core::{AgentResult, LlmProvider, Message, PlatformSender, ToolSchema};

use crate::job::{CronJob, DeliverConfig, DeliverTarget};
use crate::scheduler::CronError;

// ---------------------------------------------------------------------------
// DeliverTarget helpers
// ---------------------------------------------------------------------------

/// Map a `DeliverTarget` to its platform name string.
impl DeliverTarget {
    /// Get the platform name string for this target.
    pub fn platform_name(&self) -> Option<&'static str> {
        match self {
            DeliverTarget::Origin => None,
            DeliverTarget::Local => None,
            DeliverTarget::Telegram => Some("telegram"),
            DeliverTarget::Discord => Some("discord"),
            DeliverTarget::Slack => Some("slack"),
            DeliverTarget::Email => Some("email"),
            DeliverTarget::WhatsApp => Some("whatsapp"),
            DeliverTarget::Signal => Some("signal"),
            DeliverTarget::Matrix => Some("matrix"),
            DeliverTarget::Mattermost => Some("mattermost"),
            DeliverTarget::DingTalk => Some("dingtalk"),
            DeliverTarget::Feishu => Some("feishu"),
            DeliverTarget::WeCom => Some("wecom"),
            DeliverTarget::Weixin => Some("weixin"),
            DeliverTarget::BlueBubbles => Some("bluebubbles"),
            DeliverTarget::Sms => Some("sms"),
            DeliverTarget::HomeAssistant => Some("homeassistant"),
        }
    }
}

// ---------------------------------------------------------------------------
// CronRunner
// ---------------------------------------------------------------------------

/// Executes cron jobs by spinning up a fresh agent loop for each invocation.
pub struct CronRunner {
    /// LLM provider for agent completions.
    llm_provider: Arc<dyn LlmProvider>,
    /// Tool registry providing available tools.
    tool_registry: Arc<ToolRegistry>,
    /// Platform sender for delivering results. None = log-only delivery.
    sender: Option<Arc<dyn PlatformSender>>,
}

impl CronRunner {
    /// Create a new cron runner without platform delivery (log-only).
    pub fn new(llm_provider: Arc<dyn LlmProvider>, tool_registry: Arc<ToolRegistry>) -> Self {
        Self {
            llm_provider,
            tool_registry,
            sender: None,
        }
    }

    /// Create a new cron runner with platform delivery support.
    pub fn with_sender(
        llm_provider: Arc<dyn LlmProvider>,
        tool_registry: Arc<ToolRegistry>,
        sender: Arc<dyn PlatformSender>,
    ) -> Self {
        Self {
            llm_provider,
            tool_registry,
            sender: Some(sender),
        }
    }

    /// Run a cron job.
    /// Creates a fresh agent loop context, loads skills per the job's config,
    /// and delivers results to the configured target platform.
    /// The agent is run with a restricted tool set that excludes the
    /// `cronjob` tool to prevent recursive scheduling.
    pub async fn run_job(&self, job: &CronJob) -> Result<AgentResult, CronError> {
        tracing::info!(
            "Running cron job '{}' ({})",
            job.name.as_deref().unwrap_or(&job.id),
            job.id
        );

        // Build agent config from job settings
        let mut config = AgentConfig::default();
        if let Some(ref model_cfg) = job.model {
            if let Some(ref model) = model_cfg.model {
                config.model = model.clone();
            }
        }
        // System prompt includes safety notice that cron tools are unavailable
        config.system_prompt = Some(format!(
            "You are executing a scheduled cron job. \
             You cannot schedule or manage other cron jobs from within a cron job execution. \
             Focus on completing the assigned task.\n\nTask: {}",
            job.prompt
        ));

        // Build tool list, excluding the cronjob tool to prevent recursive scheduling
        let tools = self.filtered_tool_schemas();

        // Create a fresh agent loop
        let agent_loop = AgentLoop::new(
            config,
            self.tool_registry.clone(),
            self.llm_provider.clone(),
        );

        // Build initial messages
        let messages = self.build_messages(job);

        // Run the agent loop
        let result = agent_loop
            .run(messages, Some(tools))
            .await
            .map_err(CronError::Agent)?;

        // Deliver results if configured
        if let Some(ref deliver) = job.deliver {
            if let Err(e) = self.deliver_result(&result, deliver).await {
                tracing::warn!("Failed to deliver result for job '{}': {}", job.id, e);
            }
        }

        Ok(result)
    }

    /// Build the initial messages for the agent from the job definition.
    fn build_messages(&self, job: &CronJob) -> Vec<Message> {
        let mut messages = Vec::new();

        // If a script is provided, use it as the user message; otherwise use the prompt
        let user_content = if let Some(ref script) = job.script {
            script.clone()
        } else {
            job.prompt.clone()
        };

        // Include skill context if skills are configured
        if let Some(ref skills) = job.skills {
            if !skills.is_empty() {
                let skill_context =
                    format!("Available skills for this task: {}", skills.join(", "));
                messages.push(Message::user(skill_context));
            }
        }

        messages.push(Message::user(user_content));
        messages
    }

    /// Filter out the `cronjob` tool from the registry to prevent recursive scheduling.
    fn filtered_tool_schemas(&self) -> Vec<ToolSchema> {
        self.tool_registry
            .schemas()
            .into_iter()
            .filter(|schema| schema.name != "cronjob")
            .collect()
    }

    /// Deliver the agent result to the configured target.
    /// Uses the injected `PlatformSender` when available. Falls back to
    /// logging for Origin/Local targets or when no sender is configured.
    async fn deliver_result(
        &self,
        result: &AgentResult,
        deliver: &DeliverConfig,
    ) -> Result<(), CronError> {
        // Extract the final text from the agent result
        let text = result
            .messages
            .iter()
            .rev()
            .find_map(|msg| {
                if msg.role == hermix_core::MessageRole::Assistant {
                    msg.content.clone()
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "(no output)".to_string());

        match deliver.target {
            DeliverTarget::Origin => {
                tracing::debug!("Delivering result to origin");
            }
            DeliverTarget::Local => {
                tracing::info!("Cron job result (local delivery):\n{}", text);
            }
            _ => {
                // Platform delivery: use injected sender or fall back to log
                let platform = deliver.target.platform_name().unwrap_or("unknown");
                let chat_id = deliver.platform.as_deref().unwrap_or("");

                if let Some(ref sender) = self.sender {
                    if let Err(e) = sender.send_message(platform, chat_id, &text).await {
                        tracing::warn!(
                            "Platform delivery failed for {}/{}: {}",
                            platform,
                            chat_id,
                            e
                        );
                        return Err(CronError::PlatformDelivery(e));
                    }
                    tracing::info!(
                        "Delivered cron job result to {}/{}",
                        platform,
                        chat_id
                    );
                } else {
                    tracing::info!(
                        "Cron job result delivery to {:?} (platform: {:?}):\n{}",
                        deliver.target,
                        deliver.platform,
                        text
                    );
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::CronJob;
    use hermix_core::ToolError;

    #[test]
    fn test_filtered_tool_schemas_excludes_cronjob() {
        // Create a minimal tool registry with a cronjob tool
        let mut registry = ToolRegistry::new();
        registry.register(
            "cronjob",
            hermix_core::tool_schema(
                "cronjob",
                "Manage cron jobs",
                hermix_core::JsonSchema::new("object"),
            ),
            Arc::new(|_params: serde_json::Value| -> Result<String, ToolError> {
                Ok("ok".to_string())
            }),
        );
        registry.register(
            "terminal",
            hermix_core::tool_schema(
                "terminal",
                "Run commands",
                hermix_core::JsonSchema::new("object"),
            ),
            Arc::new(|_params: serde_json::Value| -> Result<String, ToolError> {
                Ok("ok".to_string())
            }),
        );

        let runner = CronRunner {
            llm_provider: Arc::new(MockLlmProvider),
            tool_registry: Arc::new(registry),
            sender: None,
        };

        let schemas = runner.filtered_tool_schemas();
        let names: Vec<&str> = schemas.iter().map(|s| s.name.as_str()).collect();
        assert!(!names.contains(&"cronjob"));
        assert!(names.contains(&"terminal"));
    }

    #[test]
    fn test_build_messages_with_skills() {
        let mut job = CronJob::new("0 9 * * *", "Say hello");
        job.skills = Some(vec!["web_search".to_string(), "terminal".to_string()]);

        let runner = CronRunner {
            llm_provider: Arc::new(MockLlmProvider),
            tool_registry: Arc::new(ToolRegistry::new()),
            sender: None,
        };

        let messages = runner.build_messages(&job);
        // Should have skill context + prompt message
        assert_eq!(messages.len(), 2);
        assert!(messages[0].content.as_ref().unwrap().contains("web_search"));
        assert_eq!(messages[1].content.as_deref(), Some("Say hello"));
    }

    #[test]
    fn test_build_messages_with_script() {
        let mut job = CronJob::new("0 9 * * *", "Say hello");
        job.script = Some("echo hello world".to_string());

        let runner = CronRunner {
            llm_provider: Arc::new(MockLlmProvider),
            tool_registry: Arc::new(ToolRegistry::new()),
            sender: None,
        };

        let messages = runner.build_messages(&job);
        // Script overrides prompt as user message
        assert_eq!(messages[0].content.as_deref(), Some("echo hello world"));
    }

    // Minimal mock LLM provider for testing
    struct MockLlmProvider;

    #[async_trait::async_trait]
    impl LlmProvider for MockLlmProvider {
        async fn chat_completion(
            &self,
            _messages: &[hermix_core::Message],
            _tools: &[hermix_core::ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermix_core::LlmResponse, hermix_core::AgentError> {
            Ok(hermix_core::LlmResponse {
                message: hermix_core::Message::assistant("done"),
                usage: None,
                model: "mock".to_string(),
                finish_reason: Some("stop".to_string()),
            })
        }

        fn chat_completion_stream(
            &self,
            _messages: &[hermix_core::Message],
            _tools: &[hermix_core::ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> futures::stream::BoxStream<
            'static,
            Result<hermix_core::StreamChunk, hermix_core::AgentError>,
        > {
            Box::pin(futures::stream::empty())
        }
    }

    // Mock PlatformSender for testing
    struct MockPlatformSender {
        messages: std::sync::Mutex<Vec<(String, String, String)>>,
    }

    impl MockPlatformSender {
        fn new() -> Self {
            Self {
                messages: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn sent(&self) -> Vec<(String, String, String)> {
            self.messages.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl hermix_core::PlatformSender for MockPlatformSender {
        async fn send_message(
            &self,
            platform: &str,
            chat_id: &str,
            text: &str,
        ) -> Result<(), String> {
            self.messages
                .lock()
                .unwrap()
                .push((platform.to_string(), chat_id.to_string(), text.to_string()));
            Ok(())
        }
    }

    #[test]
    fn test_deliver_target_platform_name() {
        assert_eq!(DeliverTarget::Origin.platform_name(), None);
        assert_eq!(DeliverTarget::Local.platform_name(), None);
        assert_eq!(DeliverTarget::Telegram.platform_name(), Some("telegram"));
        assert_eq!(DeliverTarget::Discord.platform_name(), Some("discord"));
        assert_eq!(DeliverTarget::DingTalk.platform_name(), Some("dingtalk"));
        assert_eq!(DeliverTarget::Feishu.platform_name(), Some("feishu"));
        assert_eq!(DeliverTarget::Weixin.platform_name(), Some("weixin"));
    }

    #[tokio::test]
    async fn test_platform_sender_delivers_message() {
        let sender = MockPlatformSender::new();
        sender.send_message("telegram", "12345", "hello").await.unwrap();
        let sent = sender.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "telegram");
        assert_eq!(sent[0].1, "12345");
        assert_eq!(sent[0].2, "hello");
    }

    #[test]
    fn test_runner_with_sender() {
        let sender = Arc::new(MockPlatformSender::new());
        let runner = CronRunner::with_sender(
            Arc::new(MockLlmProvider),
            Arc::new(ToolRegistry::new()),
            sender,
        );
        assert!(runner.sender.is_some());
    }

    #[test]
    fn test_runner_without_sender() {
        let runner = CronRunner::new(
            Arc::new(MockLlmProvider),
            Arc::new(ToolRegistry::new()),
        );
        assert!(runner.sender.is_none());
    }
}
