import { useState, useEffect, useCallback } from "react";
import { Cpu, Loader2, RefreshCw, Wifi, WifiOff } from "lucide-react";
import * as api from "../api";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { toastError } from "../hooks/useToast";

export function BackendStatus() {
  // Three explicit states:
  //   "connecting" (default) — sidecar spawned, waiting for first ready/health
  //   "offline"              — health failed (or gave-up event fired)
  //   "online"               — health OK / hermix:server-ready fired
  // The previous implementation returned `null` while connecting, which
  // looked like a dead/hung UI during the 2-6s hermix.exe boot window.
  type ConnState = "connecting" | "online" | "offline";
  const [state, setState] = useState<ConnState>("connecting");
  const [mode, setMode] = useState<string>("local");
  const [restarting, setRestarting] = useState(false);
  const [gaveUp, setGaveUp] = useState(false);

  const check = useCallback(async () => {
    // Skip all network calls when the circuit breaker is tripped —
    // the backend is known to be unreachable and polling would only
    // waste resources. The circuit resets automatically after cooldown.
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
        // health explicitly returned false (not a network error).
        // Don't downgrade the "connecting" state on a single failed
        // probe — the user would see a flash of "离线" during normal
        // startup. The `server-restarting` event handles post-boot
        // downgrades so this branch only fires when the server is
        // up and reachable but the answer was a hard no.
        setState("offline");
      }
    } catch {
      // network/transport error during boot — keep "connecting" so we
      // don't mislead the user into thinking the backend died.
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

  // Listen for backend lifecycle events from the Tauri sidecar watchdog.
  // The "hermix:server-ready" event fires the moment hermix.exe finishes
  // boot (binds port + reports healthy) — we use it to skip the
  // health-poll latency for the very first connection.
  useEffect(() => {
    let unlistenReady: (() => void) | undefined;
    let unlistenRestarting: (() => void) | undefined;
    let unlistenGaveUp: (() => void) | undefined;
    let cancelled = false;

    listen("hermix:server-ready", () => {
      if (cancelled) return;
      setState("online");
      setGaveUp(false);
    }).then((fn) => {
      if (cancelled) fn();
      else unlistenReady = fn;
    });

    listen<number>("hermix:server-restarting", () => {
      if (cancelled) return;
      setState("offline");
      setRestarting(true);
      // After restart, poll health to detect recovery.
      const poll = async () => {
        for (let i = 0; i < 10; i++) {
          if (cancelled) return;
          await new Promise((r) => setTimeout(r, 1000));
          if (cancelled) return;
          try {
            const ok = await api.checkBackendHealth();
            if (ok) {
              setState("online");
              setRestarting(false);
              return;
            }
          } catch { /* keep polling */ }
        }
        setRestarting(false);
      };
      poll();
    }).then((fn) => {
      if (cancelled) fn();
      else unlistenRestarting = fn;
    });

    listen("hermix:server-gave-up", () => {
      if (cancelled) return;
      setGaveUp(true);
      setRestarting(false);
      setState("offline");
    }).then((fn) => {
      if (cancelled) fn();
      else unlistenGaveUp = fn;
    });

    return () => {
      cancelled = true;
      unlistenReady?.();
      unlistenRestarting?.();
      unlistenGaveUp?.();
    };
  }, []);

  const handleRestart = useCallback(async () => {
    setRestarting(true);
    setGaveUp(false);
    try {
      await invoke("restart_sidecar");
      // Give the server a moment to bind
      await new Promise((r) => setTimeout(r, 1500));
      await check();
    } catch (e) {
      console.error("[Hermix] sidecar restart failed:", e);
toastError("后端服务重启失败，请尝试手动重启");
    } finally {
      setRestarting(false);
    }
  }, [check]);

  // Shared restart button (shown once the user is in a definable state —
  // i.e. not "connecting"). During boot the connecting pill already
  // signals activity; adding a restart button there would invite the
  // user to fight an in-progress startup.
  const restartBtn = state !== "connecting" ? (
    <button
      onClick={handleRestart}
      disabled={restarting}
      className="ml-1 p-0.5 rounded hover:bg-white/10 transition-colors disabled:opacity-50"
      title="重启后端服务"
     aria-label="重启后端服务">
      <RefreshCw
        size={11}
        className={restarting ? "animate-spin text-text-muted" : "text-text-muted"}
      />
    </button>
  ) : null;

  if (mode === "local") {
    if (gaveUp) {
      return (
        <div className="flex items-center gap-1.5 px-2 py-1 rounded text-[11px] text-amber-400" title="后端多次重启失败，需要手动重启">
          <WifiOff size={11} className="text-amber-400" />
          <span>重启失败</span>
          {restartBtn}
        </div>
      );
    }
    if (restarting) {
      return (
        <div className="flex items-center gap-1.5 px-2 py-1 rounded text-[11px] text-zinc-400" title="后端正在重启...">
          <RefreshCw size={11} className="animate-spin text-zinc-400" />
          <span>重启中...</span>
        </div>
      );
    }
    if (state === "connecting") {
      // Boot window — backend spawned, health probe not yet returned
      // OK. Don't lie with "离线" while the user can see the spinner
      // and trust that we're doing work.
      return (
        <div className="flex items-center gap-1.5 px-2 py-1 rounded text-[11px] text-text-muted" title="本地后端正在启动...">
          <Loader2 size={11} className="animate-spin text-text-muted" />
          <span>正在连接</span>
        </div>
      );
    }
    if (state === "offline") {
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
      title={
        state === "online"
          ? "已连接到后端"
          : state === "offline"
            ? "后端未连接"
            : "正在连接后端..."
      }
    >
      {state === "connecting" ? (
        <>
          <Loader2 size={11} className="animate-spin text-text-muted" />
          <span>正在连接</span>
        </>
      ) : state === "online" ? (
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
