//! Telemetry bootstrap and in-process metrics registry.
//!
//! ## Log categories
//!
//! Events are routed to category-specific log files based on their `target`:
//! - `hermix.crash` / `panic` → `crash.log`
//! - `hermix.agent` / `llm` / `memory` → `agent.log`
//! - `hermix.tools` / `approval` / `tool` → `tools.log`
//! - everything else → `server.log`
//!
//! Each category file rotates by size (default 10 MiB) and keeps up to 5
//! rotated copies. A background task prunes files older than 7 days.

use std::sync::atomic::{AtomicU64, Ordering};
use hermix_config::env_compat;
use std::path::PathBuf;
use std::time::Duration;

use once_cell::sync::Lazy;
use serde::Serialize;

mod log_manager;
mod run_trace;

#[cfg(feature = "otlp")]
mod otlp;
#[cfg(not(feature = "otlp"))]
mod otlp {
    use tracing_subscriber::layer::Layer;
    use tracing_subscriber::Registry;
    /// Stub when the `otlp` feature is disabled — never actually invoked because
    /// the OTLP block is feature-gated in `log_manager::init_unified_telemetry`.
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

// ---------------------------------------------------------------------------
// Re-export log manager API (categories, rotation, reading, config)
// ---------------------------------------------------------------------------

pub use log_manager::{
    get_log_dir, init_unified_telemetry, list_log_files, log_disk_usage, read_log_range,
    read_log_tail, set_log_dir, LogCategory, LogFileInfo, TelemetryConfig, TelemetryGuard,
};

// ---------------------------------------------------------------------------
// init_telemetry_from_env — delegates to log_manager
// ---------------------------------------------------------------------------

/// Build [`TelemetryConfig`] from environment and call the unified telemetry init.
/// Honors `RUST_LOG` / `HERMES_LOG_JSON` / `HERMES_OTLP_ENDPOINT`.
/// `HERMIX_LOG_DIR` (or `HERMES_LOG_DIR`) overrides the log directory;
/// defaults to `<hermix_home>/logs/` (a directory, not a file path).
///
/// The `TelemetryGuard` is automatically pinned in a process-lifetime static
/// so background cleanup tasks keep running until process exit.
pub fn init_telemetry_from_env(
    service_name: impl Into<String>,
    default_level: impl AsRef<str>,
    log_to_stderr: bool,
) {
    let level = tracing_subscriber::EnvFilter::try_from_default_env()
        .map(|f| f.to_string())
        .unwrap_or_else(|_| default_level.as_ref().to_string());
    let service_name = service_name.into();
    // Default log_dir is the logs directory (not a file path —
    // each category gets its own <category>.log file).
    let log_dir = env_compat("HERMIX_LOG_DIR", "HERMES_LOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| hermix_config::paths::hermix_home().join("logs"));

    let cfg = TelemetryConfig {
        level,
        json: env_compat("HERMIX_LOG_JSON", "HERMES_LOG_JSON").as_deref() == Some("1"),
        service_name,
        otlp_endpoint: env_compat("HERMIX_OTLP_ENDPOINT", "HERMES_OTLP_ENDPOINT"),
        log_to_stderr,
        log_dir: Some(log_dir),
        ..TelemetryConfig::default()
    };

    let guard = log_manager::init_unified_telemetry(&cfg);
    log_manager::pin_init_guard(guard);
}

// ---------------------------------------------------------------------------
// Metrics (unchanged — not related to logging)
// ---------------------------------------------------------------------------

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
    let _ = writeln!(&mut out, "hermes_llm_requests_total {}", s.llm_requests_total);
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
    let _ = writeln!(&mut out, "hermes_tool_time_ms_total {}", s.tool_time_ms_total);
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
    let _ = writeln!(&mut out, "hermes_http_requests_total {}", s.http_requests_total);
    let _ = writeln!(
        &mut out,
        "# HELP hermes_http_rejects_total HTTP requests rejected (auth / IP / rate limit)."
    );
    let _ = writeln!(&mut out, "# TYPE hermes_http_rejects_total counter");
    let _ = writeln!(&mut out, "hermes_http_rejects_total {}", s.http_rejects_total);
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
    let _ = writeln!(&mut out, "hermes_prompt_cache_misses {}", s.prompt_cache_misses);
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
