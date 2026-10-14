// Tauri IPC bridge — replaces Electron preload
import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import { writeFile } from "@tauri-apps/plugin-fs";
import { toastError } from "./hooks/useToast";

export async function pickDirectory(): Promise<string | null> {
  const selected = await open({ directory: true, title: "选择目录" });
  if (typeof selected === "string") return selected;
  if (selected && typeof selected === "object" && "path" in selected) return (selected as { path: string }).path;
  return null;
}

/**
 * 「导出文件」统一出口：save 对话框选目标路径 → plugin-fs 写字节。
 * 返回实际写入的绝对路径；用户取消对话框返回 null（不是错误，调用方
 * 静默即可）。写失败如实抛出，由调用方 toast。
 *
 * capability 依据：`dialog:allow-save` 与 fs 写权限已在
 * capabilities/default.json 白名单（此前无人走，此函数是首个消费者）。
 */
export async function saveBinaryFile(
  defaultName: string,
  bytes: Uint8Array,
  filters: { name: string; extensions: string[] }[],
): Promise<string | null> {
  const target = await save({ defaultPath: defaultName, filters });
  if (!target) return null;
  await writeFile(target, bytes);
  return target;
}

export async function getRuntimeInfo(): Promise<{ runtime: string; platform: string; userDataDir: string }> {
  return invoke("get_runtime_info");
}

/** 「运行会话时保持电脑唤醒」开关变化事件。开关在自动化页写入，
 *  App 层监听它并联动会话流式状态调用 set_keep_awake。 */
export const KEEP_AWAKE_CHANGED_EVENT = "hermix:keep-awake-changed";

/** 调用 Tauri 命令设置系统保活状态；失败静默（尽力而为的 OS 能力）。 */
export function setKeepAwake(on: boolean): void {
  invoke("set_keep_awake", { on }).catch((e) => {
    console.warn("[keep-awake] set_keep_awake failed:", e);
  });
}

/**
 * Open the user's data directory in the system file explorer.
 *
 * **Preferred API**: this is the portable-mode-aware version. It
 * returns the install-root `/data` directory when the app is running
 * in portable mode (the Tauri launcher injects `HERMIX_PORTABLE_DATA_DIR`
 * for the sidecar and the Rust commands resolve the same path), or
 * the platform config dir when running non-portable.
 *
 * Use this when you need to show the user "your data is here" —
 * it's the single source of truth for "where does Hermix Desktop
 * store my stuff on this machine?".
 */
export async function openDataDir(): Promise<void> {
  const path = await invoke<string>("get_data_dir");
  await invoke("open_external_url", { url: path });
}

/**
 * 数据目录路径本身（供设置页"数据目录"行展示）。
 * 走同一个 `get_data_dir` 命令，因此与 `openDataDir` 打开的必然是同一处，
 * 不会出现在"显示 A 路径 / 打开 B 路径"的错位。
 */
export async function getDataDir(): Promise<string> {
  return invoke<string>("get_data_dir");
}

export async function openExternalUrl(url: string): Promise<void> {
  try {
    await invoke("open_external_url", { url });
  } catch (e) {
    console.error("[desktopBridge] openExternalUrl failed:", url, e);
toastError("打开外部链接失败");
  }
}

/**
 * Open a local path (typically a workspace folder) in the system file explorer.
 *
 * Implementation: encode the path as a `file://` URL and route through
 * `open_external_url` (the `open` crate). On Windows this opens
 * Explorer at the folder; on macOS it opens Finder; on Linux it opens
 * the configured file manager.
 *
 * Note: in Tauri 2 the `tauri-plugin-opener` would be the more "native"
 * choice (and supports a `reveal` verb on Windows that selects the file
 * inside Explorer), but the existing app already uses the
 * `open_external_url` Tauri command everywhere, so we stay consistent
 * to keep the IPC surface small. If a future bug surfaces around
 * "show file in folder" (vs "open containing folder"), promote to
 * `tauri-plugin-opener::revealItemInDir`.
 */
export async function openInExplorer(absolutePath: string): Promise<void> {
  if (!absolutePath) return;
  // `file:///` works for both Windows and POSIX paths. We URL-encode each
  // path segment (but not the slashes) so spaces and CJK stay readable
  // when the OS shell hands them off to Explorer.
  const normalized = absolutePath.replace(/\\/g, "/");
  const encoded = normalized.split("/").map(encodeURIComponent).join("/");
  const url = `file:///${encoded.replace(/^\/+/, "")}`;
  try {
    await invoke("open_external_url", { url });
  } catch (e) {
    console.error("[desktopBridge] openInExplorer failed:", absolutePath, e);
    toastError("无法打开文件资源管理器");
  }
}

/**
 * Copy a plain-text string to the system clipboard.
 *
 * Prefers the async Clipboard API (requires a secure context — `https`,
 * `localhost`, or a Tauri `tauri://` origin); falls back to the legacy
 * `document.execCommand("copy")` for environments where the async API
 * is blocked (e.g. some embedded webviews or older Tauri builds).
 *
 * Returns `true` on success so callers can show a toast without an
 * extra try/catch.
 */
export async function copyText(text: string): Promise<boolean> {
  if (!text) return false;
  try {
    if (typeof navigator !== "undefined" && navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch (e) {
    console.warn("[desktopBridge] navigator.clipboard.writeText failed, falling back:", e);
  }
  // Legacy fallback: hidden textarea + execCommand. Only triggers in
  // a real user-gesture handler (which is true here — copy menu item
  // click), so the browser allows it.
  try {
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.setAttribute("readonly", "");
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    ta.style.pointerEvents = "none";
    document.body.appendChild(ta);
    ta.select();
    const ok = document.execCommand("copy");
    document.body.removeChild(ta);
    return ok;
  } catch (e) {
    console.error("[desktopBridge] legacy copy failed:", e);
    return false;
  }
}

export async function restartDesktopApp(): Promise<void> {
  await invoke("restart_app");
}

export async function getServerStatus(): Promise<{ running: boolean; healthy: boolean; port: number; base_url: string }> {
  return invoke("get_server_status");
}

/// Read a file from the local filesystem (used by the artifact preview pane
/// AND the in-chat inline preview popover).
/// /// The Tauri command validates the path is under `HERMES_DESKTOP_ARTIFACT_ROOTS`
/// (defaults to the user's home directory), caps at 2 MiB, and detects binary
/// content via UTF-8 decoding.
/// /// If `path` is relative, the backend anchors it against `workspaceRoot`
/// (or `HERMES_DESKTOP_WORKSPACE_ROOT` env, or the first allowed root, or
/// `~/`, in that order). This is what fixes the "tmp\xx\方案.md" bug: the
/// LLM often speaks in relative paths, and we now have a way to find them.
/// /// `resolvedPath` is the canonical absolute path the backend actually read —
/// the front-end stores this in the artifact record so subsequent opens
/// always succeed.
export async function readArtifactFile(
  path: string,
  workspaceRoot?: string | null,
): Promise<{
  kind: "text" | "binary" | "empty";
  content: string | null | undefined;
  size: number;
  resolvedPath?: string | null;
}> {
  return invoke("read_artifact_file", { path, workspaceRoot: workspaceRoot ?? null });
}

/// Read an artifact's raw bytes as a base64 string. Used by the
/// front-end parser (SheetJS, PapaParse) for binary formats like
/// `.xlsx` where UTF-8 decoding is lossy. Size cap is 10 MiB
/// (vs 2 MiB for the text path).
export async function readArtifactFileBytes(
  path: string,
  workspaceRoot?: string | null,
): Promise<{
  kind: "bytes";
  contentBase64: string;
  size: number;
  resolvedPath?: string | null;
}> {
  return invoke("read_artifact_file_bytes", { path, workspaceRoot: workspaceRoot ?? null });
}

/// Check whether an artifact path resolves to an existing file on disk.
/// Uses the same anchoring logic as `readArtifactFile` (relative paths
/// are resolved against workspaceRoot).
export async function artifactPathExists(
  path: string,
  workspaceRoot?: string | null,
): Promise<boolean> {
  return invoke("artifact_path_exists", { path, workspaceRoot: workspaceRoot ?? null });
}

/// Convert a document (Office / PDF / HTML / EPUB / CSV …) to Markdown
/// using the bundled `markitdown` sidecar. Used as the text-view path in
/// the preview panel and as the representation for legacy binary Office
/// formats (doc / xls / ppt) that have no in-browser parser — Markdown is
/// searchable and copyable, unlike an embedded HTML render.
export interface DocumentMarkdownResult {
  markdown: string;
  fileName: string;
  truncated: boolean;
}

export async function convertDocumentToMarkdown(
  filePath: string,
  workspaceRoot?: string | null,
): Promise<DocumentMarkdownResult> {
  return invoke("document_to_markdown", {
    filePath,
    workspaceRoot: workspaceRoot ?? null,
  });
}

/// List the contents of a directory for the "电脑" (Computer) tab.
/// Uses the same security model as `readArtifactFile` — the path must
/// live under an allowed artifact root (or `workspaceRoot`). This
/// replaces the direct `@tauri-apps/plugin-fs` `readDir` call which was
/// limited to `$HOME` by the Tauri capability scope and failed for
/// workspace directories on other drives (e.g. `F:\`).
export interface DirEntry {
  name: string;
  path: string;
  isDir: boolean;
  size: number;
  modifiedMs: number;
  extension?: string;
}

export async function listDirectory(
  path: string,
  workspaceRoot?: string | null,
): Promise<DirEntry[]> {
  return invoke("list_directory", { path, workspaceRoot: workspaceRoot ?? null });
}

// ── Diagnostics / logs (P0-7) ──────────────────────────────────────
//
// Backend writes rolling logs to `<data_dir>/logs/*.log` (these were
// previously unreachable because the packaged Tauri app's stderr is
// dropped). The settings panel surfaces these so users and support
// have a single clickable place to see what's actually happening.

export interface LogFileEntry {
  name: string;
  path: string;
  sizeBytes: number;
  modifiedSecs: number;
}

export interface LogFileInfo {
  dir: string;
  files: LogFileEntry[];
  totalBytes: number;
}

export interface BackendLogTail {
  path: string;
  fileName: string;
  totalLines: number;
  lines: string[];
}

export async function listBackendLogFiles(): Promise<LogFileInfo> {
  return invoke("list_backend_log_files");
}

export async function readBackendLogTail(tailLines: number): Promise<BackendLogTail> {
  return invoke("read_backend_log_tail", { tailLines });
}

export async function openLogDirectory(): Promise<string> {
  return invoke("open_log_directory");
}

// ── 系统级设置（L4 通知 / 自启 / 托盘，L3.6 壁纸）────────────────────
// 全部对应 system_settings.rs 里的 #[tauri::command]。失败一律 reject，
// 前端必须把错误如实弹出来：这些能力"看起来开了但没生效"最难排查。

export function getAutostart(): Promise<boolean> {
  return invoke("get_autostart");
}

export function setAutostart(enabled: boolean): Promise<void> {
  return invoke("set_autostart", { enabled });
}

export function sendNotification(title: string, body: string): Promise<void> {
  return invoke("send_notification", { title, body });
}

export function syncTray(enabled: boolean): Promise<void> {
  return invoke("sync_tray", { enabled });
}

/** 桌面层 UI 行为开关（system_settings.rs::DesktopSettings）。 */
export interface DesktopSettings {
  tray: boolean;
  /** 主窗点 X：true = 最小化到托盘，false = 退出程序。 */
  close_to_tray: boolean;
}

export function getDesktopSettings(): Promise<DesktopSettings> {
  return invoke("get_desktop_settings");
}

export function setCloseAction(closeToTray: boolean): Promise<void> {
  return invoke("set_close_action", { closeToTray });
}

/** 上报正在流式执行的会话数，驱动托盘菜单「正在执行的任务」项。 */
export function setTrayTaskCount(count: number): Promise<void> {
  return invoke("tray_set_task_count", { count });
}

// Aliases for SettingsView compatibility
// `openSettingsDirectory` is the legacy name used by SettingsView.
// It's a deprecated alias for `openDataDir`; callers should migrate
// to `openDataDir` directly.
export const openSettingsDirectory = openDataDir;
export const restartApp = restartDesktopApp;
