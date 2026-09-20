import { PanelLeftOpen, Search } from "lucide-react";

/** 侧栏折叠后出现在主区顶部的动作行：展开 + 搜索。
 *
 * 两条使用路径：
 * 1. App 在非对话页渲染一条真实工具行（占位，把页面内容整体推下去）；
 * 2. ChatView 在无标题栏的分支（欢迎页/加载态/错误态）渲染同一行。
 * 对话中的情况不用它 —— 那里按钮直接嵌进 ModelStatusBar 的标题行（同一行），
 * 而不是在标题上方另起一行。
 *
 * 用真实行而不是绝对定位浮层：各页面顶部都有自己的左上内容（对话标题、
 * 「专家」「自动化」等页头），浮层必然压住其中之一。
 */
export function SidebarRevealRow({
  onExpand,
  onOpenSearch,
}: {
  onExpand: () => void;
  onOpenSearch?: () => void;
}) {
  return (
    <div className="sidebar-reveal-row">
      <button
        type="button"
        onClick={onExpand}
        className="icon-btn"
        aria-label="展开侧边栏"
        title="展开侧边栏"
      >
        <PanelLeftOpen className="icon-md" />
      </button>
      {onOpenSearch && (
        <button
          type="button"
          onClick={onOpenSearch}
          className="icon-btn"
          aria-label="搜索对话"
          title="搜索对话"
        >
          <Search className="icon-md" />
        </button>
      )}
    </div>
  );
}
