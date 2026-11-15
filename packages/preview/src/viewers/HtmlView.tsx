// 网页预览：在沙箱 iframe 里把 HTML 真正渲染成页面。
//
// 之前 .html 根本没有对应的预览种类：`resolveKind` 落到 `detectLanguage`
// 认出 "markup"，于是走代码视图 —— 只能看源码，页面一个字都渲染不出来。
// 现在 html 是一等载荷：排版视图渲染页面，文本视图仍然能看源码。
//
// 沙箱取舍：
//   allow-scripts  开：HTML 常常是带交互的小页面，不开等于渲染了个寂寞。
//                   配合**不开** allow-same-origin，iframe 拿到的是不透明
//                   源，页面脚本够不到我们的应用与存储。
//   不开 allow-forms / allow-popups：预览页不该能提交表单或弹窗。
//   链接点击由注入的桥脚本转交宿主，用系统浏览器打开——沙箱内既打不开新
//   窗口，相对路径也能按文档所在目录补全后交给系统处理。
//
// 已知边界（与沙箱一致的诚实说明，不是 bug）：预览页继承宿主 CSP，所以
// 走 https 的外部样式表/脚本不加载；相对路径图片不解析。需要完整网络
// 环境时用工具条上的「浏览器打开」。
//
// 内联 <style>/<script> 能不能用，取决于宿主 style-src/script-src 里
// 'unsafe-inline' 是不是**唯一**的放行方式：按 CSP3，一旦指令里出现
// nonce-source 或 hash-source，'unsafe-inline' 对行内内容立即失效，而
// srcdoc 子文档继承父策略、拿不到父文档的 nonce → 页面自己的样式表会被
// 整块丢弃（只剩浏览器默认样式，h1 变大、表格无边框，与"样式丢了"一样）。
// Tauri 编译期会主动给 index.html 的内联 <style> 注入 nonce、给内联
// <script> 注入 sha256 并追加到 CSP——于是 desktop-tauri 的
// tauri.conf.json 必须保留 `dangerousDisableAssetCspModification` 里的
// style-src/script-src，否则这条链在打包版里必然复发（dev 走 devUrl，
// codegen 不注入，看不到）。实证与取舍见
// docs/notes/implemented/bug-fix/2026-10-30-html-preview-inline-style-csp.md。

import { useCallback, useEffect, useMemo, useState } from "react";
import { Monitor, Tablet, Smartphone, ExternalLink, Scan } from "lucide-react";
import { dirnameOf, resolveRelative } from "../lib/localAssets";

type Viewport = "fit" | "desktop" | "tablet" | "phone";

const VIEWPORT_WIDTH: Record<Exclude<Viewport, "fit">, number> = {
  desktop: 1280,
  tablet: 834,
  phone: 390,
};

/** 只接管导航，不改动页面其它行为。 */
const BRIDGE_SCRIPT = `<script>(function(){
  document.addEventListener("click", function (e) {
    var t = e.target;
    var a = t && t.closest ? t.closest("a[href]") : null;
    if (!a) return;
    e.preventDefault();
    try { parent.postMessage({ hermixHref: a.href }, "*"); } catch (_) {}
  }, true);
})();<\/script>`;

function injectBridge(html: string): string {
  if (/<\/head>/i.test(html)) return html.replace(/<\/head>/i, `${BRIDGE_SCRIPT}</head>`);
  if (/<html[^>]*>/i.test(html)) return html.replace(/<html[^>]*>/i, (m) => m + BRIDGE_SCRIPT);
  return BRIDGE_SCRIPT + html;
}

/** 只放行系统能打开的协议，javascript: / data: 一律不外传。 */
function safeExternal(href: string): string | null {
  if (/^(https?:|file:)/i.test(href)) return href;
  return null;
}

export function HtmlView({
  content,
  resolvedPath,
  zoom,
  fileName,
  onOpenExternalUrl,
}: {
  content: string;
  resolvedPath: string | null;
  zoom: number;
  fileName?: string;
  /**
   * 外部链接的统一出口 —— desktop 走 Tauri `openExternalUrl`，
   * viewer 走 `openWithSystem` / 浏览器兜底。
   * 不传则用户点击链接时 iframe 内不跳转。
   */
  onOpenExternalUrl?: (target: string) => void;
}) {
  const [viewport, setViewport] = useState<Viewport>("fit");

  const srcDoc = useMemo(() => injectBridge(content), [content]);

  useEffect(() => {
    const onMessage = (e: MessageEvent) => {
      const href = (e.data as { hermixHref?: unknown } | null)?.hermixHref;
      if (typeof href !== "string") return;
      const dir = resolvedPath ? dirnameOf(resolvedPath) : null;
      const absolute = dir ? resolveRelative(dir, href) : href;
      const target = safeExternal(absolute);
      if (target && onOpenExternalUrl) onOpenExternalUrl(target);
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [resolvedPath, onOpenExternalUrl]);

  const openInBrowser = useCallback(() => {
    if (resolvedPath && onOpenExternalUrl) onOpenExternalUrl(resolvedPath);
  }, [resolvedPath, onOpenExternalUrl]);

  const shellStyle = useMemo(
    () => ({ width: viewport === "fit" ? "100%" : VIEWPORT_WIDTH[viewport], zoom: zoom / 100 }),
    [viewport, zoom],
  );

  return (
    <div className="html-root">
      <div className="preview-pdf-bar">
        <div className="html-viewports" role="group" aria-label="预览视口宽度">
          {/* 适应宽度与桌面都用 Monitor 时两个按钮长得一样，用户点了分不出
              变化（且下方 max-width 曾把固定宽度压回 100%）。改用 Scan 区分。 */}
          <ViewportButton icon={<Scan className="icon-md" />} label="适应面板宽度" active={viewport === "fit"} onClick={() => setViewport("fit")} />
          <ViewportButton icon={<Monitor className="icon-md" />} label="桌面 1280" active={viewport === "desktop"} onClick={() => setViewport("desktop")} />
          <ViewportButton icon={<Tablet className="icon-md" />} label="平板 834" active={viewport === "tablet"} onClick={() => setViewport("tablet")} />
          <ViewportButton icon={<Smartphone className="icon-md" />} label="手机 390" active={viewport === "phone"} onClick={() => setViewport("phone")} />
        </div>
        <span className="preview-pdf-spacer" />
        <button
          type="button"
          className="head-action"
          onClick={openInBrowser}
          disabled={!resolvedPath || !onOpenExternalUrl}
          title={fileName ? `用默认浏览器打开 ${fileName}（可加载外部资源与相对路径图片）` : "用默认浏览器打开"}
          aria-label="用默认浏览器打开"
        >
          <ExternalLink className="icon-md" />
        </button>
      </div>
      <div className="html-stage">
        <iframe
          className="html-frame"
          srcDoc={srcDoc}
          title="网页预览"
          sandbox="allow-scripts"
          style={shellStyle}
        />
      </div>
    </div>
  );
}

function ViewportButton({
  icon,
  label,
  active,
  onClick,
}: {
  icon: React.ReactNode;
  label: string;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      className={active ? "head-action is-active" : "head-action"}
      onClick={onClick}
      title={label}
      aria-label={label}
      aria-pressed={active}
    >
      {icon}
    </button>
  );
}
