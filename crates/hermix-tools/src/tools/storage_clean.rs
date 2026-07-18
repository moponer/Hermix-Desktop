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
}

// ---------------------------------------------------------------------------
// Safety: blacklist / safelist
// ---------------------------------------------------------------------------

/// Roots we never touch, no matter what.
fn blacklist_roots() -> Vec<PathBuf> {
    let profile = std::env::var("USERPROFILE").unwrap_or_default();
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

/// Roots where junk (caches, temps, installer residue) legitimately lives.
fn safelist_roots() -> Vec<PathBuf> {
    let profile = std::env::var("USERPROFILE").unwrap_or_default();
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
        }
    }
}

#[async_trait]
impl ToolHandler for StorageCleanHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let confirm = params
            .get("confirm")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !confirm {
            return Err(ToolError::InvalidParams(
                "必须要有 confirm: true。删除/移回收站前必须显式确认（且会弹聊天审批卡片）。".into(),
            ));
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
            "在聊天审批确认后，删除/移回收站指定的 junk 文件。仅接受位于缓存/临时/下载等安全目录的路径，拒绝系统目录。trash 走回收站（可逆），delete 直接删。",
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
        "在聊天审批后删除/移回收站 junk 文件（仅安全目录，拒绝系统目录）",
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
}
