// src/components/PolishButton.tsx
//
// v1 (2026-06-22): PLAN_UI_ENHANCEMENT §1.3 — 对话润色按钮。
// 位置：语音按钮右侧。
// 行为：点击后调 polishChat() API，**覆盖**当前 inputValue。
// 上下文策略：润色前的原文只活在 React state 里，发送时被 state 覆写，
//            因此**润色前原文自然不计入对话上下文**——零额外字段。

import { useState, useCallback } from "react";
import { Sparkles, Loader2 } from "lucide-react";
import { polishChat } from "../api/polish";
import type { ChatPolishInput } from "../prompts/polish.chat";

interface PolishButtonProps {
  /** 当前草稿原文 */
  draft: string;
  /** 把润色结果回填到输入框（**覆盖**原值） */
  onResult: (polished: string) => void;
  /** 选填：当前对话上下文片段，让润色更连贯 */
  recentTurns?: string;
  /** 选填：是否有附件 */
  hasAttachments?: boolean;
  /** 选填：禁用条件 */
  disabled?: boolean;
}

export function PolishButton({
  draft,
  onResult,
  recentTurns,
  hasAttachments,
  disabled,
}: PolishButtonProps) {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const canPolish = !loading && !disabled && draft.trim().length > 0;

  const handleClick = useCallback(async () => {
    if (!canPolish) return;
    setLoading(true);
    setError(null);
    try {
      const payload: ChatPolishInput = {
        draft,
        ctx: {
          recentTurns,
          hasAttachments,
        },
      };
      const polished = await polishChat(payload);
      // 重要：直接覆盖，**不弹窗、不显示 diff、不保留原草稿**
      onResult(polished);
    } catch (e) {
      setError(e instanceof Error ? e.message : "润色失败");
    } finally {
      setLoading(false);
    }
  }, [canPolish, draft, recentTurns, hasAttachments, onResult]);

  return (
    <button
      type="button"
      onClick={handleClick}
      disabled={!canPolish}
      title={error ?? "润色输入内容"}
      className="flex items-center justify-center w-8 h-8 rounded-lg transition-all disabled:opacity-30 disabled:cursor-not-allowed text-text-muted hover:text-text-primary hover:bg-bg-tertiary"
    >
      {loading ? <Loader2 size={14} className="animate-spin" /> : <Sparkles size={14} />}
    </button>
  );
}
