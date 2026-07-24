// src/hooks/useExpertCutoff.ts
// PLAN_UI_ENHANCEMENT §1.1 — 专家切换分界 hook。
// 工作机制：
// - 每次 setExpert() 时记录当前时间戳为 expertCutoff-{sessionId}
// - 渲染时 MessageBubble 拿到 `cutoffAt: number | null`
// - 历史消息（timestamp < cutoffAt）继续按旧逻辑；新消息按新专家
// 用途：
// - 防止切了专家后历史消息的 avatar/时间/名称突然被覆盖
// - 切换的瞬间成为新旧消息的"分水岭"

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
      // 切换时：null → 有 id，或 id → null，或 idA → idB，**都算切换**
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
