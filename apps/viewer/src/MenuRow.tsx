// 菜单行内容（勾选 / 行首槽 / 文字 / 快捷键）——标题栏菜单与右键菜单
// 共用一处渲染，防两套各写一套再漂移（2026-10-24 前两边的行首结构、
// 图标尺寸已经各走各的）。外层 <button>（角色 / 禁用语义 / 事件）由
// 各菜单自己定，这里只管行内容。
//
// 行首两种形态，对应两种菜单的信息结构：
// - 标题栏菜单有可勾选项（视图：隐藏文件 / 主题…），勾选列**常驻**对齐，
//   图标独立成列，两者可同时出现（"隐藏文件"= 勾选 + 漏斗图标）；
// - 右键菜单（Windows 习惯）：勾选与图标共用一个槽位，二选一。

import type { ReactNode } from "react";
import { Check } from "lucide-react";

export function MenuRow({
  label,
  icon,
  shortcut,
  checked,
  reserveCheckSlot = false,
}: {
  label: string;
  icon?: ReactNode;
  /** 右侧快捷键提示（纯展示，键位绑定在各菜单侧）。 */
  shortcut?: string;
  checked?: boolean;
  /** 勾选列常驻（标题栏菜单）；false 时勾选占用行首槽（右键菜单）。 */
  reserveCheckSlot?: boolean;
}) {
  return (
    <>
      {reserveCheckSlot && (
        <span className="menu-check" aria-hidden="true">
          {checked ? <Check size={12} /> : null}
        </span>
      )}
      <span className="menu-lead" aria-hidden="true">
        {checked && !reserveCheckSlot ? <Check size={12} /> : icon}
      </span>
      <span className="menu-label">{label}</span>
      {shortcut && <span className="menu-kbd">{shortcut}</span>}
    </>
  );
}
