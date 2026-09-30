import { useEffect, useRef, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { X } from "lucide-react";

import { cx } from "../../lib/cx";
import { Button } from "./Button";

export interface ModalProps {
  open: boolean;
  onClose: () => void;
  title?: ReactNode;
  children: ReactNode;
  width?: number;
  footer?: ReactNode;
  className?: string;
}

/**
 * 居中弹窗。遮罩用 token（`--color-overlay-scrim`），此前 `.modal-backdrop`
 * 和 `.pane-overlay` 各自硬编码了一个 rgba，是三份不同深度的遮罩。
 *
 * 焦点陷阱（Tab 循环 + Esc 关闭 + 关闭后还焦）与 `inert` 背景是这套东西的
 * 全部价值；少了前两项就是个能聚焦到后面去的 div。
 */
export function Modal({ open, onClose, title, children, width = 560, footer, className }: ModalProps) {
  const panelRef = useRef<HTMLDivElement>(null);
  const restoreRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    if (!open) return;
    restoreRef.current = document.activeElement as HTMLElement | null;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
        return;
      }
      if (e.key !== "Tab" || !panelRef.current) return;
      const focusables = panelRef.current.querySelectorAll<HTMLElement>(
        'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])',
      );
      if (focusables.length === 0) return;
      const first = focusables[0]!;
      const last = focusables[focusables.length - 1]!;
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKey, true);
    const t = window.setTimeout(() => panelRef.current?.querySelector<HTMLElement>("button, input")?.focus(), 0);
    return () => {
      document.removeEventListener("keydown", onKey, true);
      window.clearTimeout(t);
      restoreRef.current?.focus?.();
    };
  }, [open, onClose]);

  if (!open) return null;

  return createPortal(
    <div
      className="fixed inset-0 z-[60] flex items-center justify-center bg-[var(--color-overlay-scrim)] p-4"
      onClick={onClose}
    >
      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        onClick={(e) => e.stopPropagation()}
        style={{ width, maxWidth: "calc(100vw - 2rem)" }}
        className={cx(
          "flex max-h-[85vh] flex-col overflow-hidden rounded-[var(--radius-3xl)]",
          "border border-[var(--color-border-primary)] bg-[var(--color-bg-elevated)]",
          "shadow-[var(--shadow-modal)]",
          className,
        )}
      >
        {title && (
          <div className="flex shrink-0 items-center justify-between gap-3 px-5 py-4">
            <h2 className="text-[length:var(--text-title)] font-semibold leading-tight text-[var(--color-text-primary)]">
              {title}
            </h2>
            <button
              onClick={onClose}
              aria-label="关闭"
              className="grid size-9 shrink-0 place-items-center rounded-full text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-bg-hover)] hover:text-[var(--color-text-primary)]"
            >
              <X className="size-[18px]" />
            </button>
          </div>
        )}
        <div className="min-h-0 flex-1 overflow-y-auto px-5 pb-5 text-[length:var(--text-ui)] leading-6 text-[var(--color-text-secondary)]">
          {children}
        </div>
        {footer && (
          <div className="flex shrink-0 items-center justify-end gap-2 border-t border-[var(--color-border-primary)] bg-[var(--color-bg-secondary)] px-5 py-3">
            {footer}
          </div>
        )}
      </div>
    </div>,
    document.body,
  );
}

export interface ConfirmDialogProps {
  open: boolean;
  title: string;
  body: ReactNode;
  confirmLabel?: string;
  cancelLabel?: string;
  /** 危险操作（删除、撤销、解绑）用 danger —— 默认就是 danger，
   *  因为确认弹窗绝大多数场景确实是危险操作。 */
  confirmVariant?: "primary" | "danger";
  onConfirm: () => void;
  onCancel: () => void;
}

export function ConfirmDialog({
  open,
  title,
  body,
  confirmLabel = "确认",
  cancelLabel = "取消",
  confirmVariant = "danger",
  onConfirm,
  onCancel,
}: ConfirmDialogProps) {
  return (
    <Modal
      open={open}
      onClose={onCancel}
      title={title}
      width={460}
      footer={
        <>
          <Button variant="secondary" onClick={onCancel}>
            {cancelLabel}
          </Button>
          <Button variant={confirmVariant} onClick={onConfirm}>
            {confirmLabel}
          </Button>
        </>
      }
    >
      {typeof body === "string" ? <p className="text-[length:var(--text-ui)] leading-6">{body}</p> : body}
    </Modal>
  );
}
