//! Categorized log management with rotation and retention.
//!
//! Design:
//! - Each log category (server/agent/tools/crash) gets its own `<category>.log` file.
//! - Size-based rotation: when the active file exceeds `max_bytes`, it is renamed to
//!   `.1`, existing `.1` → `.2` … up to `max_files` — oldest is deleted.
//! - A background task periodically prunes files older than `retain_days`.
//! - The `CategoryLayer` routes events by their `target()` to the right writer.

use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Write as IoWrite, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;
use tracing_subscriber::Layer;

/// Maximum rotated files kept per category.
const DEFAULT_MAX_FILES: usize = 5;
/// Default max size (bytes) before rotation per category log file.
pub const DEFAULT_MAX_BYTES: u64 = 10 * 1024 * 1024; // 10 MiB
/// Default retention in days.
const DEFAULT_RETAIN_DAYS: u64 = 7;
/// Cleanup interval: every 30 minutes.
const CLEANUP_INTERVAL_SECS: u64 = 30 * 60;

// ---------------------------------------------------------------------------
// LogCategory
// ---------------------------------------------------------------------------

/// Log category — each maps to its own log file with independent rotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogCategory {
    Server,
    Agent,
    Tools,
    Crash,
}

impl LogCategory {
    pub fn file_name(&self) -> &'static str {
        match self {
            LogCategory::Server => "server",
            LogCategory::Agent => "agent",
            LogCategory::Tools => "tools",
            LogCategory::Crash => "crash",
        }
    }

    pub fn all() -> &'static [LogCategory] {
        &[LogCategory::Server, LogCategory::Agent, LogCategory::Tools, LogCategory::Crash]
    }

    /// Determine category from a tracing target string.
    pub fn from_target(target: &str) -> LogCategory {
        let t = target.to_lowercase();
        if t.contains("crash") || t.contains("panic") {
            LogCategory::Crash
        } else if t.contains("agent") || t.contains("llm") || t.contains("memory") {
            LogCategory::Agent
        } else if t.contains("tool") || t.contains("approval") {
            LogCategory::Tools
        } else {
            LogCategory::Server
        }
    }
}

impl std::fmt::Display for LogCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.file_name())
    }
}

// ---------------------------------------------------------------------------
// Global log directory
// ---------------------------------------------------------------------------

static LOG_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

pub fn set_log_dir(dir: PathBuf) {
    let _ = LOG_DIR.set(dir);
}

pub fn get_log_dir() -> Option<&'static PathBuf> {
    LOG_DIR.get()
}

// ---------------------------------------------------------------------------
// Per-category rotating writer
// ---------------------------------------------------------------------------

struct CategoryWriter {
    dir: PathBuf,
    category: LogCategory,
    max_bytes: u64,
    max_files: usize,
    file_path: PathBuf,
    buf: BufWriter<fs::File>,
    buffer_bytes: u64,
}

impl CategoryWriter {
    fn new(dir: PathBuf, category: LogCategory, max_bytes: u64, max_files: usize) -> io::Result<Self> {
        fs::create_dir_all(&dir)?;
        let file_path = dir.join(format!("{}.log", category.file_name()));
        let file = OpenOptions::new().create(true).append(true).open(&file_path)?;
        let buffer_bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            dir,
            category,
            max_bytes,
            max_files,
            file_path,
            buf: BufWriter::new(file),
            buffer_bytes,
        })
    }

    fn write(&mut self, msg: &str) -> io::Result<()> {
        let len = msg.len() as u64;
        if self.buffer_bytes + len >= self.max_bytes {
            self.rotate()?;
        }
        self.buf.write_all(msg.as_bytes())?;
        self.buffer_bytes += len;
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.buf.flush()?;
        self.buffer_bytes = fs::metadata(&self.file_path).map(|m| m.len()).unwrap_or(0);
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.buf.flush()?;
        self.buffer_bytes = 0;

        // Remove oldest.
        let oldest = self.dir.join(format!(
            "{}.log.{}",
            self.category.file_name(),
            self.max_files
        ));
        let _ = fs::remove_file(&oldest);

        // Shift .N → .N+1
        for i in (1..self.max_files).rev() {
            let src = self.dir.join(format!("{}.log.{}", self.category.file_name(), i));
            let dst = self.dir.join(format!("{}.log.{}", self.category.file_name(), i + 1));
            let _ = fs::rename(&src, &dst);
        }

        // Rename current → .1
        let dst = self.dir.join(format!("{}.log.1", self.category.file_name()));
        let _ = fs::rename(&self.file_path, &dst);

        // Open fresh current file.
        let file = OpenOptions::new().create(true).append(true).open(&self.file_path)?;
        self.buf = BufWriter::new(file);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// CategoryLayer — routes events to per-category files
// ---------------------------------------------------------------------------

pub struct CategoryLayer {
    writers: Mutex<Vec<CategoryWriter>>,
    json_mode: bool,
}

impl CategoryLayer {
    /// Create a new category layer. `log_dir` is the logs root directory.
    pub fn new(
        log_dir: &Path,
        max_bytes: u64,
        max_files: usize,
        retain_days: u64,
        json_mode: bool,
    ) -> io::Result<Self> {
        let mut writers = Vec::new();
        for &cat in LogCategory::all() {
            writers.push(CategoryWriter::new(
                log_dir.to_path_buf(),
                cat,
                max_bytes,
                max_files,
            )?);
        }

        // Spawn background cleanup task.
        let dir = log_dir.to_path_buf();
        let max_files_clone = max_files;
        let retain_days_clone = retain_days;
        std::thread::spawn(move || {
            // Initial cleanup.
            prune_old_files(&dir, retain_days_clone, max_files_clone);

            // Periodic cleanup using a simple timer loop (no tokio needed for sync I/O).
            loop {
                std::thread::sleep(Duration::from_secs(CLEANUP_INTERVAL_SECS));
                prune_old_files(&dir, retain_days_clone, max_files_clone);
            }
        });

        Ok(Self {
            writers: Mutex::new(writers),
            json_mode,
        })
    }
}

/// Format a tracing `Event` into a String using the `Visit` trait.
fn format_event(event: &Event<'_>, json_mode: bool) -> String {
    use tracing::field::Visit;

    struct EventVisitor {
        msg: String,
        other_fields: Vec<(String, String)>,
    }

    impl EventVisitor {
        fn new() -> Self {
            Self {
                msg: String::new(),
                other_fields: Vec::new(),
            }
        }

        fn finish_json(&self, ts: &str, level: &str, target: &str) -> String {
            let fields_str = self
                .other_fields
                .iter()
                .map(|(k, v)| format!("\"{}\":\"{}\"", k, v.replace('"', "\\\"")))
                .collect::<Vec<_>>()
                .join(",");
            if fields_str.is_empty() {
                format!(
                    "{{\"ts\":\"{}\",\"level\":\"{}\",\"target\":\"{}\",\"msg\":\"{}\"}}\n",
                    ts,
                    level,
                    target,
                    self.msg.replace('"', "\\\"")
                )
            } else {
                format!(
                    "{{\"ts\":\"{}\",\"level\":\"{}\",\"target\":\"{}\",\"msg\":\"{}\",{}}}\n",
                    ts,
                    level,
                    target,
                    self.msg.replace('"', "\\\""),
                    fields_str
                )
            }
        }

        /// Plain line: `ts LEVEL [target] msg key=value …`.
        ///
        /// Structured fields used to be dropped here, which silently
        /// discarded every `tracing::info!(api_ms = …, tool_ms = …)`
        /// the agent emits — `turn report`, the turn watchdog's
        /// `elapsed_secs`/`deadline_secs`, `handle_max_iterations`'
        /// `error=…`. Latency triage was impossible from the log for
        /// that reason alone. Values are collapsed to one line so the
        /// file stays line-delimited.
        fn finish_plain(&self, ts: &str, level: &str, target: &str) -> String {
            let msg = self.msg.trim_end().replace('\n', " ");
            if self.other_fields.is_empty() {
                return format!("{ts} {level} [{target}] {msg}\n");
            }
            let mut line = format!("{ts} {level} [{target}] {msg}");
            for (k, v) in &self.other_fields {
                line.push(' ');
                line.push_str(k);
                line.push('=');
                line.push_str(&v.replace('\n', " "));
            }
            line.push('\n');
            line
        }
    }

    impl Visit for EventVisitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            let name = field.name();
            if name == "message" {
                self.msg = format!("{:?}", value);
            } else {
                self.other_fields.push((name.to_string(), format!("{:?}", value)));
            }
        }

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            let name = field.name();
            if name == "message" {
                self.msg = value.to_string();
            } else {
                self.other_fields.push((name.to_string(), value.to_string()));
            }
        }

        fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
            self.record_debug(field, &value);
        }

        fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
            self.record_debug(field, &value);
        }

        fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
            self.record_debug(field, &value);
        }
    }

    let mut visitor = EventVisitor::new();
    event.record(&mut visitor);

    let now_ts = chrono::Utc::now().to_rfc3339();
    let now_plain = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let level = event.metadata().level().to_string();
    let target = event.metadata().target();

    if json_mode {
        visitor.finish_json(&now_ts, &level, target)
    } else {
        visitor.finish_plain(&now_plain.to_string(), &level, target)
    }
}

/// 因写盘锁竞争而丢掉的日志条数。排障时「日志里没有这一条」必须能被区分成
/// 「没发生」还是「被丢了」——此前 `try_lock` 失败是完全静默的。
static DROPPED_RECORDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// 实际写盘/刷新失败次数（`let _ =` 吞掉的那部分）。
static WRITE_FAILURES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// flush 节流（P2-7）：累计多少条日志刷一次盘。
///
/// 此前**每条**日志都 `write + flush`，热路径（流式 chunk、工具 stdout、
/// 每秒上百条的轮次日志）上等于每条一次系统调用。改成攒批刷盘后，最坏
/// 情况是崩溃丢掉这一窗口内的尾部条目——而崩溃前几百毫秒的日志本来也
/// 大多写不全，用这个换取热路径的固定开销是划算的。
const FLUSH_EVERY_N: u64 = 64;
/// 距上次 flush 超过这么多毫秒就强制刷一次（防止低频日志长期滞留在缓冲区）。
const FLUSH_EVERY_MS: u64 = 200;
static PENDING_SINCE_FLUSH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static LAST_FLUSH_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 记录一次写盘失败；每 100 次才打一条 stderr，避免写盘坏掉时自己刷爆。
fn record_write_failure() {
    let n = WRITE_FAILURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if n % 100 == 0 {
        eprintln!("[hermix-telemetry] log write failed {n} time(s)");
    }
}

impl<S: Subscriber> Layer<S> for CategoryLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        use std::sync::atomic::Ordering;

        let target = event.metadata().target();
        let category = LogCategory::from_target(target);
        // writers[0..4] correspond to Server/Agent/Tools/Crash in order.
        let idx = match category {
            LogCategory::Server => 0,
            LogCategory::Agent => 1,
            LogCategory::Tools => 2,
            LogCategory::Crash => 3,
        };

        let line = format_event(event, self.json_mode);

        let mut writers = match self.writers.try_lock() {
            Ok(w) => w,
            Err(_) => {
                DROPPED_RECORDS.fetch_add(1, Ordering::Relaxed);
                return;
            }
        };

        // 先补记此前因抢锁失败丢掉的条数（ack 归零，只补一次）。
        let dropped = DROPPED_RECORDS.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            if let Some(w) = writers.get_mut(idx) {
                let note = format!(
                    "[log] {dropped} record(s) dropped: writer lock busy at emit time\n"
                );
                let _ = w.write(&note);
            }
        }

        if let Some(w) = writers.get_mut(idx) {
            if w.write(&line).is_err() {
                record_write_failure();
            } else {
                let pending = PENDING_SINCE_FLUSH.fetch_add(1, Ordering::Relaxed) + 1;
                let now_ms = now_millis();
                let last = LAST_FLUSH_MS.load(Ordering::Relaxed);
                if pending >= FLUSH_EVERY_N || now_ms.saturating_sub(last) >= FLUSH_EVERY_MS {
                    PENDING_SINCE_FLUSH.store(0, Ordering::Relaxed);
                    LAST_FLUSH_MS.store(now_ms, Ordering::Relaxed);
                    if w.flush().is_err() {
                        record_write_failure();
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Prune old files
// ---------------------------------------------------------------------------

fn prune_old_files(dir: &Path, retain_days: u64, max_files: usize) {
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(retain_days * 24 * 3600))
        .unwrap_or(SystemTime::UNIX_EPOCH);

    for cat in LogCategory::all() {
        let prefix = format!("{}.log", cat.file_name());
        let mut files: Vec<_> = match fs::read_dir(dir) {
            Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
            Err(_) => continue,
        };

        files.retain(|e| {
            let name_str = e.file_name();
            let name_str = name_str.to_string_lossy();
            name_str == prefix || name_str.starts_with(&format!("{}.", prefix))
        });

        // Sort by mtime ascending (oldest first) for age-based deletion.
        files.sort_by(|a, b| {
            let ma = fs::metadata(a.path()).and_then(|m| m.modified()).ok();
            let mb = fs::metadata(b.path()).and_then(|m| m.modified()).ok();
            ma.cmp(&mb)
        });

        // Delete files older than cutoff.
        for entry in &files {
            if let Ok(mtime) = fs::metadata(entry.path()).and_then(|m| m.modified()) {
                if mtime < cutoff {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }

        // Re-sort descending (newest first) for max-files trim.
        files.sort_by(|a, b| {
            let ma = fs::metadata(a.path()).and_then(|m| m.modified()).ok();
            let mb = fs::metadata(b.path()).and_then(|m| m.modified()).ok();
            mb.cmp(&ma)
        });

        // Remove files beyond max_files.
        for entry in files.iter().skip(max_files) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

// ---------------------------------------------------------------------------
// Public API for reading logs
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct LogFileInfo {
    pub category: String,
    pub filename: String,
    pub size_bytes: u64,
    pub modified: String,
}

/// List all log files for a given category (or all categories if None).
pub fn list_log_files(
    log_dir: &Path,
    category: Option<LogCategory>,
) -> io::Result<Vec<LogFileInfo>> {
    let categories: Vec<LogCategory> = category
        .map(|c| vec![c])
        .unwrap_or_else(|| LogCategory::all().to_vec());

    let mut out = Vec::new();
    for cat in categories {
        let prefix = format!("{}.log", cat.file_name());
        for entry in fs::read_dir(log_dir)? {
            let entry = entry?;
            let name_str = entry.file_name();
            let name_str = name_str.to_string_lossy();
            if name_str == prefix || name_str.starts_with(&format!("{}.", prefix)) {
                let meta = entry.metadata()?;
                let mtime = meta
                    .modified()
                    .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339())
                    .unwrap_or_default();
                out.push(LogFileInfo {
                    category: cat.file_name().to_string(),
                    filename: name_str.to_string(),
                    size_bytes: meta.len(),
                    modified: mtime,
                });
            }
        }
    }

    out.sort_by(|a, b| {
        a.category
            .cmp(&b.category)
            .then_with(|| b.modified.cmp(&a.modified))
    });

    Ok(out)
}

/// Read the tail of a specific log file (last `max_lines` lines).
pub fn read_log_tail(log_dir: &Path, filename: &str, max_lines: usize) -> io::Result<Vec<String>> {
    let path = log_dir.join(filename);
    let file = fs::File::open(&path)?;
    let reader = BufReader::new(file);
    let all_lines: Vec<String> = reader.lines().filter_map(|l| l.ok()).collect();
    let start = all_lines.len().saturating_sub(max_lines);
    Ok(all_lines[start..].to_vec())
}

/// Read lines from a specific range (0-indexed, inclusive start, exclusive end).
pub fn read_log_range(
    log_dir: &Path,
    filename: &str,
    start_line: usize,
    end_line: usize,
) -> io::Result<Vec<String>> {
    let path = log_dir.join(filename);
    let file = fs::File::open(&path)?;
    let reader = BufReader::new(file);
    let all_lines: Vec<String> = reader.lines().filter_map(|l| l.ok()).collect();
    let start = start_line.min(all_lines.len());
    let end = end_line.min(all_lines.len());
    Ok(all_lines[start..end].to_vec())
}

/// Compute total disk usage of all log files (optionally filtered by category).
pub fn log_disk_usage(log_dir: &Path, category: Option<LogCategory>) -> io::Result<u64> {
    let files = list_log_files(log_dir, category)?;
    Ok(files.iter().map(|f| f.size_bytes).sum())
}

// ---------------------------------------------------------------------------
// TelemetryConfig & TelemetryGuard (public API)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryConfig {
    pub level: String,
    pub json: bool,
    pub service_name: String,
    pub otlp_endpoint: Option<String>,
    pub log_to_stderr: bool,
    pub log_dir: Option<PathBuf>,
    pub log_max_bytes: u64,
    pub log_files: usize,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            json: false,
            service_name: "hermes".to_string(),
            otlp_endpoint: None,
            log_to_stderr: false,
            log_dir: None,
            log_max_bytes: DEFAULT_MAX_BYTES,
            log_files: DEFAULT_MAX_FILES,
        }
    }
}

pub struct TelemetryGuard {
    _inner: Option<()>,
}

pub fn pin_init_guard(guard: TelemetryGuard) {
    static PINNED: std::sync::Mutex<Option<TelemetryGuard>> = std::sync::Mutex::new(None);
    if let Ok(mut g) = PINNED.lock() {
        *g = Some(guard);
    } else {
        std::mem::forget(guard);
    }
}

// ---------------------------------------------------------------------------
// init_unified_telemetry
// ---------------------------------------------------------------------------

enum Sink {
    Stderr,
    Stdout,
}

/// Unified telemetry init: console (stderr/stdout) + per-category file layer + OTLP.
pub fn init_unified_telemetry(config: &TelemetryConfig) -> TelemetryGuard {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(config.level.clone()));

    let sink = if config.log_to_stderr {
        Sink::Stderr
    } else {
        Sink::Stdout
    };

    // Resolve log_dir to a directory (not a file path).
    let log_dir = config.log_dir.as_ref().map(|p| {
        if p.extension().is_some() {
            p.parent().unwrap_or(p)
        } else {
            p
        }
        .to_path_buf()
    });

    if let Some(ref dir) = log_dir {
        set_log_dir(dir.clone());
    }

    // Build the category layer.
    let category_layer: Option<CategoryLayer> = log_dir
        .as_ref()
        .and_then(|dir| {
            CategoryLayer::new(
                dir,
                config.log_max_bytes,
                config.log_files,
                DEFAULT_RETAIN_DAYS,
                config.json,
            )
            .ok()
        });

    let _ = match (category_layer, sink, config.json) {
        // Console + file
        (Some(cl), Sink::Stderr, false) => tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(true)
                    .with_writer(std::io::stderr),
            )
            .with(cl)
            .try_init(),
        (Some(cl), Sink::Stderr, true) => tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(true)
                    .json()
                    .with_writer(std::io::stderr),
            )
            .with(cl)
            .try_init(),
        (Some(cl), Sink::Stdout, false) => tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(true)
                    .with_writer(std::io::stdout),
            )
            .with(cl)
            .try_init(),
        (Some(cl), Sink::Stdout, true) => tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(true)
                    .json()
                    .with_writer(std::io::stdout),
            )
            .with(cl)
            .try_init(),
        // Console only (no file layer)
        (None, Sink::Stderr, false) => tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(true)
                    .with_writer(std::io::stderr),
            )
            .try_init(),
        (None, Sink::Stderr, true) => tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(true)
                    .json()
                    .with_writer(std::io::stderr),
            )
            .try_init(),
        (None, Sink::Stdout, false) => tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(true)
                    .with_writer(std::io::stdout),
            )
            .try_init(),
        (None, Sink::Stdout, true) => tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(true)
                    .json()
                    .with_writer(std::io::stdout),
            )
            .try_init(),
    };

    // OTLP（P3-5）：原实现在这里再建一个 registry 并 `try_init()`。全局
    // subscriber 已由上面的主 registry 设置，第二次 `try_init` **必然** Err 且
    // 被 `let _ =` 吞掉 —— 这条链路构造上不可能生效，却让人以为"配了 endpoint
    // 就有 trace"。要真正接进来，OTLP layer 必须进**同一个** registry 的静态
    // Layer 组合（`Option<L>` 没有 Layer impl，得在下面 8 个 match 分支里各带
    // 一次，或统一改成 boxed layer），属独立批次。当前 `otlp` feature 也从未
    // 在任何构建里启用，所以先删掉这条假链路；`otlp.rs` 与 feature 定义保留，
    // 接线时复用。

    TelemetryGuard { _inner: Some(()) }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn list_log_files_empty() {
        let tmp = TempDir::new().unwrap();
        let files = list_log_files(tmp.path(), Some(LogCategory::Server)).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn log_disk_usage_empty() {
        let tmp = TempDir::new().unwrap();
        let usage = log_disk_usage(tmp.path(), Some(LogCategory::Agent)).unwrap();
        assert_eq!(usage, 0);
    }

    #[test]
    fn category_from_target() {
        assert_eq!(LogCategory::from_target("hermix.crash"), LogCategory::Crash);
        assert_eq!(LogCategory::from_target("panic_handler"), LogCategory::Crash);
        assert_eq!(LogCategory::from_target("hermix_agent_loop"), LogCategory::Agent);
        assert_eq!(LogCategory::from_target("hermix_tools_approval"), LogCategory::Tools);
        assert_eq!(LogCategory::from_target("hermix_server_routes"), LogCategory::Server);
        assert_eq!(LogCategory::from_target("axum_router"), LogCategory::Server);
    }

    #[test]
    fn read_log_tail_empty() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("server.log"), b"line1\nline2\nline3\n").unwrap();
        let lines = read_log_tail(tmp.path(), "server.log", 10).unwrap();
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn read_log_range_from_file() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("server.log"),
            b"line0\nline1\nline2\nline3\nline4\n",
        )
        .unwrap();
        let lines = read_log_range(tmp.path(), "server.log", 1, 3).unwrap();
        assert_eq!(lines, vec!["line1", "line2"]);
    }

    #[test]
    fn plain_lines_keep_structured_fields() {
        // The agent loop stamps `api_ms` / `tool_ms` / `elapsed_secs` /
        // `error` on its events. Dropping them made "why is this turn
        // slow" unanswerable from the log — only the bare message
        // survived. One line per event, fields appended.
        let tmp = TempDir::new().unwrap();
        let layer = CategoryLayer::new(tmp.path(), DEFAULT_MAX_BYTES, 5, 7, false).unwrap();
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                target: "hermix_agent_loop",
                api_ms = 9_100u64,
                tool_ms = 40u64,
                has_visible_text = false,
                "turn report"
            );
        });
        let body = fs::read_to_string(tmp.path().join("agent.log")).unwrap();
        assert!(body.contains("turn report"), "message lost: {body}");
        assert!(body.contains("api_ms=9100"), "api_ms lost: {body}");
        assert!(body.contains("tool_ms=40"), "tool_ms lost: {body}");
        assert!(
            body.contains("has_visible_text=false"),
            "bool field lost: {body}"
        );
        assert_eq!(body.lines().count(), 1, "must stay one line per event");
    }
}
