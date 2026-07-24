//! Researcher OS queue infrastructure (Phase 2.2).
//!
//! Four-level JSONL queue with atomic writes.
//!
//! Note: File locking via `fs4` is planned but not yet wired in
//! this module. For now, we use a simple Mutex-based in-memory
//! lock to prevent concurrent access within the same process.
//! Cross-process locking will be added in a follow-up.

use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// Queue item
// ---------------------------------------------------------------------------

/// An item in the researcher queue.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QueueItem {
    /// Unique identifier for this item.
    pub id: String,
    /// Which queue the item is currently in.
    pub status: ItemStatus,
    /// The source ID this item was derived from.
    pub source_id: String,
    /// Optional mechanism ID extracted from the source.
    pub mechanism_id: Option<String>,
    /// Optional skill name this item maps to.
    pub skill_name: Option<String>,
    /// Human-readable summary of what this item represents.
    pub summary: String,
    /// When this item was created (unix timestamp).
    pub created_at: u64,
    /// When this item was last updated (unix timestamp).
    pub updated_at: u64,
    /// Who reviewed this item (if parked and reviewed).
    pub reviewed_by: Option<String>,
}

/// Status of a queue item — determines which queue file it lives in.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    /// Waiting to be processed.
    Inbox,
    /// Processed and waiting for human review.
    Parked,
    /// Processing complete.
    Done,
    /// Quarantined due to evaluation failure.
    Quarantine,
}

impl ItemStatus {
    /// Get the filename for this status.
    pub fn filename(&self) -> &'static str {
        match self {
            ItemStatus::Inbox => "inbox.jsonl",
            ItemStatus::Parked => "parked.jsonl",
            ItemStatus::Done => "done.jsonl",
            ItemStatus::Quarantine => "quarantine.jsonl",
        }
    }
}

// ---------------------------------------------------------------------------
// Queue operations
// ---------------------------------------------------------------------------

/// A researcher queue backed by JSONL files.
/// /// Uses a Mutex for in-process concurrency safety. Cross-process
/// file locking (`fs4`) will be added in a follow-up.
#[derive(Debug)]
pub struct ResearcherQueue {
    /// Root directory containing the queue files.
    dir: PathBuf,
    /// In-memory lock for single-process safety.
    lock: Mutex<()>,
}

impl ResearcherQueue {
    /// Create a new queue backed by the given directory.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            lock: Mutex::new(()),
        }
    }

    /// Ensure the queue directory exists.
    pub fn ensure_dir(&self) -> std::io::Result<()> {
        fs::create_dir_all(&self.dir)
    }

    // -------------------------------------------------------------------
    // Append (write)
    // -------------------------------------------------------------------

    /// Append an item to the appropriate queue file.
    pub fn append(&self, item: &QueueItem) -> Result<(), String> {
        self.ensure_dir().map_err(|e| format!("Failed to create queue dir: {}", e))?;
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());

        let path = self.dir.join(item.status.filename());
        self.append_to_file(&path, item)
    }

    /// Inner append logic — assumes lock is held.
    fn append_to_file(&self, path: &Path, item: &QueueItem) -> Result<(), String> {
        let line = serde_json::to_string(item)
            .map_err(|e| format!("Failed to serialize queue item: {}", e))?;

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("Failed to open queue file: {}", e))?;

        writeln!(file, "{}", line).map_err(|e| format!("Failed to write queue item: {}", e))?;
        Ok(())
    }

    // -------------------------------------------------------------------
    // Read
    // -------------------------------------------------------------------

    /// Read all items from a queue.
    pub fn read_all(&self, status: ItemStatus) -> Result<Vec<QueueItem>, String> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.read_all_inner(status)
    }

    fn read_all_inner(&self, status: ItemStatus) -> Result<Vec<QueueItem>, String> {
        let path = self.dir.join(status.filename());
        if !path.exists() {
            return Ok(Vec::new());
        }

        let file = File::open(&path).map_err(|e| format!("Failed to open queue file: {}", e))?;
        let reader = BufReader::new(file);

        let mut items = Vec::new();
        for line in reader.lines() {
            let line = line.map_err(|e| format!("Failed to read line: {}", e))?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<QueueItem>(&line) {
                Ok(item) => items.push(item),
                Err(e) => {
                    tracing::warn!("Skipping malformed queue item: {}", e);
                }
            }
        }

        Ok(items)
    }

    // -------------------------------------------------------------------
    // Move between queues
    // -------------------------------------------------------------------

    /// Move an item from one queue to another.
    pub fn move_item(&self, item_id: &str, to: ItemStatus) -> Result<QueueItem, String> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());

        // Find the item across all queues.
        let mut found_item = None;
        let mut source_status = None;

        for status in &[ItemStatus::Inbox, ItemStatus::Parked, ItemStatus::Done, ItemStatus::Quarantine] {
            let items = self.read_all_inner(*status)?;
            if let Some(item) = items.iter().find(|i| i.id == item_id) {
                found_item = Some(item.clone());
                source_status = Some(*status);
                break;
            }
        }

        let mut item = found_item.ok_or_else(|| format!("Item {} not found in any queue", item_id))?;
        let from_status = source_status.unwrap();

        // Remove from source queue.
        self.remove_from_queue_inner(item_id, from_status)?;

        // Update status and timestamp.
        item.status = to;
        item.updated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Append to destination queue.
        let dest_path = self.dir.join(to.filename());
        self.append_to_file(&dest_path, &item)?;

        Ok(item)
    }

    /// Remove an item from a specific queue file.
    fn remove_from_queue_inner(&self, item_id: &str, status: ItemStatus) -> Result<(), String> {
        let path = self.dir.join(status.filename());
        if !path.exists() {
            return Ok(());
        }

        let items = self.read_all_inner(status)?;
        let filtered: Vec<&QueueItem> = items.iter().filter(|i| i.id != item_id).collect();

        // Atomic write: write to temp file, then rename.
        let temp_path = path.with_extension("tmp");

        let mut temp = File::create(&temp_path)
            .map_err(|e| format!("Failed to create temp file: {}", e))?;

        for item in &filtered {
            let line = serde_json::to_string(item)
                .map_err(|e| format!("Failed to serialize: {}", e))?;
            writeln!(temp, "{}", line)
                .map_err(|e| format!("Failed to write temp file: {}", e))?;
        }

        drop(temp); // Close before rename.

        fs::rename(&temp_path, &path)
            .map_err(|e| format!("Failed to rename temp file: {}", e))?;

        Ok(())
    }

    // -------------------------------------------------------------------
    // Promote (human review)
    // -------------------------------------------------------------------

    /// Promote a parked item to done after human review.
    pub fn promote(&self, item_id: &str, reviewed_by: &str) -> Result<QueueItem, String> {
        let mut item = self.move_item(item_id, ItemStatus::Done)?;
        item.reviewed_by = Some(reviewed_by.to_string());
        // Update the item in the done queue with the reviewer.
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.remove_from_queue_inner(item_id, ItemStatus::Done)?;
        let dest_path = self.dir.join(ItemStatus::Done.filename());
        self.append_to_file(&dest_path, &item)?;
        Ok(item)
    }

    // -------------------------------------------------------------------
    // Stats
    // -------------------------------------------------------------------

    /// Get the count of items in each queue.
    pub fn counts(&self) -> QueueCounts {
        QueueCounts {
            inbox: self.read_all(ItemStatus::Inbox).map(|v| v.len()).unwrap_or(0),
            parked: self.read_all(ItemStatus::Parked).map(|v| v.len()).unwrap_or(0),
            done: self.read_all(ItemStatus::Done).map(|v| v.len()).unwrap_or(0),
            quarantine: self.read_all(ItemStatus::Quarantine).map(|v| v.len()).unwrap_or(0),
        }
    }
}

/// Counts of items in each queue.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QueueCounts {
    pub inbox: usize,
    pub parked: usize,
    pub done: usize,
    pub quarantine: usize,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_item(id: &str, status: ItemStatus) -> QueueItem {
        QueueItem {
            id: id.to_string(),
            status,
            source_id: format!("source-{}", id),
            mechanism_id: None,
            skill_name: None,
            summary: format!("Test item {}", id),
            created_at: 0,
            updated_at: 0,
            reviewed_by: None,
        }
    }

    #[test]
    fn append_and_read() {
        let tmp = tempfile::tempdir().unwrap();
        let queue = ResearcherQueue::new(tmp.path());
        queue.ensure_dir().unwrap();

        let item = make_item("i1", ItemStatus::Inbox);
        queue.append(&item).unwrap();

        let items = queue.read_all(ItemStatus::Inbox).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "i1");
    }

    #[test]
    fn move_between_queues() {
        let tmp = tempfile::tempdir().unwrap();
        let queue = ResearcherQueue::new(tmp.path());
        queue.ensure_dir().unwrap();

        let item = make_item("i1", ItemStatus::Inbox);
        queue.append(&item).unwrap();

        let moved = queue.move_item("i1", ItemStatus::Parked).unwrap();
        assert_eq!(moved.status, ItemStatus::Parked);

        // Should no longer be in inbox.
        let inbox = queue.read_all(ItemStatus::Inbox).unwrap();
        assert!(inbox.is_empty());

        // Should be in parked.
        let parked = queue.read_all(ItemStatus::Parked).unwrap();
        assert_eq!(parked.len(), 1);
    }

    #[test]
    fn promote_item() {
        let tmp = tempfile::tempdir().unwrap();
        let queue = ResearcherQueue::new(tmp.path());
        queue.ensure_dir().unwrap();

        let item = make_item("i1", ItemStatus::Parked);
        queue.append(&item).unwrap();

        let promoted = queue.promote("i1", "human").unwrap();
        assert_eq!(promoted.status, ItemStatus::Done);
        assert_eq!(promoted.reviewed_by, Some("human".to_string()));
    }

    #[test]
    fn counts() {
        let tmp = tempfile::tempdir().unwrap();
        let queue = ResearcherQueue::new(tmp.path());
        queue.ensure_dir().unwrap();

        queue.append(&make_item("i1", ItemStatus::Inbox)).unwrap();
        queue.append(&make_item("i2", ItemStatus::Inbox)).unwrap();
        queue.append(&make_item("i3", ItemStatus::Parked)).unwrap();

        let counts = queue.counts();
        assert_eq!(counts.inbox, 2);
        assert_eq!(counts.parked, 1);
        assert_eq!(counts.done, 0);
    }
}
