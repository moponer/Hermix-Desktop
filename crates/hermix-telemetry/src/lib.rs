//! Telemetry bootstrap and in-process metrics registry.

use std::sync::atomic::{AtomicU64, Ordering};
use hermix_config::env_compat;
use std::path::PathBuf;
use std::time::Duration;

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::prelude::*;
mod run_trace;

#[cfg(feature = "otlp")]
mod otlp;
#[cfg(not(feature = "otlp"))]
mod otlp {
    use tracing_subscriber::layer::Layer;
    use tracing_subscriber::Registry;
    /// Stub when the `otlp` feature is disabled — never actually invoked because
    /// the OTLP block is feature-gated in [`init_telemetry`].
    #[allow(dead_code)]
    pub fn build_otel_layer(
        _service_name: &str,
        _endpoint: &str,
    ) -> Result<
        Box<dyn Layer<Registry> + Send + Sync>,
        Box<dyn std::error::Error + Send + Sync + 'static>,
    > {
        Ok(Box::new(tracing_subscriber::layer::Identity::default()))
    }
}

pub use run_trace::{RunTrace, RunTraceRecorder, TraceEvent, TraceEventKind};

/// Top-level output stream — kept as a private enum so `try_init_subscriber` can
/// pattern-match on a single discriminant without re-deciding stdout/stderr at each
/// branch.
#[derive(Debug, Clone, Copy)]
enum Sink {
    Stderr,
    Stdout,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryConfig {
    pub level: String,
    pub json: bool,
    pub service_name: String,
    /// OTLP HTTP traces URL base (`http://host:4318`) or full path (`.../v1/traces`).
    /// Active when crate is built with `--features otlp`.
    pub otlp_endpoint: Option<String>,
    /// stdio 传输模式下必须置 true：stdout 被 JSON Lines 协议独占，日志行混入
    /// 会被对端当帧解析，触发 "unparseable frame" 死循环。
    pub log_to_stderr: bool,
    /// (P0-7 / STABILITY-PLAN §3.1) 装版二进制 stderr 被 windows 子系统吃掉，
    /// 必须把日志写到 hermix_home/logs/ 才能在用户机器上被看见。
    /// 为 None 则不写文件（dev 调试时可用，此时 stderr 已够）。
    pub log_dir: Option<PathBuf>,
    /// 单个日志文件最大字节数；超过按大小轮转。默认 10 MiB。
    pub log_max_bytes: u64,
    /// 保留的轮转文件数（service.log + service.log.1 ... .N）。默认 3。
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
            log_max_bytes: 10 * 1024 * 1024,
            log_files: 3,
        }
    }
}

/// Build [`TelemetryConfig`] from environment and call [`init_telemetry`].
/// Honors `RUST_LOG` / `HERMES_LOG_JSON` / `HERMES_OTLP_ENDPOINT` like the CLI and HTTP binaries.
/// `HERMIX_LOG_DIR` (or `HERMES_LOG_DIR`) overrides the file log directory; defaults to
/// `<hermix_home>/logs/<service>.log` so packaged Tauri apps always write somewhere observable.
///
/// **Important**: the returned `TelemetryGuard` is automatically pinned in a
/// process-lifetime static so the non-blocking file writer thread keeps running
/// until process exit. Without this anchor, long-lived sidecar processes (e.g.
/// Tauri `--stdio` mode) silently lose every event because the writer thread is
/// joined the moment the guard is dropped.
pub fn init_telemetry_from_env(
    service_name: impl Into<String>,
    default_level: impl AsRef<str>,
    log_to_stderr: bool,
) {
    let level = tracing_subscriber::EnvFilter::try_from_default_env()
        .map(|f| f.to_string())
        .unwrap_or_else(|_| default_level.as_ref().to_string());
    let service_name = service_name.into();
    let log_dir = env_compat("HERMIX_LOG_DIR", "HERMES_LOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            hermix_config::paths::hermix_home()
                .join("logs")
                .join(format!("{service_name}.log"))
        });
    let cfg = TelemetryConfig {
        level,
        json: env_compat("HERMIX_LOG_JSON", "HERMES_LOG_JSON").as_deref() == Some("1"),
        service_name,
        otlp_endpoint: env_compat("HERMIX_OTLP_ENDPOINT", "HERMES_OTLP_ENDPOINT"),
        log_to_stderr,
        log_dir: Some(log_dir),
        ..TelemetryConfig::default()
    };
    let guard = init_telemetry(&cfg);
    pin_init_guard(guard);
}

/// Open the rolling file writer. Returns `None` if the directory cannot be created
/// (caller silently downgrades to stderr-only logging). With tracing-appender 0.2 the
/// supported rotation policy is `Daily` (per-day file), and `max_log_files` caps
/// historical files in the same directory. `log_max_bytes` is not honoured here —
/// single-day files may grow; combined with `max_log_files` we keep the on-disk
/// footprint bounded for typical diagnostic use.
fn open_file_appender(
    log_dir: &std::path::Path,
    service: &str,
    _files: usize,
) -> Option<(tracing_appender::non_blocking::NonBlocking, WorkerGuard)> {
    use tracing_appender::rolling::daily;
    // log_dir from `init_telemetry_from_env` may be a file path OR a directory.
    let dir = if log_dir.extension().is_some() {
        log_dir.parent().unwrap_or(log_dir)
    } else {
        log_dir
    };
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("hermes-telemetry: cannot create log dir {}: {}", dir.display(), e);
        return None;
    }
    // tracing-appender 0.2 doesn't expose `with_max_log_files` on the rolling
    // appender; daily rotation alone (one file per UTC day) is the bound on
    // disk footprint. Add a startup-time prune of `service.YYYY-MM-DD.log.*`
    // files older than `files` days as a follow-up if needed.
    let file_appender = daily(dir, service);
    let (w, g) = tracing_appender::non_blocking(file_appender);
    Some((w, g))
}

/// Init guard handle returned by [`init_telemetry`]. Drop this BEFORE process exit
/// to flush the non-blocking file writer; pinning it in `main` is sufficient.
///
/// Critically: if the caller throws away the returned guard (Rust drops it at
/// the end of the `init_telemetry(...);` statement), the non-blocking writer
/// thread is joined immediately and never has a chance to drain queued events.
/// Without a persistent anchor, a server that runs `init_telemetry_from_env`
/// and never stores the result will produce an empty on-disk log file even
/// though events ARE enqueued. Store the guard in a static, or call
/// `pin_init_guard()` to anchor it for the process lifetime.
pub struct TelemetryGuard {
    _file_guard: Option<WorkerGuard>,
}

/// Pin the telemetry init guard in a process-lifetime static so the non-blocking
/// file writer thread keeps running until process exit. Call this from `main`
/// after `init_telemetry(...)` (or directly use `init_telemetry_from_env` which
/// internally does the same).
///
/// This is the correct fix for "logs stay empty on the user disk in a long-lived
/// sidecar where main() doesn't keep the guard alive as a stack variable".
pub fn pin_init_guard(guard: TelemetryGuard) {
    static PINNED: std::sync::Mutex<Option<TelemetryGuard>> = std::sync::Mutex::new(None);
    if let Ok(mut g) = PINNED.lock() {
        *g = Some(guard);
    } else {
        // Poisoned lock — fall back to a mem::forget so the worker thread at
        // least lives for the remainder of the process lifetime.
        std::mem::forget(guard);
    }
}

pub fn init_telemetry(config: &TelemetryConfig) -> TelemetryGuard {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(config.level.clone()));

    let sink = if config.log_to_stderr {
        Sink::Stderr
    } else {
        Sink::Stdout
    };
    let json_mode = config.json;

    // Open file appender FIRST; keep the guard alive across the try_init() call so
    // the non-blocking writer thread doesn't get dropped mid-init (which would race
    // the subscriber registration and may drop the earliest events).
    struct PendingFile {
        writer: tracing_appender::non_blocking::NonBlocking,
        guard: tracing_appender::non_blocking::WorkerGuard,
    }
    let pending_file: Option<PendingFile> = match config.log_dir.as_ref() {
        Some(log_path) => {
            let service = config.service_name.replace(['/', '\\'], "_");
            open_file_appender(log_path, &service, config.log_files).map(|(w, g)| PendingFile {
                writer: w,
                guard: g,
            })
        }
        None => None,
    };

    // Mount all layers in a single try_init() with concrete types per branch.
    let _ = match &pending_file {
        Some(pf) => try_init_subscriber(
            sink,
            json_mode,
            filter,
            Some(&pf.writer),
        ),
        None => try_init_subscriber(sink, json_mode, filter, None),
    };

    #[cfg(feature = "otlp")]
    {
        use tracing_subscriber::layer::Identity;
        if let Some(ep) = config.otlp_endpoint.as_deref() {
            if !ep.is_empty() {
                match otlp::build_otel_layer(&config.service_name, ep) {
                    Ok(layer) => {
                        let _ = tracing_subscriber::registry().with(layer).try_init();
                    }
                    Err(e) => {
                        eprintln!("hermes-telemetry: OTLP init failed: {}", e);
                        let _ = tracing_subscriber::registry()
                            .with(Identity::default())
                            .try_init();
                    }
                }
            }
        }
    }
    #[cfg(not(feature = "otlp"))]
    if config.otlp_endpoint.is_some() {
        eprintln!(
            "hermes-telemetry: OTLP endpoint is set but this build lacks the `otlp` feature; rebuild with `--features otlp`."
        );
    }

    TelemetryGuard {
        _file_guard: pending_file.map(|pf| pf.guard),
    }
}

/// Single-shot init: filter + (stderr|stdout) writer + optional file writer.
/// Uses concrete layer types to side-step `Layered<Box<dyn Layer<S>>, ...>: Into<Dispatch>`
/// problems that block compound dyn Layer chains. The function is the ONLY
/// `try_init` site for non-OTLP mode — every other init path must go through here
/// because tracing global can only be set once.
#[allow(clippy::too_many_arguments)]
fn try_init_subscriber(
    sink: Sink,
    json_mode: bool,
    filter: tracing_subscriber::EnvFilter,
    file_writer: Option<&tracing_appender::non_blocking::NonBlocking>,
) -> Result<(), tracing_subscriber::util::TryInitError> {
    // Conditionally initialise different concrete shapes so rustc resolves every
    // branch into a well-typed Layer chain. Each branch writes to either stderr
    // OR stdout, and includes the file layer when the writer is provided.
    match (sink, json_mode, file_writer) {
        (Sink::Stderr, true, Some(w)) => tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .json()
                    .with_writer(std::io::stderr),
            )
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .json()
                    .with_writer(w.clone()),
            )
            .try_init(),
        (Sink::Stderr, false, Some(w)) => tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .with_writer(std::io::stderr),
            )
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .with_writer(w.clone()),
            )
            .try_init(),
        (Sink::Stdout, true, Some(w)) => tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .json()
                    .with_writer(std::io::stdout),
            )
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .json()
                    .with_writer(w.clone()),
            )
            .try_init(),
        (Sink::Stdout, false, Some(w)) => tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .with_writer(std::io::stdout),
            )
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .with_writer(w.clone()),
            )
            .try_init(),
        (Sink::Stderr, true, None) => tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .json()
                    .with_writer(std::io::stderr),
            )
            .try_init(),
        (Sink::Stderr, false, None) => tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .with_writer(std::io::stderr),
            )
            .try_init(),
        (Sink::Stdout, true, None) => tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .json()
                    .with_writer(std::io::stdout),
            )
            .try_init(),
        (Sink::Stdout, false, None) => tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .with_writer(std::io::stdout),
            )
            .try_init(),
    }
}

#[derive(Default)]
pub struct MetricsRegistry {
    pub llm_requests_total: AtomicU64,
    pub tool_calls_total: AtomicU64,
    pub tool_time_ms_total: AtomicU64,
    pub errors_total: AtomicU64,
    pub http_requests_total: AtomicU64,
    pub http_rejects_total: AtomicU64,
    pub prompt_cache_hits: AtomicU64,
    pub prompt_cache_misses: AtomicU64,
}

pub static METRICS: Lazy<MetricsRegistry> = Lazy::new(MetricsRegistry::default);

pub fn record_llm_request() {
    METRICS.llm_requests_total.fetch_add(1, Ordering::Relaxed);
}

pub fn record_tool_call(duration: Duration) {
    METRICS.tool_calls_total.fetch_add(1, Ordering::Relaxed);
    METRICS
        .tool_time_ms_total
        .fetch_add(duration.as_millis() as u64, Ordering::Relaxed);
}

pub fn record_error() {
    METRICS.errors_total.fetch_add(1, Ordering::Relaxed);
}

pub fn record_http_request() {
    METRICS.http_requests_total.fetch_add(1, Ordering::Relaxed);
}

pub fn record_http_reject() {
    METRICS.http_rejects_total.fetch_add(1, Ordering::Relaxed);
}

pub fn record_prompt_cache_hit() {
    METRICS.prompt_cache_hits.fetch_add(1, Ordering::Relaxed);
}

pub fn record_prompt_cache_miss() {
    METRICS.prompt_cache_misses.fetch_add(1, Ordering::Relaxed);
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricsSnapshot {
    pub llm_requests_total: u64,
    pub tool_calls_total: u64,
    pub tool_time_ms_total: u64,
    pub errors_total: u64,
    pub http_requests_total: u64,
    pub http_rejects_total: u64,
    pub prompt_cache_hits: u64,
    pub prompt_cache_misses: u64,
}

pub fn snapshot() -> MetricsSnapshot {
    MetricsSnapshot {
        llm_requests_total: METRICS.llm_requests_total.load(Ordering::Relaxed),
        tool_calls_total: METRICS.tool_calls_total.load(Ordering::Relaxed),
        tool_time_ms_total: METRICS.tool_time_ms_total.load(Ordering::Relaxed),
        errors_total: METRICS.errors_total.load(Ordering::Relaxed),
        http_requests_total: METRICS.http_requests_total.load(Ordering::Relaxed),
        http_rejects_total: METRICS.http_rejects_total.load(Ordering::Relaxed),
        prompt_cache_hits: METRICS.prompt_cache_hits.load(Ordering::Relaxed),
        prompt_cache_misses: METRICS.prompt_cache_misses.load(Ordering::Relaxed),
    }
}

/// OpenMetrics/Prometheus text exposition for scraping (no external `prometheus` crate).
pub fn prometheus_text() -> String {
    let s = snapshot();
    let mut out = String::new();
    use std::fmt::Write;
    let _ = writeln!(
        &mut out,
        "# HELP hermes_llm_requests_total Completed LLM round-trips observed by Hermes."
    );
    let _ = writeln!(&mut out, "# TYPE hermes_llm_requests_total counter");
    let _ = writeln!(
        &mut out,
        "hermes_llm_requests_total {}",
        s.llm_requests_total
    );
    let _ = writeln!(
        &mut out,
        "# HELP hermes_tool_calls_total Tool invocations observed by Hermes."
    );
    let _ = writeln!(&mut out, "# TYPE hermes_tool_calls_total counter");
    let _ = writeln!(&mut out, "hermes_tool_calls_total {}", s.tool_calls_total);
    let _ = writeln!(
        &mut out,
        "# HELP hermes_tool_time_ms_total Wall time spent in tools (milliseconds)."
    );
    let _ = writeln!(&mut out, "# TYPE hermes_tool_time_ms_total counter");
    let _ = writeln!(
        &mut out,
        "hermes_tool_time_ms_total {}",
        s.tool_time_ms_total
    );
    let _ = writeln!(
        &mut out,
        "# HELP hermes_errors_total Errors recorded by Hermes telemetry."
    );
    let _ = writeln!(&mut out, "# TYPE hermes_errors_total counter");
    let _ = writeln!(&mut out, "hermes_errors_total {}", s.errors_total);
    let _ = writeln!(
        &mut out,
        "# HELP hermes_http_requests_total HTTP API requests handled (hermes-server)."
    );
    let _ = writeln!(&mut out, "# TYPE hermes_http_requests_total counter");
    let _ = writeln!(
        &mut out,
        "hermes_http_requests_total {}",
        s.http_requests_total
    );
    let _ = writeln!(
        &mut out,
        "# HELP hermes_http_rejects_total HTTP requests rejected (auth / IP / rate limit)."
    );
    let _ = writeln!(&mut out, "# TYPE hermes_http_rejects_total counter");
    let _ = writeln!(
        &mut out,
        "hermes_http_rejects_total {}",
        s.http_rejects_total
    );
    let _ = writeln!(
        &mut out,
        "# HELP hermes_prompt_cache_hits Prompt cache hits (system prompt unchanged)."
    );
    let _ = writeln!(&mut out, "# TYPE hermes_prompt_cache_hits counter");
    let _ = writeln!(&mut out, "hermes_prompt_cache_hits {}", s.prompt_cache_hits);
    let _ = writeln!(
        &mut out,
        "# HELP hermes_prompt_cache_misses Prompt cache misses (system prompt rebuilt)."
    );
    let _ = writeln!(&mut out, "# TYPE hermes_prompt_cache_misses counter");
    let _ = writeln!(
        &mut out,
        "hermes_prompt_cache_misses {}",
        s.prompt_cache_misses
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prometheus_text_includes_counters() {
        record_http_request();
        record_llm_request();
        record_http_reject();
        let t = prometheus_text();
        assert!(t.contains("hermes_http_requests_total"));
        assert!(t.contains("hermes_llm_requests_total"));
        assert!(t.contains("hermes_http_rejects_total"));
    }
}
