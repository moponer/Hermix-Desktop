import { useEffect, useId, useRef, useState, type PointerEvent as ReactPointerEvent, type ReactNode } from "react";
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
  /** 面板高度；缺省只受 max-h-[85vh] 约束。整幅面板（设置）传具体高度。 */
  height?: number | string;
  /** 内容区类名覆盖。整幅面板需要贴边布局时传 `p-0 overflow-hidden`。 */
  bodyClassName?: string;
  /** 无 title 时的可访问名（设置这类自绘头部的面板用）。 */
  ariaLabel?: string;
  /**
   * Esc 是否直接关闭。默认 true；需要「先清搜索再关」两级行为的调用方
   * 传 false 自行处理（否则第一下 Esc 就把面板关了）。
   */
  closeOnEscape?: boolean;
  /**
   * 面板可拖动：内容里带 `data-modal-drag-handle` 的元素作为拖拽把手
   * （按钮/输入框上的按下不算）。默认 false —— 小确认框不需要挪位置。
   */
  draggable?: boolean;
}

/**
 * 居中弹窗。遮罩用 token（`--color-overlay-scrim`），此前 `.modal-backdrop`
 * 和 `.pane-overlay` 各自硬编码了一个 rgba，是三份不同深度的遮罩。
 *
 * R2-2（2026-10-16）加固：入场动画（复用 `.modal-scale-in` 关键帧）、
 * 遮罩 backdrop-blur、`aria-labelledby` 指向标题、焦点陷阱选择器补
 * details/summary/contenteditable 并排除 disabled 元素。
 *
 * 焦点陷阱（Tab 循环 + Esc 关闭 + 关闭后还焦）与 `inert` 背景是这套东西的
 * 全部价值；少了前两项就是个能聚焦到后面去的 div。
 */
export function Modal({
  open, onClose, title, children, width = 560, footer, className,
  height, bodyClassName, ariaLabel, closeOnEscape = true, draggable = false,
}: ModalProps) {
  const panelRef = useRef<HTMLDivElement>(null);
  const restoreRef = useRef<HTMLElement | null>(null);
  const titleId = useId();

  // 拖动位移只作用在外层包装上：面板本身的入场动画也吃 transform，
  // 两者写在同一元素会互相覆盖。
  const [dragOffset, setDragOffset] = useState({ x: 0, y: 0 });
  const dragRef = useRef<{
    startX: number; startY: number; baseX: number; baseY: number; rect: DOMRect;
  } | null>(null);

  useEffect(() => {
    if (!open) setDragOffset({ x: 0, y: 0 });
  }, [open]);

  useEffect(() => {
    if (!open) return;
    restoreRef.current = document.activeElement as HTMLElement | null;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        if (!closeOnEscape) return;
        e.stopPropagation();
        onClose();
        return;
      }
      if (e.key !== "Tab" || !panelRef.current) return;
      const focusables = Array.from(
        panelRef.current.querySelectorAll<HTMLElement>(
          'button, [href], input, select, textarea, details, summary, [contenteditable="true"], [tabindex]:not([tabindex="-1"])',
        ),
      ).filter((el) => !el.hasAttribute("disabled") && el.offsetParent !== null);
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
  }, [open, onClose, closeOnEscape]);

  /** 拖拽：只在 `[data-modal-drag-handle]` 上按下时启动，按钮/输入框不接管。 */
  const onHandlePointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (!draggable || !panelRef.current) return;
    const target = e.target as HTMLElement;
    if (!target.closest("[data-modal-drag-handle]")) return;
    if (target.closest("button, input, textarea, select, a, [role='button']")) return;
    const rect = panelRef.current.getBoundingClientRect();
    dragRef.current = {
      startX: e.clientX, startY: e.clientY,
      baseX: dragOffset.x, baseY: dragOffset.y, rect,
    };
    e.currentTarget.setPointerCapture(e.pointerId);
    e.preventDefault();
  };

  const onHandlePointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    const d = dragRef.current;
    if (!d) return;
    // 至少留 8px 在视口内，避免把面板拖丢。
    const dx = Math.min(
      Math.max(e.clientX - d.startX, 8 - d.rect.left),
      window.innerWidth - 8 - d.rect.right,
    );
    const dy = Math.min(
      Math.max(e.clientY - d.startY, 8 - d.rect.top),
      window.innerHeight - 8 - d.rect.bottom,
    );
    setDragOffset({ x: d.baseX + dx, y: d.baseY + dy });
  };

  const endDrag = () => { dragRef.current = null; };

  if (!open) return null;

  return createPortal(
    /* 墨与纸 v4 · 弹层纪律（WorkBuddy 设置弹层逐像素采样）：不用遮罩 ——
       背景保持原亮度，浮层靠自身亮度差 + 底部单向投影"坐"在页面上；
       无描边；圆角 17px（radius-xl）。backdrop 仅保留点击外部关闭的
       命中区，视觉全透明。 */
    <div className="fixed inset-0 z-[60] flex items-center justify-center p-4" onClick={onClose}>
      {/* 拖动位移挂在外层：面板自身有 scale-in 动画，同样占用 transform。 */}
      <div
        style={{
          transform: dragOffset.x || dragOffset.y
            ? `translate(${dragOffset.x}px, ${dragOffset.y}px)`
            : undefined,
        }}
      >
      <div
        ref={panelRef}
        onPointerDown={onHandlePointerDown}
        onPointerMove={onHandlePointerMove}
        onPointerUp={endDrag}
        onPointerCancel={endDrag}
        role="dialog"
        aria-modal="true"
        aria-labelledby={title ? titleId : undefined}
        aria-label={title ? undefined : ariaLabel}
        onClick={(e) => e.stopPropagation()}
        style={{ width, maxWidth: "calc(100vw - 2rem)", height }}
        className={cx(
          "flex max-h-[85vh] flex-col overflow-hidden rounded-[var(--radius-xl)]",
          "bg-[var(--color-bg-elevated)]",
          "shadow-[var(--shadow-modal)]",
          "[animation:modal-scale-in_var(--duration-base,160ms)_var(--ease-out,cubic-bezier(0.16,1,0.3,1))_both]",
          className,
        )}
      >
        {title && (
          <div className="flex shrink-0 items-center justify-between gap-3 px-5 py-4">
            <h2
              id={titleId}
              className="text-[length:var(--text-title)] font-semibold leading-tight text-[var(--color-text-primary)]"
            >
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
        <div
          className={cx(
            "min-h-0 flex-1",
            bodyClassName
              ?? "overflow-y-auto px-5 pb-5 text-[length:var(--text-ui)] leading-6 text-[var(--color-text-secondary)]",
          )}
        >
          {children}
        </div>
        {footer && (
          <div className="flex shrink-0 items-center justify-end gap-2 border-t border-[var(--color-border-primary)] bg-[var(--color-bg-secondary)] px-5 py-3">
            {footer}
          </div>
        )}
      </div>
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
  /** 异步确认进行中：双按钮禁用，确认键显示「处理中…」。
   *  （R2-2：从 ToolTabs 私有 ConfirmDialog 收编的能力） */
  busy?: boolean;
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
  busy = false,
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
          <Button variant="secondary" onClick={onCancel} disabled={busy}>
            {cancelLabel}
          </Button>
          <Button variant={confirmVariant} onClick={onConfirm} disabled={busy}>
            {busy ? "处理中…" : confirmLabel}
          </Button>
        </>
      }
    >
      {typeof body === "string" ? <p className="text-[length:var(--text-ui)] leading-6">{body}</p> : body}
    </Modal>
  );
}
