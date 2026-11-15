/**
 * ToastContainer — renders the global toast stack.
 *
 * Mount once at the App root. Reads from the singleton toast store
 * (see hooks/useToast.ts). No props needed.
 *
 * C 方向（design-previews/2026-10-12-toast-redesign）：右下角紧凑条，
 * 语义色小圆点承担状态标识，进度条与圆底图标已随胶囊形态一起废除；
 * 关闭按钮 hover / focus 才现身（样式在 base.css toast 段）。
 */

import { useToasts, dismissToast } from "../hooks/useToast";
import { X } from "lucide-react";
import { CLOSE } from "./strings";

type ToastVariant = "error" | "warning" | "info" | "success";

const variantClass: Record<ToastVariant, string> = {
  error: "toast--error",
  warning: "toast--warning",
  info: "toast--info",
  success: "toast--success",
};

export function ToastContainer() {
  const { toasts } = useToasts();

  if (toasts.length === 0) return null;

  return (
    <div className="toast-container" role="alert" aria-live="polite">
      {toasts.map((t) => {
        const cls = variantClass[t.variant] ?? "toast--info";
        return (
          <div
            key={t.id}
            className={`toast ${cls}`}
            onClick={() => dismissToast(t.id)}
          >
            <span className="toast__dot" aria-hidden="true" />
            <span className="toast__msg">{t.message}</span>
            <button
              type="button"
              className="toast__close"
              aria-label={CLOSE}
              onClick={(e) => { e.stopPropagation(); dismissToast(t.id); }}
            >
              <X className="icon-sm" />
            </button>
          </div>
        );
      })}
    </div>
  );
}
