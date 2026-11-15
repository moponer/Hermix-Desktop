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
//
// `chrome`：是否自带「后退 / 前进 / 刷新 + 网址 + 系统浏览器」工具条。
// 纯净模式关掉它——那些控件与网址已经在顶栏地址栏里（见 App.tsx 的
// .pure-bar），两处都画等于同一串地址出现两遍。

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

export function WebPane({
  url,
  webId,
  chrome = true,
  veiled = false,
}: {
  url: string;
  /** 本标签在 Rust 侧的子 webview id（label `web-<id>`）：切标签不销毁，
   *  各标签的视图互相独立，保活池满了由 Rust 按 LRU 淘汰。 */
  webId: string;
  chrome?: boolean;
  /** 浮层让位中（网页被 hide）：占位区上一层暗面底色，读作"暂时盖住"而不是空白。 */
  veiled?: boolean;
}) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const [current, setCurrent] = useState(url);
  const [error, setError] = useState<string | null>(null);

  // 页面自己跳了别的地址 → 地址显示跟着变（Rust 侧 on_page_load 推的事件，
  // 载荷带 id，只认自己这个视图的）。
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<string>("web-navigated", (event) => {
      try {
        const p = JSON.parse(event.payload) as { id?: string; url?: string };
        if (p.id === `web-${webId}` && p.url) setCurrent(p.url);
      } catch {
        /* 兼容旧载荷（纯 URL 字符串） */
      }
    }).then((fn) => {
      unlisten = fn;
    });
    return () => unlisten?.();
  }, [webId]);

  // 打开 / 切换网址：同 id 同 URL 幂等（保活的视图直接 show 回来）；
  // 换了 URL 由 Rust 销毁重建这一个视图。
  useEffect(() => {
    let cancelled = false;
    const raf = requestAnimationFrame(() => {
      const el = hostRef.current;
      if (!el || cancelled) return;
      const rect = physicalRect(el);
      if (rect.w < 4 || rect.h < 4) return;
      setError(null);
      void invoke("web_open", { id: webId, url, ...rect })
        .then(() => {
          // 卸载发生在打开过程中 → 补一次收起（保活，标签还在的话切回来即恢复）。
          if (cancelled) void invoke("web_release", { id: webId }).catch(() => undefined);
        })
        .catch((e: unknown) => {
          if (!cancelled) setError(typeof e === "string" ? e : String(e));
        });
    });
    return () => {
      cancelled = true;
      cancelAnimationFrame(raf);
      // 卸载 = 切走这个标签：hide 保活（页面状态、滚动位置、登录态都保留），
      // 标签被真正关闭时的销毁由 App 的标签对账（web_close）负责。
      void invoke("web_release", { id: webId }).catch(() => undefined);
    };
  }, [url, webId]);

  // 位置 / 尺寸同步：窗口缩放、面板开合、预览区重排都会让占位矩形变。
  useEffect(() => {
    const el = hostRef.current;
    if (!el) return;
    const sync = () => {
      const rect = physicalRect(el);
      if (rect.w < 4 || rect.h < 4) return;
      void invoke("web_bounds", { id: webId, ...rect }).catch(() => undefined);
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
  }, [webId]);

  const run = useCallback(
    (script: string) => {
      void invoke("web_exec", { id: webId, script }).catch(() => undefined);
    },
    [webId],
  );

  const openExternally = useCallback(() => {
    void invoke("viewer_open_with_system", { path: current }).catch(() => undefined);
  }, [current]);

  return (
    <div className="preview-pane web-pane">
      {/* 纯净模式把这条工具条交给顶栏（地址栏 + 导航按钮），这里不再画第二份：
          两行都显示同一个网址、两个「用系统打开」，正是用户提的"重复"。 */}
      {chrome && (
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
      )}
      <div className="preview-pane-inner web-pane-host">
        {/* 占位：子 webview 正好盖在这个矩形上。让位期间 webview 被 hide，
            `is-yield` 给占位一层暗面底色，避免露出突兀的空白。 */}
        <div className={`web-pane-slot${veiled ? " is-yield" : ""}`} ref={hostRef} />
        {error && <div className="web-pane-error">{error}</div>}
      </div>
    </div>
  );
}
