// components/RightRail.tsx
//
// 侧栏开关 —— 锚在窗口右上角，**位置恒定**。
//
// 为什么它不能住在右栏那一格里：那一格是 grid 第三列，收起 44px /
// 展开 640px。按钮一旦是它的子元素，位置必然随列宽漂移 —— 这正是
// "点一下图标飞过去"的来源。把它移出栅格、绝对定位到 `.app-body`
// （宽度恒等于窗口）的右上角后，栅格怎么伸缩都与它无关：变的只是
// 它下方的区域，图标自己一像素不动。用户感知到的是"面板从图标下方
// 长出来"，而不是"图标被拽走"。这是参考项目同款手感的关键。
//
// 收起态不留 44px 竖条：开关已常驻可见，再留竖条是白占 3.4% 窗口宽。
// 竖条原本承担的"有 N 个没看"提示，移到开关的角标上。
//
// 与「关闭」的区别：窄视口（< 900px）下开关一并撤掉 —— 那时 44px
// 不如留给正文。

import { PanelRight } from "lucide-react";
import { clsx } from "clsx";

export function RightRail({
  count,
  open,
  onToggle,
}: {
  /** 已打开的标签数，> 0 时显示角标。 */
  count: number;
  /** 侧栏当前是否展开 —— 展开时按钮带激活底色（对齐参考项目的浅色圆角块）。 */
  open: boolean;
  /** 点击 → 切换侧栏（走 App 的 handleToggleRight，含窄视口自动扩窗）。 */
  onToggle: () => void;
}) {
  return (
    <div className="right-rail" role="complementary" aria-label="侧栏开关">
      <button
        type="button"
        className={clsx("right-rail__btn", open && "is-active")}
        onClick={onToggle}
        title={open ? "收起侧栏" : count > 0 ? `展开侧栏（${count} 个标签）` : "展开侧栏"}
        aria-label={open ? "收起侧栏" : count > 0 ? `展开侧栏，${count} 个标签` : "展开侧栏"}
        aria-expanded={open}
        aria-pressed={open}
      >
        <PanelRight className="icon-md" />
        {count > 0 && (
          <span className="right-rail__badge" aria-hidden="true">
            {count > 99 ? "99+" : count}
          </span>
        )}
      </button>
    </div>
  );
}
