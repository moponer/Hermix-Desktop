// PDF 文档工具对话框（重量模式）。
//
// 与「页面操作」面板的分工：那边做**页级**增删排（Rust 的 lopdf 内容流
// 手术），这边做**文档级**操作（合并 / 裁剪 / 水印 / 页码 / 信息 / 密码，
// 前端 pdf-lib）。两边各自导出一个新文件，不交叉——交叉要跨 IPC 传整份
// PDF 字节，本轮不值得。
//
// 产物永远是**新文件**：走 Rust 的 `write_bytes`（`safe_output_path` 守卫，
// 与源文件相同一律拒绝）。

import { useCallback, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { FileStack, Scissors, Droplets, ListOrdered, FileText, X } from "lucide-react";
import type { FileSource } from "@hermix/preview";
import { applyPdfTools, mergePdfFiles, type PdfToolSet } from "./pdfTools";
import { decodeBase64, encodeBase64, editedName } from "./office";

interface SaveResult {
  path: string;
  bytes: number;
}

type ToolKey = "merge" | "crop" | "watermark" | "numbers" | "meta";

const TOOLS: { key: ToolKey; label: string; icon: typeof FileStack }[] = [
  { key: "merge", label: "合并 PDF", icon: FileStack },
  { key: "crop", label: "裁剪页面", icon: Scissors },
  { key: "watermark", label: "文字水印", icon: Droplets },
  { key: "numbers", label: "页码", icon: ListOrdered },
  { key: "meta", label: "文档信息", icon: FileText },
];

/** "1-3,5" → [1,2,3,5]；空串 → 空数组（= 全部页）。 */
function parsePageRange(text: string): number[] {
  const out = new Set<number>();
  for (const part of text.split(/[,，]/)) {
    const t = part.trim();
    if (!t) continue;
    const m = t.match(/^(\d+)\s*[-~]\s*(\d+)$/);
    if (m) {
      const a = Math.min(Number(m[1]), Number(m[2]));
      const b = Math.max(Number(m[1]), Number(m[2]));
      for (let i = a; i <= b; i++) out.add(i);
    } else if (/^\d+$/.test(t)) {
      out.add(Number(t));
    }
  }
  return [...out].sort((a, b) => a - b);
}

export function PdfToolsDialog({
  path,
  name,
  source,
  onClose,
}: {
  path: string;
  name: string;
  source: FileSource;
  onClose: () => void;
}) {
  const [tool, setTool] = useState<ToolKey>("merge");
  const [note, setNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // 裁剪
  const [cropPages, setCropPages] = useState("");
  const [cropL, setCropL] = useState(0);
  const [cropR, setCropR] = useState(0);
  const [cropT, setCropT] = useState(0);
  const [cropB, setCropB] = useState(0);
  // 水印
  const [wmText, setWmText] = useState("");
  const [wmSize, setWmSize] = useState(36);
  const [wmAngle, setWmAngle] = useState(45);
  const [wmOpacity, setWmOpacity] = useState(0.25);
  const [wmPages, setWmPages] = useState("");
  // 页码
  const [numFormat, setNumFormat] = useState("{n} / {total}");
  const [numStart, setNumStart] = useState(1);
  const [numPages, setNumPages] = useState("");
  // 文档信息
  const [title, setTitle] = useState("");
  const [author, setAuthor] = useState("");
  const [subject, setSubject] = useState("");
  const [keywords, setKeywords] = useState("");

  const pick = useCallback(async () => {
    setError(null);
    setNote(null);
    setBusy(true);
    try {
      const picked = await invoke<string[] | null>("viewer_pick_files", {
        filters: [["PDF", ["pdf"]]],
      });
      if (!picked || picked.length === 0) {
        setNote("没有选择文件");
        return;
      }
      const parts: Uint8Array[] = [];
      for (const p of picked) {
        const r = await source.readBytes(p);
        if (!r.ok) throw new Error(r.message);
        parts.push(r.bytes);
      }
      const merged = await mergePdfFiles(parts);
      const out = await invoke<SaveResult>("write_bytes", {
        srcPath: picked[0],
        outPath: null,
        dataBase64: encodeBase64(merged),
      });
      setNote(`已合并 ${picked.length} 份 → ${out.path}（${Math.round(out.bytes / 1024)} KB）`);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }, [source]);

  const toolSet = useMemo<PdfToolSet>(() => {
    const set: PdfToolSet = {};
    if (tool === "crop" && (cropL || cropR || cropT || cropB)) {
      set.crop = {
        pages: parsePageRange(cropPages),
        left: cropL,
        right: cropR,
        top: cropT,
        bottom: cropB,
      };
    }
    if (tool === "watermark" && wmText.trim()) {
      set.watermark = {
        text: wmText,
        fontSize: wmSize,
        opacity: wmOpacity,
        angle: wmAngle,
        color: "#808080",
        pages: parsePageRange(wmPages),
      };
    }
    if (tool === "numbers" && numFormat.trim()) {
      set.pageNumbers = {
        format: numFormat,
        startAt: numStart,
        pages: parsePageRange(numPages),
      };
    }
    if (tool === "meta") {
      set.meta = { title, author, subject, keywords };
    }
    return set;
  }, [
    tool, cropPages, cropL, cropR, cropT, cropB,
    wmText, wmSize, wmOpacity, wmAngle, wmPages,
    numFormat, numStart, numPages,
    title, author, subject, keywords,
  ]);

  const apply = useCallback(
    async (outPath: string | null) => {
      setBusy(true);
      setError(null);
      setNote(null);
      try {
        const r = await source.readBytes(path);
        if (!r.ok) throw new Error(r.message);
        const bytes = await applyPdfTools(r.bytes, toolSet);
        const out = await invoke<SaveResult>("write_bytes", {
          srcPath: path,
          outPath,
          dataBase64: encodeBase64(bytes),
        });
        setNote(`已导出 ${out.path}（${Math.round(out.bytes / 1024)} KB）`);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setBusy(false);
      }
    },
    [path, source, toolSet],
  );

  const applyAs = useCallback(async () => {
    setBusy(true);
    try {
      const out = await invoke<string | null>("image_save_dialog", {
        defaultName: editedName(name, "pdf"),
        filters: [["PDF", ["pdf"]]],
      });
      setBusy(false);
      if (!out) return;
      await apply(out);
    } catch (e) {
      setBusy(false);
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [name, apply]);

  return (
    <div className="set-overlay" role="dialog" aria-label="PDF 文档工具">
      <div className="set-card pdf-tools-card">
        <div className="set-head">
          <span className="set-title">PDF 文档工具</span>
          <button type="button" className="set-close" onClick={onClose} aria-label="关闭">
            <X className="icon-sm" />
          </button>
        </div>

        <div className="set-body">
          <div className="set-nav">
            {TOOLS.map((t) => (
              <button
                key={t.key}
                type="button"
                className={tool === t.key ? "set-nav-item is-active" : "set-nav-item"}
                onClick={() => {
                  setTool(t.key);
                  setError(null);
                  setNote(null);
                }}
              >
                <t.icon size={14} />
                {t.label}
              </button>
            ))}
          </div>

          <div className="pdf-tools-body">
            {tool === "merge" && (
              <>
                <p className="pdf-tools-hint">
                  把多份 PDF 按选择顺序合成一份（每份的页依次追加）。产物是新文件。
                </p>
                <button type="button" className="ed-btn" onClick={() => void pick()} disabled={busy}>
                  <FileStack className="icon-sm" /> 选择多份 PDF 并合并
                </button>
              </>
            )}

            {tool === "crop" && (
              <>
                <p className="pdf-tools-hint">
                  裁掉页面四周的白边/扫描仪黑边（单位：磅，1 磅 = 1/72 英寸）。
                </p>
                <label className="pdf-tools-field">
                  <span>页码范围（空 = 全部）</span>
                  <input value={cropPages} onChange={(e) => setCropPages(e.target.value)} placeholder="如 1-3,5" />
                </label>
                <div className="pdf-tools-row">
                  <label className="pdf-tools-field">
                    <span>左</span>
                    <input type="number" value={cropL} min={0} onChange={(e) => setCropL(Number(e.target.value) || 0)} />
                  </label>
                  <label className="pdf-tools-field">
                    <span>右</span>
                    <input type="number" value={cropR} min={0} onChange={(e) => setCropR(Number(e.target.value) || 0)} />
                  </label>
                  <label className="pdf-tools-field">
                    <span>上</span>
                    <input type="number" value={cropT} min={0} onChange={(e) => setCropT(Number(e.target.value) || 0)} />
                  </label>
                  <label className="pdf-tools-field">
                    <span>下</span>
                    <input type="number" value={cropB} min={0} onChange={(e) => setCropB(Number(e.target.value) || 0)} />
                  </label>
                </div>
              </>
            )}

            {tool === "watermark" && (
              <>
                <p className="pdf-tools-hint">
                  给页面叠一层文字。英文数字用矢量字体（体积小）；中文走位图渲染。
                </p>
                <label className="pdf-tools-field">
                  <span>水印文字</span>
                  <input value={wmText} onChange={(e) => setWmText(e.target.value)} placeholder="如：内部资料 请勿外传" />
                </label>
                <div className="pdf-tools-row">
                  <label className="pdf-tools-field">
                    <span>字号</span>
                    <input type="number" value={wmSize} min={6} max={200} onChange={(e) => setWmSize(Number(e.target.value) || 12)} />
                  </label>
                  <label className="pdf-tools-field">
                    <span>角度</span>
                    <input type="number" value={wmAngle} min={-90} max={90} onChange={(e) => setWmAngle(Number(e.target.value) || 0)} />
                  </label>
                  <label className="pdf-tools-field">
                    <span>不透明度</span>
                    <input type="number" step={0.05} value={wmOpacity} min={0} max={1} onChange={(e) => setWmOpacity(Number(e.target.value) || 0)} />
                  </label>
                </div>
                <label className="pdf-tools-field">
                  <span>页码范围（空 = 全部）</span>
                  <input value={wmPages} onChange={(e) => setWmPages(e.target.value)} placeholder="如 1-3" />
                </label>
              </>
            )}

            {tool === "numbers" && (
              <>
                <p className="pdf-tools-hint">
                  在页面底部居中加页码。{"{n}"} 是当前页码，{"{total}"} 是总页数。
                </p>
                <label className="pdf-tools-field">
                  <span>格式</span>
                  <input value={numFormat} onChange={(e) => setNumFormat(e.target.value)} />
                </label>
                <div className="pdf-tools-row">
                  <label className="pdf-tools-field">
                    <span>起始页码</span>
                    <input type="number" value={numStart} min={1} onChange={(e) => setNumStart(Number(e.target.value) || 1)} />
                  </label>
                  <label className="pdf-tools-field">
                    <span>页码范围（空 = 全部）</span>
                    <input value={numPages} onChange={(e) => setNumPages(e.target.value)} placeholder="如 2-9" />
                  </label>
                </div>
              </>
            )}

            {tool === "meta" && (
              <>
                <p className="pdf-tools-hint">修改 PDF 的文档属性（标题/作者/主题/关键词）。</p>
                <label className="pdf-tools-field">
                  <span>标题</span>
                  <input value={title} onChange={(e) => setTitle(e.target.value)} />
                </label>
                <label className="pdf-tools-field">
                  <span>作者</span>
                  <input value={author} onChange={(e) => setAuthor(e.target.value)} />
                </label>
                <label className="pdf-tools-field">
                  <span>主题</span>
                  <input value={subject} onChange={(e) => setSubject(e.target.value)} />
                </label>
                <label className="pdf-tools-field">
                  <span>关键词</span>
                  <input value={keywords} onChange={(e) => setKeywords(e.target.value)} />
                </label>
              </>
            )}


            {error && <p className="pdf-tools-error">{error}</p>}
            {note && <p className="pdf-tools-note">{note}</p>}
          </div>
        </div>

        <div className="set-foot">
          {tool !== "merge" && (
            <>
              <button type="button" className="ed-btn" onClick={() => void apply(null)} disabled={busy}>
                导出为新文件
              </button>
              <button type="button" className="ed-btn" onClick={() => void applyAs()} disabled={busy}>
                另存为…
              </button>
            </>
          )}
          <button type="button" className="ed-btn" onClick={onClose}>关闭</button>
        </div>
      </div>
    </div>
  );
}

/** 供外部复用的 base64 解码（把 Rust 回传的字节转成 Uint8Array）。 */
export { decodeBase64 };
