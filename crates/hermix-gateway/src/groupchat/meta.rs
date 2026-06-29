//! META parser (S1).
//!
//! Agents embed a self-summary block at the end of their output:
//!
//! ```text
//! ...actual content...
//! ---META---
//! summary: A one-line summary of what I just produced.
//! key_points: point A | point B | point C
//! progress: 40%
//! confidence: high
//! ```
//!
//! We parse this out so the summary can be archived into the task
//! node's `summary_a` (Layer A), the key points become `summary_b`
//! (Layer B), and the progress percentage drives task state.
//!
//! Why this matters: the next time the agent runs, we inject the
//! META summary instead of the full output.  That cuts context by
//! ~75% on average, which is the difference between "ran out of
//! budget after 3 chapters" and "wrote 10 chapters no problem".

use serde::{Deserialize, Serialize};

/// Parsed META block from an agent's output.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetaBlock {
    /// One-line summary (~50 chars) — Layer A.
    pub summary: String,
    /// Bullet points (~3-7 items) — Layer B.
    pub key_points: Vec<String>,
    /// Progress 0..=100 (None if not stated).
    pub progress: Option<u32>,
    /// Self-assessed confidence: "low" | "medium" | "high" | None.
    pub confidence: Option<String>,
    /// Optional list of references the agent wants loaded next time.
    /// Format: `task:task_xxx` or `chapter:5` etc.
    pub refs: Vec<String>,
    /// Raw remainder text with META stripped.
    pub content: String,
}

/// Sentinel markers (lowercase for case-insensitive matching).
const META_START: &str = "---meta---";
const META_END: &str = "---end---";

/// Parse an agent's raw output into (content, meta).
///
/// We look for the LAST occurrence of `---META---` followed by
/// `---END---` (or end of input).  Anything before is content,
/// anything between is parsed as `key: value` lines.
///
/// We are intentionally tolerant: a missing META block returns
/// `(raw, MetaBlock::default())` so the caller can fall back to
/// "use raw as content, no summary available".
pub fn parse_meta(raw: &str) -> (String, MetaBlock) {
    // Find the META block — try `---META---...---END---` first,
    // then fall back to `---META---...EOF`.
    let lower = raw.to_lowercase();
    let start = lower.rfind(META_START);
    let Some(start_idx) = start else {
        return (raw.to_string(), MetaBlock::default());
    };
    // Skip past the marker
    let after_marker = start_idx + META_START.len();
    let after_marker = skip_newline(raw, after_marker);

    // Look for END marker
    let end_rel = lower[after_marker..].rfind(META_END);
    let meta_text = match end_rel {
        Some(rel) => {
            let abs_end = after_marker + rel;
            &raw[after_marker..abs_end]
        }
        None => &raw[after_marker..],
    };

    let content = raw[..start_idx].trim_end().to_string();
    let meta = parse_meta_lines(meta_text);

    (content, meta)
}

fn skip_newline(s: &str, mut idx: usize) -> usize {
    while idx < s.len() && (s.as_bytes()[idx] == b'\n' || s.as_bytes()[idx] == b'\r') {
        idx += 1;
    }
    idx
}

fn parse_meta_lines(text: &str) -> MetaBlock {
    let mut block = MetaBlock::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Split on first ':'
        let Some(colon) = line.find(':') else { continue; };
        let key = line[..colon].trim().to_lowercase();
        let val = line[colon + 1..].trim();
        match key.as_str() {
            "summary" => block.summary = val.to_string(),
            "key_points" | "points" => {
                block.key_points = val.split('|')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            "progress" => {
                let pct = val.trim_end_matches('%').parse::<u32>().ok();
                block.progress = pct.map(|p| p.min(100));
            }
            "confidence" => {
                let c = val.to_lowercase();
                if matches!(c.as_str(), "low" | "medium" | "high") {
                    block.confidence = Some(c);
                }
            }
            "ref" | "refs" | "references" => {
                block.refs = val.split(|c: char| c == ',' || c == '|' || c.is_whitespace())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            _ => {} // ignore unknown keys
        }
    }
    block
}

/// Strip just the META block (return only the content portion).
/// Useful when you've already extracted the meta elsewhere.
pub fn strip_meta_block(raw: &str) -> String {
    parse_meta(raw).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_meta_block() {
        let raw = "I wrote chapter 5 today.\n\n---META---\n\
                   summary: 主角进入新副本,遭遇第一波敌人.\n\
                   key_points: 副本开启 | 三人组队 | 遇到强敌\n\
                   progress: 40%\n\
                   confidence: high\n\
                   refs: chapter:3\n\
                   ---END---";
        let (content, meta) = parse_meta(raw);
        assert!(content.starts_with("I wrote chapter 5"));
        assert!(!content.contains("---META---"));
        assert_eq!(meta.summary, "主角进入新副本,遭遇第一波敌人.");
        assert_eq!(meta.key_points.len(), 3);
        assert_eq!(meta.progress, Some(40));
        assert_eq!(meta.confidence.as_deref(), Some("high"));
        assert_eq!(meta.refs, vec!["chapter:3"]);
    }

    #[test]
    fn no_meta_block_returns_raw() {
        let raw = "just some content, no meta here";
        let (content, meta) = parse_meta(raw);
        assert_eq!(content, raw);
        assert!(meta.summary.is_empty());
    }

    #[test]
    fn meta_without_end_marker_works() {
        let raw = "hello world\n---META---\nsummary: short version\n";
        let (content, meta) = parse_meta(raw);
        assert_eq!(content, "hello world");
        assert_eq!(meta.summary, "short version");
    }

    #[test]
    fn last_meta_block_wins() {
        // Multiple META blocks — we want the last one
        let raw = "first content\n---META---\nsummary: A\n---END---\n\
                   middle content\n\
                   ---META---\nsummary: B\n---END---";
        let (_, meta) = parse_meta(raw);
        assert_eq!(meta.summary, "B");
    }

    #[test]
    fn case_insensitive_keys() {
        let raw = "x\n---META---\nSummary: hi\nPROGRESS: 50%\n";
        let (_, meta) = parse_meta(raw);
        assert_eq!(meta.summary, "hi");
        assert_eq!(meta.progress, Some(50));
    }

    #[test]
    fn meta_block_capped_summary_for_layer_a() {
        let raw = "c\n---META---\nsummary: 写了第3章,主角遇到神秘老者,获得第一件法宝,伏笔埋下.\n---END---";
        let (_, meta) = parse_meta(raw);
        // Layer A wants ~50 chars; we don't truncate here, but the
        // caller is expected to.  We just verify it's plumbed.
        assert!(!meta.summary.is_empty());
    }
}
