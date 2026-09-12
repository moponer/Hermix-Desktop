import { getCurrentWindow } from "@tauri-apps/api/window";
import { Menu, Minus, Square, X } from "lucide-react";
import { useCallback } from "react";

interface TitleBarProps {
  onToggleSidebar: () => void;
}

export function TitleBar({ onToggleSidebar }: TitleBarProps) {
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

      {/* 左侧按钮组: 折叠菜单. `no-drag` 包一层即可. */}
      <div className="titlebar-group no-drag">
        <button
          type="button"
          onClick={onToggleSidebar}
          className="icon-btn"
          aria-label="切换侧边栏"
          title="切换侧边栏"
        >
          <Menu className="icon-lg" />
        </button>
      </div>

      {/* Window controls: minimize / maximize / close.
          Each button carries both `no-drag` and `data-tauri-no-drag`:
          `-webkit-app-region: no-drag` does not inherit, so a parent
          drag region alone would swallow the child button's clicks.
          `data-tauri-no-drag` is Tauri's recommended marker and wins
          regardless of CSS specificity / import order. */}
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
