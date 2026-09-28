// components/RightRail.tsx
//
// 预览栏的「折叠态」—— 44px 竖条，停在右栏原本的位置上。
//
// 为什么不直接关掉：右栏收起后如果什么都不留，"有 3 个产物没看"这件事
// 就随收起动作一起消失了，用户要重新点开才知道。留一条常驻竖条 +
// 数量角标，让这个状态一直可读，点一下原路展开（宽度沿用上次的
// --ab-right-w，不���要额外的位置/尺寸记忆）。
//
// 与「关闭」的区别：窄视口（< 900px）下 CSS 会把这条竖条也藏掉并把
// 栅格列归零 —— 那时 44px 比产物提示更值钱。

import { FolderOpen } from "lucide-react";

export function RightRail({
  count,
  onExpand,
}: {
  /** 已打开的预览数量，> 0 时显示角标。 */
  count: number;
  /** 点击竖条 → 展开右栏（走 App 的 handleToggleRight，含窄视口自动扩窗）。 */
  onExpand: () => void;
}) {
  const hasItems = count > 0;
  return (
    <div className="right-rail" role="complementary" aria-label="预览栏（已折叠）">
      <button
        type="button"
        className="right-rail__btn"
        onClick={onExpand}
        title={hasItems ? `展开预览栏（${count} 个预览）` : "展开预览栏"}
        aria-label={hasItems ? `展开预览栏，${count} 个预览` : "展开预览栏"}
        aria-expanded={false}
      >
        <FolderOpen className="icon-md" />
        {hasItems && (
          <span className="right-rail__badge" aria-hidden="true">
            {count > 99 ? "99+" : count}
          </span>
        )}
      </button>
      <span className="right-rail__label">预览</span>
    </div>
  );
}
