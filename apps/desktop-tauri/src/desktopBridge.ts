// Tauri IPC bridge — replaces Electron preload
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

export async function pickDirectory(): Promise<string | null> {
  const selected = await open({ directory: true, title: "选择目录" });
  if (typeof selected === "string") return selected;
  if (selected && typeof selected === "object" && "path" in selected) return (selected as { path: string }).path;
  return null;
}

export async function getRuntimeInfo(): Promise<{ runtime: string; platform: string; userDataDir: string }> {
  return invoke("get_runtime_info");
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

export async function openExternalUrl(url: string): Promise<void> {
  try {
    await invoke("open_external_url", { url });
  } catch (e) {
    console.error("[desktopBridge] openExternalUrl failed:", url, e);
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
///
/// The Tauri command validates the path is under `HERMES_DESKTOP_ARTIFACT_ROOTS`
/// (defaults to the user's home directory), caps at 2 MiB, and detects binary
/// content via UTF-8 decoding.
///
/// If `path` is relative, the backend anchors it against `workspaceRoot`
/// (or `HERMES_DESKTOP_WORKSPACE_ROOT` env, or the first allowed root, or
/// `~/`, in that order). This is what fixes the "tmp\xx\方案.md" bug: the
/// LLM often speaks in relative paths, and we now have a way to find them.
///
/// `resolvedPath` is the canonical absolute path the backend actually read —
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

/// Render an Office document (.docx/.xlsx/.pptx) to HTML using the
/// bundled `officecli` binary. Returns the rendered HTML content
/// directly so the front-end can use `srcDoc` and avoid the Tauri 2
/// asset-protocol scope complexity. `htmlPath` is still returned for
/// debug / "open in browser" actions.
export async function renderOfficePreview(
  filePath: string,
  workspaceRoot?: string | null,
): Promise<{ htmlPath: string; htmlContent: string; fileName: string }> {
  return invoke("render_office_preview", { filePath, workspaceRoot: workspaceRoot ?? null });
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

// Aliases for SettingsView compatibility
//
// `openSettingsDirectory` is the legacy name used by SettingsView.
// It's a deprecated alias for `openDataDir`; callers should migrate
// to `openDataDir` directly.
export const openSettingsDirectory = openDataDir;
export const restartApp = restartDesktopApp;
