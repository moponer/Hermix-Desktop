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
import { Pencil, Check, Printer, FileSpreadsheet, FileType2 } from "lucide-react";
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
    } else if (d.hermixCanvasPrint) {
      // 打印请求：编辑态先退出（排队块放行），保证打印的是完整文档。
      if (EDITING) setEditing(false);
      window.print();
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

/** 画布页面骨架：几何样式内联，运行时脚本置于 body 末尾。
 *  打印样式（@page + @media print）让"打印 → 另存 PDF"的输出与画布
 *  所见一致：slide16x9 → 13.33×7.5in 横向页，a4 → A4 纵向页；
 *  灰底/阴影/页间空隙只在屏幕态存在。 */
function canvasShell(geometry: "slide16x9" | "a4"): string {
  const printCss =
    geometry === "slide16x9"
      ? `@page{size:13.33in 7.5in;margin:0;}`
      : `@page{size:A4;margin:0;}`;
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
  @media print{
    body{background:#fff;padding:0;}
    #stage{width:auto;margin:0;}
    [data-hermix-block]{margin:0;box-shadow:none;border-radius:0;
      width:100%;min-height:0;}
    ${printCss}
  }
</style></head><body><div id="stage"></div>
<script>${CANVAS_RUNTIME}</script>
</body></html>`;
}

export function DocCanvasView({
  fileName,
  onEditCommit,
  onExportXlsx,
  onExportDocx,
}: {
  fileName: string;
  /** 用户退出编辑时回传的整份文档 HTML（导出旁路与宿主侧共用）。 */
  onEditCommit?: (html: string) => void;
  /**
   * 导出 xlsx：收到的是文档当前 HTML（编辑后则以编辑版为准）。
   * 宿主负责转换（ExcelJS）与落盘（save 对话框 + plugin-fs）。
   */
  onExportXlsx?: (html: string) => void;
  /** 导出 docx：同上，宿主用 docx 库映射落盘。 */
  onExportDocx?: (html: string) => void;
}) {
  const [meta, setMeta] = useState<CanvasMeta>(docCanvasStore.getMeta());
  const [editing, setEditing] = useState(false);
  const iframeRef = useRef<HTMLIFrameElement>(null);
  /**
   * 画布代数：每次"新画布开启"（closed true→false 边沿）+1。
   * 用作 iframe 的 React key——代数变化强制卸载重建 iframe（旧 DOM、
   * 旧渲染脚本状态全部丢弃），同时作为重置游标/编辑态的信号源。
   * 不能依赖 geometry 变化：连续两份同几何画布 geometry 不变；
   * 也不能依赖 geometry===null：openCanvas 从旧值直切新值，不经 null。
   */
  const [generation, setGeneration] = useState(0);
  /** 已投递到当前 iframe 的最大块下标 + 1。订阅块与 onLoad 重放共用
   *  这一个游标：两边都以它为起点发送，恰好一次（修复 iframe 重载
   *  期间订阅块与重放双重投递的竞态）。新画布开启时归零（见下）。 */
  const sentCursorRef = useRef(0);
  /**
   * 导出真相源：文档当前 HTML。两路维护——用户编辑回传时整份替换；
   * 流式期间从 store 块历史同步（编辑版优先：一旦编辑过，流式块的
   * 追加不再覆盖编辑结果，直到下次编辑回传）。新画布开启时重置
   * （否则第二份画布的导出会把上一份的编辑版写进磁盘）。
   */
  const currentHtmlRef = useRef<string | null>(null);
  const editedRef = useRef(false);

  // 几何变化（新画布开启）→ 重建 srcDoc。低频：一份回复至多一次。
  const srcDoc = useMemo(() => canvasShell(meta.geometry ?? "a4"), [meta.geometry]);

  // 元信息订阅：壳层的块数 / 生成中 / 收口状态随 store 刷新（store 侧
  // 已合并到微任务，多块一帧只触发一次 React 更新）。
  // 新画布开启边沿（closed true→false）：generation+1 → iframe 因 key
  // 变化而重建（清旧 DOM），sentCursor/编辑态全部归零——"第二份画布"
  // 的系统性重置点（评审 P1 根因：openCanvas 不经过 geometry=null，
  // 仅靠 geometry 边沿会漏掉同几何连续两份画布）。
  useEffect(() => {
    let wasClosed = docCanvasStore.getMeta().closed;
    return docCanvasStore.onMeta((m) => {
      setMeta(m);
      if (wasClosed && !m.closed) {
        sentCursorRef.current = 0;
        currentHtmlRef.current = null;
        editedRef.current = false;
        setEditing(false);
        setGeneration((g) => g + 1);
      }
      wasClosed = m.closed;
    });
  }, []);

  // 块数据不经 React：订阅 store 直接 postMessage 进 iframe。
  // iframe 未就绪时块已记入 store.blocks，onLoad 重放兜底（见下）。
  useEffect(() => {
    return docCanvasStore.onBlock((html, index) => {
      if (index < sentCursorRef.current) return; // 恰好一次
      sentCursorRef.current = index + 1;
      iframeRef.current?.contentWindow?.postMessage({ hermixCanvasBlock: html }, "*");
    });
  }, []);

  // iframe 就绪：从游标起重放剩余块历史。onLoad 前经订阅通道发出的块
  // 会随 srcDoc 重置丢失——游标是唯一真相：重放 = blocks[sentCursor..]，
  // 两条路合计恰好一次。
  const handleIframeLoad = () => {
    const win = iframeRef.current?.contentWindow;
    if (!win) return;
    const blocks = docCanvasStore.getBlocks();
    for (let i = sentCursorRef.current; i < blocks.length; i++) {
      win.postMessage({ hermixCanvasBlock: blocks[i] }, "*");
    }
    sentCursorRef.current = blocks.length;
  };

  // 编辑回传：整份 HTML 交给宿主（导出旁路用）。
  useEffect(() => {
    const onMessage = (e: MessageEvent) => {
      const edited = (e.data as { hermixCanvasEdit?: unknown } | null)?.hermixCanvasEdit;
      if (typeof edited !== "string") return;
      // 只接受当前画布 iframe 的消息：应用内其他沙箱 iframe（如 HtmlView
      // 渲染不可信 HTML）可以 postMessage 任意内容，不校验 source 等于
      // 把导出内容的伪造权交给所有预览页（评审 P2-3）。
      if (e.source !== iframeRef.current?.contentWindow) return;
      setEditing(false);
      // 真相源切换到编辑版：后续流式块（若有）不再覆盖编辑结果。
      editedRef.current = true;
      currentHtmlRef.current = edited;
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

  // 导出 PDF：请求 iframe 内 window.print()（打印的是文档本身而非 app
  // UI），系统对话框里选"另存为 PDF"。@page 打印样式保证分页与画布
  // 几何一致（slide16x9 → 13.33×7.5in 横向，a4 → A4 纵向）。
  const printCanvas = () => {
    iframeRef.current?.contentWindow?.postMessage({ hermixCanvasPrint: true }, "*");
  };

  /** 导出的 HTML 真相源：编辑版优先，否则从 store 块历史拼接。 */
  const exportSourceHtml = (): string | null => {
    if (editedRef.current && currentHtmlRef.current) return currentHtmlRef.current;
    const blocks = docCanvasStore.getBlocks();
    return blocks.length > 0 ? blocks.join("\n") : currentHtmlRef.current;
  };

  const exportXlsx = () => {
    const html = exportSourceHtml();
    if (html) onExportXlsx?.(html);
  };
  const exportDocx = () => {
    const html = exportSourceHtml();
    if (html) onExportDocx?.(html);
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
          className="head-action"
          onClick={printCanvas}
          disabled={editing}
          title="导出 PDF（打印对话框里选「另存为 PDF」）"
          aria-label="导出 PDF"
        >
          <Printer className="icon-md" />
        </button>
        <button
          type="button"
          className="head-action"
          onClick={exportXlsx}
          disabled={editing || !onExportXlsx}
          title="导出 Excel（文档中的表格 → .xlsx）"
          aria-label="导出 Excel"
        >
          <FileSpreadsheet className="icon-md" />
        </button>
        <button
          type="button"
          className="head-action"
          onClick={exportDocx}
          disabled={editing || !onExportDocx}
          title="导出 Word（文档 → .docx）"
          aria-label="导出 Word"
        >
          <FileType2 className="icon-md" />
        </button>
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
          key={generation}
          ref={iframeRef}
          className="html-frame"
          srcDoc={srcDoc}
          title="文档画布"
          sandbox="allow-scripts allow-modals"
          onLoad={handleIframeLoad}
        />
      </div>
    </div>
  );
}
