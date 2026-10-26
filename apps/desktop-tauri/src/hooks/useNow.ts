import { useEffect, useState } from "react";

/**
 * 共享时钟：多个组件订阅同一个 interval，而不是各自 `setInterval`。
 *
 * 背景（2026-10-26，P2-4）：全前端曾有 17 处独立轮询。同时打开 3 个运行中
 * 工具卡就是每秒 3~6 次独立 setState → render，叠加流式输出即稳定掉帧源。
 * 这里按 interval 分桶共享一个 timer，并让**同一 tick 内所有订阅者读到同一个
 * now**（各自 `Date.now()` 会在一帧里给出不同毫秒值，进度条与耗时 chip 抖动）。
 *
 * 页面隐藏时整桶跳过 tick：后台不再产生 render，也不耗电。恢复可见后的第一次
 * tick 会把时间一次性对齐（最长延迟一个 interval）。
 *
 * `active=false` 时不订阅（工具已结束 / 未在压缩），与原来的条件 setInterval
 * 语义一致。
 */
const buckets = new Map<number, { timer: number; subs: Set<() => void> }>();

function subscribe(intervalMs: number, fn: () => void): () => void {
  let bucket = buckets.get(intervalMs);
  if (!bucket) {
    const subs = new Set<() => void>();
    const timer = window.setInterval(() => {
      if (document.hidden) return;
      subs.forEach((s) => s());
    }, intervalMs);
    bucket = { timer, subs };
    buckets.set(intervalMs, bucket);
  }
  const owned = bucket;
  owned.subs.add(fn);
  return () => {
    owned.subs.delete(fn);
    if (owned.subs.size === 0) {
      window.clearInterval(owned.timer);
      buckets.delete(intervalMs);
    }
  };
}

/** 秒级（或自定义间隔）共享时钟；`active` 为 false 时冻结在当前值。 */
export function useNow(active: boolean, intervalMs = 1000): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    const tick = () => setNow(Date.now());
    tick(); // 立即对齐一次，避免显示上一次会话残留的旧值
    return subscribe(intervalMs, tick);
  }, [active, intervalMs]);
  return now;
}
