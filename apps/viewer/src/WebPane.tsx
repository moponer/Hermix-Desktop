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

export function WebPane({ url, chrome = true }: { url: string; chrome?: boolean }) {
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

  // 地址栏编辑态：null = 展示 current；非 null = 用户正在输入。
  // 2026-10-22：旧版是只读 span——用户输入网址后没有任何方式确认进入
  //（没有回车处理，也没有 Go 按钮）。
  const [draft, setDraft] = useState<string | null>(null);

  /** 地址栏导航：补全协议后用 location.href 跳转——**不重建 webview**，
   *  会话、登录态、滚动位置全部保留（web_open 会整个重开）。 */
  const go = useCallback(
    (raw: string) => {
      const t = raw.trim();
      if (!t) return;
      const target = /^https?:\/\//i.test(t) ? t : `https://${t}`;
      setDraft(null);
      run(`location.href = ${JSON.stringify(target)};`);
    },
    [run],
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
          <input
            className="web-pane-url web-pane-url-input"
            value={draft ?? current}
            onChange={(e) => setDraft(e.target.value)}
            onFocus={() => setDraft(current)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                go(draft ?? current);
                (e.target as HTMLInputElement).blur();
              } else if (e.key === "Escape") {
                setDraft(null);
                (e.target as HTMLInputElement).blur();
              }
            }}
            title="输入网址后回车进入"
            aria-label="网址"
            spellCheck={false}
          />
          <button
            type="button"
            className="tb-btn tb-icon"
            onClick={() => go(draft ?? current)}
            disabled={!draft || !draft.trim()}
            title="进入网页（回车）"
            aria-label="进入网页"
          >
            <ArrowRight size={14} className="icon-sm" />
          </button>
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
        {/* 占位：子 webview 正好盖在这个矩形上。 */}
        <div className="web-pane-slot" ref={hostRef} />
        {error && <div className="web-pane-error">{error}</div>}
      </div>
    </div>
  );
}
