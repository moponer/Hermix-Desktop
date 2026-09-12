// src/components/PolishButton.tsx
// 位置：语音按钮右侧。
// 行为：点击后调 polishChat() API，**覆盖**当前 inputValue；再次点击可撤销。
// 上下文策略：润色前的原文只活在 React state 里，发送时被 state 覆写，
// 因此**润色前原文自然不计入对话上下文**——零额外字段。

import { useCallback, useEffect, useState } from "react";
import { Sparkles, Loader2, RotateCcw } from "lucide-react";
import { polishChat } from "../api/polish";
import { toastError } from "../hooks/useToast";
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
  const [lastOriginal, setLastOriginal] = useState<string | null>(null);
  const [lastPolished, setLastPolished] = useState<string | null>(null);

  // 当前输入框里的内容就是最近一次润色结果，显示撤销态；
  // 用户一旦手动编辑，draft !== lastPolished，撤销态自动消失。
  const showingPolished = lastPolished !== null && draft === lastPolished && lastOriginal !== null;

  const canAct = !loading && !disabled && draft.trim().length > 0;

  const handleClick = useCallback(async () => {
    if (loading) return;

    if (showingPolished && lastOriginal !== null) {
      onResult(lastOriginal);
      setLastOriginal(null);
      setLastPolished(null);
      return;
    }

    if (!draft.trim()) return;

    setLoading(true);
    try {
      const payload: ChatPolishInput = {
        draft,
        ctx: {
          recentTurns,
          hasAttachments,
        },
      };
      const polished = await polishChat(payload);
      setLastOriginal(draft);
      setLastPolished(polished);
      onResult(polished);
    } catch (e) {
      const msg = e instanceof Error ? e.message : "润色失败";
      toastError(msg);
    } finally {
      setLoading(false);
    }
  }, [loading, showingPolished, lastOriginal, draft, recentTurns, hasAttachments, onResult]);

  // 快捷键：textarea 聚焦时按 Ctrl+/ 或 ⌘+/ 触发润色/撤销。
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key !== "/") return;
      if (!(e.ctrlKey || e.metaKey)) return;
      const active = document.activeElement;
      if (!active || active.tagName !== "TEXTAREA") return;
      e.preventDefault();
      handleClick();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [handleClick]);

  const title = loading
    ? "润色中…"
    : showingPolished
    ? "还原原文"
    : "润色输入内容 (Ctrl+/ / ⌘+/)";

  return (
    <button
      type="button"
      onClick={handleClick}
      disabled={!canAct}
      title={title}
      className="flex items-center justify-center w-8 h-8 rounded-lg transition-[color,background-color,border-color,opacity] disabled:opacity-30 disabled:cursor-not-allowed text-text-muted hover:text-text-primary hover:bg-bg-tertiary"
    >
      {loading ? (
        <Loader2 className="icon-md animate-spin" />
      ) : showingPolished ? (
        <RotateCcw className="icon-md" />
      ) : (
        <Sparkles className="icon-md" />
      )}
    </button>
  );
}
