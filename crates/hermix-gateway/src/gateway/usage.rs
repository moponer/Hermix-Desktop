//! Session usage accounting, context compression, status text, background commands.
use hermix_core::errors::GatewayError;
use hermix_core::types::Message;


use super::*;

impl Gateway {
    pub(crate) async fn bump_input_usage(&self, session_key: &str, chars: usize) {
        let mut usage = self.usage_stats.write().await;
        let stat = usage.entry(session_key.to_string()).or_default();
        stat.user_messages += 1;
        stat.input_chars += chars as u64;
        stat.last_updated_at = Some(Utc::now());
    }

    pub(crate) async fn bump_output_usage(&self, session_key: &str, chars: usize) {
        let mut usage = self.usage_stats.write().await;
        let stat = usage.entry(session_key.to_string()).or_default();
        stat.assistant_messages += 1;
        stat.output_chars += chars as u64;
        stat.last_updated_at = Some(Utc::now());
    }

    pub(crate) async fn build_usage_text(&self, session_key: &str) -> String {
        let usage = self.usage_stats.read().await;
        let stat = usage.get(session_key).cloned().unwrap_or_default();
        let approx_input_tokens = stat.input_chars / 4;
        let approx_output_tokens = stat.output_chars / 4;
        format!(
            "📊 Usage\n- user messages: {}\n- assistant messages: {}\n- input chars: {} (~{} tokens)\n- output chars: {} (~{} tokens)",
            stat.user_messages,
            stat.assistant_messages,
            stat.input_chars,
            approx_input_tokens,
            stat.output_chars,
            approx_output_tokens
        )
    }

    pub(crate) async fn compress_context(&self, session_key: &str, max_messages: usize) -> usize {
        let current = self.session_manager.get_messages(session_key).await;
        if current.len() <= max_messages {
            return 0;
        }

        let mut compressed = Vec::new();
        if let Some(first) = current.first() {
            if first.role == MessageRole::System {
                compressed.push(first.clone());
            }
        }
        let keep_tail = max_messages.saturating_sub(compressed.len());
        let mut tail: Vec<Message> = current.iter().rev().take(keep_tail).cloned().collect();
        tail.reverse();
        compressed.extend(tail);

        let removed = current.len().saturating_sub(compressed.len());
        self.session_manager
            .replace_messages(session_key, compressed)
            .await;
        removed
    }

    pub(crate) async fn build_status_text(&self, session_key: &str) -> String {
        let state = self
            .runtime_state
            .read()
            .await
            .get(session_key)
            .cloned()
            .unwrap_or_default();
        let usage = self
            .usage_stats
            .read()
            .await
            .get(session_key)
            .cloned()
            .unwrap_or_default();
        let messages = self.session_manager.get_messages(session_key).await;
        let running_tasks = self
            .background_tasks
            .list_tasks()
            .into_iter()
            .filter(|(_, status, _)| *status == TaskStatus::Running)
            .count();

        let hook_line = if let Some(reg) = self.hook_registry.read().await.as_ref() {
            let (inv, ok, err) = reg.stats_snapshot();
            format!("\n- hook handlers: invoked={} ok={} err={}", inv, ok, err)
        } else {
            String::new()
        };

        let body = format!(
            "🧭 Gateway status\n- model: {}\n- provider: {}\n- profile: {}\n- branch: {}\n- personality: {}\n- reasoning: {}\n- verbose: {}\n- yolo: {}\n- home: {}\n- messages in session: {}\n- running background tasks: {}\n- mcp generation: {}\n- input/output chars: {}/{}",
            state.model.unwrap_or_else(|| "default".to_string()),
            state.provider.unwrap_or_else(|| "default".to_string()),
            state.profile.unwrap_or_else(|| "default".to_string()),
            state.branch.unwrap_or_else(|| "main".to_string()),
            state.personality.unwrap_or_else(|| "default".to_string()),
            if state.reasoning { "ON" } else { "OFF" },
            if state.verbose { "ON" } else { "OFF" },
            if state.yolo { "ON" } else { "OFF" },
            state.home.unwrap_or_else(|| "(not set)".to_string()),
            messages.len(),
            running_tasks,
            *self.mcp_reload_generation.read().await,
            usage.input_chars,
            usage.output_chars
        );
        format!("{}{}", body, hook_line)
    }

    pub(crate) async fn handle_background_command(
        &self,
        incoming: &IncomingMessage,
        session_key: &str,
        prompt: &str,
        isolated_context: bool,
    ) -> Result<bool, GatewayError> {
        let trimmed = prompt.trim();
        if trimmed.eq_ignore_ascii_case("list") {
            let tasks = self.background_tasks.list_tasks();
            let summary = if tasks.is_empty() {
                "No background tasks.".to_string()
            } else {
                let mut out = String::from("🧵 Background tasks:\n");
                for (id, status, task_prompt) in tasks {
                    out.push_str(&format!("- {} [{:?}] {}\n", id, status, task_prompt));
                }
                out
            };
            self.send_message(&incoming.platform, &incoming.chat_id, &summary, None)
                .await?;
            return Ok(true);
        }
        if let Some(task_id) = trimmed.strip_prefix("cancel ").map(str::trim) {
            let ok = self.background_tasks.cancel(task_id);
            let msg = if ok {
                format!("Cancelled background task {}", task_id)
            } else {
                format!("Task {} was not running or not found", task_id)
            };
            self.send_message(&incoming.platform, &incoming.chat_id, &msg, None)
                .await?;
            return Ok(true);
        }
        if let Some(task_id) = trimmed.strip_prefix("status ").map(str::trim) {
            let msg = match self.background_tasks.get_status(task_id) {
                Some(TaskStatus::Running) => format!("Task {} is running", task_id),
                Some(TaskStatus::Completed) => {
                    let result = self
                        .background_tasks
                        .get_result(task_id)
                        .unwrap_or_default();
                    format!("Task {} completed.\n{}", task_id, result)
                }
                Some(TaskStatus::Failed(err)) => format!("Task {} failed: {}", task_id, err),
                Some(TaskStatus::Cancelled) => format!("Task {} was cancelled", task_id),
                None => format!("Task {} not found", task_id),
            };
            self.send_message(&incoming.platform, &incoming.chat_id, &msg, None)
                .await?;
            return Ok(true);
        }

        let task_id = if isolated_context {
            Self::python_async_task_id("btw")
        } else {
            Self::python_async_task_id("bg")
        };
        self.background_tasks
            .submit_with_id(task_id.clone(), trimmed.to_string())
            .map_err(GatewayError::Platform)?;

        let preview = Self::gateway_command_preview(trimmed);
        let ack = if isolated_context {
            format!("💬 /btw: \"{}\"\nReply will appear here shortly.", preview)
        } else {
            format!(
                "🔄 Background task started: \"{}\"\nTask ID: {}\nYou can keep chatting — results will appear when done.",
                preview, task_id
            )
        };
        self.send_message(&incoming.platform, &incoming.chat_id, &ack, None)
            .await?;

        let legacy_handler = self.message_handler.read().await.as_ref().cloned();
        let context_handler = self
            .message_handler_with_context
            .read()
            .await
            .as_ref()
            .cloned();
        if context_handler.is_none() && legacy_handler.is_none() {
            return Err(GatewayError::Platform(
                "No message handler configured".into(),
            ));
        }
        let manager = self.background_tasks.clone();
        let task_id_for_task = task_id.clone();
        // Python `GatewayRunner._run_background_task`: only `user_message=prompt` (fresh session).
        // Python `_run_btw_task`: `conversation_history` snapshot + ephemeral user turn (no tools).
        let original_messages = if isolated_context {
            let mut history = self.session_manager.get_messages(session_key).await;
            let btw_user = format!(
                "[Ephemeral /btw side question. Answer using the conversation \
                 context. No tools available. Be direct and concise.]\n\n{}",
                trimmed
            );
            history.push(Message::user(btw_user));
            history
        } else {
            vec![Message::user(trimmed)]
        };
        let legacy_messages = original_messages.clone();
        let runtime_context = self.build_runtime_context(incoming, session_key).await;
        tokio::spawn(async move {
            let result = if let Some(handler) = context_handler {
                handler(original_messages, runtime_context).await
            } else if let Some(handler) = legacy_handler {
                handler(legacy_messages).await
            } else {
                Err(GatewayError::Platform(
                    "No message handler configured".into(),
                ))
            };

            match result {
                Ok(result) => manager.complete(&task_id_for_task, result),
                Err(err) => manager.fail(&task_id_for_task, err.to_string()),
            }
        });

        Ok(true)
    }
}
