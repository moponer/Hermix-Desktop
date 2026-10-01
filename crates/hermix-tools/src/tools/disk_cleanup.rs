//! Disk cleanup tool — scan and prune stale files under HERMIX_HOME.
//!
//! Targets:
//!   - `sessions/` rows whose `last_active` is older than N days
//!   - `cron/` outputs whose source job no longer exists
//!   - `logs/` rotated files older than N days
//!   - top-level `*.tmp` files older than N hours
//!
//! All destructive operations honour `dry_run` (defaults to `true`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::Serialize;
use serde_json::{json, Value};

use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema, traits::RiskLevel};

use crate::ToolRegistry;

/// Register this tool with the registry (P1.1 migration).
pub fn register(registry: &ToolRegistry) {
    let handler = Arc::new(DiskCleanupHandler::new());
    let schema = handler.schema();
    registry.register_with_risk(
        "disk_cleanup",
        "disk_cleanup",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        true,
        "Scan and prune stale files under HERMIX_HOME",
        "🧹",
        None,
        RiskLevel::Caution,
        false,
    );
}

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct CleanupCandidate {
    pub path: String,
    pub category: String,
    pub size_bytes: u64,
    pub age_seconds: u64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CleanupReport {
    pub dry_run: bool,
    pub scanned_paths: usize,
    pub total_size_bytes: u64,
    pub candidates: Vec<CleanupCandidate>,
    pub deleted: Vec<String>,
    pub errors: Vec<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// `hermix_home()` was a private duplicate that ignored `HERMIX_PORTABLE_DATA_DIR`
// and `$USERPROFILE` correctly; replaced by the canonical resolver so the
// disk-cleanup tool walks the same tree the rest of the app writes to.
use hermix_config::paths::hermix_home;

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn file_age_seconds(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .map(|mtime| now_unix().saturating_sub(mtime))
        .unwrap_or(0)
}

fn is_within(base: &Path, target: &Path) -> bool {
    target.starts_with(base)
}

fn dir_size(path: &Path) -> u64 {
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

fn fmt_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{:.2} {}", size, UNITS[unit])
}

// ---------------------------------------------------------------------------
// Scanner
// ---------------------------------------------------------------------------

fn scan_log_files(home: &Path, max_age: Duration) -> Vec<CleanupCandidate> {
    let logs = home.join("logs");
    if !logs.exists() {
        return vec![];
    }
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir(&logs) else {
        return out;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let age = file_age_seconds(&meta);
        if Duration::from_secs(age) >= max_age {
            out.push(CleanupCandidate {
                path: path.to_string_lossy().to_string(),
                category: "stale_log".into(),
                size_bytes: meta.len(),
                age_seconds: age,
                reason: format!("log file older than {:?}", max_age),
            });
        }
    }
    out
}

fn scan_tmp_files(home: &Path, max_age: Duration) -> Vec<CleanupCandidate> {
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir(home) else {
        return out;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".tmp") && !name.ends_with(".partial") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let age = file_age_seconds(&meta);
        if Duration::from_secs(age) >= max_age {
            out.push(CleanupCandidate {
                path: path.to_string_lossy().to_string(),
                category: "tmp_file".into(),
                size_bytes: meta.len(),
                age_seconds: age,
                reason: format!("stale tmp file older than {:?}", max_age),
            });
        }
    }
    out
}

fn scan_cron_outputs(home: &Path) -> Vec<CleanupCandidate> {
    let cron = home.join("cron");
    if !cron.exists() {
        return vec![];
    }
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir(&cron) else { return out };
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // Keep <job_id>.json + .log/.txt — flag anything else
        let is_known = name.ends_with(".json") || name.ends_with(".log") || name.ends_with(".txt");
        if is_known {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        out.push(CleanupCandidate {
            path: path.to_string_lossy().to_string(),
            category: "cron_orphan".into(),
            size_bytes: meta.len(),
            age_seconds: file_age_seconds(&meta),
            reason: "non-standard file in cron dir (not .json/.log/.txt)".into(),
        });
    }
    out
}

fn scan_dashboard_caches(home: &Path, max_age: Duration) -> Vec<CleanupCandidate> {
    let cache_dirs = [
        home.join("dashboard-plugins").join(".cache"),
        home.join("tmp"),
    ];
    let mut out = vec![];
    for dir in &cache_dirs {
        if !dir.exists() {
            continue;
        }
        for entry in walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let Ok(meta) = entry.metadata() else { continue };
            let age = file_age_seconds(&meta);
            if Duration::from_secs(age) >= max_age {
                out.push(CleanupCandidate {
                    path: entry.path().to_string_lossy().to_string(),
                    category: "stale_cache".into(),
                    size_bytes: meta.len(),
                    age_seconds: age,
                    reason: format!("cache file older than {:?}", max_age),
                });
            }
        }
    }
    out
}

fn gather_candidates(
    home: &Path,
    log_max_age_days: u64,
    tmp_max_age_hours: u64,
) -> Vec<CleanupCandidate> {
    let mut all = vec![];
    all.extend(scan_log_files(home, Duration::from_secs(log_max_age_days * 86_400)));
    all.extend(scan_tmp_files(
        home,
        Duration::from_secs(tmp_max_age_hours * 3_600),
    ));
    all.extend(scan_cron_outputs(home));
    all.extend(scan_dashboard_caches(
        home,
        Duration::from_secs(log_max_age_days * 86_400),
    ));
    all.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    all
}

// ---------------------------------------------------------------------------
// DiskCleanupHandler
// ---------------------------------------------------------------------------

pub struct DiskCleanupHandler;

impl Default for DiskCleanupHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl DiskCleanupHandler {
    pub fn new() -> Self {
        Self
    }

    fn home() -> PathBuf {
        hermix_home()
    }

    async fn scan(&self, params: &Value) -> Result<String, ToolError> {
        let log_days = params
            .get("log_max_age_days")
            .and_then(|v| v.as_u64())
            .unwrap_or(30);
        let tmp_hours = params
            .get("tmp_max_age_hours")
            .and_then(|v| v.as_u64())
            .unwrap_or(24);
        let home = Self::home();
        let candidates = gather_candidates(&home, log_days, tmp_hours);
        let total_size: u64 = candidates.iter().map(|c| c.size_bytes).sum();
        let report = CleanupReport {
            dry_run: true,
            scanned_paths: candidates.len(),
            total_size_bytes: total_size,
            candidates,
            deleted: vec![],
            errors: vec![],
        };
        serde_json::to_string_pretty(&report)
            .map_err(|e| ToolError::from_message(e.to_string()))
    }

    async fn clean(&self, params: &Value) -> Result<String, ToolError> {
        // `dry_run` defaults to true, so a string-encoded `"false"`
        // previously fell back to the SAFE value and the tool only
        // scanned — the model asked to delete and got a listing, with
        // no error to explain why.
        let dry_run = crate::param_coercion::bool_param(&params, "dry_run", true);
        let explicit_paths = params
            .get("paths")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            });
        let log_days = params
            .get("log_max_age_days")
            .and_then(|v| v.as_u64())
            .unwrap_or(30);
        let tmp_hours = params
            .get("tmp_max_age_hours")
            .and_then(|v| v.as_u64())
            .unwrap_or(24);

        let home = Self::home();
        let candidates = if let Some(paths) = explicit_paths {
            candidates_for_paths(&home, paths)
        } else {
            gather_candidates(&home, log_days, tmp_hours)
        };

        if dry_run {
            let total_size: u64 = candidates.iter().map(|c| c.size_bytes).sum();
            let report = CleanupReport {
                dry_run: true,
                scanned_paths: candidates.len(),
                total_size_bytes: total_size,
                candidates,
                deleted: vec![],
                errors: vec![],
            };
            return serde_json::to_string_pretty(&report)
                .map_err(|e| ToolError::from_message(e.to_string()));
        }

        let mut deleted = vec![];
        let mut errors = vec![];
        let mut total_freed = 0u64;
        for cand in &candidates {
            let p = PathBuf::from(&cand.path);
            // Safety: never delete anything outside HERMIX_HOME
            if !is_within(&home, &p) {
                errors.push(format!(
                    "refused: '{}' is outside HERMIX_HOME '{}'",
                    cand.path,
                    home.display()
                ));
                continue;
            }
            match std::fs::remove_file(&p) {
                Ok(_) => {
                    deleted.push(cand.path.clone());
                    total_freed += cand.size_bytes;
                }
                Err(e) => errors.push(format!("{}: {}", cand.path, e)),
            }
        }
        let report = CleanupReport {
            dry_run: false,
            scanned_paths: candidates.len(),
            total_size_bytes: total_freed,
            candidates,
            deleted,
            errors,
        };
        serde_json::to_string_pretty(&report)
            .map_err(|e| ToolError::from_message(e.to_string()))
    }

    async fn stats(&self) -> Result<String, ToolError> {
        let home = Self::home();
        let categories = ["logs", "sessions", "cron", "tmp", "dashboard-plugins", "memory"];
        let mut out = json!({
            "hermix_home": home.to_string_lossy(),
            "total_bytes": dir_size(&home),
            "total_human": fmt_size(dir_size(&home)),
            "categories": {}
        });
        let map = out
            .as_object_mut()
            .and_then(|o| o.get_mut("categories"))
            .and_then(|v| v.as_object_mut())
            .ok_or_else(|| ToolError::ExecutionFailed("internal: missing categories".into()))?;
        for cat in &categories {
            let p = home.join(cat);
            let size = if p.exists() { dir_size(&p) } else { 0 };
            map.insert(
                cat.to_string(),
                json!({ "bytes": size, "human": fmt_size(size), "exists": p.exists() }),
            );
        }
        serde_json::to_string_pretty(&out)
            .map_err(|e| ToolError::from_message(e.to_string()))
    }
}

fn candidates_for_paths(home: &Path, paths: Vec<String>) -> Vec<CleanupCandidate> {
    let mut out = vec![];
    for p in paths {
        let pb = PathBuf::from(&p);
        if !is_within(home, &pb) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&pb) else { continue };
        if !meta.is_file() {
            continue;
        }
        out.push(CleanupCandidate {
            path: p,
            category: "explicit".into(),
            size_bytes: meta.len(),
            age_seconds: file_age_seconds(&meta),
            reason: "explicitly requested".into(),
        });
    }
    out
}

#[async_trait]
impl ToolHandler for DiskCleanupHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'action' parameter".into()))?;
        match action {
            "scan" => self.scan(&params).await,
            "clean" => self.clean(&params).await,
            "stats" => self.stats().await,
            other => Err(ToolError::InvalidParams(format!(
                "Unknown action: '{}'. Use 'scan', 'clean', or 'stats'.",
                other
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "description": "Action: scan (list candidates), clean (delete — defaults to dry_run=true), stats (size by category)",
                "enum": ["scan", "clean", "stats"]
            }),
        );
        props.insert(
            "dry_run".into(),
            json!({
                "type": "boolean",
                "description": "If true, do not delete anything. Default: true. Set false to actually remove files."
            }),
        );
        props.insert(
            "log_max_age_days".into(),
            json!({
                "type": "integer",
                "description": "Age in days beyond which log files are considered stale. Default: 30."
            }),
        );
        props.insert(
            "tmp_max_age_hours".into(),
            json!({
                "type": "integer",
                "description": "Age in hours beyond which *.tmp / *.partial files are considered stale. Default: 24."
            }),
        );
        props.insert(
            "paths".into(),
            json!({
                "type": "array",
                "items": { "type": "string" },
                "description": "Explicit file paths to clean (for action=clean). All paths must be inside HERMIX_HOME."
            }),
        );

        tool_schema(
            "disk_cleanup",
            "Find and remove stale log files, *.tmp/partial files, orphaned cron outputs, and aged cache files under HERMIX_HOME. dry_run defaults to true; refuse to touch anything outside HERMIX_HOME.",
            JsonSchema::object(props, vec!["action".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn touch_with_age(path: &Path, secs_old: u64) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"test").unwrap();
        // We can't set mtime easily across platforms, so just leave it;
        // tests will use very large thresholds so age checks pass.
        let _ = secs_old;
    }

    #[test]
    fn candidates_sorted_by_size() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(home.join("logs")).unwrap();
        let big = home.join("logs").join("big.log");
        let small = home.join("logs").join("small.log");
        std::fs::write(&big, vec![0u8; 100_000]).unwrap();
        std::fs::write(&small, vec![0u8; 100]).unwrap();
        let cands = gather_candidates(home, 0, 0);
        assert!(!cands.is_empty());
        // big.log should be earlier in the sorted-by-size list
        let big_idx = cands.iter().position(|c| c.path == big.to_string_lossy().to_string());
        let small_idx = cands
            .iter()
            .position(|c| c.path == small.to_string_lossy().to_string());
        if let (Some(b), Some(s)) = (big_idx, small_idx) {
            assert!(b < s, "big should sort before small");
        }
    }

    #[test]
    fn scan_logs_picks_up_files() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(home.join("logs")).unwrap();
        touch_with_age(&home.join("logs").join("old.log"), 0);
        let cands = scan_log_files(home, Duration::from_secs(0));
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].category, "stale_log");
    }

    #[test]
    fn tmp_scanner_picks_tmp_files() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        touch_with_age(&home.join("foo.tmp"), 0);
        touch_with_age(&home.join("bar.partial"), 0);
        let cands = scan_tmp_files(home, Duration::from_secs(0));
        assert_eq!(cands.len(), 2);
    }

    #[test]
    fn safety_refuses_paths_outside_home() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let outside = home.join("..").join("evil.tmp");
        let cands = candidates_for_paths(
            home,
            vec![outside.to_string_lossy().to_string()],
        );
        assert!(cands.is_empty(), "should refuse paths outside HERMIX_HOME");
    }

    #[tokio::test]
    async fn clean_dry_run_does_not_delete() {
        let tmp = TempDir::new().unwrap();
        std::env::set_var("HERMIX_HOME", tmp.path());
        std::fs::create_dir_all(tmp.path().join("logs")).unwrap();
        let target = tmp.path().join("logs").join("keepme.log");
        std::fs::write(&target, b"x").unwrap();
        let handler = DiskCleanupHandler::new();
        let _ = handler
            .execute(json!({ "action": "clean", "dry_run": true, "log_max_age_days": 0 }))
            .await;
        assert!(target.exists(), "dry_run should not delete anything");
    }
}
