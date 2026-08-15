//! Message routing: route_message, slash commands, streaming, context build.
use std::sync::Arc;

use hermix_core::errors::GatewayError;
use hermix_core::types::Message;

use crate::commands::GatewayCommandResult;

use super::*;

impl Gateway {
    // Message routing
    // -----------------------------------------------------------------------

    /// Route an incoming message through the full pipeline:
    /// DM check → session lookup → agent loop → response.
    pub async fn route_message(&self, incoming: &IncomingMessage) -> Result<(), GatewayError> {
        // 1. Check DM authorization if this is a direct message
        if incoming.is_dm {
            let dm_manager = self.dm_manager.read().await;
            let decision = dm_manager
                .handle_dm(&incoming.user_id, &incoming.platform)
                .await;

            match decision {
                DmDecision::Allow => {
                    // Proceed
                }
                DmDecision::Pair { message } => {
                    // Send pairing message and return
                    if let Some(msg) = message {
                        self.send_message(&incoming.platform, &incoming.chat_id, &msg, None)
                            .await?;
                    }
                    return Ok(());
                }
                DmDecision::Deny => {
                    debug!(
                        user_id = incoming.user_id,
                        platform = incoming.platform,
                        "DM denied for unauthorized user"
                    );
                    return Ok(());
                }
            }
        }

        // 2. Get or create session
        let session_key = self.session_manager.compose_session_key(
            &incoming.platform,
            &incoming.chat_id,
            &incoming.user_id,
        );
        let existing_session = self.session_manager.get_session(&session_key).await;
        let session = self
            .session_manager
            .get_or_create_session(&incoming.platform, &incoming.chat_id, &incoming.user_id)
            .await;
        let session_started = existing_session.is_none();
        let session_auto_reset = existing_session
            .as_ref()
            .map(|s| s.created_at != session.created_at)
            .unwrap_or(false);
        if session_started || session_auto_reset {
            self.emit_hook_event(
                "session:start",
                hook_payloads::session_start_from_incoming(
                    incoming,
                    &session_key,
                    if session_started { "new" } else { "auto_reset" },
                ),
            )
            .await;
        }

        // Slash commands are executed directly by the gateway command runtime.
        if incoming.text.trim_start().starts_with('/')
            && self.execute_slash_command(incoming, &session_key).await?
        {
            return Ok(());
        }

        let enriched_text = self
            .enrich_message_with_transcription(&self.enrich_message_with_vision(&incoming.text));
        self.maybe_apply_smart_model_routing(&session_key, &enriched_text)
            .await;

        // 3. Add the user message to the session
        self.session_manager
            .add_message(&session_key, Message::user(enriched_text))
            .await;
        self.bump_input_usage(&session_key, incoming.text.chars().count())
            .await;

        // 4. Get all session messages for the agent loop
        let messages = self.session_manager.get_messages(&session_key).await;

        // 5. Process through agent loop (streaming or non-streaming)
        if self.config.streaming_enabled {
            self.route_streaming(incoming, messages, &session_key)
                .await?;
        } else {
            self.route_non_streaming(incoming, messages, &session_key)
                .await?;
        }

        Ok(())
    }

    pub(crate) async fn execute_slash_command(
        &self,
        incoming: &IncomingMessage,
        session_key: &str,
    ) -> Result<bool, GatewayError> {
        let result = handle_command(&incoming.text);
        if let Some(command_name) = Self::extract_command_name(&incoming.text) {
            let line = incoming.text.trim();
            self.emit_hook_event(
                &format!("command:{}", command_name),
                hook_payloads::command_context(incoming, session_key, command_name.as_str(), line),
            )
            .await;
        }
        let handled = self
            .apply_command_result(incoming, session_key, result)
            .await?;
        Ok(handled)
    }

    pub(crate) async fn apply_command_result(
        &self,
        incoming: &IncomingMessage,
        session_key: &str,
        result: GatewayCommandResult,
    ) -> Result<bool, GatewayError> {
        match result {
            GatewayCommandResult::Reply(text)
            | GatewayCommandResult::ShowHelp(text)
            | GatewayCommandResult::Unknown(text) => {
                self.send_message(&incoming.platform, &incoming.chat_id, &text, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ResetSession(reply) => {
                self.emit_hook_event(
                    "session:end",
                    hook_payloads::session_lifecycle_from_incoming(incoming, session_key),
                )
                .await;
                self.session_manager.reset_session(session_key).await;
                self.emit_hook_event(
                    "session:reset",
                    hook_payloads::session_lifecycle_from_incoming(incoming, session_key),
                )
                .await;
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchModel { model, reply } => {
                let mut states = self.runtime_state.write().await;
                states.entry(session_key.to_string()).or_default().model = Some(model);
                drop(states);
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchPersonality { name, reply } => {
                let mut states = self.runtime_state.write().await;
                states
                    .entry(session_key.to_string())
                    .or_default()
                    .personality = Some(name);
                drop(states);
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ApproveUser { user_id } => {
                let mut dm = self.dm_manager.write().await;
                if !dm.is_admin(&incoming.user_id) {
                    drop(dm);
                    self.send_message(
                        &incoming.platform,
                        &incoming.chat_id,
                        "🚫 /approve requires admin privileges.",
                        None,
                    )
                    .await?;
                    return Ok(true);
                }
                dm.authorize_user(user_id.clone());
                drop(dm);
                self.send_message(
                    &incoming.platform,
                    &incoming.chat_id,
                    &format!("✅ User '{}' has been approved for DM access.", user_id),
                    None,
                )
                .await?;
                Ok(true)
            }
            GatewayCommandResult::DenyUser { user_id } => {
                let mut dm = self.dm_manager.write().await;
                if !dm.is_admin(&incoming.user_id) {
                    drop(dm);
                    self.send_message(
                        &incoming.platform,
                        &incoming.chat_id,
                        "🚫 /deny requires admin privileges.",
                        None,
                    )
                    .await?;
                    return Ok(true);
                }
                dm.deauthorize_user(&user_id);
                drop(dm);
                self.send_message(
                    &incoming.platform,
                    &incoming.chat_id,
                    &format!("⛔ User '{}' has been removed from DM allowlist.", user_id),
                    None,
                )
                .await?;
                Ok(true)
            }
            GatewayCommandResult::StopAgent(reply) => {
                for (task_id, status, _) in self.background_tasks.list_tasks() {
                    if status == TaskStatus::Running {
                        let _ = self.background_tasks.cancel(&task_id);
                    }
                }
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ShowUsage(_) => {
                let text = self.build_usage_text(session_key).await;
                self.send_message(&incoming.platform, &incoming.chat_id, &text, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::CompressContext(_) => {
                let removed = self.compress_context(session_key, 24).await;
                let reply = format!("📦 Context compressed. Removed {} old messages.", removed);
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ShowInsights(text) => {
                self.send_message(&incoming.platform, &incoming.chat_id, &text, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ToggleVerbose(_) => {
                let mut states = self.runtime_state.write().await;
                let state = states.entry(session_key.to_string()).or_default();
                state.verbose = !state.verbose;
                let reply = format!(
                    "📝 Verbose mode: {}",
                    if state.verbose { "ON" } else { "OFF" }
                );
                drop(states);
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ToggleYolo(_) => {
                let mut states = self.runtime_state.write().await;
                let state = states.entry(session_key.to_string()).or_default();
                state.yolo = !state.yolo;
                let reply = format!("🤠 YOLO mode: {}", if state.yolo { "ON" } else { "OFF" });
                drop(states);
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::SetHome { path, reply } => {
                let target = std::path::Path::new(&path);
                let response = if target.exists() && target.is_dir() {
                    let mut states = self.runtime_state.write().await;
                    states.entry(session_key.to_string()).or_default().home = Some(path);
                    reply
                } else {
                    format!("❌ Path not found or not a directory: {}", path)
                };
                self.send_message(&incoming.platform, &incoming.chat_id, &response, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ShowStatus(_) => {
                let text = self.build_status_text(session_key).await;
                self.send_message(&incoming.platform, &incoming.chat_id, &text, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ReloadMcp => {
                let mut generation = self.mcp_reload_generation.write().await;
                *generation += 1;
                let current = *generation;
                drop(generation);
                self.send_message(
                    &incoming.platform,
                    &incoming.chat_id,
                    &format!("🔄 MCP registry reloaded (generation {}).", current),
                    None,
                )
                .await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchProvider { provider, reply } => {
                let mut states = self.runtime_state.write().await;
                states.entry(session_key.to_string()).or_default().provider = Some(provider);
                drop(states);
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchProfile { profile, reply } => {
                let mut states = self.runtime_state.write().await;
                states.entry(session_key.to_string()).or_default().profile = Some(profile);
                drop(states);
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchBranch { branch } => {
                let reply = match branch {
                    Some(name) => {
                        let mut states = self.runtime_state.write().await;
                        states.entry(session_key.to_string()).or_default().branch =
                            Some(name.clone());
                        format!("🌿 Branch context switched to: {}", name)
                    }
                    None => {
                        let branch = self
                            .runtime_state
                            .read()
                            .await
                            .get(session_key)
                            .and_then(|s| s.branch.clone())
                            .unwrap_or_else(|| "main".to_string());
                        format!("🌿 Current branch context: {}", branch)
                    }
                };
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::Rollback { steps } => {
                let mut removed = 0usize;
                for _ in 0..steps {
                    if self
                        .session_manager
                        .pop_last_message(session_key)
                        .await
                        .is_some()
                    {
                        removed += 1;
                    } else {
                        break;
                    }
                }
                self.send_message(
                    &incoming.platform,
                    &incoming.chat_id,
                    &format!("↪️ Rolled back {} message(s).", removed),
                    None,
                )
                .await?;
                Ok(true)
            }
            GatewayCommandResult::CheckUpdate => {
                let version =
                    env_compat("HERMIX_LATEST_VERSION", "HERMES_LATEST_VERSION").unwrap_or_else(|| "latest".to_string());
                self.send_update_notification(&incoming.platform, &incoming.chat_id, &version)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::BackgroundTask { prompt } => {
                let handled = self
                    .handle_background_command(incoming, session_key, &prompt, false)
                    .await?;
                Ok(handled)
            }
            GatewayCommandResult::BtwTask { prompt } => {
                let handled = self
                    .handle_background_command(incoming, session_key, &prompt, true)
                    .await?;
                Ok(handled)
            }
            GatewayCommandResult::ToggleReasoning(_) => {
                let mut states = self.runtime_state.write().await;
                let state = states.entry(session_key.to_string()).or_default();
                state.reasoning = !state.reasoning;
                let reply = format!(
                    "🧠 Reasoning visibility: {}",
                    if state.reasoning { "ON" } else { "OFF" }
                );
                drop(states);
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchFast(_) => {
                let mut states = self.runtime_state.write().await;
                states.entry(session_key.to_string()).or_default().model =
                    Some("openai:gpt-4o-mini".to_string());
                drop(states);
                self.send_message(
                    &incoming.platform,
                    &incoming.chat_id,
                    "⚡ Fast model enabled: openai:gpt-4o-mini",
                    None,
                )
                .await?;
                Ok(true)
            }
            GatewayCommandResult::Retry => {
                let mut messages = self.session_manager.get_messages(session_key).await;
                if matches!(
                    messages.last().map(|m| m.role),
                    Some(MessageRole::Assistant)
                ) {
                    messages.pop();
                }
                if messages.is_empty() {
                    self.send_message(
                        &incoming.platform,
                        &incoming.chat_id,
                        "No previous message to retry.",
                        None,
                    )
                    .await?;
                    return Ok(true);
                }
                self.session_manager
                    .replace_messages(session_key, messages.clone())
                    .await;
                self.route_non_streaming(incoming, messages, session_key)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::Undo => {
                let mut removed = 0usize;
                if let Some(last) = self.session_manager.pop_last_message(session_key).await {
                    removed += 1;
                    if last.role == MessageRole::Assistant {
                        if let Some(prev) = self.session_manager.pop_last_message(session_key).await
                        {
                            if prev.role == MessageRole::User {
                                removed += 1;
                            }
                        }
                    }
                }
                let reply = if removed == 0 {
                    "Nothing to undo.".to_string()
                } else {
                    format!("↩️ Removed {} message(s) from current session.", removed)
                };
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ListTools { filter } => {
                let text = if let Some(ref registry) = *self.tool_registry.read().await {
                    let tools = registry.list_tools();
                    let mut filtered: Vec<_> = if let Some(ref f) = filter {
                        tools
                            .into_iter()
                            .filter(|t| t.name.contains(f) || t.description.contains(f))
                            .collect()
                    } else {
                        tools
                    };
                    filtered.sort_by(|a, b| a.name.cmp(&b.name));
                    let total = filtered.len();
                    let enabled_count = filtered.iter().filter(|t| t.is_enabled).count();
                    let header = format!("🔧 Tools ({}/{} enabled):", enabled_count, total);
                    let list: Vec<String> = filtered
                        .iter()
                        .map(|t| {
                            let status = if t.is_enabled { "✅" } else { "⛔" };
                            format!("  {} `{}` — {}", status, t.name, t.toolset)
                        })
                        .collect();
                    if list.is_empty() {
                        if let Some(f) = filter {
                            format!("🔧 No tools matching filter '{}'.", f)
                        } else {
                            "🔧 No tools registered.".to_string()
                        }
                    } else {
                        format!("{}\n{}", header, list.join("\n"))
                    }
                } else {
                    "🔧 No tool registry available.".to_string()
                };
                self.send_message(&incoming.platform, &incoming.chat_id, &text, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::EnableTool { name } => {
                if let Some(ref registry) = *self.tool_registry.read().await {
                    let count = registry.set_toolset_enabled(&name, true);
                    if count > 0 {
                        self.send_message(
                            &incoming.platform,
                            &incoming.chat_id,
                            &format!(
                                "✅ Enabled {} tool(s) in toolset '{}'.",
                                count, name
                            ),
                            None,
                        )
                        .await?;
                    } else {
                        self.send_message(
                            &incoming.platform,
                            &incoming.chat_id,
                            &format!(
                                "⚠️ No toolset named '{}' found. Use /tools to list available toolsets.",
                                name
                            ),
                            None,
                        )
                        .await?;
                    }
                } else {
                    self.send_message(
                        &incoming.platform,
                        &incoming.chat_id,
                        "🔧 No tool registry available.",
                        None,
                    )
                    .await?;
                }
                Ok(true)
            }
            GatewayCommandResult::DisableTool { name } => {
                if let Some(ref registry) = *self.tool_registry.read().await {
                    let count = registry.set_toolset_enabled(&name, false);
                    if count > 0 {
                        self.send_message(
                            &incoming.platform,
                            &incoming.chat_id,
                            &format!(
                                "⛔ Disabled {} tool(s) in toolset '{}'.",
                                count, name
                            ),
                            None,
                        )
                        .await?;
                    } else {
                        self.send_message(
                            &incoming.platform,
                            &incoming.chat_id,
                            &format!(
                                "⚠️ No toolset named '{}' found. Use /tools to list available toolsets.",
                                name
                            ),
                            None,
                        )
                        .await?;
                    }
                } else {
                    self.send_message(
                        &incoming.platform,
                        &incoming.chat_id,
                        "🔧 No tool registry available.",
                        None,
                    )
                    .await?;
                }
                Ok(true)
            }
            GatewayCommandResult::ListSessions => {
                let sessions = self
                    .session_manager
                    .get_user_sessions(&incoming.user_id)
                    .await;
                let text = if sessions.is_empty() {
                    "📚 No sessions found for your user.".to_string()
                } else {
                    let mut out = String::from("📚 **Your sessions:**\n\n");
                    for s in sessions {
                        let key = self.session_manager.compose_session_key(
                            &s.platform,
                            &s.chat_id,
                            &s.user_id,
                        );
                        out.push_str(&format!(
                            "• `{}` — {} messages, platform `{}` (id `{}`)\n",
                            key,
                            s.messages.len(),
                            s.platform,
                            s.id
                        ));
                    }
                    out.push_str("\nUse `/sessions <key or id>` to switch.");
                    out
                };
                self.send_message(&incoming.platform, &incoming.chat_id, &text, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchSession { session_id } => {
                let sessions = self
                    .session_manager
                    .get_user_sessions(&incoming.user_id)
                    .await;
                let matched = sessions.iter().any(|s| {
                    let key = self.session_manager.compose_session_key(
                        &s.platform,
                        &s.chat_id,
                        &s.user_id,
                    );
                    key == session_id || s.id == session_id
                });
                let msg = if matched {
                    format!(
                        "🔁 Session `{}` matches your account.\n\
                         (Cross-chat transcript routing is not fully wired in this gateway build.)",
                        session_id
                    )
                } else {
                    format!(
                        "❌ No session matching `{}` for your user. Try `/sessions` to list keys.",
                        session_id
                    )
                };
                self.send_message(&incoming.platform, &incoming.chat_id, &msg, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ShowBudget { new_budget } => {
                let mut states = self.runtime_state.write().await;
                let state = states.entry(session_key.to_string()).or_default();
                let msg = match new_budget {
                    Some(b) => {
                        state.budget = Some(b);
                        format!("💰 Usage budget set to {:.4}.", b)
                    }
                    None => match state.budget {
                        Some(b) => format!("💰 Current usage budget: {:.4}.", b),
                        None => {
                            "💰 No usage budget set. Use `/budget <amount>` to set one.".to_string()
                        }
                    },
                };
                drop(states);
                self.send_message(&incoming.platform, &incoming.chat_id, &msg, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::Noop => Ok(true),
        }
    }

    /// Non-streaming message routing: invoke agent, send complete response.
    pub(crate) async fn route_non_streaming(
        &self,
        incoming: &IncomingMessage,
        messages: Vec<Message>,
        session_key: &str,
    ) -> Result<(), GatewayError> {
        self.emit_hook_event(
            "agent:start",
            hook_payloads::agent_start(incoming, session_key, false),
        )
        .await;
        let deferred_messages = Arc::new(StdMutex::new(Vec::new()));
        let deferred_release = Arc::new(AtomicBool::new(false));
        let mut runtime_context = self.build_runtime_context(incoming, session_key).await;
        runtime_context.deferred_post_delivery_messages = Some(deferred_messages.clone());
        runtime_context.deferred_post_delivery_released = Some(deferred_release.clone());
        let context_handler = self.message_handler_with_context.read().await.clone();
        let response_result = if let Some(handler) = context_handler {
            handler(messages, runtime_context).await
        } else {
            let handler = self.message_handler.read().await;
            let handler = handler
                .as_ref()
                .ok_or_else(|| GatewayError::Platform("No message handler configured".into()))?;
            let messages = self.inject_runtime_hints(session_key, messages).await;
            handler(messages).await
        };
        let response = match response_result {
            Ok(text) => text,
            Err(e) => {
                self.flush_post_delivery_messages(
                    &incoming.platform,
                    &incoming.chat_id,
                    deferred_messages.clone(),
                    deferred_release.clone(),
                )
                .await;
                self.emit_hook_event(
                    "agent:end",
                    hook_payloads::agent_end_error(incoming, session_key, false, &e.to_string()),
                )
                .await;
                return Err(e);
            }
        };

        // Add assistant response to session
        self.session_manager
            .add_message(session_key, Message::assistant(&response))
            .await;
        self.bump_output_usage(session_key, response.chars().count())
            .await;

        // Send response back to the platform
        self.send_message(&incoming.platform, &incoming.chat_id, &response, None)
            .await?;
        self.flush_post_delivery_messages(
            &incoming.platform,
            &incoming.chat_id,
            deferred_messages,
            deferred_release,
        )
        .await;
        self.emit_hook_event(
            "agent:end",
            hook_payloads::agent_end_success(
                incoming,
                session_key,
                false,
                response.chars().count(),
            ),
        )
        .await;

        Ok(())
    }

    /// Streaming message routing: progressively edit messages as tokens arrive.
    pub(crate) async fn route_streaming(
        &self,
        incoming: &IncomingMessage,
        messages: Vec<Message>,
        session_key: &str,
    ) -> Result<(), GatewayError> {
        self.emit_hook_event(
            "agent:start",
            hook_payloads::agent_start(incoming, session_key, true),
        )
        .await;
        let deferred_messages = Arc::new(StdMutex::new(Vec::new()));
        let deferred_release = Arc::new(AtomicBool::new(false));
        let mut runtime_context = self.build_runtime_context(incoming, session_key).await;
        runtime_context.deferred_post_delivery_messages = Some(deferred_messages.clone());
        runtime_context.deferred_post_delivery_released = Some(deferred_release.clone());
        let context_handler = self.streaming_handler_with_context.read().await.clone();
        let legacy_messages = self
            .inject_runtime_hints(session_key, messages.clone())
            .await;

        // Start a stream
        let stream_handle = self
            .stream_manager
            .start_stream(&incoming.platform, &incoming.chat_id)
            .await;
        let stream_id = stream_handle.id.clone();

        // Send an initial placeholder message
        self.send_message(&incoming.platform, &incoming.chat_id, "...", None)
            .await?;

        // Set up the chunk callback that updates the stream and edits the message
        let stream_manager = self.stream_manager.clone();
        let platform = incoming.platform.clone();
        let chat_id = incoming.chat_id.clone();
        let gateway_adapters = self.adapters.read().await.clone();
        let sid = stream_id.clone();

        let on_chunk: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |chunk: String| {
            let sm = stream_manager.clone();
            let sid = sid.clone();
            let platform = platform.clone();
            let chat_id = chat_id.clone();
            let adapters = gateway_adapters.clone();

            tokio::spawn(async move {
                if let Some(should_flush) = sm.update_stream(&sid, &chunk).await {
                    if should_flush {
                        if let Some(content) = sm.get_stream_content(&sid).await {
                            if let Some(adapter) = adapters.get(&platform) {
                                // For streaming, we'd need the message_id from the initial send.
                                // This is a simplified version.
                                let _ = adapter.send_message(&chat_id, &content, None).await;
                            }
                        }
                    }
                }
            });
        });

        // Invoke the streaming handler
        let response_result = if let Some(handler) = context_handler {
            handler(messages, runtime_context, on_chunk).await
        } else {
            let handler = self.streaming_handler.read().await;
            let handler = handler
                .as_ref()
                .ok_or_else(|| GatewayError::Platform("No streaming handler configured".into()))?;
            handler(legacy_messages, on_chunk).await
        };
        let response = match response_result {
            Ok(text) => text,
            Err(e) => {
                self.flush_post_delivery_messages(
                    &incoming.platform,
                    &incoming.chat_id,
                    deferred_messages.clone(),
                    deferred_release.clone(),
                )
                .await;
                self.emit_hook_event(
                    "agent:end",
                    hook_payloads::agent_end_error(incoming, session_key, true, &e.to_string()),
                )
                .await;
                return Err(e);
            }
        };

        // Finish the stream
        self.stream_manager.finish_stream(&stream_id).await;

        // Add assistant response to session
        self.session_manager
            .add_message(session_key, Message::assistant(&response))
            .await;
        self.bump_output_usage(session_key, response.chars().count())
            .await;
        self.flush_post_delivery_messages(
            &incoming.platform,
            &incoming.chat_id,
            deferred_messages,
            deferred_release,
        )
        .await;
        self.emit_hook_event(
            "agent:end",
            hook_payloads::agent_end_success(incoming, session_key, true, response.chars().count()),
        )
        .await;

        Ok(())
    }

    pub(crate) async fn inject_runtime_hints(
        &self,
        session_key: &str,
        messages: Vec<Message>,
    ) -> Vec<Message> {
        let state = self
            .runtime_state
            .read()
            .await
            .get(session_key)
            .cloned()
            .unwrap_or_default();

        let mut hints = Vec::new();
        if let Some(model) = state.model {
            hints.push(format!("model={}", model));
        }
        if let Some(provider) = state.provider {
            hints.push(format!("provider={}", provider));
        }
        if let Some(profile) = state.profile {
            hints.push(format!("profile={}", profile));
        }
        if let Some(branch) = state.branch {
            hints.push(format!("branch={}", branch));
        }
        if hints.is_empty() {
            return messages;
        }

        let mut out = Vec::with_capacity(messages.len() + 1);
        out.push(Message::system(format!(
            "[gateway_runtime]\n{}",
            hints.join("\n")
        )));
        out.extend(messages);
        out
    }

    pub(crate) async fn build_runtime_context(
        &self,
        incoming: &IncomingMessage,
        session_key: &str,
    ) -> GatewayRuntimeContext {
        let state = self
            .runtime_state
            .read()
            .await
            .get(session_key)
            .cloned()
            .unwrap_or_default();
        let mcp_reload_generation = *self.mcp_reload_generation.read().await;

        GatewayRuntimeContext {
            session_key: session_key.to_string(),
            platform: incoming.platform.clone(),
            chat_id: incoming.chat_id.clone(),
            user_id: incoming.user_id.clone(),
            model: state.model,
            provider: state.provider,
            profile: state.profile,
            branch: state.branch,
            personality: state.personality,
            home: state.home,
            verbose: state.verbose,
            yolo: state.yolo,
            reasoning: state.reasoning,
            mcp_reload_generation,
            deferred_post_delivery_messages: None,
            deferred_post_delivery_released: None,
        }
    }

    pub(crate) async fn flush_post_delivery_messages(
        &self,
        platform: &str,
        chat_id: &str,
        pending: Arc<StdMutex<Vec<String>>>,
        released: Arc<AtomicBool>,
    ) {
        released.store(true, Ordering::Release);
        let queued = match pending.lock() {
            Ok(mut guard) => std::mem::take(&mut *guard),
            Err(_) => Vec::new(),
        };
        for message in queued {
            if let Err(e) = self.send_message(platform, chat_id, &message, None).await {
                warn!(
                    platform = platform,
                    chat_id = chat_id,
                    error = %e,
                    "Failed to flush deferred post-delivery message"
                );
            }
        }
    }
}
