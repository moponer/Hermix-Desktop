// useCustomApps — load the list of installed custom apps once per mount,
// refetching when the sidecar signals ready. Without the ready listener,
// if the chat mounts before hermix.exe binds port 3000 (typical startup
// race) the fetch fails once and the list is stuck at 0 forever — even
// after the server recovers. BackendStatus bubbles the same Tauri event,
// so reusing it here costs nothing and keeps both views in sync.
//
// Polling keeps the @app mention card in sync with apps created by the LLM
// mid-conversation — without it, the chat-side list stays at its mount-time
// snapshot while the dedicated management page shows the current state.
//
// 退避而非熔断: 本 hook 挂在常驻的 ChatView 上，比任何页面都早挂载，必然
// 撞上"hermix.exe 尚未 bind 3000"的启动竞态。旧实现连续失败 3 次后彻底
// 停掉 setInterval，且没有 focus 重拉 —— 后端起来了列表也永远空着，@app
// 卡片只剩"创建新应用"一项（管理页因每次进入都重挂载反而正常）。现在改成
// 指数退避（5s→10s→20s→30s 封顶），成功即复位，永不放弃。

import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { listCustomApps, type CustomAppSummary } from "../lib/customAppApi";

const BASE_INTERVAL_MS = 5_000;
const MAX_INTERVAL_MS = 30_000;

export function useCustomApps() {
  const [apps, setApps] = useState<CustomAppSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    let unlistenReady: (() => void) | undefined;
    let unlistenRestarting: (() => void) | undefined;
    let timerId: number | null = null;
    // 连续失败次数只用来拉长间隔，不再用来永久停表。
    let consecutiveFailures = 0;

    const scheduleNext = () => {
      if (cancelled) return;
      // 先清掉在飞的 timer：focus / server-ready 等手动触发的 load 也会走到
      // 这里，不清就会叠加出多条并行轮询链（间隔越跑越短）。
      if (timerId !== null) window.clearTimeout(timerId);
      const backoff = Math.min(
        BASE_INTERVAL_MS * 2 ** consecutiveFailures,
        MAX_INTERVAL_MS,
      );
      timerId = window.setTimeout(runPoll, backoff);
    };

    const runPoll = async () => {
      if (cancelled) return;
      // 熔断窗口内不发请求（底层会直接抛 TypeError），但**不计失败**，
      // 退避一轮后再试 —— 熔断是 15s 冷却，不是永久判死。
      try {
        const list = await listCustomApps();
        if (cancelled) return;
        consecutiveFailures = 0;
        setApps(list ?? []);
        setError(null);
      } catch (e: unknown) {
        if (cancelled) return;
        consecutiveFailures++;
        setError(e instanceof Error ? e.message : "加载自定义应用失败");
        // 不重置 apps — 保留旧的非空值, 避免后端慢启动时短暂清空
      } finally {
        if (!cancelled) setLoading(false);
        scheduleNext();
      }
    };

    const load = async () => {
      if (cancelled) return;
      setLoading(true);
      await runPoll();
    };

    load();

    // 后端 ready 时重拉一次, 抓住"前 mounted、后 up"的竞态。
    listen("hermix:server-ready", () => {
      if (!cancelled) void load();
    }).then((fn) => {
      if (cancelled) fn();
      else unlistenReady = fn;
    });

    // 重启恢复后续再拉一次 (server-restarting 之后可能伴随 server-ready,
    // 双绑防丢)。
    listen("hermix:server-restarting", () => {
      if (!cancelled) void load();
    }).then((fn) => {
      if (cancelled) fn();
      else unlistenRestarting = fn;
    });

    // 窗口重新获得焦点 / 页面重新可见时立刻重拉 —— 与 CustomAppsView
    // 同款兜底。用户从别的窗口切回来时不再等到下一个退避 tick。
    const onFocus = () => {
      if (!cancelled) void runPoll();
    };
    const onVisibility = () => {
      if (document.visibilityState === "visible" && !cancelled) void runPoll();
    };
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onVisibility);

    return () => {
      cancelled = true;
      if (timerId !== null) window.clearTimeout(timerId);
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVisibility);
      unlistenReady?.();
      unlistenRestarting?.();
    };
  }, []);

  return { apps, loading, error };
}
