// Custom App Bridge — Tauri IPC bridge for custom app operations
// This provides a fallback path that goes directly through Tauri IPC
// instead of the HTTP API, useful when the HTTP server is not running
// or for operations that need direct filesystem access.

import { invoke } from "@tauri-apps/api/core";

export interface CustomAppInitResult {
  shell_path: string;
  shell_exists: boolean;
  esbuild_path: string;
  esbuild_exists: boolean;
  apps_dir: string;
}

export async function initCustomApps(): Promise<CustomAppInitResult> {
  return invoke<CustomAppInitResult>("custom_app_init");
}

export async function listCustomAppsViaIPC(): Promise<{
  success: boolean;
  apps: unknown[];
  count: number;
}> {
  return invoke("custom_app_list");
}

export async function packageCustomAppViaIPC(
  id: string,
): Promise<{ success: boolean; message: string; exe_path: string }> {
  return invoke("custom_app_package", { id });
}

export async function runCustomAppViaIPC(
  id: string,
): Promise<{ success: boolean; message: string }> {
  return invoke("custom_app_run", { id });
}

export async function openAppDirViaIPC(id: string): Promise<void> {
  return invoke("custom_app_open_dir", { id });
}
