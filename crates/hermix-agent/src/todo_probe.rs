//! Unfinished-todo detection for the goal-continuation decision.
//!
//! ## The problem this solves
//!
//! A long task runs as: work a while -> write a summary -> stop. The summary
//! is a *visible text turn with no tool calls*, which is exactly the shape the
//! loop treats as "done". The user then types a follow-up and the next run
//! repeats the same shape. Measured on this machine: 184 goal-continuation
//! nudges fired historically, but `nudge_budget_exhausted` blocked 8 times --
//! after 8 nudges the loop gives up and hands control back, regardless of how
//! much work is actually left.
//!
//! The missing ingredient is an **objective** answer to "is the task done?".
//! The model's own summary cannot supply it (that is the thing being judged),
//! and turn counts cannot either (a task can be 3 turns or 300). A todo list
//! can: it is written down before the work, and items flip to `completed` only
//! when the model claims they are done.
//!
//! ## Why read the file instead of asking the tool layer
//!
//! `FileTodoBackend` owns a `Mutex<Vec<TodoItem>>` behind a `TodoBackend`
//! trait, and the agent loop has no handle to it -- the registry builds one per
//! session for the `todo` tool. Reaching through the registry would couple the
//! loop to tool construction. The backend's storage *is* `todos.json`, so
//! reading that file is the same data without the coupling. A parse failure is
//! treated as "no todos" rather than an error: this informs the stop decision,
//! it must never fail a run.

use serde::Deserialize;

/// How many unfinished titles to quote in the continuation nudge.
pub(crate) const MAX_REPORTED: usize = 8;

/// Terminal states -- a todo in any of these is not outstanding work.
const DONE_STATUSES: [&str; 3] = ["completed", "failed", "cancelled"];

/// Minimal view of `hermix_tools::backends::todo::TodoItem` -- only the fields
/// needed to decide whether work remains. Kept separate so this module does
/// not depend on the tools crate's full item shape (and so a field added there
/// cannot break deserialisation here).
#[derive(Debug, Deserialize)]
struct TodoProbe {
    #[serde(default)]
    title: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    session_id: Option<String>,
}

/// Outstanding todos belonging to `session_id`, read from `todos.json` under
/// the Hermix home directory.
pub(crate) fn unfinished(session_id: Option<&str>) -> (Vec<String>, usize) {
    let path = hermix_config::paths::hermix_home().join("todos.json");
    unfinished_at(&path, session_id)
}

/// Same as [`unfinished`] but against an explicit file. Split out so tests can
/// point at a temp file instead of mutating the process-wide `HERMIX_HOME`
/// env var (which races with every other test in the crate).
///
/// Returns `(pending_titles, total)`.
/// - `pending_titles` -- titles of items whose status is not terminal, capped
///   at [`MAX_REPORTED`] so a 500-item backlog cannot flood the nudge text.
/// - `total` -- how many items exist for this session at all. The caller uses
///   this to tell "no plan was ever written" (total == 0, nothing to say) from
///   "a plan exists and is unfinished" (the case worth continuing).
///
/// `session_id == None` returns empty: without a session the file mixes every
/// conversation's items together and "unfinished" would be meaningless.
pub(crate) fn unfinished_at(path: &std::path::Path, session_id: Option<&str>) -> (Vec<String>, usize) {
    let Some(session_id) = session_id.filter(|s| !s.is_empty()) else {
        return (Vec::new(), 0);
    };
    let Ok(raw) = std::fs::read_to_string(path) else {
        return (Vec::new(), 0);
    };
    let Ok(items) = serde_json::from_str::<Vec<TodoProbe>>(&raw) else {
        return (Vec::new(), 0);
    };

    let mut total = 0usize;
    let mut pending = Vec::new();
    for item in items {
        if item.session_id.as_deref() != Some(session_id) {
            continue;
        }
        total += 1;
        let done = DONE_STATUSES
            .iter()
            .any(|d| item.status.eq_ignore_ascii_case(d));
        if !done && pending.len() < MAX_REPORTED {
            pending.push(item.title);
        }
    }
    (pending, total)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn write(json: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("todos.json");
        std::fs::write(&path, json).expect("write");
        (dir, path)
    }

    /// The whole point: a plan with open items is *not* a finished task, even
    /// when the model just wrote a tidy summary claiming it is.
    #[test]
    fn open_items_are_reported_as_unfinished() {
        let (_d, p) = write(
            r#"[
              {"title":"step one","status":"completed","session_id":"s1"},
              {"title":"step two","status":"pending","session_id":"s1"},
              {"title":"step three","status":"in_progress","session_id":"s1"}
            ]"#,
        );
        let (pending, total) = unfinished_at(&p, Some("s1"));
        assert_eq!(total, 3);
        assert_eq!(pending, vec!["step two", "step three"]);
    }

    #[test]
    fn all_terminal_items_are_finished() {
        let (_d, p) = write(
            r#"[
              {"title":"a","status":"completed","session_id":"s1"},
              {"title":"b","status":"failed","session_id":"s1"}
            ]"#,
        );
        let (pending, total) = unfinished_at(&p, Some("s1"));
        assert_eq!(total, 2);
        assert!(pending.is_empty());
    }

    #[test]
    fn other_sessions_are_ignored() {
        let (_d, p) = write(
            r#"[
              {"title":"mine","status":"pending","session_id":"s1"},
              {"title":"theirs","status":"pending","session_id":"s2"}
            ]"#,
        );
        let (pending, total) = unfinished_at(&p, Some("s1"));
        assert_eq!(total, 1);
        assert_eq!(pending, vec!["mine"]);
    }

    /// A missing or corrupt file means "no plan" -- but it must never panic or
    /// error a run.
    #[test]
    fn missing_file_is_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("nope.json");
        let (pending, total) = unfinished_at(&p, Some("s1"));
        assert_eq!(total, 0);
        assert!(pending.is_empty());
    }

    #[test]
    fn corrupt_file_is_not_an_error() {
        let (_d, p) = write("not json at all");
        let (pending, total) = unfinished_at(&p, Some("s1"));
        assert_eq!(total, 0);
        assert!(pending.is_empty());
    }

    #[test]
    fn no_session_means_no_judgement() {
        let (_d, p) = write(r#"[{"title":"x","status":"pending","session_id":"s1"}]"#);
        let (pending, total) = unfinished_at(&p, None);
        assert_eq!(total, 0);
        assert!(pending.is_empty());
    }

    #[test]
    fn reported_titles_are_capped() {
        let items: Vec<String> = (0..20)
            .map(|i| format!(r#"{{"title":"t{i}","status":"pending","session_id":"s1"}}"#))
            .collect();
        let (_d, p) = write(&format!("[{}]", items.join(",")));
        let (pending, total) = unfinished_at(&p, Some("s1"));
        assert_eq!(total, 20, "total must count every item");
        assert_eq!(pending.len(), MAX_REPORTED, "titles must be capped");
    }
}
