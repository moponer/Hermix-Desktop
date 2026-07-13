//! Results model — `TaskResultRef` collection helpers.
//!
//! Provides utilities for managing the `result_refs` collection on a
//! Task, supporting the `present_files` equivalent (P7-1).
//!
//! (Phase 1 — P1-4)

use hermix_core::{TaskArtifactType, TaskResultRef};

/// Helper wrapper for managing a collection of `TaskResultRef`.
///
/// Provides deduplication, type filtering, and summary generation
/// for the frontend's "产物" tab.
#[derive(Debug, Clone, Default)]
pub struct TaskResults {
    refs: Vec<TaskResultRef>,
}

impl TaskResults {
    /// Create an empty collection.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create from an existing vector.
    pub fn from_vec(refs: Vec<TaskResultRef>) -> Self {
        Self { refs }
    }

    /// Add a result reference. Deduplicates by path — if a ref with
    /// the same path already exists, it's updated in place.
    pub fn add(&mut self, result: TaskResultRef) {
        if let Some(existing) = self.refs.iter_mut().find(|r| r.path == result.path) {
            *existing = result;
        } else {
            self.refs.push(result);
        }
    }

    /// Add multiple result references.
    pub fn extend(&mut self, results: impl IntoIterator<Item = TaskResultRef>) {
        for r in results {
            self.add(r);
        }
    }

    /// Remove a result reference by path.
    pub fn remove(&mut self, path: &str) -> bool {
        let before = self.refs.len();
        self.refs.retain(|r| r.path != path);
        self.refs.len() < before
    }

    /// Get all result references.
    pub fn as_slice(&self) -> &[TaskResultRef] {
        &self.refs
    }

    /// Convert to the underlying vector.
    pub fn into_vec(self) -> Vec<TaskResultRef> {
        self.refs
    }

    /// Filter by artifact type.
    pub fn filter_by_type(&self, artifact_type: TaskArtifactType) -> Vec<&TaskResultRef> {
        self.refs
            .iter()
            .filter(|r| r.artifact_type == artifact_type)
            .collect()
    }

    /// Count of result references.
    pub fn len(&self) -> usize {
        self.refs.len()
    }

    /// Whether the collection is empty.
    pub fn is_empty(&self) -> bool {
        self.refs.is_empty()
    }

    /// Generate a summary string suitable for display.
    pub fn summary(&self) -> String {
        if self.refs.is_empty() {
            return "No deliverables".to_string();
        }
        let html_count = self
            .filter_by_type(TaskArtifactType::Html)
            .len();
        let image_count = self
            .filter_by_type(TaskArtifactType::Image)
            .len();
        let file_count = self
            .filter_by_type(TaskArtifactType::File)
            .len();
        let report_count = self
            .filter_by_type(TaskArtifactType::Report)
            .len();

        let mut parts = Vec::new();
        if html_count > 0 {
            parts.push(format!("{} HTML", html_count));
        }
        if image_count > 0 {
            parts.push(format!("{} image", image_count));
        }
        if report_count > 0 {
            parts.push(format!("{} report", report_count));
        }
        if file_count > 0 {
            parts.push(format!("{} file", file_count));
        }
        let other = self.refs.len() - html_count - image_count - file_count - report_count;
        if other > 0 {
            parts.push(format!("{} other", other));
        }

        format!("{} deliverables ({})", self.refs.len(), parts.join(", "))
    }
}

impl From<Vec<TaskResultRef>> for TaskResults {
    fn from(refs: Vec<TaskResultRef>) -> Self {
        Self::from_vec(refs)
    }
}

impl From<TaskResults> for Vec<TaskResultRef> {
    fn from(results: TaskResults) -> Self {
        results.into_vec()
    }
}

impl std::ops::Deref for TaskResults {
    type Target = [TaskResultRef];
    fn deref(&self) -> &Self::Target {
        &self.refs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn html_ref(path: &str) -> TaskResultRef {
        TaskResultRef {
            path: path.to_string(),
            artifact_type: TaskArtifactType::Html,
            summary: None,
        }
    }

    fn image_ref(path: &str) -> TaskResultRef {
        TaskResultRef {
            path: path.to_string(),
            artifact_type: TaskArtifactType::Image,
            summary: None,
        }
    }

    #[test]
    fn test_add_and_dedup() {
        let mut results = TaskResults::new();
        results.add(html_ref("/a.html"));
        results.add(html_ref("/b.html"));
        assert_eq!(results.len(), 2);

        // Dedup by path
        results.add(html_ref("/a.html"));
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_add_updates_existing() {
        let mut results = TaskResults::new();
        results.add(html_ref("/a.html"));
        let updated = TaskResultRef {
            path: "/a.html".to_string(),
            artifact_type: TaskArtifactType::Html,
            summary: Some("Updated summary".to_string()),
        };
        results.add(updated);
        assert_eq!(results.len(), 1);
        assert!(results.as_slice()[0].summary.is_some());
    }

    #[test]
    fn test_remove() {
        let mut results = TaskResults::new();
        results.add(html_ref("/a.html"));
        assert!(results.remove("/a.html"));
        assert!(results.is_empty());
        assert!(!results.remove("/nonexistent"));
    }

    #[test]
    fn test_filter_by_type() {
        let mut results = TaskResults::new();
        results.add(html_ref("/a.html"));
        results.add(image_ref("/b.png"));
        results.add(html_ref("/c.html"));
        let htmls = results.filter_by_type(TaskArtifactType::Html);
        assert_eq!(htmls.len(), 2);
        let images = results.filter_by_type(TaskArtifactType::Image);
        assert_eq!(images.len(), 1);
    }

    #[test]
    fn test_summary_empty() {
        let results = TaskResults::new();
        assert_eq!(results.summary(), "No deliverables");
    }

    #[test]
    fn test_summary_with_items() {
        let mut results = TaskResults::new();
        results.add(html_ref("/a.html"));
        results.add(html_ref("/b.html"));
        results.add(image_ref("/c.png"));
        let s = results.summary();
        assert!(s.contains("3 deliverables"));
        assert!(s.contains("2 HTML"));
        assert!(s.contains("1 image"));
    }

    #[test]
    fn test_extend() {
        let mut results = TaskResults::new();
        results.extend(vec![html_ref("/a.html"), html_ref("/b.html")]);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_into_vec() {
        let mut results = TaskResults::new();
        results.add(html_ref("/a.html"));
        let vec: Vec<TaskResultRef> = results.into();
        assert_eq!(vec.len(), 1);
    }

    #[test]
    fn test_from_vec() {
        let results = TaskResults::from_vec(vec![html_ref("/a.html"), html_ref("/b.html")]);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_deref() {
        let mut results = TaskResults::new();
        results.add(html_ref("/a.html"));
        assert_eq!(results.len(), 1);
        // Deref gives us &[TaskResultRef]
        assert_eq!(results[0].path, "/a.html");
    }
}
