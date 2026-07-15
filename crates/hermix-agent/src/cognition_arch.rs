// P7 - Cognitive architecture upgrades.
//
// Three cooperating modules:
//   P7-T1 BdiModel          - beliefs / desires / intentions state
//   P7-T2 LatentBriefing    - delegate_task KV-cache friendly
//   P7-T3 FilesystemContext - multi-file memory addressed by path
//
// All are pure data + small policy helpers. They live here so the
// agent loop can hold the *shape* of cognition without depending on
// any runtime I/O.

use std::collections::{BTreeMap, HashMap};

// ===========================================================================
// P7-T1 - BDI (Beliefs / Desires / Intentions)
// ===========================================================================

/// World facts the agent currently holds as true. Each belief has
/// a `confidence` in 0..=1 so it can be downweighted or contradicted
/// without losing the source.
#[derive(Debug, Clone, PartialEq)]
pub struct Belief {
    pub statement: String,
    pub confidence: f32,
    pub source: BeliefSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeliefSource {
    User,
    SelfObservation,
    ToolReturn,
    Inferred,
    OperatorNote,
}

/// Goals the agent is *trying* to reach. Each desire carries an
/// optional rationale (the source belief that triggered the goal).
#[derive(Debug, Clone, PartialEq)]
pub struct Desire {
    pub statement: String,
    pub priority: u8,
    pub rationale: Option<String>,
}

/// Concrete next-action commitments. An intention is the smallest
/// unit of "I am doing X now". Once committed, the agent owes the
/// user an outcome, not just a thought.
#[derive(Debug, Clone, PartialEq)]
pub struct Intention {
    pub id: String,
    pub action: String,
    pub satisfies: Vec<String>,
    pub committed_at: u64,
}

#[derive(Default, Debug)]
pub struct BdiModel {
    pub beliefs: Vec<Belief>,
    pub desires: Vec<Desire>,
    pub intentions: Vec<Intention>,
}

impl BdiModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add (or replace) a belief with the same statement.
    /// Bumps confidence on re-add; only keeps the max.
    pub fn add_belief(&mut self, belief: Belief) {
        match self.beliefs.iter_mut().find(|b| b.statement == belief.statement) {
            Some(existing) => {
                if belief.confidence > existing.confidence {
                    *existing = belief;
                }
            }
            None => self.beliefs.push(belief),
        }
    }

    pub fn drop_belief(&mut self, statement: &str) -> bool {
        if let Some(pos) = self.beliefs.iter().position(|b| b.statement == statement) {
            self.beliefs.remove(pos);
            true
        } else {
            false
        }
    }

    pub fn add_desire(&mut self, desire: Desire) {
        self.desires.push(desire);
        // Higher-priority items first.
        self.desires.sort_by(|a, b| b.priority.cmp(&a.priority));
    }

    pub fn commit_intention(&mut self, intention: Intention) {
        self.intentions.retain(|i| i.id != intention.id);
        self.intentions.push(intention);
    }

    /// Pull the desire that the intention `satisfies` so the caller
    /// can mark it accomplished.
    pub fn intention_satisfies(&self, intention_id: &str) -> Vec<String> {
        self.intentions
            .iter()
            .find(|i| i.id == intention_id)
            .map(|i| i.satisfies.clone())
            .unwrap_or_default()
    }

    /// Drop everything committed - called at session end.
    pub fn clear_intentions(&mut self) {
        self.intentions.clear();
    }
}

// ===========================================================================
// P7-T2 - LatentBriefing
// ===========================================================================

/// A scratchpad that travels WITH a delegated turn. KV-cache
/// friendly in the sense that the briefing is pre-marshalled
/// markdown at the boundary, so the inner model can keep its
/// KV-cache warm across simple prompt boundaries.
#[derive(Debug, Clone, PartialEq)]
pub struct LatentBriefing {
    pub task: String,
    pub context_keys: Vec<String>,
    pub constraints: Vec<String>,
    pub deliverables: Vec<String>,
    pub scratchpad: String,
}

impl LatentBriefing {
    pub fn new(task: impl Into<String>) -> Self {
        Self {
            task: task.into(),
            context_keys: Vec::new(),
            constraints: Vec::new(),
            deliverables: Vec::new(),
            scratchpad: String::new(),
        }
    }

    pub fn with_key(mut self, k: impl Into<String>) -> Self {
        self.context_keys.push(k.into());
        self
    }

    pub fn with_constraint(mut self, c: impl Into<String>) -> Self {
        self.constraints.push(c.into());
        self
    }

    pub fn with_deliverable(mut self, d: impl Into<String>) -> Self {
        self.deliverables.push(d.into());
        self
    }

    /// Render as a stable markdown block. Same input -> same output
    /// bytes, which is what lets the parent cache the briefing.
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(256 + self.task.len() + self.scratchpad.len());
        out.push_str("[Briefing]\n\n");
        out.push_str("Task: ");
        out.push_str(&self.task);
        out.push('\n');
        if !self.context_keys.is_empty() {
            out.push_str("\nContext keys:\n");
            for k in &self.context_keys {
                out.push_str("- ");
                out.push_str(k);
                out.push('\n');
            }
        }
        if !self.constraints.is_empty() {
            out.push_str("\nConstraints:\n");
            for c in &self.constraints {
                out.push_str("- ");
                out.push_str(c);
                out.push('\n');
            }
        }
        if !self.deliverables.is_empty() {
            out.push_str("\nDeliverables:\n");
            for d in &self.deliverables {
                out.push_str("- ");
                out.push_str(d);
                out.push('\n');
            }
        }
        if !self.scratchpad.is_empty() {
            out.push_str("\nScratchpad:\n");
            out.push_str(&self.scratchpad);
            out.push('\n');
        }
        out
    }

    /// Stable hash so the caller can cache `render()` outputs.
    pub fn digest(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h: u64 = 0xcbf29ce484222325;
        let mut mix = |s: &str, h_ref: &mut u64| {
            for b in s.bytes() {
                *h_ref ^= b as u64;
                *h_ref = h_ref.wrapping_mul(0x100000001b3);
            }
            // field separator
            *h_ref ^= 0xff;
            *h_ref = h_ref.wrapping_mul(0x100000001b3);
        };
        mix(&self.task, &mut h);
        for k in &self.context_keys {
            mix(k, &mut h);
        }
        for c in &self.constraints {
            mix(c, &mut h);
        }
        for d in &self.deliverables {
            mix(d, &mut h);
        }
        mix(&self.scratchpad, &mut h);
        h
    }
}

// ===========================================================================
// P7-T3 - FilesystemContext
// ===========================================================================

/// Addressable multi-file memory. Paths are POSIX-style for
/// portability; the agent loop is free to map them to any backing
/// store. Each entry is keyed by `path` so updates replace, not
/// append.
#[derive(Debug, Clone, PartialEq)]
pub struct FilesystemContext {
    files: BTreeMap<String, String>,
}

impl FilesystemContext {
    pub fn new() -> Self {
        Self {
            files: BTreeMap::new(),
        }
    }

    pub fn write(&mut self, path: impl Into<String>, body: impl Into<String>) {
        self.files.insert(path.into(), body.into());
    }

    pub fn read(&self, path: &str) -> Option<&str> {
        self.files.get(path).map(|s| s.as_str())
    }

    pub fn delete(&mut self, path: &str) -> bool {
        self.files.remove(path).is_some()
    }

    pub fn paths(&self) -> Vec<String> {
        self.files.keys().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Render a markdown view of the context, capped by `char_budget`
    /// characters. Paths are sorted, so the output is stable.
    pub fn render_markdown(&self, char_budget: usize) -> String {
        if self.files.is_empty() || char_budget == 0 {
            return String::new();
        }
        let mut out = String::new();
        out.push_str("[Filesystem Context]\n\n");
        for (path, body) in &self.files {
            let block = format!("### {path}\n\n{body}\n\n");
            if out.len() + block.len() > char_budget {
                // Calculate how much room we have after writing the
                // header. We must leave space for the '### path\n\n'
                // prefix + a '...\n\n(truncated)\n\n' suffix so the
                // entry reads cleanly even when cut.
                let prefix = format!("### {path}\n\n");
                let suffix = "\n\n(truncated - file too large)\n\n";
                let overhead = prefix.len() + suffix.len();
                let remaining = char_budget.saturating_sub(out.len() + overhead);
                if remaining > 8 {
                    let truncated = truncate_chars(body, remaining);
                    out.push_str(&prefix);
                    out.push_str(&truncated);
                    out.push_str(suffix);
                }
                break;
            }
            out.push_str(&block);
        }
        out
    }

    /// Quick word-counted summary for telemetry.
    pub fn word_count_by_path(&self) -> HashMap<String, usize> {
        self.files
            .iter()
            .map(|(p, b)| (p.clone(), b.split_whitespace().count()))
            .collect()
    }
}

impl Default for FilesystemContext {
    fn default() -> Self {
        Self::new()
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
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

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // P7-T1
    #[test]
    fn bdi_add_belief_dedupes_by_statement() {
        let mut m = BdiModel::new();
        m.add_belief(Belief { statement: "sky is blue".into(), confidence: 0.4, source: BeliefSource::User });
        m.add_belief(Belief { statement: "sky is blue".into(), confidence: 0.9, source: BeliefSource::ToolReturn });
        assert_eq!(m.beliefs.len(), 1);
        assert!((m.beliefs[0].confidence - 0.9).abs() < 1e-6);
    }

    #[test]
    fn bdi_lower_confidence_does_not_replace() {
        let mut m = BdiModel::new();
        m.add_belief(Belief { statement: "x".into(), confidence: 0.8, source: BeliefSource::User });
        m.add_belief(Belief { statement: "x".into(), confidence: 0.3, source: BeliefSource::Inferred });
        assert_eq!(m.beliefs.len(), 1);
        assert!((m.beliefs[0].confidence - 0.8).abs() < 1e-6);
    }

    #[test]
    fn bdi_drop_belief_removes() {
        let mut m = BdiModel::new();
        m.add_belief(Belief { statement: "y".into(), confidence: 0.5, source: BeliefSource::User });
        assert!(m.drop_belief("y"));
        assert!(!m.drop_belief("z"));
        assert!(m.beliefs.is_empty());
    }

    #[test]
    fn bdi_desires_sort_by_priority_desc() {
        let mut m = BdiModel::new();
        m.add_desire(Desire { statement: "low".into(), priority: 1, rationale: None });
        m.add_desire(Desire { statement: "high".into(), priority: 5, rationale: None });
        m.add_desire(Desire { statement: "mid".into(), priority: 3, rationale: None });
        assert_eq!(m.desires[0].statement, "high");
        assert_eq!(m.desires[2].statement, "low");
    }

    #[test]
    fn bdi_intention_replaces_same_id() {
        let mut m = BdiModel::new();
        m.commit_intention(Intention { id: "i".into(), action: "a".into(), satisfies: vec![], committed_at: 0 });
        m.commit_intention(Intention { id: "i".into(), action: "b".into(), satisfies: vec![], committed_at: 0 });
        assert_eq!(m.intentions.len(), 1);
        assert_eq!(m.intentions[0].action, "b");
    }

    #[test]
    fn bdi_intention_satisfies_returns_targets() {
        let mut m = BdiModel::new();
        m.commit_intention(Intention {
            id: "i".into(),
            action: "act".into(),
            satisfies: vec!["d1".into(), "d2".into()],
            committed_at: 0,
        });
        assert_eq!(m.intention_satisfies("i"), vec!["d1", "d2"]);
        assert!(m.intention_satisfies("missing").is_empty());
    }

    // P7-T2
    #[test]
    fn briefing_render_has_all_sections() {
        let b = LatentBriefing::new("draft RFC")
            .with_key("memories/2026-07-15")
            .with_constraint("soft fail only")
            .with_deliverable("/tmp/rfc.md");
        let s = b.render();
        assert!(s.starts_with("[Briefing]"));
        assert!(s.contains("Task: draft RFC"));
        assert!(s.contains("memories/2026-07-15"));
        assert!(s.contains("soft fail only"));
        assert!(s.contains("/tmp/rfc.md"));
    }

    #[test]
    fn briefing_digest_changes_with_input() {
        let a = LatentBriefing::new("alpha").digest();
        let b = LatentBriefing::new("beta").digest();
        assert_ne!(a, b);
        // Same -> same.
        let c = LatentBriefing::new("alpha").digest();
        assert_eq!(a, c);
    }

    // P7-T3
    #[test]
    fn fs_context_write_read_delete() {
        let mut c = FilesystemContext::new();
        c.write("/a", "alpha");
        c.write("/b", "bravo");
        assert_eq!(c.read("/a"), Some("alpha"));
        assert!(c.delete("/a"));
        assert_eq!(c.read("/a"), None);
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn fs_context_paths_sorted() {
        let mut c = FilesystemContext::new();
        c.write("/z", "z");
        c.write("/a", "a");
        c.write("/m", "m");
        let p = c.paths();
        assert_eq!(p, vec!["/a", "/m", "/z"]);
    }

    #[test]
    fn fs_context_render_empty_returns_empty() {
        let c = FilesystemContext::new();
        assert!(c.render_markdown(10_000).is_empty());
        let mut c2 = FilesystemContext::new();
        c2.write("/x", "y");
        assert!(c2.render_markdown(0).is_empty());
    }

    #[test]
    fn fs_context_render_budget_truncates() {
        let mut c = FilesystemContext::new();
        let big = "word ".repeat(500); // ~2500 chars
        c.write("/big", &big);
        c.write("/small", "tiny");
        let s = c.render_markdown(1500);
        assert!(s.len() <= 1700); // small slack for the truncation note
        // The big file must be cut, the small one must still appear
        // (BTreeMap order: /big before /small in ASCII).
        assert!(s.contains("/big"));
        assert!(s.contains("truncated"));
    }

    #[test]
    fn fs_context_word_count_by_path() {
        let mut c = FilesystemContext::new();
        c.write("/a", "one two three");
        c.write("/b", "four five");
        let wc = c.word_count_by_path();
        assert_eq!(wc.get("/a"), Some(&3));
        assert_eq!(wc.get("/b"), Some(&2));
    }
}
