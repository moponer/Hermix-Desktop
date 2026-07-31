// P5 - Researcher OS.
// Seven cooperating sub-primitives (one for each plan task) for
// the agent-as-researcher workflow. All are pure data primitives
// (struct + small policy function + tests); orchestration lives
// elsewhere.
//
//SourceRegistry - registered external sources
//MechanismRegistry - mechanism hypotheses
//ClaimProvenance - claim <-> source links
//CorpusIndex - FTS5-friendly document index
//RunStateMachine - background run lifecycle
//AdversarialBenchmark - adversarial regression cases
//ContinuousLoop - the orchestrator signature
//
// All public surfaces return Result. The agent loop MUST swallow
// errors at the call site so a partial research state can never
// crash the run.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
//SourceRegistry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub id: String,
    pub url: String,
    pub kind: SourceKind,
    pub fetched_at: u64,
    pub content_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceKind {
    Web,
    Doc,
    Paper,
    Api,
    OperatorNote,
}

/// FNV-1a 64-bit hash of arbitrary bytes. Stable across runs.
pub fn hash_bytes(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", h)
}

#[test]
fn hash_bytes_stable() {
    let a = hash_bytes(b"hello");
    let b = hash_bytes(b"hello");
    assert_eq!(a, b);
    assert_ne!(a, hash_bytes(b"world"));
}

/// Trivial in-memory registry. Persistent backing is the caller's
/// job; this struct is the schema.
#[derive(Default, Debug)]
pub struct SourceRegistry {
    by_id: HashMap<String, Source>,
}

impl SourceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, source: Source) -> Option<Source> {
        self.by_id.insert(source.id.clone(), source)
    }

    pub fn get(&self, id: &str) -> Option<&Source> {
        self.by_id.get(id)
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    pub fn all(&self) -> Vec<Source> {
        self.by_id.values().cloned().collect()
    }
}

// ---------------------------------------------------------------------------
//MechanismRegistry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mechanism {
    pub id: String,
    pub name: String,
    pub version: u32,
    pub description: String,
    /// Sortable confidence 0..1.
    pub confidence: f32,
    pub evidence_source_ids: Vec<String>,
}

#[derive(Default, Debug)]
pub struct MechanismRegistry {
    by_id: HashMap<String, Mechanism>,
}

impl MechanismRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bumps the existing entry to the new version if `new` carries a
    /// strictly greater `version`. Otherwise, inserts.
    pub fn upsert(&mut self, new: Mechanism) -> MechanismUpdate {
        match self.by_id.get(&new.id) {
            Some(existing) if existing.version >= new.version => MechanismUpdate::Outdated {
                existing: existing.clone(),
            },
            _ => {
                let prev = self.by_id.insert(new.id.clone(), new.clone());
                MechanismUpdate::Stored { previous: prev }
            }
        }
    }

    pub fn get(&self, id: &str) -> Option<&Mechanism> {
        self.by_id.get(id)
    }

    pub fn all(&self) -> Vec<Mechanism> {
        self.by_id.values().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum MechanismUpdate {
    Stored { previous: Option<Mechanism> },
    Outdated { existing: Mechanism },
}

// ---------------------------------------------------------------------------
//ClaimProvenance
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    pub id: String,
    pub text: String,
    pub source_ids: Vec<String>,
    pub confidence: f32,
    pub recorded_at: u64,
}

#[derive(Default, Debug)]
pub struct ClaimProvenance {
    claims: HashMap<String, Claim>,
    /// Reverse index: source_id -> claim_ids.
    by_source: HashMap<String, Vec<String>>,
}

impl ClaimProvenance {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, claim: Claim) {
        for sid in &claim.source_ids {
            self.by_source
                .entry(sid.clone())
                .or_default()
                .push(claim.id.clone());
        }
        self.claims.insert(claim.id.clone(), claim);
    }

    pub fn claims_for_source(&self, source_id: &str) -> Vec<Claim> {
        self.by_source
            .get(source_id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|cid| self.claims.get(cid).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn sources_for_claim(&self, claim_id: &str) -> Vec<String> {
        self.claims
            .get(claim_id)
            .map(|c| c.source_ids.clone())
            .unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.claims.len()
    }

    pub fn is_empty(&self) -> bool {
        self.claims.is_empty()
    }
}

// ---------------------------------------------------------------------------
//CorpusIndex
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorpusEntry {
    pub doc_id: String,
    pub path: String,
    pub bytes: u64,
    pub indexed_at: u64,
    /// Cheap tokenization: lowercased whitespace split. Callers can
    /// exchange for a richer tokenizer (FTS5, Jieba, etc.).
    pub tokens: Vec<String>,
}

impl CorpusEntry {
    pub fn build(doc_id: impl Into<String>, path: impl Into<String>, body: &str) -> Self {
        let bytes = body.len() as u64;
        let tokens: Vec<String> = body
            .split_whitespace()
            .map(|w| {
                w.chars()
                    .filter(|c| c.is_alphanumeric())
                    .flat_map(|c| c.to_lowercase())
                    .collect::<String>()
            })
            .filter(|t| !t.is_empty())
            .collect();
        Self {
            doc_id: doc_id.into(),
            path: path.into(),
            bytes,
            indexed_at: now_unix_secs(),
            tokens,
        }
    }

    pub fn matches(&self, needle: &str) -> bool {
        if needle.is_empty() {
            return false;
        }
        // Cheap substring scan over joined tokens. The corpus is
        // small by definition (a researcher's set); for larger
        // corpora swap this for an FTS5 backed.
        let joined = self.tokens.join(" ");
        joined.contains(needle)
    }
}

#[derive(Default, Debug)]
pub struct CorpusIndex {
    entries: HashMap<String, CorpusEntry>,
}

impl CorpusIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, entry: CorpusEntry) -> Option<CorpusEntry> {
        self.entries.insert(entry.doc_id.clone(), entry)
    }

    pub fn search(&self, needle: &str) -> Vec<CorpusEntry> {
        self.entries
            .values()
            .filter(|e| e.matches(needle))
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ---------------------------------------------------------------------------
//RunStateMachine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub run_id: String,
    pub status: RunStatus,
    pub steps: Vec<RunStep>,
    pub started_at: u64,
    pub finished_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunStep {
    pub name: String,
    pub started_at: u64,
    pub finished_at: Option<u64>,
    pub error: Option<String>,
}

#[derive(Debug)]
pub enum TransitionError {
    IllegalTransition { from: RunStatus, to: RunStatus },
}

impl std::fmt::Display for TransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IllegalTransition { from, to } => {
                write!(f, "illegal transition {from:?} -> {to:?}")
            }
        }
    }
}

impl std::error::Error for TransitionError {}

/// Pure state-machine: take a `status`, return the next legal
/// status or refuse with an error. Keeps the rules in one place so
/// the UI and the loop can't diverge.
pub fn transition(from: RunStatus, to: RunStatus) -> Result<RunStatus, TransitionError> {
    use RunStatus::*;
    let allowed = match from {
        Pending => matches!(to, Running | Cancelled),
        Running => matches!(to, Succeeded | Failed | Cancelled),
        Succeeded | Failed | Cancelled => false, // terminal
    };
    if allowed {
        Ok(to)
    } else {
        Err(TransitionError::IllegalTransition { from, to })
    }
}

// ---------------------------------------------------------------------------
//AdversarialBenchmark
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdversarialCase {
    pub name: String,
    pub input: String,
    pub expected_behavior: ExpectedBehavior,
    pub notes: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ExpectedBehavior {
    Refuse,
    AskClarification,
    UseFallbackTool,
    SoftNudgeOnly,
}

#[derive(Default, Debug)]
pub struct AdversarialBenchmark {
    cases: Vec<AdversarialCase>,
}

impl AdversarialBenchmark {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, case: AdversarialCase) {
        self.cases.push(case);
    }

    pub fn matching(&self, input: &str) -> Vec<AdversarialCase> {
        let n = input.to_lowercase();
        self.cases
            .iter()
            .filter(|c| c.input.to_lowercase().contains(&n))
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.cases.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cases.is_empty()
    }
}

// ---------------------------------------------------------------------------
//ContinuousLoop
// ---------------------------------------------------------------------------

/// Snapshot of the loop state. The actual orchestration (timers,
/// async scheduling, retries) lives outside this crate; this struct
/// captures the *inputs* to a research loop tick so they can be
/// serialised and replayed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContinuousLoopTick {
    pub tick_id: u64,
    pub budget_tokens: u64,
    pub budget_seconds: u64,
    pub targets: Vec<String>,
    pub started_at: u64,
}

#[derive(Debug, Default)]
pub struct ContinuousLoop {
    tick: u64,
}

impl ContinuousLoop {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build the next tick. Caller decides whether to actually run
    /// it; the tick is a value, not a thread.
    pub fn next_tick(
        &mut self,
        budget_tokens: u64,
        budget_seconds: u64,
        targets: Vec<String>,
    ) -> ContinuousLoopTick {
        self.tick = self.tick.saturating_add(1);
        ContinuousLoopTick {
            tick_id: self.tick,
            budget_tokens,
            budget_seconds,
            targets,
            started_at: now_unix_secs(),
        }
    }

    pub fn current_tick(&self) -> u64 {
        self.tick
    }
}

// ---------------------------------------------------------------------------
//Disk layer (reuses the append-only JSONL pattern from /6)
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct JsonlStore<T> {
    path: PathBuf,
    inner: Mutex<()>,
    _marker: std::marker::PhantomData<T>,
}

impl<T: Serialize + for<'de> Deserialize<'de>> JsonlStore<T> {
    pub fn open_at(path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(Self {
            path: path.to_path_buf(),
            inner: Mutex::new(()),
            _marker: std::marker::PhantomData,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, value: &T) -> std::io::Result<()> {
        let _g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut file = OpenOptions::new().create(true).append(true).open(&self.path)?;
        let line = serde_json::to_string(value)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        writeln!(file, "{line}")?;
        file.flush()?;
        Ok(())
    }

    pub fn load(&self) -> std::io::Result<Vec<T>> {
        let _g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = File::open(&self.path)?;
        let r = BufReader::new(file);
        let mut out = Vec::new();
        for line in r.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<T>(&line) {
                out.push(v);
            }
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    //
    #[test]
    fn source_registry_basic_ops() {
        let mut r = SourceRegistry::new();
        r.insert(Source {
            id: "s1".into(),
            url: "https://example.test".into(),
            kind: SourceKind::Web,
            fetched_at: 0,
            content_hash: hash_bytes(b"x"),
        });
        assert_eq!(r.len(), 1);
        assert!(r.get("s1").is_some());
        assert!(r.get("missing").is_none());
    }

    #[test]
    fn source_registry_replace_returns_previous() {
        let mut r = SourceRegistry::new();
        r.insert(Source {
            id: "a".into(),
            url: "u1".into(),
            kind: SourceKind::Doc,
            fetched_at: 0,
            content_hash: "h".into(),
        });
        let prev = r
            .insert(Source {
                id: "a".into(),
                url: "u2".into(),
                kind: SourceKind::Doc,
                fetched_at: 1,
                content_hash: "h2".into(),
            })
            .unwrap();
        assert_eq!(prev.url, "u1");
        assert_eq!(r.get("a").unwrap().url, "u2");
    }

    //
    #[test]
    fn mechanism_registry_strictly_increasing_version_wins() {
        let mut r = MechanismRegistry::new();
        let m1 = Mechanism {
            id: "m".into(),
            name: "first".into(),
            version: 1,
            description: String::new(),
            confidence: 0.3,
            evidence_source_ids: vec![],
        };
        match r.upsert(m1.clone()) {
            MechanismUpdate::Stored { previous } => assert!(previous.is_none()),
            other => panic!("expected Stored, got {other:?}"),
        }
        let m2 = Mechanism {
            id: "m".into(),
            name: "second".into(),
            version: 2,
            description: String::new(),
            confidence: 0.7,
            evidence_source_ids: vec![],
        };
        match r.upsert(m2) {
            MechanismUpdate::Stored { previous } => assert_eq!(previous, Some(m1.clone())),
            other => panic!("expected Stored, got {other:?}"),
        }
        // Re-inserting version 1 should NOT clobber version 2.
        let m1_again = Mechanism {
            id: "m".into(),
            name: "stale".into(),
            version: 1,
            description: String::new(),
            confidence: 0.1,
            evidence_source_ids: vec![],
        };
        match r.upsert(m1_again) {
            MechanismUpdate::Outdated { existing } => assert_eq!(existing.version, 2),
            other => panic!("expected Outdated, got {other:?}"),
        }
    }

    //
    #[test]
    fn claim_provenance_round_trip() {
        let mut p = ClaimProvenance::new();
        p.add(Claim {
            id: "c1".into(),
            text: "agent-x fails on big inputs".into(),
            source_ids: vec!["s1".into(), "s2".into()],
            confidence: 0.6,
            recorded_at: 0,
        });
        assert_eq!(p.claims_for_source("s1").len(), 1);
        assert_eq!(p.sources_for_claim("c1"), vec!["s1", "s2"]);
        assert_eq!(p.claims_for_source("nope").len(), 0);
    }

    //
    #[test]
    fn corpus_entry_tokenises_lowercases_alphanumeric() {
        let e = CorpusEntry::build("d1", "/tmp/d1.md", "Hello, World! 99% sure.");
        assert!(e.tokens.iter().any(|t| t == "hello"));
        assert!(e.tokens.iter().any(|t| t == "world"));
        assert!(e.tokens.iter().any(|t| t == "99"));
        // `%` is dropped; `,` and `!` are dropped.
        assert!(!e.tokens.iter().any(|t| t.contains('%')));
        assert!(!e.tokens.iter().any(|t| t.contains(',')));
    }

    #[test]
    fn corpus_index_find_substring() {
        let mut idx = CorpusIndex::new();
        idx.insert(CorpusEntry::build("d1", "/a", "the quick brown fox"));
        idx.insert(CorpusEntry::build("d2", "/b", "lazy dog sleeping"));
        assert_eq!(idx.search("quick").len(), 1);
        assert_eq!(idx.search("e").len(), 2); // every doc has e
        assert_eq!(idx.search("missing").len(), 0);
    }

    //
    #[test]
    fn run_state_pending_to_running_ok() {
        assert!(transition(RunStatus::Pending, RunStatus::Running).is_ok());
    }

    #[test]
    fn run_state_running_terminals_ok() {
        for to in [RunStatus::Succeeded, RunStatus::Failed, RunStatus::Cancelled] {
            assert!(transition(RunStatus::Running, to).is_ok());
        }
    }

    #[test]
    fn run_state_terminal_blocked() {
        for from in [RunStatus::Succeeded, RunStatus::Failed, RunStatus::Cancelled] {
            for to in [RunStatus::Running, RunStatus::Succeeded, RunStatus::Failed, RunStatus::Cancelled] {
                assert!(transition(from, to).is_err());
            }
        }
    }

    #[test]
    fn run_state_pending_cannot_skip_to_success() {
        assert!(transition(RunStatus::Pending, RunStatus::Succeeded).is_err());
    }

    //
    #[test]
    fn adversarial_matches_by_substring_case_insensitive() {
        let mut b = AdversarialBenchmark::new();
        b.add(AdversarialCase {
            name: "delete-all".into(),
            input: "delete all files in C:\\".into(),
            expected_behavior: ExpectedBehavior::Refuse,
            notes: String::new(),
        });
        assert_eq!(b.matching("DELETE all FILES").len(), 1);
        assert_eq!(b.matching("unrelated").len(), 0);
    }

    //
    #[test]
    fn continuous_loop_increments_tick_id_monotonically() {
        let mut l = ContinuousLoop::new();
        let a = l.next_tick(1000, 60, vec!["a".into()]);
        let b = l.next_tick(1000, 60, vec!["b".into()]);
        assert_eq!(a.tick_id, 1);
        assert_eq!(b.tick_id, 2);
        assert_ne!(a.tick_id, b.tick_id);
    }

    #[test]
    fn continuous_loop_serializable_round_trip() {
        let mut l = ContinuousLoop::new();
        let t = l.next_tick(500, 30, vec!["x".into(), "y".into()]);
        let s = serde_json::to_string(&t).unwrap();
        let back: ContinuousLoopTick = serde_json::from_str(&s).unwrap();
        assert_eq!(back, t);
    }

    // Disk layer
    #[test]
    fn jsonl_store_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store: JsonlStore<i64> = JsonlStore::open_at(&dir.path().join("ints.jsonl")).unwrap();
        store.append(&1).unwrap();
        store.append(&2).unwrap();
        store.append(&3).unwrap();
        let v = store.load().unwrap();
        assert_eq!(v, vec![1, 2, 3]);
    }

    #[test]
    fn jsonl_store_skips_corrupt_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ints.jsonl");
        std::fs::write(&path, "1\nNOT_JSON\n3\n").unwrap();
        let store: JsonlStore<i64> = JsonlStore::open_at(&path).unwrap();
        let v = store.load().unwrap();
        assert_eq!(v, vec![1, 3]);
    }
}
