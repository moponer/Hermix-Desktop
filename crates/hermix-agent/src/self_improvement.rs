// P4-T6 - self-improvement rules.
//
// When the same failure pattern fires >= SELF_RULE_TRIGGER times
// across sessions, the harness drops a SelfRule into
// ~/.hermix/self_rules.jsonl. Next session, build_self_rules_block
// reads the rules and renders them as a system-prompt block so the
// LLM acts on past lessons without re-deriving them.
//
// Storage:
// ~/.hermix/self_rules.jsonl  (append-only JSONL)
//
// Triggers:
// 1. Tool failure burst: same tool errors >= 3 turns in a row.
// 2. Repeated negative feedback: >= 3 FeedbackEntry rows with
// the same action prefix and success=false.
//
// This module provides store + render primitives. The trigger
// wiring sits in run_loop.rs.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::feedback_loop::FeedbackLog;
use crate::tool_error_demotion::ToolErrorCounter;

pub const SELF_RULE_TRIGGER: u32 = 3;

pub const SELF_RULE_LIMIT: usize = 64;

pub const DEFAULT_SELF_RULE_BLOCK_BUDGET: usize = 1500;

pub const SELF_RULES_FILE: &str = "self_rules.jsonl";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelfRule {
    pub id: String,
    pub body: String,
    pub evidence_count: u32,
    pub source: RuleSource,
    pub created_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleSource {
    ToolFailureBurst,
    FeedbackRepeated,
    OperatorPinned,
}

impl RuleSource {
    fn label(self) -> &'static str {
        match self {
            Self::ToolFailureBurst => "tool failures",
            Self::FeedbackRepeated => "repeated feedback",
            Self::OperatorPinned => "operator pinned",
        }
    }
}

// FNV-1a 64-bit hash, no crypto needed (just a dedup key).
pub fn rule_id_for_body(body: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in body.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", hash)
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn self_rules_path() -> PathBuf {
    hermix_config::paths::hermix_home().join(SELF_RULES_FILE)
}

pub struct SelfRuleStore {
    path: PathBuf,
    inner: Mutex<()>,
}

impl SelfRuleStore {
    pub fn open() -> std::io::Result<Self> {
        let path = self_rules_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(Self { path, inner: Mutex::new(()) })
    }

    pub fn open_at(path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(Self { path: path.to_path_buf(), inner: Mutex::new(()) })
    }

    fn load(&self) -> std::io::Result<Vec<SelfRule>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = File::open(&self.path)?;
        let reader = BufReader::new(file);
        let mut out = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(r) = serde_json::from_str::<SelfRule>(&line) {
                out.push(r);
            }
        }
        Ok(out)
    }

    pub fn record(&self, body: &str, source: RuleSource) -> std::io::Result<bool> {
        let body = body.trim().to_string();
        if body.is_empty() {
            return Ok(false);
        }
        let id = rule_id_for_body(&body);
        let _guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut rules = self.load()?;
        let appended: bool;
        if let Some(existing) = rules.iter_mut().find(|r| r.id == id) {
            existing.evidence_count = existing.evidence_count.saturating_add(1);
            existing.body = body;
            appended = false;
        } else {
            rules.push(SelfRule {
                id: id.clone(),
                body: body.clone(),
                evidence_count: 1,
                source,
                created_at: now_unix_secs(),
            });
            appended = true;
        }
        if rules.len() > SELF_RULE_LIMIT {
            rules.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            rules.truncate(SELF_RULE_LIMIT);
        }
        let tmp = self.path.with_extension("jsonl.tmp");
        let s = render_jsonl(&rules)?;
        fs::write(&tmp, s)?;
        fs::rename(&tmp, &self.path)?;
        Ok(appended)
    }

    pub fn all(&self) -> std::io::Result<Vec<SelfRule>> {
        let _guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.load()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn render_jsonl(rules: &[SelfRule]) -> std::io::Result<String> {
    let mut out = String::new();
    for r in rules {
        let line = serde_json::to_string(r)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        out.push_str(&line);
        out.push('\n');
    }
    Ok(out)
}

pub fn tool_failure_proposals(counter: &ToolErrorCounter) -> Vec<String> {
    let mut out = Vec::new();
    for (name, count) in counter.snapshot() {
        if count >= SELF_RULE_TRIGGER as u32 {
            out.push(format!(
                "Tool `{}` failed {} consecutive times - prefer an alternative before retrying.",
                name, count
            ));
        }
    }
    out.sort();
    out
}

pub fn feedback_proposals(feedback: &FeedbackLog) -> std::io::Result<Vec<String>> {
    let entries = feedback.snapshot().unwrap_or_default();
    let mut by_action: HashMap<String, Vec<String>> = HashMap::new();
    for e in &entries {
        if !e.success {
            let key = e.action.split(':').next().unwrap_or(&e.action).to_string();
            by_action.entry(key).or_default().push(e.learnings.clone());
        }
    }
    let mut out = Vec::new();
    for (action, learnings) in by_action {
        if (learnings.len() as u32) >= SELF_RULE_TRIGGER {
            let last = truncate_chars(&learnings.join(" / "), 160);
            out.push(format!(
                "Repeated negative feedback on `{}` ({} samples) - recent: {}",
                action, learnings.len(), last
            ));
        }
    }
    out.sort();
    Ok(out)
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    let total = s.chars().count();
    if total <= max_chars {
        return s.to_string();
    }
    let mut out = String::with_capacity(max_chars);
    for (i, c) in s.chars().enumerate() {
        if i >= max_chars.saturating_sub(1) {
            out.push('\u{2026}');
            break;
        }
        out.push(c);
    }
    out
}

pub fn build_self_rules_block(
    store: &SelfRuleStore,
    char_budget: usize,
    max_items: usize,
) -> std::io::Result<Option<String>> {
    if max_items == 0 || char_budget == 0 {
        return Ok(None);
    }
    let mut rules = store.all().unwrap_or_default();
    if rules.is_empty() {
        return Ok(None);
    }
    rules.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    rules.truncate(max_items);

    let header = "[Self-Improvement Rules]\n\n";
    let mut out = String::with_capacity(char_budget);
    out.push_str(header);
    for r in rules {
        let line = format!("- ({}; evidence={}): {}\n", r.source.label(), r.evidence_count, r.body);
        if out.len() + line.len() > char_budget {
            break;
        }
        out.push_str(&line);
    }
    if out.len() <= header.len() {
        Ok(None)
    } else {
        Ok(Some(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feedback_loop::FeedbackEntry;
    use crate::tool_error_demotion::ToolErrorCounter;

    #[test]
    fn rule_id_is_stable() {
        assert_eq!(rule_id_for_body("abc"), rule_id_for_body("abc"));
        assert_ne!(rule_id_for_body("abc"), rule_id_for_body("abd"));
    }

    #[test]
    fn store_appends_new_rule() {
        let dir = tempfile::tempdir().unwrap();
        let store = SelfRuleStore::open_at(&dir.path().join("rules.jsonl")).unwrap();
        let new = store
            .record("no double tool retries", RuleSource::ToolFailureBurst)
            .unwrap();
        assert!(new);
        let n = store
            .record("another rule", RuleSource::FeedbackRepeated)
            .unwrap();
        assert!(n);
        assert_eq!(store.all().unwrap().len(), 2);
    }

    #[test]
    fn store_dedupes_by_body() {
        let dir = tempfile::tempdir().unwrap();
        let store = SelfRuleStore::open_at(&dir.path().join("rules.jsonl")).unwrap();
        store
            .record("same body", RuleSource::ToolFailureBurst)
            .unwrap();
        let second = store
            .record("same body", RuleSource::OperatorPinned)
            .unwrap();
        assert!(!second);
        let entries = store.all().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].evidence_count, 2);
    }

    #[test]
    fn store_evicts_when_over_limit() {
        let dir = tempfile::tempdir().unwrap();
        let store = SelfRuleStore::open_at(&dir.path().join("rules.jsonl")).unwrap();
        for i in 0..(SELF_RULE_LIMIT as u32 + 5) {
            store
                .record(&format!("rule-{i}"), RuleSource::ToolFailureBurst)
                .unwrap();
        }
        let entries = store.all().unwrap();
        assert!(entries.len() <= SELF_RULE_LIMIT);
    }

    #[test]
    fn store_rejects_empty_body() {
        let dir = tempfile::tempdir().unwrap();
        let store = SelfRuleStore::open_at(&dir.path().join("rules.jsonl")).unwrap();
        let result = store.record("   ", RuleSource::FeedbackRepeated).unwrap();
        assert!(!result);
        assert_eq!(store.all().unwrap().len(), 0);
    }

    #[test]
    fn tool_failure_proposals_filter_below_threshold() {
        let mut counter = ToolErrorCounter::default();
        counter.record_error("shell");
        counter.record_error("shell");
        let out = tool_failure_proposals(&counter);
        assert!(out.is_empty());
        counter.record_error("shell");
        let out = tool_failure_proposals(&counter);
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("shell"));
    }

    #[test]
    fn feedback_proposals_require_group_of_3() {
        let dir = tempfile::tempdir().unwrap();
        let fb = FeedbackLog::open_at(&dir.path().join("fb.log.jsonl")).unwrap();
        fb.record(&FeedbackEntry {
            turn: 1, action: "tool:shell:1".into(), success: true, learnings: "ok".into(),
        }).unwrap();
        fb.record(&FeedbackEntry {
            turn: 2, action: "tool:shell:2".into(), success: false, learnings: "bad".into(),
        }).unwrap();
        fb.record(&FeedbackEntry {
            turn: 3, action: "tool:shell:3".into(), success: false, learnings: "worse".into(),
        }).unwrap();
        assert!(feedback_proposals(&fb).unwrap().is_empty());
        fb.record(&FeedbackEntry {
            turn: 4, action: "tool:shell:4".into(), success: false, learnings: "sigh".into(),
        }).unwrap();
        let props = feedback_proposals(&fb).unwrap();
        assert_eq!(props.len(), 1);
        assert!(props[0].contains("tool"));
    }

    #[test]
    fn build_block_returns_none_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            SelfRuleStore::open_at(&dir.path().join("rules.jsonl")).unwrap();
        let out = build_self_rules_block(&store, 1500, 5).unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn build_block_returns_none_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            SelfRuleStore::open_at(&dir.path().join("rules.jsonl")).unwrap();
        store.record("x", RuleSource::ToolFailureBurst).unwrap();
        assert!(build_self_rules_block(&store, 1500, 0).unwrap().is_none());
        assert!(build_self_rules_block(&store, 0, 5).unwrap().is_none());
    }

    #[test]
    fn build_block_format_with_header_and_bullets() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            SelfRuleStore::open_at(&dir.path().join("rules.jsonl")).unwrap();
        store.record("rule one", RuleSource::ToolFailureBurst).unwrap();
        store.record("rule two", RuleSource::FeedbackRepeated).unwrap();
        let block = build_self_rules_block(&store, 1500, 5).unwrap().unwrap();
        assert!(block.starts_with("[Self-Improvement Rules]"));
        assert!(block.contains("rule one"));
        assert!(block.contains("rule two"));
    }

    #[test]
    fn build_block_respects_budget() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            SelfRuleStore::open_at(&dir.path().join("rules.jsonl")).unwrap();
        for i in 0..20 {
            store
                .record(&format!("rule-{i}"), RuleSource::ToolFailureBurst)
                .unwrap();
        }
        let block = build_self_rules_block(&store, 100, 20).unwrap().unwrap();
        let lines = block.lines().filter(|l| l.starts_with('-')).count();
        // 100 char budget + ~32 char header leaves room for ~2 bullets.
        assert!(lines <= 4, "got {lines} bullets, expected <= 4");
    }
}
