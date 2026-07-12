// src/components/PlanConfirmationDialog.tsx
//
// P2-4: Plan 模式方案确认弹层
//
// 当 Agent 在 Plan 模式下产出方案后，弹出此对话框。
// 用户确认后 PATCH task status → in_progress，Agent 继续执行。
// 用户取消则 PATCH task status → archived。
//
// 样式遵守铁律 V：无蓝紫，用 gold/amber + gray/zinc。
// Dark 双轨：通过 CSS 变量自动适配。

import { memo, useState, useCallback } from "react";
import { clsx } from "clsx";
import { Check, X, ClipboardList, Loader2 } from "lucide-react";

export interface PlanConfirmationDialogProps {
  /** Task ID */
  taskId: string;
  /** Agent 产出的方案文本 */
  planText: string;
  /** 确认执行回调 → PATCH status → in_progress */
  onConfirm: (taskId: string) => Promise<void>;
  /** 取消回调 → PATCH status → archived */
  onCancel: (taskId: string) => Promise<void>;
  /** 关闭弹层（不触发任何状态变更） */
  onDismiss?: () => void;
}

export const PlanConfirmationDialog = memo(function PlanConfirmationDialog({
  taskId,
  planText,
  onConfirm,
  onCancel,
  onDismiss,
}: PlanConfirmationDialogProps) {
  const [busy, setBusy] = useState<"confirm" | "cancel" | null>(null);
  const [error, setError] = useState<string | null>(null);

  const handleConfirm = useCallback(async () => {
    setBusy("confirm");
    setError(null);
    try {
      await onConfirm(taskId);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  }, [taskId, onConfirm]);

  const handleCancel = useCallback(async () => {
    setBusy("cancel");
    setError(null);
    try {
      await onCancel(taskId);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  }, [taskId, onCancel]);

  return (
    <div className="fixed inset-0 z-[100] flex items-center justify-center p-4">
      {/* Backdrop */}
      <button
        type="button"
        aria-label="关闭"
        onClick={onDismiss}
        className="absolute inset-0 bg-black/40 backdrop-blur-sm"
      />

      {/* Dialog */}
      <div
        className={clsx(
          "relative z-10 w-full max-w-[600px] max-h-[80vh] flex flex-col",
          "rounded-2xl border shadow-2xl",
          "bg-bg-secondary border-border-subtle",
        )}
      >
        {/* Header */}
        <div className="flex items-center gap-2.5 px-5 py-4 border-b border-border-subtle">
          <div
            className="w-8 h-8 rounded-lg flex items-center justify-center"
            style={{ background: "hsla(150, 60%, 50%, 0.12)" }}
          >
            <ClipboardList size={16} style={{ color: "hsl(150, 60%, 45%)" }} />
          </div>
          <div className="flex-1">
            <h2 className="text-[14px] font-bold text-text-primary">
              方案确认
            </h2>
            <p className="text-[10.5px] text-text-muted">
              Agent 已产出执行方案，请确认后继续
            </p>
          </div>
          {onDismiss && (
            <button
              type="button"
              onClick={onDismiss}
              className="p-1.5 rounded-md text-text-muted hover:text-text-secondary hover:bg-bg-tertiary transition-colors"
            >
              <X size={14} />
            </button>
          )}
        </div>

        {/* Plan content */}
        <div className="flex-1 overflow-y-auto px-5 py-4">
          <div
            className="rounded-xl p-3 border"
            style={{
              background: "hsla(43, 96%, 56%, 0.06)",
              borderColor: "hsla(43, 96%, 56%, 0.20)",
            }}
          >
            <p className="text-[10.5px] font-bold text-text-muted uppercase tracking-wider mb-2">
              执行方案
            </p>
            <div className="text-[13px] text-text-primary leading-relaxed whitespace-pre-wrap">
              {planText}
            </div>
          </div>

          {error && (
            <p className="mt-3 text-[11.5px] text-red-400">{error}</p>
          )}
        </div>

        {/* Footer — action buttons */}
        <div className="flex items-center gap-2.5 px-5 py-4 border-t border-border-subtle">
          <button
            type="button"
            onClick={handleConfirm}
            disabled={busy !== null}
            className={clsx(
              "flex items-center gap-1.5 px-4 py-2 rounded-lg text-[12.5px] font-bold",
              "text-white transition-opacity",
              busy === "confirm" ? "opacity-70" : "hover:opacity-90",
              "disabled:cursor-not-allowed",
            )}
            style={{ background: "hsl(150, 60%, 45%)" }}
          >
            {busy === "confirm" ? (
              <Loader2 size={13} className="animate-spin" />
            ) : (
              <Check size={13} />
            )}
            确认执行
          </button>

          <button
            type="button"
            onClick={handleCancel}
            disabled={busy !== null}
            className={clsx(
              "flex items-center gap-1.5 px-4 py-2 rounded-lg text-[12.5px] font-semibold",
              "text-text-muted hover:text-red-400 hover:bg-red-500/10",
              "transition-colors disabled:opacity-50 disabled:cursor-not-allowed",
            )}
          >
            {busy === "cancel" ? (
              <Loader2 size={13} className="animate-spin" />
            ) : (
              <X size={13} />
            )}
            取消
          </button>

          <span className="ml-auto text-[10.5px] text-text-faint">
            确认后 Agent 将开始执行
          </span>
        </div>
      </div>
    </div>
  );
});
