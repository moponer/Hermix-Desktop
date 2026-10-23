import { useEffect, useId, useRef, useState, type PointerEvent as ReactPointerEvent, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { AlertTriangle, X } from "lucide-react";

import { cx } from "../../lib/cx";
import { useFocusTrap } from "../../hooks/useFocusTrap";
import { Button } from "./Button";
import { CANCEL, CLOSE, CONFIRM } from "../strings";

/**
 * 弹窗宽度档。**必须取自 `--width-*`**（tokens.css:170-176）：
 * narrow 240 / base 420 / wide 720 / full 100%。
 *
 * 此前 8 种裸数字宽度（420/448/460/560/720/800/1080）并存，是"每个文件
 * 自己写一个弹窗"的直接后果 —— 同一屏里两个弹窗不一样宽，看起来就像两个产品。
 */
export type ModalWidth = "narrow" | "base" | "wide" | "panel" | "full";

const WIDTH_VAR: Record<ModalWidth, string> = {
  narrow: "var(--width-narrow)",
  base: "var(--width-base)",
  wide: "var(--width-wide)",
  panel: "var(--width-panel)",
  full: "100%",
};

/**
 * 弹窗面板高度上限（全站统一档）。
 *
 * 此前 6 档并存：85vh（ui/Modal）/ 90vh（AutomationView×3）/ 80vh（TestDialog）/
 * 70vh（.modal-form）/ 60vh / 40vh（DiagnosticsTab）。收编后一律走这一档。
 *
 * 建议主控 agent 后续把它提升为全局令牌 `--dialog-max-h`（tokens.css），
 * 届时本文件改为消费 var(--dialog-max-h)。此处暂用常量而不是 var()，是因为
 * `scripts/check-theme-tokens.mjs` 的孤儿引用检查要求每个 var() 引用都能在
 * `src/styles/*.css` 里找到定义，而 tokens.css 不在本批次的可改文件内。
 *
 * 2026-10-21：`--dialog-max-h` 令牌已落地到 tokens.css，这里改为消费它 ——
 * 弹窗最大高度此前散着 85/90/80/70/60/40vh 六种，现在只有一档。
 */
export const DIALOG_MAX_H = "var(--dialog-max-h)";

export interface ModalProps {
  open: boolean;
  onClose: () => void;
  title?: ReactNode;
  children: ReactNode;
  /**
   * 宽度档。**必须取自 `--width-*`**：`narrow` / `base` / `wide` / `full`。
   * 裸数字只作迁移期兼容（存量调用方归其他批次所有），新代码一律用档位名。
   */
  width?: ModalWidth | number;
  footer?: ReactNode;
  className?: string;
  /** 面板高度；缺省只受 max-h（DIALOG_MAX_H）约束。整幅面板（设置）传具体高度。 */
  height?: number | string;
  /** 遮罩层 z 阶。默认 60；「强制更新」这类必须盖住一切的场景传更大值。 */
  zIndex?: number;
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
   * 遮罩点击是否关闭。默认 true。
   *
   * **表单类弹窗（有未保存改动）传 false** —— 点一下遮罩内容就没了，
   * 是最难复现也最招人恨的一种丢数据。Esc 有 `closeOnEscape` 可关，
   * 遮罩此前没有对应开关（问题 45）。
   */
  closeOnBackdrop?: boolean;
  /**
   * 可见遮罩。默认 false —— 墨与纸 v4 的弹层纪律是「不用遮罩」：背景保持
   * 原亮度，浮层靠自身亮度差 + 底部单向投影"坐"在页面上；遮罩只保留点击
   * 外部关闭的命中区。需要把背景压暗的模态（确认框 / 命令面板）显式传 true。
   */
  scrim?: boolean;
  /** 垂直对齐：center（默认）/ top（命令面板式贴顶）。 */
  align?: "center" | "top";
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
  open, onClose, title, children, width = "base", footer, className,
  height, zIndex, bodyClassName, ariaLabel, closeOnEscape = true, closeOnBackdrop = true,
  scrim = false, align = "center", draggable = false,
}: ModalProps) {
  const panelRef = useRef<HTMLDivElement>(null);

  /* onClose 走 ref：调用方几乎都传内联箭头函数（`onClose={() => setX(null)}`），
     每次父组件渲染都是新函数。若把它放进下面那个 effect 的依赖里，effect 会
     在父组件每次渲染时重跑 —— cleanup 会把焦点抢回"打开前的元素"，
     搜索框这类需要持续输入的控件会被反复夺走焦点。 */
  const onCloseRef = useRef(onClose);
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
    onCloseRef.current = onClose;
  }, [onClose]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        if (!closeOnEscape) return;
        e.stopPropagation();
        onCloseRef.current();
      }
    };
    document.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("keydown", onKey, true);
    };
  }, [open, closeOnEscape]);

  // 焦点陷阱（Tab 循环 / 初始落焦 / 关闭后归还）由 hook 提供 ——
  // PreviewPanel 的全屏覆盖层同样声明 aria-modal="true"，两处共用同一份实现。
  useFocusTrap(panelRef, open, "[data-modal-autofocus]");

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

  const panelWidth = typeof width === "number" ? width : WIDTH_VAR[width];

  return createPortal(
    /* 墨与纸 v4 · 弹层纪律（WorkBuddy 设置弹层逐像素采样）：默认不用遮罩 ——
       背景保持原亮度，浮层靠自身亮度差 + 底部单向投影"坐"在页面上；
       无描边；圆角 17px（radius-xl）。需要压暗背景的模态传 scrim，遮罩用
       `--color-overlay-scrim` 令牌。 */
    <div
      className={cx(
        "fixed inset-0 flex justify-center p-4",
        align === "top" ? "items-start pt-[12vh]" : "items-center",
        scrim && "bg-[var(--color-overlay-scrim)] backdrop-blur-sm",
      )}
      style={{ zIndex: zIndex ?? 60 }}
      onClick={closeOnBackdrop ? onClose : undefined}
    >
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
        style={{
          width: panelWidth,
          maxWidth: "calc(100vw - 2rem)",
          height,
          maxHeight: DIALOG_MAX_H,
        }}
        className={cx(
          "flex flex-col overflow-hidden rounded-[var(--radius-xl)]",
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
              className="min-w-0 text-[length:var(--text-title)] font-semibold leading-tight text-[var(--color-text-primary)]"
            >
              {title}
            </h2>
            {/* 关闭按钮尺寸全站统一：32px 方形 + 16px 图标（问题 44 —— 此前
                同一动作有 36/28/24px 三种尺寸）。 */}
            <button
              onClick={onClose}
              aria-label={CLOSE}
              className="grid size-8 shrink-0 place-items-center rounded-full text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-bg-hover)] hover:text-[var(--color-text-primary)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]"
            >
              <X className="icon-lg" />
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
          /* 底栏靠**面亮度差**分层（bg-secondary 比 elevated 低一档），不画
             border-t —— 2026-10-23 去描边后，全站只剩这一处还在给 footer
             画线，同一屏里弹窗有边、其它面板没边，看起来像两套设计。 */
          <div className="flex shrink-0 items-center justify-end gap-2 bg-[var(--color-bg-secondary)] px-5 py-3">
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
  /** 标题（纯文本优先；传节点时请自行保证语义）。 */
  title: ReactNode;
  body: ReactNode;
  /**
   * 标题左侧的语义图标（`Trash2` / `AlertTriangle` / `LogOut` …）。
   * 不传则只有文字。**色调由 confirmVariant 决定**，调用方只给图标本身，
   * 避免各页自己挑颜色（此前 Sidebar 的删除框用了 `bg-success-soft` 的
   * 绿色垃圾桶 —— 删除是 error 语义，绿色会让用户以为"这是好事"）。
   */
  icon?: ReactNode;
  /**
   * 是否在正文下补一行「此操作不可恢复」。
   * 默认：danger 变体为 true。
   *
   * **调用方不要自己在 body 里再拼这句话** —— 此前 Sidebar 两头都写了，
   * 弹窗里同一句话出现两遍（正文尾句 + 尾注），是最典型的"组件没管住、
   * 调用方各写一遍"的结果。
   */
  irreversible?: boolean;
  confirmLabel?: string;
  cancelLabel?: string;
  /** 危险操作（删除、撤销、解绑）用 danger —— 默认就是 danger，
   *  因为确认弹窗绝大多数场景确实是危险操作。 */
  confirmVariant?: "primary" | "danger";
  /** 异步确认进行中：双按钮禁用，确认键显示「处理中…」。
   *  （R2-2：从 ToolTabs 私有 ConfirmDialog 收编的能力） */
  busy?: boolean;
  /** 可见遮罩（默认 false，与 ui/Modal 一致）。 */
  scrim?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

/**
 * 确认弹窗。层次靠**三层信息密度**而不是线：
 *
 *   图标(40) + 标题(title/600)      ← 第一层：这是什么
 *            描述(ui/secondary)     ← 第二层：会发生什么
 *            ⚠ 不可恢复(tiny/error) ← 第三层：最坏后果，胶囊底色
 *   ── bg-secondary 底栏 ──        ← 面亮度差分区，取消 | 确认
 *
 * 2026-10-23 重做：此前 ConfirmDialog 直接把 title 交给 Modal 的头部，
 * body 丢进内容区，两者字号只差一档、间距也只有 4px，读起来是一坨 ——
 * 用户反馈「这个卡片一点层次没有」。现在标题/描述/后果三段各有明确
 * 字号、颜色与间距，且 danger 变体自动补「不可恢复」尾注。
 */
export function ConfirmDialog({
  open,
  title,
  body,
  icon,
  irreversible,
  confirmLabel = CONFIRM,
  cancelLabel = CANCEL,
  confirmVariant = "danger",
  busy = false,
  scrim = false,
  onConfirm,
  onCancel,
}: ConfirmDialogProps) {
  const danger = confirmVariant === "danger";
  const showIrreversible = irreversible ?? danger;

  return (
    <Modal
      open={open}
      onClose={onCancel}
      width="base"
      scrim={scrim}
      ariaLabel={typeof title === "string" ? title : "确认操作"}
      bodyClassName="p-0 overflow-hidden"
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
      <div className="flex items-start gap-3.5 px-5 pt-5 pb-4">
        {icon && (
          <span
            aria-hidden
            className={cx(
              "grid size-9 shrink-0 place-items-center rounded-[var(--radius-md)]",
              danger
                ? "bg-[var(--color-bg-error-soft)] text-[var(--color-text-error)]"
                : "bg-[var(--color-bg-accent-soft)] text-[var(--color-text-accent)]",
            )}
          >
            {icon}
          </span>
        )}
        <div className="min-w-0 flex-1">
          <h2 className="text-[length:var(--text-title)] font-semibold leading-snug text-[var(--color-text-primary)]">
            {title}
          </h2>
          <div className="mt-1.5 text-[length:var(--text-ui)] leading-6 text-[var(--color-text-secondary)]">
            {typeof body === "string" ? <p className="whitespace-pre-wrap">{body}</p> : body}
          </div>
          {showIrreversible && (
            <p className="mt-3 inline-flex items-center gap-1.5 rounded-[var(--radius-sm)] bg-[var(--color-bg-error-soft)] px-2 py-1 text-[length:var(--text-tiny)] font-medium text-[var(--color-text-error)]">
              <AlertTriangle className="icon-xs" aria-hidden />
              此操作不可恢复
            </p>
          )}
        </div>
      </div>
    </Modal>
  );
}
