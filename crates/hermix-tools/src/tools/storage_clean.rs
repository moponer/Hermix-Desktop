//! Storage cleaner — chat-confirmed, safelisted junk deletion.
//!
//! Companion to `storage_analyzer`. Where the analyzer is read-only and
//! `Safe`, this tool is **`Dangerous`**: the dispatch gate
//! (`local_agent_service::decide_pre_tool_dispatch`) pops a chat approval
//! card before it runs — no local HTTP server, no browser, the
//! confirmation happens in the conversation.
//!
//! Safety model (mirrors the Python skill's whitelist, tightened):
//!   - Every path is canonicalized (realpath) to defeat `..` traversal.
//!   - Paths under a system/blacklist root (C:\Windows, Program Files,
//!     ProgramData, hermix's own home) are ALWAYS refused.
//!   - Paths are only accepted if they fall under a known junk root
//!     (AppData\Local, TEMP, user-profile dev caches, Downloads).
//!   - `mode: "trash"` sends the file to the Recycle Bin via
//!     `SHFileOperationW(FOF_ALLOWUNDO)` — reversible. `mode: "delete"`
//!     removes it directly — irreversible.
//!   - `confirm: true` is required; without it the tool refuses, so the
//!     agent cannot "accidentally" call it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::Serialize;
use serde_json::{json, Value};

use hermix_config::paths::hermix_home;
use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema, traits::RiskLevel};

use crate::ToolRegistry;

/// Get the USERPROFILE path, logging a warning if the env var is missing.
/// Returns empty string as fallback (matching previous behavior) but makes
/// the failure observable.
fn user_profile() -> String {
    match std::env::var("USERPROFILE") {
        Ok(p) if !p.is_empty() => p,
        _ => {
            tracing::warn!("USERPROFILE env var is missing or empty; storage paths may be invalid");
            String::new()
        }
    }
}

/// Recycle-bin FFI (shell32 is always present on Windows).
#[cfg(windows)]
#[repr(C)]
struct ShFileOpStructW {
    hwnd: *mut std::ffi::c_void,
    w_func: u32,
    p_from: *const u16,
    p_to: *const u16,
    f_flags: u16,
    f_any_operation_aborted: i32,
    h_name_mappings: *mut std::ffi::c_void,
    lpsz_progress_title: *const u16,
}

#[cfg(windows)]
const FO_DELETE: u32 = 3;
#[cfg(windows)]
const FOF_ALLOWUNDO: u16 = 0x0040;
#[cfg(windows)]
const FOF_NOCONFIRMATION: u16 = 0x0010;
#[cfg(windows)]
const FOF_NOERRORUI: u16 = 0x0400;
#[cfg(windows)]
const FOF_SILENT: u16 = 0x0004;

#[cfg(windows)]
#[link(name = "shell32")]
extern "system" {
    fn SHFileOperationW(lp_file_op: *mut ShFileOpStructW) -> i32;
}

#[cfg(windows)]
fn wide_double_null(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::repeat(0u16).take(2)) // double null terminator
        .collect()
}

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct CleanReport {
    pub mode: String,
    pub requested: usize,
    pub deleted: Vec<String>,
    pub freed_bytes: u64,
    pub refused: Vec<String>,
    pub errors: Vec<String>,
    /// 体积快照在 deadline 内没走完（子树被低估）。删除照常执行，
    /// freed 仅供参考。
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub size_scan_truncated: bool,
}

// ---------------------------------------------------------------------------
// Safety: blacklist / safelist
// ---------------------------------------------------------------------------

/// Hermix-owned scratch root: the one place the agent is told to put
/// throwaway files (test scripts, probe outputs, debug dumps).
///
/// Why this exists as a *code-defined* path rather than a prompt
/// convention: the prompt can tell the model where to write, but only
/// code can guarantee the prefix. `is_scratch_scoped` downgrades the
/// tool's risk to `Caution` when every requested path resolves inside
/// this root, which is what lets self-cleanup run without an approval
/// round-trip. A model-chosen filename cannot get here — the root is
/// fixed by this function, and `safety_check` still runs in full.
pub fn scratch_root() -> PathBuf {
    let temp = std::env::var("TEMP")
        .or_else(|_| std::env::var("TMP"))
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().to_string());
    PathBuf::from(temp).join("hermix-scratch")
}

/// Roots we never touch, no matter what.
fn blacklist_roots() -> Vec<PathBuf> {
    let profile = user_profile();
    let local = std::env::var("LOCALAPPDATA")
        .unwrap_or_else(|_| format!("{}\\AppData\\Local", profile));
    let roots = vec![
        PathBuf::from("C:\\Windows"),
        PathBuf::from("C:\\Program Files"),
        PathBuf::from("C:\\Program Files (x86)"),
        PathBuf::from("C:\\ProgramData"),
        hermix_home(),
    ];
    // Also refuse anything under hermix's own binary/runtime dirs by
    // name if they resolve outside the home.
    let _ = local;
    roots
}

/// True when every requested path resolves inside [`scratch_root`].
///
/// Used as the tool's risk predicate: hermix's own scratch tree is by
/// construction disposable, so cleaning it does not need a human in
/// the loop. Anything outside — the wider `%TEMP%`, `Downloads`, the
/// whole safelist — keeps the full `Dangerous` treatment.
///
/// Deliberately conservative:
///   - the scratch root must already exist (it is created on first
///     use by whoever writes there), so a typo'd path can't match;
///   - comparison is on **canonicalized** paths, so `..` traversal and
///     junction/symlink escapes don't slip through;
///   - an empty path list is NOT scratch-scoped — an unscoped call must
///     still be treated as dangerous.
pub fn is_scratch_scoped(raw_paths: &[String]) -> bool {
    if raw_paths.is_empty() {
        return false;
    }
    let Ok(root) = scratch_root().canonicalize() else {
        return false;
    };
    raw_paths.iter().all(|raw| {
        PathBuf::from(raw)
            .canonicalize()
            .map(|c| is_within(&root, &c))
            // An unresolvable path cannot be proven to be inside the
            // scratch root, so it must not earn the risk downgrade.
            .unwrap_or(false)
    })
}

/// Roots where junk (caches, temps, installer residue) legitimately lives.
fn safelist_roots() -> Vec<PathBuf> {
    let profile = user_profile();
    let local = std::env::var("LOCALAPPDATA")
        .unwrap_or_else(|_| format!("{}\\AppData\\Local", profile));
    let temp = std::env::var("TEMP").unwrap_or_else(|_| format!("{}\\Temp", local));

    let mut roots: Vec<PathBuf> = vec![
        PathBuf::from(&local),
        PathBuf::from(&temp),
        PathBuf::from(format!("{}\\Downloads", profile)),
        PathBuf::from(format!("{}\\.cache", profile)),
        PathBuf::from(format!("{}\\.npm", profile)),
        PathBuf::from(format!("{}\\.gradle", profile)),
        PathBuf::from(format!("{}\\.m2", profile)),
        PathBuf::from(format!("{}\\.nuget\\packages", profile)),
        PathBuf::from(format!("{}\\.cargo", profile)),
        PathBuf::from(format!("{}\\.rustup", profile)),
        PathBuf::from(format!("{}\\pip\\Cache", local)),
        PathBuf::from(format!("{}\\Yarn", local)),
        PathBuf::from(format!("{}\\uv", local)),
        PathBuf::from(format!("{}\\ms-playwright", local)),
        PathBuf::from(format!("{}\\go-build", local)),
        // Hermix's own scratch tree — where the prompt tells the agent
        // to put throwaway files. Listed so `safety_check` accepts it
        // (it lives under %TEMP%, which is already safelisted, but
        // naming it explicitly keeps the two mechanisms in sync).
        scratch_root(),
    ];
    // De-dupe (some roots coincide on different env layouts).
    roots.sort();
    roots.dedup();
    roots
}

fn is_within(base: &Path, target: &Path) -> bool {
    target.starts_with(base)
}

/// Returns Ok(canonical_path) if the path is safe to delete, or an error
/// reason string if it must be refused.
fn safety_check(raw: &str) -> Result<PathBuf, String> {
    let pb = PathBuf::from(raw);
    let canonical = pb
        .canonicalize()
        .map_err(|e| format!("无法解析路径 '{}': {}", raw, e))?;

    for bl in blacklist_roots() {
        if let Ok(blc) = bl.canonicalize() {
            if is_within(&blc, &canonical) {
                return Err(format!(
                    "拒绝：'{}' 位于系统/受保护目录 '{}' 内",
                    canonical.display(),
                    blc.display()
                ));
            }
        } else if is_within(&bl, &canonical) {
            return Err(format!(
                "拒绝：'{}' 位于系统/受保护目录 '{}' 内",
                canonical.display(),
                bl.display()
            ));
        }
    }

    let safelist = safelist_roots();
    let allowed = safelist.iter().any(|root| {
        if let Ok(rc) = root.canonicalize() {
            is_within(&rc, &canonical)
        } else {
            is_within(root, &canonical)
        }
    });
    if !allowed {
        return Err(format!(
            "拒绝：'{}' 不在已知 junk 安全目录内（仅允许缓存/临时/下载安装包等）",
            canonical.display()
        ));
    }
    Ok(canonical)
}

// ---------------------------------------------------------------------------
// Deletion primitives
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn send_to_recycle_bin(path: &Path) -> Result<(), String> {
    let wide = wide_double_null(&path.to_string_lossy());
    let mut op = ShFileOpStructW {
        hwnd: std::ptr::null_mut(),
        w_func: FO_DELETE,
        p_from: wide.as_ptr(),
        p_to: std::ptr::null(),
        f_flags: FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT,
        f_any_operation_aborted: 0,
        h_name_mappings: std::ptr::null_mut(),
        lpsz_progress_title: std::ptr::null(),
    };
    // SAFETY: `wide` is a valid double-null-terminated wide string.
    // All pointers in `ShFileOpStructW` are derived from `wide` or null.
    let rc = unsafe { SHFileOperationW(&mut op) };
    if rc == 0 {
        Ok(())
    } else {
        Err(format!("SHFileOperationW 失败，错误码 {}", rc))
    }
}

#[cfg(not(windows))]
fn send_to_recycle_bin(_path: &Path) -> Result<(), String> {
    Err("回收站仅在 Windows 上支持".into())
}

fn delete_direct(path: &Path) -> Result<(), String> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
    .map_err(|e| format!("删除失败: {}", e))
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

pub struct StorageCleanHandler;

impl Default for StorageCleanHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl StorageCleanHandler {
    pub fn new() -> Self {
        Self
    }

    fn run_clean(paths: Vec<String>, mode: &str) -> CleanReport {
        let mut deleted = vec![];
        let mut refused = vec![];
        let mut errors = vec![];
        let mut freed: u64 = 0;
        // 体积快照总预算：删除前的 freed 统计不值得无界遍历（数百万文件的
        // 子树会把 blocking 线程钉住几分钟）。超时后 freed 低估、删除照常。
        let scan_deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        let mut size_scan_truncated = false;

        for raw in paths {
            let canonical = match safety_check(&raw) {
                Ok(p) => p,
                Err(reason) => {
                    refused.push(format!("{} — {}", raw, reason));
                    continue;
                }
            };
            // Snapshot size before deletion for the freed tally.
            let size = if canonical.is_dir() {
                walkdir::WalkDir::new(&canonical)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .take_while(|_| {
                        if std::time::Instant::now() >= scan_deadline {
                            size_scan_truncated = true;
                            false
                        } else {
                            true
                        }
                    })
                    .filter(|e| e.file_type().is_file())
                    .filter_map(|e| e.metadata().ok())
                    .map(|m| m.len())
                    .sum()
            } else {
                canonical.metadata().map(|m| m.len()).unwrap_or(0)
            };

            let result = if mode == "delete" {
                delete_direct(&canonical)
            } else {
                send_to_recycle_bin(&canonical)
            };

            match result {
                Ok(_) => {
                    deleted.push(canonical.to_string_lossy().to_string());
                    freed += size;
                }
                Err(e) => errors.push(format!("{} — {}", canonical.display(), e)),
            }
        }

        CleanReport {
            mode: mode.to_string(),
            requested: deleted.len() + refused.len() + errors.len(),
            deleted,
            freed_bytes: freed,
            refused,
            errors,
            size_scan_truncated,
        }
    }
}

#[async_trait]
impl ToolHandler for StorageCleanHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        // `confirm` is a safety interlock, so it must NEVER be inferred.
        // But `as_bool()` alone is too strict in practice: plenty of
        // models emit `"confirm": "true"` (string) rather than a JSON
        // boolean, and the old check silently treated that as "not
        // confirmed" — producing "必须有 confirm: true" for a call that
        // visibly passed it, with the user having already clicked
        // approve. The model then retried the same broken shape.
        //
        // Only these forms count as confirmation:
        //   - JSON `true`
        //   - the exact strings "true" / "1" (trimmed, case-insensitive)
        // Everything else — including "false", "yes", "", 1, and any
        // absent value — is NOT confirmation. A non-boolean number is
        // also rejected rather than truthy-cast, because silently
        // accepting `confirm: 1` would be exactly the kind of
        // "close enough" coercion a delete interlock must not have.
        let confirm_raw = params.get("confirm");
        let confirm = match confirm_raw {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(s)) => {
                let t = s.trim();
                t.eq_ignore_ascii_case("true") || t == "1"
            }
            _ => false,
        };
        if !confirm {
            // Name the shape we actually saw. "必须有 confirm: true" sent
            // the model hunting for a wrong field name when the real
            // problem was a string-vs-boolean mismatch.
            return Err(ToolError::InvalidParams(match confirm_raw {
                Some(Value::String(s)) => format!(
                    "confirm 必须是 JSON 布尔 true，不能是字符串（收到 \"{}\"）。\
                     请输出 \"confirm\": true（不带引号）。",
                    s
                ),
                Some(other) => format!(
                    "confirm 必须是 JSON 布尔 true，收到 {} 类型。",
                    match other {
                        Value::Null => "null".to_string(),
                        Value::Number(_) => "数字".to_string(),
                        v => format!("{}", v),
                    }
                ),
                None => "必须要有 confirm: true。删除/移回收站前必须显式确认（且会弹聊天审批卡片）。"
                    .to_string(),
            }));
        }
        let paths = params
            .get("paths")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                ToolError::InvalidParams("缺少 paths 参数（要清理的路径数组）。".into())
            })?;

        let mode = params
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("trash");
        if mode != "trash" && mode != "delete" {
            return Err(ToolError::InvalidParams(
                "mode 必须是 'trash'（移回收站，可逆）或 'delete'（直接删除，不可逆）。".into(),
            ));
        }

        let report = tokio::task::spawn_blocking({
            let paths = paths.clone();
            let mode = mode.to_string();
            move || StorageCleanHandler::run_clean(paths, &mode)
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        serde_json::to_string_pretty(&report)
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "paths".into(),
            json!({
                "type": "array",
                "items": { "type": "string" },
                "description": "要清理的路径数组。每个路径必须位于已知 junk 安全目录（AppData\\Local、TEMP、用户开发缓存、Downloads 安装包等）。拒绝 C:\\Windows、Program Files、ProgramData、hermix 自身目录。"
            }),
        );
        props.insert(
            "mode".into(),
            json!({
                "type": "string",
                "description": "trash（移回收站，可逆，默认）或 delete（直接删除，不可逆）",
                "enum": ["trash", "delete"]
            }),
        );
        props.insert(
            "confirm".into(),
            json!({
                "type": "boolean",
                "description": "必须显式传 true。配合 dispatch 的聊天审批卡片，双保险防止误删。"
            }),
        );
        tool_schema(
            "storage_clean",
            "在聊天审批确认后，删除/移回收站指定的 junk 文件。仅接受位于缓存/临时/下载等安全目录的路径，拒绝系统目录。trash 走回收站（可逆），delete 直接删。\n\n【临时文件自清理】把测试脚本、探针输出、调试 dump 等一次性产物写到 %TEMP%\\hermix-scratch\\ 下（该目录由程序定义），收尾时清理它们**不需要弹审批卡**。写到 %TEMP% 其它位置或 Downloads 则仍需用户逐次确认。",
            JsonSchema::object(props, vec!["paths".into(), "confirm".into()]),
        )
    }
}

/// Register as `Dangerous` — the dispatch gate will require a chat
/// approval before this ever runs.
pub fn register(registry: &ToolRegistry) {
    let handler = Arc::new(StorageCleanHandler::new());
    let schema = handler.schema();
    registry.register_with_risk(
        "storage_clean",
        "storage",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        true,
        "在聊天审批后删除/移回收站 junk 文件（仅安全目录，拒绝系统目录）。\
         BOUNDARY（存储三件套）：先 `storage_analyzer` 看占用（只读），再用本工具删除选中路径；\
         只想清理 HERMIX_HOME（应用自身缓存）时用 `disk_cleanup`，无需审批卡。",
        "🧹",
        None,
        RiskLevel::Dangerous,
        false,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safety_refuses_system_dir() {
        let r = safety_check("C:\\Windows\\System32\\drivers");
        assert!(r.is_err(), "must refuse Windows system dirs");
    }

    #[test]
    fn safety_refuses_non_junk() {
        // A random user document dir is not on the safelist.
        let profile = std::env::var("USERPROFILE").unwrap_or_default();
        let doc = format!("{}\\Documents\\thesis.docx", profile);
        let r = safety_check(&doc);
        assert!(r.is_err(), "must refuse non-junk paths: {:?}", r);
    }

    #[tokio::test]
    async fn confirm_required() {
        let handler = StorageCleanHandler::new();
        let err = handler
            .execute(json!({ "paths": ["x"], "mode": "trash" }))
            .await;
        // No confirm -> InvalidParams, not a crash.
        assert!(err.is_err());
    }

    // ---- confirm coercion -----------------------------------------
    //
    // Regression: models routinely emit `"confirm": "true"` as a
    // string. The old `as_bool()` check rejected that, so the tool
    // reported "必须有 confirm: true" for a call that visibly passed it
    // — after the user had already clicked approve. The user saw two
    // approvals and zero deletions.

    #[tokio::test]
    async fn string_true_counts_as_confirmation() {
        // Must get PAST the confirm gate. We assert on the error *shape*:
        // an empty path list fails with the "缺少 paths" message, not
        // the confirm one, which proves the gate opened.
        let handler = StorageCleanHandler::new();
        for raw in [
            json!({ "paths": [], "confirm": "true" }),
            json!({ "paths": [], "confirm": "TRUE" }),
            json!({ "paths": [], "confirm": " true " }),
            json!({ "paths": [], "confirm": "1" }),
        ] {
            let err = handler.execute(raw).await.unwrap_err();
            let msg = err.to_string();
            assert!(
                !msg.contains("confirm"),
                "string form should satisfy the gate, got: {}",
                msg
            );
        }
    }

    #[tokio::test]
    async fn real_boolean_true_counts_as_confirmation() {
        let handler = StorageCleanHandler::new();
        let err = handler
            .execute(json!({ "paths": [], "confirm": true }))
            .await
            .unwrap_err();
        assert!(!err.to_string().contains("confirm"));
    }

    #[tokio::test]
    async fn near_miss_values_are_still_refused() {
        // A delete interlock must not be truthy-cast. Anything that is
        // not an explicit "yes" is a refusal.
        let handler = StorageCleanHandler::new();
        for raw in [
            json!({ "paths": [], "confirm": "false" }),
            json!({ "paths": [], "confirm": "yes" }),
            json!({ "paths": [], "confirm": "" }),
            json!({ "paths": [], "confirm": 1 }),
            json!({ "paths": [], "confirm": null }),
            json!({ "paths": [], "confirm": "是" }),
        ] {
            let err = handler.execute(raw).await.unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("confirm"),
                "must refuse non-explicit confirmations, got: {}",
                msg
            );
        }
    }

    #[test]
    fn refuses_paths_outside_safelist_in_report() {
        let report = StorageCleanHandler::run_clean(
            vec!["C:\\Windows\\notreal".into()],
            "trash",
        );
        assert_eq!(report.deleted.len(), 0);
        assert!(!report.refused.is_empty());
    }

    #[test]
    fn clean_deletes_safe_temp_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let f = tmp.path().join("junk.tmp");
        std::fs::write(&f, b"garbage").unwrap();
        // Only run the real delete if the temp dir resolves under a
        // safelist root (it won't on CI); otherwise assert the safety
        // layer engaged rather than crashing.
        let report = StorageCleanHandler::run_clean(vec![f.to_string_lossy().to_string()], "delete");
        // Either it was deleted (temp under safelist) or refused — both
        // are safe outcomes; we must NOT have silently touched system.
        assert!(report.deleted.len() + report.refused.len() > 0);
    }

    // ---- scratch-scope risk downgrade -------------------------------
    //
    // The downgrade is what lets the agent clean up after itself without
    // an approval round-trip. These tests pin the boundary: only a path
    // provably inside `scratch_root()` earns it, and ONE loose path in
    // the list is enough to lose it for the whole call.

    /// Create a **unique** subdirectory under the real scratch root and
    /// drop only that subdirectory on teardown.
    ///
    /// Earlier this guard removed the whole `scratch_root()`, which races
    /// every other scratch test running in parallel — one test's teardown
    /// deletes another's files mid-assertion. Each test now owns a
    /// private leaf, so the tests stay independent while still exercising
    /// the real code-defined root (which is the only thing that can earn
    /// the downgrade).
    struct ScratchGuard(PathBuf);
    impl ScratchGuard {
        fn create() -> Self {
            let root = scratch_root();
            std::fs::create_dir_all(&root).expect("create scratch root");
            // Test name is unique within the binary; add the thread id so
            // even a re-run inside one process cannot collide.
            let unique = format!(
                "t-{:?}-{}",
                std::thread::current().id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            );
            let dir = root.join(unique);
            std::fs::create_dir_all(&dir).expect("create scratch subdir");
            Self(dir)
        }
        /// A file inside this test's private scratch subdir.
        fn file(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }
    impl Drop for ScratchGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn scratch_root_lives_under_the_system_temp_dir() {
        let root = scratch_root();
        assert!(
            root.to_string_lossy().contains("hermix-scratch"),
            "scratch root must be code-defined and namespaced, got {:?}",
            root
        );
    }

    #[test]
    fn a_real_file_in_scratch_is_scratch_scoped() {
        let g = ScratchGuard::create();
        let f = g.file("probe.txt");
        std::fs::write(&f, b"x").unwrap();
        assert!(
            is_scratch_scoped(&[f.to_string_lossy().to_string()]),
            "a file the agent itself dropped in the scratch tree must qualify"
        );
    }

    #[test]
    fn a_path_outside_scratch_is_not_scoped() {
        // Downloads is on the safelist, so `safety_check` would allow it —
        // but it is the USER's directory, so it must keep the approval.
        let other = std::env::var("USERPROFILE")
            .map(|p| PathBuf::from(p).join("Downloads").join("keep.txt"))
            .unwrap_or_else(|_| PathBuf::from("C:/definitely/not/scratch/keep.txt"));
        std::fs::create_dir_all(other.parent().unwrap()).ok();
        std::fs::write(&other, b"user file").ok();
        assert!(
            !is_scratch_scoped(&[other.to_string_lossy().to_string()]),
            "a user's Downloads file must never earn the scratch downgrade"
        );
    }

    #[test]
    fn a_mixed_list_loses_the_downgrade_entirely() {
        // The dangerous one poisons the batch: all-or-nothing on purpose,
        // so a call can never smuggle a real delete past the prompt by
        // tacking a scratch file onto the front of the list.
        let g = ScratchGuard::create();
        let inside = g.file("ok.txt");
        std::fs::write(&inside, b"x").unwrap();
        let outside = std::env::var("USERPROFILE")
            .map(|p| PathBuf::from(p).join("Downloads").join("keep.txt"))
            .unwrap_or_else(|_| PathBuf::from("C:/definitely/not/scratch/keep.txt"));
        std::fs::create_dir_all(outside.parent().unwrap()).ok();
        std::fs::write(&outside, b"user file").ok();
        assert!(!is_scratch_scoped(&[
            inside.to_string_lossy().to_string(),
            outside.to_string_lossy().to_string(),
        ]));
    }

    #[test]
    fn traversal_and_missing_paths_do_not_qualify() {
        let _g = ScratchGuard::create();
        // `..` out of the scratch root must not resolve to "inside".
        let escape = scratch_root().join("..").join("..").join("evil.txt");
        assert!(
            !is_scratch_scoped(&[escape.to_string_lossy().to_string()]),
            "a traversal path must not earn the downgrade"
        );
        // A path that cannot be canonicalized cannot be proven safe.
        assert!(
            !is_scratch_scoped(&["C:/nope/does/not/exist.txt".to_string()]),
            "an unresolvable path must not earn the downgrade"
        );
    }

    #[test]
    fn an_empty_list_is_never_scoped() {
        // No paths means we know nothing; treating it as scratch-scoped
        // would silently disable the prompt for a malformed call.
        assert!(!is_scratch_scoped(&[]));
    }
}
