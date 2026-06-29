import { getCurrentWindow } from "@tauri-apps/api/window";
import { Menu, Minus, Square, X } from "lucide-react";
import { useCallback } from "react";
import { ThemeToggle } from "./ThemeToggle";

interface TitleBarProps {
  onToggleSidebar: () => void;
}

/* v25 fix: 之前 `const appWindow = getCurrentWindow()` 在模块顶层调用.
   如果某个 import 链路导致 Tauri IPC 还没 ready（比如 HMR 时序、热
   重载组件上下文重建、vite dev 模式下 webview 还没 attach），getCurrentWindow
   会抛或返回未初始化 proxy，导致点击 → IPC 失败 → 三个按钮静默失灵。
   现在改为：组件 mount 时 lazy 获取 + 在 handler 里直接 invoke,避开
   任何初始化时序问题. */
export function TitleBar({ onToggleSidebar }: TitleBarProps) {
  // Lazily resolve the window on each interaction so we never hold a
  // stale proxy (and so the call never happens before Tauri IPC is up).
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
      console.error("[TitleBar] toggleMaximize failed:", err);
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

      {/* 左侧按钮组: 折叠菜单 + 主题切换. `no-drag` 包一层即可. */}
      <div className="titlebar-group no-drag">
        <button
          type="button"
          onClick={onToggleSidebar}
          className="icon-btn"
          aria-label="切换侧边栏"
          title="切换侧边栏"
        >
          <Menu size={18} />
        </button>
        <ThemeToggle compact />
      </div>

      {/* 窗口控制按钮: 最小化 / 最大化 / 关闭.
         关键修复 (v25):
         1. 每个 button 自身加 `no-drag` class + `data-tauri-no-drag` 属性.
            Chromium 的 `-webkit-app-region: no-drag` **不继承**, 只作用
            在声明它的元素本身. 父 div 有 no-drag 是不够的, 子 button
            收不到 click 事件 — 点击被 drag 区域吞掉, 按钮看起来"没反应".
         2. 改用 data-tauri-no-drag 属性, 而不是只靠 className.
            data-tauri-no-drag 是 Tauri 官方推荐的写法, 优先级最高,
            不受 CSS specificity / import 顺序影响. */}
      <div className="flex">
        <button
          type="button"
          onClick={handleMinimize}
          className="titlebar-btn no-drag"
          data-tauri-no-drag
          title="最小化"
          aria-label="最小化"
        >
          <Minus size={16} />
        </button>
        <button
          type="button"
          onClick={handleMaximize}
          className="titlebar-btn no-drag"
          data-tauri-no-drag
          title="最大化"
          aria-label="最大化"
        >
          <Square size={14} />
        </button>
        <button
          type="button"
          onClick={handleClose}
          className="titlebar-btn titlebar-btn-danger no-drag"
          data-tauri-no-drag
          title="关闭"
          aria-label="关闭"
        >
          <X size={18} />
        </button>
      </div>
    </div>
  );
}
