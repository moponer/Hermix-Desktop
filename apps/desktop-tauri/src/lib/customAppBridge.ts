// Tauri IPC bridge for custom-app operations. Fallback path used when
// the HTTP server isn't running or when callers need direct filesystem
// access (avoiding the /api/custom-apps round-trip).

import { invoke } from "@tauri-apps/api/core";

export interface CustomAppInitResult {
  /** Path to the wry app loader (hermix-app-loader.exe). */
  loader_path: string;
  /** Whether the loader binary exists at the resolved path. */
  loader_exists: boolean;
  /** Path to the custom-apps directory. */
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
