//! Central result budgets (ZCode declarative `resultBudget` analog).
//!
//! Every tool result eventually becomes model context, so the size caps are a
//! **product decision**, not an implementation detail. They used to be
//! scattered literals (`300_000` in `file.rs`, two separate `100_000`s in
//! `terminal.rs`, `50_000` in `pdf.rs`, `20_000` in the desktop snapshot).
//! Keeping them here means:
//!  - one place to answer "how big can a tool result get?";
//!  - the numbers are reviewable in a diff without hunting 40 files;
//!  - per-tool *semantic* limits (rows, depth, node counts) stay with their
//!    tool — only the **character budget handed to the model** lives here.
//!
//! The registry-level `max_result_size_chars` remains the outer safety net
//! (default 50 000 in `ToolRegistry::new`, overridable per registration);
//! these constants govern the point where a tool decides to stop *building*
//! a result, which is earlier and cheaper than truncating afterwards.

/// `read_file` body cap.
///
/// 80 K chars ≈ 20 K tokens. It used to be 300 K (≈ 75 K tokens): one read of
/// a large lockfile or minified bundle could put a 128 K-token window at the
/// compression threshold by itself, and the read that did it was invisible to
/// the model's own planning. `read_file` pages (`next_offset`), so a big file
/// costs one extra call instead of a third of the window — the paging path is
/// cheaper than the truncation path was.
pub const READ_FILE_CHARS: usize = 80_000;

/// `terminal` output cap (stdout+stderr combined) — 60 K chars ≈ 15 K tokens.
/// Commands that dump more should be piped through `Select-String`/`head`; the
/// truncation note says so, and the head/tail split keeps a build failure's
/// final status visible. Lowered from 100 K for the same reason as
/// [`READ_FILE_CHARS`]: one result should not own a sixth of the window.
pub const TERMINAL_OUTPUT_CHARS: usize = 60_000;

/// `pdf` extracted-text cap per call.
pub const PDF_TEXT_CHARS: usize = 50_000;

/// Desktop automation snapshot cap (`win_*` element dumps).
pub const DESKTOP_SNAPSHOT_CHARS: usize = 20_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budgets_are_ordered_by_expected_payload() {
        // The ordering encodes intent: file bodies may be biggest, desktop
        // snapshots must stay small (they are re-taken frequently).
        assert!(READ_FILE_CHARS > TERMINAL_OUTPUT_CHARS);
        assert!(TERMINAL_OUTPUT_CHARS > PDF_TEXT_CHARS);
        assert!(PDF_TEXT_CHARS > DESKTOP_SNAPSHOT_CHARS);
        assert!(DESKTOP_SNAPSHOT_CHARS >= 10_000);
    }

    #[test]
    fn budgets_exceed_the_registry_default() {
        // A per-tool budget below the registry default would be dead code
        // (the tool stops building before the registry would have cut it) —
        // that is fine, but these particular tools are explicitly meant to
        // use their own budget, so they must be at least the default.
        const REGISTRY_DEFAULT: usize = 50_000;
        assert!(READ_FILE_CHARS >= REGISTRY_DEFAULT);
        assert!(TERMINAL_OUTPUT_CHARS >= REGISTRY_DEFAULT);
        assert!(PDF_TEXT_CHARS >= REGISTRY_DEFAULT);
    }
}
