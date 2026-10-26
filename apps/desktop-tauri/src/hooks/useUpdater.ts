/**
 * useUpdater — 在线更新的前端状态机。
 *
 * 后端（src-tauri/src/updater.rs）三个命令 + 两个事件：
 *   update_check    → UpdateStatus（none/optional/forced）
 *   update_download → 流式发 update://progress {phase,received,total}
 *   update_install  → 解压 + 启动交换 helper（helper 等本进程退出后换目录）
 *   update://status → 后端调度器查到 forced 版本时推送（只推一次）
 *
 * 流程：startUpdate = download → install → restart_app。
 * install 返回即 helper 已起跑，稍候调 restart_app 让 PID 退出，
 * helper 接管交换与重启；失败由 helper 回滚（update.failed）。
 */

import { useCallback, useEffect, useState } from "react";
import { invoke } from "../ipc";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface UpdateStatus {
  kind: "none" | "optional" | "forced";
  current_version: string;
  latest_version: string;
  notes: string;
  pub_date: string;
  download_url: string;
  sha256: string;
}

export interface UpdateProgress {
  phase: "download" | "extracted" | "ready_to_exit";
  received: number;
  total: number;
}

export type UpdatePhase = "idle" | "downloading" | "installing" | "restarting";

export function useUpdater() {
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  const [panelOpen, setPanelOpen] = useState(false);
  const [progress, setProgress] = useState<UpdateProgress | null>(null);
  const [phase, setPhase] = useState<UpdatePhase>("idle");
  const [error, setError] = useState<string | null>(null);
  const [checkFailed, setCheckFailed] = useState(false);

  useEffect(() => {
    const regs: Promise<UnlistenFn>[] = [
      listen<UpdateStatus>("update://status", (e) => {
        setStatus(e.payload);
        // 强制版：后端保证只推一次，这里直接弹阻断面板。
        if (e.payload.kind === "forced") setPanelOpen(true);
      }),
      listen<UpdateProgress>("update://progress", (e) => setProgress(e.payload)),
    ];
    return () => {
      regs.forEach((p) => p.then((f) => f()).catch(() => {}));
    };
  }, []);

  const checkUpdate = useCallback(async (): Promise<UpdateStatus | null> => {
    try {
      const s = await invoke<UpdateStatus>("update_check");
      setStatus(s);
      setCheckFailed(false);
      setError(null);
      return s;
    } catch (e) {
      // 检查失败面板显示失败态；真实原因（404=源未发布 / DNS=网络）透传给面板，不吞。
      setError(String(e));
      setCheckFailed(true);
      return null;
    }
  }, []);

  const startUpdate = useCallback(async () => {
    if (!status) return;
    setError(null);
    setPhase("downloading");
    try {
      await invoke("update_download", {
        downloadUrl: status.download_url,
        expectedSha256: status.sha256,
      });
      setPhase("installing");
      await invoke("update_install");
      setPhase("restarting");
      // helper 是 detached 进程，给它半秒起跑，再走正常退出（sidecar 收尾）。
      await new Promise((r) => setTimeout(r, 500));
      await invoke("restart_app");
    } catch (e) {
      setError(String(e));
      setPhase("idle");
    }
  }, [status]);

  return { status, panelOpen, setPanelOpen, progress, phase, error, checkFailed, checkUpdate, startUpdate };
}

export type Updater = ReturnType<typeof useUpdater>;
