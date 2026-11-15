//! Crash dump writer for post-mortem diagnostics.
//!
//! ( / ) When the agent task's `catch_unwind`
//! catches a panic, this module writes a structured JSON dump to
//! `hermix_home()/crash_dumps/` containing:
//! - Timestamp + session ID
//! - Panic message + backtrace status
//! - Last N turns of conversation (truncated)
//!
//! Design constraints (from F10):
//! - Dump failure must NOT block the main flow — fallback to `tracing::error!`.
//! - Dump files are rotated: keep at most 50 most recent.
//! - All I/O is synchronous (called from within `catch_unwind` recovery
//!   path, not async context) — no `spawn_blocking` needed.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::Utc;
use serde::{Deserialize, Serialize};

/// Monotonic suffix so two panics in the same wall-clock second don't
/// overwrite each other's log file.
static PANIC_SEQ: AtomicU64 = AtomicU64::new(0);

/// Maximum number of recent messages to include in the dump.
const MAX_MESSAGES: usize = 10;

/// Maximum characters per message content in the dump.
const MAX_CONTENT_CHARS: usize = 500;

/// Maximum crash dump files to retain.
const MAX_DUMP_FILES: usize = 50;

/// Structured crash dump payload.
#[derive(Debug, Serialize, Deserialize)]
pub struct CrashDump {
    /// ISO-8601 timestamp of when the panic was caught.
    pub timestamp: String,
    /// Session ID where the panic occurred.
    pub session_id: String,
    /// The panic message extracted from the `Box<dyn Any>` payload.
    pub panic_message: String,
    /// Whether a backtrace was captured.
    pub backtrace_status: String,
    /// Recent conversation turns (truncated), oldest-first.
    pub recent_turns: Vec<TurnSummary>,
}

/// A truncated summary of a single conversation turn.
#[derive(Debug, Serialize, Deserialize)]
pub struct TurnSummary {
    pub role: String,
    /// First `MAX_CONTENT_CHARS` chars of the message content.
    pub content_preview: String,
}

/// Write a crash dump to `hermix_home()/crash_dumps/`.
///
/// This function is infallible — if any I/O step fails, it logs an
/// error and returns silently. The panic recovery path must never
/// be blocked by dump-write failures ( F10).
pub fn write_crash_dump(
    hermix_home: &Path,
    session_id: &str,
    panic_message: &str,
    recent_messages: &[hermix_core::Message],
) {
    let dump = build_dump(session_id, panic_message, recent_messages);
    let dump_dir = hermix_home.join("crash_dumps");

    // Try to create the directory (may already exist).
    if let Err(e) = std::fs::create_dir_all(&dump_dir) {
        tracing::error!(
            error = %e,
            "crash_dump: failed to create crash_dumps directory — dump lost"
        );
        return;
    }

    // Serialize to JSON.
    let json = match serde_json::to_string_pretty(&dump) {
        Ok(j) => j,
        Err(e) => {
            tracing::error!(
                error = %e,
                "crash_dump: failed to serialize dump JSON — dump lost"
            );
            return;
        }
    };

    // Write file: YYYY-MM-DD-HHMMSS_session-id-slug.json
    let ts_slug = Utc::now().format("%Y-%m-%d-%H%M%S").to_string();
    let session_slug = session_id
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect::<String>();
    let filename = format!("{}_{}.json", ts_slug, session_slug);
    let dump_path = dump_dir.join(&filename);

    if let Err(e) = std::fs::write(&dump_path, json.as_bytes()) {
        tracing::error!(
            error = %e,
            path = %dump_path.display(),
            "crash_dump: failed to write dump file — dump lost"
        );
        return;
    }

    tracing::info!(
        path = %dump_path.display(),
        session = %session_id,
        "crash_dump: written successfully"
    );

    // Rotate: keep only MAX_DUMP_FILES most recent.
    rotate_dumps(&dump_dir);
}

fn build_dump(
    session_id: &str,
    panic_message: &str,
    recent_messages: &[hermix_core::Message],
) -> CrashDump {
    // Capture backtrace status (release builds may not have symbols).
    let backtrace_status = {
        let bt = std::backtrace::Backtrace::force_capture();
        let bt_str = bt.to_string();
        if bt_str.is_empty() || bt_str == "disabled backtrace" {
            "unavailable (no debug symbols or disabled)".to_string()
        } else {
            // Truncate backtrace to avoid massive dumps.
            if bt_str.len() > 4000 {
                format!("{}...(truncated)", &bt_str[..bt_str.floor_char_boundary(4000)])
            } else {
                bt_str
            }
        }
    };

    // Take last MAX_MESSAGES messages, truncate content.
    let recent_turns: Vec<TurnSummary> = recent_messages
        .iter()
        .rev()
        .take(MAX_MESSAGES)
        .rev()
        .map(|m| TurnSummary {
            role: format!("{:?}", m.role),
            content_preview: truncate_str(m.content.as_deref().unwrap_or(""), MAX_CONTENT_CHARS),
        })
        .collect();

    CrashDump {
        timestamp: Utc::now().to_rfc3339(),
        session_id: session_id.to_string(),
        panic_message: panic_message.to_string(),
        backtrace_status,
        recent_turns,
    }
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    // 判长从**字节**改为**字符**：此前 CJK 串在字节口径下会提前判"不超长"
    // 而漏截断（不 panic，但语义错）；切分统一走 core 的 UTF-8 安全实现。
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        format!(
            "{}...(truncated)",
            hermix_core::text_utils::truncate_chars(s, max_chars)
        )
    }
}

/// Remove old crash dumps, keeping only the `MAX_DUMP_FILES` most recent.
fn rotate_dumps(dir: &Path) {
    let mut entries: Vec<(PathBuf, std::time::SystemTime)> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let path = e.path();
                let mtime = e.metadata().ok()?.modified().ok()?;
                Some((path, mtime))
            })
            .collect(),
        Err(_) => return,
    };

    if entries.len() <= MAX_DUMP_FILES {
        return;
    }

    // Sort by modification time, newest first.
    entries.sort_by(|a, b| b.1.cmp(&a.1));

    // Remove everything beyond MAX_DUMP_FILES.
    for (path, _) in entries.iter().skip(MAX_DUMP_FILES) {
        if let Err(e) = std::fs::remove_file(path) {
            tracing::warn!(
                error = %e,
                path = %path.display(),
                "crash_dump: failed to remove old dump during rotation"
            );
        }
    }
}

/// Install a process-wide panic hook that durably records every panic —
/// location, message, thread, and backtrace — to `<home>/logs/panic-<ts>-<seq>.log`
/// before delegating to the previous (default) hook.
///
/// Why this exists: the agent loop's per-task `catch_unwind` + crash dump only
/// covers the retired WebSocket path (`ws.rs`). The stdio transport the desktop
/// sidecar actually uses runs the loop on the main future with **no** such
/// wrapper, and `panic = "unwind"`, so a panic anywhere in the stdio loop
/// unwinds straight into `main` and the process exits ("后端进程已退出"). The
/// default hook's message only reaches stderr, which the packaged GUI app
/// (`windows_subsystem = "windows"`) discards — the "backend exited mid-task,
/// no local log, can't tell why" dead end. Writing the file synchronously here
/// (the hook runs on the panicking thread before unwind completes) makes the
/// cause durable on disk for every panic, on every transport.
///
/// Call once from `main` after telemetry init. Fires for *every* panic,
/// including ones a later `catch_unwind` recovers — that noise is worth having:
/// a panic happened, and now it's on record.
pub fn install_panic_hook(home: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info: &std::panic::PanicHookInfo<'_>| {
        if let Err(e) = write_panic_log(&home, info) {
            eprintln!("[crash_dump] failed to write panic log: {e}");
        }
        previous(info);
    }));
}

/// Write one panic's details to `<home>/logs/panic-<ts>-<seq>.log`. Synchronous
/// and infallible-by-contract: the caller logs any I/O error and moves on, so a
/// panic-in-progress is never blocked by the diagnostics write itself.
fn write_panic_log(home: &Path, info: &std::panic::PanicHookInfo<'_>) -> std::io::Result<()> {
    let dir = home.join("logs");
    std::fs::create_dir_all(&dir)?;
    let ts = Utc::now().format("%Y-%m-%d-%H%M%S");
    let seq = PANIC_SEQ.fetch_add(1, Ordering::Relaxed);
    let path = dir.join(format!("panic-{ts}-{seq}.log"));

    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "<unknown location>".to_string());
    let message = info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "<non-string panic payload>".to_string());
    let thread_name = std::thread::current()
        .name()
        .unwrap_or("<unnamed>")
        .to_string();
    let backtrace = std::backtrace::Backtrace::force_capture();

    let body = format!(
        "Hermix panic\n\
         location:  {location}\n\
         thread:    {thread_name}\n\
         message:   {message}\n\
         \n\
         backtrace:\n{backtrace}\n"
    );
    std::fs::write(&path, body)?;

    // Best-effort mirror into the rolling server log so the in-app diagnostics
    // panel (which tails <home>/logs) surfaces it inline. Tracing may be
    // mid-teardown during a fatal unwind; failure here is harmless — the file
    // above is the durable copy.
    tracing::error!(
        target: "hermix.crash",
        path = %path.display(),
        "panic hook fired: {message} @ {location}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_core::{Message, MessageRole};
    use tempfile::TempDir;

    #[test]
    fn write_crash_dump_creates_file() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();

        let messages = vec![
            Message {
                role: MessageRole::User,
                content: Some("Hello, can you help me?".to_string()),
                ..Default::default()
            },
            Message {
                role: MessageRole::Assistant,
                content: Some("Of course! What do you need?".to_string()),
                ..Default::default()
            },
        ];

        write_crash_dump(home, "test-session-123", "test panic: index out of bounds", &messages);

        let dump_dir = home.join("crash_dumps");
        assert!(dump_dir.exists(), "crash_dumps directory should be created");

        let files: Vec<_> = std::fs::read_dir(&dump_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(files.len(), 1, "exactly one dump file should exist");

        // Read and parse the dump.
        let content = std::fs::read_to_string(files[0].path()).unwrap();
        let dump: CrashDump = serde_json::from_str(&content).unwrap();
        assert_eq!(dump.session_id, "test-session-123");
        assert_eq!(dump.panic_message, "test panic: index out of bounds");
        assert_eq!(dump.recent_turns.len(), 2);
        assert_eq!(dump.recent_turns[0].role, "User");
        assert_eq!(dump.recent_turns[1].role, "Assistant");
    }

    #[test]
    fn truncate_str_handles_unicode() {
        let s = "你好世界你好世界你好世界"; // 12 chars, 36 bytes
        let truncated = truncate_str(s, 5);
        assert!(truncated.contains("你好世"));
        assert!(truncated.contains("...(truncated)"));
    }

    #[test]
    fn truncate_str_short_string_unchanged() {
        let s = "short";
        let truncated = truncate_str(s, 100);
        assert_eq!(truncated, "short");
    }

    #[test]
    fn rotate_removes_old_dumps() {
        let tmp = TempDir::new().unwrap();
        let dump_dir = tmp.path().join("crash_dumps");
        std::fs::create_dir_all(&dump_dir).unwrap();

        // Create MAX_DUMP_FILES + 5 dummy files with incrementing mtimes.
        for i in 0..(MAX_DUMP_FILES + 5) {
            let path = dump_dir.join(format!("2026-01-{:02}-000000_{:03}.json", i + 1, i));
            std::fs::write(&path, b"{}").unwrap();
            // Slightly different mtime for each file.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644));
            }
            // Small sleep to ensure distinct mtimes.
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        rotate_dumps(&dump_dir);

        let remaining = std::fs::read_dir(&dump_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .count();
        assert_eq!(
            remaining, MAX_DUMP_FILES,
            "should keep exactly MAX_DUMP_FILES after rotation"
        );
    }

    #[test]
    fn write_crash_dump_with_empty_messages() {
        let tmp = TempDir::new().unwrap();
        write_crash_dump(tmp.path(), "empty-session", "panic with no history", &[]);
        let dump_dir = tmp.path().join("crash_dumps");
        let files: Vec<_> = std::fs::read_dir(&dump_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(files.len(), 1);

        let content = std::fs::read_to_string(files[0].path()).unwrap();
        let dump: CrashDump = serde_json::from_str(&content).unwrap();
        assert!(dump.recent_turns.is_empty());
    }

    #[test]
    fn build_dump_truncates_long_content() {
        let long_content = "A".repeat(2000);
        let messages = vec![Message {
            role: MessageRole::User,
            content: Some(long_content),
            ..Default::default()
        }];
        let dump = build_dump("s1", "panic", &messages);
        assert!(dump.recent_turns[0].content_preview.len() < 600);
        assert!(dump.recent_turns[0].content_preview.contains("...(truncated)"));
    }
}
