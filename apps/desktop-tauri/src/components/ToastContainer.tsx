/**
 * ToastContainer — renders the global toast stack.
 *
 * Mount once at the App root. Reads from the singleton toast store
 * (see hooks/useToast.ts). No props needed.
 */

import { useToasts, dismissToast } from "../hooks/useToast";

const variantClass: Record<string, string> = {
  error: "toast--error",
  warning: "toast--warning",
  info: "toast--info",
  success: "toast--success",
};

const variantIcon: Record<string, string> = {
  error: "✕",
  warning: "!",
  info: "ℹ",
  success: "✓",
};

export function ToastContainer() {
  const { toasts } = useToasts();

  if (toasts.length === 0) return null;

  return (
    <div className="toast-container" role="alert" aria-live="polite">
      {toasts.map((t) => (
        <div
          key={t.id}
          className={`toast ${variantClass[t.variant] ?? "toast--info"}`}
          onClick={() => dismissToast(t.id)}
        >
          <span className="toast__icon">{variantIcon[t.variant] ?? "ℹ"}</span>
          <span className="toast__msg">{t.message}</span>
          <button
            className="toast__close"
            aria-label="关闭"
            onClick={(e) => { e.stopPropagation(); dismissToast(t.id); }}
          >×</button>
        </div>
      ))}
    </div>
  );
}
