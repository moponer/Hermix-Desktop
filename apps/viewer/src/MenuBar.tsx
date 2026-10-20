// 标题栏里的菜单栏（文件 / 视图 / 工具 / 帮助）。
//
// 交互照 Windows 菜单的既有习惯，不自创：
// - 点一下展开；展开状态下鼠标移到别的菜单直接切换（不用再点）；
// - 点外面 / Esc / 选中一项 → 收起；
// - 选中项用 ✓ 表示"当前生效"（显示隐藏文件、主题、AI 栏这类开关）。
//
// 为什么不复用 ContextMenu：那是"在坐标处弹一次"的浮层（fixed + 手动定
// 位），菜单栏要的是"挂在按钮下方、随窗口移动、有 hover 切换"，用同一套
// 逻辑反而要加一堆模式开关。

import { useEffect, useRef, useState } from "react";
import { Check } from "lucide-react";

export interface MenuItem {
  id: string;
  label: string;
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
}

export function MenuBar({ menus, onCommand }: MenuBarProps) {
  const [open, setOpen] = useState<string | null>(null);
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (e: PointerEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(null);
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(null);
    };
    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("keydown", onKeyDown, true);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      document.removeEventListener("keydown", onKeyDown, true);
    };
  }, [open]);

  return (
    <div className="menubar" role="menubar" ref={rootRef}>
      {menus.map((menu) => {
        const expanded = open === menu.id;
        return (
          <div className="menubar-slot" key={menu.id}>
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
              <div className="menubar-pop" role="menu" aria-label={menu.label}>
                {menu.items.map((entry, index) =>
                  entry === "-" ? (
                    <div className="menubar-sep" key={`sep-${index}`} role="separator" />
                  ) : (
                    <button
                      type="button"
                      key={entry.id}
                      role="menuitemcheckbox"
                      aria-checked={entry.checked ? "true" : "false"}
                      className={`menubar-item${entry.danger ? " is-danger" : ""}`}
                      disabled={entry.disabled}
                      onClick={() => {
                        setOpen(null);
                        onCommand(entry.id);
                      }}
                    >
                      <span className="menubar-check" aria-hidden="true">
                        {entry.checked ? <Check size={12} /> : null}
                      </span>
                      <span className="menubar-label">{entry.label}</span>
                      {entry.shortcut && (
                        <span className="menubar-kbd">{entry.shortcut}</span>
                      )}
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
