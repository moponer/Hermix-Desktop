// 文档画布：LLM 流式生成的 HTML 文档在这里"长"出来。
//
// 与 HtmlView 的本质区别：内容不是一次性字符串，而是随对话流式到达的
// 块序列。srcDoc 整体替换 = 每个块都重排整份文档 = 卡顿，所以画布
// iframe 里驻一段渲染脚本，宿主经 postMessage 推增量，DOM 只追加。
//
// 流畅性铁律在本组件的落点：
//   - React 只订阅元信息（几何/块数/收口）——store 已合并到微任务，
//     一帧多个块只触发一次壳层渲染
//   - 块数据不经 React：store → postMessage → iframe 脚本 → appendChild
//   - 旧块 DOM 永不修改（纯追加）；编辑态暂停追加消费（块排队），退出
//     编辑后放行并回传全量编辑结果——流式块与用户编辑不互相覆盖
//
// 时序：iframe 的加载晚于首个块事件（srcDoc 文档解析是异步的），所以
// 渲染脚本在 body 末尾（stage 已存在）+ 宿主在 iframe onLoad 时重放
// store 块历史。onLoad 之后的块走常规 postMessage 通道，两条路合计
// 恰好一次（重放以 store.blocks 为准，脚本端用 document 判空防重）。
//
// 页面几何由 skill 层在 marker 里声明（slide16x9 = 横版幻灯片，横向
// 翻页；a4 = A4 竖版文档，纵向滚动），渲染层不猜（2026-10-13 拍板）。
//
// 沙箱与 HtmlView 同款：allow-scripts、不开 allow-same-origin——iframe
// 是不透明源，编辑内容经 postMessage 出来，不碰应用存储。

import { useEffect, useMemo, useRef, useState } from "react";
import { Pencil, Check } from "lucide-react";
import { docCanvasStore, type CanvasMeta } from "../lib/docCanvasStore";

// ── iframe 内的常驻渲染脚本 ──────────────────────────────────────
// 职责：收块 → 追加 → 编辑回传。无框架、无依赖、零轮询。
// 注入位置在 body 末尾（见 canvasShell），此处 getElementById 必然命中。
const CANVAS_RUNTIME = `(function(){
  var stage = document.getElementById('stage');
  var EDITING = false;

  function appendBlock(html) {
    var tpl = document.createElement('template');
    tpl.innerHTML = html;
    var node = tpl.content.firstElementChild;
    if (!node) return;
    node.dataset.hermixBlock = '1';
    stage.appendChild(node);
    // 编辑态不追焦（用户正在别处打字）；流式态跟读到最新块。
    if (!EDITING) node.scrollIntoView({ block: 'nearest' });
  }

  window.addEventListener('message', function (e) {
    var d = e.data || {};
    if (d.hermixCanvasBlock && stage) {
      if (EDITING) { (stage.__queue = stage.__queue || []).push(d.hermixCanvasBlock); return; }
      appendBlock(d.hermixCanvasBlock);
    } else if (d.hermixCanvasSetEdit !== undefined) {
      setEditing(!!d.hermixCanvasSetEdit);
    }
  });

  function setEditing(on) {
    EDITING = on;
    stage.contentEditable = on ? 'true' : 'false';
    document.documentElement.dataset.editing = on ? '1' : '0';
    if (!on) {
      var q = stage.__queue || []; stage.__queue = [];
      q.forEach(appendBlock);
      // 退出编辑：整份文档 HTML 回传宿主（导出旁路消费）。画布是易逝
      // 视图，回传全量而非增量 diff——正确性优先，导出是低频动作。
      parent.postMessage({ hermixCanvasEdit: stage.innerHTML }, '*');
    }
  }
})();`;

/** 画布页面骨架：几何样式内联，运行时脚本置于 body 末尾。 */
function canvasShell(geometry: "slide16x9" | "a4"): string {
  const pageCss =
    geometry === "slide16x9"
      ? `body{background:#3a3f4b}
         #stage{width:1280px;margin:0 auto;}
         [data-hermix-block]{width:1280px;height:720px;box-sizing:border-box;
           display:flex;flex-direction:column;justify-content:center;
           padding:64px 80px;background:#fff;margin:0 0 24px;
           box-shadow:0 2px 12px rgba(0,0,0,.12);border-radius:4px;overflow:hidden;}`
      : `body{background:#525659}
         #stage{width:794px;margin:0 auto;}
         [data-hermix-block]{min-height:1123px;box-sizing:border-box;
           padding:72px 64px;background:#fff;margin:0 0 24px;
           box-shadow:0 2px 12px rgba(0,0,0,.12);border-radius:4px;}`;
  return `<!DOCTYPE html><html><head><meta charset="utf-8"><style>
  *{margin:0;padding:0}
  body{font-family:"Segoe UI","Microsoft YaHei",system-ui,sans-serif;padding:24px 0;}
  #stage{outline:none;}
  [data-editing="1"] #stage{box-shadow:0 0 0 2px #4c8bf5 inset;}
  ${pageCss}
</style></head><body><div id="stage"></div>
<script>${CANVAS_RUNTIME}</script>
</body></html>`;
}

export function DocCanvasView({
  fileName,
  onEditCommit,
}: {
  fileName: string;
  /** 用户退出编辑时回传的整份文档 HTML（宿主可交给导出旁路）。 */
  onEditCommit?: (html: string) => void;
}) {
  const [meta, setMeta] = useState<CanvasMeta>(docCanvasStore.getMeta());
  const [editing, setEditing] = useState(false);
  const iframeRef = useRef<HTMLIFrameElement>(null);

  // 几何变化（新画布开启）→ 重建 srcDoc。低频：一份回复至多一次。
  const srcDoc = useMemo(() => canvasShell(meta.geometry ?? "a4"), [meta.geometry]);

  // 块数据不经 React：订阅 store 直接 postMessage 进 iframe。
  // iframe 未就绪时块已记入 store.blocks，onLoad 重放兜底（见下）。
  useEffect(() => {
    return docCanvasStore.onBlock((html) => {
      iframeRef.current?.contentWindow?.postMessage({ hermixCanvasBlock: html }, "*");
    });
  }, []);

  // iframe 就绪：重放 store 块历史。onLoad 前经 postMessage 发出的块
  // 会随 srcDoc 重置丢失，重放是唯一可靠通道；onLoad 后的块走订阅
  // 通道。渲染脚本端不做去重——重放发生在任何订阅块到达之前
  //（onLoad 先于后续块事件），两路恰好一次。
  const handleIframeLoad = () => {
    const win = iframeRef.current?.contentWindow;
    if (!win) return;
    for (const block of docCanvasStore.getBlocks()) {
      win.postMessage({ hermixCanvasBlock: block }, "*");
    }
  };

  // 编辑回传：整份 HTML 交给宿主（导出旁路用）。
  useEffect(() => {
    const onMessage = (e: MessageEvent) => {
      const edited = (e.data as { hermixCanvasEdit?: unknown } | null)?.hermixCanvasEdit;
      if (typeof edited !== "string") return;
      setEditing(false);
      onEditCommit?.(edited);
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [onEditCommit]);

  const toggleEdit = () => {
    const next = !editing;
    setEditing(next);
    iframeRef.current?.contentWindow?.postMessage({ hermixCanvasSetEdit: next }, "*");
  };

  if (!meta.geometry) {
    return (
      <div className="preview-state-center" style={{ padding: 48 }}>
        等待文档流…
      </div>
    );
  }

  return (
    <div className="html-root">
      <div className="preview-pdf-bar">
        <span className="text-sm text-text-secondary">
          {fileName} · {meta.geometry === "slide16x9" ? "幻灯片 16:9" : "文档 A4"} ·{" "}
          {meta.blockCount} 块{meta.closed ? "" : " · 生成中…"}
        </span>
        <span className="preview-pdf-spacer" />
        <button
          type="button"
          className={editing ? "head-action is-active" : "head-action"}
          onClick={toggleEdit}
          title={editing ? "完成编辑" : "编辑内容"}
          aria-label={editing ? "完成编辑" : "编辑内容"}
        >
          {editing ? <Check className="icon-md" /> : <Pencil className="icon-md" />}
        </button>
      </div>
      <div className="html-stage">
        <iframe
          ref={iframeRef}
          className="html-frame"
          srcDoc={srcDoc}
          title="文档画布"
          sandbox="allow-scripts"
          onLoad={handleIframeLoad}
        />
      </div>
    </div>
  );
}
