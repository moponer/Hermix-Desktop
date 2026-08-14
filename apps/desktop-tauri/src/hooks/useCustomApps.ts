// useCustomApps — load the list of installed custom apps once per mount.
// Returns the summaries (id / name / icon / description) used both by the
// @app mention popup and by send-time id resolution.

import { useEffect, useState } from "react";
import { listCustomApps, type CustomAppSummary } from "../lib/customAppApi";

export function useCustomApps() {
  const [apps, setApps] = useState<CustomAppSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    listCustomApps()
      .then((list) => {
        if (cancelled) return;
        setApps(list ?? []);
        setError(null);
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        setError(e instanceof Error ? e.message : "加载自定义应用失败");
        setApps([]);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return { apps, loading, error };
}
