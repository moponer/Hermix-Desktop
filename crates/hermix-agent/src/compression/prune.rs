//! Old tool-result pruning (Pass 1).


use hermix_core::{
    Message, MessageRole,
};
use hermix_core::types::ContentPart;

use super::*;

impl ContextCompressor {
    /// Replace the contents of old, oversized `tool` messages with
    /// informative 1-line summaries (not blank placeholders). Also
    /// deduplicates identical tool results and truncates large tool_call
    /// arguments while preserving JSON validity.
    /// Returns `(messages, dedup_count, pruned_count)` where:
    /// - `dedup_count` — number of messages replaced with the
    /// `[Duplicate tool output]` marker (Pass 1, MD5 content-hash
    /// dedup). Previously hard-coded to 0 in `CompressionMetrics`,
    /// this is what the dashboard now actually shows.
    /// - `pruned_count` — total of all pruning passes (dedup +
    /// summary replacement + screenshot stripping + argument
    /// truncation). This is the historical "pruned" counter the
    /// breaker and logs already consumed.
    /// if `pin` is `Some`, messages that
    /// match the pin filter are preserved verbatim — every pass
    /// below short-circuits on pinned indices. user-text
    /// dedup runs *before* this method (see
    /// `compression_pipeline::dedup_adjacent_user`) and is also
    /// pin-aware.
    pub fn prune_old_tool_results(
        &self,
        messages: &[Message],
        protect_tail_count: usize,
        protect_tail_tokens: Option<u64>,
        pin: Option<&crate::compression_pipeline::PinConfig>,
    ) -> (Vec<Message>, usize, usize) {
        if messages.is_empty() {
            return (Vec::new(), 0, 0);
        }

        let n = messages.len();
        let mut result: Vec<Message> = messages.to_vec();
        let mut pruned = 0;
        let mut deduped = 0;

        // pin filter. Build a `bool` slice
        // once so per-iteration checks stay O(1). When `pin` is
        // `None`, every index is "not pinned" and the existing
        // behavior is preserved bit-for-bit.
        let pinned: Vec<bool> = match pin {
            Some(p) => result.iter().map(|m| p.is_pinned(m)).collect(),
            None => vec![false; n],
        };
        let is_pinned = |i: usize| -> bool { pinned.get(i).copied().unwrap_or(false) };

        let prune_boundary = if let Some(budget) = protect_tail_tokens.filter(|b| *b > 0) {
            let mut accumulated: u64 = 0;
            let mut boundary = n;
            let min_protect = protect_tail_count.min(n.saturating_sub(1));
            for i in (0..n).rev() {
                let msg = &result[i];
                let msg_tokens = estimate_message_tokens(msg) as u64;
                if accumulated + msg_tokens > budget && (n - i) >= min_protect {
                    boundary = i;
                    break;
                }
                accumulated += msg_tokens;
                boundary = i;
            }
            boundary.max(n.saturating_sub(min_protect))
        } else {
            n.saturating_sub(protect_tail_count)
        };

        // Pass 1: Deduplicate identical tool results (MD5 hash).
        let mut content_hashes: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for i in (0..prune_boundary).rev() {
            if is_pinned(i) {
                continue;
            }
            let content = result[i].content.as_deref().unwrap_or("");
            if result[i].role != MessageRole::Tool || content.len() < 200 {
                continue;
            }
            let h = format!("{:016x}", md5::compute(content.as_bytes()));
            if let Some(&prev_idx) = content_hashes.get(&h) {
                let _ = prev_idx; // keep only the most recent copy
                result[i].content = Some(
                    "[ALREADY EXECUTED] [Duplicate tool output — same content as a more recent call]".into(),
                );
                pruned += 1;
                deduped += 1;
            } else {
                content_hashes.insert(h, i);
            }
        }

        // Pass 1.5: SmartCrusher — compress large JSON arrays in tool results.
        // This runs before the summary pass to reduce token count while
        // preserving structured data. Content-aware compression preserves
        // errors, anomalies, and schema information.
        for (i, msg) in result.iter_mut().take(prune_boundary).enumerate() {
            if is_pinned(i) || msg.role != MessageRole::Tool {
                continue;
            }
            let content = match msg.content.as_deref() {
                Some(c) => c,
                None => continue,
            };
            // Skip already-processed or very short content
            if content.len() < 500
                || content.starts_with("[Duplicate tool output")
                || content.starts_with("[ALREADY EXECUTED]")
            {
                continue;
            }
            if let Some(crushed) = crate::json_crusher::try_crush(content) {
                if crushed.ratio > 0.1 {
                    // Only apply if we save more than 10%
                    if !self.config.quiet_mode {
                        tracing::info!(
                            "SmartCrusher: compressed tool result #{} ({} -> {} items, {:.0}% savings)",
                            i,
                            crushed.items_before,
                            crushed.items_after,
                            crushed.ratio * 100.0,
                        );
                    }
                    msg.content = Some(crushed.value.to_string());
                    pruned += 1;
                }
            }
        }

        // Pass 1.6: Semantic Dedup — detect and merge semantically similar
        // messages (paraphrases, repeated questions). Uses Jaccard similarity
        // on token sets, no embedding model required.
        let dedup_config = crate::semantic_dedup::SemanticDedupConfig::default();
        let dedup_result = crate::semantic_dedup::semantic_dedup(&result, &dedup_config);
        if dedup_result.removed_count > 0 {
            if !self.config.quiet_mode {
                tracing::info!(
                    "Semantic dedup: removed {} similar messages ({} tokens saved)",
                    dedup_result.removed_count,
                    dedup_result.tokens_saved,
                );
            }
            result = dedup_result.messages;
            pruned += dedup_result.removed_count;
        }

        // Pass 2: Replace old tool results with informative summaries.
        // Build tool_call_id -> (tool_name, arguments) map for summary generation.
        let mut call_id_to_tool: std::collections::HashMap<String, (String, String)> =
            std::collections::HashMap::new();
        for msg in &result {
            if msg.role == MessageRole::Assistant {
                if let Some(tcs) = msg.tool_calls.as_ref() {
                    for tc in tcs {
                        call_id_to_tool.insert(
                            tc.id.clone(),
                            (tc.function.name.clone(), tc.function.arguments.clone()),
                        );
                    }
                }
            }
        }

        for (i, msg) in result.iter_mut().take(prune_boundary).enumerate() {
            if is_pinned(i) || msg.role != MessageRole::Tool {
                continue;
            }
            let content = msg.content.as_deref().unwrap_or("");
            if content.is_empty()
                || content == PRUNED_TOOL_PLACEHOLDER
                || content.starts_with("[Duplicate tool output")
                || content.starts_with("[terminal]")
                || content.starts_with("[read_file]")
                || content.starts_with("[write_file]")
                || content.starts_with("[search_files]")
                || content.starts_with("[patch]")
            {
                continue;
            }
            if content.len() > 200 {
                let call_id = msg.tool_call_id.as_deref().unwrap_or("");
                let (tool_name, tool_args) = call_id_to_tool
                    .get(call_id)
                    .cloned()
                    .unwrap_or_else(|| ("unknown".into(), String::new()));
                let summary = summarize_tool_result(&tool_name, &tool_args, content);
                msg.content = Some(summary);
                pruned += 1;
            }
        }

        // Pass 3: Truncate large tool_call arguments in assistant messages
        // outside the protected tail.
        for (i, msg) in result.iter_mut().take(prune_boundary).enumerate() {
            if is_pinned(i) || msg.role != MessageRole::Assistant {
                continue;
            }
            if let Some(ref mut tcs) = msg.tool_calls {
                for tc in tcs.iter_mut() {
                    if tc.function.arguments.len() > 500 {
                        tc.function.arguments =
                            truncate_tool_call_args_json(&tc.function.arguments);
                    }
                }
            }
        }

        // Pass 4: Strip old screenshots/images from tool results.
        // Replace base64-encoded image data with a placeholder to prevent
        // re-shipping multi-MB blobs across compression passes.
        for (i, msg) in result.iter_mut().take(prune_boundary).enumerate() {
            if is_pinned(i) || msg.role != MessageRole::Tool {
                continue;
            }
            if let Some(ref mut content) = msg.content {
                // Check for multimodal content (JSON with _multimodal flag)
                if content.starts_with('{') && content.contains("_multimodal") {
                    *content = "[screenshot removed to save context]".to_string();
                    pruned += 1;
                    continue;
                }
                // Check for base64 image data in text content
                if content.len() > 10_000 && (
                    content.contains("data:image/") ||
                    content.contains("\"_image_data\"") ||
                    content.contains("base64,") && content.len() > 50_000
                ) {
                    *content = "[image data removed to save context]".to_string();
                    pruned += 1;
                }
            }
        }

        //Pass 4.5 (, Bug #4): strip base64 image data from
        // *user* messages' `content_parts`. The legacy Pass 4 above
        // only catches Tool-role content — it skips anything where
        // `msg.role != MessageRole::Tool`. The `read_image` tool
        // result handling at `run_loop.rs:510` injects a fresh
        // `Message::user_with_image(...)` (User role + content_parts
        // with one Text + one ImageUrl) on every successful read, so
        // each compression pass leaves the full base64 payload in
        // place and re-ships it on every subsequent LLM turn —
        // context blows up the moment one read_image is invoked.
        // Pinned messages are still skipped (existing contract); the
        // kept-text-part already carries the `[Image loaded from:
        // <path>]` hint from `read_image` so the LLM can re-invoke
        // read_image for higher resolution if needed.
        for (i, msg) in result.iter_mut().take(prune_boundary).enumerate() {
            if is_pinned(i) {
                continue;
            }
            if msg.role != MessageRole::User {
                continue;
            }
            let Some(parts) = msg.content_parts.as_mut() else {
                continue;
            };
            let had_image = parts
                .iter()
                .any(|p| matches!(p, ContentPart::ImageUrl { .. }));
            if !had_image {
                continue;
            }
            let mut kept: Vec<ContentPart> = Vec::with_capacity(parts.len() + 1);
            for part in parts.drain(..) {
                match part {
                    ContentPart::Text { text } => {
                        if !text.is_empty() {
                            kept.push(ContentPart::Text { text });
                        }
                    }
                    ContentPart::ImageUrl { .. } => {
                        // Drop the base64 payload. The companion Text
                        // part (preserved above) already names the
                        // file path so a follow-up read_image call
                        // can recover full-resolution data.
                    }
                }
            }
            kept.push(ContentPart::Text {
                text: "[image data removed to save context]".to_string(),
            });
            msg.content_parts = if kept.is_empty() { None } else { Some(kept) };
            pruned += 1;
        }

        (result, deduped, pruned)
    }
}
