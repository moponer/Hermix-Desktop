// 标题栏里的菜单栏（文件 / 视图 / 工具 / 帮助）。
//
// 交互照 Windows 菜单的既有习惯，不自创：
// - 点一下展开；展开状态下鼠标移到别的菜单直接切换（不用再点）；
// - 点外面 / Esc / 选中一项 → 收起；
// - 选中项用 ✓ 表示"当前生效"（显示隐藏文件、主题、AI 栏这类开关）；
// - 方向键在菜单项之间走，Enter 执行（键盘用户不该被排除在外）。
//
// 弹出层方向：靠窗口右缘的菜单（工具 / 帮助）如果仍按 `left:0` 展开，
// 会有一半跑到窗口外面——实测"菜单太长看不到右边"。所以打开后先量一下
// 宽度，越界就改成右对齐（与 ContextMenu 的翻转同思路）。

import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { MenuRow } from "./MenuRow";

export interface MenuItem {
  id: string;
  label: string;
  /** 行首图标（与文字同为 14px，保持视觉重量一致）。 */
  icon?: ReactNode;
  /** 右侧快捷键提示（纯展示，键位绑定在 App 里）。 */
  shortcut?: string;
  disabled?: boolean;
  checked?: boolean;
  /** 危险操作（清空、退出）用红色。 */
  danger?: boolean;
}

/** 一项或一条分隔线（`"-"`）。 */
export type MenuEntry = MenuItem | "-";

export interface MenuGroup {
  id: string;
  label: string;
  items: MenuEntry[];
}

export interface MenuBarProps {
  menus: MenuGroup[];
  onCommand: (id: string) => void;
  /**
   * 展开 / 收起时通知外部。
   *
   * 用途：网页是**独立的子 webview（独立 HWND）**，永远盖在 DOM 之上 ——
   * 菜单展开到内容区时会被它整个挡住（z-index 管不到，不是同一层）。
   * 外部据此把网页藏起来。
   */
  onOpenChange?: (open: boolean) => void;
}

export function MenuBar({ menus, onCommand, onOpenChange }: MenuBarProps) {
  const [open, setOpen] = useState<string | null>(null);
  const [flip, setFlip] = useState(false);
  const [activeIndex, setActiveIndex] = useState(-1);
  const rootRef = useRef<HTMLDivElement>(null);
  const popRef = useRef<HTMLDivElement>(null);

  // 展开后测量：右边界越界就翻到右对齐。窗口大小变了也要重测。
  useLayoutEffect(() => {
    if (!open || !popRef.current) {
      setFlip(false);
      return;
    }
    const measure = () => {
      const rect = popRef.current?.getBoundingClientRect();
      if (!rect) return;
      setFlip(rect.right > window.innerWidth - 6);
    };
    measure();
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  }, [open]);

  useEffect(() => {
    if (!open) {
      setActiveIndex(-1);
      return;
    }
    const onPointerDown = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(null);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setOpen(null);
        return;
      }
      if (!popRef.current) return;
      const buttons = Array.from(
        popRef.current.querySelectorAll<HTMLButtonElement>("button.menu-item:not(:disabled)"),
      );
      if (buttons.length === 0) return;
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        const step = event.key === "ArrowDown" ? 1 : -1;
        const next = (activeIndex + step + buttons.length) % buttons.length;
        setActiveIndex(next);
        buttons[next]?.focus();
      }
    };
    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("keydown", onKeyDown, true);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      document.removeEventListener("keydown", onKeyDown, true);
    };
  }, [open, activeIndex]);

  // 展开状态变化时通知外部（网页子 webview 要让位）。
  useEffect(() => {
    onOpenChange?.(open !== null);
  }, [open, onOpenChange]);

  const run = useCallback(
    (item: MenuItem) => {
      if (item.disabled) return;
      setOpen(null);
      onCommand(item.id);
    },
    [onCommand],
  );

  return (
    <div className="menubar" role="menubar" ref={rootRef}>
      {menus.map((menu) => {
        const expanded = open === menu.id;
        return (
          /* data-menu 供 CSS 按菜单给图标上色（见 styles.css 的
             .menubar-slot[data-menu=…] 规则）—— 菜单项图标此前全是灰的，
             在展开的那一列里完全不参与辨识。 */
          <div className="menubar-slot" key={menu.id} data-menu={menu.id}>
            <button
              type="button"
              role="menuitem"
              aria-haspopup="menu"
              aria-expanded={expanded}
              className={`menubar-btn${expanded ? " is-open" : ""}`}
              onClick={() => setOpen(expanded ? null : menu.id)}
              onMouseEnter={() => {
                // 已有菜单展开时才跟随鼠标切换，否则会"划过就弹"。
                if (open) setOpen(menu.id);
              }}
            >
              {menu.label}
            </button>
            {expanded && (
              <div
                className={`menubar-pop${flip ? " is-flipped" : ""}`}
                role="menu"
                aria-label={menu.label}
                ref={popRef}
              >
                {menu.items.map((entry, index) =>
                  entry === "-" ? (
                    <div className="menu-sep" key={`sep-${index}`} role="separator" />
                  ) : (
                    <button
                      type="button"
                      key={entry.id}
                      role="menuitemcheckbox"
                      aria-checked={entry.checked ? "true" : "false"}
                      className={`menu-item${entry.danger ? " is-danger" : ""}`}
                      disabled={entry.disabled}
                      onFocus={() => {
                        const buttons = Array.from(
                          popRef.current?.querySelectorAll<HTMLButtonElement>(
                            "button.menu-item:not(:disabled)",
                          ) ?? [],
                        );
                        setActiveIndex(buttons.indexOf(document.activeElement as HTMLButtonElement));
                      }}
                      onClick={() => run(entry)}
                    >
                      <MenuRow
                        label={entry.label}
                        icon={entry.icon}
                        shortcut={entry.shortcut}
                        checked={entry.checked}
                        reserveCheckSlot
                      />
                    </button>
                  ),
                )}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
