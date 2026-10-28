import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "../ipc";
import { listen } from "@tauri-apps/api/event";
import * as api from "../api";
import { usePolling } from "./usePolling";
import { toastError, toastSuccess } from "./useToast";

export type ConnState = "connecting" | "online" | "offline";

/** 重启后等待服务可应答的预算。hermix.exe 冷启动（嵌入模型加载、
 *  toolset 初始化）实测可达数十秒（watchdog 侧校准 ~70s），30s 必然误判。 */
const RESTART_PROBE_BUDGET_MS = 90_000;
const RESTART_PROBE_INTERVAL_MS = 1_000;

/**
 * 在线时的探测间隔。`backend_ready` 走的是 stdio 桥的一来一回（本机管道，
 * 毫秒级），比原来的 30s 快一个量级才有意义 —— 后端中途死掉时，用户
 * 30s 内点任何东西都在报错，而指示器还是绿的（"只有点项目才知道断了"）。
 * 同时间隔不能太小：离线时每次探测要等满 2s 超时，5s 轮询等于 40% 时间
 * 有探测在飞。
 */
const PROBE_INTERVAL_MS = 6_000;
/** 冷启动宽限：从第一次探测失败算起，这段时间内保持 "connecting"（不报
 *  离线，避免正常慢启动闪一下"离线"）；超过仍连不上就**如实**转离线 ——
 *  否则后端彻底没起来时界面会永远转圈，用户以为只是慢。 */
const BOOT_GRACE_MS = 30_000;
/** 断线 / 恢复提示的最小间隔：抖动（watchdog 连环重启）不该刷屏。 */
const NOTICE_COOLDOWN_MS = 20_000;

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
  // （否则每次状态变化都让 check 换引用，6s 轮询 effect 反复重挂）。
  const stateRef = useRef<ConnState>("connecting");
  const restartingRef = useRef(false);
  restartingRef.current = restarting;
  const lastNoticeRef = useRef(0);
  /** 第一次探测失败的时刻（0 = 还没失败过）。冷启动宽限的计时起点。 */
  const firstFailureAtRef = useRef(0);

  /**
   * 唯一的写状态入口：只有这里能改 `state`，顺带做两件事 ——
   * 同步 `stateRef`（事件回调与轮询读它）、在**真实跃迁**时给用户一条
   * 可见提示。此前断线是绝对静默的，用户只能靠"点东西报错"发现。
   */
  const transition = useCallback((next: ConnState) => {
    const prev = stateRef.current;
    if (prev === next) return;
    stateRef.current = next;
    setState(next);

    const now = Date.now();
    if (next === "offline" && prev === "online" && now - lastNoticeRef.current > NOTICE_COOLDOWN_MS) {
      lastNoticeRef.current = now;
      toastError("本地后端已断开，正在自动重连；功能可能暂时不可用。");
    } else if (next === "online" && prev === "offline" && now - lastNoticeRef.current > NOTICE_COOLDOWN_MS) {
      lastNoticeRef.current = now;
      toastSuccess("本地后端已恢复。");
    }
  }, []);

  const check = useCallback(async () => {
    // 先探活再拉配置：离线时 getConfig 必吃一次 503 往返，纯浪费。
    // `backend_ready` 是同步真值（桥 + /health 往返 2s 上限），很便宜。
    let ok = false;
    try {
      ok = await api.checkBackendHealth();
    } catch {
      ok = false;
    }
    if (!ok) {
      if (stateRef.current === "connecting") {
        // 冷启动需要时间，探一次就报离线会让正常启动闪一下"离线"。
        // 但宽限过后必须如实转离线 —— 否则后端根本没起来时界面永远转圈。
        if (firstFailureAtRef.current === 0) firstFailureAtRef.current = Date.now();
        if (Date.now() - firstFailureAtRef.current < BOOT_GRACE_MS) return;
      } else if (restartingRef.current) {
        return; // 重启流程自己收尾（探到 online 或 gave-up）
      }
      transition("offline");
      return;
    }
    firstFailureAtRef.current = 0;
    transition("online");
    setGaveUp(false);
    try {
      const config = await api.getConfig();
      setMode(String(config.mode ?? "local"));
    } catch {
      // 配置拉取失败不影响连接判定（可能是单次查询失败）。
    }
  }, [transition]);

  // 探测收敛进 usePolling（P2-4）：行为与原先的手写 visibilityState
  // 判断一致，另外多出「恢复可见立刻补探一次」，切回窗口不用等满一轮。
  usePolling(() => check(), PROBE_INTERVAL_MS);

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
      if (cancelled || restartingRef.current) return;
      // 去重：watchdog 每次 spawn 都发此事件（"spawn 成功→又崩"会连环
      // 发），若每次都重开一轮 30s 轮询，restarting 永远为 true —— 左下角
      // 无限转圈。已有轮询在跑就让它自然收尾（探活成功置 online，
      // gave-up 事件置失败）。
      transition("offline");
      setRestarting(true);
      void (async () => {
        const deadline = Date.now() + RESTART_PROBE_BUDGET_MS;
        while (!cancelled && Date.now() < deadline) {
          await new Promise((r) => setTimeout(r, RESTART_PROBE_INTERVAL_MS));
          if (cancelled) return;
          if (await api.checkBackendHealth()) {
            transition("online");
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
      transition("offline");
    }));

    return () => {
      cancelled = true;
      unlisteners.forEach((fn) => fn());
    };
  }, [check, transition]);

  const restart = useCallback(async () => {
    setRestarting(true);
    setGaveUp(false);
    firstFailureAtRef.current = 0;
    try {
      await invoke("restart_sidecar");
      // 轮询到真正可应答为止；固定延时会把慢启动判成失败。
      const deadline = Date.now() + RESTART_PROBE_BUDGET_MS;
      while (Date.now() < deadline) {
        await new Promise((r) => setTimeout(r, RESTART_PROBE_INTERVAL_MS));
        if (await api.checkBackendHealth()) {
          transition("online");
          setRestarting(false);
          return true;
        }
      }
      transition("offline");
      setGaveUp(true);
      setRestarting(false);
      return false;
    } catch (e) {
      console.error("[backend-connection] restart failed:", e);
      transition("offline");
      setRestarting(false);
      return false;
    }
  }, [transition]);

  return { state, mode, restarting, gaveUp, restart, check };
}
