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

import { useCallback, useEffect, useRef, useState } from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Copy, Eye, Minus, Square, X } from "lucide-react";
import { MenuBar, type MenuGroup } from "./MenuBar";

export interface TitleBarProps {
  /** 主标题（应用名）。 */
  title: string;
  /** 副标题：当前文件或目录名。 */
  subject?: string;
  /** 副标题的完整值（悬停提示）。 */
  subjectTitle?: string;
  /** 菜单栏菜单（文件/视图/工具/帮助）。 */
  menus?: MenuGroup[];
  onMenuCommand?: (id: string) => void;
}

export function TitleBar({ title, subject, subjectTitle, menus, onMenuCommand }: TitleBarProps) {
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

  /**
   * 双击标题栏 = 最大化 / 还原；按住拖动 = 移动窗口。
   *
   * 这两件事**不能同时靠浏览器事件**：startDragging() 走的是系统模态拖拽
   * 循环，一旦启动，后续点击就被它吞掉——dblclick 事件根本不会派发（第一版
   * 就是这么写的，真机双击毫无反应）。
   *
   * 所以改成自己判：按下时先不拖，记下时间与位置；
   *   - 400ms 内的第二次按下 → 双击，直接切换最大化（此时还没起拖拽，
   *     第二下不会被吞）；
   *   - 指针一动（>4px）→ 判定为拖动，立刻起拖（无延迟）；
   *   - 按住不动 → 等满 220ms 再起拖（此时仍不是双击）。
   */
  const dragRef = useRef<{
    x: number;
    y: number;
    downAt: number;
    dragging: boolean;
    timer?: number;
  } | null>(null);

  const startDrag = useCallback(() => {
    const st = dragRef.current;
    if (!st || st.dragging) return;
    st.dragging = true;
    if (st.timer) {
      window.clearTimeout(st.timer);
      st.timer = undefined;
    }
    // 失败要静默：拖不动窗口不该把错误甩到界面上（权限缺失之类的
    // 是部署问题，不是用户操作问题）。
    void win.startDragging().catch(() => undefined);
  }, [win]);

  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    if ((e.target as HTMLElement).closest("button")) return;
    const now = Date.now();
    const st = dragRef.current;
    if (st && now - st.downAt < 400 && st.downAt > 0) {
      // 双击：最大化 / 还原。downAt 清零，第三次点击不再判定为双击。
      st.downAt = 0;
      if (st.timer) {
        window.clearTimeout(st.timer);
        st.timer = undefined;
      }
      void win.toggleMaximize();
      return;
    }
    // 拖动时别选中标题文字。
    e.preventDefault();
    dragRef.current = { x: e.clientX, y: e.clientY, downAt: now, dragging: false };
    dragRef.current.timer = window.setTimeout(startDrag, 220);
  };

  const onPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const st = dragRef.current;
    if (!st || st.dragging) return;
    if (Math.abs(e.clientX - st.x) > 4 || Math.abs(e.clientY - st.y) > 4) startDrag();
  };

  const onPointerUp = () => {
    const st = dragRef.current;
    if (!st) return;
    // 没起拖就松手 = 一次点击（不是拖动）：把待起拖的定时器撤掉。
    // downAt 保留——双击判定看的是「两次按下的间隔」，松手不清。
    if (!st.dragging && st.timer) {
      window.clearTimeout(st.timer);
      st.timer = undefined;
    }
  };

  return (
    <div
      className="titlebar"
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
    >
      <div className="tb-brand">
        <Eye className="tb-mark" aria-hidden="true" />
        <span className="tb-title">{title}</span>
        {subject && (
          <span className="tb-subject" title={subjectTitle ?? subject}>
            {subject}
          </span>
        )}
      </div>
      {menus && menus.length > 0 && (
        <>
          <MenuBar menus={menus} onCommand={onMenuCommand ?? (() => undefined)} />
          <div className="tb-spacer" />
        </>
      )}
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
