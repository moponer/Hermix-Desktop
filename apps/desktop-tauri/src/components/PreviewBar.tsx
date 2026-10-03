// components/PreviewBar.tsx
//
// 右侧栏 = **异构标签宿主**：文件预览与工作区浏览是同一种 tab 的两种
// 载荷，可以并存、可以来回切。
//
// 为什么替掉原来的「文件 tab + 一个 browserOpen 开关」：那个开关是
// 组件内 `useState<boolean>`，与文件 tab **互斥** —— 开一个另一个就没了。
// 边栏因此被降级成「单形态切换器」，做不出「同时开着 3 个文件 + 1 个
// 浏览目录」这种真实工作状态。改成 tab 联合类型后，浏览目录就是标签条
// 上的一个普通标签，关掉它不影响任何已开的文件预览。
//
// 渲染只有一处：文件走 PreviewPanel（内部再走 PreviewSurface 的单一
// switch），浏览走 ComputerTab，这里不做第二套渲染。全屏由 PreviewPanel
// 自己提供。
//
// 空态是「这个宿主能开什么」的清单（卡片式入口），不是一句"暂无内容"
// —— 与参考项目同构。**只列真的会开的**：终端（无 PTY）、审查（后端
// file_change 不带 diff 正文）现在都做不出来，列出来就是点击无响应。

import { X, FolderOpen, FileText, MessageSquare, ClipboardCheck } from "lucide-react";
import { clsx } from "clsx";
import type { ReactNode } from "react";
import type { Artifact } from "../utils/artifacts";
import type { ChatMessage, SubAgentState } from "../types";
import { ComputerTab } from "./ComputerTab";
import { PreviewPanel } from "./PreviewPanel";
import { FileTypeIcon } from "./FileTypeIcon";
import { ReviewTab } from "./ReviewTab";
import { SubAgentTab } from "./SubAgentTab";

/** 浏览目录是单例标签：固定 id，重复点只是切回它，不会开第二个。 */
export const BROWSER_TAB_ID = "browser";
/** 审查 / 辅助对话都是单例：整个会话共用一份聚合视图，不需要多开。 */
export const REVIEW_TAB_ID = "review";
export const SUBAGENT_TAB_ID = "subagent";

/**
 * 右侧栏的一个标签。判别联合而不是单一载荷 —— 新增标签种类时只加一个
 * 成员，标签条 / 空态 / 计数都从 `type` 派生，不需要改第二处。
 */
export type SideTab =
  | { id: string; type: "file"; artifact: Artifact }
  | { id: string; type: "browser" }
  | { id: string; type: "review" }
  | { id: string; type: "subagent" };

/** 标签标题。 */
function tabTitle(tab: SideTab): string {
  switch (tab.type) {
    case "browser":
      return "工作区";
    case "review":
      return "审查";
    case "subagent":
      return "辅助对话";
    case "file":
      return tab.artifact.name;
  }
}

/**
 * 标签条上的图标 + 悬浮提示。抽成函数而不是在 JSX 里写三元链 ——
 * 判别联合的收窄只在 `switch` 分支里成立，写成
 * `t.type === "browser" ? A : t.artifact.name` 时 TS 无法把 `t` 收窄到
 * file 成员（review / subagent 分支也会走到 else）。
 */
function tabIcon(tab: SideTab): { icon: ReactNode; hint: string } {
  switch (tab.type) {
    case "browser":
      return { icon: <FolderOpen className="icon-sm shrink-0" />, hint: "浏览工作区文件" };
    case "review":
      return { icon: <ClipboardCheck className="icon-sm shrink-0" />, hint: "本次会话的文件变更" };
    case "subagent":
      return { icon: <MessageSquare className="icon-sm shrink-0" />, hint: "子代理目标与执行过程" };
    case "file":
      return { icon: <FileTypeIcon name={tab.artifact.name} size={12} />, hint: tab.artifact.path };
  }
}

export function PreviewBar({
  tabs,
  activeId,
  onActivate,
  onClose,
  onOpenBrowser,
  onOpenReview,
  onOpenSubagent,
  messages,
  subAgents,
  workspaceRoot,
  onAddFileToChat,
  onPreviewFile,
}: {
  tabs: SideTab[];
  activeId: string | null;
  onActivate: (id: string) => void;
  onClose: (id: string) => void;
  /** 开/切到浏览目录标签（单例，幂等）。 */
  onOpenBrowser: () => void;
  /** 开/切到「审查」标签（单例，幂等）。 */
  onOpenReview: () => void;
  /** 开/切到「辅助对话」标签（单例，幂等）。 */
  onOpenSubagent: () => void;
  /** 审查视图的数据源：当前会话消息（读 execution_timeline 的 file_change）。 */
  messages?: ChatMessage[] | null;
  /** 辅助对话视图的数据源。 */
  subAgents: Record<string, SubAgentState>;
  workspaceRoot?: string | null;
  onAddFileToChat?: (path: string) => void;
  onPreviewFile: (path: string, name: string) => void;
}) {
  const active = tabs.find((t) => t.id === activeId) ?? null;

  return (
    <aside
      className="flex h-full flex-col flex-1 overflow-hidden"
      style={{ background: "var(--color-bg-primary)" }}
    >
      <div className="preview-bar__strip" role="tablist" aria-label="侧栏标签">
        {/* 浏览目录入口：标签条上常驻一个 + 号。已开时点击切回，没开时新开。 */}
        <button
          type="button"
          className="preview-bar__icon"
          onClick={onOpenBrowser}
          title="浏览工作区文件（预览 / 添加到对话）"
          aria-label="浏览工作区文件"
        >
          <FolderOpen className="icon-md" />
        </button>

        <div className="preview-bar__tabs">
          {tabs.map((t) => {
            const { icon, hint } = tabIcon(t);
            return (
            <div
              key={t.id}
              role="tab"
              aria-selected={t.id === activeId}
              className={clsx("preview-tab", t.id === activeId && "is-active")}
              onClick={() => onActivate(t.id)}
              title={hint}
            >
              {icon}
              <span className="preview-tab__name">{tabTitle(t)}</span>
              <button
                type="button"
                className="preview-tab__close"
                onClick={(e) => {
                  e.stopPropagation();
                  onClose(t.id);
                }}
                aria-label={`关闭标签：${tabTitle(t)}`}
                title="关闭这个标签"
              >
                <X className="icon-sm" />
              </button>
            </div>
            );
          })}
        </div>

        {/* 右端不再放"收起侧栏"的 X：那个职责已由绝对定位的开关
            （RightRail，锚在窗口右上角）承担，它必须在标签条**外面**
            才能位置恒定。 */}
      </div>

      {active?.type === "browser" ? (
        <div className="min-h-0 flex-1 flex flex-col">
          <ComputerTab
            workspaceRoot={workspaceRoot}
            onAddToChat={onAddFileToChat}
            onPreviewFile={onPreviewFile}
          />
        </div>
      ) : active?.type === "review" ? (
        <div className="min-h-0 flex-1 flex flex-col">
          <ReviewTab messages={messages} onOpenFile={onPreviewFile} />
        </div>
      ) : active?.type === "subagent" ? (
        <div className="min-h-0 flex-1 flex flex-col">
          <SubAgentTab subAgents={subAgents} />
        </div>
      ) : active?.type === "file" ? (
        <div className="min-h-0 flex-1 flex flex-col">
          {/* onBack = 关闭当前 tab（PreviewPanel 头部那个 X 就是这个语义）。 */}
          <PreviewPanel
            artifact={active.artifact}
            workspaceRoot={workspaceRoot}
            onBack={() => onClose(active.id)}
          />
        </div>
      ) : (
        <div className="preview-bar__empty">
          <p className="preview-bar__empty-title">打开标签页</p>
          <p className="preview-bar__hint">
            点对话里的文件路径就会在这里开一个预览标签，或从下面选一个入口。
          </p>
          <div className="preview-bar__cards">
            <button
              type="button"
              className="preview-bar__card"
              onClick={onOpenBrowser}
            >
              <FolderOpen className="icon-md" />
              <span>浏览工作区</span>
              <span className="preview-bar__card-sub">找文件 · 预览 · 加入对话</span>
            </button>
            <button
              type="button"
              className="preview-bar__card"
              onClick={onOpenReview}
            >
              <ClipboardCheck className="icon-md" />
              <span>审查</span>
              <span className="preview-bar__card-sub">本次会话改过的文件</span>
            </button>
            <button
              type="button"
              className="preview-bar__card"
              onClick={onOpenSubagent}
            >
              <MessageSquare className="icon-md" />
              <span>辅助对话</span>
              <span className="preview-bar__card-sub">子代理目标与执行过程</span>
            </button>
            <button
              type="button"
              className="preview-bar__card is-hint"
              disabled
              title="对话里点任意文件路径即可，无需入口"
            >
              <FileText className="icon-md" />
              <span>对话中的文件</span>
              <span className="preview-bar__card-sub">点正文里的路径直接开</span>
            </button>
          </div>
        </div>
      )}
    </aside>
  );
}
