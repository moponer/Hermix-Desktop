import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import * as api from "../api";

export type ConnState = "connecting" | "online" | "offline";

/** 重启后等待服务可应答的预算。hermix.exe 冷启动（嵌入模型加载、
 *  toolset 初始化）实测可达数十秒，固定等 1.5s 必然误判为失败。 */
const RESTART_PROBE_BUDGET_MS = 30_000;
const RESTART_PROBE_INTERVAL_MS = 1_000;

/**
 * 后端连接状态：探测 + 事件订阅 + 重启。
 *
 * 三处 UI（TitleBar / ConnectionIndicator / BackendStatus）此前各自复制
 * 了一份同样的逻辑，且都带两个相同的缺陷：
 *   1. `hermix:server-ready` 事件被直接当作"在线"。事件只说明 sidecar
 *      进程起来了，不代表服务能应答 —— 这正是"绿灯但发不出消息"的来源。
 *   2. 重启后固定 `setTimeout(1500)` 再探一次，慢启动会被判成失败。
 * 统一到这里，只保留一份正确实现。
 */
export function useBackendConnection() {
  const [state, setState] = useState<ConnState>("connecting");
  const [mode, setMode] = useState<string>("local");
  const [restarting, setRestarting] = useState(false);
  const [gaveUp, setGaveUp] = useState(false);
  // 事件回调里要读到最新状态，但又不能把 state 塞进依赖数组
  // （否则每次状态变化都让 check 换引用，30s 轮询 effect 反复重挂）。
  const stateRef = useRef<ConnState>("connecting");
  stateRef.current = state;
  const restartingRef = useRef(false);
  restartingRef.current = restarting;

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
        return;
      }
      // 探测失败不降级 "connecting"：冷启动需要时间，探一次就报离线
      // 会让正常启动闪一下"离线"。重启期间同样保持，由重启流程收尾。
      if (stateRef.current !== "connecting" && !restartingRef.current) {
        setState("offline");
      }
    } catch {
      if (stateRef.current !== "connecting" && !restartingRef.current) {
        setState("offline");
      }
    }
  }, []);

  useEffect(() => {
    check();
    const interval = setInterval(() => {
      if (document.visibilityState === "visible") void check();
    }, 30_000);
    return () => clearInterval(interval);
  }, [check]);

  useEffect(() => {
    let cancelled = false;
    const unlisteners: Array<() => void> = [];
    const reg = (p: Promise<() => void>) => {
      p.then((fn) => {
        if (cancelled) fn();
        else unlisteners.push(fn);
      }).catch(() => {});
    };

    reg(listen("hermix:server-ready", () => {
      if (cancelled) return;
      setGaveUp(false);
      // 回探而不是直接置 online —— 事件不等于服务可用。
      void check();
    }));
    reg(listen<number>("hermix:server-restarting", () => {
      if (cancelled) return;
      setState("offline");
      setRestarting(true);
      void (async () => {
        const deadline = Date.now() + RESTART_PROBE_BUDGET_MS;
        while (!cancelled && Date.now() < deadline) {
          await new Promise((r) => setTimeout(r, RESTART_PROBE_INTERVAL_MS));
          if (cancelled) return;
          if (await api.checkBackendHealth()) {
            setState("online");
            setRestarting(false);
            return;
          }
        }
        if (!cancelled) setRestarting(false);
      })();
    }));
    reg(listen("hermix:server-gave-up", () => {
      if (cancelled) return;
      setGaveUp(true);
      setRestarting(false);
      setState("offline");
    }));

    return () => {
      cancelled = true;
      unlisteners.forEach((fn) => fn());
    };
  }, [check]);

  const restart = useCallback(async () => {
    setRestarting(true);
    setGaveUp(false);
    try {
      await invoke("restart_sidecar");
      // 轮询到真正可应答为止；固定延时会把慢启动判成失败。
      const deadline = Date.now() + RESTART_PROBE_BUDGET_MS;
      while (Date.now() < deadline) {
        await new Promise((r) => setTimeout(r, RESTART_PROBE_INTERVAL_MS));
        if (await api.checkBackendHealth()) {
          setState("online");
          setRestarting(false);
          return true;
        }
      }
      setState("offline");
      setGaveUp(true);
      setRestarting(false);
      return false;
    } catch (e) {
      console.error("[backend-connection] restart failed:", e);
      setState("offline");
      setRestarting(false);
      return false;
    }
  }, []);

  return { state, mode, restarting, gaveUp, restart, check };
}
