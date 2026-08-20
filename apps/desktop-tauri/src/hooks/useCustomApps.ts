// useCustomApps — load the list of installed custom apps once per mount,
// refetching when the sidecar signals ready. Without the ready listener,
// if the chat mounts before hermix.exe binds port 3000 (typical startup
// race) the fetch fails once and the list is stuck at 0 forever — even
// after the server recovers. BackendStatus bubbles the same Tauri event,
// so reusing it here costs nothing and keeps both views in sync.

import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { listCustomApps, type CustomAppSummary } from "../lib/customAppApi";

export function useCustomApps() {
  const [apps, setApps] = useState<CustomAppSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    let unlistenReady: (() => void) | undefined;
    let unlistenRestarting: (() => void) | undefined;

    const load = () => {
      listCustomApps()
        .then((list) => {
          if (cancelled) return;
          setApps(list ?? []);
          setError(null);
        })
        .catch((e: unknown) => {
          if (cancelled) return;
          setError(e instanceof Error ? e.message : "加载自定义应用失败");
          // 不重置 apps — 保留旧的非空值, 避免后端慢启动时短暂清空
        })
        .finally(() => {
          if (!cancelled) setLoading(false);
        });
    };

    load();

    // 后端 ready 时重拉一次, 抓住"前 mounted、后 up"的竞态。
    listen("hermix:server-ready", () => {
      if (!cancelled) load();
    }).then((fn) => {
      if (cancelled) fn();
      else unlistenReady = fn;
    });

    // 重启恢复后续再拉一次 (server-restarting 之后可能伴随 server-ready,
    // 双绑防丢)。
    listen("hermix:server-restarting", () => {
      if (!cancelled) load();
    }).then((fn) => {
      if (cancelled) fn();
      else unlistenRestarting = fn;
    });

    return () => {
      cancelled = true;
      unlistenReady?.();
      unlistenRestarting?.();
    };
  }, []);

  return { apps, loading, error };
}
