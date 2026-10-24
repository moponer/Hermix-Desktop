/**
 * 应用内右键菜单 —— 桌面端不再把浏览器的原生菜单端给用户。
 *
 * **为什么不用 Chromium 的原生菜单**：那是"网页菜单"——带着重新加载、
 * 检查、后台打开、书签这些网页专属项。在桌面壳里它们既没意义，又把界面
 * 拉回浏览器观感。桌面应用的右键菜单该是"应用菜单"：剪切 / 复制 / 粘贴 /
 * 全选 / 复制图片 / 复制链接。
 *
 * 视觉与既有浮层同一套语言（浮起的面 + 阴影 + 圆角），**不描边**
 * （用户纪律：UI 一律不用描边，层次靠底色差 + 阴影 + 间距）。
 *
 * 键盘：↑/↓ 在可用项间移动，Enter 执行，Esc 关闭并把焦点还给触发点。
 */
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ComponentType,
} from "react";

export interface ContextMenuItem {
  id: string;
  label: string;
  /** 左侧图标。省略则整项左对齐到文字。 */
  icon?: ComponentType<{ className?: string }>;
  disabled?: boolean;
  /** 在本格**之前**画一条分隔线。 */
  separator?: boolean;
  onSelect?: () => void;
}

export interface ContextMenuProps {
  /** 视口坐标（clientX/clientY）。 */
  x: number;
  y: number;
  items: ContextMenuItem[];
  onClose: () => void;
}

/** 菜单根节点的类名 —— 宿主用它识别"点在菜单里"，避免自己接管自己。 */
export const CONTEXT_MENU_CLASS = "app-context-menu";

/**
 * 组件自己处理了右键菜单（弹了别的菜单）时调它，全局菜单就不接管这次右键。
 *
 * 必须**同步**调用：全局监听在捕获阶段 preventDefault（保证网页菜单
 * 永远不出现），组件自己的处理器在冒泡阶段跑 —— 两者之间没有等待窗口，
 * 所以只能靠这个标记传递"我已经处理了"。
 */
export function markContextMenuHandled(e: { nativeEvent?: Event } | Event): void {
  const target = "nativeEvent" in e ? e.nativeEvent : e;
  if (target) {
    (target as Event & { __hermixContextMenu?: boolean }).__hermixContextMenu = true;
  }
}

/** 事件是否已被某个组件自行处理。 */
export function isContextMenuHandled(e: Event): boolean {
  return (e as Event & { __hermixContextMenu?: boolean }).__hermixContextMenu === true;
}

export function ContextMenu({ x, y, items, onClose }: ContextMenuProps) {
  const menuRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: x, top: y });
  const [activeIndex, setActiveIndex] = useState(-1);

  // 贴边翻转：菜单比视口矮/窄时向内收，不出屏。
  useLayoutEffect(() => {
    const el = menuRef.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    const margin = 8;
    const maxLeft = Math.max(margin, window.innerWidth - rect.width - margin);
    const maxTop = Math.max(margin, window.innerHeight - rect.height - margin);
    setPos({
      left: Math.max(margin, Math.min(x, maxLeft)),
      top: Math.max(margin, Math.min(y, maxTop)),
    });
  }, [x, y, items]);

  const enabledIndexes = useMemo(
    () =>
      items
        .map((it, i) => (it.disabled ? -1 : i))
        .filter((i) => i >= 0),
    [items],
  );

  const focusItem = useCallback(
    (index: number) => {
      const el = menuRef.current?.querySelectorAll<HTMLButtonElement>("[data-menu-item]")[index];
      el?.focus();
    },
    [],
  );

  const move = useCallback(
    (delta: number) => {
      if (enabledIndexes.length === 0) return;
      const current = enabledIndexes.indexOf(activeIndex);
      const next =
        current < 0
          ? delta > 0
            ? enabledIndexes[0]!
            : enabledIndexes[enabledIndexes.length - 1]!
          : enabledIndexes[(current + delta + enabledIndexes.length) % enabledIndexes.length]!;
      setActiveIndex(next);
      focusItem(next);
    },
    [activeIndex, enabledIndexes, focusItem],
  );

  // 关闭条件：点外面 / Esc / 失焦 / 滚动 / 改窗口大小。
  useEffect(() => {
    const onPointerDown = (e: MouseEvent) => {
      if (!menuRef.current?.contains(e.target as Node)) onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      } else if (e.key === "ArrowDown") {
        e.preventDefault();
        move(1);
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        move(-1);
      } else if (e.key === "Enter" && activeIndex >= 0) {
        e.preventDefault();
        items[activeIndex]?.onSelect?.();
      }
    };
    const onBlur = () => onClose();
    const onReflow = () => onClose();
    document.addEventListener("mousedown", onPointerDown, true);
    document.addEventListener("keydown", onKey, true);
    window.addEventListener("blur", onBlur);
    window.addEventListener("resize", onReflow);
    window.addEventListener("scroll", onReflow, true);
    return () => {
      document.removeEventListener("mousedown", onPointerDown, true);
      document.removeEventListener("keydown", onKey, true);
      window.removeEventListener("blur", onBlur);
      window.removeEventListener("resize", onReflow);
      window.removeEventListener("scroll", onReflow, true);
    };
  }, [activeIndex, items, move, onClose]);

  return (
    <div
      ref={menuRef}
      role="menu"
      aria-label="上下文菜单"
      className={`${CONTEXT_MENU_CLASS} fixed z-[90] min-w-[190px] rounded-xl bg-bg-elevated p-1.5 shadow-pop`}
      style={{ left: pos.left, top: pos.top }}
      onContextMenu={(e) => {
        // 菜单自身不再触发全局菜单（否则会自我递归）。
        e.preventDefault();
        markContextMenuHandled(e);
      }}
    >
      {items.map((item, i) => (
        <div key={item.id}>
          {item.separator && i > 0 && (
            <div className="my-1 h-px bg-bg-tertiary" role="separator" />
          )}
          <button
            type="button"
            role="menuitem"
            data-menu-item={i}
            disabled={item.disabled}
            onMouseEnter={() => setActiveIndex(i)}
            onClick={() => {
              item.onSelect?.();
              onClose();
            }}
            className="flex w-full items-center gap-2.5 rounded-lg px-2.5 py-1.5 text-left text-[length:var(--text-ui)] text-text-primary transition-colors hover:bg-bg-hover disabled:pointer-events-none disabled:opacity-40"
          >
            {item.icon && <item.icon className="icon-md shrink-0 text-text-muted" />}
            <span className="truncate">{item.label}</span>
          </button>
        </div>
      ))}
    </div>
  );
}
