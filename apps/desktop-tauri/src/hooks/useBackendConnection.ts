import { IPC } from "../ipc/commands";
import { useCallback, useEffect, useRef, useState } from "react";import { invoke } from "../ipc";
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
/** 冷启动宽限：从第一次探测失败算起，这段时间内保持 "connecting"（不报
 *  离线，避免正常慢启动闪一下"离线"）；超过仍连不上就**如实**转离线 ——
 *  否则后端彻底没起来时界面会永远转圈。hermix.exe 冷启动（嵌入模型加载、
 *  toolset 初始化）实测可达 ~70s（watchdog 侧校准、重启探测预算都是 90s），
 *  此前 30s 会在慢机器上把还在正常启动的后端判成离线。 */
const BOOT_GRACE_MS = 90_000;
/** online→offline 需要连续失败的探测次数。/health 往返有 2s 预算，而审批
 *  选中后（终端进程创建 + 杀软实时扫描）、工具执行中（大输出、子进程 spawn）
 *  恰是后端最忙的时刻 —— 单次探测超时就把活着的后端判离线，用户点「重启」
 *  反而杀掉正在干活的会话。连续两次（跨 ~6s）都失败才认定真离线。 */
const OFFLINE_FAILURE_THRESHOLD = 2;
/** 断线自动重启前的静置窗口：给「探测尖峰误判」留恢复时间（期间回到
 *  online 就取消），也避免与 watchdog 的重启节流互相踩。 */
const AUTO_RESTART_DELAY_MS = 8_000;
/** 冷启动失速阈值：splash 在 ~22s 撤掉，此后 connecting 状态对用户没有任何
 *  出口（标题栏的重启入口只在 offline/gaveUp 显示）。与 splash 兜底对齐，
 *  到点把 connectStalled 置位 —— 只提前亮出重启入口，不改判 offline
 *  （90s 冷启动宽限继续生效，慢启动不会被误杀）。 */
const CONNECT_STALLED_MS = 22_000;
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
  /** 离线时的现场快照（进程 / 桥 / 最近探测 / 启动期错误），供界面把
   *  「未连接」翻译成可行动的原因。online 时清空，避免展示过期结论。 */
  const [diagnosis, setDiagnosis] = useState<api.BackendDiagnosis | null>(null);
  // 事件回调里要读到最新状态，但又不能把 state 塞进依赖数组
  // （否则每次状态变化都让 check 换引用，6s 轮询 effect 反复重挂）。
  const stateRef = useRef<ConnState>("connecting");
  const restartingRef = useRef(false);
  restartingRef.current = restarting;
  const lastNoticeRef = useRef(0);
  /** 第一次探测失败的时刻（0 = 还没失败过）。冷启动宽限的计时起点。 */
  const firstFailureAtRef = useRef(0);
  /** 连续探测失败计数（成功即清零）。达到阈值才把 online 判成 offline。 */
  const consecutiveFailuresRef = useRef(0);
  /** offline 自动重启是否已在本段正常期尝试过（防循环：失败落 gaveUp）。 */
  const autoRestartedRef = useRef(false);
  /** 冷启动失速：connecting 持续超过 CONNECT_STALLED_MS，标题栏提前亮出
   *  重启入口（不改判 offline，90s 宽限继续生效）。 */
  const [connectStalled, setConnectStalled] = useState(false);

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
    } else     if (next === "online" && prev === "offline" && now - lastNoticeRef.current > NOTICE_COOLDOWN_MS) {
      lastNoticeRef.current = now;
      toastSuccess("本地后端已恢复。");
    }
    if (next === "online") {
      // 回到 online = 新的一段正常期；offline 自动重启每段只尝试一次。
      autoRestartedRef.current = false;
    }
  }, []);

  /** 取一次后端故障快照。失败（旧 exe 没有该命令）返回 null，不影响判离线。 */
  const refreshDiagnosis = useCallback(async () => {
    try {
      const d = await api.getBackendDiagnosis();
      setDiagnosis(d);
      return d;
    } catch {
      return null;
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
      consecutiveFailuresRef.current += 1;
      if (stateRef.current === "connecting") {
        // 冷启动需要时间，探一次就报离线会让正常启动闪一下"离线"。
        // 但宽限过后必须如实转离线 —— 否则后端根本没起来时界面永远转圈。
        if (firstFailureAtRef.current === 0) firstFailureAtRef.current = Date.now();
        if (Date.now() - firstFailureAtRef.current < BOOT_GRACE_MS) return;
      } else if (restartingRef.current) {
        return; // 重启流程自己收尾（探到 online 或 gave-up）
      } else if (consecutiveFailuresRef.current < OFFLINE_FAILURE_THRESHOLD) {
        return; // 单次超时不算数：后端重负载（审批后 spawn、工具输出洪峰）下常见，等下一轮复判。
      }
      // 判离线前先取现场：三层（进程 / 桥 / 探测）里哪一层断了，决定用户该
      // 查杀软、该重启、还是程序自己就崩在启动期。
      const wasConnecting = stateRef.current === "connecting";
      const d = await refreshDiagnosis();
      transition("offline");
      // 冷启动直接失败（connecting → offline）此前完全静默：顶栏换个图标就
      // 没了，用户不知道内核是没起来还是被拦了。这条 toast 是唯一的现场说明。
      if (wasConnecting && d) {
        const reason = api.describeBackendDiagnosis(d);
        toastError(reason ? `本地后端未连接：${reason}` : "本地后端未连接");
      }
      return;
    }
    consecutiveFailuresRef.current = 0;
    firstFailureAtRef.current = 0;
    transition("online");
    setGaveUp(false);
    setDiagnosis(null);
    try {
      const config = await api.getConfig();
      setMode(String(config.mode ?? "local"));
    } catch {
      // 配置拉取失败不影响连接判定（可能是单次查询失败）。
    }
  }, [transition, refreshDiagnosis]);

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
    reg(listen<{ reason: string; generation: number; current_generation: number }>(
      "hermix:stream-bridge-dead",
      (e) => {
        if (cancelled) return;
        // stdout 读线程退出 = 子进程死了（真离线的确定性信号），立即判离线，
        // 不等连续两次探测（那会多花 ~6s）。旧代读线程的退出事件（新一代
        // 已接管）忽略，否则会把刚重启好的后端误判成离线。
        const p = e.payload;
        if (p?.current_generation !== undefined && p.generation !== p.current_generation) return;
        consecutiveFailuresRef.current = 0;
        transition("offline");
      },
    ));
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
      // 自动重启也失败了 → 取一份现场，界面要把「为什么起不来」说出来。
      void refreshDiagnosis();
    }));

    return () => {
      cancelled = true;
      unlisteners.forEach((fn) => fn());
    };
  }, [check, transition, refreshDiagnosis]);

  const restart = useCallback(async () => {
    setRestarting(true);
    setGaveUp(false);
    firstFailureAtRef.current = 0;
    consecutiveFailuresRef.current = 0;
    try {
      await invoke(IPC.restartSidecar);
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

  // 冷启动失速（A4）：splash 在 ~22s 撤掉后，connecting 没有给用户任何出口。
  // 到点置位 connectStalled，标题栏提前亮出重启入口 —— 不改判 offline。
  useEffect(() => {
    if (state !== "connecting") {
      setConnectStalled(false);
      return;
    }
    const t = setTimeout(() => setConnectStalled(true), CONNECT_STALLED_MS);
    return () => clearTimeout(t);
  }, [state]);

  // 断线自动重启（A3）：此前 offline 只弹 toast「正在自动重连」但从不调用
  // restart —— 文案在撒谎，hung 型后端（进程活着但不应答，watchdog 探不到）
  // 永远没人救。现在 offline 稳定 8s 后自动重启一次；每个 online→offline
  // 周期只试一次，失败落 gaveUp 交还用户。恢复 online 会重置标记。
  useEffect(() => {
    if (state !== "offline" || gaveUp || restarting) return;
    if (autoRestartedRef.current) return;
    autoRestartedRef.current = true;
    const t = setTimeout(() => {
      if (stateRef.current === "offline") void restart();
    }, AUTO_RESTART_DELAY_MS);
    return () => clearTimeout(t);
  }, [state, gaveUp, restarting, restart]);

  return { state, mode, restarting, gaveUp, connectStalled, restart, check, diagnosis };
}
