// Expert-switch cutoff.
// Switching experts must not retroactively restyle earlier messages:
// we stamp a cutoff timestamp at every switch, and MessageBubble renders
// messages before the cutoff with their original expert.

import { useEffect, useState, useCallback } from "react";
import { storage, sessionKey } from "../utils/storageCompat";

const EXPERT_CUTOFF_KEY = (sessionId: string) => sessionKey(sessionId, "expertCutoffAt");

export function useExpertCutoff(sessionId: string | null | undefined): {
  cutoffAt: number | null;
  recordCutoff: (expertId: string | null) => void;
} {
  const [cutoffAt, setCutoffAt] = useState<number | null>(() => {
    if (!sessionId) return null;
    try {
      const raw = storage.getItem(EXPERT_CUTOFF_KEY(sessionId));
      if (!raw) return null;
      const n = Number(raw);
      return Number.isFinite(n) && n > 0 ? n : null;
    } catch {
      return null;
    }
  });

  useEffect(() => {
    if (!sessionId) {
      setCutoffAt(null);
      return;
    }
    try {
      const raw = storage.getItem(EXPERT_CUTOFF_KEY(sessionId));
      const n = raw ? Number(raw) : NaN;
      setCutoffAt(Number.isFinite(n) && n > 0 ? n : null);
    } catch {
      setCutoffAt(null);
    }
  }, [sessionId]);

  const recordCutoff = useCallback(
    (_expertId: string | null) => {
      if (!sessionId) return;
      // Any transition (set / clear / swap) counts as a switch.
      const now = Date.now();
      try {
        storage.setItem(EXPERT_CUTOFF_KEY(sessionId), String(now));
        setCutoffAt(now);
      } catch {}
    },
    [sessionId],
  );

  return { cutoffAt, recordCutoff };
}
