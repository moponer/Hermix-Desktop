// src/components/AiGenerateButton.tsx
// 位置：新建自动化页的 PROMPT 输入区。
// 行为：用户描述大概情况 → AI 润色改写 → 回填到 PROMPT 输入框。
// 与 1.3 的 PolishButton 走 **不同的 prompt 模板**（polish.automation.ts），
// 强调结构化、保留 {{变量}} 占位符、可执行性强。

import { useState, useCallback } from "react";
import { Sparkles, Loader2 } from "lucide-react";
import { polishAutomation } from "../api/polish";
import type { AutomationPolishInput } from "../prompts/polish.automation";

interface AiGenerateButtonProps {
  /** 当前草稿（用户的口语描述） */
  draft: string;
  /** 回填到 PROMPT 输入框（**覆盖**原值） */
  onResult: (generated: string) => void;
  /** 选填：触发类型，让 prompt 模板选对风格 */
  triggerKind?: "cron" | "webhook" | "event" | "manual";
  /** 选填：调度提示 */
  scheduleHint?: string;
  /** 选填：禁用条件 */
  disabled?: boolean;
}

export function AiGenerateButton({
  draft,
  onResult,
  triggerKind,
  scheduleHint,
  disabled,
}: AiGenerateButtonProps) {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const canGenerate = !loading && !disabled && draft.trim().length > 0;

  const handleClick = useCallback(async () => {
    if (!canGenerate) return;
    setLoading(true);
    setError(null);
    try {
      const payload: AutomationPolishInput = {
        draft,
        ctx: { triggerKind, scheduleHint },
      };
      const generated = await polishAutomation(payload);
      // 重要：直接覆盖，**不弹窗、不显示 diff**
      onResult(generated);
    } catch (e) {
      setError(e instanceof Error ? e.message : "AI 生成失败");
    } finally {
      setLoading(false);
    }
  }, [canGenerate, draft, triggerKind, scheduleHint, onResult]);

  return (
    <button
      type="button"
      onClick={handleClick}
      disabled={!canGenerate}
      title={error ?? "AI 生成结构化 PROMPT"}
      className="flex items-center gap-1 px-2 py-1 rounded text-text-muted hover:text-text-primary hover:bg-bg-tertiary disabled:opacity-30 disabled:cursor-not-allowed transition-colors text-[12px]"
    >
      {loading ? <Loader2 className="icon-sm animate-spin" /> : <Sparkles className="icon-sm" />}
      <span>AI 生成</span>
    </button>
  );
}
