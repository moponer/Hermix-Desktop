// src/components/PolishButton.tsx
// 位置：语音按钮右侧。
// 行为：点击后调 polishChat() API，**覆盖**当前 inputValue；再次点击可撤销。
// 上下文策略：润色前的原文只活在 React state 里，发送时被 state 覆写，
// 因此**润色前原文自然不计入对话上下文**——零额外字段。
//
// 分工：这里只做**同义替换**（更清楚、长度不放大、不加结构）。
// 「把一句话扩写成结构化提示词」是兄弟组件 EnhanceButton 的活
// （/api/polish/enhance），两者提示词模板互不共用。

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
  /** 工具条过窄时把文字收掉，只留图标（同「自动/编码模式」的做法） */
  compact?: boolean;
}

export function PolishButton({
  draft,
  onResult,
  recentTurns,
  hasAttachments,
  disabled,
  compact,
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
      className="flex items-center gap-1 px-2 py-1 rounded transition-colors text-[length:var(--text-meta)] text-text-muted hover:text-text-primary hover:bg-bg-tertiary disabled:opacity-30 disabled:cursor-not-allowed"
    >
      {loading ? (
        <Loader2 className="icon-sm animate-spin" />
      ) : showingPolished ? (
        <RotateCcw className="icon-sm" />
      ) : (
        <Sparkles className="icon-sm" />
      )}
      <span className={compact ? "max-w-0 overflow-hidden whitespace-nowrap" : "truncate"}>
        {showingPolished ? "还原" : "润色"}
      </span>
    </button>
  );
}
