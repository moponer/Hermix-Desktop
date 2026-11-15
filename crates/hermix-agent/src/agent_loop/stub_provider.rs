use super::*;
use std::sync::Arc;


use crate::agent_config::AgentConfig;
use crate::agent_helpers::ToolRegistry;
use crate::context::ContextManager;
    use async_trait::async_trait;
    use futures::stream::{BoxStream, StreamExt};
    use hermix_core::{AgentError, LlmProvider, LlmResponse, Message, StreamChunk, ToolSchema};

    pub struct EmptyProvider;

    #[async_trait]
    impl LlmProvider for EmptyProvider {
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
                message: Message::assistant(""),
                usage: None,
                model: "stub".into(),
                finish_reason: Some("stop".into()),
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
            futures::stream::empty().boxed()
        }
    }

    pub(crate) fn make_reasoning_response(reasoning: &str) -> hermix_core::LlmResponse {
        let mut msg = Message::assistant("");
        msg.reasoning_content = Some(reasoning.to_string());
        hermix_core::LlmResponse {
            message: msg,
            usage: None,
            model: "gpt-4o".to_string(),
            finish_reason: None,
        }
    }

    pub(crate) fn make_empty_response() -> hermix_core::LlmResponse {
        hermix_core::LlmResponse {
            message: Message::assistant(""),
            usage: None,
            model: "gpt-4o".to_string(),
            finish_reason: None,
        }
    }

    pub(crate) fn make_loop_for_classify() -> (AgentLoop, ContextManager) {
        let config = AgentConfig::default();
        // `classify_post_llm_response` is a pure synchronous classifier
        // — it never touches the LLM provider, tool registry, or any
        // IO channel — so an empty stub provider and an empty registry
        // are enough to satisfy the `AgentLoop::new` constructor.
        let loop_ = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(EmptyProvider),
        );
        let ctx = ContextManager::new(100);
        (loop_, ctx)
    }

    #[test]
    pub(crate) fn p4_1_classify_breaks_on_tool_calls() {
        // Tool calls present → Break(r) regardless of content /
        // reasoning state. Caller unwraps the response and dispatches.
        let (loop_, mut ctx) = make_loop_for_classify();
        let mut inner_thinking = 0;
        let mut inner_empty = 0;
        let mut msg = Message::assistant("");
        msg.tool_calls = Some(vec![hermix_core::ToolCall {
            id: "tc1".into(),
            function: hermix_core::FunctionCall {
                name: "noop".into(),
                arguments: "{}".into(),
            },
        }]);
        let action = loop_.classify_post_llm_response(
            hermix_core::LlmResponse {
                message: msg,
                usage: None,
                model: "gpt-4o".to_string(),
                finish_reason: None,
            },
            &mut inner_thinking,
            &mut inner_empty,
            &mut ctx,
        );
        assert!(matches!(action, PostLlmAction::Break(_)));
        assert_eq!(inner_thinking, 0);
        assert_eq!(inner_empty, 0);
        assert_eq!(ctx.get_messages().len(), 0);
    }

    #[test]
    pub(crate) fn p4_1_classify_breaks_on_visible_text() {
        // No tools, but assistant content is non-empty → Break(r).
        // Counter budgets must not be touched.
        let (loop_, mut ctx) = make_loop_for_classify();
        let mut inner_thinking = 0;
        let mut inner_empty = 0;
        let action = loop_.classify_post_llm_response(
            hermix_core::LlmResponse {
                message: Message::assistant("hello world"),
                usage: None,
                model: "gpt-4o".to_string(),
                finish_reason: None,
            },
            &mut inner_thinking,
            &mut inner_empty,
            &mut ctx,
        );
        assert!(matches!(action, PostLlmAction::Break(_)));
        assert_eq!(inner_thinking, 0);
        assert_eq!(inner_empty, 0);
        assert_eq!(ctx.get_messages().len(), 0);
    }

    #[test]
    pub(crate) fn p4_1_classify_reasoning_only_breaks_immediately() {
        // Reasoning-only responses (no visible text, no tools)
        // now Break immediately instead of retrying. The outer loop's
        // `decide_empty_response` handles the post-tool nudge.
        let (mut loop_, mut ctx) = make_loop_for_classify();
        loop_.config.thinking_prefill_max_retries = 2;
        let mut inner_thinking = 0;
        let mut inner_empty = 0;
        let action = loop_.classify_post_llm_response(
            make_reasoning_response("thinking…"),
            &mut inner_thinking,
            &mut inner_empty,
            &mut ctx,
        );
        assert!(matches!(action, PostLlmAction::Break(_)));
        assert_eq!(inner_thinking, 0);
        assert_eq!(inner_empty, 0);
        assert_eq!(ctx.get_messages().len(), 0);
    }

    #[test]
    pub(crate) fn p4_1_classify_retries_empty_within_budget() {
        // No tools, no reasoning, no content → RetryEmpty (not
        // RetryReasoning). The message must NOT be pushed — empty
        // responses are noise that shouldn't pollute the conversation.
        let (mut loop_, mut ctx) = make_loop_for_classify();
        loop_.config.empty_content_max_retries = 1;
        let mut inner_thinking = 0;
        let mut inner_empty = 0;
        let action = loop_.classify_post_llm_response(
            make_empty_response(),
            &mut inner_thinking,
            &mut inner_empty,
            &mut ctx,
        );
        assert!(matches!(action, PostLlmAction::RetryEmpty));
        assert_eq!(inner_thinking, 0);
        assert_eq!(inner_empty, 1);
        assert_eq!(ctx.get_messages().len(), 0);
    }
