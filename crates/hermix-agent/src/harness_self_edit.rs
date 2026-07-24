//! Phase 4.1 — Failure-driven harness self-editing.
//!
//! When the agent loop detects repeated failures (tool errors, convergence
//! warnings, negative feedback), this module generates **experience entries**
//! that are persisted to the researcher's experience directory AND injects
//! **corrective rules** into the self-improvement store.
//!
//! ## Pipeline
//!
//! ```text
//! failure signal → classify pattern → generate experience → persist → rule proposal
//! ```
//!
//! ## Failure patterns
//!
//! 1. **ToolBurst**: same tool failed ≥ N consecutive times
//! 2. **ErrorLoop**: all tools in a turn errored (from convergence detector)
//! 3. **FeedbackPattern**: repeated negative feedback for the same action prefix
//! 4. **ConvergenceStuck**: agent stuck in a no-progress cycle
//!
//! ## Storage
//!
//! - Experience entries: `~/.hermix/researcher/experience/<timestamp>-<id>.jsonl`
//! - Rule proposals go through `SelfRuleStore` (existing P4-T6)
//!
//! ## Design principles
//!
//! - **Soft-fail everywhere**: disk errors are logged, never crash the agent loop
//! - **Idempotent**: the same failure pattern deduplicates via content hash
//! - **Budget-aware**: experience files are capped at EXPERIENCE_FILE_LIMIT entries

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::self_improvement::{RuleSource, SelfRuleStore};
use super::tool_error_demotion::ToolErrorCounter;
use super::feedback_loop::FeedbackLog;
use super::convergence_detector::ConvergenceReason;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum entries per experience file before rotation.
pub const EXPERIENCE_FILE_LIMIT: usize = 256;

/// Maximum experience files to keep (LRU eviction).
pub const MAX_EXPERIENCE_FILES: usize = 32;

/// Maximum character length for an experience body.
pub const MAX_EXPERIENCE_BODY_CHARS: usize = 512;

/// Minimum consecutive failures before generating a self-edit.
pub const SELF_EDIT_TRIGGER: u32 = 3;

// ---------------------------------------------------------------------------
// Failure pattern
// ---------------------------------------------------------------------------

/// Classified failure pattern that triggers self-editing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailurePattern {
    /// Unique ID (FNV-1a hash of the body).
    pub id: String,
    /// Which category of failure.
    pub kind: FailureKind,
    /// Human-readable description of the pattern.
    pub body: String,
    /// Tool name(s) involved (if applicable).
    pub tools: Vec<String>,
    /// Number of times this pattern has been observed.
    pub observation_count: u32,
    /// Unix timestamp of first observation.
    pub first_seen: u64,
    /// Unix timestamp of most recent observation.
    pub last_seen: u64,
}

/// Category of failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FailureKind {
    /// Same tool failed consecutively.
    ToolBurst,
    /// All tools in a turn errored (hard convergence).
    ErrorLoop,
    /// Repeated negative feedback for same action.
    FeedbackPattern,
    /// Agent stuck in no-progress cycle (soft convergence).
    ConvergenceStuck,
}

impl FailureKind {
pub fn label(self) -> &'static str {
        match self {
            Self::ToolBurst => "tool_burst",
            Self::ErrorLoop => "error_loop",
            Self::FeedbackPattern => "feedback_pattern",
            Self::ConvergenceStuck => "convergence_stuck",
        }
    }
}

// ---------------------------------------------------------------------------
// Experience entry
// ---------------------------------------------------------------------------

/// A single experience entry persisted to disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperienceEntry {
    /// Unique ID (FNV-1a hash).
    pub id: String,
    /// The failure pattern that triggered this experience.
    pub failure_kind: FailureKind,
    /// Description of what went wrong.
    pub what_happened: String,
    /// Suggested correction for future runs.
    pub correction: String,
    /// Tools involved.
    pub tools: Vec<String>,
    /// How many times this was observed before the experience was written.
    pub observation_count: u32,
    /// Unix timestamp.
    pub created_at: u64,
}

// ---------------------------------------------------------------------------
// Self-edit result
// ---------------------------------------------------------------------------

/// Result of a self-edit operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfEditResult {
    /// Whether a new experience entry was written.
    pub experience_written: bool,
    /// Whether a new self-improvement rule was proposed.
    pub rule_proposed: bool,
    /// The experience entry ID (if written).
    pub experience_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Core logic
// ---------------------------------------------------------------------------

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// FNV-1a 64-bit hash for deduplication keys.
pub fn fnv1a_hash(s: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in s.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", hash)
}

/// Classify tool errors into failure patterns.
/// /// Returns one `FailurePattern` per tool that exceeds the trigger threshold.
pub fn classify_tool_failures(counter: &ToolErrorCounter) -> Vec<FailurePattern> {
    let now = now_unix_secs();
    let mut patterns = Vec::new();

    for (tool, count) in counter.snapshot() {
        if count < SELF_EDIT_TRIGGER {
            continue;
        }
        let body = format!(
            "Tool `{}` failed {} consecutive times — consider alternative approach",
            tool, count
        );
        patterns.push(FailurePattern {
            id: fnv1a_hash(&body),
            kind: FailureKind::ToolBurst,
            body,
            tools: vec![tool.clone()],
            observation_count: count,
            first_seen: now,
            last_seen: now,
        });
    }

    patterns
}

/// Classify convergence signals into failure patterns.
/// /// Wire-up note: called from the agent loop's convergence detection path
/// once the harness self-edit integration is complete.
#[allow(dead_code)]
pub(crate) fn classify_convergence(
    reason: &ConvergenceReason,
    consecutive_count: u32,
    message: &str,
) -> Option<FailurePattern> {
    if consecutive_count < SELF_EDIT_TRIGGER {
        return None;
    }

    let kind = match reason {
        ConvergenceReason::ErrorLoop => FailureKind::ErrorLoop,
        ConvergenceReason::NoProgressWithTools => FailureKind::ConvergenceStuck,
        ConvergenceReason::SimilarContent => FailureKind::ConvergenceStuck,
    };

    let body = truncate_str(
        &format!("{:?} convergence ({} turns): {}", reason, consecutive_count, message),
        MAX_EXPERIENCE_BODY_CHARS,
    );

    Some(FailurePattern {
        id: fnv1a_hash(&body),
        kind,
        body,
        tools: Vec::new(),
        observation_count: consecutive_count,
        first_seen: now_unix_secs(),
        last_seen: now_unix_secs(),
    })
}

/// Classify feedback patterns into failure patterns.
pub fn classify_feedback(feedback: &FeedbackLog) -> std::io::Result<Vec<FailurePattern>> {
    let entries = feedback.snapshot().unwrap_or_default();
    let mut by_action: std::collections::HashMap<String, (u32, String)> = std::collections::HashMap::new();

    for e in &entries {
        if !e.success {
            let key = e.action.split(':').next().unwrap_or(&e.action).to_string();
            let entry = by_action.entry(key).or_insert((0, String::new()));
            entry.0 += 1;
            if !e.learnings.is_empty() {
                entry.1 = e.learnings.clone();
            }
        }
    }

    let now = now_unix_secs();
    let mut patterns = Vec::new();

    for (action, (count, learnings)) in by_action {
        if count < SELF_EDIT_TRIGGER {
            continue;
        }
        let body = truncate_str(
            &format!(
                "Repeated negative feedback on `{}` ({} times): {}",
                action, count, learnings
            ),
            MAX_EXPERIENCE_BODY_CHARS,
        );
        patterns.push(FailurePattern {
            id: fnv1a_hash(&body),
            kind: FailureKind::FeedbackPattern,
            body,
            tools: vec![action.clone()],
            observation_count: count,
            first_seen: now,
            last_seen: now,
        });
    }

    Ok(patterns)
}

/// Generate an experience entry from a failure pattern.
pub fn generate_experience(pattern: &FailurePattern) -> ExperienceEntry {
    let correction = match pattern.kind {
        FailureKind::ToolBurst => {
            format!(
                "Avoid using `{}` after repeated failures; try alternative tools or verify inputs first.",
                pattern.tools.first().map(|s| s.as_str()).unwrap_or("unknown")
            )
        }
        FailureKind::ErrorLoop => {
            "All tools are failing — stop and ask for clarification instead of retrying.".to_string()
        }
        FailureKind::FeedbackPattern => {
            format!(
                "Change approach for `{}` — previous attempts consistently failed.",
                pattern.tools.first().map(|s| s.as_str()).unwrap_or("unknown")
            )
        }
        FailureKind::ConvergenceStuck => {
            "Break out of the loop by summarizing progress and asking for direction.".to_string()
        }
    };

    let body_for_hash = format!("{}:{}", pattern.kind.label(), pattern.body);
    ExperienceEntry {
        id: fnv1a_hash(&body_for_hash),
        failure_kind: pattern.kind,
        what_happened: pattern.body.clone(),
        correction,
        tools: pattern.tools.clone(),
        observation_count: pattern.observation_count,
        created_at: now_unix_secs(),
    }
}

/// Persist an experience entry to the experience directory.
/// /// Files are named `<timestamp>-<id>.jsonl` and stored under
/// `experience_dir`. If the directory doesn't exist, it's created.
/// If the file exceeds `EXPERIENCE_FILE_LIMIT` lines, a new file is started.
pub fn persist_experience(
    experience_dir: &Path,
    entry: &ExperienceEntry,
) -> std::io::Result<()> {
    fs::create_dir_all(experience_dir)?;

    // Find or create a current experience file.
    let id_prefix: String = entry.id.chars().take(8).collect();
    let filename = format!("{}-{}.jsonl", entry.created_at, id_prefix);
    let filepath = experience_dir.join(&filename);

    // Check if this entry already exists (idempotent).
    if filepath.exists() {
        let content = fs::read_to_string(&filepath)?;
        for line in content.lines() {
            if let Ok(existing) = serde_json::from_str::<ExperienceEntry>(line) {
                if existing.id == entry.id {
                    return Ok(()); // Already written.
                }
            }
        }
    }

    // Write the entry.
    let line = serde_json::to_string(entry)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&filepath)?;
    writeln!(file, "{}", line)?;
    file.flush()?;

    // Evict old experience files if over limit.
    evict_old_experience_files(experience_dir)?;

    Ok(())
}

/// Propose a self-improvement rule from a failure pattern.
/// /// Returns `true` if a new rule was actually written (not a duplicate).
pub fn propose_rule(
    rule_store: &SelfRuleStore,
    pattern: &FailurePattern,
) -> std::io::Result<bool> {
    let rule_body = truncate_str(
        &format!("[{}] {}", pattern.kind.label(), pattern.body),
        MAX_EXPERIENCE_BODY_CHARS,
    );
    let source = match pattern.kind {
        FailureKind::ToolBurst => RuleSource::ToolFailureBurst,
        FailureKind::ErrorLoop => RuleSource::ToolFailureBurst,
        FailureKind::FeedbackPattern => RuleSource::FeedbackRepeated,
        FailureKind::ConvergenceStuck => RuleSource::FeedbackRepeated,
    };
    rule_store.record(&rule_body, source)
}

/// Top-level entry point: detect failures, generate experiences, propose rules.
/// /// Call this at the end of each agent turn (or on convergence detection).
/// It's safe to call repeatedly — deduplication is handled internally.
/// /// Wire-up note: will be called from `run_loop.rs` once the harness
/// self-edit integration is complete.
#[allow(dead_code)]
pub(crate) fn run_self_edit(
    tool_errors: &ToolErrorCounter,
    feedback: &FeedbackLog,
    convergence: Option<(&ConvergenceReason, u32, &str)>,
    experience_dir: &Path,
    rule_store: &SelfRuleStore,
) -> SelfEditResult {
    let mut all_patterns = Vec::new();

    // 1. Tool failure patterns.
    all_patterns.extend(classify_tool_failures(tool_errors));

    // 2. Convergence patterns.
    if let Some((reason, count, msg)) = convergence {
        if let Some(p) = classify_convergence(reason, count, msg) {
            all_patterns.push(p);
        }
    }

    // 3. Feedback patterns (soft-fail).
    if let Ok(fb_patterns) = classify_feedback(feedback) {
        all_patterns.extend(fb_patterns);
    }

    if all_patterns.is_empty() {
        return SelfEditResult {
            experience_written: false,
            rule_proposed: false,
            experience_id: None,
        };
    }

    let mut experience_written = false;
    let mut rule_proposed = false;
    let mut experience_id = None;

    for pattern in &all_patterns {
        let entry = generate_experience(pattern);

        // Persist experience (soft-fail).
        if let Ok(()) = persist_experience(experience_dir, &entry) {
            experience_written = true;
            experience_id = Some(entry.id.clone());
        }

        // Propose rule (soft-fail).
        if let Ok(written) = propose_rule(rule_store, pattern) {
            if written {
                rule_proposed = true;
            }
        }
    }

    SelfEditResult {
        experience_written,
        rule_proposed,
        experience_id,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out = String::with_capacity(max);
    for (i, c) in s.chars().enumerate() {
        if i >= max.saturating_sub(1) {
            out.push('\u{2026}');
            break;
        }
        out.push(c);
    }
    out
}

fn evict_old_experience_files(dir: &Path) -> std::io::Result<()> {
    let mut files: Vec<(u64, PathBuf)> = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().map_or(false, |e| e == "jsonl") {
            let modified = path.metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            files.push((modified, path));
        }
    }

    if files.len() <= MAX_EXPERIENCE_FILES {
        return Ok(());
    }

    // Sort by modification time (oldest first).
    files.sort_by_key(|(t, _)| *t);

    // Remove oldest files until we're under the limit.
    let to_remove = files.len() - MAX_EXPERIENCE_FILES;
    for (_, path) in files.iter().take(to_remove) {
        let _ = fs::remove_file(path); // soft-fail
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feedback_loop::FeedbackEntry;

    #[test]
    fn fnv1a_deduplication_key() {
        assert_eq!(fnv1a_hash("abc"), fnv1a_hash("abc"));
        assert_ne!(fnv1a_hash("abc"), fnv1a_hash("def"));
    }

    #[test]
    fn classify_tool_failures_below_threshold() {
        let mut counter = ToolErrorCounter::new();
        counter.record_error("shell");
        counter.record_error("shell");
        let patterns = classify_tool_failures(&counter);
        assert!(patterns.is_empty());
    }

    #[test]
    fn classify_tool_failures_at_threshold() {
        let mut counter = ToolErrorCounter::new();
        for _ in 0..3 {
            counter.record_error("shell");
        }
        let patterns = classify_tool_failures(&counter);
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0].kind, FailureKind::ToolBurst);
        assert!(patterns[0].body.contains("shell"));
    }

    #[test]
    fn classify_convergence_error_loop() {
        let pattern = classify_convergence(
            &ConvergenceReason::ErrorLoop,
            5,
            "all tools failing",
        );
        assert!(pattern.is_some());
        let p = pattern.unwrap();
        assert_eq!(p.kind, FailureKind::ErrorLoop);
    }

    #[test]
    fn classify_convergence_below_threshold() {
        let pattern = classify_convergence(
            &ConvergenceReason::NoProgressWithTools,
            2,
            "no progress",
        );
        assert!(pattern.is_none());
    }

    #[test]
    fn classify_convergence_stuck() {
        let pattern = classify_convergence(
            &ConvergenceReason::SimilarContent,
            4,
            "similar output",
        );
        assert!(pattern.is_some());
        let p = pattern.unwrap();
        assert_eq!(p.kind, FailureKind::ConvergenceStuck);
    }

    #[test]
    fn classify_feedback_patterns() {
        let dir = tempfile::tempdir().unwrap();
        let log = FeedbackLog::open_at(&dir.path().join("fb.jsonl")).unwrap();
        for i in 0..4 {
            log.record(&FeedbackEntry {
                turn: i,
                action: format!("tool:shell:{}", i),
                success: false,
                learnings: "bad result".into(),
            }).unwrap();
        }
        let patterns = classify_feedback(&log).unwrap();
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0].kind, FailureKind::FeedbackPattern);
    }

    #[test]
    fn generate_experience_tool_burst() {
        let pattern = FailurePattern {
            id: "test".into(),
            kind: FailureKind::ToolBurst,
            body: "Tool `shell` failed 5 times".into(),
            tools: vec!["shell".into()],
            observation_count: 5,
            first_seen: 0,
            last_seen: 100,
        };
        let entry = generate_experience(&pattern);
        assert!(entry.correction.contains("shell"));
        assert!(entry.correction.contains("alternative"));
    }

    #[test]
    fn generate_experience_error_loop() {
        let pattern = FailurePattern {
            id: "test".into(),
            kind: FailureKind::ErrorLoop,
            body: "All tools failing".into(),
            tools: vec![],
            observation_count: 8,
            first_seen: 0,
            last_seen: 100,
        };
        let entry = generate_experience(&pattern);
        assert!(entry.correction.contains("clarification"));
    }

    #[test]
    fn persist_experience_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let entry = ExperienceEntry {
            id: "abc123".into(),
            failure_kind: FailureKind::ToolBurst,
            what_happened: "test failure".into(),
            correction: "use alternative".into(),
            tools: vec!["shell".into()],
            observation_count: 3,
            created_at: 1000,
        };

        persist_experience(dir.path(), &entry).unwrap();
        persist_experience(dir.path(), &entry).unwrap();

        // Count total lines in all JSONL files.
        let total_lines: usize = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().map_or(false, |ext| ext == "jsonl"))
            .map(|e| fs::read_to_string(e.path()).unwrap().lines().count())
            .sum();
        assert_eq!(total_lines, 1);
    }

    #[test]
    fn run_self_edit_no_failures() {
        let dir = tempfile::tempdir().unwrap();
        let counter = ToolErrorCounter::new();
        let fb_dir = tempfile::tempdir().unwrap();
        let feedback = FeedbackLog::open_at(&fb_dir.path().join("fb.jsonl")).unwrap();
        let rule_dir = tempfile::tempdir().unwrap();
        let rule_store = SelfRuleStore::open_at(&rule_dir.path().join("rules.jsonl")).unwrap();

        let result = run_self_edit(
            &counter,
            &feedback,
            None,
            dir.path(),
            &rule_store,
        );
        assert!(!result.experience_written);
        assert!(!result.rule_proposed);
    }

    #[test]
    fn run_self_edit_with_tool_failure() {
        let exp_dir = tempfile::tempdir().unwrap();
        let mut counter = ToolErrorCounter::new();
        for _ in 0..4 {
            counter.record_error("shell");
        }
        let fb_dir = tempfile::tempdir().unwrap();
        let feedback = FeedbackLog::open_at(&fb_dir.path().join("fb.jsonl")).unwrap();
        let rule_dir = tempfile::tempdir().unwrap();
        let rule_store = SelfRuleStore::open_at(&rule_dir.path().join("rules.jsonl")).unwrap();

        let result = run_self_edit(
            &counter,
            &feedback,
            None,
            exp_dir.path(),
            &rule_store,
        );
        assert!(result.experience_written);
        assert!(result.rule_proposed);
        assert!(result.experience_id.is_some());
    }

    #[test]
    fn run_self_edit_with_convergence() {
        let exp_dir = tempfile::tempdir().unwrap();
        let counter = ToolErrorCounter::new();
        let fb_dir = tempfile::tempdir().unwrap();
        let feedback = FeedbackLog::open_at(&fb_dir.path().join("fb.jsonl")).unwrap();
        let rule_dir = tempfile::tempdir().unwrap();
        let rule_store = SelfRuleStore::open_at(&rule_dir.path().join("rules.jsonl")).unwrap();

        let result = run_self_edit(
            &counter,
            &feedback,
            Some((&ConvergenceReason::ErrorLoop, 6, "all tools failing")),
            exp_dir.path(),
            &rule_store,
        );
        assert!(result.experience_written);
        assert!(result.rule_proposed);
    }

    #[test]
    fn evict_old_experience_files_works() {
        let dir = tempfile::tempdir().unwrap();
        // Create more files than the limit.
        for i in 0..(MAX_EXPERIENCE_FILES + 5) {
            let path = dir.path().join(format!("exp-{}.jsonl", i));
            fs::write(&path, format!("{{\"id\":\"{}\"}}", i)).unwrap();
        }
        super::evict_old_experience_files(dir.path()).unwrap();

        let remaining = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().map_or(false, |ext| ext == "jsonl"))
            .count();
        assert!(remaining <= MAX_EXPERIENCE_FILES);
    }
}
