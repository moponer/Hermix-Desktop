use crate::AppState;
use crate::officecli;
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
fn resolve_data_dir() -> PathBuf {
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

/// **Deprecated**: kept for one release as an alias for [`get_data_dir`].
///
/// Phase 9 callers (and any third-party code that wired itself to
/// `get_settings_dir`) continue to work — the implementation now
/// resolves to the same place as `get_data_dir` instead of
/// `%APPDATA%\\hermix\\` (which would silently scatter data outside
/// the install root, defeating the whole portable-mode promise).
///
/// Front-end code MUST migrate to `get_data_dir`. The companion
/// `openSettingsDir` in `desktopBridge.ts` is also deprecated.
#[tauri::command]
pub fn get_settings_dir() -> Result<String, String> {
    eprintln!(
        "[Hermix] WARNING: 'get_settings_dir' is deprecated, use 'get_data_dir' instead"
    );
    get_data_dir()
}

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
    app.restart();
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
/// - `kind = "text"`   → content is the decoded UTF-8 string
/// - `kind = "binary"` → content is `null`; UI shows a binary placeholder
/// - `kind = "empty"`  → content is `""`; UI shows an empty-state hint
/// - `resolved_path`   → the canonical absolute path (so the front-end can
///                       display it back to the user and update its records)
#[tauri::command]
pub async fn read_artifact_file(
    path: String,
    workspace_root: Option<String>,
) -> Result<ArtifactContent, String> {
    const MAX_BYTES: u64 = 2 * 1024 * 1024; // 2 MiB

    let raw = PathBuf::from(&path);
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
            content: None,
            size,
            resolved_path: Some(resolved),
        });
    }

    match String::from_utf8(bytes) {
        Ok(s) => Ok(ArtifactContent {
            kind: "text",
            content: Some(s),
            size,
            resolved_path: Some(resolved),
        }),
        Err(_) => Ok(ArtifactContent {
            kind: "binary",
            content: None,
            size,
            resolved_path: Some(resolved),
        }),
    }
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

    let raw = PathBuf::from(&path);
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
    pub kind: &'static str, // "text" | "binary" | "empty"
    pub content: Option<String>,
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

const OFFICE_EXTENSIONS: &[&str] = &["docx", "doc", "xlsx", "xls", "pptx", "ppt"];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OfficePreviewResult {
    pub html_path: String,
    pub html_content: String,
    pub file_name: String,
}

/// Render an Office document (.docx/.xlsx/.pptx) to HTML using the
/// bundled `officecli` binary, and return the path to the generated
/// HTML file for preview in an iframe.
///
/// The HTML is cached in `$APP_DATA/preview_cache/` keyed by a hash
/// of the input path + modification time, so repeated previews of
/// the same unchanged file are instant.
#[tauri::command]
pub async fn render_office_preview(
    file_path: String,
    workspace_root: Option<String>,
) -> Result<OfficePreviewResult, String> {
    let raw = PathBuf::from(&file_path);
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
            .or_else(|| dirs::home_dir())
            .ok_or_else(|| "no workspace base for relative path".to_string())?;
        base.join(&raw)
    };

    let canonical = anchored
        .canonicalize()
        .map_err(|e| format!("cannot resolve '{}': {e}", anchored.display()))?;

    // Validate extension.
    let ext = canonical
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !OFFICE_EXTENSIONS.contains(&ext.as_str()) {
        return Err(format!(
            "unsupported file type '.{ext}'; supported: docx, doc, xlsx, xls, pptx, ppt"
        ));
    }

    let file_name = canonical
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("document")
        .to_string();

    // Resolve officecli binary.
    let officecli_bin = officecli::ensure_officecli()?;

    // Build output path in preview_cache.
    let data_dir = resolve_data_dir();
    let cache_dir = data_dir.join("preview_cache");
    std::fs::create_dir_all(&cache_dir)
        .map_err(|e| format!("cannot create preview cache: {e}"))?;

    // Use a deterministic hash: file path + last modified time.
    let meta = std::fs::metadata(&canonical)
        .map_err(|e| format!("cannot stat file: {e}"))?;
    let modified = meta
        .modified()
        .map(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        })
        .unwrap_or(0);
    let hash_input = format!("{}:{}", canonical.display(), modified);
    let hash = {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        hash_input.hash(&mut h);
        format!("{:016x}", h.finish())
    };
    let html_path = cache_dir.join(format!("{hash}.html"));

    // If cached HTML exists and is newer than the source, skip re-render.
    let needs_render = if html_path.exists() {
        html_path
            .metadata()
            .map(|m| m.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH) < meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH))
            .unwrap_or(true)
    } else {
        true
    };

    if needs_render {
        let output = tokio::process::Command::new(&officecli_bin)
            .args(["view", canonical.to_str().unwrap_or(""), "html"])
            .env("OFFICECLI_NO_AUTO_RESIDENT", "1")
            .creation_flags(0x08000000) // CREATE_NO_WINDOW: 防黑窗闪烁
            .output()
            .await
            .map_err(|e| format!("failed to run officecli: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("officecli failed: {}", stderr.trim()));
        }

        std::fs::write(&html_path, &output.stdout)
            .map_err(|e| format!("cannot write preview HTML: {e}"))?;
    }

    let resolved = strip_unc_prefix(html_path.as_os_str());
    let html_content = std::fs::read_to_string(&html_path)
        .map_err(|e| format!("cannot read cached preview: {e}"))?;
    Ok(OfficePreviewResult {
        html_path: resolved,
        html_content,
        file_name,
    })
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
    let raw = PathBuf::from(&path);
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
    use tokio::process::Command as TokioCommand;
    let output = TokioCommand::new(python)
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .ok()?;
    // `python --version` writes to stdout on Unix and stderr on
    // Windows Python.org builds; we concatenate both and look for the
    // first `Python X.Y.Z` token.
    let mut out = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.stderr.is_empty() {
        out.push('\n');
        out.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    out.lines()
        .find(|l| l.contains("Python"))
        .map(|s| s.trim().to_string())
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
