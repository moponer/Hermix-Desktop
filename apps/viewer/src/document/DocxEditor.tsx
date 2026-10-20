// Word 文档编辑器（重量模式专属）——**文档本体就地编辑**。
//
// 用户诉求（2026-10-20）："打开 word 文档默认直接就可以编辑，因为 office
// 办公软件就是这样的"。所以这里不做「段落列表 + 旁路表单」那种编辑器，
// 而是**渲染出来的文档本身就是编辑面**：docx-preview 还原版面 → 每个段落
// 变成可编辑块 → 用户在文档上直接改字 → 导出新 .docx。
//
// **回写路线**：改动按段落索引回写 OOXML（Rust 侧 docx_edit.rs），只改
// 用户动过的那几段，其余字节原样搬运——样式、页眉页脚、图片、编号、纸张
// 设置全不丢。这比「转成 HTML 再导回」那种整篇重建保真得多。
//
// **保真的边界（明说）**：被改写的段落里，段内局部混排（"一句话中间加粗"）
// 会抹平成该段首个文本 run 的统一格式；段落级/表格级/文档级设置不受影响。
//
// **DOM ↔ OOXML 的段落对齐**：docx-preview 渲染出的 `<p>` 与 document.xml
// 里的 `<w:p>` 一一对应，但页眉/页脚/脚注里的段落**不在** document.xml 里，
// 必须排除，否则索引整体错位。载入后会做一次**逐段文本比对**：对不上就
// 禁用就地编辑并退回段落列表模式，绝不拿错位的索引去改别人的文档。
//
// 后端在 Rust 侧（docx_edit.rs）：ZIP + OOXML 手术，`zip` / `quick-xml`
// 已在工作区 Cargo.lock 里，不新增编译单元。原文件永不覆盖。

import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Save, FolderOpen, Search, RotateCcw, TriangleAlert, Wand2 } from "lucide-react";
import { editedName } from "./office";

interface DocxParagraph {
  index: number;
  text: string;
  kind: string;
  level: number;
  inTable: boolean;
  hasImage: boolean;
}

interface DocxDocument {
  path: string;
  paragraphs: DocxParagraph[];
  total: number;
  truncated: boolean;
}

interface DocxEdit {
  index: number;
  text: string;
}

interface SaveResult {
  path: string;
  bytes: number;
}

/** 归一化文本：只用于**比对**，不用于写回。 */
function normalize(text: string): string {
  return text.replace(/\u00a0/g, " ").replace(/\s+/g, " ").trim();
}

/**
 * 取参与编辑的段落元素。
 *
 * 排除页眉 / 页脚 / 脚注 / 尾注里的段落：它们在 document.xml 之外，
 * 混进来会让索引整体错位（改 A 段结果写到 B 段）。
 */
function editableParagraphs(root: HTMLElement): HTMLElement[] {
  const out: HTMLElement[] = [];
  for (const el of Array.from(root.querySelectorAll("p"))) {
    if (el.closest("header, footer")) continue;
    const cls = el.parentElement?.className ?? "";
    if (typeof cls === "string" && /footnote|endnote/i.test(cls)) continue;
    out.push(el);
  }
  return out;
}

/** 段落类型的展示标签（列表模式用）。 */
function kindLabel(p: DocxParagraph): string {
  if (p.kind === "heading") return `标题 ${p.level || ""}`.trim();
  if (p.kind === "list") return "列表";
  if (p.kind === "quote") return "引用";
  return p.inTable ? "表格" : "正文";
}

/**
 * 段落列表行——**仅在就地编辑对不上时作为退路**。
 *
 * 非受控 textarea：文档动辄上千段，受控写法每敲一个字都要重渲染整个列表。
 */
const ParaRow = memo(function ParaRow({
  p,
  changed,
  onEdit,
}: {
  p: DocxParagraph;
  changed: boolean;
  onEdit: (index: number, text: string) => void;
}) {
  const ref = useRef<HTMLTextAreaElement | null>(null);
  const resize = useCallback(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, []);
  useEffect(resize, [resize]);

  return (
    <div className="dx-row" data-kind={p.kind} data-changed={changed ? "1" : "0"}>
      <div className="dx-meta">
        <span className="dx-idx">{p.index + 1}</span>
        <span className="dx-kind">{kindLabel(p)}</span>
        {p.hasImage && <span className="dx-flag">图</span>}
      </div>
      <textarea
        ref={ref}
        className="dx-text"
        defaultValue={p.text}
        rows={1}
        spellCheck={false}
        aria-label={`第 ${p.index + 1} 段`}
        onChange={(e) => {
          resize();
          onEdit(p.index, e.currentTarget.value);
        }}
      />
    </div>
  );
});

export function DocxEditor({
  path,
  name,
  onPreview,
}: {
  path: string;
  name: string;
  /** 切回只读预览（内嵌在预览区时提供）。 */
  onPreview?: () => void;
}) {
  const [doc, setDoc] = useState<DocxDocument | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  /** 就地编辑与原文对不上 → 退回段落列表模式。 */
  const [alignFailed, setAlignFailed] = useState(false);
  const [filter, setFilter] = useState("");
  const [limit, setLimit] = useState(300);
  const [changedCount, setChangedCount] = useState(0);
  /** 文档缩放：预览栏窄的时候 A4 会被裁掉，得能缩着看。 */
  const [zoom, setZoom] = useState(100);
  /** 本机装了 WordCraft 的话，给一个「用它编辑」的入口（开源 Rust 文字处理器）。 */
  const [wordcraft, setWordcraft] = useState<string | null>(null);

  const bodyRef = useRef<HTMLDivElement | null>(null);
  const styleRef = useRef<HTMLDivElement | null>(null);
  /** 渲染完成后抓一份原始文本快照，导出时逐段比对。 */
  const originalRef = useRef<string[]>([]);
  const editsRef = useRef(new Map<number, string>());

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const d = await invoke<DocxDocument>("docx_read", { path });
        if (cancelled) return;
        setDoc(d);

        // 渲染版面（与共享预览层同一套 docx-preview 选项）。
        const body = bodyRef.current;
        if (!body) return;
        body.innerHTML = "";
        const { renderAsync } = await import("docx-preview");
        const bytes = await readOfficeBytes(path);
        if (cancelled) return;
        await renderAsync(bytes.buffer as ArrayBuffer, body, styleRef.current ?? undefined, {
          inWrapper: true,
          ignoreWidth: false,
          ignoreHeight: false,
          renderHeaders: true,
          renderFooters: true,
          renderFootnotes: true,
          renderEndnotes: true,
          useBase64URL: true,
          className: "docx",
        });
        if (cancelled) return;

        const ps = editableParagraphs(body);
        const domTexts = ps.map((el) => el.textContent ?? "");
        originalRef.current = domTexts;
        // 逐段比对：数量与文本都要对得上，才敢把 DOM 索引当成 OOXML 索引。
        const ok =
          domTexts.length === d.paragraphs.length &&
          d.paragraphs.every((p, i) => normalize(p.text) === normalize(domTexts[i] ?? ""));
        setAlignFailed(!ok);
        // 对得上 → 每段变成可编辑块。
        if (ok) {
          ps.forEach((el) => {
            el.setAttribute("contenteditable", "true");
            el.setAttribute("spellcheck", "false");
            el.classList.add("dx-editable");
          });
        }
      } catch (e) {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [path]);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const p = await invoke<string | null>("viewer_wordcraft_path");
        if (!cancelled) setWordcraft(p);
      } catch {
        /* 没装 / 探测失败：只是少一个入口，不影响任何功能。 */
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  /** 交给本机的 WordCraft 编辑（真·Word 级能力：样式 / 表格 / 修订 / 目录）。 */
  const openInWordcraft = useCallback(async () => {
    try {
      await invoke("viewer_open_with", { path, app: "wordcraft" });
    } catch (e) {
      setNote(`打开 WordCraft 失败：${e instanceof Error ? e.message : String(e)}`);
    }
  }, [path]);

  /** 就地编辑时的改动计数（导出时按文本比对得出真值，这里只做即时反馈）。 */
  const onInput = useCallback(() => {
    const body = bodyRef.current;
    if (!body) return;
    const ps = editableParagraphs(body);
    let n = 0;
    ps.forEach((el, i) => {
      if (normalize(el.textContent ?? "") !== normalize(originalRef.current[i] ?? "")) n++;
    });
    setChangedCount(n);
  }, []);

  const onEdit = useCallback((index: number, text: string) => {
    const map = editsRef.current;
    const before = map.size;
    map.set(index, text);
    if (map.size !== before) setChangedCount(map.size);
  }, []);

  const resetAll = useCallback(() => {
    const body = bodyRef.current;
    const ps = body ? editableParagraphs(body) : [];
    ps.forEach((el, i) => {
      // 还原：把原始文本写回去（保留原 DOM 结构，格式不丢）。
      el.textContent = originalRef.current[i] ?? "";
    });
    editsRef.current.clear();
    setChangedCount(0);
    setNote("已撤销全部修改（未保存）");
  }, []);

  const visible = useMemo(() => {
    const list = doc?.paragraphs ?? [];
    const f = filter.trim().toLowerCase();
    const matched = f ? list.filter((p) => p.text.toLowerCase().includes(f)) : list;
    return matched.slice(0, limit);
  }, [doc, filter, limit]);

  /** 收集改动：就地模式按 DOM 文本比对，列表模式按改动表。 */
  const collectEdits = useCallback((): DocxEdit[] => {
    if (alignFailed) {
      return [...editsRef.current.entries()].map(([index, text]) => ({ index, text }));
    }
    const body = bodyRef.current;
    if (!body) return [];
    const ps = editableParagraphs(body);
    const edits: DocxEdit[] = [];
    ps.forEach((el, i) => {
      const now = el.textContent ?? "";
      if (normalize(now) !== normalize(originalRef.current[i] ?? "")) {
        edits.push({ index: i, text: now });
      }
    });
    return edits;
  }, [alignFailed]);

  const doWrite = useCallback(
    async (outPath: string | null) => {
      const edits = collectEdits();
      if (edits.length === 0) {
        setNote("没有修改可保存");
        return;
      }
      setBusy(true);
      setNote(null);
      try {
        const r = await invoke<SaveResult>("docx_write", {
          srcPath: path,
          outPath,
          edits,
          expectParagraphs: doc?.total ?? null,
        });
        setNote(`已导出 ${r.path}（${Math.round(r.bytes / 1024)} KB）`);
      } catch (e) {
        setNote(`导出失败：${e instanceof Error ? e.message : String(e)}`);
      } finally {
        setBusy(false);
      }
    },
    [path, doc, collectEdits],
  );

  const doSaveAs = useCallback(async () => {
    setBusy(true);
    try {
      const out = await invoke<string | null>("image_save_dialog", {
        defaultName: editedName(name, "docx"),
        filters: [["Word", ["docx"]]],
      });
      setBusy(false);
      if (!out) return;
      await doWrite(out);
    } catch (e) {
      setBusy(false);
      setNote(`另存失败：${e instanceof Error ? e.message : String(e)}`);
    }
  }, [name, doWrite]);

  const reveal = useCallback(async () => {
    if (!note) return;
    const m = note.match(/已导出 (.+?)（/);
    if (m) await invoke("viewer_reveal_in_explorer", { path: m[1] });
  }, [note]);

  return (
    <div className="ed-editor">
      <div className="ed-toolbar">
        <span className="ed-toolbar-name" title={path}>
          {name}
        </span>
        {changedCount > 0 && <span className="ed-dirty">已改 {changedCount} 段</span>}
        <div className="toolbar-spacer" />
        {alignFailed ? (
          <label className="dx-filter">
            <Search className="icon-sm" />
            <input
              type="search"
              value={filter}
              placeholder="查找段落"
              aria-label="查找段落"
              onChange={(e) => setFilter(e.target.value)}
            />
          </label>
        ) : null}
        <button
          type="button"
          className="tb-btn"
          onClick={resetAll}
          disabled={busy || changedCount === 0}
          title="撤销全部修改"
          aria-label="撤销全部修改"
        >
          <RotateCcw className="icon-sm" /> 撤销
        </button>
        <button
          type="button"
          className="tb-btn"
          onClick={() => void doWrite(null)}
          disabled={busy || changedCount === 0 || !!error}
          title="导出为新文件"
          aria-label="导出 Word 文档"
        >
          <Save className="icon-sm" /> 导出
        </button>
        <button
          type="button"
          className="tb-btn"
          onClick={() => void doSaveAs()}
          disabled={busy || changedCount === 0 || !!error}
          title="另存为…"
          aria-label="另存 Word 文档"
        >
          <FolderOpen className="icon-sm" /> 另存为
        </button>
        {wordcraft && (
          <button
            type="button"
            className="tb-btn"
            onClick={() => void openInWordcraft()}
            disabled={busy}
            title="用 WordCraft 打开这个文档（开源 Rust 文字处理器：样式 / 表格 / 修订 / 目录）"
            aria-label="用 WordCraft 打开"
          >
            <Wand2 className="icon-sm" /> WordCraft
          </button>
        )}
        {onPreview && (
          <button
            type="button"
            className="tb-btn"
            onClick={onPreview}
            title="切回只读预览（改动不会丢，只是不再显示编辑面）"
            aria-label="切回只读预览"
          >
            预览
          </button>
        )}
        <div className="seg" role="group" aria-label="文档缩放">
          <button
            type="button"
            onClick={() => setZoom((z) => Math.max(50, z - 10))}
            disabled={zoom <= 50}
            title="缩小"
            aria-label="缩小文档"
          >
            −
          </button>
          <span className="seg-val">{zoom}%</span>
          <button
            type="button"
            onClick={() => setZoom((z) => Math.min(200, z + 10))}
            disabled={zoom >= 200}
            title="放大"
            aria-label="放大文档"
          >
            ＋
          </button>
        </div>
      </div>

      <div className="ed-body">
        <div className="ed-panel dx-canvas">
          {error ? (
            <div className="preview-state-center">
              <div className="preview-state-error">
                <p>{error}</p>
                <small>{path}</small>
              </div>
            </div>
          ) : alignFailed ? (
            <div className="dx-list">
              <div className="dx-warn">
                <TriangleAlert className="icon-sm" />
                预览结构与原文段落对不上（可能是页眉页脚/脚注里的段落），已切换到
                段落列表模式以免改错位置。
              </div>
              {visible.map((p) => (
                <ParaRow
                  key={p.index}
                  p={p}
                  changed={editsRef.current.has(p.index)}
                  onEdit={onEdit}
                />
              ))}
              {visible.length < (doc?.paragraphs.length ?? 0) && (
                <button
                  type="button"
                  className="tb-btn dx-more"
                  onClick={() => setLimit((n) => n + 300)}
                >
                  显示更多（还有 {(doc?.paragraphs.length ?? 0) - visible.length} 段）
                </button>
              )}
            </div>
          ) : (
            <div
              className="docx-shell dx-doc"
              style={{ zoom: zoom / 100 }}
              onInput={onInput}
              onKeyDown={(e) => {
                // 回车会拆出一个新段落 → DOM 索引与 OOXML 索引就对不上了，
                // 直接拦住（新增段落需要改写 document.xml 的结构，超出当前
                // 段落级回写的能力边界）。
                if (e.key === "Enter") e.preventDefault();
              }}
            >
              <div ref={styleRef} />
              <div ref={bodyRef} />
            </div>
          )}
        </div>

        <aside className="ed-side">
          <div className="sb-head">状态</div>
          <div className="ed-status">
            {doc ? `${doc.total} 段` : "—"} · 已改 {changedCount} 段
          </div>
          <div className="ed-status">
            直接在文档上点击段落即可改字（表格单元格里的段落同样可改）。
          </div>
          <div className="ed-status">
            只改你动过的段落，其余原样保留（样式、图片、页眉页脚不丢）。
          </div>
          <div className="ed-status">
            被改写的段落会采用该段首个文本 run 的格式，段内局部混排会拉平。
          </div>
          <div className="ed-status">原文件不会被改动，导出为新文件。</div>
          {note && (
            <div className="ed-note">
              {note}
              {note.startsWith("已导出") && (
                <button type="button" className="img-link" onClick={() => void reveal()}>
                  在资源管理器中显示
                </button>
              )}
            </div>
          )}
        </aside>
      </div>
    </div>
  );
}

/** 读文件字节（docx-preview 需要 ArrayBuffer）。 */
async function readOfficeBytes(path: string): Promise<Uint8Array> {
  const { viewerFileSource } = await import("../fileSource");
  const r = await viewerFileSource.readBytes(path);
  if (!r.ok) throw new Error(r.message);
  return r.bytes;
}
