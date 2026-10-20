// 自绘右键菜单。
//
// 为什么不用浏览器默认菜单：WebView2 的原生菜单是 Chromium 的（"重新
// 加载 / 查看源代码 / 检查"），配色与本项目两套主题都不搭，而且项目
// 需要的是**文件操作**菜单（打开方式 / 在资源管理器中显示 / 复制文件…）
// ——原生菜单给不了。App.tsx 在 document 上做 capture 阶段拦截，
// 这里只负责呈现。
//
// 行为约定（与 Windows 资源管理器一致）：
//   - 贴着屏幕边缘时向内收，下方放不下就往上翻；
//   - 点任意空白处关闭（背后有一层透明遮罩吃掉这次点击）；
//   - Esc 关闭；方向键在菜单项间移动，Home/End 跳到首尾；
//   - 禁用项仍然能被键盘走到（aria-disabled），但按下去不执行。

import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { createPortal } from "react-dom";
import { Check } from "lucide-react";

export interface MenuItem {
  id: string;
  label: string;
  icon?: ReactNode;
  /** 快捷键提示（右对齐，等宽小字）。 */
  hint?: string;
  /** 禁用：不执行，但仍可聚焦（无障碍）。 */
  disabled?: boolean;
  /** 危险操作（破坏性）——文字用危险色。 */
  danger?: boolean;
  /** 勾选态（当前排序方式这类"状态"）。 */
  checked?: boolean;
  onSelect: () => void;
}

export interface ContextMenuProps {
  x: number;
  y: number;
  /** 菜单分组：组与组之间画分隔线。 */
  groups: MenuItem[][];
  /**
   * 是否自动聚焦第一项。
   *
   * 键盘唤起菜单（Shift+F10 / 菜单键）时为 true——键盘用户要能直接
   * 用方向键走。鼠标唤起时为 false：否则第一项会顶着焦点环（Chromium
   * 对程序化 focus 也匹配 :focus-visible），鼠标用户看着像"被选中"。
   */
  autoFocusFirst?: boolean;
  onClose: () => void;
}

/** 菜单与视口边缘的最小间距。 */
const MENU_PAD = 8;

export function ContextMenu({ x, y, groups, autoFocusFirst, onClose }: ContextMenuProps) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: x, top: y });

  // 定位：先按点击点摆，再按实际尺寸往回收。翻转逻辑在测量之后做——
  // 菜单高度只有渲染出来才知道，"先翻再量"会翻错。
  //
  // 依赖只放 [x, y]：菜单内容变了不重算位置（重算会在 setPos 里
  // 触发新一轮渲染，而 groups 每次渲染都是新数组——放进依赖就是死循环）。
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    let left = x;
    let top = y;
    if (left + rect.width > vw - MENU_PAD) {
      left = Math.max(MENU_PAD, vw - rect.width - MENU_PAD);
    }
    if (top + rect.height > vh - MENU_PAD) {
      // 下方放不下 → 往上翻（菜单底边贴住点击点）。
      top = y - rect.height;
      if (top < MENU_PAD) {
        // 菜单比屏幕还高：贴顶 + 内部滚动（CSS 里给了 max-height）。
        top = MENU_PAD;
      }
    }
    // 位置没变就返回同一个对象：避免 setState 触发又一轮渲染。
    setPos((p) => (p.left === left && p.top === top ? p : { left, top }));
  }, [x, y]);

  // 打开即聚焦：只有键盘唤起时才聚焦第一项（鼠标唤起不要焦点环）。
  // 焦点落在菜单容器上时，方向键仍然可用（keydown 挂在容器上）。
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    if (autoFocusFirst) {
      el.querySelector<HTMLButtonElement>(".ctxmenu-item")?.focus();
    } else {
      el.focus();
    }
  }, [autoFocusFirst]);

  // 关闭时把焦点还给原来的元素（键盘操作不丢焦点位置）。
  useEffect(() => {
    const prev = document.activeElement as HTMLElement | null;
    return () => {
      if (prev && typeof prev.focus === "function" && document.contains(prev)) prev.focus();
    };
  }, []);

  const onKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      onClose();
      return;
    }
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp" && e.key !== "Home" && e.key !== "End") {
      return;
    }
    e.preventDefault();
    const items = Array.from(
      ref.current?.querySelectorAll<HTMLButtonElement>(".ctxmenu-item") ?? [],
    );
    if (items.length === 0) return;
    const cur = items.indexOf(document.activeElement as HTMLButtonElement);
    let next = 0;
    if (e.key === "ArrowDown") next = cur < 0 ? 0 : (cur + 1) % items.length;
    else if (e.key === "ArrowUp") next = cur < 0 ? items.length - 1 : (cur - 1 + items.length) % items.length;
    else if (e.key === "End") next = items.length - 1;
    items[next]?.focus();
  };

  return createPortal(
    <>
      <div className="ctxmenu-backdrop" onPointerDown={onClose} aria-hidden="true" />
      <div
        ref={ref}
        className="ctxmenu"
        style={{ left: pos.left, top: pos.top }}
        role="menu"
        aria-label="上下文菜单"
        tabIndex={-1}
        onKeyDown={onKeyDown}
        // 菜单上再右键不应该弹出第二层菜单（更不该出现原生菜单）。
        onContextMenu={(e) => {
          e.preventDefault();
          e.stopPropagation();
        }}
      >
        {groups.map((group, gi) => (
          <div className="ctxmenu-group" key={gi} role="group">
            {group.map((item) => (
              <button
                key={item.id}
                type="button"
                role="menuitem"
                className={`ctxmenu-item${item.danger ? " is-danger" : ""}${
                  item.checked ? " is-checked" : ""
                }`}
                aria-disabled={item.disabled || undefined}
                onClick={() => {
                  if (item.disabled) return;
                  // 先关菜单再执行：菜单是"一次性动作"的容器，
                  // 动作多半是异步的（打开程序 / 写剪贴板）。
                  onClose();
                  item.onSelect();
                }}
              >
                <span className="ctxmenu-icon" aria-hidden="true">
                  {item.checked ? <Check className="ctxmenu-check" /> : item.icon}
                </span>
                <span className="ctxmenu-label">{item.label}</span>
                {item.hint && <span className="ctxmenu-hint">{item.hint}</span>}
              </button>
            ))}
          </div>
        ))}
      </div>
    </>,
    document.body,
  );
}
