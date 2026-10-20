// 内嵌网页视图（通道 A）：远程网页渲染在**主窗口内部**。
//
// 实现要点：网页不是画在 DOM 里的，而是主窗口里的一个**子 webview**
// （独立 HWND）。所以这里的 div 只是一个「占位」——它的物理像素矩形
// 会被同步给 Rust，子 webview 正好盖在这个矩形上。
//
// 为什么要占位：子 webview 永远盖在 DOM 之上，不给它留位置，它就会浮在
// 应用界面上（挡住工具条、AI 栏）。
//
// 坐标是**物理像素**：子 webview 是独立 HWND，CSS 像素定位会随 DPI 偏。

import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ArrowLeft, ArrowRight, ExternalLink, RefreshCw } from "lucide-react";

/** 把占位元素的矩形换算成物理像素。 */
function physicalRect(el: HTMLElement): { x: number; y: number; w: number; h: number } {
  const rect = el.getBoundingClientRect();
  const dpr = window.devicePixelRatio || 1;
  return {
    x: Math.round(rect.left * dpr),
    y: Math.round(rect.top * dpr),
    w: Math.round(rect.width * dpr),
    h: Math.round(rect.height * dpr),
  };
}

export function WebPane({ url }: { url: string }) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const [current, setCurrent] = useState(url);
  const [error, setError] = useState<string | null>(null);

  // 页面自己跳了别的地址 → 地址显示跟着变（Rust 侧 on_page_load 推的事件）。
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<string>("web-navigated", (event) => {
      setCurrent(event.payload);
    }).then((fn) => {
      unlisten = fn;
    });
    return () => unlisten?.();
  }, []);

  // 打开 / 切换网址：关掉旧的，开新的。
  useEffect(() => {
    let cancelled = false;
    const raf = requestAnimationFrame(() => {
      const el = hostRef.current;
      if (!el || cancelled) return;
      const rect = physicalRect(el);
      if (rect.w < 4 || rect.h < 4) return;
      setError(null);
      void invoke("web_open", { url, ...rect })
        .then(() => {
          // 卸载发生在打开过程中 → 补一次关闭，否则网页会浮在应用上。
          if (cancelled) void invoke("web_close").catch(() => undefined);
        })
        .catch((e: unknown) => {
          if (!cancelled) setError(typeof e === "string" ? e : String(e));
        });
    });
    return () => {
      cancelled = true;
      cancelAnimationFrame(raf);
      void invoke("web_close").catch(() => undefined);
    };
  }, [url]);

  // 位置 / 尺寸同步：窗口缩放、面板开合、预览区重排都会让占位矩形变。
  useEffect(() => {
    const el = hostRef.current;
    if (!el) return;
    const sync = () => {
      const rect = physicalRect(el);
      if (rect.w < 4 || rect.h < 4) return;
      void invoke("web_bounds", rect).catch(() => undefined);
    };
    const observer = new ResizeObserver(sync);
    observer.observe(el);
    window.addEventListener("resize", sync);
    const timer = window.setTimeout(sync, 150);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", sync);
      window.clearTimeout(timer);
    };
  }, []);

  const run = useCallback((script: string) => {
    void invoke("web_exec", { script }).catch(() => undefined);
  }, []);

  const openExternally = useCallback(() => {
    void invoke("viewer_open_with_system", { path: current }).catch(() => undefined);
  }, [current]);

  return (
    <div className="preview-pane web-pane">
      <div className="preview-toolbar">
        <button
          type="button"
          className="tb-btn tb-icon"
          onClick={() => run("history.back();")}
          title="后退"
          aria-label="后退"
        >
          <ArrowLeft size={14} className="icon-sm" />
        </button>
        <button
          type="button"
          className="tb-btn tb-icon"
          onClick={() => run("history.forward();")}
          title="前进"
          aria-label="前进"
        >
          <ArrowRight size={14} className="icon-sm" />
        </button>
        <button
          type="button"
          className="tb-btn tb-icon"
          onClick={() => run("location.reload();")}
          title="刷新"
          aria-label="刷新网页"
        >
          <RefreshCw size={14} className="icon-sm" />
        </button>
        <span className="web-pane-url" title={current}>
          {current}
        </span>
        <span className="preview-toolbar-spacer" />
        <button
          type="button"
          className="tb-btn"
          onClick={openExternally}
          title="在系统默认浏览器中打开"
          aria-label="在系统默认浏览器中打开"
        >
          <ExternalLink size={14} className="icon-sm" />
          系统浏览器
        </button>
      </div>
      <div className="preview-pane-inner web-pane-host">
        {/* 占位：子 webview 正好盖在这个矩形上。 */}
        <div className="web-pane-slot" ref={hostRef} />
        {error && <div className="web-pane-error">{error}</div>}
      </div>
    </div>
  );
}
