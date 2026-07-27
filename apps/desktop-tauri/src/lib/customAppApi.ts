// Custom Apps API client — communicates with the backend HTTP API
// at /api/custom-apps/*

import { invoke } from "@tauri-apps/api/core";
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
  /** Path to the dist directory (if packaged) */
  dist_path: string | null;
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

/** Open the packaged app's dist directory in the system file explorer. */
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

// ── 开发期自验证 (shift-left): 由 agent 在生成/修改应用代码后调用 ──

export interface ValidationIssue {
  /** "error" 阻断打包; "warning" 仅提示 */
  level: string;
  /** manifest / import / esbuild */
  code: string;
  message: string;
}

export interface ValidationReport {
  /** 是否通过 (无 error 级问题) */
  ok: boolean;
  issues: ValidationIssue[];
}

// ── 运行时冒烟测试 (L2) ──

export interface SmokeError {
  message: string;
}

export interface SmokeReport {
  /** 是否通过 (已挂载且无运行时错误) */
  ok: boolean;
  /** 应用是否成功挂载 (React 渲染出内容) */
  ready: boolean;
  /** 是否已打包 (dist 存在) */
  packaged: boolean;
  /** 运行时错误列表 */
  errors: SmokeError[];
  /** 详细日志 (每行一条事件) */
  detail: string;
}

/**
 * 开发期自验证: 在打包/运行前校验应用源码, 把打包期错误提前到开发期暴露。
 * 校验 manifest 完整性、导入白名单 (只允许 react / react-dom/client +
 * 相对/URL)、并用与真实打包同款的 esbuild 标志做预打包。
 *
 * 通过 Tauri 命令 `custom_app_validate` 暴露 (与 HTTP 路由并列)。
 * agent 开发循环应在生成/修改代码后调用, `ok === false` 时继续修复直至通过。
 */
export async function validateCustomApp(id: string): Promise<ValidationReport> {
  return invoke<ValidationReport>("custom_app_validate", { id });
}

/**
 * L2 运行时冒烟测试: 先刷新打包产物, 再无头启动壳加载应用并捕获运行时错误
 * (白屏 / 运行时异常 / 挂载失败)。通过 Tauri 命令 `custom_app_smoke_test` 暴露。
 * 返回结构化报告, `ok === false` 时 `errors` 含具体运行时问题。
 */
export async function smokeTestCustomApp(id: string): Promise<SmokeReport> {
  return invoke<SmokeReport>("custom_app_smoke_test", { id });
}
