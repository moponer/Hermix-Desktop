//! Expert registry: a tiny, keyless, in-memory catalog of "expert" role
//! packages the desktop client can hire into a session.
//!
//! The contract is intentionally minimal — a name, an avatar, a category
//! label, a system-prompt body, and an optional backend toolset. There is no
//! IO, no async, no async-trait, no plugin machinery. Every "expert" the
//! desktop shows in the UI must round-trip through this registry.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

/// A single expert entry. Mirrors the TS `ExpertDef` in
/// `apps/desktop-tauri/src/data/builtinExperts.ts`. The system prompt is
/// the only field with non-trivial size — keep the rest compact so the
/// list endpoint stays cheap.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExpertEntry {
    /// Stable id, e.g. `"builtin:senior-dev"`.
    pub id: String,
    /// Display name in the user's language (we ship Chinese out of the box).
    pub name: String,
    /// Single-emoji avatar used as the chip avatar.
    pub avatar: String,
    /// Short role tag.
    pub role: String,
    /// Free-form category bucket shown in the Expert Center UI.
    pub category: String,
    /// One-line description for catalog cards.
    pub description: String,
    /// System prompt body, injected at the top of the agent system prompt.
    pub system_prompt: String,
    /// Optional backend toolset name; the agent loop uses it to filter the
    /// tool allow-list. See `crates/hermes-tools/src/toolset_distributions.rs`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub toolset: Option<String>,
    /// `true` for entries bundled with the desktop binary.
    pub is_builtin: bool,
}

/// Thread-safe in-memory catalog of experts. Lookups are cheap; the registry
/// is built up at startup and never expected to churn at runtime.
#[derive(Clone, Default, Debug)]
pub struct ExpertRegistry {
    inner: Arc<RwLock<HashMap<String, ExpertEntry>>>,
}

impl ExpertRegistry {
    /// Create a new, empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a registry pre-populated with the given entries.
    pub fn with_entries(entries: Vec<ExpertEntry>) -> Self {
        let mut inner: HashMap<String, ExpertEntry> = HashMap::with_capacity(entries.len());
        for e in entries {
            inner.insert(e.id.clone(), e);
        }
        Self {
            inner: Arc::new(RwLock::new(inner)),
        }
    }

    /// Insert or replace an entry. Returns the previous entry with the same
    /// id, if any.
    pub fn register(&self, entry: ExpertEntry) -> Option<ExpertEntry> {
        let mut guard = self.inner.write().expect("expert registry poisoned");
        guard.insert(entry.id.clone(), entry)
    }

    /// Bulk variant of [`register`]. Returns the number of new entries
    /// actually inserted (existing ids are replaced and not counted).
    pub fn register_many(&self, entries: Vec<ExpertEntry>) -> usize {
        let mut guard = self.inner.write().expect("expert registry poisoned");
        let mut added = 0;
        for e in entries {
            if guard.insert(e.id.clone(), e).is_none() {
                added += 1;
            }
        }
        added
    }

    /// Look up a single expert by id.
    pub fn get(&self, id: &str) -> Option<ExpertEntry> {
        let guard = self.inner.read().expect("expert registry poisoned");
        guard.get(id).cloned()
    }

    /// Snapshot the full catalog, sorted by id for stable ordering.
    pub fn list(&self) -> Vec<ExpertEntry> {
        let guard = self.inner.read().expect("expert registry poisoned");
        let mut out: Vec<ExpertEntry> = guard.values().cloned().collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Snapshot the catalog grouped by `category`, preserving stable
    /// ordering inside each bucket.
    pub fn list_by_category(&self) -> Vec<(String, Vec<ExpertEntry>)> {
        let guard = self.inner.read().expect("expert registry poisoned");
        let mut grouped: HashMap<String, Vec<ExpertEntry>> = HashMap::new();
        for entry in guard.values() {
            grouped
                .entry(entry.category.clone())
                .or_default()
                .push(entry.clone());
        }
        let mut out: Vec<(String, Vec<ExpertEntry>)> = grouped.into_iter().collect();
        for (_, items) in out.iter_mut() {
            items.sort_by(|a, b| a.id.cmp(&b.id));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Number of registered entries.
    pub fn len(&self) -> usize {
        self.inner.read().expect("expert registry poisoned").len()
    }

    /// `true` if no experts are registered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(id: &str, name: &str, category: &str) -> ExpertEntry {
        ExpertEntry {
            id: id.into(),
            name: name.into(),
            avatar: "🤖".into(),
            role: "test".into(),
            category: category.into(),
            description: "test expert".into(),
            system_prompt: "You are a test.".into(),
            toolset: None,
            is_builtin: true,
        }
    }

    #[test]
    fn register_and_get_round_trip() {
        let reg = ExpertRegistry::new();
        reg.register(sample("e1", "E1", "A"));
        let got = reg.get("e1").expect("registered");
        assert_eq!(got.name, "E1");
    }

    #[test]
    fn list_returns_sorted() {
        let reg = ExpertRegistry::with_entries(vec![
            sample("b", "B", "Cat"),
            sample("a", "A", "Cat"),
        ]);
        let all = reg.list();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, "a");
        assert_eq!(all[1].id, "b");
    }

    #[test]
    fn by_category_groups() {
        let reg = ExpertRegistry::with_entries(vec![
            sample("a1", "A1", "X"),
            sample("a2", "A2", "X"),
            sample("b1", "B1", "Y"),
        ]);
        let grouped = reg.list_by_category();
        assert_eq!(grouped.len(), 2);
        let x = grouped.iter().find(|(c, _)| c == "X").unwrap();
        assert_eq!(x.1.len(), 2);
    }

    #[test]
    fn register_many_counts_inserts_only() {
        let reg = ExpertRegistry::with_entries(vec![sample("a", "A", "X")]);
        let n = reg.register_many(vec![sample("a", "A2", "X"), sample("b", "B", "X")]);
        assert_eq!(n, 1);
        assert_eq!(reg.len(), 2);
        // The replacement wins:
        assert_eq!(reg.get("a").unwrap().name, "A2");
    }
}
