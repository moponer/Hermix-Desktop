import { useState, useEffect, useCallback } from "react";
import { Cpu, RefreshCw, Wifi, WifiOff } from "lucide-react";
import * as api from "../api";
import { invoke } from "@tauri-apps/api/core";

export function BackendStatus() {
  const [connected, setConnected] = useState<boolean | null>(null);
  const [mode, setMode] = useState<string>("local");
  const [restarting, setRestarting] = useState(false);

  const check = useCallback(async () => {
    try {
      const config = await api.getConfig();
      setMode(String(config.mode ?? "local"));
      const ok = await api.checkBackendHealth();
      setConnected(ok);
    } catch {
      setConnected(false);
    }
  }, []);

  useEffect(() => {
    check();
    const interval = setInterval(check, 30000);
    return () => clearInterval(interval);
  }, [check]);

  const handleRestart = useCallback(async () => {
    setRestarting(true);
    try {
      await invoke("restart_sidecar");
      // Give the server a moment to bind
      await new Promise((r) => setTimeout(r, 1500));
      await check();
    } catch (e) {
      console.error("[Hermix] sidecar restart failed:", e);
    } finally {
      setRestarting(false);
    }
  }, [check]);

  if (connected === null) return null;

  // Shared restart button
  const restartBtn = (
    <button
      onClick={handleRestart}
      disabled={restarting}
      className="ml-1 p-0.5 rounded hover:bg-white/10 transition-colors disabled:opacity-50"
      title="重启后端服务"
    >
      <RefreshCw
        size={11}
        className={restarting ? "animate-spin text-text-muted" : "text-text-muted"}
      />
    </button>
  );

  if (mode === "local") {
    if (!connected) {
      return (
        <div className="flex items-center gap-1.5 px-2 py-1 rounded text-[11px] text-text-muted" title="本地模式 — 后端未连接">
          <WifiOff size={11} className="text-text-muted" />
          <span>离线</span>
          {restartBtn}
        </div>
      );
    }
    return (
      <div className="flex items-center gap-1.5 px-2 py-1 rounded text-[11px] text-text-muted" title="本地模式 — 内嵌 agent 运行时">
        <Cpu size={11} className="text-success" />
        <span>本地已连接</span>
      </div>
    );
  }

  return (
    <div
      className="flex items-center gap-1.5 px-2 py-1 rounded text-[11px] text-text-muted"
      title={connected ? "已连接到后端" : "后端未连接"}
    >
      {connected ? (
        <>
          <Wifi size={11} className="text-success" />
          <span>已连接</span>
        </>
      ) : (
        <>
          <WifiOff size={11} className="text-text-muted" />
          <span>离线</span>
          {restartBtn}
        </>
      )}
    </div>
  );
}
