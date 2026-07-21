// Custom Apps API client — communicates with the backend HTTP API
// at /api/custom-apps/*

import { apiUrl } from "../api";

// ── Types ──

export interface CustomAppManifest {
  id: string;
  name: string;
  version: string;
  description: string;
  long_description?: string;
  author: string;
  icon: string;
  /** Custom icon file name (e.g. "icon.png") */
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
  /** Custom icon file name (e.g. "icon.png") — null if no custom icon */
  icon_file: string | null;
  category: string;
  author: string;
  is_packaged: boolean;
}

export interface CustomAppResponse {
  manifest: CustomAppManifest;
  backend_code: string;
  ui_code: string | null;
  is_packaged: boolean;
  dist_path: string | null;
}

export interface CreateAppRequest {
  manifest: CustomAppManifest;
  backend_code: string;
  ui_code: string | null;
}

export interface UpdateAppRequest {
  backend_code?: string;
  ui_code?: string;
}

export interface PackageResponse {
  success: boolean;
  message: string;
  exe_path: string;
  output_dir?: string;
  error?: string;
}

// ── API Functions ──

export async function listCustomApps(): Promise<CustomAppSummary[]> {
  const res = await fetch(apiUrl("/api/custom-apps"));
  if (!res.ok) throw new Error(`列表失败: ${res.status}`);
  const data = await res.json();
  return data.apps ?? [];
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
