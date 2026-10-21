// src/components/EnhanceButton.tsx
// 位置：输入框工具条，润色按钮右侧。
// 行为：**一键**把草稿扩写成结构化提示词回填输入框；再点一次 = 还原原文。
//
// 与 PolishButton（润色）的分工：
// - 润色：同义替换，文字更清楚，长度不放大。
// - 增强：**扩写**，补齐角色/背景/任务拆解/输入/约束/输出格式，
//   段内按草稿隐含的场景（周报/方案/竞品分析…）落到具体产出规格。
//   场景规格表在后端提示词里（ENHANCE_PROMPT），**用户不需要选场景**。
// 两者提示词模板互不共用。

import { useCallback, useEffect, useState } from "react";
import { Wand2, Loader2, RotateCcw } from "lucide-react";
import { polishEnhance } from "../api/polish";
import { toastError } from "../hooks/useToast";

interface EnhanceButtonProps {
  /** 当前草稿原文 */
  draft: string;
  /** 把增强结果回填到输入框（**覆盖**原值） */
  onResult: (enhanced: string) => void;
  /** 选填：当前对话上下文片段，让增强出的提示词更连贯 */
  recentTurns?: string;
  /** 选填：是否有附件 */
  hasAttachments?: boolean;
  /** 选填：禁用条件 */
  disabled?: boolean;
  /** 工具条过窄时把文字收掉，只留图标（同「自动/编码模式」的做法） */
  compact?: boolean;
}

export function EnhanceButton({
  draft,
  onResult,
  recentTurns,
  hasAttachments,
  disabled,
  compact,
}: EnhanceButtonProps) {
  const [loading, setLoading] = useState(false);
  const [lastOriginal, setLastOriginal] = useState<string | null>(null);
  const [lastEnhanced, setLastEnhanced] = useState<string | null>(null);

  // 输入框里现在就是最近一次的增强结果 → 显示撤销态；
  // 用户手动改过（draft !== lastEnhanced）则撤销态自动消失。
  const showingEnhanced = lastEnhanced !== null && draft === lastEnhanced && lastOriginal !== null;

  const canAct = !loading && !disabled && draft.trim().length > 0;

  const handleClick = useCallback(async () => {
    if (loading) return;

    // 已经增强过 → 这次点击是撤销（不重复调用）。
    if (showingEnhanced && lastOriginal !== null) {
      onResult(lastOriginal);
      setLastOriginal(null);
      setLastEnhanced(null);
      return;
    }

    if (!draft.trim()) return;

    setLoading(true);
    try {
      const enhanced = await polishEnhance({
        draft,
        ctx: { recentTurns, hasAttachments },
      });
      setLastOriginal(draft);
      setLastEnhanced(enhanced);
      onResult(enhanced);
    } catch (e) {
      const msg = e instanceof Error ? e.message : "增强失败";
      toastError(msg);
    } finally {
      setLoading(false);
    }
  }, [loading, showingEnhanced, lastOriginal, draft, recentTurns, hasAttachments, onResult]);

  // 快捷键：textarea 聚焦时 Ctrl+Shift+/ 触发增强（Ctrl+/ 是润色）。
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.code !== "Slash" || !e.shiftKey) return;
      if (!(e.ctrlKey || e.metaKey)) return;
      const active = document.activeElement;
      if (!active || active.tagName !== "TEXTAREA") return;
      e.preventDefault();
      void handleClick();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [handleClick]);

  const title = loading
    ? "增强中…"
    : showingEnhanced
    ? "还原原文"
    : "增强提示词：把一句话扩写成角色/背景/任务/输入/约束/输出格式 (Ctrl+Shift+/)";

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
      ) : showingEnhanced ? (
        <RotateCcw className="icon-sm" />
      ) : (
        <Wand2 className="icon-sm" />
      )}
      <span className={compact ? "max-w-0 overflow-hidden whitespace-nowrap" : "truncate"}>
        {showingEnhanced ? "还原" : "增强"}
      </span>
    </button>
  );
}
