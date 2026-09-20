import { getCurrentWindow } from "@tauri-apps/api/window";
import { Minus, Square, X } from "lucide-react";
import { useCallback } from "react";

/** 标题栏只承担「窗口」层级的动作：拖拽 + 最小化 / 最大化 / 关闭。
 *  侧栏折叠与搜索是「内容」层级，已移入侧栏头部（Sidebar 的 .sidebar-head）——
 *  它们控制的区域就在那一行下面，放这里比隔着整个屏幕更贴切。 */
export function TitleBar() {
  const handleMinimize = useCallback(() => {
    try {
      void getCurrentWindow().minimize();
    } catch (err) {
      console.error("[TitleBar] minimize failed:", err);
    }
  }, []);

  const handleMaximize = useCallback(() => {
    try {
      void getCurrentWindow().toggleMaximize();
    } catch (err) {
      console.error("[TitleBar] maximize failed:", err);
    }
  }, []);

  const handleClose = useCallback(() => {
    try {
      void getCurrentWindow().close();
    } catch (err) {
      console.error("[TitleBar] close failed:", err);
    }
  }, []);

  return (
    <div className="titlebar drag">
      <div className="titlebar-brand no-drag">
        <img
          src="/mascot-avatar-bg-round-24.png"
          alt=""
          width={22}
          height={22}
          draggable={false}
          className="titlebar-logo"
        />
        <span className="titlebar-title">Hermix</span>
      </div>

      {/* 空白间隔区 — `no-drag` 让点击穿透到下层按钮 */}
      <div className="no-drag flex-1" />

      {/* Window controls: minimize / maximize / close.
          Each button carries both `no-drag` and `data-tauri-no-drag`:
          the class covers the CSS path, while `data-tauri-no-drag` is
          Tauri's own marker and wins regardless of CSS specificity or
          import order — belt and braces, since a swallowed click here
          would leave the window undraggable-but-unclosable. */}
      <div className="flex">
        <button
          type="button"
          onClick={handleMinimize}
          className="titlebar-btn no-drag"
          data-tauri-no-drag
          title="最小化"
          aria-label="最小化"
        >
          <Minus className="icon-lg" />
        </button>
        <button
          type="button"
          onClick={handleMaximize}
          className="titlebar-btn no-drag"
          data-tauri-no-drag
          title="最大化"
          aria-label="最大化"
        >
          <Square className="icon-md" />
        </button>
        <button
          type="button"
          onClick={handleClose}
          className="titlebar-btn titlebar-btn-danger no-drag"
          data-tauri-no-drag
          title="关闭"
          aria-label="关闭"
        >
          <X className="icon-lg" />
        </button>
      </div>
    </div>
  );
}
