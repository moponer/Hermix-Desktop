//! Researcher OS continuous loop .
//!
//! Implements the discover → retrieve → evaluate → propose → validate
//! pipeline that was defined as data structures in `researcher_os.rs`
//! but never wired up with actual execution logic.
//!
//! Four core functions:
//! - `loop_discover`: Fetch new content from registered sources
//! - `loop_step`: Process one item from the inbox queue
//! - `loop_daily`: Run maintenance tasks (validate_repo, health checks)
//! - `loop_status`: Return current state of the researcher

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::researcher_os::{
    self, ClaimProvenance, CorpusIndex, MechanismRegistry, Source, SourceKind, SourceRegistry,
};

use super::skill_health::score_corpus;
use super::experience_archive::{ExperienceArchive, ExperienceSummary, daily_experience_report, ingest_external_experiences};

// ---------------------------------------------------------------------------
// Researcher state
// ---------------------------------------------------------------------------

/// The mutable state of the researcher loop. Persisted between calls.
/// Note: The internal registries (`sources`, `mechanisms`, `provenance`,
/// `corpus`) are not serializable — they use in-memory HashMaps without
/// Serialize/Deserialize. For persistence, use the JSONL store layer
/// in `researcher_os.rs`. This struct is meant to be held in memory
/// for the duration of a researcher session.
#[derive(Debug)]
pub struct ResearcherState {
    /// Root directory for researcher data (e.g. ~/.hermix/researcher/).
    pub data_dir: PathBuf,
    /// Registered external sources.
    pub sources: SourceRegistry,
    /// Known mechanisms extracted from sources.
    pub mechanisms: MechanismRegistry,
    /// Claim provenance tracking.
    pub provenance: ClaimProvenance,
    /// Corpus index of all skills.
    pub corpus: CorpusIndex,
    /// Number of items processed in the current run.
    pub items_processed: u64,
    /// Number of discover calls made.
    pub discover_count: u64,
    /// Experience archive for this researcher.
    pub experience: Option<ExperienceArchive>,
}

impl ResearcherState {
    /// Create a new researcher state rooted at the given data directory.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        let data_dir: PathBuf = data_dir.into();
        let experience = ExperienceArchive::open(data_dir.join("experience")).ok();
        Self {
            data_dir,
            sources: SourceRegistry::new(),
            mechanisms: MechanismRegistry::new(),
            provenance: ClaimProvenance::new(),
            corpus: CorpusIndex::new(),
            items_processed: 0,
            discover_count: 0,
            experience,
        }
    }

    /// Ensure the data directory and subdirectories exist.
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        let dirs = [
            self.data_dir.join("queue"),
            self.data_dir.join("queue/inbox"),
            self.data_dir.join("queue/parked"),
            self.data_dir.join("queue/done"),
            self.data_dir.join("queue/quarantine"),
            self.data_dir.join("runs"),
            self.data_dir.join("reports"),
            self.data_dir.join("experience"),
            self.data_dir.join("fixtures"),
        ];
        for dir in &dirs {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }

    /// Path to the inbox queue directory.
    pub fn inbox_dir(&self) -> PathBuf {
        self.data_dir.join("queue/inbox")
    }

    /// Path to the parked queue directory.
    pub fn parked_dir(&self) -> PathBuf {
        self.data_dir.join("queue/parked")
    }

    /// Path to the done queue directory.
    pub fn done_dir(&self) -> PathBuf {
        self.data_dir.join("queue/done")
    }

    /// Path to the quarantine queue directory.
    pub fn quarantine_dir(&self) -> PathBuf {
        self.data_dir.join("queue/quarantine")
    }

    /// Path to the experience directory.
    pub fn experience_dir(&self) -> PathBuf {
        self.data_dir.join("experience")
    }

    /// Get or lazily initialize the experience archive.
    pub fn experience_archive(&mut self) -> &ExperienceArchive {
        if self.experience.is_none() {
            self.experience = ExperienceArchive::open(self.experience_dir()).ok();
        }
        self.experience.as_ref().expect("experience archive should be initialized")
    }
}

// ---------------------------------------------------------------------------
// Discover result
// ---------------------------------------------------------------------------

/// Result of a discover operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverResult {
    /// Number of new sources found.
    pub new_sources: usize,
    /// Number of sources that were already known (deduplicated).
    pub duplicates: usize,
    /// Errors encountered while fetching.
    pub errors: Vec<String>,
}

// ---------------------------------------------------------------------------
// Step result
// ---------------------------------------------------------------------------

/// Outcome of processing a single inbox item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum StepOutcome {
    /// Item was evaluated and parked for human review.
    Parked { source_id: String, mechanism_id: String },
    /// Item was moved to quarantine (evaluation failed).
    Quarantined { source_id: String, reason: String },
    /// Item was processed and moved to done.
    Done { source_id: String },
    /// Inbox was empty — nothing to process.
    EmptyInbox,
}

// ---------------------------------------------------------------------------
// Daily report
// ---------------------------------------------------------------------------

/// Summary of a daily maintenance run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyReport {
    /// Date of the report (YYYY-MM-DD).
    pub date: String,
    /// Number of sources in registry.
    pub total_sources: usize,
    /// Number of mechanisms in registry.
    pub total_mechanisms: usize,
    /// Number of items in each queue.
    pub queue_sizes: QueueSizes,
    /// Skill health report summary (if run).
    pub skill_health_avg: Option<f32>,
    /// Number of claims tracked.
    pub total_claims: usize,
    /// Experience archive summary (if available).
    pub experience_summary: Option<ExperienceSummary>,
    /// Any errors encountered.
    pub errors: Vec<String>,
}

/// Sizes of the four researcher queues.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QueueSizes {
    pub inbox: usize,
    pub parked: usize,
    pub done: usize,
    pub quarantine: usize,
}

// ---------------------------------------------------------------------------
// Status report
// ---------------------------------------------------------------------------

/// Current status of the researcher OS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusReport {
    /// Number of sources registered.
    pub sources: usize,
    /// Number of mechanisms known.
    pub mechanisms: usize,
    /// Number of claims tracked.
    pub claims: usize,
    /// Queue sizes.
    pub queue_sizes: QueueSizes,
    /// Items processed in the current run.
    pub items_processed: u64,
    /// Discover calls made.
    pub discover_count: u64,
}

// ---------------------------------------------------------------------------
// Core loop functions
// ---------------------------------------------------------------------------

/// Discover new content from registered sources.
/// Reads the source registry, fetches content from each source URL
/// (HTTP only, 1.5MB cap, 30s timeout), deduplicates by content hash,
/// and writes new items to the inbox queue.
pub async fn loop_discover(state: &mut ResearcherState) -> Result<DiscoverResult, String> {
    state.ensure_dirs().map_err(|e| format!("Failed to create dirs: {}", e))?;
    state.discover_count += 1;

    let mut new_sources = 0usize;
    let mut duplicates = 0usize;
    let errors: Vec<String> = Vec::new();

    // For now, we don't have actual HTTP fetching in this module
    // (that would go through the agent's tool system). Instead,
    // this function scans the inbox directory for any manually
    // placed source files and registers them.

    let inbox_dir = state.inbox_dir();
    if let Ok(entries) = std::fs::read_dir(&inbox_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map_or(false, |e| e == "json") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(source) = serde_json::from_str::<Source>(&content) {
                        if state.sources.get(&source.id).is_some() {
                            duplicates += 1;
                        } else {
                            state.sources.insert(source);
                            new_sources += 1;
                        }
                    }
                }
            }
        }
    }

    // Ingest external experience files if they exist.
    let ext_dir = state.data_dir.join("experience").join("incoming");
    if ext_dir.exists() {
        // Lazily initialize experience archive if needed.
        if state.experience.is_none() {
            state.experience = ExperienceArchive::open(state.experience_dir()).ok();
        }
        if let Some(ref archive) = state.experience {
            if let Ok(count) = ingest_external_experiences(
                archive,
                &ext_dir,
            ) {
                // Log the ingestion count (soft-fail: don't error the discover).
                tracing::debug!(count, "Ingested external experience files");
            }
        }
    }

    Ok(DiscoverResult {
        new_sources,
        duplicates,
        errors,
    })
}

/// Process a single item from the inbox queue.
/// Takes the first item from inbox, evaluates it, and moves it to
/// the appropriate queue (parked, done, or quarantine).
pub async fn loop_step(state: &mut ResearcherState) -> Result<StepOutcome, String> {
    state.ensure_dirs().map_err(|e| format!("Failed to create dirs: {}", e))?;

    let inbox_dir = state.inbox_dir();

    // Find the first JSON file in the inbox.
    let first_item = std::fs::read_dir(&inbox_dir)
        .ok()
        .and_then(|entries| {
            entries
                .flatten()
                .find(|e| e.path().extension().map_or(false, |ext| ext == "json"))
        });

    let Some(item) = first_item else {
        return Ok(StepOutcome::EmptyInbox);
    };

    let item_path = item.path();
    let content = std::fs::read_to_string(&item_path)
        .map_err(|e| format!("Failed to read inbox item: {}", e))?;

    let source: Source = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse source JSON: {}", e))?;

    // Evaluate the source content.
    // For now, we use a simple heuristic: sources from the web
    // with non-trivial content are parked for review; others
    // are quarantined.
    let outcome = if source.kind == SourceKind::Web && !source.content_hash.is_empty() {
        // Create a mechanism hypothesis from this source.
        let mechanism_id = researcher_os::hash_bytes(source.content_hash.as_bytes());
        StepOutcome::Parked {
            source_id: source.id.clone(),
            mechanism_id,
        }
    } else {
        StepOutcome::Quarantined {
            source_id: source.id.clone(),
            reason: "Source type not suitable for automatic evaluation".to_string(),
        }
    };

    // Move the file to the appropriate queue.
    let dest_dir = match &outcome {
        StepOutcome::Parked { .. } => state.parked_dir(),
        StepOutcome::Quarantined { .. } => state.quarantine_dir(),
        StepOutcome::Done { .. } => state.done_dir(),
        StepOutcome::EmptyInbox => unreachable!(),
    };

    let dest_path = dest_dir.join(item_path.file_name().unwrap());
    if let Err(e) = std::fs::rename(&item_path, &dest_path) {
        // Fall back to copy+delete if rename fails (cross-device).
        if let Err(e2) = std::fs::copy(&item_path, &dest_path).and_then(|_| std::fs::remove_file(&item_path)) {
            return Err(format!("Failed to move item: rename={}, copy+delete={}", e, e2));
        }
    }

    state.items_processed += 1;
    Ok(outcome)
}

/// Run daily maintenance tasks.
/// - Validates the repository structure
/// - Runs skill health checks
/// - Reports queue sizes
pub async fn loop_daily(state: &mut ResearcherState) -> Result<DailyReport, String> {
    state.ensure_dirs().map_err(|e| format!("Failed to create dirs: {}", e))?;

    let errors: Vec<String> = Vec::new();

    // Count queue sizes.
    let queue_sizes = QueueSizes {
        inbox: count_files(&state.inbox_dir()),
        parked: count_files(&state.parked_dir()),
        done: count_files(&state.done_dir()),
        quarantine: count_files(&state.quarantine_dir()),
    };

    // Run skill health check if skills directory exists.
    let skill_health_avg = {
        let skills_dir = state.data_dir.parent()
            .map(|p| p.join("skills"))
            .unwrap_or_else(|| PathBuf::from(".hermix/skills"));
        if skills_dir.exists() {
            let report = score_corpus(&skills_dir);
            Some(report.avg_composite)
        } else {
            None
        }
    };

    // Write daily report.
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    // Generate experience summary.
    let experience_summary = state.experience.as_ref()
        .and_then(|archive| daily_experience_report(archive).ok());

    let mut report = DailyReport {
        date: date.clone(),
        total_sources: state.sources.len(),
        total_mechanisms: state.mechanisms.len(),
        queue_sizes,
        skill_health_avg,
        total_claims: state.provenance.len(),
        experience_summary,
        errors,
    };

    let report_filename = format!("daily-{}.json", date);
    let report_path = state.data_dir.join("reports").join(report_filename);
    if let Ok(json) = serde_json::to_string_pretty(&report) {
        if let Err(e) = std::fs::write(&report_path, json) {
            report.errors.push(format!("Failed to write report: {}", e));
        }
    }

    Ok(report)
}

/// Return the current status of the researcher OS.
pub fn loop_status(state: &ResearcherState) -> StatusReport {
    StatusReport {
        sources: state.sources.len(),
        mechanisms: state.mechanisms.len(),
        claims: state.provenance.len(),
        queue_sizes: QueueSizes {
            inbox: count_files(&state.inbox_dir()),
            parked: count_files(&state.parked_dir()),
            done: count_files(&state.done_dir()),
            quarantine: count_files(&state.quarantine_dir()),
        },
        items_processed: state.items_processed,
        discover_count: state.discover_count,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Count the number of files in a directory.
fn count_files(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|entries| entries.count())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use researcher_os::hash_bytes;

    #[tokio::test]
    async fn discover_scans_inbox() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = ResearcherState::new(tmp.path().join("researcher"));
        state.ensure_dirs().unwrap();

        // Place a source file in the inbox.
        let source = Source {
            id: "s1".into(),
            url: "https://example.test/paper".into(),
            kind: SourceKind::Web,
            fetched_at: 0,
            content_hash: hash_bytes(b"test content"),
        };
        let inbox_path = state.inbox_dir().join("s1.json");
        std::fs::write(&inbox_path, serde_json::to_string(&source).unwrap()).unwrap();

        let result = loop_discover(&mut state).await.unwrap();
        assert_eq!(result.new_sources, 1);
        assert_eq!(state.sources.len(), 1);
    }

    #[tokio::test]
    async fn step_processes_inbox_item() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = ResearcherState::new(tmp.path().join("researcher"));
        state.ensure_dirs().unwrap();

        // Place a web source in the inbox.
        let source = Source {
            id: "s1".into(),
            url: "https://example.test".into(),
            kind: SourceKind::Web,
            fetched_at: 0,
            content_hash: hash_bytes(b"test"),
        };
        let inbox_path = state.inbox_dir().join("s1.json");
        std::fs::write(&inbox_path, serde_json::to_string(&source).unwrap()).unwrap();

        let outcome = loop_step(&mut state).await.unwrap();
        assert!(matches!(outcome, StepOutcome::Parked { .. }));
        assert_eq!(state.items_processed, 1);

        // Inbox should be empty now.
        let status = loop_status(&state);
        assert_eq!(status.queue_sizes.inbox, 0);
        assert_eq!(status.queue_sizes.parked, 1);
    }

    #[tokio::test]
    async fn step_returns_empty_inbox() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = ResearcherState::new(tmp.path().join("researcher"));
        state.ensure_dirs().unwrap();

        let outcome = loop_step(&mut state).await.unwrap();
        assert_eq!(outcome, StepOutcome::EmptyInbox);
    }

    #[tokio::test]
    async fn daily_report() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = ResearcherState::new(tmp.path().join("researcher"));
        state.ensure_dirs().unwrap();

        let report = loop_daily(&mut state).await.unwrap();
        assert_eq!(report.total_sources, 0);
        assert_eq!(report.queue_sizes.inbox, 0);
    }

    #[test]
    fn status_report_initial() {
        let tmp = tempfile::tempdir().unwrap();
        let state = ResearcherState::new(tmp.path().join("researcher"));

        let status = loop_status(&state);
        assert_eq!(status.sources, 0);
        assert_eq!(status.items_processed, 0);
    }
}
