//! Real file tool backends: patch (fuzzy match) and search (regex/glob).

use async_trait::async_trait;
use regex::Regex;
use serde_json::{json, Value};

use crate::tools::file::{PatchBackend, SearchBackend};
use hermix_core::ToolError;

// ---------------------------------------------------------------------------
// LocalPatchBackend
// ---------------------------------------------------------------------------

/// Real file patch backend using fuzzy string matching.
/// Implements an 8-strategy matching chain (ported from Python `fuzzy_match.py`):
/// 1. Exact match
/// 2. Line-trimmed (strip leading/trailing whitespace per line)
/// 3. Whitespace normalized (collapse multiple spaces/tabs)
/// 4. Indentation flexible (ignore indentation differences)
/// 5. Escape normalized (convert \\n literals to newlines)
/// 6. Trimmed boundary (trim first/last line only)
/// 7. Block anchor (match first+last lines, similarity for middle)
/// 8. Context-aware (50% line similarity threshold)
pub struct LocalPatchBackend;

impl LocalPatchBackend {
    pub fn new() -> Self {
        Self
    }

    /// Find the best fuzzy match for `needle` in `haystack`.
    /// Returns (start_index, end_index) of the best match, or None.
    fn fuzzy_find(haystack: &str, needle: &str) -> Option<(usize, usize)> {
        // Strategy 1: Exact match
        if let Some(pos) = haystack.find(needle) {
            return Some((pos, pos + needle.len()));
        }

        // Strategy 2: Line-trimmed match
        if let Some(result) = Self::strategy_line_trimmed(haystack, needle) {
            return Some(result);
        }

        // Strategy 3: Whitespace normalized
        if let Some(result) = Self::strategy_whitespace_normalized(haystack, needle) {
            return Some(result);
        }

        // Strategy 4: Indentation flexible
        if let Some(result) = Self::strategy_indentation_flexible(haystack, needle) {
            return Some(result);
        }

        // Strategy 5: Escape normalized
        if let Some(result) = Self::strategy_escape_normalized(haystack, needle) {
            return Some(result);
        }

        // Strategy 6: Trimmed boundary
        if let Some(result) = Self::strategy_trimmed_boundary(haystack, needle) {
            return Some(result);
        }

        // Strategy 7: Block anchor
        if let Some(result) = Self::strategy_block_anchor(haystack, needle) {
            return Some(result);
        }

        // Strategy 8: Context-aware
        Self::strategy_context_aware(haystack, needle)
    }

    /// Strategy 2: Match with line-by-line whitespace trimming.
    fn strategy_line_trimmed(haystack: &str, needle: &str) -> Option<(usize, usize)> {
        let needle_lines: Vec<&str> = needle.lines().map(|l| l.trim()).collect();
        let haystack_lines: Vec<&str> = haystack.lines().collect();

        if needle_lines.is_empty() {
            return None;
        }

        for i in 0..haystack_lines.len().saturating_sub(needle_lines.len() - 1) {
            let mut matches = true;
            for (j, nl) in needle_lines.iter().enumerate() {
                if i + j >= haystack_lines.len() || haystack_lines[i + j].trim() != *nl {
                    matches = false;
                    break;
                }
            }
            if matches {
                return Some(Self::line_positions(
                    haystack,
                    &haystack_lines,
                    i,
                    i + needle_lines.len(),
                ));
            }
        }
        None
    }

    /// Strategy 3: Collapse multiple whitespace to single space.
    fn strategy_whitespace_normalized(haystack: &str, needle: &str) -> Option<(usize, usize)> {
        let norm_needle = Self::normalize_whitespace(needle);
        let norm_haystack = Self::normalize_whitespace(haystack);

        if let Some(pos) = norm_haystack.find(&norm_needle) {
            let start = Self::map_normalized_pos(haystack, &norm_haystack, pos);
            let end = Self::map_normalized_pos(haystack, &norm_haystack, pos + norm_needle.len());
            return Some((start, end));
        }
        None
    }

    /// Strategy 4: Ignore indentation differences entirely.
    fn strategy_indentation_flexible(haystack: &str, needle: &str) -> Option<(usize, usize)> {
        let needle_lines: Vec<&str> = needle.lines().map(|l| l.trim_start()).collect();
        let haystack_lines: Vec<&str> = haystack.lines().collect();

        if needle_lines.is_empty() {
            return None;
        }

        for i in 0..haystack_lines.len().saturating_sub(needle_lines.len() - 1) {
            let mut matches = true;
            for (j, nl) in needle_lines.iter().enumerate() {
                if i + j >= haystack_lines.len() || haystack_lines[i + j].trim_start() != *nl {
                    matches = false;
                    break;
                }
            }
            if matches {
                return Some(Self::line_positions(
                    haystack,
                    &haystack_lines,
                    i,
                    i + needle_lines.len(),
                ));
            }
        }
        None
    }

    /// Strategy 5: Convert escape sequences to actual characters.
    fn strategy_escape_normalized(haystack: &str, needle: &str) -> Option<(usize, usize)> {
        let unescaped = needle
            .replace("\\n", "\n")
            .replace("\\t", "\t")
            .replace("\\r", "\r");

        if unescaped == needle {
            return None; // No escapes to convert
        }

        haystack
            .find(&unescaped)
            .map(|pos| (pos, pos + unescaped.len()))
    }

    /// Strategy 6: Trim whitespace from first and last lines only.
    fn strategy_trimmed_boundary(haystack: &str, needle: &str) -> Option<(usize, usize)> {
        let mut needle_lines: Vec<String> = needle.lines().map(|l| l.to_string()).collect();
        if needle_lines.is_empty() {
            return None;
        }

        needle_lines[0] = needle_lines[0].trim().to_string();
        if needle_lines.len() > 1 {
            let last = needle_lines.len() - 1;
            needle_lines[last] = needle_lines[last].trim().to_string();
        }

        let haystack_lines: Vec<&str> = haystack.lines().collect();
        let n = needle_lines.len();

        for i in 0..haystack_lines.len().saturating_sub(n - 1) {
            let mut check_lines: Vec<String> = haystack_lines[i..i + n]
                .iter()
                .map(|l| l.to_string())
                .collect();
            check_lines[0] = check_lines[0].trim().to_string();
            if check_lines.len() > 1 {
                let last = check_lines.len() - 1;
                check_lines[last] = check_lines[last].trim().to_string();
            }

            if check_lines == needle_lines {
                return Some(Self::line_positions(haystack, &haystack_lines, i, i + n));
            }
        }
        None
    }

    /// Strategy 7: Match by anchoring on first and last lines.
    fn strategy_block_anchor(haystack: &str, needle: &str) -> Option<(usize, usize)> {
        let needle_lines: Vec<&str> = needle.lines().collect();
        if needle_lines.len() < 2 {
            return None;
        }

        let first_line = needle_lines[0].trim();
        let last_line = needle_lines[needle_lines.len() - 1].trim();
        let haystack_lines: Vec<&str> = haystack.lines().collect();
        let n = needle_lines.len();

        let mut candidates = Vec::new();
        for i in 0..haystack_lines.len().saturating_sub(n - 1) {
            if haystack_lines[i].trim() == first_line
                && haystack_lines[i + n - 1].trim() == last_line
            {
                candidates.push(i);
            }
        }

        // Use lower threshold for unique matches
        let threshold = if candidates.len() == 1 { 0.10 } else { 0.30 };

        for i in candidates {
            let similarity = if n <= 2 {
                1.0
            } else {
                let content_middle: String = haystack_lines[i + 1..i + n - 1].join("\n");
                let pattern_middle: String = needle_lines[1..n - 1].join("\n");
                Self::sequence_similarity(&content_middle, &pattern_middle)
            };

            if similarity >= threshold {
                return Some(Self::line_positions(haystack, &haystack_lines, i, i + n));
            }
        }
        None
    }

    /// Strategy 8: Line-by-line similarity with 50% threshold.
    fn strategy_context_aware(haystack: &str, needle: &str) -> Option<(usize, usize)> {
        let needle_lines: Vec<&str> = needle.lines().collect();
        let haystack_lines: Vec<&str> = haystack.lines().collect();

        if needle_lines.is_empty() {
            return None;
        }

        let n = needle_lines.len();

        for i in 0..haystack_lines.len().saturating_sub(n - 1) {
            let block = &haystack_lines[i..i + n];
            let mut high_sim_count = 0;

            for (pl, cl) in needle_lines.iter().zip(block.iter()) {
                let sim = Self::sequence_similarity(pl.trim(), cl.trim());
                if sim >= 0.80 {
                    high_sim_count += 1;
                }
            }

            if high_sim_count >= (n as f64 * 0.5) as usize {
                return Some(Self::line_positions(haystack, &haystack_lines, i, i + n));
            }
        }
        None
    }

    fn normalize_whitespace(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn map_normalized_pos(original: &str, _normalized: &str, norm_pos: usize) -> usize {
        let mut orig_idx = 0;
        let mut norm_idx = 0;
        let orig_bytes = original.as_bytes();

        while norm_idx < norm_pos && orig_idx < orig_bytes.len() {
            if orig_bytes[orig_idx].is_ascii_whitespace() {
                orig_idx += 1;
                if norm_idx < norm_pos {
                    while orig_idx < orig_bytes.len() && orig_bytes[orig_idx].is_ascii_whitespace()
                    {
                        orig_idx += 1;
                    }
                    norm_idx += 1;
                }
            } else {
                orig_idx += 1;
                norm_idx += 1;
            }
        }

        orig_idx.min(original.len())
    }

    /// Calculate start and end byte positions from line indices.
    fn line_positions(
        content: &str,
        lines: &[&str],
        start_line: usize,
        end_line: usize,
    ) -> (usize, usize) {
        let start: usize = lines[..start_line].iter().map(|l| l.len() + 1).sum();
        let end: usize = lines[..end_line].iter().map(|l| l.len() + 1).sum();
        let end = (end.saturating_sub(1)).min(content.len());
        (start, end)
    }

    /// Simple sequence similarity ratio (0.0 to 1.0).
    fn sequence_similarity(a: &str, b: &str) -> f64 {
        if a.is_empty() && b.is_empty() {
            return 1.0;
        }
        if a.is_empty() || b.is_empty() {
            return 0.0;
        }

        // Use longest common subsequence ratio
        let a_chars: Vec<char> = a.chars().collect();
        let b_chars: Vec<char> = b.chars().collect();
        let lcs_len = Self::lcs_length(&a_chars, &b_chars);
        (2.0 * lcs_len as f64) / (a_chars.len() + b_chars.len()) as f64
    }

    /// Longest common subsequence length.
    fn lcs_length(a: &[char], b: &[char]) -> usize {
        let m = a.len();
        let n = b.len();
        // Use two rows to save memory
        let mut prev = vec![0usize; n + 1];
        let mut curr = vec![0usize; n + 1];

        for i in 1..=m {
            for j in 1..=n {
                if a[i - 1] == b[j - 1] {
                    curr[j] = prev[j - 1] + 1;
                } else {
                    curr[j] = prev[j].max(curr[j - 1]);
                }
            }
            std::mem::swap(&mut prev, &mut curr);
            curr.iter_mut().for_each(|x| *x = 0);
        }
        *prev.iter().max().unwrap_or(&0)
    }
}

impl Default for LocalPatchBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PatchBackend for LocalPatchBackend {
    async fn patch_file(
        &self,
        path: &str,
        old_string: &str,
        new_string: &str,
        replace_all: bool,
        allow_fuzzy: bool,
    ) -> Result<String, ToolError> {
        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read '{}': {}", path, e)))?;

        // 安全中心：覆盖前把旧版本存进备份目录（best-effort，失败不影响写）。
        crate::backup::try_backup(std::path::Path::new(path), content.as_bytes());

        if old_string.is_empty() {
            // Empty old_string means create new file or append
            let new_content = if content.is_empty() {
                new_string.to_string()
            } else {
                format!("{}\n{}", content, new_string)
            };
            tokio::fs::write(path, &new_content).await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to write '{}': {}", path, e))
            })?;
            return Ok(json!({"status": "ok", "message": "Content appended"}).to_string());
        }

        if replace_all {
            // Simple replace all occurrences
            if !content.contains(old_string) {
                return Err(ToolError::ExecutionFailed(format!(
                    "Could not find '{}' in '{}'",
                    &old_string[..old_string.len().min(100)],
                    path
                )));
            }
            let new_content = content.replace(old_string, new_string);
            let count = content.matches(old_string).count();
            tokio::fs::write(path, &new_content).await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to write '{}': {}", path, e))
            })?;
            return Ok(json!({"status": "ok", "replacements": count}).to_string());
        }

        // Single replacement.
        //
        // H2 correctness contract (mirrors dsh `edit`): a literal match
        // must be UNIQUE unless the caller opted into `replace_all` or
        // `allow_fuzzy`. The old code ran an 8-strategy fuzzy chain
        // whenever the literal missed, so a one-character difference in
        // whitespace/indentation could land the edit on a *different*
        // occurrence of the same shape — silently corrupting the file
        // with no error anywhere.
        //
        // Order matters: exact first, and if that is ambiguous we stop
        // rather than guessing. Fuzzy is only reachable when the caller
        // explicitly asked for it.
        let literal_hits = content.matches(old_string).count();
        if literal_hits > 1 {
            return Err(ToolError::ExecutionFailed(format!(
                "Found {literal_hits} occurrences of the given text in '{path}'. \
                 The edit is ambiguous — patch would be applied to an arbitrary one. \
                 Include more surrounding context to make it unique, or pass \
                 replace_all: true to change all of them."
            )));
        }

        if literal_hits == 1 {
            let pos = content.find(old_string).expect("literal_hits == 1");
            let new_content = format!(
                "{}{}{}",
                &content[..pos],
                new_string,
                &content[pos + old_string.len()..]
            );
            tokio::fs::write(path, &new_content).await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to write '{}': {}", path, e))
            })?;
            return Ok(json!({"status": "ok", "replacements": 1, "match": "literal"}).to_string());
        }

        // No literal hit. Fuzzy fallback only on explicit opt-in.
        if !allow_fuzzy {
            return Err(ToolError::ExecutionFailed(format!(
                "Could not find the given text verbatim in '{path}'. \
                 The file was NOT modified. [don't blindly retry] \
                 Re-read the file to get the exact current text (whitespace and \
                 indentation must match exactly), or pass allow_fuzzy: true to \
                 accept whitespace/indentation-tolerant matching."
            )));
        }

        match Self::fuzzy_find(&content, old_string) {
            Some((start, end)) => {
                let mut new_content = String::with_capacity(content.len());
                new_content.push_str(&content[..start]);
                new_content.push_str(new_string);
                new_content.push_str(&content[end..]);

                tokio::fs::write(path, &new_content).await.map_err(|e| {
                    ToolError::ExecutionFailed(format!("Failed to write '{}': {}", path, e))
                })?;

                Ok(json!({"status": "ok", "replacements": 1, "match": "fuzzy"}).to_string())
            }
            None => Err(ToolError::ExecutionFailed(format!(
                "Could not find a match for the specified text in '{}' \
                 (even with allow_fuzzy: true)",
                path
            ))),
        }
    }

    /// Multi-edit implementation: read the file once, apply all edits
    /// sequentially in memory, write once. If any edit fails to match,
    /// the file is NOT modified and the error is returned immediately.
    ///
    /// This avoids N disk reads + N disk writes for N edits, and keeps
    /// the operation atomic — either all edits apply or none do.
    async fn patch_file_multi(
        &self,
        path: &str,
        edits: &[(String, String, bool)],
        allow_fuzzy: bool,
    ) -> Result<String, ToolError> {
        // Read the file once.
        let mut content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read '{}': {}", path, e)))?;

        // 安全中心：覆盖前把旧版本存进备份目录（best-effort，失败不影响写）。
        crate::backup::try_backup(std::path::Path::new(path), content.as_bytes());

        let mut applied: Vec<Value> = Vec::with_capacity(edits.len());

        for (i, (old_string, new_string, replace_all)) in edits.iter().enumerate() {
            if old_string.is_empty() {
                // Empty old_string = append (same semantics as single patch).
                // Only valid as the LAST edit — if it's not last, the
                // subsequent edits would operate on appended text, which
                // is rarely what the LLM intends.
                content = if content.is_empty() {
                    new_string.clone()
                } else {
                    format!("{}\n{}", content, new_string)
                };
                applied.push(json!({"edit": i + 1, "action": "appended"}));
                continue;
            }

            if *replace_all {
                if !content.contains(old_string.as_str()) {
                    return Err(ToolError::ExecutionFailed(format!(
                        "Edit {} (replace_all): could not find '{}' in '{}'",
                        i + 1,
                        &old_string[..old_string.len().min(100)],
                        path
                    )));
                }
                let count = content.matches(old_string.as_str()).count();
                content = content.replace(old_string.as_str(), new_string.as_str());
                applied.push(json!({"edit": i + 1, "replacements": count}));
            } else {
                match Self::fuzzy_find(&content, old_string) {
                    Some((start, end)) => {
                        let mut new_content = String::with_capacity(content.len());
                        new_content.push_str(&content[..start]);
                        new_content.push_str(new_string.as_str());
                        new_content.push_str(&content[end..]);
                        content = new_content;
                        applied.push(json!({"edit": i + 1, "replacements": 1}));
                    }
                    None => {
                        return Err(ToolError::ExecutionFailed(format!(
                            "Edit {}: could not find a match for the specified text in '{}'",
                            i + 1,
                            path
                        )));
                    }
                }
            }
        }

        // Write the final content once.
        tokio::fs::write(path, &content)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to write '{}': {}", path, e)))?;

        Ok(json!({
            "status": "ok",
            "edits_applied": edits.len(),
            "details": applied,
        })
        .to_string())
    }
}

// ---------------------------------------------------------------------------
// LocalSearchBackend
// ---------------------------------------------------------------------------

/// Real file search backend using regex for content and glob for filenames.
pub struct LocalSearchBackend;

impl LocalSearchBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LocalSearchBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SearchBackend for LocalSearchBackend {
    async fn search_content(
        &self,
        pattern: &str,
        path: &str,
        file_glob: Option<&str>,
        max_results: Option<usize>,
        context: usize,
    ) -> Result<String, ToolError> {
        let re = Regex::new(pattern)
            .map_err(|e| ToolError::InvalidParams(format!("Invalid regex pattern: {}", e)))?;

        let max = max_results.unwrap_or(50);
        let mut results: Vec<Value> = Vec::new();

        let path = std::path::Path::new(path);
        if !path.exists() {
            return Err(ToolError::ExecutionFailed(format!(
                "Path '{}' does not exist",
                path.display()
            )));
        }

        Self::search_dir_content(&re, path, file_glob, max, context, &mut results).await;

        Ok(json!({
            "matches": results,
            "total": results.len(),
            "pattern": pattern,
        })
        .to_string())
    }

    async fn search_files(&self, pattern: &str, path: &str) -> Result<String, ToolError> {
        let mut results: Vec<Value> = Vec::new();
        let base = std::path::Path::new(path);

        if !base.exists() {
            return Err(ToolError::ExecutionFailed(format!(
                "Path '{}' does not exist",
                base.display()
            )));
        }

        Self::search_dir_names(pattern, base, &mut results).await;

        Ok(json!({
            "files": results,
            "total": results.len(),
            "pattern": pattern,
        })
        .to_string())
    }
}

const MAX_SEARCH_DEPTH: u32 = 12;

impl LocalSearchBackend {
    async fn search_dir_content(
        re: &Regex,
        dir: &std::path::Path,
        file_glob: Option<&str>,
        max: usize,
        context: usize,
        results: &mut Vec<Value>,
    ) {
        Self::search_dir_content_depth(re, dir, file_glob, max, context, results, 0).await;
    }

    async fn search_dir_content_depth(
        re: &Regex,
        dir: &std::path::Path,
        file_glob: Option<&str>,
        max: usize,
        context: usize,
        results: &mut Vec<Value>,
        depth: u32,
    ) {
        if results.len() >= max || depth >= MAX_SEARCH_DEPTH {
            return;
        }

        let entries = match tokio::fs::read_dir(dir).await {
            Ok(e) => e,
            Err(_) => return,
        };

        let mut entries = entries;
        while let Ok(Some(entry)) = entries.next_entry().await {
            if results.len() >= max {
                break;
            }

            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

            if name.starts_with('.')
                || name == "node_modules"
                || name == "target"
                || name == "__pycache__"
            {
                continue;
            }

            if path.is_dir() {
                Box::pin(Self::search_dir_content_depth(
                    re,
                    &path,
                    file_glob,
                    max,
                    context,
                    results,
                    depth + 1,
                ))
                .await;
            } else if path.is_file() {
                // Check glob filter
                if let Some(glob) = file_glob {
                    if !Self::matches_glob(name, glob) {
                        continue;
                    }
                }

                // Try to read as text
                if let Ok(content) = tokio::fs::read_to_string(&path).await {
                    let lines: Vec<&str> = content.lines().collect();
                    let file = path.display().to_string();
                    let mut i = 0usize;
                    while i < lines.len() {
                        if results.len() >= max {
                            break;
                        }
                        if !re.is_match(lines[i]) {
                            i += 1;
                            continue;
                        }
                        // H5: emit the matched line plus up to
                        // `context` lines either side, so the model can
                        // judge the match without a second read_file
                        // round-trip. Line numbers are 1-based.
                        let start = i.saturating_sub(context);
                        let end = (i + context + 1).min(lines.len());
                        let mut block = String::new();
                        for (k, l) in lines[start..end].iter().enumerate() {
                            if k > 0 {
                                block.push('\n');
                            }
                            block.push_str(&format!(
                                "{}: {}",
                                start + k + 1,
                                l.chars().take(500).collect::<String>()
                            ));
                        }
                        results.push(json!({
                            "file": file,
                            "line": i + 1,
                            "content": block,
                        }));
                        // Skip past the emitted context so overlapping
                        // blocks do not spam the result set.
                        i = end;
                    }
                }
            }
        }
    }

    async fn search_dir_names(pattern: &str, dir: &std::path::Path, results: &mut Vec<Value>) {
        Self::search_dir_names_depth(pattern, dir, results, 0).await;
    }

    async fn search_dir_names_depth(
        pattern: &str,
        dir: &std::path::Path,
        results: &mut Vec<Value>,
        depth: u32,
    ) {
        if depth >= MAX_SEARCH_DEPTH {
            return;
        }

        let entries = match tokio::fs::read_dir(dir).await {
            Ok(e) => e,
            Err(_) => return,
        };

        let mut entries = entries;
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

            if name.starts_with('.') || name == "node_modules" || name == "target" {
                continue;
            }

            if Self::matches_glob(name, pattern) {
                results.push(json!({
                    "path": path.display().to_string(),
                    "name": name,
                    "is_dir": path.is_dir(),
                }));
            }

            if path.is_dir() {
                Box::pin(Self::search_dir_names_depth(
                    pattern,
                    &path,
                    results,
                    depth + 1,
                ))
                .await;
            }
        }
    }

    fn matches_glob(name: &str, pattern: &str) -> bool {
        // Simple glob matching: * matches any sequence, ? matches single char
        let re_pattern = pattern
            .replace('.', "\\.")
            .replace('*', ".*")
            .replace('?', ".");
        Regex::new(&format!("^{}$", re_pattern))
            .map(|re| re.is_match(name))
            .unwrap_or(false)
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn write_tmp(name: &str, body: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, body).unwrap();
        let s = path.to_string_lossy().to_string();
        (dir, s)
    }

    // ---- H2 behaviour contract: patch uniqueness --------------------
    //
    // The old code ran an 8-strategy fuzzy chain whenever the literal
    // missed, so a one-space difference could silently rewrite a
    // DIFFERENT occurrence of the same shape. These pin the corrected
    // contract: literal-only by default, ambiguity is an error, and
    // fuzzy is opt-in.

    #[tokio::test]
    async fn patch_applies_a_unique_literal_match() {
        let (_d, path) = write_tmp("u.txt", "alpha\nbeta\ngamma\n");
        let out = LocalPatchBackend::new()
            .patch_file(&path, "beta", "BETA", false, false)
            .await
            .unwrap();
        assert!(out.contains("\"match\":\"literal\""), "got: {out}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha\nBETA\ngamma\n");
    }

    #[tokio::test]
    async fn patch_refuses_ambiguous_literal_instead_of_guessing() {
        // Two identical lines: patching "x" could hit either. The old
        // code picked the first silently.
        let (_d, path) = write_tmp("a.txt", "x\nkeep\nx\n");
        let err = LocalPatchBackend::new()
            .patch_file(&path, "x", "y", false, false)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ambiguous") || msg.contains("occurrences"), "got: {msg}");
        // Critically: the file must be untouched.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "x\nkeep\nx\n",
            "an ambiguous patch must not modify the file"
        );
    }

    #[tokio::test]
    async fn patch_without_literal_and_without_fuzzy_does_not_modify() {
        // A whitespace-only difference used to be silently rescued by the
        // fuzzy chain — possibly onto the wrong line. It must now refuse.
        // NB: the needle must differ from the file in a way that is NOT
        // a substring, otherwise it IS a literal hit.
        let (_d, path) = write_tmp("w.txt", "fn main() {\n\tbody();\n}\n");
        let original = std::fs::read_to_string(&path).unwrap();
        let err = LocalPatchBackend::new()
            .patch_file(&path, "  body();\n", "    body();\n", false, false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("verbatim"), "got: {err}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "a refused patch must leave the file alone"
        );
    }

    #[tokio::test]
    async fn patch_fuzzy_is_reachable_on_explicit_opt_in() {
        let (_d, path) = write_tmp("f.txt", "fn main() {\n\tbody();\n}\n");
        LocalPatchBackend::new()
            .patch_file(&path, "  body();\n", "    body();\n", false, true)
            .await
            .expect("allow_fuzzy: true must reach the fuzzy chain");
        assert!(std::fs::read_to_string(&path).unwrap().contains("    body();"));
    }

    #[tokio::test]
    async fn patch_replace_all_still_works_without_fuzzy() {
        let (_d, path) = write_tmp("r.txt", "x\ny\nx\n");
        LocalPatchBackend::new()
            .patch_file(&path, "x", "z", true, false)
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "z\ny\nz\n");
    }

    // ---- H5 behaviour contract: search context lines ----------------
    //
    // Before this, search_files returned only the matched line, so the
    // model had to issue a second read_file to judge whether a hit was
    // relevant. `context` is grep's -C.

    #[tokio::test]
    async fn search_context_includes_surrounding_lines() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("a.txt"),
            "one\ntwo\nNEEDLE\nfour\nfive\n",
        )
        .unwrap();
        let out = LocalSearchBackend::new()
            .search_content("NEEDLE", dir.path().to_str().unwrap(), None, Some(10), 2)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let content = v["matches"][0]["content"].as_str().unwrap();
        // context=2 around line 3 → lines 1..5, each prefixed with its
        // 1-based line number so the model can cite it.
        assert_eq!(content, "1: one\n2: two\n3: NEEDLE\n4: four\n5: five", "got: {content}");
    }

    #[tokio::test]
    async fn search_context_zero_keeps_matched_line_only() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("b.txt"), "alpha\nNEEDLE\nomega\n").unwrap();
        let out = LocalSearchBackend::new()
            .search_content("NEEDLE", dir.path().to_str().unwrap(), None, Some(10), 0)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let content = v["matches"][0]["content"].as_str().unwrap();
        // context=0 adds no neighbours, but the line number prefix stays
        // (it is what lets the model cite the hit precisely).
        assert_eq!(content, "2: NEEDLE", "got: {content}");
    }
}