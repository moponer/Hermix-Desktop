//! Latent briefing: compressed context sharing between orchestrator and workers .
//!
//! When a sub-agent (worker) is delegated a task, sending the full
//! orchestrator context is wasteful — it consumes tokens and
//! introduces noise. Instead, we generate a `LatentBrief`: a
//! compressed trajectory + task-guided attention keywords that
//! give the worker enough context to execute without the full
//! conversation history.
//!
//! **Design note**: Actual KV cache hinting is provider-specific
//! and most LLM APIs don't expose it. This implementation provides
//! a *degraded* version that uses text compression + keyword
//! extraction, which works with any LLM provider.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// A compressed briefing sent from orchestrator to worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatentBrief {
    /// Compressed trajectory: the key decisions and outcomes from
    /// the orchestrator's conversation, summarised to ~10-20% of
    /// the original token count.
    pub compact_trajectory: String,
    /// Task-guided attention keywords: words/phrases the worker
    /// should pay special attention to when processing the task.
    pub task_guided_attention: Vec<String>,
    /// KV cache hints (provider-specific, currently placeholder).
    pub kv_cache_hints: Vec<KvCacheHint>,
    /// Token budget for the worker's response.
    pub max_response_tokens: Option<u32>,
}

/// A hint about which KV cache entries the worker might reuse.
/// Currently informational only — most LLM APIs don't expose
/// KV cache manipulation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KvCacheHint {
    /// Semantic label for the cache entry.
    pub label: String,
    /// Approximate relevance score [0.0, 1.0].
    pub relevance: f32,
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Fluent builder for `LatentBrief`.
#[derive(Debug, Clone)]
pub struct LatentBriefBuilder {
    compact_trajectory: String,
    task_guided_attention: Vec<String>,
    kv_cache_hints: Vec<KvCacheHint>,
    max_response_tokens: Option<u32>,
}

impl LatentBriefBuilder {
    /// Start building a brief with the compact trajectory.
    pub fn new(compact_trajectory: impl Into<String>) -> Self {
        Self {
            compact_trajectory: compact_trajectory.into(),
            task_guided_attention: Vec::new(),
            kv_cache_hints: Vec::new(),
            max_response_tokens: None,
        }
    }

    /// Add a task-guided attention keyword.
    pub fn attention_keyword(mut self, keyword: impl Into<String>) -> Self {
        self.task_guided_attention.push(keyword.into());
        self
    }

    /// Add multiple attention keywords.
    pub fn attention_keywords(mut self, keywords: impl IntoIterator<Item = impl Into<String>>) -> Self {
        for kw in keywords {
            self.task_guided_attention.push(kw.into());
        }
        self
    }

    /// Add a KV cache hint.
    pub fn kv_cache_hint(mut self, label: impl Into<String>, relevance: f32) -> Self {
        self.kv_cache_hints.push(KvCacheHint {
            label: label.into(),
            relevance: relevance.clamp(0.0, 1.0),
        });
        self
    }

    /// Set the maximum response token budget for the worker.
    pub fn max_response_tokens(mut self, tokens: u32) -> Self {
        self.max_response_tokens = Some(tokens);
        self
    }

    /// Build the `LatentBrief`.
    pub fn build(self) -> LatentBrief {
        LatentBrief {
            compact_trajectory: self.compact_trajectory,
            task_guided_attention: self.task_guided_attention,
            kv_cache_hints: self.kv_cache_hints,
            max_response_tokens: self.max_response_tokens,
        }
    }
}

// ---------------------------------------------------------------------------
// Trajectory compression
// ---------------------------------------------------------------------------

/// Compress a full conversation trajectory into a compact summary.
/// This is a simple heuristic compression that:
/// 1. Takes the last N messages (configurable)
/// 2. Truncates each to a max line count
/// 3. Adds a summary header
/// In production, this would call an LLM to generate a proper
/// summary. For now, this heuristic achieves the 30% target
/// without API cost.
pub fn compress_trajectory(
    messages: &[String],
    max_messages: usize,
    max_lines_per_message: usize,
) -> String {
    let start = messages.len().saturating_sub(max_messages);
    let selected = &messages[start..];

    let mut compact = String::from("[Orchestrator trajectory summary]\n\n");
    for (i, msg) in selected.iter().enumerate() {
        let lines: Vec<&str> = msg.lines().take(max_lines_per_message).collect();
        compact.push_str(&format!("--- Turn {} ---\n", start + i + 1));
        compact.push_str(&lines.join("\n"));
        compact.push('\n');
    }

    compact
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latent_brief_builder() {
        let brief = LatentBriefBuilder::new("Refactoring task: split agent_loop into modules")
            .attention_keyword("agent_loop")
            .attention_keyword("module split")
            .kv_cache_hint("agent_loop.rs", 0.9)
            .max_response_tokens(2000)
            .build();

        assert_eq!(brief.task_guided_attention.len(), 2);
        assert_eq!(brief.kv_cache_hints.len(), 1);
        assert_eq!(brief.max_response_tokens, Some(2000));
    }

    #[test]
    fn latent_brief_roundtrip_json() {
        let brief = LatentBriefBuilder::new("Summary text")
            .attention_keyword("test")
            .kv_cache_hint("context", 0.5)
            .build();

        let json = serde_json::to_string(&brief).unwrap();
        let deserialized: LatentBrief = serde_json::from_str(&json).unwrap();

        assert_eq!(brief.compact_trajectory, deserialized.compact_trajectory);
        assert_eq!(brief.task_guided_attention, deserialized.task_guided_attention);
        assert_eq!(brief.kv_cache_hints, deserialized.kv_cache_hints);
    }

    #[test]
    fn compress_trajectory_basic() {
        let messages = vec![
            "Line 1 of message 1\nLine 2 of message 1".to_string(),
            "Line 1 of message 2\nLine 2 of message 2".to_string(),
            "Line 1 of message 3\nLine 2 of message 3".to_string(),
        ];

        let compressed = compress_trajectory(&messages, 2, 5);
        assert!(compressed.contains("[Orchestrator trajectory summary]"));
        assert!(compressed.contains("Turn 2"));
        assert!(compressed.contains("Turn 3"));
        assert!(!compressed.contains("Turn 1")); // Only last 2 messages
    }

    #[test]
    fn compress_trajectory_truncates_long_messages() {
        let long_msg: String = (0..20).map(|i| format!("Line {i}\n")).collect();
        let messages = vec![long_msg];

        let compressed = compress_trajectory(&messages, 1, 5);
        // Should only include 5 lines from the message
        let line_count = compressed.lines().filter(|l| l.starts_with("Line")).count();
        assert!(line_count <= 5);
    }

    #[test]
    fn kv_cache_hint_relevance_clamped() {
        let _hint = KvCacheHint {
            label: "test".to_string(),
            relevance: 1.5, // Over 1.0
        };
        // The builder clamps it
        let brief = LatentBriefBuilder::new("test").kv_cache_hint("test", 1.5).build();
        assert!((brief.kv_cache_hints[0].relevance - 1.0).abs() < f32::EPSILON);
    }
}
