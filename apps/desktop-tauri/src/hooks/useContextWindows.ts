import { useEffect, useState } from "react";

import { resolveContextWindows } from "../api/rest";

export type ContextWindowEntry = { value: number; source: string };

/**
 * Backend-authoritative context-window resolution for a set of models.
 *
 * The backend owns the static catalog
 * (`model_metadata::get_model_context_length_with_source`); the settings UI
 * used to hand-mirror that table so it could label where a number came from,
 * and the two copies drifted apart. This hook asks the backend instead.
 *
 * Resolves to `{}` when the backend is unreachable (older build, server
 * down) — callers then display the documented 128K default rather than
 * inventing a number locally.
 */
export function useContextWindows(
  models: string[],
): Record<string, ContextWindowEntry> {
  const [windows, setWindows] = useState<Record<string, ContextWindowEntry>>({});
  // Depend on the joined ids so a fresh array literal each render doesn't
  // re-trigger the effect.
  const key = models.join("\u0000");

  useEffect(() => {
    const list = key ? key.split("\u0000") : [];
    if (list.length === 0) {
      setWindows({});
      return;
    }
    let cancelled = false;
    resolveContextWindows(list)
      .then((resolved) => {
        if (!cancelled) setWindows(resolved);
      })
      .catch(() => {
        if (!cancelled) setWindows({});
      });
    return () => {
      cancelled = true;
    };
  }, [key]);

  return windows;
}
