// Custom Apps API client — communicates with the backend HTTP API
// at /api/custom-apps/*.

import { invoke } from "@tauri-apps/api/core";
import { apiUrl } from "../api";

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
  category: string;
  author: string;
  is_packaged: boolean;
  /** Null when not yet packaged. */
  dist_path: string | null;
}

export interface CustomAppResponse {
  manifest: CustomAppManifest;
  /** Self-contained index.html source. */
  index_html: string | null;
  is_packaged: boolean;
  dist_path: string | null;
}

export interface CreateAppRequest {
  manifest: CustomAppManifest;
  index_html?: string;
}

export interface UpdateAppRequest {
  index_html?: string;
  /** Partial manifest — fields left undefined are preserved. */
  manifest?: Partial<CustomAppManifest>;
}

export interface PackageResponse {
  success: boolean;
  message: string;
  exe_path: string;
  output_dir?: string;
  error?: string;
}

export async function listCustomApps(): Promise<CustomAppSummary[]> {
  const res = await fetch(apiUrl("/api/custom-apps"));
  if (!res.ok) throw new Error(`列表失败: ${res.status}`);
  const data = await res.json();
  return data.apps ?? [];
}

// Opens the packaged app's dist directory in the system file explorer.
export async function openCustomAppDist(path: string): Promise<void> {
  await invoke("open_external_url", { url: path });
}

export async function getCustomApp(id: string): Promise<CustomAppResponse> {
  const res = await fetch(apiUrl(`/api/custom-apps/${encodeURIComponent(id)}`));
  if (!res.ok) throw new Error(`获取失败: ${res.status}`);
  const data = await res.json();
  return data.app;
}

export async function createCustomApp(req: CreateAppRequest): Promise<CustomAppResponse> {
  const res = await fetch(apiUrl("/api/custom-apps"), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(req),
  });
  if (!res.ok) {
    const err = await res.json().catch(() => ({ error: res.statusText }));
    throw new Error(err.error || `创建失败: ${res.status}`);
  }
  const data = await res.json();
  return data.app;
}

export async function updateCustomApp(id: string, req: UpdateAppRequest): Promise<CustomAppResponse> {
  const res = await fetch(apiUrl(`/api/custom-apps/${encodeURIComponent(id)}`), {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(req),
  });
  if (!res.ok) {
    const err = await res.json().catch(() => ({ error: res.statusText }));
    throw new Error(err.error || `更新失败: ${res.status}`);
  }
  const data = await res.json();
  return data.app;
}

export async function deleteCustomApp(id: string): Promise<void> {
  const res = await fetch(apiUrl(`/api/custom-apps/${encodeURIComponent(id)}`), {
    method: "DELETE",
  });
  if (!res.ok) {
    const err = await res.json().catch(() => ({ error: res.statusText }));
    throw new Error(err.error || `删除失败: ${res.status}`);
  }
}

export async function packageCustomApp(id: string): Promise<PackageResponse> {
  const res = await fetch(apiUrl(`/api/custom-apps/${encodeURIComponent(id)}/package`), {
    method: "POST",
  });
  if (!res.ok) {
    const err = await res.json().catch(() => ({ error: res.statusText }));
    throw new Error(err.error || `打包失败: ${res.status}`);
  }
  return res.json();
}

export async function runCustomApp(id: string): Promise<void> {
  const res = await fetch(apiUrl(`/api/custom-apps/${encodeURIComponent(id)}/run`), {
    method: "POST",
  });
  if (!res.ok) {
    const err = await res.json().catch(() => ({ error: res.statusText }));
    throw new Error(err.error || `启动失败: ${res.status}`);
  }
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

// Runs static checks (manifest integrity, HTML structure, RPC call
// patterns, permission consistency, JS bracket balance) before
// packaging. Agent development loops should call this after editing
// app code and retry until `ok === true`.
export async function validateCustomApp(id: string): Promise<ValidationReport> {
  return invoke<ValidationReport>("custom_app_validate", { id });
}

// Headless shell run after re-packaging: catches white-screen,
// runtime exceptions, and mount failures. Returns structured errors
// when `ok === false`.
export async function smokeTestCustomApp(id: string): Promise<SmokeReport> {
  return invoke<SmokeReport>("custom_app_smoke_test", { id });
}
