//! Storage analyzer — read-only disk scanner (Windows-native).
//!
//! Ports the scanning half of the `storage-analyzer` agent skill into a
//! native Rust tool. It walks the standard Windows disk-hog hot spots
//! (user profile, AppData, Temp, Downloads, Program Files, dev caches)
//! and every mounted volume, then reports per-directory sizes as a
//! structured JSON blob for the agent to classify in the chat.
//!
//! **STRICTLY READ-ONLY.** This tool never creates, moves, or deletes
//! anything. Deletion lives in the separate `storage_clean` tool, which
//! is registered as `Dangerous` so the dispatch gate pops a chat
//! approval card before anything is touched. No local HTTP server, no
//! browser — everything happens in the conversation.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::Serialize;
use serde_json::{json, Value};

use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema, traits::RiskLevel};

use crate::ToolRegistry;

/// Floor below which we drop a directory from the report (matches the
/// Python skill's 50 MB / 51200 KB cutoff so the chat output stays readable).
const MIN_KB: u64 = 51_200;
/// Max children reported per group (matches the Python skill).
const PER_GROUP_LIMIT: usize = 40;
/// hard upper bound for a single scan invocation.
/// Background: spawning walkdir on `C:\` (or even just `C:\Users`)
/// can run for **minutes** on real machines because the entry count
/// is unbounded. spawn_blocking can't be cleanly cancelled, so an
/// oversized scan pins a worker thread + blocks the agent loop
/// indefinitely. 90s matches the frontend's STALE_STREAM_TIMEOUT_MS
/// (useStreamChat.ts:592) so the user sees "stale_stream" before we
/// even hit the deadline — meaning a deadline hit here means the
/// frontend already settled and we missed the chance to send a
/// partial result. Still worth having as defense-in-depth: when
/// the user retries after a stale-stream settle, a fresh 90s
/// budget keeps the next attempt bounded too.
const SCAN_DEADLINE_SECS: u64 = 90;

// ---------------------------------------------------------------------------
// Windows disk-free-space FFI (kernel32 is always linked on Windows).
// ---------------------------------------------------------------------------

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetDiskFreeSpaceExW(
        lp_directory_name: *const u16,
        lp_free_bytes_available_to_caller: *mut u64,
        lp_total_number_of_bytes: *mut u64,
        lp_total_number_of_free_bytes: *mut u64,
    ) -> i32;
}

#[cfg(windows)]
fn wide_null(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Free / total bytes for a volume root like `C:\`. Returns `None` if the
/// volume is not mounted or the FFI call fails.
#[cfg(windows)]
fn disk_usage(root: &str) -> Option<(u64, u64)> {
    let wide = wide_null(root);
    let mut free = 0u64;
    let mut total = 0u64;
    let mut avail = 0u64;
    // SAFETY: `wide` is a valid null-terminated wide string. Output
    // pointers point to properly aligned stack-local u64 variables.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut avail as *mut u64,
            &mut total as *mut u64,
            &mut free as *mut u64,
        )
    };
    if ok == 0 {
        return None;
    }
    Some((total, free))
}

#[cfg(not(windows))]
fn disk_usage(_root: &str) -> Option<(u64, u64)> {
    None
}

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    pub size_h: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub denied: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DriveInfo {
    pub name: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub total_h: String,
    pub free_h: String,
    pub used_h: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanReport {
    pub generated_at: String,
    pub scan_seconds: f64,
    pub system: Value,
    pub groups: IndexMap<String, Vec<DirEntry>>,
    pub denied: Vec<String>,
    pub notes: Vec<String>,
    /// true when the SCAN_DEADLINE_SECS budget was hit
    /// before all configured targets were walked. The report still
    /// contains everything scanned up to the deadline (so the LLM
    /// can reason about partial state); this flag tells the agent
    /// "you may want to suggest a more targeted scan" instead of
    /// assuming the picture is complete. Serialized as a top-level
    /// JSON field so the LLM can reference it directly.
    #[serde(default)]
    pub truncated: bool,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn fmt_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB", "PB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", size as u64, UNITS[unit])
    } else {
        format!("{:.1} {}", size, UNITS[unit])
    }
}

/// Recursive size in bytes via walkdir. Skips symlinks and unreadable
/// entries (mirrors `dir_size_bytes` in the Python skill).
/// optional `deadline` adds a `take_while` to the iterator
/// so a deep tree (e.g. `C:\Windows\WinSxS` with millions of files)
/// bails out instead of holding the spawn_blocking worker past
/// SCAN_DEADLINE_SECS. Returning early **underestimates** the size
/// of that subtree — but `truncated=true` in the report tells the
/// LLM to suggest a more targeted re-scan, so the user gets an
/// actionable answer instead of a frozen UI.
fn dir_size_bytes(path: &Path, deadline: Option<std::time::Instant>) -> u64 {
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
        .take_while(|_| match deadline {
            Some(d) => std::time::Instant::now() < d,
            None => true,
        })
        .filter(|e| !e.file_type().is_symlink())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

/// Size the immediate children of `path`, sorted desc, filtered to `MIN_KB`.
/// optional `deadline` short-circuits both the inner walkdir
/// (per-entry check via `take_while`) and the outer children loop, so a
/// single huge subdirectory can't burn the whole SCAN_DEADLINE_SECS
/// budget while the rest of the targets get skipped.
fn scan_children(path: &Path, deadline: Option<std::time::Instant>) -> (Vec<DirEntry>, bool) {
    let mut denied = false;
    let mut entries: Vec<DirEntry> = vec![];
    let read_dir = match std::fs::read_dir(path) {
        Ok(rd) => rd,
        Err(_) => {
            denied = true;
            return (entries, denied);
        }
    };
    for entry in read_dir.flatten() {
        // Outer-loop deadline check (cheap; one per child).
        if let Some(d) = deadline {
            if std::time::Instant::now() >= d {
                tracing::debug!(target: "hermix.storage_analyzer", path = %path.display(), "scan_children: deadline hit, truncating child list");
                break;
            }
        }
        let child = entry.path();
        if child.is_symlink() {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let size = if meta.is_file() {
            meta.len()
        } else if meta.is_dir() {
            dir_size_bytes(&child, deadline)
        } else {
            continue;
        };
        if size / 1024 < MIN_KB {
            continue;
        }
        let name = child
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| child.to_string_lossy().to_string());
        entries.push(DirEntry {
            name,
            path: child.to_string_lossy().to_string(),
            size_bytes: size,
            size_h: fmt_size(size),
            denied: false,
        });
    }
    entries.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    entries.truncate(PER_GROUP_LIMIT);
    (entries, denied)
}

/// All mounted volumes A:..Z:\.
fn list_drives() -> Vec<DriveInfo> {
    let mut drives = vec![];
    for letter in b'A'..=b'Z' {
        let root = format!("{}:\\", letter as char);
        if !Path::new(&root).exists() {
            continue;
        }
        if let Some((total, free)) = disk_usage(&root) {
            let used = total.saturating_sub(free);
            drives.push(DriveInfo {
                name: root,
                total_bytes: total,
                free_bytes: free,
                total_h: fmt_size(total),
                free_h: fmt_size(free),
                used_h: fmt_size(used),
            });
        }
    }
    drives
}

fn env_var_fallback(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.to_string())
}

/// Default Windows hot-spot scan targets (mirrors `scan_windows` in scan.py).
/// Returns `(targets, dev_cache_entries)`.
fn default_targets() -> (Vec<(String, PathBuf)>, Vec<DirEntry>) {
    let profile = env_var_fallback("USERPROFILE", "");
    let local = env_var_fallback(
        "LOCALAPPDATA",
        &format!("{}\\AppData\\Local", profile),
    );
    let roaming = env_var_fallback("APPDATA", &format!("{}\\AppData\\Roaming", profile));
    let temp = env_var_fallback("TEMP", &format!("{}\\Temp", local));
    let program_files = env_var_fallback("ProgramFiles", "C:\\Program Files");
    let program_files_x86 = env_var_fallback("ProgramFiles(x86)", "C:\\Program Files (x86)");

    let mut targets = vec![
        ("user_profile".into(), PathBuf::from(&profile)),
        ("appdata_local".into(), PathBuf::from(&local)),
        ("appdata_roaming".into(), PathBuf::from(&roaming)),
        ("temp".into(), PathBuf::from(&temp)),
        (
            "downloads".into(),
            PathBuf::from(format!("{}\\Downloads", profile)),
        ),
        ("program_files".into(), PathBuf::from(&program_files)),
        ("program_files_x86".into(), PathBuf::from(&program_files_x86)),
    ];

    // Dev caches — only included when present.
    let dev_paths: Vec<PathBuf> = [
        format!("{}\\.cache", profile),
        format!("{}\\.npm", profile),
        format!("{}\\.gradle", profile),
        format!("{}\\.m2", profile),
        format!("{}\\.nuget\\packages", profile),
        format!("{}\\.cargo", profile),
        format!("{}\\pip\\Cache", local),
        format!("{}\\Yarn", local),
        format!("{}\\uv", local),
        format!("{}\\ms-playwright", local),
        format!("{}\\go-build", local),
    ]
    .iter()
    .map(PathBuf::from)
    .collect();

    let mut dev_cache_entries: Vec<DirEntry> = vec![];
    for p in dev_paths {
        if !p.is_dir() {
            continue;
        }
        let size = dir_size_bytes(&p, None);
        if size / 1024 < MIN_KB {
            continue;
        }
        dev_cache_entries.push(DirEntry {
            name: p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            path: p.to_string_lossy().to_string(),
            size_bytes: size,
            size_h: fmt_size(size),
            denied: false,
        });
    }
    dev_cache_entries.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    dev_cache_entries.truncate(PER_GROUP_LIMIT);

    targets.push(("dev_caches".into(), PathBuf::from("__dev_caches__")));
    (targets, dev_cache_entries)
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

pub struct StorageAnalyzerHandler;

impl Default for StorageAnalyzerHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl StorageAnalyzerHandler {
    pub fn new() -> Self {
        Self
    }

    /// Run the full default-target scan. Returns the serializable report.
    fn run_scan() -> ScanReport {
        let started = SystemTime::now();
        // per-invocation deadline. We check at the top of
        // each group boundary (cheap) and pass the deadline down to
        // scan_children so it can `take_while` inside the walkdir
        // iterator (catches a single huge target hogging the budget).
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_secs(SCAN_DEADLINE_SECS);
        tracing::info!(target: "hermix.storage_analyzer", deadline_secs = SCAN_DEADLINE_SECS, "scan: starting default-target walk");
        let (targets, dev_caches) = default_targets();
        let (mut targets, dev_caches) = (targets, dev_caches);
        tracing::info!(target: "hermix.storage_analyzer", target_count = targets.len(), dev_cache_count = dev_caches.len(), "scan: targets resolved");

        let mut groups: IndexMap<String, Vec<DirEntry>> = IndexMap::new();
        let mut denied: Vec<String> = vec![];
        let mut truncated = false;

        for (key, path) in targets.drain(..) {
            // Check deadline BETWEEN groups so a completed group
            // always lands in the report even if it pushed us over.
            if std::time::Instant::now() >= deadline {
                tracing::warn!(
                    target: "hermix.storage_analyzer",
                    skipped_groups = 1, // at least this one; outer break ends further iteration
                    remaining_targets_estimate = 1,
                    "scan: deadline hit; truncating remaining targets"
                );
                truncated = true;
                break;
            }
            if key == "dev_caches" {
                if !dev_caches.is_empty() {
                    groups.insert(key, dev_caches.clone());
                }
                continue;
            }
            if !path.exists() {
                tracing::debug!(target: "hermix.storage_analyzer", group = %key, path = %path.display(), "scan: target missing, skipping");
                continue;
            }
            tracing::debug!(target: "hermix.storage_analyzer", group = %key, path = %path.display(), "scan: walking target");
            let group_started = SystemTime::now();
            let (entries, was_denied) = scan_children(&path, Some(deadline));
            let group_elapsed = group_started.elapsed().map(|d| d.as_millis()).unwrap_or(0);
            tracing::info!(
                target: "hermix.storage_analyzer",
                group = %key,
                path = %path.display(),
                entries = entries.len(),
                denied = was_denied,
                elapsed_ms = group_elapsed,
                "scan: group done"
            );
            if was_denied {
                denied.push(path.to_string_lossy().to_string());
            }
            if !entries.is_empty() {
                groups.insert(key, entries);
            }
            // If scan_children bailed mid-walk because the deadline
            // expired inside it, the outer loop's next iteration will
            // catch it at the top and `break`. We don't try to
            // recover the half-scanned child (walkdir's iterator
            // doesn't expose "I gave up early" cheaply; the cost
            // would dominate the savings on small trees).
            if std::time::Instant::now() >= deadline {
                truncated = true;
            }
        }

        let drives = list_drives();
        let system_drive = env_var_fallback("SystemDrive", "C:").trim_end_matches('\\').to_string() + "\\";
        let (total, free) = disk_usage(&system_drive).unwrap_or((0, 0));
        tracing::info!(target: "hermix.storage_analyzer", drives = drives.len(), system_drive = %system_drive, "scan: drives enumerated");

        let system = json!({
            "os": format!("{} {}", env_var_fallback("OS", "Windows"), env_var_fallback("PROCESSOR_ARCHITECTURE", "")),
            "user": env_var_fallback("USERNAME", ""),
            "home": env_var_fallback("USERPROFILE", ""),
            "system_drive": system_drive,
            "disk_total": fmt_size(total),
            "disk_free": fmt_size(free),
            "disk_used": fmt_size(total.saturating_sub(free)),
            "drives": drives.iter().map(|d| json!({
                "name": d.name,
                "total": d.total_h,
                "free": d.free_h,
                "used": d.used_h,
            })).collect::<Vec<_>>(),
        });

        let elapsed = started.elapsed().map(|d| d.as_secs_f64()).unwrap_or(0.0);
        tracing::info!(
            target: "hermix.storage_analyzer",
            elapsed_seconds = elapsed,
            groups = groups.len(),
            denied = denied.len(),
            truncated = truncated,
            "scan: complete"
        );
        // Build notes dynamically so the LLM sees a clear "I gave
        // up early" signal in-context rather than having to notice
        // the `truncated` flag buried in JSON.
        let mut notes = vec![
            "扫描全程只读。删除请使用 storage_clean 工具（会弹聊天确认）。".into(),
            "大小均为估算值（递归统计文件，可能遗漏无权限目录）。".into(),
            "🟢/🟡/🔴 分级由对话中的 agent 依据分类方法论给出，不在本工具内。".into(),
        ];
        if truncated {
            notes.push(format!(
                "⚠️ 扫描在 {SCAN_DEADLINE_SECS}s 预算内未完成；已扫描的目录数据有效，但部分目标被跳过。建议用 path 参数指定更小范围重扫。"
            ));
        }
        ScanReport {
            generated_at: hermix_core::time::beijing_now().format("%Y-%m-%d %H:%M:%S (北京时间)").to_string(),
            scan_seconds: elapsed,
            system,
            groups,
            denied,
            notes,
            truncated,
        }
    }

    /// Scan a single user-supplied path's immediate children.
    fn run_path_scan(path: &Path) -> Result<ScanReport, ToolError> {
        if !path.exists() {
            return Err(ToolError::InvalidParams(format!(
                "路径不存在: {}",
                path.display()
            )));
        }
        let started = SystemTime::now();
        // Single-path scan respects the same 90s budget as the
        // default-target variant — the user might still point at a
        // tree deep enough to overrun (e.g. `C:\Windows\WinSxS`).
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_secs(SCAN_DEADLINE_SECS);
        let (entries, denied) = scan_children(path, Some(deadline));
        let deadline_hit = std::time::Instant::now() >= deadline;
        let mut groups: IndexMap<String, Vec<DirEntry>> = IndexMap::new();
        if denied {
            groups.insert(
                "scan".into(),
                vec![DirEntry {
                    name: "(permission denied)".into(),
                    path: path.to_string_lossy().to_string(),
                    size_bytes: 0,
                    size_h: "?".into(),
                    denied: true,
                }],
            );
        } else {
            groups.insert("scan".into(), entries);
        }
        let elapsed = started.elapsed().map(|d| d.as_secs_f64()).unwrap_or(0.0);
        let mut notes = vec!["扫描全程只读。删除请使用 storage_clean 工具。".into()];
        if deadline_hit {
            notes.push(format!(
                "⚠️ 单路径扫描在 {SCAN_DEADLINE_SECS}s 预算内未完成；已扫描到的子目录数据有效，但可能遗漏深层大目录。"
            ));
        }
        Ok(ScanReport {
            generated_at: hermix_core::time::beijing_now().format("%Y-%m-%d %H:%M:%S (北京时间)").to_string(),
            scan_seconds: elapsed,
            system: json!({ "path": path.to_string_lossy().to_string() }),
            groups,
            denied: if denied {
                vec![path.to_string_lossy().to_string()]
            } else {
                vec![]
            },
            notes,
            truncated: deadline_hit,
        })
    }
}

#[async_trait]
impl ToolHandler for StorageAnalyzerHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        tracing::info!(
            target: "hermix.storage_analyzer",
            mode = if path.is_some() { "single_path" } else { "default_targets" },
            path = path.as_deref().unwrap_or(""),
            "execute: dispatching to spawn_blocking"
        );

        // Scanning can take a while on big trees; don't block the async
        // runtime's worker thread.
        let report = match path {
            Some(p) => {
                let pb = PathBuf::from(p);
                tokio::task::spawn_blocking(move || Self::run_path_scan(&pb))
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))??
            }
            None => {
                tokio::task::spawn_blocking(Self::run_scan)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
            }
        };

        let json = serde_json::to_string_pretty(&report)
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        tracing::info!(
            target: "hermix.storage_analyzer",
            output_bytes = json.len(),
            "execute: serialised report"
        );
        Ok(json)
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "path".into(),
            json!({
                "type": "string",
                "description": "可选。要扫描的目录路径（如 'C:\\' 或 'C:\\Users\\me\\Downloads'）。若省略，则扫描整套 Windows 热点目录（用户配置/AppData/Temp/下载/Program Files/开发缓存）并列出所有盘符容量。"
            }),
        );
        tool_schema(
            "storage_analyzer",
            "【磁盘分析首选工具】只读磁盘占用分析器。扫描指定目录或整套 Windows 热点目录，返回各子目录大小（≥50MB，降序，Top40）与所有盘符容量。\n\n⚠️ 当用户要求分析磁盘占用/清理空间/查看大文件时，必须优先调用本工具，而非在 terminal 中运行 PowerShell 脚本。本工具是 Safe 级别（只读、无审批），而 terminal 中的 PowerShell 脚本可能因 Git Bash 引号剥离而失败，且每次调用都可能触发审批。\n\n使用方式：\n- 无参数：扫描整套 Windows 热点（用户配置/AppData/Temp/下载/Program Files/开发缓存）+ 所有盘符容量\n- 带 path 参数：扫描指定目录的子目录大小\n\n全程只读，绝不删除任何文件。删除请使用 storage_clean 工具。",
            JsonSchema::object(props, vec![]),
        )
    }
}

/// Register the tool as `Safe` — it is strictly read-only.
pub fn register(registry: &ToolRegistry) {
    let handler = Arc::new(StorageAnalyzerHandler::new());
    let schema = handler.schema();
    registry.register_with_risk(
        "storage_analyzer",
        "storage",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        true,
        "【磁盘分析首选工具】只读磁盘占用分析：扫描目录或 Windows 热点，返回子目录大小与盘符容量。优先使用此工具而非 terminal+PowerShell。\
         BOUNDARY（存储三件套）：`storage_analyzer` = 只读分析用户磁盘；`storage_clean` = 在审批后删除用户选中的 junk 路径（Dangerous）；\
         `disk_cleanup` = 只清理 HERMIX_HOME（本应用自己的缓存/旧会话产物）。",
        "💽",
        None,
        RiskLevel::Safe,
        false,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_size_scales() {
        assert_eq!(fmt_size(500), "500 B");
        assert_eq!(fmt_size(1024), "1.0 KB");
        assert_eq!(fmt_size(1024 * 1024 * 14), "14.0 MB");
        assert!(fmt_size(1024u64 * 1024 * 1024 * 27).starts_with("27.0 GB"));
    }

    #[test]
    fn dir_size_sums_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(tmp.path().join("a.bin"), vec![0u8; 1000]).unwrap();
        std::fs::write(sub.join("b.bin"), vec![0u8; 2000]).unwrap();
        assert_eq!(dir_size_bytes(tmp.path(), None), 3000);
    }

    #[test]
    fn scan_children_filters_small_and_sorts() {
        let tmp = tempfile::TempDir::new().unwrap();
        // 10 KB file -> below MIN_KB, dropped
        std::fs::write(tmp.path().join("small.txt"), vec![0u8; 10 * 1024]).unwrap();
        // 100 MB dir -> kept
        let big = tmp.path().join("big");
        std::fs::create_dir_all(&big).unwrap();
        std::fs::write(big.join("x.bin"), vec![0u8; 100 * 1024 * 1024]).unwrap();
        let (entries, denied) = scan_children(tmp.path(), None);
        assert!(!denied);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "big");
    }

    #[test]
    fn scan_children_marks_denied() {
        // A path we cannot read_dir (e.g. a file passed as dir) -> denied flag.
        let tmp = tempfile::TempDir::new().unwrap();
        let f = tmp.path().join("file.txt");
        std::fs::write(&f, b"x").unwrap();
        let (entries, denied) = scan_children(&f, None);
        assert!(denied);
        assert!(entries.is_empty());
    }

    #[test]
    fn default_targets_include_dev_caches_marker() {
        let (targets, _) = default_targets();
        assert!(targets.iter().any(|(k, _)| k == "dev_caches"));
        assert!(targets.iter().any(|(k, _)| k == "user_profile"));
    }

    #[test]
    fn run_path_scan_reports_sizes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let big = tmp.path().join("big");
        std::fs::create_dir_all(&big).unwrap();
        std::fs::write(big.join("x.bin"), vec![0u8; 100 * 1024 * 1024]).unwrap();
        let report = StorageAnalyzerHandler::run_path_scan(tmp.path()).unwrap();
        assert!(report.groups.contains_key("scan"));
        assert_eq!(report.groups["scan"].len(), 1);
        assert_eq!(report.groups["scan"][0].name, "big");
        // Fresh run_path_scan always succeeds within the 90s budget
        // on tiny temp dirs; truncated must be false.
        assert!(!report.truncated);
    }

    /// when an already-elapsed deadline is passed in,
    /// `dir_size_bytes` must short-circuit immediately (returning 0
    /// bytes) rather than walking the tree. This guards against the
    /// regression where the deadline argument was ignored.
    #[test]
    fn dir_size_bytes_honors_already_elapsed_deadline() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.bin"), vec![0u8; 1000]).unwrap();
        let already_elapsed = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(1))
            .expect("clock went backwards");
        // Deadline in the past → take_while fires on the very first
        // walkdir entry → sum is 0, NOT the 1000 bytes actually on disk.
        let size = dir_size_bytes(tmp.path(), Some(already_elapsed));
        assert_eq!(size, 0, "elapsed deadline must short-circuit walkdir");
    }

    /// run_path_scan happy path must NOT emit the
    /// deadline-hit warning. Truncation handling is exercised
    /// end-to-end by the production code path on real machines; the
    /// test here just guards against a regression where the warning
    /// gets prepended unconditionally.
    #[test]
    fn run_path_scan_happy_path_has_no_deadline_warning() {
        let tmp = tempfile::TempDir::new().unwrap();
        let report = StorageAnalyzerHandler::run_path_scan(tmp.path()).unwrap();
        assert!(!report.truncated);
        assert!(
            !report.notes.iter().any(|n| n.contains("未完成")),
            "happy path must not emit the deadline-hit warning"
        );
    }
}
