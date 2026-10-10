import { useRef, useLayoutEffect, useEffect, useState } from "react";
import {
  FileText, Eye, Copy, FolderOpen, ExternalLink, AppWindow,
} from "lucide-react";
import type { DirEntry } from "../desktopBridge";

export function FileExplorerContextMenu({
  x,
  y,
  entry,
  onAction,
  onClose,
}: {
  x: number;
  y: number;
  entry: DirEntry;
  onAction: (action: string) => void;
  onClose: () => void;
}) {
  const menuRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: x, top: y });

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
  }, [x, y]);

  useEffect(() => {
    const handleClick = (e: MouseEvent) => {
      const target = e.target as HTMLElement;
      if (!target.closest(".file-explorer-menu")) {
        onClose();
      }
    };
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [onClose]);

  const fileItems = [
    { action: "open", label: "打开", icon: FileText, divider: false },
    { action: "preview", label: "预览", icon: Eye, divider: false },
    { action: "add_to_chat", label: "加入对话", icon: Copy, divider: true },
    { action: "copy_path", label: "复制路径", icon: Copy, divider: false },
    { action: "open_in_explorer", label: "在资源管理器中打开", icon: FolderOpen, divider: true },
    { action: "open_external", label: "用默认程序打开", icon: ExternalLink, divider: false },
    { action: "open_in_viewer", label: "在查看器中打开", icon: AppWindow, divider: false },
  ];

  const dirItems = [
    { action: "open", label: "打开", icon: FolderOpen, divider: false },
    { action: "copy_path", label: "复制路径", icon: Copy, divider: true },
    { action: "open_in_explorer", label: "在资源管理器中打开", icon: FolderOpen, divider: false },
  ];

  const items = entry.isDir ? dirItems : fileItems;

  return (
    <div
      ref={menuRef}
      className="file-explorer-menu fixed z-[70] rounded-lg border border-border-primary shadow-xl py-1 min-w-[180px]"
      style={{
        left: pos.left,
        top: pos.top,
        background: "var(--color-bg-card)",
      }}
    >
      {items.map((item, i) => (
        <div key={item.action}>
          {item.divider && i > 0 && <div className="file-explorer-menu-divider" />}
          <button
            type="button"
            onClick={() => onAction(item.action)}
            className="flex items-center gap-2 w-full px-3 py-1.5 text-xs text-text-primary hover:bg-bg-hover transition-colors text-left"
          >
            <item.icon className="icon-md shrink-0 text-text-muted" />
            {item.label}
          </button>
        </div>
      ))}
    </div>
  );
}
