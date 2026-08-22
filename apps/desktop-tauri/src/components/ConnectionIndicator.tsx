import { useState, useEffect, useCallback } from "react";
import { Loader2 } from "lucide-react";
import * as api from "../api";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/**
 * Connection status indicator — small colored dot shown in the sidebar.
 * Color semantics:
 *   - green   = online / connected
 *   - amber   = connecting (boot) or restarting
 *   - gray    = offline / error
 * Shows as a bare dot; no label, no hover tooltip clutter.
 */
export function ConnectionIndicator() {
  type ConnState = "connecting" | "online" | "offline";
  const [state, setState] = useState<ConnState>("connecting");
  const [mode, setMode] = useState<string>("local");
  const [restarting, setRestarting] = useState(false);
  const [gaveUp, setGaveUp] = useState(false);

  const check = useCallback(async () => {
    if (api.isCircuitTripped()) {
      setState("offline");
      return;
    }
    try {
      const config = await api.getConfig();
      setMode(String(config.mode ?? "local"));
      const ok = await api.checkBackendHealth();
      if (ok) {
        setState("online");
        setGaveUp(false);
      } else if (state !== "connecting") {
        setState("offline");
      }
    } catch {
      if (state !== "connecting") setState("offline");
    }
  }, [state]);

  useEffect(() => {
    check();
    const interval = setInterval(() => {
      if (document.visibilityState === "visible") check();
    }, 30000);
    return () => clearInterval(interval);
  }, [check]);

  useEffect(() => {
    let cancelled = false;
    listen("hermix:server-ready", () => {
      if (cancelled) return;
      setState("online");
      setGaveUp(false);
    });
    listen<number>("hermix:server-restarting", () => {
      if (cancelled) return;
      setState("offline");
      setRestarting(true);
      const poll = async () => {
        for (let i = 0; i < 10; i++) {
          if (cancelled) return;
          await new Promise((r) => setTimeout(r, 1000));
          if (cancelled) return;
          try {
            const ok = await api.checkBackendHealth();
            if (ok) { setState("online"); setRestarting(false); return; }
          } catch { /* keep polling */ }
        }
        setRestarting(false);
      };
      poll();
    });
    listen("hermix:server-gave-up", () => {
      if (cancelled) return;
      setGaveUp(true);
      setRestarting(false);
      setState("offline");
    });
    return () => { cancelled = true; };
  }, []);

  const handleRestart = useCallback(async () => {
    setRestarting(true);
    setGaveUp(false);
    try {
      await invoke("restart_sidecar");
      await new Promise((r) => setTimeout(r, 1500));
      await check();
    } catch { console.error("[ConnectionIndicator] restart failed"); }
    finally { setRestarting(false); }
  }, [check]);

  // Visual semantics
  const isOnline  = state === "online" && mode === "local";
  const isOffline = state === "offline" || gaveUp;
  const isConnecting = state === "connecting" || restarting || mode !== "local";

  const dotColor = isOnline
    ? "var(--color-success)"
    : isOffline
      ? "var(--color-text-faint)"
      : "var(--color-warning)";

  return (
    <button
      type="button"
      onClick={isOffline && !restarting ? handleRestart : undefined}
      title={
        isOnline   ? "后端已连接"
        : isOffline ? (gaveUp ? "重启失败，点击重试" : "后端离线，点击重启")
        : "正在连接..."
      }
      aria-label="连接状态"
      className="connection-indicator"
      style={{ cursor: isOffline && !restarting ? "pointer" : "default" }}
    >
      {isConnecting && <Loader2 size={10} className="animate-spin opacity-70" />}
      <span className="connection-dot" style={{ background: dotColor }} />
    </button>
  );
}
