// Custom Apps API client — communicates with the backend HTTP API
// at /api/custom-apps/*.

import { IPC } from "../ipc/commands";
import { invoke } from "../ipc";import { fetchJSON } from "../api";

export interface CustomAppManifest {
  id: string;
  name: string;
  version: string;
  description: string;
  long_description?: string;
  author: string;
  icon: string;
  /** Custom icon file name (e.g. "icon.png"). */
  icon_file?: string;
  /**
   * Tile background color for the app icon container, used in:
   *   - dashboard list / detail view (CSS background on the icon container)
   *   - packaged EXE taskbar / shell icon (composited over a rounded square)
   * Format: `#RRGGBB` or `#RRGGBBAA` (case-insensitive). `undefined` falls
   * back to the neutral gray container for in-app views and to the raw
   * lucide SVG (no background) for the packaged EXE.
   */
  tile_color?: string;
  category: string;
  tags: string[];
  window_config: {
    width: number;
    height: number;
    resizable: boolean;
    title?: string;
  };
  permissions: string[];
}

export interface CustomAppSummary {
  id: string;
  name: string;
  version: string;
  description: string;
  icon: string;
  /** Null if no custom icon. */
  icon_file: string | null;
  /** Null if no tile color set (falls back to neutral gray / transparent). */
  tile_color: string | null;
  category: string;
  author: string;
  is_packaged: boolean;
  /** Null when not yet packaged. */
  dist_path: string | null;
}

export interface CustomAppResponse {
  manifest: CustomAppManifest;
  /** Self-contained index.html source (or auto-generated HTML shell). */
  index_html: string | null;
  /** TypeScript/React source code (if app uses TypeScript mode). */
  app_source: string | null;
  is_packaged: boolean;
  dist_path: string | null;
}

export interface CreateAppRequest {
  manifest: CustomAppManifest;
  /** Plain HTML mode: self-contained index.html. */
  index_html?: string;
  /** TypeScript/React mode (★recommended): TSX entry source. */
  app_source?: string;
  /** Additional TypeScript source files (relative path → content). */
  source_files?: Record<string, string>;
}

export interface UpdateAppRequest {
  index_html?: string;
  /** TypeScript/React source code to update. */
  app_source?: string;
  /** Additional TypeScript source files to update. */
  source_files?: Record<string, string>;
  /** Partial manifest — fields left undefined are preserved. */
  manifest?: Partial<CustomAppManifest>;
}

export interface PackageResponse {
  success: boolean;
  message: string;
  exe_path: string;
  output_dir?: string;
  build_mode?: string;
  error?: string;
}

/** Build environment status for neu-build mode. */
// EnvStatus was removed when neu-build support was dropped (hx-shell
// uses single-file Sidecar packaging exclusively). All callers
// (CustomAppsView) have been updated to not reference it.

export async function listCustomApps(): Promise<CustomAppSummary[]> {
  // Goes through fetchJSON → fetchApi → withRetry, which provides
  // circuit breaker, 10s timeout, and 3 retries with backoff.
  const data = await fetchJSON<{ apps: CustomAppSummary[] }>("/api/custom-apps");
  return data.apps ?? [];
}

// Opens the packaged app's dist directory in the system file explorer.
export async function openCustomAppDist(path: string): Promise<void> {
  await invoke(IPC.openExternalUrl, { url: path });
}

export async function getCustomApp(id: string): Promise<CustomAppResponse> {
  const data = await fetchJSON<{ app: CustomAppResponse }>(
    `/api/custom-apps/${encodeURIComponent(id)}`,
  );
  return data.app;
}

export async function createCustomApp(req: CreateAppRequest): Promise<CustomAppResponse> {
  const data = await fetchJSON<{ app: CustomAppResponse }>("/api/custom-apps", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(req),
  });
  return data.app;
}

export async function updateCustomApp(id: string, req: UpdateAppRequest): Promise<CustomAppResponse> {
  const data = await fetchJSON<{ app: CustomAppResponse }>(
    `/api/custom-apps/${encodeURIComponent(id)}`,
    {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(req),
    },
  );
  return data.app;
}

export async function deleteCustomApp(id: string): Promise<void> {
  await fetchJSON(`/api/custom-apps/${encodeURIComponent(id)}`, {
    method: "DELETE",
  });
}

/**
 * Package a custom app as a standalone EXE.
 * @param id App ID
 * @param buildMode "sidecar" (default, zero-dependency) or "neu-build" (requires Node.js)
 */
export async function packageCustomApp(
  id: string,
  buildMode?: "sidecar" | "neu-build",
): Promise<PackageResponse> {
  const mode = buildMode ?? "sidecar";
  return fetchJSON<PackageResponse>(
    `/api/custom-apps/${encodeURIComponent(id)}/package?mode=${mode}`,
    { method: "POST" },
  );
}

export async function runCustomApp(id: string): Promise<void> {
  await fetchJSON(`/api/custom-apps/${encodeURIComponent(id)}/run`, {
    method: "POST",
  });
}

export interface ValidationIssue {
  /** "error" blocks packaging; "warning" is informational only. */
  level: string;
  /** One of: manifest / html / rpc / permissions / js. */
  code: string;
  message: string;
}

export interface ValidationReport {
  /** True when no `error`-level issues were found. */
  ok: boolean;
  issues: ValidationIssue[];
}

export interface SmokeError {
  message: string;
}

export interface SmokeReport {
  /** True when the app mounted AND emitted no runtime errors. */
  ok: boolean;
  /** App mounted (DOMContentLoaded fired). */
  ready: boolean;
  /** dist directory exists. */
  packaged: boolean;
  errors: SmokeError[];
  /** Newline-separated event log. */
  detail: string;
}

// Runs static checks (manifest integrity, HTML structure, hx API
// patterns, permission consistency, JS bracket balance) before
// packaging. Agent development loops should call this after editing
// app code and retry until `ok === true`.
export async function validateCustomApp(id: string): Promise<ValidationReport> {
  return invoke<ValidationReport>(IPC.customAppValidate, { id });
}

// Launches the packaged hx-shell exe and checks if it stays alive.
// Returns structured errors when `ok === false`.
export async function smokeTestCustomApp(id: string): Promise<SmokeReport> {
  return invoke<SmokeReport>(IPC.customAppSmokeTest, { id });
}
