//! Skill loader — auto-matches user messages to skills and injects instructions.
//!
//! (Phase 6 — P6-3)
//!
//! The existing [`SkillOrchestrator`] handles slash commands (`/skill-name`)
//! and session-wide preloading. This module adds **automatic skill matching**:
//!
//! 1. **Keyword matching**: When a user message contains keywords that match
//!    a skill's name, description, or tags, the skill's instructions are
//!    injected into the system prompt for that turn.
//!
//! 2. **Capability gap fallback** (`find-skills`): When the LLM encounters a
//!    task it can't complete with the available tools, it can call
//!    `find-skills` to discover skills that might help. This is the
//!    "能力缺口兜底" (capability gap backstop) from the 铁律 agent_skills.
//!
//! ## Integration
//!
//! The orchestration core calls [`SkillLoader::match_message`] at the start
//! of each turn with the user's message text. If a skill matches, its
//! instructions are returned and injected into the system prompt alongside
//! the existing `<available_skills>` block.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::skill_orchestrator::{SkillCommandInfo, SkillOrchestrator};

// ---------------------------------------------------------------------------
// SkillMatch
// ---------------------------------------------------------------------------

/// A skill that matched a user message.
#[derive(Debug, Clone)]
pub struct SkillMatch {
    /// The slash command key (e.g. `/commit-message`).
    pub command_key: String,
    /// The skill name.
    pub name: String,
    /// The skill description.
    pub description: String,
    /// The relevance score (0.0 to 1.0).
    pub score: f32,
    /// The full skill body (instructions to inject).
    pub body: String,
    /// Which field matched: "name", "description", or "tags".
    pub matched_on: String,
}

// ---------------------------------------------------------------------------
// SkillLoader
// ---------------------------------------------------------------------------

/// Auto-matches user messages to skills and produces injection text.
///
/// Wraps a [`SkillOrchestrator`] and adds keyword-based matching on top of
/// the slash-command resolution. The orchestrator handles discovery (scanning
/// `~/.hermix/skills/` for SKILL.md files); the loader adds the matching
/// layer.
pub struct SkillLoader {
    orchestrator: SkillOrchestrator,
    /// Minimum score threshold for a match (default 0.3).
    min_score: f32,
    /// Maximum number of auto-matched skills to inject per turn (default 3).
    max_matches: usize,
    /// Maximum lines of an auto-injected SKILL.md body (default 500).
    /// Soft cap — longer bodies are truncated to this many lines, not
    /// an error.
    max_body_lines: usize,
}

impl SkillLoader {
    /// Create a new loader using the default skills directory.
    pub fn new() -> Self {
        Self {
            orchestrator: SkillOrchestrator::default_dir(),
            min_score: 0.3,
            max_matches: 3,
            max_body_lines: 500,
        }
    }

    /// Create a new loader with a specific skills directory.
    pub fn with_dir(skills_dir: impl Into<PathBuf>) -> Self {
        Self {
            orchestrator: SkillOrchestrator::new(skills_dir),
            min_score: 0.3,
            max_matches: 3,
            max_body_lines: 500,
        }
    }

    /// Set the minimum score threshold.
    pub fn with_min_score(mut self, score: f32) -> Self {
        self.min_score = score;
        self
    }

    /// Set the maximum number of auto-matched skills per turn.
    pub fn with_max_matches(mut self, max: usize) -> Self {
        self.max_matches = max;
        self
    }

    /// Override the per-skill body line cap. Default is 500; we
    /// expose the setter so tests can pin a tiny cap (5 lines) to
    /// assert the truncation footer fires deterministically.
    pub fn with_max_body_lines(mut self, max: usize) -> Self {
        self.max_body_lines = max;
        self
    }

    /// Scan for skills (delegates to the orchestrator).
    pub fn scan(&mut self) -> &HashMap<String, SkillCommandInfo> {
        self.orchestrator.scan_skill_commands()
    }

    /// Get the underlying orchestrator.
    pub fn orchestrator(&self) -> &SkillOrchestrator {
        &self.orchestrator
    }

    /// Match a user message to skills.
    ///
    /// Returns up to `max_matches` skills whose name, description, or tags
    /// contain keywords from the message. The score is based on the ratio
    /// of matched keywords to total message keywords. P2-T2 added a
    /// fuzzy subscore on top.
    ///
    /// Effective threshold scales with query length: a single-token
    /// query needs ≥ 0.3 (rich signal required), a multi-token query
    /// only needs ≥ 0.15 (because 1 hit / 6 stop-words is a real
    /// relevance signal that a fixed 0.3 threshold would suppress).
    pub fn match_message(&self, message: &str) -> Vec<SkillMatch> {
        let commands = self.orchestrator.get_commands();
        if commands.is_empty() {
            return Vec::new();
        }

        let msg_lower = message.to_lowercase();
        let msg_tokens: Vec<&str> = msg_lower
            .split_whitespace()
            .filter(|t| t.len() >= 3) // skip short tokens
            .filter(|t| !STOPWORDS.contains(t)) // skip trivial words
            .collect();

        if msg_tokens.is_empty() {
            return Vec::new();
        }

        // Token-aware effective threshold — but ONLY soften the
        // default `min_score`. Operator-supplied thresholds
        // (e.g. a test pinning min_score = 0.99) are honored
        // verbatim regardless of query length.
        let effective_min = if msg_tokens.len() == 1 {
            self.min_score.max(0.3)
        } else if self.min_score <= 0.3 {
            0.15
        } else {
            self.min_score
        };

        let mut scored: Vec<SkillMatch> = Vec::new();

        for (cmd_key, info) in commands {
            let name_lower = info.name.to_lowercase();
            let desc_lower = info.description.to_lowercase();
            let tags_lower: Vec<String> = info.tags.iter().map(|t| t.to_lowercase()).collect();

            // Whole-word desc-set so the cheap keyword pass doesn't
            // light up on substrings like "and" inside "safe and
            // effective". The same split is used by the fuzzy helper
            // later in the loop, so we cache it once per skill.
            let desc_words: std::collections::HashSet<&str> = desc_lower
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| !w.is_empty())
                .collect();

            // Score: how many message tokens appear in the skill's fields.
            // Phase 2 (P2-T2): we blend the cheap keyword-overlap pass with
            // a subsequence-based fuzzy score so a query like "compres"
            // still surfaces "context-compression". The exact-token score
            // always wins over the fuzzy one — we never want fuzzy hits to
            // crowd out a clean keyword match.
            let mut matched_tokens = 0;
            let mut matched_on = String::new();

            for token in &msg_tokens {
                let in_name = name_lower.split(|c: char| !c.is_alphanumeric())
                    .filter(|w| !w.is_empty())
                    .any(|w| w == *token);
                let in_desc = desc_words.contains(*token);
                // Tags are short tags; we accept both whole-tag
                // equality (preserving hyphens, "best-practices")
                // and word-equality on the hyphen-split so a
                // bare query token like "best" can still surface
                // a "best-practices"-tagged skill.
                let in_tags = tags_lower.iter().any(|t| {
                    if t == *token {
                        return true;
                    }
                    t.split(|c: char| !c.is_alphanumeric())
                        .filter(|w| !w.is_empty())
                        .any(|w| w == *token)
                });

                if in_name || in_desc || in_tags {
                    matched_tokens += 1;
                    if in_name && matched_on.is_empty() {
                        matched_on = "name".into();
                    } else if in_desc && matched_on.is_empty() {
                        matched_on = "description".into();
                    } else if in_tags && matched_on.is_empty() {
                        matched_on = "tags".into();
                    }
                }
            }

            // --- CJK (Chinese) trigger fallback ---
            // The English-oriented tokenizer above keeps a Chinese
            // sentence (no whitespace) as one long token, so it
            // never matches the short description words.
            // `cjk_bigram_hit` adds a curated bigram-overlap check
            // against the skill's tags/name so Chinese queries fire.
            // See the helper for the false-positive bounding.
            if cjk_bigram_hit(&msg_lower, &name_lower, &tags_lower) {
                matched_tokens = matched_tokens.max(1);
                if matched_on.is_empty() {
                    matched_on = "cjk".to_string();
                }
            }

            let keyword_score = matched_tokens as f32 / msg_tokens.len() as f32;
            let fuzzy_score = description_fuzzy_score(&msg_tokens, &desc_lower);
            let score = keyword_score.max(fuzzy_score);

            if score < effective_min {
                continue;
            }

            // Load the skill body
            let body = match std::fs::read_to_string(&info.skill_md_path) {
                Ok(content) => {
                    let (_, body) = crate::skill_orchestrator::parse_frontmatter(&content);
                    let trimmed = body.trim();
                    // Soft-cap body at `max_body_lines` lines so a 5,000-line
                    // skill doesn't blow up the context. Trailing truncation
                    // is not an error — the rest is still on disk and
                    // retrievable via `skill_view` if the LLM needs more.
                    let capped: String = if trimmed.lines().count() > self.max_body_lines {
                        let mut out = String::new();
                        for (i, line) in trimmed.lines().enumerate() {
                            if i >= self.max_body_lines {
                                out.push_str(&format!(
                                    "\n\n[…truncated at {} lines; use skill_view to load the rest.]",
                                    self.max_body_lines
                                ));
                                break;
                            }
                            out.push_str(line);
                            out.push('\n');
                        }
                        out
                    } else {
                        trimmed.to_string()
                    };
                    capped
                }
                Err(_) => continue,
            };

            scored.push(SkillMatch {
                command_key: cmd_key.clone(),
                name: info.name.clone(),
                description: info.description.clone(),
                score,
                body,
                matched_on,
            });
        }

        // Sort by score descending, take top N
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(self.max_matches);
        scored
    }

    /// Build the injection text for matched skills.
    ///
    /// Returns `None` if no skills matched. The returned text is suitable
    /// for appending to the system prompt.
    pub fn build_injection(&self, message: &str) -> Option<String> {
        let matches = self.match_message(message);
        if matches.is_empty() {
            return None;
        }

        let mut parts = Vec::new();
        parts.push(
            "## Auto-Matched Skills (system-injected)\n\
             The following skills were automatically matched to the user's message. \
             Review their instructions and apply them if relevant to the current task.\n"
                .to_string(),
        );

        for m in &matches {
            parts.push(format!(
                "### {} (score: {:.2}, matched on: {})\n{}\n",
                m.name, m.score, m.matched_on, m.body
            ));
        }

        Some(parts.join("\n"))
    }

    /// Find skills for a capability gap.
    ///
    /// When the LLM can't complete a task with the available tools, it can
    /// describe what it needs and this function returns skills that might
    /// help. This is the "find-skills 能力缺口兜底" from 铁律 agent_skills.
    pub fn find_skills_for_capability_gap(&self, description: &str) -> Vec<SkillMatch> {
        // Reuse the same matching logic — the "capability gap description"
        // is treated like a user message for matching purposes.
        self.match_message(description)
    }

    /// Build the find-skills response text for the LLM.
    ///
    /// Returns a JSON string listing matching skills with their names,
    /// descriptions, and command keys. The LLM can then use `skill_view`
    /// to inspect a skill's full content.
    pub fn find_skills_response(&self, description: &str) -> String {
        let matches = self.find_skills_for_capability_gap(description);
        if matches.is_empty() {
            return serde_json::json!({
                "found": false,
                "message": "No skills found matching the description. Try rephrasing or use skills_list to see all available skills.",
            })
            .to_string();
        }

        let results: Vec<serde_json::Value> = matches
            .iter()
            .map(|m| {
                serde_json::json!({
                    "name": m.name,
                    "command": m.command_key,
                    "description": m.description,
                    "score": m.score,
                    "matched_on": m.matched_on,
                })
            })
            .collect();

        serde_json::json!({
            "found": true,
            "count": results.len(),
            "skills": results,
            "hint": "Use skill_view with the skill name to see full instructions.",
        })
        .to_string()
    }
}

impl Default for SkillLoader {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Stopword filter (kept tiny on purpose — the auto-router is
// strict about activating skills, and any word on this list would
// false-positive virtually every skill with prose descriptions).
// ---------------------------------------------------------------------------

const STOPWORDS: &[&str] = &[
    // Pure function / grammatical glue — never a domain concept.
    "and", "the", "for", "with", "are", "that", "this",
    "your", "can", "use", "how", "what", "why", "who", "when",
    "from", "into", "have", "has", "had", "but", "not", "any",
    "all", "some", "out", "also", "just", "than", "then", "now",
    "tell", "give", "make", "about",
];

// ---------------------------------------------------------------------------
// Fuzzy matching helpers (Phase 2 — P2-T2)
// ---------------------------------------------------------------------------

/// Returns true if `needle`'s characters all appear in `haystack`,
/// in order, with adjacent matches no more than 5 characters apart.
/// This is the cheap fuzzy backbone the keyword pass falls back to
/// when exact word-overlap misses.
///
/// The 5-char gap budget is what separates a real fuzzy hit ("compres"
/// spans the inside of "compression") from a sparse false positive
/// ("world" picks up w/o/r/l/d from "review code for quality and best
/// practices" each separated by 2-12 unrelated characters).
///
/// Example: subsequence_match("compres", "context compression")
/// returns true; subsequence_match("momeression", "context
/// compression") returns false. It is intentionally *cheap* — no
/// Levenshtein, no embeddings — so it stays deterministic and
/// synchronous (the router runs on every turn).
/// Returns true if `needle`'s characters all appear **inside a
/// single word** of `haystack`, in order, with adjacent matches no
/// more than 3 characters apart.
///
/// Word-boundary split is what separates a real typo-bridge from
/// the historical false positive where `needle="world"` substring-
/// matched "review code for quality and best practices" (w / o / r /
/// l / d each picked up from unrelated words). Restricting the
/// search to one word at a time rejects that pattern while still
/// surfacing within-word partial-prefixes (`compres` → `compression`,
/// `memry` → `memory`).
pub fn subsequence_match(needle: &str, haystack: &str) -> bool {
    const MAX_GAP: usize = 3;

    let needle = needle.trim();
    if needle.is_empty() {
        return false;
    }
    if needle.len() < 3 {
        // Anything shorter than 3 chars trips false positives
        // everywhere ("is" subsequence-matches nearly every word).
        return false;
    }
    // Reject needles that consist only of punctuation —
    // they'd vacuously "match" everything because the loop
    // skips non-alphanumeric chars and there are zero
    // characters left to fail the iteration.
    if needle.chars().all(|c| !c.is_alphanumeric()) {
        return false;
    }

    // Try each word in the haystack independently. A real typo
    // bridge lives inside a single word; the sparse multi-word
    // case is exactly what we want to filter out.
    for word in haystack.split(|c: char| !c.is_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        if subsequence_match_within_word(needle, word, MAX_GAP) {
            return true;
        }
    }
    false
}

/// Internal: per-word subsequence check with a configurable gap.
fn subsequence_match_within_word(needle: &str, word: &str, max_gap: usize) -> bool {
    let w_chars: Vec<char> = word.chars().collect();
    let mut last_pos: Option<usize> = None;

    for nc in needle.chars() {
        if !nc.is_alphanumeric() {
            continue;
        }
        let start = last_pos.map(|p| p + 1).unwrap_or(0);
        let mut found = None;
        for (i, &hc) in w_chars.iter().enumerate().skip(start) {
            if hc.eq_ignore_ascii_case(&nc) {
                found = Some(i);
                break;
            }
        }
        let i = match found {
            Some(i) => i,
            None => return false,
        };
        if let Some(last) = last_pos {
            if i - last > max_gap {
                return false;
            }
        }
        last_pos = Some(i);
    }
    true
}

/// Returns a fuzzy score (0.0–1.0) for how well a user's message
/// tokens align with a skill description. We blend two cheap
/// signals so a richly-described skill beats a thinly-described
/// skill for the same query:
///
///   - exact-token-overlap (count of shared tokens / total message
///     tokens)
///   - subsequence coverage (fraction of message tokens with at
///     least one descriptor-character subsequence hit)
///
/// The blend weights are picked to favour the cheaper signal but
/// still reward descriptions that *contain* the user's intent even
/// when word boundaries differ (e.g. "memory" vs "memory-systems"
/// vs "persistent memory across sessions").
pub fn description_fuzzy_score(message_tokens: &[&str], description_lower: &str) -> f32 {
    if message_tokens.is_empty() {
        return 0.0;
    }

    // Whole-word membership check so a token like "compres" does
    // NOT count as a substring-hit inside "compression". The
    // `split_whitespace()` cost is one-shot — we cache the word set
    // for the whole token list iteration.
    let desc_words: std::collections::HashSet<&str> = description_lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();

    let mut exact_hits = 0usize;
    let mut subseq_hits = 0usize;

    for token in message_tokens {
        if desc_words.contains(*token) {
            exact_hits += 1;
        } else if subsequence_match(token, description_lower) {
            subseq_hits += 1;
        }
    }

    let exact_score = exact_hits as f32 / message_tokens.len() as f32;
    // A weighted-token score: an exact hit counts as 1.0, a
    // subsequence-only hit counts as 0.5 (still substantive but
    // not as good as a clean word overlap). The combined ratio
    // stays in 0.0–1.0. We then take the max with `exact_score`
    // so descriptions with strong whole-word matches always win.
    //
    // Examples with default `min_score = 0.3`:
    //   "compres" 1 token 1 subseq → 0.5/1 = 0.5  ✓ activate
    //   "memory and compres" 3 tokens (memory exact + compres
    //     subseq) → (1 + 0.5) / 3 ≈ 0.5 ✓ activate
    //   "hello world" 2 tokens 1 subseq → 0.5 / 2 = 0.25 ✗ reject
    let weighted = exact_hits as f32 + subseq_hits as f32 * 0.5;
    let weighted_score = (weighted / message_tokens.len() as f32).min(1.0);

    exact_score.max(weighted_score)
}

// ---------------------------------------------------------------------------
// CJK (Chinese) trigger fallback
// ---------------------------------------------------------------------------

/// True if `s` contains any CJK ideograph (BMP Unified + Ext A).
/// Good enough for trigger-phrase routing; we don't need full
/// Unicode script detection.
fn has_cjk(s: &str) -> bool {
    s.chars().any(|c| {
        ('\u{4E00}'..='\u{9FFF}').contains(&c) || ('\u{3400}'..='\u{4DBF}').contains(&c)
    })
}

/// Extract 2-character CJK bigrams from `s`. Only runs of CJK
/// characters are bigrammed; ASCII/whitespace are skipped. This is
/// the classic cheap Chinese IR trick: it makes word-order
/// independent ("清理磁盘" vs "磁盘清理" both surface the 清理/磁盘
/// bigrams) and survives natural phrasing ("帮我清理一下电脑磁盘"
/// still contains the 清理/磁盘 bigrams).
fn cjk_bigrams(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if has_cjk(&chars[i].to_string()) {
            let start = i;
            let mut end = i + 1;
            while end < chars.len() && has_cjk(&chars[end].to_string()) {
                end += 1;
            }
            let run: Vec<char> = chars[start..end].to_vec();
            for w in run.windows(2) {
                out.push(w.iter().collect());
            }
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

/// Returns true if the user message's CJK bigrams overlap the
/// skill's curated CJK trigger `tags`/`name` by at least 2 distinct
/// bigrams.
///
/// We match against the skill's *tags* and *name* (hand-authored by
/// the skill author), NOT the whole description prose — this bounds
/// the false-positive surface to the tag vocabulary. The ≥2-bigram
/// threshold suppresses single-common-word false positives (e.g.
/// "分析一下这个项目" shares only the 分析 bigram with the
/// storage-analyzer skill, which is below the threshold and would
/// otherwise light it up).
fn cjk_bigram_hit(msg_lower: &str, name_lower: &str, tags_lower: &[String]) -> bool {
    if !has_cjk(msg_lower) {
        return false;
    }
    let msg_bigrams = cjk_bigrams(msg_lower);
    let mut consider: Vec<String> = tags_lower.to_vec();
    if has_cjk(name_lower) {
        consider.push(name_lower.to_string());
    }
    // Curated-tag substring match: the message contains a full trigger
    // phrase (e.g. "c盘", "清理磁盘"). This catches cases where
    // bigrams alone fall short — a single-CJK-char tail like "C盘"
    // yields no bigram, and a short phrase may share only one bigram
    // with a tag.
    for t in &consider {
        if has_cjk(t) && msg_lower.contains(t) {
            return true;
        }
    }
    // Otherwise require ≥2 shared bigrams to suppress single-common-
    // word false positives (e.g. "分析一下这个项目" shares only the
    // 分析 bigram with the storage skill).
    if msg_bigrams.len() < 2 {
        return false;
    }
    let mut shared: std::collections::HashSet<String> = std::collections::HashSet::new();
    for t in &consider {
        if !has_cjk(t) {
            continue;
        }
        for bg in cjk_bigrams(t) {
            if msg_bigrams.contains(&bg) {
                shared.insert(bg);
            }
        }
    }
    shared.len() >= 2
}



#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_test_skills() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        // Create a few test skills. Tags are comma-separated inside
        // the inline list so the YAML parser actually splits them
        // (an unspaced single string inside `[]` collapses into one
        // tag, which doesn't reflect how real SKILL.md files look).
        let skills = [
            (
                "commit-message",
                "Format a Conventional Commit message",
                vec!["git", "commit", "conventional", "message", "format"],
                "# Commit Message Skill\n\nWhen writing a commit message, use the Conventional Commits format.",
            ),
            (
                "code-review",
                "Review code for quality and best practices",
                vec!["code", "review", "quality", "best-practices"],
                "# Code Review Skill\n\nReview the code for quality, security, and best practices.",
            ),
            (
                "pdf-extract",
                "Extract text and data from PDF files",
                vec!["pdf", "extract", "text", "data"],
                "# PDF Extract Skill\n\nUse read_pdf to extract text from PDF files.",
            ),
        ];

        for (name, desc, tags, body) in &skills {
            let skill_dir = dir.join(name);
            fs::create_dir_all(&skill_dir).unwrap();
            let tags_inline = tags.join(", ");
            let content = format!(
                "---\nname: {name}\ndescription: {desc}\ntags: [{tags_inline}]\n---\n{body}"
            );
            fs::write(skill_dir.join("SKILL.md"), content).unwrap();
        }

        tmp
    }

    #[test]
    fn test_match_message_finds_relevant_skill() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let matches = loader.match_message("help me write a git commit message");
        assert!(!matches.is_empty());
        // The commit-message skill should be in the top matches
        let names: Vec<&str> = matches.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"commit-message"));
    }

    #[test]
    fn test_match_message_returns_empty_for_irrelevant() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let matches = loader.match_message("hello world");
        // "hello" and "world" don't match any skill
        assert!(matches.is_empty());
    }

    #[test]
    fn test_match_message_respects_max_matches() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path()).with_max_matches(1);
        loader.scan();

        let matches = loader.match_message("code review commit message");
        assert!(!matches.is_empty());
        assert!(matches.len() <= 1);
    }

    #[test]
    fn test_build_injection_returns_none_when_no_match() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let injection = loader.build_injection("hello world");
        assert!(injection.is_none());
    }

    #[test]
    fn test_build_injection_returns_text_when_matched() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let injection = loader.build_injection("help me write a commit message");
        assert!(injection.is_some());
        let text = injection.unwrap();
        assert!(text.contains("Auto-Matched Skills"));
        assert!(text.contains("commit-message"));
        assert!(text.contains("Conventional Commits"));
    }

    #[test]
    fn test_find_skills_for_capability_gap() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let matches = loader.find_skills_for_capability_gap("I need to extract text from a PDF");
        assert!(!matches.is_empty());
        let names: Vec<&str> = matches.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"pdf-extract"));
    }

    #[test]
    fn test_find_skills_response_json() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let response = loader.find_skills_response("I need to review code quality");
        let parsed: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed["found"], true);
        assert!(parsed["count"].as_u64().unwrap() > 0);
        assert!(parsed["skills"].is_array());
    }

    #[test]
    fn test_find_skills_response_empty() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let response = loader.find_skills_response("xyzzy qwerty");
        let parsed: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed["found"], false);
    }

    #[test]
    fn test_match_on_tags() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        // "best-practices" is a tag on the code-review skill
        let matches = loader.match_message("show me best-practices for coding");
        assert!(!matches.is_empty());
        let names: Vec<&str> = matches.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"code-review"));
    }

    #[test]
    fn test_score_threshold_filters_low_relevance() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path()).with_min_score(0.99);
        loader.scan();

        // Very high threshold — should filter out most matches
        let matches = loader.match_message("help me write a commit message about code review");
        // The score would be low because only a few tokens match each skill
        assert!(matches.is_empty() || matches.iter().all(|m| m.score >= 0.99));
    }

    #[test]
    fn test_empty_skills_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let matches = loader.match_message("anything at all");
        assert!(matches.is_empty());
    }

    #[test]
    fn test_nonexistent_skills_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let nonexistent = tmp.path().join("nonexistent");
        let mut loader = SkillLoader::with_dir(&nonexistent);
        loader.scan();

        let matches = loader.match_message("anything");
        assert!(matches.is_empty());
    }

    // --- CJK (Chinese) keyword auto-routing ---

    /// Build a temp skills dir with two skills that carry Chinese
    /// trigger tags: a storage-analyzer-like skill (磁盘/缓存/存储
    /// triggers) and a ponytail-like skill (懒人/最简/写代码 triggers).
    fn setup_cjk_test_skills() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let skills = [
            (
                "storage-analyzer",
                "Windows read-only storage analysis assistant",
                vec!["清理磁盘", "磁盘清理", "C盘", "空间不够", "清缓存", "存储分析"],
                "# Storage Analyzer\n\nScan disk usage and classify junk.",
            ),
            (
                "ponytail",
                "Force the laziest solution that works",
                vec!["懒人模式", "最简方案", "少写代码", "最小实现", "过度工程"],
                "# Ponytail\n\nWrite the minimal code that works.",
            ),
        ];
        for (name, desc, tags, body) in &skills {
            let skill_dir = tmp.path().join(name);
            fs::create_dir_all(&skill_dir).unwrap();
            let tags_inline = tags.join(", ");
            let content = format!(
                "---\nname: {name}\ndescription: {desc}\ntags: [{tags_inline}]\n---\n{body}"
            );
            fs::write(skill_dir.join("SKILL.md"), content).unwrap();
        }
        tmp
    }

    #[test]
    fn cjk_match_storage_chinese_query() {
        let tmp = setup_cjk_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        // Natural phrasing with extra filler words still fires.
        let matches = loader.match_message("帮我清理一下C盘");
        assert!(!matches.is_empty(), "Chinese storage query should match");
        let names: Vec<&str> = matches.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"storage-analyzer"), "got: {names:?}");
    }

    #[test]
    fn cjk_match_ponytail_chinese_query() {
        let tmp = setup_cjk_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let matches = loader.match_message("用最简方案写这段代码");
        assert!(!matches.is_empty(), "Chinese ponytail query should match");
        let names: Vec<&str> = matches.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"ponytail"), "got: {names:?}");
    }

    #[test]
    fn cjk_no_false_positive_on_common_words() {
        let tmp = setup_cjk_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        // "分析一下这个项目" only shares the single 分析 bigram with
        // the storage skill (below the ≥2 threshold) → must NOT fire.
        let matches = loader.match_message("分析一下这个项目");
        let names: Vec<&str> = matches.iter().map(|m| m.name.as_str()).collect();
        assert!(
            !names.contains(&"storage-analyzer"),
            "common-word query must not false-trigger storage: {names:?}"
        );

        // "帮我存储这个文件" — 存储 appears inside 存储分析 but as a
        // single bigram, below threshold → must NOT fire.
        let matches2 = loader.match_message("帮我存储这个文件");
        let names2: Vec<&str> = matches2.iter().map(|m| m.name.as_str()).collect();
        assert!(
            !names2.contains(&"storage-analyzer"),
            "store-file query must not false-trigger storage: {names2:?}"
        );
    }

    // --- P2-T2: fuzzy matching helpers ---

    #[test]
    fn p2_fuzzy_subsequence_matches_within_word() {
        // "compres" subsequence-matches inside "compression" itself —
        // we want fuzzy to bridge drop-final-letter typos without
        // crossing whitespace into unrelated words.
        assert!(subsequence_match("compres", "compression"));
        // "memry" — needle chars all appear in order inside
        // "memory" (m-e-m-r-y vs m-e-m-o-r-y, the missing 'o'
        // is just skipped over via the gap budget).
        assert!(subsequence_match("memry", "memory"));
    }

    #[test]
    fn p2_fuzzy_subsequence_rejects_substitution_typos() {
        // subsequence_match expects every needle char IN ORDER.
        // A character-substitution typo (e → o) is not a match —
        // that's Levenshtein territory, out of scope for our
        // cheap fuzzy pass.
        assert!(!subsequence_match("memery", "memory"));
    }

    #[test]
    fn p2_fuzzy_subsequence_finds_match_in_any_word() {
        // With the per-word split, "compres" finds the matching
        // "compression" word even when it's not the first word in
        // the description. This is the whole reason we restrict
        // to within-word: the *first* candidate word ("practices")
        // would have trapped a naive single-pass matcher if `c`
        // wasn't the trailing character gap.
        assert!(subsequence_match(
            "compres",
            "best practices for lossy compression of conversation history"
        ));
    }

    #[test]
    fn p2_fuzzy_subsequence_rejects_sparse_multi_word_hit() {
        // The historical false positive: "world" → "review code
        // for quality and best practices". Each char of `world`
        // lives in a different word; the per-word restriction
        // rejects the pattern even when no single word starts
        // with `w`.
        assert!(!subsequence_match(
            "world",
            "review code for quality and best practices"
        ));
    }

    #[test]
    fn p2_fuzzy_subsequence_requires_all_chars_in_order() {
        // "momeression" — same chars scrambled, must NOT match.
        assert!(!subsequence_match("momeression", "context compression"));
    }

    #[test]
    fn p2_fuzzy_subsequence_rejects_short_needles() {
        // 2-char needles trip false positives everywhere.
        assert!(!subsequence_match("is", "this is a sentence"));
        assert!(!subsequence_match("", "anything"));
    }

    #[test]
    fn p2_fuzzy_subsequence_skips_needle_punctuation() {
        // Punctuation in the needle is ignored at the char-iteration
        // level, so the matcher operates on alphanumeric runs only.
        // "h-y-p-e-n" effectively becomes "hypen" — a full ordered
        // hit inside the single haystack word "hypen-ated".
        assert!(subsequence_match("h-y-p-e-n", "hypen-ated real word"));
    }

    #[test]
    fn p2_fuzzy_score_single_subseq_hit_passes_default_threshold() {
        // "compres" against "compression of conversation history"
        // (within-word, no cross-word gap):
        //   → exact_hits = 0 (no whole-word `compres` in desc)
        //   → subseq_hits = 1, len = 1
        //   → exact_score = 0, weighted = 0.5, weighted_score = 0.5/1 = 0.5
        //   → max(0, 0.5) = 0.5 ≥ default `min_score` of 0.3
        let score = description_fuzzy_score(
            &["compres"],
            "compression of conversation history",
        );
        assert!(
            (score - 0.5).abs() < f32::EPSILON,
            "expected 0.5, got {score}"
        );
    }

    #[test]
    fn p2_fuzzy_score_sparse_subseq_across_tokens_stays_below_threshold() {
        // The historical false positive that bit us before P2-T2:
        // "world" subsequence-matches "review code for quality and
        // best practices" (w-o-r-l-d all scattered around), but
        // across two message tokens the discounted score must stay
        // under the default `min_score = 0.3` so we don't surface
        // an irrelevant skill.
        let score = description_fuzzy_score(
            &["hello", "world"],
            "review code for quality and best practices",
        );
        assert!(
            score < 0.3,
            "sparse subseq across 2 tokens should not pass 0.3, got {score}"
        );
    }

    #[test]
    fn p2_fuzzy_score_exact_token_dominates_fuzzy() {
        // Exact token overlap on 1 of 2 tokens → 0.5 ≥ any subseq path.
        let score = description_fuzzy_score(
            &["commit", "kangaroo"],
            "format a conventional commit message",
        );
        assert!(score >= 0.5, "exact hit should dominate, got {score}");
    }

    // --- P2-T1: body line-cap ---

    #[test]
    fn p2_body_cap_truncates_long_skills_without_erroring() {
        let tmp = tempfile::tempdir().unwrap();
        let long_dir = tmp.path().join("long-skill");
        std::fs::create_dir_all(&long_dir).unwrap();

        // Build a 200-line body — well past `with_max_body_lines(5)`.
        let mut body = String::from("---\nname: long-skill\ndescription: A long skill\ntags: [long]\n---\n# Long\n");
        for i in 0..200 {
            body.push_str(&format!("line {i}\n"));
        }
        std::fs::write(long_dir.join("SKILL.md"), body).unwrap();

        let mut loader = SkillLoader::with_dir(tmp.path())
            .with_max_matches(1)
            .with_max_body_lines(5);
        loader.scan();

        let matches = loader.match_message("show me a long skill");
        assert_eq!(matches.len(), 1);
        let m = &matches[0];

        // Body has the truncation footer at the configured line cap.
        assert!(
            m.body.contains("truncated at 5 lines"),
            "expected soft-cap footer, got body ending: …{}",
            &m.body[m.body.len().saturating_sub(80)..]
        );
        // It must NOT contain line 199 — that means truncation
        // actually fired and not just appended a label.
        assert!(!m.body.contains("line 199"));
    }
}
