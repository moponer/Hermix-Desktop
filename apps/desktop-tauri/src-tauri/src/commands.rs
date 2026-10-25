use crate::AppState;
use crate::portable_env;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Local re-implementation of `hermix_config::env_compat`.
///
/// The `hermix-desktop` crate is built standalone (it has its own
/// `[workspace]` table, opting out of the root workspace), so it
/// cannot depend on the `hermix-config` workspace crate. To keep the
/// build self-contained we inline a 1:1 copy of the helper here.
///
/// Semantics:
/// - Returns the value of the **new** env var (`HERMIX_*`) if set and
///   non-empty.
/// - Otherwise returns the value of the **old** env var (`HERMES_*`)
///   if set and non-empty, and prints a one-shot stderr deprecation
///   notice the first time a particular `(new, old)` pair falls back.
/// - Returns `None` if neither is set or both are empty.
///
/// SAFETY: `seen_keys` is initialized at most once via `OnceLock` and
/// is only ever written from `.get_or_init(...)`, which the std
/// library guarantees is race-free. We then mutate the returned
/// `Vec` via a non-atomic `push` — this is safe because
/// `get_or_init` guarantees only one thread runs the closure, and
/// after that all other threads see a `&mut` borrow of the same Vec
/// for the duration of the call (they wait for the closure to
/// finish). Once the closure returns, `seen_keys` is shared, but
/// we never push to it again. So the "mutable after shared borrow"
/// problem never arises.
fn env_compat(new_name: &str, old_name: &str) -> Option<String> {
    use std::sync::Mutex;

    static SEEN: OnceLock<Mutex<Vec<(String, String)>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(Vec::new()));

    if let Ok(v) = std::env::var(new_name) {
        if !v.is_empty() {
            return Some(v);
        }
    }
    let old_value = std::env::var(old_name).ok().filter(|v| !v.is_empty())?;

    // One-shot deprecation notice, keyed on the (new, old) pair so
    // different env vars don't share a "did we already warn" bit.
    let key = (new_name.to_string(), old_name.to_string());
    let already_warned = {
        let guard = seen.lock().expect("env_compat deprecation table poisoned");
        guard.iter().any(|k| k == &key)
    };
    if !already_warned {
        let mut guard = seen.lock().expect("env_compat deprecation table poisoned");
        guard.push(key);
        eprintln!(
            "[hermix-compat] env var {old_name} is deprecated, use {new_name} instead \
             (will keep falling back for one release)"
        );
    }

    Some(old_value)
}

/// Resolve the canonical data directory for the desktop app.
///
/// 1. `$HERMIX_PORTABLE_DATA_DIR` (set in dev mode only)
/// 2. `$HERMIX_HOME` (explicit override)
/// 3. Dev-mode: `hermix-data/` at workspace root
/// 4. Last-ditch: `<install_root>/data`
///
/// `pub(crate)` 是因为 `system_settings`（壁纸落盘）必须用**同一条**解析
/// 链——复制一份就会在便携模式 / HERMIX_HOME 覆盖下走到不同分支，
/// 出现"设置页显示的数据目录"与"壁纸实际落盘位置"不一致。
pub(crate) fn resolve_data_dir() -> PathBuf {
    if let Some(p) = portable_env::portable_data_dir() {
        return p;
    }
    if let Some(p) = std::env::var_os("HERMIX_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
    {
        return p;
    }
    // Dev-mode: detect hermix-data/ at workspace root.
    if portable_env::is_tauri_app_binary() {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let hermix_data = manifest_dir.join("../../../hermix-data");
        if hermix_data.is_dir() {
            return hermix_data;
        }
    }
    // Last-ditch: next to the executable.
    if let Ok(install_root) = portable_env::compute_install_root() {
        return install_root.join("data");
    }
    PathBuf::from(".")
}

/// Return the current effective data directory for the running
/// desktop app.
///
/// **Preferred command** (Phase 10 — Portable Desktop):
/// `invoke<string>("get_data_dir")` returns the absolute path to the
/// directory where user data (config, memory, skills, sessions,
/// auth tokens) currently lives. In portable mode this is
/// `<install_root>/data`; in non-portable mode it falls back to the
/// platform config dir.
///
/// Front-end code should call this (not `get_settings_dir`) when it
/// needs to show the user "your data is here" — the value is the
/// single source of truth for "where does Hermix Desktop store my
/// stuff on this machine?".
#[tauri::command]
pub fn get_data_dir() -> Result<String, String> {
    let path = resolve_data_dir();
    Ok(path.to_string_lossy().to_string())
}

/// `get_settings_dir` 于 2026-10-18 移除（零调用）：前端已全部迁到
/// `get_data_dir`，其"保留一个 release 的 deprecated alias"承诺的那个
/// release 早已过去，留着只会误导新调用方。

#[tauri::command]
pub async fn pick_directory(app: tauri::AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let result = app
        .dialog()
        .file()
        .set_title("选择目录")
        .blocking_pick_folder();
    // FilePath implements Display, so use .to_string() instead of to_string_lossy()
    Ok(result.map(|p| p.to_string()))
}

#[tauri::command]
pub async fn open_external_url(url: String) -> Result<(), String> {
    open::that(&url).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn restart_app(app: tauri::AppHandle) {
    // app.restart() 是「先 spawn 新进程再退出」：不先放单实例锁，
    // 新进程会把自己误判成第二实例直接退出（见 single_instance.rs）。
    crate::single_instance::release();
    app.restart();
}

// ── Diagnostics / log surface (P0-7 / STABILITY-PLAN §3.1) ──────────
//
// The packaged Tauri app's stderr is dropped by `windows_subsystem =
// "windows"`, so end-user reports of "the agent stopped responding" used
// to be uninvestigatable. The Rust sidecar now writes a rolling log
// file to `<hermix_home>/logs/`, and these commands let the frontend
// tail that file, open the directory in Explorer/Finder, and export
// a snapshot for support tickets.

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogFileInfo {
    /// Absolute directory containing the rolling log files.
    pub dir: String,
    /// All rotated log files in the directory, newest first.
    pub files: Vec<LogFileEntry>,
    /// Combined byte size of all listed files (sanity check for the
    /// diagnostics panel — disk-pressure indicator).
    pub total_bytes: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogFileEntry {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    /// Last-modification unix seconds; used to sort "newest first".
    pub modified_secs: i64,
}

/// Path to the canonical log directory. Mirrors the default in
/// `hermix-telemetry::init_telemetry_from_env` (which writes
/// `<hermix_home>/logs/server.<date>.log`). Kept in sync via the same
/// `hermix_home()` rule; we don't import hermix-config (desktop crate
/// is a standalone workspace) so we recompute the same way.
fn log_dir() -> PathBuf {
    resolve_data_dir().join("logs")
}

/// Read the log directory and return metadata for every file in it.
/// Returns an empty list (not an error) when the directory does not
/// exist yet — first-launch users should not see "command failed".
#[tauri::command]
pub fn list_backend_log_files() -> Result<LogFileInfo, String> {
    let dir = log_dir();
    let mut files: Vec<LogFileEntry> = Vec::new();
    let mut total_bytes: u64 = 0;
    if dir.is_dir() {
        let entries = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;
        for e in entries.flatten() {
            let p = e.path();
            if !p.is_file() {
                continue;
            }
            let meta = match e.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let size = meta.len();
            total_bytes = total_bytes.saturating_add(size);
            let modified_secs = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            files.push(LogFileEntry {
                name: p
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                path: p.to_string_lossy().into_owned(),
                size_bytes: size,
                modified_secs,
            });
        }
        files.sort_by(|a, b| b.modified_secs.cmp(&a.modified_secs));
    }
    Ok(LogFileInfo {
        dir: dir.to_string_lossy().into_owned(),
        files,
        total_bytes,
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendLogTail {
    pub path: String,
    pub file_name: String,
    pub total_lines: u64,
    /// Last `tail_lines` lines (or fewer when the file is shorter).
    /// Newest-line-last ordering — matches `tail -n` for human reading.
    pub lines: Vec<String>,
}

/// Return the last `tail_lines` of the most recently modified log file.
/// Falls back to the legacy single file `server.log` when present.
///
/// async + spawn_blocking：日志可能滚到几十 MB，同步整读会冻结调用方；
/// 这里用 Seek 只读尾部，行数统计用流式计数（不整读进内存）。
#[tauri::command]
pub async fn read_backend_log_tail(tail_lines: u64) -> Result<BackendLogTail, String> {
    tokio::task::spawn_blocking(move || read_backend_log_tail_blocking(tail_lines))
        .await
        .map_err(|e| format!("log tail task join failed: {e}"))?
}

/// 单次回读块大小（从文件尾向前，直到凑够 wanted 行）。
const LOG_TAIL_CHUNK: u64 = 64 * 1024;
/// 尾部读取总量上限——wanted 很大或行超长时不至于把内存读爆。
const LOG_TAIL_MAX_BYTES: u64 = 4 * 1024 * 1024;

fn read_backend_log_tail_blocking(tail_lines: u64) -> Result<BackendLogTail, String> {
    let dir = log_dir();
    if !dir.is_dir() {
        return Err(format!(
            "日志目录不存在：{}（首次启动尚未生成日志）",
            dir.display()
        ));
    }
    let mut newest: Option<PathBuf> = None;
    let mut newest_secs: i64 = 0;
    for e in std::fs::read_dir(&dir).map_err(|e| e.to_string())?.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let secs = e
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if secs >= newest_secs {
            newest_secs = secs;
            newest = Some(p);
        }
    }
    let path = match newest {
        Some(p) => p,
        None => return Err(format!("日志目录为空：{}", dir.display())),
    };

    use std::io::{Read, Seek, SeekFrom, BufRead, BufReader};
    let wanted = tail_lines.max(1) as usize;

    // 流式统计总行数：O(size) 但只占常数内存，在 blocking 线程不碍 UI。
    let total_lines = {
        let f = std::fs::File::open(&path).map_err(|e| e.to_string())?;
        let mut reader = BufReader::with_capacity(256 * 1024, f);
        let mut count: u64 = 0;
        loop {
            let consumed = {
                let buf = reader.fill_buf().map_err(|e| e.to_string())?;
                count += buf.iter().filter(|&&b| b == b'\n').count() as u64;
                buf.len()
            };
            if consumed == 0 {
                break;
            }
            reader.consume(consumed);
        }
        count
    };

    // 从文件尾按块回读，直到凑够 wanted 个换行或触达上限。
    let mut f = std::fs::File::open(&path).map_err(|e| e.to_string())?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let mut taken: u64 = 0;
    let mut pos = len;
    while pos > 0 && taken < LOG_TAIL_MAX_BYTES {
        let chunk = LOG_TAIL_CHUNK.min(pos).min(LOG_TAIL_MAX_BYTES - taken);
        pos -= chunk;
        taken += chunk;
        f.seek(SeekFrom::Start(pos)).map_err(|e| e.to_string())?;
        let mut tmp = vec![0u8; chunk as usize];
        f.read_exact(&mut tmp).map_err(|e| e.to_string())?;
        let newlines = tmp.iter().filter(|&&b| b == b'\n').count();
        chunks.push(tmp);
        if newlines > wanted {
            break;
        }
    }
    let mut tail_bytes = Vec::with_capacity(taken as usize);
    for c in chunks.iter().rev() {
        tail_bytes.extend_from_slice(c);
    }
    let contents = String::from_utf8_lossy(&tail_bytes);
    let mut all_lines: Vec<&str> = contents.split('\n').collect();
    // 回读窗口可能从某行中间开始：若尚未覆盖整个文件，丢弃首段残行。
    if pos > 0 {
        if all_lines.len() > 1 {
            all_lines.remove(0);
        } else {
            all_lines.clear();
        }
    }
    let start = all_lines.len().saturating_sub(wanted);
    let lines: Vec<String> = all_lines[start..]
        .iter()
        .map(|s| s.trim_end_matches('\r').to_string())
        .collect();
    Ok(BackendLogTail {
        path: path.to_string_lossy().into_owned(),
        file_name: path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        total_lines,
        lines,
    })
}

/// Open the log directory in the OS file explorer.
#[tauri::command]
pub fn open_log_directory() -> Result<String, String> {
    let dir = log_dir();
    if !dir.exists() {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    let p = dir.to_string_lossy().into_owned();
    open::that(&p).map_err(|e| e.to_string())?;
    Ok(p)
}

#[tauri::command]
pub async fn get_server_status(state: tauri::State<'_, AppState>) -> Result<ServerStatus, String> {
    // stdio transport — no TCP port. Readiness is "child alive AND bridge
    // round-trips /health". We round-trip a real HTTP frame through the
    // bridge (rather than just checking `is_healthy()`) so a stuck
    // writer — child still alive but stdout pipe buffer full — is also
    // surfaced as not-healthy. The 2s budget mirrors the TCP era's
    // reqwest::get timeout.
    let running = {
        let mut sb = state.sidecar.lock().map_err(|e| e.to_string())?;
        sb.is_running()
    };

    let healthy = if running {
        match crate::stdio_bridge::global() {
            Some(bridge) => {
                let fut = bridge.http_request("GET", "/health", None);
                matches!(
                    tokio::time::timeout(std::time::Duration::from_secs(2), fut).await,
                    Ok(Ok(reply)) if reply.contains("\"status\":\"ok\"")
                )
            }
            None => false,
        }
    } else {
        false
    };

    Ok(ServerStatus {
        running,
        healthy,
        port: 0,
        base_url: "stdio://local".into(),
    })
}

#[derive(serde::Serialize)]
pub struct ServerStatus {
    pub running: bool,
    pub healthy: bool,
    /// Legacy field, kept at `0` because the stdio transport doesn't
    /// bind a TCP port. UI code that still reads it (status panes,
    /// health banners) sees `0` and falls through to the `healthy`
    /// boolean instead of trying to build a TCP URL.
    pub port: u16,
    /// Legacy field, kept as a sentinel so any caller still diffing
    /// against the TCP-era `http://127.0.0.1:3000` string can detect
    /// the transport switch and fall back to its bridge-invoke path.
    pub base_url: String,
}

/// Forward an HTTP-style request to the stdio bridge. The backend
/// (`hermix-server`) exposes the same REST surface via the `http`
/// channel inside its stdio multiplex — this command is the single
/// chokepoint the front-end uses for everything that was previously
/// `fetch('http://127.0.0.1:3000/api/...')`. The reply is the raw
/// JSON envelope string (`{"status":N,"body":"…","enc":"utf8"|"base64"}`)
/// from the server; callers parse it on the JS side so this layer stays
/// transport-only and doesn't duplicate HTTP status branching.
#[tauri::command]
pub async fn backend_http_request(
    method: String,
    path: String,
    body: Option<String>,
) -> Result<String, String> {
    // 桥不在位 / 该代已死时返回 503 信封而不是 Err 字符串：前者是
    // "后端不可达"（可重试、用户应看到离线提示），后者会被前端当成
    // 协议异常。返回信封让前端与其他失败走同一条分类链路。
    if !crate::stdio_bridge::is_live() {
        return Ok("{\"status\":503,\"body\":\"backend offline\",\"enc\":\"utf8\"}".into());
    }
    let Some(bridge) = crate::stdio_bridge::global() else {
        return Ok("{\"status\":503,\"body\":\"backend offline\",\"enc\":\"utf8\"}".into());
    };
    bridge.http_request(&method, &path, body.as_deref()).await
}

/// Send an upstream frame on a non-HTTP channel (e.g. `stream:<sid>`,
/// `groupchat:<room>`, or a custom app-specific channel). Payload is
/// the verbatim JSON the server expects on that channel — the bridge
/// does not parse or wrap it, so callers own their protocol shape.
/// Used by the streaming hooks to send `SendMessageRequest` /
/// `groupchat send` frames after the WebSocket surface was retired.
#[tauri::command]
pub async fn backend_channel_send(channel: String, payload: String) -> Result<(), String> {
    if !crate::stdio_bridge::is_live() {
        return Err("backend offline".to_string());
    }
    let bridge = crate::stdio_bridge::global()
        .ok_or_else(|| "backend offline".to_string())?;
    bridge.send_to_channel(&channel, &payload)
}

/// Real liveness probe used by `BackendStatus` and `checkBackendHealth`.
///
/// Round-trips an actual `/health` frame through the bridge instead of
/// reporting "a bridge object exists". The old implementation returned
/// `global().is_some()`, which is true from the instant the sidecar is
/// spawned until the process exits — including every case where the child
/// is a zombie, the stdout reader died, or the server stopped answering.
/// Those are exactly the states where the user sees a green "connected"
/// dot while every send fails, so the probe must observe the child, not
/// the shell's own bookkeeping.
///
/// Budget: 2s. The bridge has its own 120s oneshot timeout; a dead child
/// normally wakes waiters immediately via the stdout-EOF path, but a
/// half-open pipe would otherwise hang the caller for two minutes.
#[tauri::command]
pub async fn backend_ready() -> bool {
    if !crate::stdio_bridge::is_live() {
        return false;
    }
    let Some(bridge) = crate::stdio_bridge::global() else {
        return false;
    };
    match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        bridge.http_request("GET", "/health", None),
    )
    .await
    {
        // 信封是 `{"status":N,"body":"…"}`；200 即真正在位。
        Ok(Ok(reply)) => serde_json::from_str::<serde_json::Value>(&reply)
            .ok()
            .and_then(|v| v.get("status").and_then(|s| s.as_u64()))
            .is_some_and(|s| (200..300).contains(&s)),
        Ok(Err(e)) => {
            eprintln!("[Hermix] backend_ready: probe failed: {e}");
            false
        }
        Err(_) => {
            eprintln!("[Hermix] backend_ready: probe timed out");
            false
        }
    }
}

/// Expand Windows environment-variable segments (`%TEMP%\x\y.csv`,
/// `%USERPROFILE%\…`) before path anchoring. The LLM legitimately writes
/// paths in this form (it's how the tool output echoes them); without
/// expansion the `%` survives into PathBuf, the file is never found, and
/// every click on the chip reports "文件不存在或不在工作区内". Unknown or
/// unset variables are left verbatim so the caller's error still names
/// what the LLM actually wrote.
fn expand_env_vars(path: &str) -> String {
    if !path.contains('%') {
        return path.to_string();
    }
    let mut out = String::with_capacity(path.len());
    let mut rest = path;
    while let Some(start) = rest.find('%') {
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(val) => out.push_str(&val),
                    Err(_) => out.push_str(&rest[start..start + end + 2]),
                }
                rest = &after[end + 1..];
            }
            _ => {
                out.push_str(rest);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Read a file for the "产物" (artifact) preview pane in the right sidebar or
/// the in-chat inline preview popover.
///
/// Security: only files under the configured allowed roots can be read. Roots
/// come from `HERMES_DESKTOP_ARTIFACT_ROOTS` (comma-separated absolute paths)
/// and fall back to the user's home directory. Symlinks and `..` are resolved
/// before the check, so a path like `/root/../etc/passwd` is rejected.
///
/// Relative paths: if `path` is not absolute, it's resolved against
/// `workspace_root` (if provided) → `HERMES_DESKTOP_WORKSPACE_ROOT` env →
/// `HERMES_DESKTOP_ARTIFACT_ROOTS`[0] → `~/`. This is the fix for the bug
/// where the LLM says "保存到 tmp/xx/方案.md" but the path is relative and
/// can't be opened on the user's machine.
///
/// Returns:
/// - `kind = "bytes"`  → `data_base64` holds the raw file bytes; the shared
///   front-end layer decides text vs binary (and which encoding to decode
///   with) — see `packages/preview/src/lib/textDecode.ts`
/// - `kind = "empty"`  → `data_base64` is `null`; UI shows an empty-state hint
/// - `resolved_path`   → the canonical absolute path (so the front-end can
///                       display it back to the user and update its records)
#[tauri::command]
pub async fn read_artifact_file(
    path: String,
    workspace_root: Option<String>,
) -> Result<ArtifactContent, String> {
    const MAX_BYTES: u64 = 2 * 1024 * 1024; // 2 MiB

    let raw = PathBuf::from(expand_env_vars(&path));
    if raw.as_os_str().is_empty() {
        return Err("empty path".to_string());
    }

    // If relative, anchor against the best available base. This makes the
    // artifact pane robust to LLM output like "tmp/xx/方案.md" or
    // "tmp\\xx\\方案.md" by joining it onto the user's workspace.
    let anchored: PathBuf = if raw.is_absolute() {
        raw
    } else {
        let base = workspace_root
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| env_compat("HERMIX_DESKTOP_WORKSPACE_ROOT", "HERMES_DESKTOP_WORKSPACE_ROOT").map(PathBuf::from))
            .or_else(|| allowed_roots().into_iter().next())
            .or_else(dirs::home_dir)
            .ok_or_else(|| "no workspace base available for relative path".to_string())?;
        base.join(&raw)
    };

    // Canonicalize resolves `..` and symlinks; fails if the file doesn't exist.
    let canonical = anchored
        .canonicalize()
        .map_err(|e| format!("cannot resolve path '{}': {e}", anchored.display()))?;

    if !is_under_allowed_root_with(&canonical, workspace_root.as_deref()) {
        return Err(format!(
            "path '{}' is outside the allowed artifact roots",
            canonical.display()
        ));
    }

    let metadata = std::fs::metadata(&canonical)
        .map_err(|e| format!("cannot stat file: {e}"))?;
    if !metadata.is_file() {
        return Err("not a regular file".to_string());
    }
    if metadata.len() > MAX_BYTES {
        return Err(format!(
            "file too large: {} bytes (max {})",
            metadata.len(),
            MAX_BYTES
        ));
    }

    let bytes = std::fs::read(&canonical).map_err(|e| format!("cannot read file: {e}"))?;
    let size = bytes.len() as u64;

    let resolved = strip_unc_prefix(canonical.as_os_str());

    if bytes.is_empty() {
        return Ok(ArtifactContent {
            kind: "empty",
            data_base64: None,
            size,
            resolved_path: Some(resolved),
        });
    }

    // 载荷是**原始字节**而不是字符串：Windows 上记事本存的 txt 常是
    // UTF-16 LE，国内老软件导出的 csv 常是 GBK。Rust 侧一刀
    // `String::from_utf8` 会把它们判成二进制（严格模式）或解成乱码
    // （宽松模式）。编码判定与二进制嗅探统一放在共享前端层
    // （packages/preview/src/lib/textDecode.ts），那边才拿得到 BOM。
    use base64::Engine;
    let data_base64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(ArtifactContent {
        kind: "bytes",
        data_base64: Some(data_base64),
        size,
        resolved_path: Some(resolved),
    })
}

/// Read an artifact's **raw bytes** as base64. Used by the front-end
/// for binary formats (xlsx, xls, csv-as-bytes for SheetJS, docx
/// later) where parsing needs the original byte stream rather than a
/// UTF-8 string. Without this the front-end would either have to
/// shell out to a sidecar (LibreOffice) or re-implement the
/// allowlist / size cap, and we already do that work here.
///
/// Size cap is **10 MiB** (vs 2 MiB for the text path) because a
/// 5 MiB .xlsx with embedded images is normal in real workflows.
/// Returns `base64Content` plus the canonical `resolvedPath` so the
/// UI can update its artifact record with the real on-disk path.
#[tauri::command]
pub async fn read_artifact_file_bytes(
    path: String,
    workspace_root: Option<String>,
) -> Result<ArtifactBytesContent, String> {
    const MAX_BYTES: u64 = 10 * 1024 * 1024; // 10 MiB

    let raw = PathBuf::from(expand_env_vars(&path));
    if raw.as_os_str().is_empty() {
        return Err("empty path".to_string());
    }

    let anchored: PathBuf = if raw.is_absolute() {
        raw
    } else {
        let base = workspace_root
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| env_compat("HERMIX_DESKTOP_WORKSPACE_ROOT", "HERMES_DESKTOP_WORKSPACE_ROOT").map(PathBuf::from))
            .or_else(|| allowed_roots().into_iter().next())
            .or_else(dirs::home_dir)
            .ok_or_else(|| "no workspace base available for relative path".to_string())?;
        base.join(&raw)
    };

    let canonical = anchored
        .canonicalize()
        .map_err(|e| format!("cannot resolve path '{}': {e}", anchored.display()))?;

    if !is_under_allowed_root_with(&canonical, workspace_root.as_deref()) {
        return Err(format!(
            "path '{}' is outside the allowed artifact roots",
            canonical.display()
        ));
    }

    let metadata = std::fs::metadata(&canonical)
        .map_err(|e| format!("cannot stat file: {e}"))?;
    if !metadata.is_file() {
        return Err("not a regular file".to_string());
    }
    if metadata.len() > MAX_BYTES {
        return Err(format!(
            "file too large: {} bytes (max {})",
            metadata.len(),
            MAX_BYTES
        ));
    }

    let bytes = std::fs::read(&canonical).map_err(|e| format!("cannot read file: {e}"))?;
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let resolved = strip_unc_prefix(canonical.as_os_str());

    Ok(ArtifactBytesContent {
        kind: "bytes",
        content_base64: b64,
        size: bytes.len() as u64,
        resolved_path: Some(resolved),
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactBytesContent {
    pub kind: &'static str, // always "bytes"
    pub content_base64: String,
    pub size: u64,
    pub resolved_path: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactContent {
    /// "bytes" | "empty" —— 文本 / 二进制的判定在共享前端层做
    /// （packages/preview/src/lib/textDecode.ts），那边才拿得到 BOM。
    pub kind: &'static str,
    pub data_base64: Option<String>,
    pub size: u64,
    /// The canonical absolute path that was actually read. Helps the front-end
    /// display "found at <path>" when the original request was relative, and
    /// lets the artifact list update its records with the real path.
    pub resolved_path: Option<String>,
}

/// Check whether `canonical` lives under one of the allowed artifact roots.
/// When `workspace_root` is provided by the caller, that workspace is also
/// trusted — the user explicitly told the agent to work there, so files
/// inside that directory are obviously in-scope, even when the global
/// allowed-roots list is the conservative `~/` default and the workspace
/// lives on another drive (e.g. `D:\_hermix_tmp`).
fn is_under_allowed_root_with(canonical: &Path, workspace_root: Option<&str>) -> bool {
    let roots = effective_allowed_roots(workspace_root);
    if roots.is_empty() {
        return false;
    }
    roots.iter().any(|r| canonical.starts_with(r))
}

/// Strip the `\\?\` Win32 long-path prefix (and the `\\?\UNC\`
/// variant) from a path before handing it back to the user.
///
/// Windows' `canonicalize` returns `\\?\C:\foo\bar` and most
/// user-facing programs (cmd.exe, Office preview, the in-app file
/// tree) refuse that namespace form. Two variants we have to
/// handle:
///
/// - `\\?\C:\foo\bar` → `C:\foo\bar`
/// - `\\?\UNC\server\share\foo` → `\\server\share\foo`
///
/// The second case is easy to miss: dropping `\\?\UNC` entirely
/// would leave an orphaned `server\share` literal that no shell
/// recognises as a UNC path. We re-prefix `\\` to preserve the
/// `\\server\share` shape that the Win32 file dialog and
/// `os.path.exists` both expect.
///
/// Mirrors `hermix_core::path_security::strip_win32_long_path` —
/// kept duplicated here to avoid pulling `hermix_core` (with its
/// `tokio`, `reqwest`, etc. transitive deps) into the Tauri
/// command surface where this is the only function we need from
/// that module. If the helper grows further, fold them together.
fn strip_unc_prefix(s: &std::ffi::OsStr) -> String {
    let lossy = s.to_string_lossy();
    if let Some(rest) = lossy.strip_prefix(r"\\?\UNC\") {
        // \\?\UNC\server\share → \\server\share
        format!(r"\\{}", rest)
    } else if let Some(rest) = lossy.strip_prefix(r"\\?\") {
        // \\?\C:\foo → C:\foo
        rest.to_string()
    } else {
        lossy.to_string()
    }
}

fn allowed_roots() -> Vec<PathBuf> {
    if let Some(raw) = env_compat("HERMIX_DESKTOP_ARTIFACT_ROOTS", "HERMES_DESKTOP_ARTIFACT_ROOTS") {
        let parsed: Vec<PathBuf> = raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.exists())
            .filter_map(|p| p.canonicalize().ok())
            .collect();
        if !parsed.is_empty() {
            return parsed;
        }
    }
    // Default: user home directory (covers typical dev projects).
    dirs::home_dir()
        .and_then(|p| p.canonicalize().ok())
        .into_iter()
        .collect()
}

/// Per-call allowed roots, augmenting [`allowed_roots`] with the workspace
/// root passed by the caller. The workspace root is the directory the user
/// explicitly told the agent to work in (via the "工作目录" chip or the
/// Tauri-launched sidecar) — files there are obviously in-scope, but the
/// global allowed_roots list might be the conservative `~/` default and
/// would reject a workspace on another drive (e.g. `D:\_hermix_tmp`).
fn effective_allowed_roots(workspace_root: Option<&str>) -> Vec<PathBuf> {
    let mut roots = allowed_roots();
    if let Some(ws) = workspace_root {
        let p = PathBuf::from(ws);
        if let Ok(canonical) = p.canonicalize() {
            if !roots.iter().any(|r| r == &canonical) {
                roots.push(canonical);
            }
        }
    }
    roots
}

#[tauri::command]
pub async fn restart_sidecar(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    // 必须 async：`restart()` → `stop()` 会等最多 3s 优雅退出再 kill/wait，
    // 同步命令会在主线程上把这 3s 卡死，窗口直接无响应（用户点"重启"
    // 看到的就是界面冻住）。spawn_blocking 把它挪到阻塞线程池，
    // 命令立即返回、UI 保持可交互。
    let sb = state.sidecar.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut guard = sb.lock().map_err(|e| format!("lock: {e}"))?;
        guard.restart(&app)?;
        Ok::<_, String>("restarted".to_string())
    })
    .await
    .map_err(|e| format!("restart task failed: {e}"))?
}


/// 文档 → Markdown。预览面板的文本视图、以及没有纯前端解析器的老二进制
/// Office 格式（doc / xls / ppt）都走这条路径：markitdown sidecar 抽取
/// 正文，可搜索、可复制，比内嵌 HTML 更适合快速扫读。
const MARKITDOWN_EXTENSIONS: &[&str] = &[
    "docx", "doc", "xlsx", "xls", "pptx", "ppt", "pdf", "html", "htm", "epub",
    "csv", "tsv", "json", "xml", "msg", "zip", "rtf", "odt", "ods", "odp",
];

/// 前端一次渲染得动的 Markdown 上限。超过这个量级 MarkdownBody 会明显卡，
/// 而扫读场景也用不到全文 —— 与文本预览的 2 MiB 上限同一思路。
const MARKITDOWN_MAX_CHARS: u32 = 200_000;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentMarkdownResult {
    pub markdown: String,
    pub file_name: String,
    /// `true` 表示输出被 sidecar 截断，前端应提示"仅显示前 N 字符"。
    pub truncated: bool,
}

#[tauri::command]
pub async fn document_to_markdown(
    file_path: String,
    workspace_root: Option<String>,
) -> Result<DocumentMarkdownResult, String> {
    let raw = PathBuf::from(&file_path);
    if raw.as_os_str().is_empty() {
        return Err("empty path".to_string());
    }
    let anchored: PathBuf = if raw.is_absolute() {
        raw
    } else {
        let base = workspace_root
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir())
            .ok_or_else(|| "no workspace base for relative path".to_string())?;
        base.join(&raw)
    };
    let canonical = anchored
        .canonicalize()
        .map_err(|e| format!("cannot resolve '{}': {e}", anchored.display()))?;

    let ext = canonical
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !MARKITDOWN_EXTENSIONS.contains(&ext.as_str()) {
        return Err(format!("unsupported file type '.{ext}' for markdown conversion"));
    }

    let file_name = canonical
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("document")
        .to_string();

    let markdown = crate::markitdown::convert_to_markdown(&canonical, Some(MARKITDOWN_MAX_CHARS))
        .await?;
    let truncated = markdown.chars().count() >= MARKITDOWN_MAX_CHARS as usize;
    Ok(DocumentMarkdownResult { markdown, file_name, truncated })
}

/// Check whether an artifact path resolves to an existing regular file.
/// Uses the same anchoring / canonicalization logic as `read_artifact_file`
/// so relative paths are resolved against workspace_root and symlinks are
/// followed.
#[tauri::command]
pub async fn artifact_path_exists(
    path: String,
    workspace_root: Option<String>,
) -> bool {
    let raw = PathBuf::from(expand_env_vars(&path));
    if raw.as_os_str().is_empty() {
        return false;
    }

    let anchored: PathBuf = if raw.is_absolute() {
        raw
    } else {
        let base = workspace_root
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| env_compat("HERMIX_DESKTOP_WORKSPACE_ROOT", "HERMES_DESKTOP_WORKSPACE_ROOT").map(PathBuf::from))
            .or_else(|| allowed_roots().into_iter().next())
            .or_else(dirs::home_dir);
        match base {
            Some(b) => b.join(&raw),
            None => return false,
        }
    };

    // canonicalize fails if the path doesn't exist
    let canonical = match anchored.canonicalize() {
        Ok(c) => c,
        Err(_) => return false,
    };

    // Must be under allowed roots AND a regular file
    is_under_allowed_root_with(&canonical, workspace_root.as_deref())
        && std::fs::metadata(&canonical).map(|m| m.is_file()).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Directory listing for the "电脑" (Computer) tab in the right sidebar.
// ---------------------------------------------------------------------------

/// A single entry returned by [`list_directory`].
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirEntry {
    pub name: String,
    /// Canonical absolute path with the `\\?\` prefix stripped.
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    /// Modified time as Unix milliseconds. 0 if unavailable.
    pub modified_ms: u64,
    pub extension: Option<String>,
}

/// List the contents of a directory.
///
/// Uses the same security model as [`read_artifact_file`]: the
/// canonicalized directory path must live under one of the allowed
/// artifact roots (or the caller-provided `workspace_root`). This
/// replaces the direct `@tauri-apps/plugin-fs` `readDir` call which
/// was limited to `$HOME` by the Tauri capability scope and failed
/// for workspace directories on other drives (e.g. `F:\`).
#[tauri::command]
pub async fn list_directory(
    path: String,
    workspace_root: Option<String>,
) -> Result<Vec<DirEntry>, String> {
    let raw = PathBuf::from(&path);
    if raw.as_os_str().is_empty() {
        return Err("empty path".to_string());
    }

    // Resolve relative paths against workspace root.
    let anchored: PathBuf = if raw.is_absolute() {
        raw
    } else {
        let base = workspace_root
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| env_compat("HERMIX_DESKTOP_WORKSPACE_ROOT", "HERMES_DESKTOP_WORKSPACE_ROOT").map(PathBuf::from))
            .or_else(|| allowed_roots().into_iter().next())
            .or_else(dirs::home_dir)
            .ok_or_else(|| "no workspace base available for relative path".to_string())?;
        base.join(&raw)
    };

    // Canonicalize resolves `..` and symlinks.
    let canonical = anchored
        .canonicalize()
        .map_err(|e| format!("cannot resolve path '{}': {e}", anchored.display()))?;

    if !is_under_allowed_root_with(&canonical, workspace_root.as_deref()) {
        return Err(format!(
            "path '{}' is outside the allowed artifact roots",
            canonical.display()
        ));
    }

    let entries = std::fs::read_dir(&canonical)
        .map_err(|e| format!("cannot read directory '{}': {e}", canonical.display()))?;

    let mut out: Vec<DirEntry> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            // Skip hidden files (dotfiles) — matches the previous
            // frontend behaviour and keeps the listing clean.
            if name.starts_with('.') {
                return None;
            }
            let meta = match e.metadata() {
                Ok(m) => m,
                Err(_) => return None,
            };
            let is_dir = meta.is_dir();
            let size = if is_dir { 0 } else { meta.len() };
            let modified_ms = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let extension = e
                .path()
                .extension()
                .and_then(|x| x.to_str())
                .map(|s| s.to_ascii_lowercase());

            // Build the full path using the OS separator so Windows
            // paths stay as `C:\Users\…\file.txt`, not `C:\Users\…/file.txt`.
            let full_path = strip_unc_prefix(e.path().as_os_str());

            Some(DirEntry {
                name,
                path: full_path,
                is_dir,
                size,
                modified_ms,
                extension,
            })
        })
        .collect();

    // Sort: directories first, then files, alphabetical within each group.
    out.sort_by(|a, b| {
        match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        }
    });

    Ok(out)
}

// ---------------------------------------------------------------------------
// Python runtime probe (used by Settings → Backend tab → "Python 运行时" UI)
// ---------------------------------------------------------------------------

/// One candidate Python interpreter probed by [`python_detect`].
#[derive(serde::Serialize)]
pub struct PythonProbeEntry {
    /// `override` | `sibling-slot` | `PATH:<candidate>` | `env:HERMIX_PYTHON`
    pub source: String,
    /// Absolute path that would be invoked.
    pub path: String,
    /// Whether the file exists on disk.
    pub exists: bool,
    /// If we actually spawned it: the captured `--version` line.
    pub version: Option<String>,
}

#[derive(serde::Serialize)]
pub struct PythonProbeResult {
    pub entries: Vec<PythonProbeEntry>,
    /// The first entry whose `exists = true`; mirrors the resolver in
    /// `crates/hermix-tools/src/backends/code_execution.rs`.
    pub selected: Option<String>,
}

/// Probe Python interpreters on this machine. Mirrors the resolver
/// in `hermix_tools::backends::code_execution::python_search_paths`
/// but is duplicated here so the desktop crate stays standalone
/// (it opts out of the main workspace in its Cargo.toml).
///
/// Why we don't add `hermix-tools` as a dep here:
///   - Cross-workspace metadata + Cargo features churn is not
///     worth it for ~50 lines.
///   - The probe is read-only — drift between the two
///     implementations can be caught by `cargo test`.
#[tauri::command]
pub async fn python_detect(override_path: Option<String>) -> Result<PythonProbeResult, String> {
    let mut entries: Vec<PythonProbeEntry> = Vec::new();

    // 1. Caller-supplied override (Settings UI).
    if let Some(p) = override_path.as_deref().filter(|s| !s.trim().is_empty()) {
        let exists = Path::new(p).is_file();
        let version = if exists {
            spawn_python_version(Path::new(p)).await
        } else {
            None
        };
        entries.push(PythonProbeEntry {
            source: "override".into(),
            path: p.to_string(),
            exists,
            version,
        });
    }

    // 2. HERMIX_PYTHON env var.
    if let Ok(env_p) = std::env::var("HERMIX_PYTHON") {
        if !env_p.is_empty() && !entries.iter().any(|e| e.path == env_p) {
            let pb = PathBuf::from(&env_p);
            let exists = pb.is_file();
            let version = if exists {
                spawn_python_version(&pb).await
            } else {
                None
            };
            entries.push(PythonProbeEntry {
                source: "env:HERMIX_PYTHON".into(),
                path: env_p,
                exists,
                version,
            });
        }
    }

    // 3. Sibling slot of the running hermix binary.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            #[cfg(windows)]
            let sibling = dir.join("python").join("python.exe");
            #[cfg(not(windows))]
            let sibling = dir.join("python").join("bin").join("python3");
            if !entries.iter().any(|e| e.path == sibling.display().to_string()) {
                let exists = sibling.is_file();
                let version = if exists {
                    spawn_python_version(&sibling).await
                } else {
                    None
                };
                entries.push(PythonProbeEntry {
                    source: "sibling-slot".into(),
                    path: sibling.display().to_string(),
                    exists,
                    version,
                });
            }
        }
    }

    // 4. PATH probe.
    #[cfg(windows)]
    const CANDIDATES: &[&str] = &["python", "py", "python3", "python2"];
    #[cfg(not(windows))]
    const CANDIDATES: &[&str] = &["python3", "python", "python2"];
    for cand in CANDIDATES {
        let resolved = which::which(cand).ok();
        let path = resolved.clone().unwrap_or_else(|| PathBuf::from(cand));
        let path_str = path.display().to_string();
        if entries.iter().any(|e| e.path == path_str) {
            continue;
        }
        let exists = resolved.is_some();
        let version = if exists {
            spawn_python_version(&path).await
        } else {
            None
        };
        entries.push(PythonProbeEntry {
            source: format!("PATH:{cand}"),
            path: path_str,
            exists,
            version,
        });
    }

    let selected = entries
        .iter()
        .find(|e| e.exists)
        .map(|e| e.path.clone());
    Ok(PythonProbeResult { entries, selected })
}

/// Spawn `<python> --version` with a 2s timeout and capture the version
/// string. Returns `None` on any failure (timeout / non-zero exit / IO
/// error) — the caller treats that as "exists but didn't return a
/// version", which still beats nothing.
async fn spawn_python_version(python: &Path) -> Option<String> {
    spawn_version(python, "--version", "Python").await
}

/// 通用 `--version` 探测：node / git / python 都是 `<exe> --version`，
/// 只是答案里该匹配的关键字不同（"v22.11.0" / "git version 2.55" /
/// "Python 3.13.1"）。统一走 stdout+stderr 拼接，取第一行非空输出。
async fn spawn_version(exe: &Path, flag: &str, keyword: &str) -> Option<String> {
    use tokio::process::Command as TokioCommand;
    // src-tauri 刻意不依赖 hermix-core（见文件头说明），所以标志就地加。
    // 探测运行时版本是纯后台动作，弹控制台黑窗没有任何信息价值。
    let mut cmd = TokioCommand::new(exe);
    cmd.arg(flag)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let output = cmd.output().await.ok()?;
    // 各运行时把版本写进 stdout 还是 stderr 没有统一约定（Windows
    // Python.org 写 stderr，git 写 stdout），两侧都读最稳。
    let mut out = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.stderr.is_empty() {
        out.push('\n');
        out.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    out.lines()
        .find(|l| l.contains(keyword))
        .or_else(|| out.lines().find(|l| !l.trim().is_empty()))
        .map(|s| s.trim().to_string())
}

// ---------------------------------------------------------------------------
// 内置运行时探测（Settings → 运行时 tab）
//
// 三类运行时共用一套"候选位 + 版本探测"逻辑，差别只在三张表：
//   · 内置位（sibling-slot）：随 exe 一起分发的目录，格式固定
//   · PATH 候选命令名
//   · 版本关键字
// 内置位存在的意义：便携版不该依赖用户机器上装没装 node/python/git。
// ---------------------------------------------------------------------------

/// 单条探测结果（source 说明这个路径是"内置 / 环境变量 / PATH / 覆盖"）。
#[derive(serde::Serialize)]
pub struct RuntimeProbeEntry {
    pub source: String,
    pub path: String,
    pub exists: bool,
    pub version: Option<String>,
}

#[derive(serde::Serialize)]
pub struct RuntimeProbeResult {
    pub entries: Vec<RuntimeProbeEntry>,
    pub selected: Option<String>,
}

struct RuntimeSpec {
    /// 内置位相对 exe 目录的子路径（便携分发时把运行时解压到这里）。
    sibling: &'static [&'static str],
    /// PATH 上要试的命令名。
    path_candidates: &'static [&'static str],
    /// 版本输出里的关键字（同时用于兜底取首行）。
    version_keyword: &'static str,
}

fn runtime_spec(kind: &str) -> Option<RuntimeSpec> {
    match kind {
        "python" => Some(RuntimeSpec {
            sibling: &["python/python.exe", "python/bin/python3"],
            path_candidates: &["python", "py", "python3"],
            version_keyword: "Python",
        }),
        "node" => Some(RuntimeSpec {
            sibling: &["node/node.exe", "node/bin/node"],
            path_candidates: &["node"],
            version_keyword: "v",
        }),
        // MinGit 的 git.exe 在 cmd/ 下（mingw64 变体在 bin/ 下），两个都试。
        "git" => Some(RuntimeSpec {
            sibling: &["git/cmd/git.exe", "git/bin/git.exe", "git/bin/git"],
            path_candidates: &["git"],
            version_keyword: "git version",
        }),
        _ => None,
    }
}

/// 探测一类运行时（node / python / git）的所有候选位。
///
/// 顺序：内置位 → PATH → 显式 override。内置位优先是刻意的：便携版里
/// 自带的运行时必须赢过用户机器上版本不明的同名命令，否则"内置"形同虚设。
#[tauri::command]
pub async fn runtime_detect(
    kind: String,
    override_path: Option<String>,
) -> Result<RuntimeProbeResult, String> {
    let spec = runtime_spec(&kind).ok_or_else(|| format!("unknown runtime kind: {kind}"))?;
    let mut entries: Vec<RuntimeProbeEntry> = Vec::new();

    let push = |source: String, path: String, entries: &mut Vec<RuntimeProbeEntry>| {
        if entries.iter().any(|e| e.path == path) {
            return;
        }
        let pb = PathBuf::from(&path);
        let exists = pb.is_file();
        entries.push(RuntimeProbeEntry {
            source,
            path,
            exists,
            version: None, // 下面统一补版本，闭包不能 async
        });
    };

    // 1. 内置位（sibling-slot）
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for rel in spec.sibling {
                push("builtin".into(), dir.join(rel).display().to_string(), &mut entries);
            }
        }
    }
    // 2. PATH
    for cand in spec.path_candidates {
        let resolved = which::which(cand).ok();
        let path = resolved
            .clone()
            .unwrap_or_else(|| PathBuf::from(cand))
            .display()
            .to_string();
        push(format!("PATH:{cand}"), path, &mut entries);
    }
    // 3. 显式覆盖（设置页手填）
    if let Some(p) = override_path.as_deref().filter(|s| !s.trim().is_empty()) {
        push("override".into(), p.to_string(), &mut entries);
    }

    // 版本探测只对真实存在的文件发起，避免为每个 PATH 未命中项 spawn 进程。
    for e in entries.iter_mut() {
        if e.exists {
            e.version = spawn_version(Path::new(&e.path), "--version", spec.version_keyword).await;
        }
    }

    let selected = entries.iter().find(|e| e.exists).map(|e| e.path.clone());
    Ok(RuntimeProbeResult { entries, selected })
}

/// Return runtime environment info for the desktop app.
///
/// The frontend's `desktopBridge.getRuntimeInfo()` calls this to
/// display OS, arch, version, and data directory in Settings → Backend.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInfo {
    pub runtime: String,
    pub platform: String,
    pub user_data_dir: String,
}

#[tauri::command]
pub fn get_runtime_info() -> Result<RuntimeInfo, String> {
    let platform = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "unknown"
    };

    let data_dir = resolve_data_dir();

    Ok(RuntimeInfo {
        runtime: format!("hermix-desktop"),
        platform: platform.to_string(),
        user_data_dir: data_dir.to_string_lossy().to_string(),
    })
}

/// Persist a Python interpreter override. The frontend writes this
/// before calling `python_detect`; the value is used by the resolver
/// in `hermix_tools::backends::code_execution::find_python_bin` if
/// we also set `HERMIX_PYTHON` in the sidecar environment.
///
/// We do both:
/// - Write `localStorage["hermix.python.override"]` on the frontend
///   side (so the UI shows the value back).
/// - Set `HERMIX_PYTHON` for the running process. The sidecar
///   (`hermix.exe`) inherits env vars from the Tauri launcher, but
///   the *running* Rust process needs it set in-place to take effect
///   immediately (no restart). See `portable_env::install_portable_env`
///   for the launch-time contract.
#[tauri::command]
pub fn set_python_override(path: Option<String>) -> Result<String, String> {
    // SAFETY: process-wide env mutation. Only safe in practice because
    // the Tauri command runs on the main thread before any other
    // spawned process reads `HERMIX_PYTHON`; Python candidates are
    // re-resolved on every `execute()` call so the new value is in
    // effect immediately.
    match path.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(p) => {
            unsafe { std::env::set_var("HERMIX_PYTHON", p) };
            Ok(p.to_string())
        }
        None => {
            unsafe { std::env::remove_var("HERMIX_PYTHON") };
            Ok(String::new())
        }
    }
}

// ── 文件关联（hermix-viewer 接管；HKCU only，用户显式授权）─────────
// 决策笔记 2026-10-09-standalone-file-viewer：按扩展名勾选，不静默抢关联。
// ProgID = Hermix.Viewer；取消勾选时恢复勾选前的旧默认值（Backup 子键）。

use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
use winreg::RegKey;

const VIEWER_PROGID: &str = "Hermix.Viewer";
/// 可勾选的扩展名白名单（信任边界：写注册表的入口必须过这扇门）。
const ASSOC_EXTS: &[&str] = &[
    "md", "markdown", "txt", "log", "json", "yaml", "yml", "toml", "csv",
    "png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "pdf",
];

fn viewer_exe_path() -> Result<std::path::PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("定位主程序失败: {e}"))?;
    let dir = exe.parent().ok_or_else(|| "无法定位安装目录".to_string())?;
    Ok(dir.join("hermix-viewer.exe"))
}

/// ProgID + open command（指向 viewer exe；缺 exe 如实报错，不写半套）。
fn ensure_viewer_progid() -> Result<(), String> {
    let viewer = viewer_exe_path()?;
    if !viewer.exists() {
        return Err("未找到 hermix-viewer.exe（完整安装包才包含查看器）".into());
    }
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (progid, _) = hkcu
        .create_subkey(format!("Software\\Classes\\{VIEWER_PROGID}"))
        .map_err(|e| format!("创建 ProgID 失败: {e}"))?;
    progid
        .set_value("", &"Hermix 文件查看器".to_string())
        .map_err(|e| format!("写 ProgID 名失败: {e}"))?;
    let (shell, _) = hkcu
        .create_subkey(format!(
            "Software\\Classes\\{VIEWER_PROGID}\\shell\\open\\command"
        ))
        .map_err(|e| format!("创建 open command 失败: {e}"))?;
    shell
        .set_value("", &format!("\"{}\" \"%1\"", viewer.display()))
        .map_err(|e| format!("写 open command 失败: {e}"))?;
    Ok(())
}

fn read_default(classes: &RegKey, path: &str) -> Option<String> {
    // 类型化读取（REG_SZ → String）；DWORD/其他类型或缺失 → None。
    // winreg 0.10 的 RegValue 是裸结构体，不手解 UTF-16。
    classes.get_value::<String, _>(path).ok()
}

/// 各扩展名当前勾选状态（owned = 默认值已指向 Hermix.Viewer）。
#[tauri::command]
pub fn file_assoc_list() -> Result<Vec<serde_json::Value>, String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let classes = hkcu
        .open_subkey("Software\\Classes")
        .map_err(|e| format!("打开注册表失败: {e}"))?;
    Ok(ASSOC_EXTS
        .iter()
        .map(|ext| {
            serde_json::json!({
                "ext": ext,
                "owned": read_default(&classes, &format!(".{ext}")).as_deref() == Some(VIEWER_PROGID),
            })
        })
        .collect())
}

/// 勾选/取消某扩展名（用户显式操作；取消时恢复旧默认值或清除）。
#[tauri::command]
pub fn file_assoc_set(ext: String, enable: bool) -> Result<(), String> {
    let ext = ext.trim().trim_start_matches('.').to_ascii_lowercase();
    if !ASSOC_EXTS.contains(&ext.as_str()) {
        return Err(format!("不支持的扩展名: {ext}"));
    }
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if enable {
        ensure_viewer_progid()?;
    }
    let classes = hkcu
        .open_subkey_with_flags("Software\\Classes", KEY_READ | KEY_WRITE)
        .map_err(|e| format!("打开注册表失败: {e}"))?;
    let path = format!(".{ext}");
    let cur = read_default(&classes, &path);
    if enable {
        // 备份现有非 Hermix 关联，取消勾选时可恢复。
        if let Some(prev) = cur.filter(|v| v != VIEWER_PROGID && !v.is_empty()) {
            if let Ok((bk, _)) = hkcu.create_subkey(format!("Software\\Classes\\{VIEWER_PROGID}\\Backup")) {
                let _ = bk.set_value(&ext, &prev);
            }
        }
        classes
            .set_value(&path, &VIEWER_PROGID.to_string())
            .map_err(|e| format!("写关联失败: {e}"))?;
    } else if cur.as_deref() == Some(VIEWER_PROGID) {
        let backup = hkcu
            .open_subkey(format!("Software\\Classes\\{VIEWER_PROGID}\\Backup"))
            .ok()
            .and_then(|bk| bk.get_value::<String, _>(ext.as_str()).ok())
            .filter(|s| !s.is_empty());
        match backup {
            Some(prev) => classes
                .set_value(&path, &prev)
                .map_err(|e| format!("恢复旧关联失败: {e}"))?,
            None => {
                let _ = classes.delete_value(path.as_str());
            }
        }
    }
    Ok(())
}

/// 在查看器中打开（主程序 → viewer 互调；命令行参数传路径，
/// viewer 单实例机制会把路径投递给已开窗口）。
///
/// path 可选：不带路径就是「打开文件管理器」——查看器按自己记住的
/// 上次目录进入资源管理器模式（Tauri v2 里 `Option<T>` 参数可以整个
/// 省略，缺失即 None）。
#[tauri::command]
pub fn open_in_viewer(path: Option<String>) -> Result<(), String> {
    let viewer = viewer_exe_path()?;
    if !viewer.exists() {
        return Err("未找到 hermix-viewer.exe（完整安装包才包含查看器）".into());
    }
    let mut cmd = std::process::Command::new(&viewer);
    if let Some(p) = path.filter(|p| !p.trim().is_empty()) {
        cmd.arg(p);
    }
    cmd.spawn().map_err(|e| format!("启动查看器失败: {e}"))?;
    Ok(())
}
