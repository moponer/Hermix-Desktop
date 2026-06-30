//! ## Context Gatherer — BaiLongma-inspired sufficiency checker
//!
//! Before the main LLM call, runs a lightweight check: "Does the current
//! context contain enough information to answer the user's request?"
//! If not, it proactively gathers needed context (reading files, searching
//! memory) and injects it before the main turn loop starts.
//!
//! This replaces the "load everything → compress later" pattern with a
//! "load only what's needed" pattern, saving significant context window
//! for large-file or multi-step tasks.
//!
//! Reference: `gatherer.js` in the BaiLongma project.

use std::sync::Arc;

use hermix_core::Message;
use hermix_focus::MemoryRouter;
use hermix_tools::ToolRegistry;

/// Maximum number of gather-check rounds before giving up.
const MAX_GATHER_ROUNDS: usize = 2;
/// Maximum characters to keep from a gathered file preview.
const GATHER_FILE_PREVIEW_CHARS: usize = 2000;

/// A single "need" identified by the checker LLM.
#[derive(Debug, Clone, PartialEq)]
enum GatherNeed {
    ReadFile { path: String },
}

/// Result of gathering context: collected text blocks.
#[derive(Debug, Default)]
pub struct GatheredContext {
    pub blocks: Vec<String>,
}

/// Check whether a user message likely needs file/memory context gathering.
/// Lightweight heuristic: check for file paths and question patterns.
fn should_gather(user_message: &str) -> bool {
    let msg = user_message.trim();
    if msg.is_empty() || msg.len() < 10 {
        return false;
    }
    // File path patterns: contains drive letter, path separators, or common extensions
    let has_path = msg.contains('\\')
        || msg.contains('/')
        || msg.contains(":\\")
        || msg.chars().filter(|&c| c == '.').count() >= 2;
    // Question patterns that suggest need for context
    let has_complex_pattern = msg.contains("帮我")
        || msg.contains("分析")
        || msg.contains("修改")
        || msg.contains("总结")
        || msg.contains("解释")
        || msg.contains("对比")
        || msg.contains("review")
        || msg.contains("analyze")
        || msg.contains("explain");
    has_path || has_complex_pattern
}

/// Run the gather loop: check sufficiency → resolve needs → repeat.
/// Returns gathered context blocks to inject.
pub async fn gather_context(
    user_message: &str,
    _memory_router: Option<&Arc<MemoryRouter>>,
    tool_registry: Option<&ToolRegistry>,
) -> GatheredContext {
    if !should_gather(user_message) {
        return GatheredContext::default();
    }

    let mut gathered = GatheredContext::default();
    let mut round = 0;

    while round < MAX_GATHER_ROUNDS {
        round += 1;
        // Check: for now we use a simple heuristic path scan instead of
        // an LLM call (which would add latency). Scan for explicit file
        // references and read them.
        let needs = scan_for_file_refs(user_message, &gathered);
        if needs.is_empty() {
            break;
        }
        let mut resolved = false;
        for need in &needs {
            if let Some(block) = resolve_need(need, tool_registry).await {
                gathered.blocks.push(block);
                resolved = true;
            }
        }
        if !resolved {
            break;
        }
    }
    gathered
}

/// Scan user message for explicit file references.
/// Returns up to 3 file reads that haven't been gathered yet.
fn scan_for_file_refs(msg: &str, gathered: &GatheredContext) -> Vec<GatherNeed> {
    let mut needs = Vec::new();
    // Simple heuristic: look for lines containing file-like patterns
    for line in msg.lines() {
        let trimmed = line.trim();
        // Windows path: D:\... or C:\...
        if trimmed.len() > 5 && (trimmed.contains(":\\") || trimmed.contains("\\\\")) {
            let path = trimmed.trim_end_matches(|c: char| c.is_ascii_punctuation() && c != ':' && c != '\\');
            if !path.is_empty() && !gathered.blocks.iter().any(|b| b.contains(path)) {
                needs.push(GatherNeed::ReadFile { path: path.to_string() });
                if needs.len() >= 3 {
                    break;
                }
            }
        }
    }
    needs
}

/// Resolve a single gather need into a text block.
/// Extracts only the parts relevant to the user's query (keyword-based
/// section extraction) and removes content already present in context.
async fn resolve_need(
    need: &GatherNeed,
    _tool_registry: Option<&ToolRegistry>,
) -> Option<String> {
    match need {
        GatherNeed::ReadFile { path } => {
            match tokio::fs::read_to_string(path).await {
                Ok(content) => {
                    let preview = extract_relevant_sections(&content, path, GATHER_FILE_PREVIEW_CHARS);
                    Some(preview)
                }
                Err(_) => None,
            }
        }
    }
}

/// Extract content from a file that is likely relevant to the user's request.
/// Strategy: keep the file header/imports + structure, then extract only
/// the sections that match keywords from the user query. Falls back to
/// simple truncation if no keywords are available.
fn extract_relevant_sections(content: &str, path: &str, max_chars: usize) -> String {
    let path_lower = path.to_lowercase();
    // Keywords derived from the file path itself: the filename (without extension)
    // and each path component are good signals for what the user is asking about.
    let keywords: Vec<&str> = path_lower
        .split(&['\\', '/', '.', '-', '_'][..])
        .filter(|s| s.len() >= 3)
        .collect();

    // Heuristic extraction: keep header area (first ~10 lines for imports/headers)
    // + any lines matching keywords signals.
    let lines: Vec<&str> = content.lines().collect();
    let header_end = lines.len().min(10);
    let header: Vec<&str> = lines[..header_end].to_vec();
    let body: Vec<&str> = lines[header_end..].to_vec();

    // Score each body line by how many keywords it matches
    let scored_lines: Vec<(usize, &str)> = body
        .iter()
        .enumerate()
        .filter(|(_, line)| {
            let line_lower = line.to_lowercase();
            keywords.iter().any(|kw| line_lower.contains(kw))
        })
        .map(|(i, line)| (i, *line))
        .collect();

    // If no lines matched keywords, fall back to simple truncation
    if scored_lines.is_empty() {
        let truncated = if content.len() > max_chars {
            format!("{}…\n(预览截断，共 {} 字符，可用 read_file 读取完整内容)",
                &content[..max_chars], content.len())
        } else {
            content.to_string()
        };
        return format!("## {path}\n{truncated}");
    }

    // Keep header + matched lines, in original order
    let mut result = String::new();
    result.push_str(&format!("## {path}\n"));
    // Include header
    for line in &header {
        if line.len() > 200 {
            result.push_str(&line[..200]);
            result.push_str("...\n");
        } else {
            result.push_str(line);
            result.push('\n');
        }
    }
    if !header.is_empty() {
        result.push_str("... (关键部分)...\n");
    }
    // Include only keyword-matched lines with context lines before/after
    let matched_indices: std::collections::HashSet<usize> = scored_lines.iter().map(|(i, _)| *i).collect();
    for (i, line) in body.iter().enumerate() {
        if matched_indices.contains(&i)
            || (i > 0 && matched_indices.contains(&(i - 1)))
            || (i + 1 < body.len() && matched_indices.contains(&(i + 1)))
        {
            let display = if line.len() > 200 { &line[..200] } else { line };
            result.push_str(display);
            result.push('\n');
        }
    }

    // Truncate result to max_chars
    if result.len() > max_chars {
        result.truncate(max_chars);
        result.push_str("\n...(相关片段提取完)");
    }
    result
}

/// Format gathered context blocks into a system message for injection.
pub fn format_gathered_context(ctx: &GatheredContext) -> Option<Message> {
    if ctx.blocks.is_empty() {
        return None;
    }
    let text = ctx.blocks.join("\n\n");
    Some(Message::system(format!(
        "[自动采集] 以下内容是系统根据你的请求预读取的上下文：\n\n{}",
        text
    )))
}
