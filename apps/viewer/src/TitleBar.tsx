// 自绘窗口标题栏。
//
// 为什么不用系统标题栏：原生标题栏是 Windows 那套深色/浅色固定配色，
// 与项目自己的浅色、深色两套主题都对不上（用户明确要求隐藏它）。代价
// 是窗口的移动 / 最小化 / 最大化 / 关闭都要自己接。
//
// 拖拽走 `startDragging()`（在 mousedown 里调），不用 data-tauri-drag-region：
// 后者靠 closest() 判定，标题栏里的按钮会被一起拖走，点「关闭」变成拖窗口。
//
// 边缘缩放：tauri.conf.json 里 decorations=false 会连 WS_THICKFRAME 一起去掉
// （窗口就不能拖边缩放了），Rust 侧 setup 时把它加回来了（见 lib.rs）。

import { useEffect, useState } from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Copy, Eye, Minus, Square, X } from "lucide-react";

export interface TitleBarProps {
  /** 主标题（应用名）。 */
  title: string;
  /** 副标题：当前文件或目录名。 */
  subject?: string;
  /** 副标题的完整值（悬停提示）。 */
  subjectTitle?: string;
}

export function TitleBar({ title, subject, subjectTitle }: TitleBarProps) {
  const [maximized, setMaximized] = useState(false);
  const win = getCurrentWindow();

  useEffect(() => {
    let alive = true;
    let timer: number | undefined;
    const sync = () => {
      void win.isMaximized().then((v) => {
        if (alive) setMaximized(v);
      });
    };
    sync();
    let unlisten: UnlistenFn | undefined;
    void win.onResized(() => {
      // 拖边缩放会连着来一串 resize 事件：抖 120ms 再问一次状态，
      // 免得每像素都发一次 IPC。
      if (timer) window.clearTimeout(timer);
      timer = window.setTimeout(sync, 120);
    }).then((u) => {
      unlisten = u;
    });
    return () => {
      alive = false;
      if (timer) window.clearTimeout(timer);
      unlisten?.();
    };
  }, [win]);

  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    if ((e.target as HTMLElement).closest("button")) return;
    // 拖动时别选中标题文字。
    e.preventDefault();
    // 失败要静默：拖不动窗口不该把错误甩到界面上（权限缺失之类的
    // 是部署问题，不是用户操作问题）。
    void win.startDragging().catch(() => undefined);
  };

  const onDoubleClick = (e: React.MouseEvent<HTMLDivElement>) => {
    if ((e.target as HTMLElement).closest("button")) return;
    void win.toggleMaximize();
  };

  return (
    <div className="titlebar" onPointerDown={onPointerDown} onDoubleClick={onDoubleClick}>
      <div className="tb-brand">
        <Eye className="tb-mark" aria-hidden="true" />
        <span className="tb-title">{title}</span>
        {subject && (
          <span className="tb-subject" title={subjectTitle ?? subject}>
            {subject}
          </span>
        )}
      </div>
      <div className="tb-window">
        <button
          type="button"
          className="wb-btn"
          onClick={() => void win.minimize()}
          aria-label="最小化"
          title="最小化"
        >
          <Minus size={14} />
        </button>
        <button
          type="button"
          className="wb-btn"
          onClick={() => void win.toggleMaximize()}
          aria-label={maximized ? "还原窗口" : "最大化窗口"}
          title={maximized ? "还原" : "最大化"}
        >
          {maximized ? <Copy size={12} /> : <Square size={12} />}
        </button>
        <button
          type="button"
          className="wb-btn is-close"
          onClick={() => void win.close()}
          aria-label="关闭窗口"
          title="关闭"
        >
          <X size={14} />
        </button>
      </div>
    </div>
  );
}
