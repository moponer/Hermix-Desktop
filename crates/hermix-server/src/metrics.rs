//! Local metric system for observability.
//!
//! (P2-1 / STABILITY-PLAN §2.8) Lightweight metric collection that
//! writes to SQLite in batches (10s flush). No external dependencies
//! (Prometheus server not required).
//!
//! Tracked metrics:
//! - WS disconnect count + reasons
//! - Active task count
//! - Tool dispatch latency (p50/p99)
//! - Memory usage (process RSS)
//! - SQLite pool stats
//!
//! Design constraints (STABILITY-PLAN §4 F6):
//! - Metric writes are async batch — never block the main flow.
//! - Metric system failure is logged but does not crash the server.
//! - Query API is read-only and cheap.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

use chrono::Utc;
use serde::{Deserialize, Serialize};

/// A single metric event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricEvent {
    pub timestamp: String,
    pub metric_type: String,
    pub value: f64,
    pub label: Option<String>,
}

/// In-memory metric collector with batch flush to SQLite.
#[derive(Clone)]
pub struct MetricsCollector {
    inner: Arc<Mutex<MetricsInner>>,
}

struct MetricsInner {
    /// Buffered events waiting for next flush.
    buffer: Vec<MetricEvent>,
    /// Cumulative counters (never reset, monotonic).
    counters: HashMap<String, f64>,
    /// Latency samples for percentile calculation.
    latency_samples: HashMap<String, Vec<f64>>,
    /// Path to the SQLite DB file.
    db_path: std::path::PathBuf,
}

impl MetricsCollector {
    /// Create a new collector backed by `hermix_home/metrics.db`.
    pub fn new(hermix_home: &Path) -> Self {
        let db_path = hermix_home.join("metrics.db");
        let collector = Self {
            inner: Arc::new(Mutex::new(MetricsInner {
                buffer: Vec::new(),
                counters: HashMap::new(),
                latency_samples: HashMap::new(),
                db_path,
            })),
        };
        collector.ensure_db();
        collector
    }

    /// Increment a counter metric.
    pub fn incr(&self, metric_type: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            *inner.counters.entry(metric_type.to_string()).or_insert(0.0) += 1.0;
            inner.buffer.push(MetricEvent {
                timestamp: Utc::now().to_rfc3339(),
                metric_type: metric_type.to_string(),
                value: 1.0,
                label: None,
            });
        }
    }

    /// Increment a counter with a label (e.g., disconnect reason).
    pub fn incr_with_label(&self, metric_type: &str, label: &str) {
        let key = format!("{metric_type}:{label}");
        if let Ok(mut inner) = self.inner.lock() {
            *inner.counters.entry(key).or_insert(0.0) += 1.0;
            inner.buffer.push(MetricEvent {
                timestamp: Utc::now().to_rfc3339(),
                metric_type: metric_type.to_string(),
                value: 1.0,
                label: Some(label.to_string()),
            });
        }
    }

    /// Record a latency sample (in milliseconds).
    pub fn record_latency(&self, metric_type: &str, ms: f64) {
        if let Ok(mut inner) = self.inner.lock() {
            inner
                .latency_samples
                .entry(metric_type.to_string())
                .or_default()
                .push(ms);
            inner.buffer.push(MetricEvent {
                timestamp: Utc::now().to_rfc3339(),
                metric_type: metric_type.to_string(),
                value: ms,
                label: None,
            });
        }
    }

    /// Set a gauge value (e.g., current task count, memory).
    pub fn set_gauge(&self, metric_type: &str, value: f64) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.counters.insert(metric_type.to_string(), value);
            inner.buffer.push(MetricEvent {
                timestamp: Utc::now().to_rfc3339(),
                metric_type: metric_type.to_string(),
                value,
                label: None,
            });
        }
    }

    /// Flush buffered events to SQLite. Called periodically (every 10s).
    /// Best-effort — logs errors but does not panic.
    pub fn flush(&self) {
        let events = {
            let Ok(mut inner) = self.inner.lock() else {
                return;
            };
            std::mem::take(&mut inner.buffer)
        };
        if events.is_empty() {
            return;
        }
        if let Err(e) = self.write_to_db(&events) {
            tracing::warn!(error = %e, "metrics: failed to flush to SQLite — events lost");
        }
    }

    /// Get a snapshot of current metric values for dashboard display.
    pub fn snapshot(&self) -> MetricsSnapshot {
        let Ok(inner) = self.inner.lock() else {
            return MetricsSnapshot::default();
        };
        let counters = inner.counters.clone();

        // Compute p50/p99 for latency metrics.
        let mut latencies = HashMap::new();
        for (key, samples) in &inner.latency_samples {
            if samples.is_empty() {
                continue;
            }
            let mut sorted = samples.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let p50 = sorted[sorted.len() / 2];
            let p99 = sorted[(sorted.len() as f64 * 0.99) as usize];
            latencies.insert(
                key.clone(),
                LatencyStats {
                    count: sorted.len() as u64,
                    p50_ms: p50,
                    p99_ms: p99,
                },
            );
        }

        MetricsSnapshot {
            counters,
            latencies,
            buffer_len: inner.buffer.len(),
        }
    }

    fn ensure_db(&self) {
        let Ok(inner) = self.inner.lock() else {
            return;
        };
        let conn = match rusqlite::Connection::open(&inner.db_path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "metrics: failed to open DB");
                return;
            }
        };
        let _ = conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS metric_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL,
                metric_type TEXT NOT NULL,
                value REAL NOT NULL,
                label TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_metric_type ON metric_events(metric_type);
            CREATE INDEX IF NOT EXISTS idx_timestamp ON metric_events(timestamp);",
        );
    }

    fn write_to_db(&self, events: &[MetricEvent]) -> Result<(), rusqlite::Error> {
        let Ok(inner) = self.inner.lock() else {
            return Ok(());
        };
        let mut conn = rusqlite::Connection::open(&inner.db_path)?;
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO metric_events (timestamp, metric_type, value, label) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for e in events {
                stmt.execute(rusqlite::params![
                    &e.timestamp,
                    &e.metric_type,
                    e.value,
                    e.label.as_deref(),
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

/// Snapshot of current metric state for dashboard display.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    pub counters: HashMap<String, f64>,
    pub latencies: HashMap<String, LatencyStats>,
    pub buffer_len: usize,
}

/// Percentile stats for a latency metric.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct LatencyStats {
    pub count: u64,
    pub p50_ms: f64,
    pub p99_ms: f64,
}

/// A scoped latency timer. Records the elapsed time when dropped.
pub struct LatencyTimer {
    collector: MetricsCollector,
    metric_type: String,
    start: Instant,
}

impl LatencyTimer {
    pub fn new(collector: &MetricsCollector, metric_type: &str) -> Self {
        Self {
            collector: collector.clone(),
            metric_type: metric_type.to_string(),
            start: Instant::now(),
        }
    }
}

impl Drop for LatencyTimer {
    fn drop(&mut self) {
        let elapsed_ms = self.start.elapsed().as_secs_f64() * 1000.0;
        self.collector.record_latency(&self.metric_type, elapsed_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn incr_increments_counter() {
        let tmp = TempDir::new().unwrap();
        let m = MetricsCollector::new(tmp.path());
        m.incr("ws_disconnect");
        m.incr("ws_disconnect");
        let snap = m.snapshot();
        assert_eq!(snap.counters.get("ws_disconnect"), Some(&2.0));
    }

    #[test]
    fn incr_with_label_creates_separate_keys() {
        let tmp = TempDir::new().unwrap();
        let m = MetricsCollector::new(tmp.path());
        m.incr_with_label("ws_disconnect", "heartbeat_timeout");
        m.incr_with_label("ws_disconnect", "client_close");
        m.incr_with_label("ws_disconnect", "heartbeat_timeout");
        let snap = m.snapshot();
        assert_eq!(
            snap.counters.get("ws_disconnect:heartbeat_timeout"),
            Some(&2.0)
        );
        assert_eq!(snap.counters.get("ws_disconnect:client_close"), Some(&1.0));
    }

    #[test]
    fn record_latency_stores_samples() {
        let tmp = TempDir::new().unwrap();
        let m = MetricsCollector::new(tmp.path());
        m.record_latency("tool_dispatch", 10.0);
        m.record_latency("tool_dispatch", 20.0);
        m.record_latency("tool_dispatch", 30.0);
        let snap = m.snapshot();
        let stats = snap.latencies.get("tool_dispatch").unwrap();
        assert_eq!(stats.count, 3);
        assert_eq!(stats.p50_ms, 20.0); // median of [10, 20, 30]
    }

    #[test]
    fn flush_writes_to_db() {
        let tmp = TempDir::new().unwrap();
        let m = MetricsCollector::new(tmp.path());
        m.incr("test_metric");
        m.incr("test_metric");
        m.flush();
        // Buffer should be empty after flush.
        let snap = m.snapshot();
        assert_eq!(snap.buffer_len, 0);
    }

    #[test]
    fn set_gauge_overwrites_previous() {
        let tmp = TempDir::new().unwrap();
        let m = MetricsCollector::new(tmp.path());
        m.set_gauge("active_tasks", 5.0);
        m.set_gauge("active_tasks", 3.0);
        let snap = m.snapshot();
        assert_eq!(snap.counters.get("active_tasks"), Some(&3.0));
    }

    #[test]
    fn latency_timer_records_on_drop() {
        let tmp = TempDir::new().unwrap();
        let m = MetricsCollector::new(tmp.path());
        {
            let _timer = LatencyTimer::new(&m, "test_op");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let snap = m.snapshot();
        let stats = snap.latencies.get("test_op").unwrap();
        assert!(stats.p50_ms > 0.0, "latency should be positive");
    }
}
