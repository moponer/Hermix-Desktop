import { useEffect, useRef } from "react";

/**
 * 定期拉取的统一入口（P2-4）。
 *
 * 相比裸 `setInterval` 的三个改进：
 * 1. **页面隐藏时不发请求**——后台标签页不该继续打后端（原来只有
 *    `useBackendConnection` 自己加了 `visibilityState` 判断）。
 * 2. **恢复可见时立刻补一次**，否则最长要等一个完整 interval 才刷新，
 *    用户切回来看到的是旧数据。
 * 3. **`run` 走 ref**：回调里常带本次渲染的闭包（session id / filter 等），
 *    直接进 `useEffect` 依赖会让 interval 每次渲染重建（抖动 + 丢相位）。
 *
 * `enabled=false` 时不订阅（例如搜索窗未打开、文件搜索 helper 未运行）。
 * 单个 tick 内部仍要自己保障「重入安全」——本 hook 不等待上一次请求完成。
 */
export function usePolling(
  run: () => void | Promise<void>,
  intervalMs: number,
  opts?: { enabled?: boolean; immediate?: boolean },
) {
  const saved = useRef(run);
  saved.current = run;
  const enabled = opts?.enabled ?? true;
  const immediate = opts?.immediate ?? true;

  useEffect(() => {
    if (!enabled) return;
    let stopped = false;
    const tick = () => {
      if (stopped || document.hidden) return;
      void saved.current();
    };
    if (immediate) tick();
    const timer = window.setInterval(tick, intervalMs);
    const onVisible = () => {
      if (!document.hidden) tick();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      stopped = true;
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [intervalMs, enabled]);
}
