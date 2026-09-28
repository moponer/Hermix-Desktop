// components/PreviewBar.tsx
//
// 右侧栏 = 预览栏：一屏可以开多个文件，用 tab 切换，每个 tab 手动关闭。
//
// 为什么替掉原来的「电脑 / 产物」两栏：
// - 产物栏只是把对话里已经出现过的路径再列一遍，用户真正想做的是**点开看**，
//   而列表本身不提供任何新信息；路径点击入口已经在对话正文里。
// - 电脑栏的价值在「浏览工作区 + 加到对话」，它不该占一整个常驻 tab ——
//   降级为预览栏里的一个入口按钮，用完即收。
//
// 预览渲染只有一处：PreviewPanel（内部再走 PreviewSurface 的单一 switch），
// 这里不做第二套渲染。全屏由 PreviewPanel 自己提供。

import { useState } from "react";
import { X, FolderOpen } from "lucide-react";
import { clsx } from "clsx";
import type { Artifact } from "../utils/artifacts";
import { ComputerTab } from "./ComputerTab";
import { PreviewPanel } from "./PreviewPanel";
import { FileTypeIcon } from "./FileTypeIcon";

export interface PreviewTab {
  id: string;
  artifact: Artifact;
}

export function PreviewBar({
  tabs,
  activeId,
  onActivate,
  onClose,
  onCloseBar,
  workspaceRoot,
  onAddFileToChat,
  onPreviewFile,
}: {
  tabs: PreviewTab[];
  activeId: string | null;
  onActivate: (id: string) => void;
  onClose: (id: string) => void;
  onCloseBar: () => void;
  workspaceRoot?: string | null;
  onAddFileToChat?: (path: string) => void;
  onPreviewFile: (path: string, name: string) => void;
}) {
  const [browserOpen, setBrowserOpen] = useState(false);
  const active = tabs.find((t) => t.id === activeId) ?? null;

  return (
    <aside
      className="flex h-full flex-col flex-1 overflow-hidden"
      style={{ background: "var(--color-bg-primary)" }}
    >
      <div className="preview-bar__strip" role="tablist" aria-label="预览标签">
        <button
          type="button"
          className={clsx("preview-bar__icon", browserOpen && "is-active")}
          onClick={() => setBrowserOpen((o) => !o)}
          title="浏览工作区文件（预览 / 添加到对话）"
          aria-label="浏览工作区文件"
          aria-pressed={browserOpen}
        >
          <FolderOpen className="icon-md" />
        </button>

        <div className="preview-bar__tabs">
          {tabs.map((t) => (
            <div
              key={t.id}
              role="tab"
              aria-selected={t.id === activeId && !browserOpen}
              className={clsx("preview-tab", t.id === activeId && !browserOpen && "is-active")}
              onClick={() => {
                setBrowserOpen(false);
                onActivate(t.id);
              }}
              title={t.artifact.path}
            >
              <FileTypeIcon name={t.artifact.name} size={12} />
              <span className="preview-tab__name">{t.artifact.name}</span>
              <button
                type="button"
                className="preview-tab__close"
                onClick={(e) => {
                  e.stopPropagation();
                  onClose(t.id);
                }}
                aria-label={`关闭预览：${t.artifact.name}`}
                title="关闭这个预览"
              >
                <X className="icon-sm" />
              </button>
            </div>
          ))}
        </div>

        <button
          type="button"
          className="preview-bar__icon ml-auto"
          onClick={onCloseBar}
          title="收起预览栏"
          aria-label="收起预览栏"
        >
          <X className="icon-md" />
        </button>
      </div>

      {browserOpen ? (
        <div className="min-h-0 flex-1 flex flex-col">
          <ComputerTab
            workspaceRoot={workspaceRoot}
            onAddToChat={onAddFileToChat}
            onPreviewFile={(p, n) => {
              onPreviewFile(p, n);
              setBrowserOpen(false);
            }}
          />
        </div>
      ) : active ? (
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
          <p>还没有预览</p>
          <p className="preview-bar__hint">
            点对话里的文件路径、或从左上角文件夹浏览，都会在这里开一个预览标签。
          </p>
        </div>
      )}
    </aside>
  );
}
