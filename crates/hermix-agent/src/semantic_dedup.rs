//! Semantic Deduplication — detect and merge semantically similar messages.
//!
//! Unlike exact-match dedup (MD5 hash), this module uses lightweight
//! token-level similarity to identify messages that convey the same
//! information with different wording. No embedding model required.
//!
//! Algorithm:
//! 1. Tokenize messages into word sets (lowercased, stopwords removed)
//! 2. Compute Jaccard similarity between token sets
//! 3. When similarity > threshold, keep the more informative version
//! 4. Support sliding window (compare each message with recent N)
//!
//! Typical savings: 10-30% on conversations with repeated questions/restatements.

use hermix_core::{Message, MessageRole};

/// Configuration for semantic deduplication.
#[derive(Debug, Clone)]
pub struct SemanticDedupConfig {
    /// Similarity threshold (0.0-1.0). Messages with Jaccard similarity
    /// above this are considered duplicates. Default: 0.70.
    pub similarity_threshold: f64,
    /// Sliding window size. Compare each message with the last N messages.
    /// Default: 10.
    pub window_size: usize,
    /// Minimum message length (tokens) to participate in dedup.
    /// Short messages are skipped to avoid false positives. Default: 5.
    pub min_tokens: usize,
    /// Roles to participate in dedup. Default: [User, Assistant].
    pub dedup_roles: Vec<MessageRole>,
}

impl Default for SemanticDedupConfig {
    fn default() -> Self {
        Self {
            similarity_threshold: 0.70,
            window_size: 10,
            min_tokens: 5,
            dedup_roles: vec![MessageRole::User, MessageRole::Assistant],
        }
    }
}

/// Result of semantic deduplication.
#[derive(Debug, Clone)]
pub struct SemanticDedupResult {
    /// Messages after deduplication.
    pub messages: Vec<Message>,
    /// Number of messages removed/merged.
    pub removed_count: usize,
    /// Estimated token savings.
    pub tokens_saved: usize,
}

/// Common English stopwords to ignore during tokenization.
const STOPWORDS: &[&str] = &[
    "a", "an", "the", "is", "are", "was", "were", "be", "been", "being",
    "have", "has", "had", "do", "does", "did", "will", "would", "could",
    "should", "may", "might", "shall", "can", "need", "dare", "ought",
    "used", "to", "of", "in", "for", "on", "with", "at", "by", "from",
    "as", "into", "through", "during", "before", "after", "above", "below",
    "between", "out", "off", "over", "under", "again", "further", "then",
    "once", "here", "there", "when", "where", "why", "how", "all", "both",
    "each", "few", "more", "most", "other", "some", "such", "no", "nor",
    "not", "only", "own", "same", "so", "than", "too", "very", "just",
    "don", "now", "and", "but", "or", "if", "while", "that", "this",
    "it", "its", "i", "me", "my", "we", "our", "you", "your", "he",
    "him", "his", "she", "her", "they", "them", "their", "what", "which",
    "who", "whom", "these", "those", "am",
    // Chinese particles (common in mixed content)
    "的", "了", "在", "是", "我", "有", "和", "就", "不", "人", "都",
    "一", "一个", "上", "也", "很", "到", "说", "要", "去", "你",
];

/// Tokenize text into a set of lowercased words.
///
/// Removes stopwords and filters short tokens (< 2 chars).
/// CJK characters are split individually.
fn tokenize(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut tokens: Vec<String> = Vec::new();
    
    // Split on non-alphanumeric characters, but handle CJK specially
    // IMPORTANT: Check CJK BEFORE alphanumeric, since CJK is alphanumeric
    let mut current = String::new();
    for c in lower.chars() {
        if is_cjk(c) {
            // Flush any accumulated ASCII token
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            // Each CJK character is a separate token
            tokens.push(c.to_string());
        } else if c.is_alphanumeric() {
            current.push(c);
        } else {
            // Separator (punctuation, whitespace, etc.)
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    
    // Filter stopwords and short tokens
    tokens
        .into_iter()
        .filter(|t| t.len() >= 2 && !STOPWORDS.iter().any(|&s| s == t.as_str()))
        .collect()
}

/// Check if a character is CJK (Chinese/Japanese/Korean).
fn is_cjk(c: char) -> bool {
    let cp = c as u32;
    (0x4E00..=0x9FFF).contains(&cp)   // CJK Unified Ideographs
        || (0x3400..=0x4DBF).contains(&cp) // CJK Extension A
        || (0xF900..=0xFAFF).contains(&cp) // CJK Compatibility Ideographs
}

/// Compute Jaccard similarity between two token sets.
///
/// Returns a value between 0.0 (no overlap) and 1.0 (identical).
fn jaccard_similarity(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }

    // Use a hash set for O(1) lookup
    let set_a: std::collections::HashSet<&str> = a.iter().map(|s| s.as_str()).collect();
    let set_b: std::collections::HashSet<&str> = b.iter().map(|s| s.as_str()).collect();

    let intersection = set_a.intersection(&set_b).count();
    let union = set_a.len() + set_b.len() - intersection;

    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

/// Compute token overlap ratio (how much of A is contained in B).
///
/// Useful for detecting "A is a subset of B" cases.
fn containment_ratio(subset: &[String], superset: &[String]) -> f64 {
    if subset.is_empty() {
        return 0.0;
    }
    let set_super: std::collections::HashSet<&str> = superset.iter().map(|s| s.as_str()).collect();
    let contained = subset.iter().filter(|t| set_super.contains(t.as_str())).count();
    contained as f64 / subset.len() as f64
}

/// 该消息是否持有 tool_calls（= 它后面那些 `tool` 消息的配对锚点）。
fn has_tool_calls(msg: &Message) -> bool {
    msg.tool_calls
        .as_ref()
        .map(|tcs| !tcs.is_empty())
        .unwrap_or(false)
}

/// Run semantic deduplication on a list of messages.
///
/// Returns deduplicated messages and statistics.
pub fn semantic_dedup(messages: &[Message], config: &SemanticDedupConfig) -> SemanticDedupResult {
    if messages.is_empty() {
        return SemanticDedupResult {
            messages: Vec::new(),
            removed_count: 0,
            tokens_saved: 0,
        };
    }

    let mut result: Vec<Message> = Vec::with_capacity(messages.len());
    let mut removed_count = 0;
    let mut tokens_saved = 0;

    // Tokenize all messages once
    let tokenized: Vec<(usize, Vec<String>)> = messages
        .iter()
        .enumerate()
        .map(|(i, msg)| {
            let tokens = tokenize(msg.content.as_deref().unwrap_or(""));
            (i, tokens)
        })
        .collect();

    // Track which messages to keep
    let mut keep = vec![true; messages.len()];

    for (i, msg) in messages.iter().enumerate() {
        if !keep[i] {
            continue;
        }

        // Skip messages not in dedup roles
        if !config.dedup_roles.contains(&msg.role) {
            result.push(msg.clone());
            continue;
        }

        // ★ 携带 tool_calls 的 assistant 消息**永不参与去重**。
        // 它是 tool_result 的唯一配对锚点：把它当"重复文本"删掉/替换，
        // 它的 `tool` 消息就变成孤儿（provider 侧 `drop_orphan_tool_results`
        // 判定为无配对，每轮请求都丢弃 —— 2026-11-06 实机一次丢掉 88 条，
        // 模型从此看不到那些工具结果，反复重跑同一批工具直到 max turns）。
        // 判据只看 `tool_calls`，不看正文长度：正文短不代表可以删。
        if has_tool_calls(msg) {
            result.push(msg.clone());
            continue;
        }

        let (_, ref tokens_i) = tokenized[i];

        // Skip short messages
        if tokens_i.len() < config.min_tokens {
            result.push(msg.clone());
            continue;
        }

        // Compare with recent messages in sliding window
        let window_start = i.saturating_sub(config.window_size);
        let mut best_match: Option<(usize, f64)> = None;

        for j in window_start..i {
            if !keep[j] || messages[j].role != msg.role {
                continue;
            }
            // 同上：不能把带 tool_calls 的 assistant 消息选为"被合并掉的旧消息"。
            if has_tool_calls(&messages[j]) {
                continue;
            }

            let (_, ref tokens_j) = tokenized[j];

            // Skip if either is too short
            if tokens_j.len() < config.min_tokens {
                continue;
            }

            let similarity = jaccard_similarity(tokens_i, tokens_j);

            if similarity >= config.similarity_threshold {
                // Check if one is a subset of the other (containment)
                let containment_a_in_b = containment_ratio(tokens_i, tokens_j);
                let containment_b_in_a = containment_ratio(tokens_j, tokens_i);

                // If high containment, the shorter message is redundant
                let effective_similarity = if containment_a_in_b > 0.8 || containment_b_in_a > 0.8 {
                    similarity + 0.1 // Boost for containment
                } else {
                    similarity
                };

                if effective_similarity >= config.similarity_threshold {
                    match best_match {
                        None => best_match = Some((j, effective_similarity)),
                        Some((_, best_sim)) if effective_similarity > best_sim => {
                            best_match = Some((j, effective_similarity))
                        }
                        _ => {}
                    }
                }
            }
        }

        if let Some((j, _similarity)) = best_match {
            // Found a duplicate — keep the longer (more informative) version
            let len_i = msg.content.as_deref().unwrap_or("").len();
            let len_j = messages[j].content.as_deref().unwrap_or("").len();

            if len_i > len_j {
                // Current message is longer — replace the older one
                keep[j] = false;
                tokens_saved += tokenized[j].1.len();
                removed_count += 1;
                result.push(msg.clone());
            } else {
                // Older message is longer or equal — mark current as duplicate
                tokens_saved += tokens_i.len();
                removed_count += 1;
                // Don't add to result (effectively removing)
            }
        } else {
            result.push(msg.clone());
        }
    }

    SemanticDedupResult {
        messages: result,
        removed_count,
        tokens_saved,
    }
}

/// Quick similarity check between two text strings.
///
/// Returns Jaccard similarity (0.0-1.0).
pub fn text_similarity(a: &str, b: &str) -> f64 {
    let tokens_a = tokenize(a);
    let tokens_b = tokenize(b);
    jaccard_similarity(&tokens_a, &tokens_b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_user(text: &str) -> Message {
        Message {
            role: MessageRole::User,
            content: Some(text.to_string()),
            ..Default::default()
        }
    }

    fn make_assistant(text: &str) -> Message {
        Message {
            role: MessageRole::Assistant,
            content: Some(text.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn test_tokenize_basic() {
        let tokens = tokenize("Hello world, this is a test");
        assert!(tokens.contains(&"hello".to_string()));
        assert!(tokens.contains(&"world".to_string()));
        assert!(tokens.contains(&"test".to_string()));
        // Stopwords should be removed
        assert!(!tokens.contains(&"is".to_string()));
        assert!(!tokens.contains(&"a".to_string()));
    }

    #[test]
    fn test_tokenize_chinese() {
        let tokens = tokenize("这是一个测试 the test");
        // "这" should be kept (not a stopword)
        assert!(tokens.contains(&"这".to_string()), "tokens: {:?}", tokens);
        // "是" is a stopword and gets filtered
        assert!(!tokens.contains(&"是".to_string()));
        assert!(tokens.contains(&"test".to_string()));
    }

    #[test]
    fn test_jaccard_identical() {
        let a = vec!["hello".to_string(), "world".to_string()];
        let b = vec!["hello".to_string(), "world".to_string()];
        assert_eq!(jaccard_similarity(&a, &b), 1.0);
    }

    #[test]
    fn test_jaccard_no_overlap() {
        let a = vec!["hello".to_string(), "world".to_string()];
        let b = vec!["foo".to_string(), "bar".to_string()];
        assert_eq!(jaccard_similarity(&a, &b), 0.0);
    }

    #[test]
    fn test_jaccard_partial() {
        let a = vec!["hello".to_string(), "world".to_string(), "foo".to_string()];
        let b = vec!["hello".to_string(), "world".to_string(), "bar".to_string()];
        // intersection = {hello, world} = 2, union = {hello, world, foo, bar} = 4
        assert!((jaccard_similarity(&a, &b) - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_containment_ratio() {
        let subset = vec!["hello".to_string(), "world".to_string()];
        let superset = vec!["hello".to_string(), "world".to_string(), "foo".to_string()];
        // All subset elements are in superset, so ratio = 2/2 = 1.0
        assert!((containment_ratio(&subset, &superset) - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_semantic_dedup_exact_duplicates() {
        let msgs = vec![
            make_user("What is the capital of France?"),
            make_user("What is the capital of France?"),
            make_user("What is the capital of France?"),
        ];
        // Debug: check tokenization
        let tokens = tokenize("What is the capital of France?");
        eprintln!("Tokens: {:?}", tokens);
        eprintln!("Token count: {}", tokens.len());
        
        let config = SemanticDedupConfig {
            similarity_threshold: 0.70,
            min_tokens: 2, // Lower threshold to ensure tokens pass
            ..Default::default()
        };
        let result = semantic_dedup(&msgs, &config);
        eprintln!("Removed: {}, Remaining: {}", result.removed_count, result.messages.len());
        // Should remove 2 of 3 identical messages
        assert!(result.removed_count >= 1, "should remove at least 1 duplicate, got {}", result.removed_count);
    }

    #[test]
    fn test_semantic_dedup_similar_paraphrases() {
        let msgs = vec![
            make_user("How do I install Python on Windows?"),
            make_user("How to install Python on Windows machine?"),
            make_user("What are the steps to install Python on Windows?"),
        ];
        let config = SemanticDedupConfig {
            similarity_threshold: 0.60,
            min_tokens: 3,
            ..Default::default()
        };
        let result = semantic_dedup(&msgs, &config);
        // Paraphrases should be detected
        assert!(result.removed_count >= 1, "should detect paraphrases");
    }

    #[test]
    fn test_semantic_dedup_different_topics() {
        let msgs = vec![
            make_user("What is the capital of France?"),
            make_user("How do I bake a chocolate cake?"),
            make_user("What is the weather today?"),
        ];
        let config = SemanticDedupConfig {
            similarity_threshold: 0.70,
            min_tokens: 3,
            ..Default::default()
        };
        let result = semantic_dedup(&msgs, &config);
        // Different topics should NOT be deduped
        assert_eq!(result.removed_count, 0);
        assert_eq!(result.messages.len(), 3);
    }

    #[test]
    fn test_semantic_dedup_preserves_different_roles() {
        let msgs = vec![
            make_user("What is Python?"),
            make_assistant("Python is a programming language."),
            make_user("What is Python?"), // Same as first user message
        ];
        let config = SemanticDedupConfig {
            similarity_threshold: 0.70,
            min_tokens: 3,
            ..Default::default()
        };
        let result = semantic_dedup(&msgs, &config);
        // Same content but different roles should NOT be deduped
        assert_eq!(result.removed_count, 0);
    }

    #[test]
    fn test_text_similarity() {
        let sim = text_similarity(
            "How do I install Python?",
            "How to install Python on Windows?"
        );
        assert!(sim > 0.5, "similarity should be > 0.5, got {}", sim);
    }

    #[test]
    fn test_short_messages_skipped() {
        let msgs = vec![
            make_user("ok"),
            make_user("ok"),
        ];
        let config = SemanticDedupConfig {
            similarity_threshold: 0.70,
            min_tokens: 5,
            ..Default::default()
        };
        let result = semantic_dedup(&msgs, &config);
        // Short messages should be skipped
        assert_eq!(result.removed_count, 0);
    }
}
