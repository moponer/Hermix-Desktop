//! Misc helpers: command prefs, message send, accessors, watchers, enrich, config load.
use std::sync::Arc;

use hermix_core::errors::GatewayError;
use hermix_core::types::Message;

use crate::session::SessionManager;
use crate::stream::StreamManager;

use super::*;

impl Gateway {
    /// `preview = prompt[:60] + ("..." if len(prompt) > 60 else "")` (Python gateway).
    pub(crate) fn gateway_command_preview(prompt: &str) -> String {
        let t = prompt.trim();
        let mut it = t.chars();
        let head: String = it.by_ref().take(60).collect();
        if it.next().is_some() {
            format!("{}...", head)
        } else {
            head
        }
    }

    /// Python: `f"{kind}_{%H%M%S}_{os.urandom(3).hex()}"` style task ids (`bg_…`, `btw_…`).
    pub(crate) fn python_async_task_id(kind: &str) -> String {
        let ts = chrono::Utc::now().format("%H%M%S");
        let salt = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| (d.subsec_nanos() as u64) ^ d.as_secs().wrapping_mul(0x9e37_79b9_85f0_a7b5))
            .unwrap_or(0xABCDEF);
        format!("{}_{}_{:06x}", kind, ts, salt & 0xFFFFFF)
    }

    pub(crate) fn extract_command_name(text: &str) -> Option<String> {
        let trimmed = text.trim_start();
        if !trimmed.starts_with('/') {
            return None;
        }
        let token = trimmed[1..].split_whitespace().next()?.trim();
        if token.is_empty() {
            return None;
        }
        Some(token.to_ascii_lowercase())
    }

    // -----------------------------------------------------------------------
    // Message sending (delegates to adapters)
    // -----------------------------------------------------------------------

    /// Send a text message to a specific platform chat.
    pub async fn send_message(
        &self,
        platform: &str,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        let adapter = self.get_adapter(platform).await.ok_or_else(|| {
            GatewayError::Platform(format!("No adapter registered for platform: {}", platform))
        })?;
        adapter.send_message(chat_id, text, parse_mode).await
    }

    /// Edit an existing message on a specific platform chat.
    pub async fn edit_message(
        &self,
        platform: &str,
        chat_id: &str,
        message_id: &str,
        text: &str,
    ) -> Result<(), GatewayError> {
        let adapter = self.get_adapter(platform).await.ok_or_else(|| {
            GatewayError::Platform(format!("No adapter registered for platform: {}", platform))
        })?;
        adapter.edit_message(chat_id, message_id, text).await
    }

    /// Send a file to a specific platform chat with an optional caption.
    pub async fn send_file(
        &self,
        platform: &str,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let adapter = self.get_adapter(platform).await.ok_or_else(|| {
            GatewayError::Platform(format!("No adapter registered for platform: {}", platform))
        })?;
        adapter.send_file(chat_id, file_path, caption).await
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    /// Get a reference to the session manager.
    pub fn session_manager(&self) -> &Arc<SessionManager> {
        &self.session_manager
    }

    /// Get a reference to the stream manager.
    pub fn stream_manager(&self) -> &Arc<StreamManager> {
        &self.stream_manager
    }

    /// Get a reference to the gateway config.
    pub fn config(&self) -> &GatewayConfig {
        &self.config
    }

    /// List the names of all registered adapters.
    pub async fn adapter_names(&self) -> Vec<String> {
        self.adapters.read().await.keys().cloned().collect()
    }

    /// Periodically expires inactive sessions.
    pub async fn session_expiry_watcher(&self, interval_secs: u64) {
        let mut ticker =
            tokio::time::interval(std::time::Duration::from_secs(interval_secs.max(30)));
        loop {
            ticker.tick().await;
            let expired = self.session_manager.expire_idle_sessions().await;
            if expired > 0 {
                tracing::info!(expired, "Expired idle sessions");
            }
        }
    }

    /// Monitors adapter health and attempts reconnect through stop/start.
    pub async fn platform_reconnect_watcher(&self, interval_secs: u64) {
        let mut ticker =
            tokio::time::interval(std::time::Duration::from_secs(interval_secs.max(20)));
        loop {
            ticker.tick().await;
            let snapshot = self.adapters.read().await.clone();
            for (name, adapter) in snapshot {
                if !adapter.is_running() {
                    tracing::warn!(platform = %name, "Adapter appears offline, reconnecting");
                    let _ = adapter.start().await;
                }
            }
        }
    }

    /// Prunes stale gateway maps and adapter-side caches (tokens, dedup tables).
    pub async fn gateway_cleanup_watcher(&self, interval_secs: u64) {
        let mut ticker =
            tokio::time::interval(std::time::Duration::from_secs(interval_secs.max(60)));
        loop {
            ticker.tick().await;
            self.run_internal_maintenance_once().await;
        }
    }

    pub(crate) async fn run_internal_maintenance_once(&self) {
        let active: std::collections::HashSet<String> = self
            .session_manager
            .list_session_keys()
            .await
            .into_iter()
            .collect();
        {
            let mut rs = self.runtime_state.write().await;
            rs.retain(|k, _| active.contains(k));
        }
        {
            let mut us = self.usage_stats.write().await;
            us.retain(|k, _| active.contains(k));
        }
        let adapters = self.adapters.read().await.clone();
        for adapter in adapters.values() {
            adapter.maintenance_prune().await;
        }
    }

    /// Attach vision hint for image-bearing messages.
    pub fn enrich_message_with_vision(&self, text: &str) -> String {
        if text.contains("http://") || text.contains("https://") {
            format!("[vision_candidate]\n{}", text)
        } else {
            text.to_string()
        }
    }

    /// Attach transcription hint for audio-bearing messages.
    pub fn enrich_message_with_transcription(&self, text: &str) -> String {
        if text.contains(".mp3") || text.contains(".wav") || text.contains(".m4a") {
            format!("[transcription_candidate]\n{}", text)
        } else {
            text.to_string()
        }
    }

    /// Build deterministic signature for config-change detection.
    pub fn agent_config_signature(&self) -> String {
        let s = serde_json::to_string(&self.config).unwrap_or_default();
        format!("{:x}", md5::compute(s))
    }

    /// Load optional prefill messages.
    pub fn load_prefill_messages(&self, path: &std::path::Path) -> Vec<Message> {
        let Ok(content) = std::fs::read_to_string(path) else {
            return vec![];
        };
        content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(Message::user)
            .collect()
    }

    /// Load optional ephemeral system prompt.
    pub fn load_ephemeral_system_prompt(&self, path: &std::path::Path) -> Option<String> {
        std::fs::read_to_string(path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Resolve model routing candidate for a message (static heuristics only; no adaptive policy store).
    pub fn load_smart_model_routing(&self, text: &str) -> Option<String> {
        Self::heuristic_model_hint(text)
    }

    pub(crate) fn heuristic_model_hint(text: &str) -> Option<String> {
        if text.len() > 2000 || text.contains("analyze") || text.contains("refactor") {
            Some("openai:gpt-4o".to_string())
        } else if text.contains("quick") || text.contains("summary") {
            Some("openai:gpt-4o-mini".to_string())
        } else {
            None
        }
    }

    /// Authorize user based on DM manager and platform context.
    pub async fn is_user_authorized(&self, user_id: &str, platform: &str) -> bool {
        let dm = self.dm_manager.read().await;
        dm.is_authorized(user_id) || dm.handle_dm(user_id, platform).await == DmDecision::Allow
    }

    /// Send update notification message to a chat.
    pub async fn send_update_notification(
        &self,
        platform: &str,
        chat_id: &str,
        latest_version: &str,
    ) -> Result<(), GatewayError> {
        let msg = format!("Update available: Hermes {}", latest_version);
        self.send_message(platform, chat_id, &msg, None).await
    }

    /// Watch external process output and forward to a callback.
    pub async fn run_process_watcher(
        &self,
        mut child: tokio::process::Child,
        on_output: Arc<dyn Fn(String) + Send + Sync>,
    ) -> Result<(), GatewayError> {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| GatewayError::Platform("Process has no stdout".into()))?;
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines
            .next_line()
            .await
            .map_err(|e| GatewayError::Platform(format!("Watcher read error: {}", e)))?
        {
            on_output(line);
        }
        Ok(())
    }

    pub(crate) async fn maybe_apply_smart_model_routing(&self, session_key: &str, text: &str) {
        let has_model = self
            .runtime_state
            .read()
            .await
            .get(session_key)
            .and_then(|s| s.model.clone())
            .is_some();
        if has_model {
            return;
        }
        if let Some(model) = Self::heuristic_model_hint(text) {
            let mut states = self.runtime_state.write().await;
            states.entry(session_key.to_string()).or_default().model = Some(model);
        }
    }
}
