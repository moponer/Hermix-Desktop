/**
 * ToastContainer — renders the global toast stack.
 *
 * Mount once at the App root. Reads from the singleton toast store
 * (see hooks/useToast.ts). No props needed.
 */

import { useToasts, dismissToast } from "../hooks/useToast";
import {
  CheckCircle2,
  AlertCircle,
  Info,
  AlertTriangle,
  X,
} from "lucide-react";

type ToastVariant = "error" | "warning" | "info" | "success";

const variantClass: Record<ToastVariant, string> = {
  error: "toast--error",
  warning: "toast--warning",
  info: "toast--info",
  success: "toast--success",
};

// Lucide icons replace the emoji glyphs (✕ ! ℹ ✓) so the toast
// stack reads as part of the same icon system as the rest of the
// UI instead of feeling like a Bootstrap-era fallback.
const variantIcon: Record<ToastVariant, typeof CheckCircle2> = {
  success: CheckCircle2,
  info: Info,
  warning: AlertTriangle,
  error: AlertCircle,
};

export function ToastContainer() {
  const { toasts } = useToasts();

  if (toasts.length === 0) return null;

  return (
    <div className="toast-container" role="alert" aria-live="polite">
      {toasts.map((t) => {
        const Icon = variantIcon[t.variant] ?? Info;
        const cls = variantClass[t.variant] ?? "toast--info";
        const showProgress = t.duration > 0;
        return (
          <div
            key={t.id}
            className={`toast ${cls}`}
            onClick={() => dismissToast(t.id)}
          >
            <span className="toast__icon" aria-hidden="true">
              <Icon size={14} strokeWidth={2.2} />
            </span>
            <span className="toast__msg">{t.message}</span>
            <button
              type="button"
              className="toast__close"
              aria-label="关闭"
              onClick={(e) => { e.stopPropagation(); dismissToast(t.id); }}
            >
              <X size={12} strokeWidth={2.2} />
            </button>
            {showProgress && (
              <span
                className="toast__progress"
                aria-hidden="true"
                style={{ animationDuration: `${t.duration}ms` }}
              />
            )}
          </div>
        );
      })}
    </div>
  );
}
