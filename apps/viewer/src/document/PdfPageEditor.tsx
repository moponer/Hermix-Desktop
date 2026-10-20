// PDF 页面编辑器（重量模式专属）。
//
// 形态：左侧页列表 + 右侧操作面板。用户改的是**页列表**（删/移/旋转/
// 插空白），导出时把页列表编译成一串 PDF op 交给 Rust 一次执行。
//
// 页号语义（最容易错的地方，钉死三条）：
//   1. 列表里每一项要么是「原第几页」，要么是「空白页」。
//   2. 编译出的 op 顺序固定为：keep（删页 + 重排一次搞定）→ insertBlank
//      （按最终位置**从小到大**插，先插小的才不会顶掉后面页码）→
//      rotate（用**最终页号**）。
//   3. 「最终页号」= 该项在列表里的下标 + 1。

import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ChevronUp, ChevronDown, RotateCw, Save, FolderOpen, FilePlus2, Wrench } from "lucide-react";
import type { FileSource } from "@hermix/preview";
import { PdfToolsDialog } from "./PdfToolsDialog";

/** 一个页面项：原页号（1-based）或空白页（source = null）。 */
interface PageEntry {
  source: number | null;
  rotation: number;
}

interface PdfInfo {
  path: string;
  pages: number;
  encrypted: boolean;
  bytes: number;
  /** 每页当前的显示角度（0/90/180/270，含继承来的）。 */
  rotations: number[];
}

interface SaveResult {
  path: string;
  bytes: number;
  encodingUsed: string | null;
  /** 实际替换的处数（仅文本替换有值）。 */
  replaced?: number | null;
}

/** 与 Rust `PdfOp` 对应（serde tag = "op"，camelCase 变体名）。 */
type PdfOp =
  | { op: "delete"; pages: number[] }
  | { op: "keep"; pages: number[] }
  | { op: "reorder"; order: number[] }
  | { op: "rotate"; pages: number[]; degrees: number }
  | { op: "insertBlank"; at: number; width: number; height: number };

/** A4（PDF point）。空白页默认尺寸。 */
const A4_W = 595;
const A4_H = 842;

export function PdfPageEditor({
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
  const [info, setInfo] = useState<PdfInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pages, setPages] = useState<PageEntry[]>([]);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [note, setNote] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // ── 文本层替换 ──
  const [replacePage, setReplacePage] = useState(1);
  const [replaceFrom, setReplaceFrom] = useState("");
  const [replaceTo, setReplaceTo] = useState("");
  // ── 文档工具（合并 / 裁剪 / 水印 / 页码 / 信息 / 密码）──
  const [toolsOpen, setToolsOpen] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void invoke<PdfInfo>("pdf_info", { srcPath: path })
      .then((r) => {
        if (cancelled) return;
        setInfo(r);
        if (r.encrypted) {
          setError("该 PDF 使用 AES 加密，当前不支持编辑");
          return;
        }
        setPages(
          Array.from({ length: r.pages }, (_, i) => ({
            source: i + 1,
            // 绝对角度（后端写的也是绝对值）：原本就转过 90° 的页，
            // 再点一次「转 90°」应该是 180° 而不是 90°。
            rotation: r.rotations?.[i] ?? 0,
          })),
        );
      })
      .catch((e) => {
        if (!cancelled) setError(String(e instanceof Error ? e.message : e));
      });
    return () => {
      cancelled = true;
    };
  }, [path]);

  const toggle = useCallback((i: number) => {
    setSelected((s) => {
      const next = new Set(s);
      if (next.has(i)) next.delete(i);
      else next.add(i);
      return next;
    });
  }, []);

  const move = useCallback((i: number, delta: number) => {
    setPages((ps) => {
      const j = i + delta;
      if (j < 0 || j >= ps.length) return ps;
      const next = ps.slice();
      [next[i], next[j]] = [next[j], next[i]];
      return next;
    });
    setSelected((s) => {
      const next = new Set(s);
      if (next.has(i)) {
        next.delete(i);
        next.add(i + delta);
      }
      return next;
    });
  }, []);

  const rotate = useCallback((i: number) => {
    setPages((ps) =>
      ps.map((p, idx) => (idx === i ? { ...p, rotation: (p.rotation + 90) % 360 } : p)),
    );
  }, []);

  const removeSelected = useCallback(() => {
    setPages((ps) => ps.filter((_, i) => !selected.has(i)));
    setSelected(new Set());
  }, [selected]);

  const insertBlank = useCallback(() => {
    setPages((ps) => [...ps, { source: null, rotation: 0 }]);
  }, []);

  const reset = useCallback(() => {
    if (!info) return;
    setPages(
      Array.from({ length: info.pages }, (_, i) => ({
        source: i + 1,
        rotation: info.rotations?.[i] ?? 0,
      })),
    );
    setSelected(new Set());
    setNote(null);
  }, [info]);

  /**
   * 页列表 → op 串。
   *
   * 顺序固定：keep → insertBlank（位置升序）→ rotate（最终页号）。
   * 空白页必须**从小到大**插：先插的小页占住前面的位置，后插的大页
   * 才不会因为页数变化而顶掉前面的页码。
   */
  const buildOps = useCallback((): PdfOp[] => {
    const originalCount = info?.pages ?? 0;
    const orig = pages
      .map((p) => p.source)
      .filter((v): v is number => v !== null);
    if (orig.length === 0) throw new Error("没有可保留的页面");
    const ops: PdfOp[] = [];
    // keep 同时表达「删页」与「重排」：它按传入顺序写页树。
    const identity =
      orig.length === originalCount && orig.every((v, i) => v === i + 1);
    if (!identity) {
      ops.push({ op: "keep", pages: orig });
    }
    // 空白页：最终位置 = 列表下标 + 1，按升序插入。
    const blanks = pages
      .map((p, i) => (p.source === null ? i + 1 : -1))
      .filter((v) => v > 0);
    for (const at of blanks) {
      ops.push({ op: "insertBlank", at, width: A4_W, height: A4_H });
    }
    // 旋转：按最终页号分组（一次 rotate op 只接受一个角度）。
    const byDeg = new Map<number, number[]>();
    pages.forEach((p, i) => {
      if (!p.rotation) return;
      const list = byDeg.get(p.rotation) ?? [];
      list.push(i + 1);
      byDeg.set(p.rotation, list);
    });
    for (const [degrees, list] of byDeg) {
      ops.push({ op: "rotate", pages: list, degrees });
    }
    return ops;
  }, [pages, info]);

  const doExport = useCallback(
    async (outPath: string | null) => {
      setBusy(true);
      setNote(null);
      try {
        const ops = buildOps();
        if (ops.length === 0) {
          setNote("没有改动，无需导出");
          return;
        }
        const r = await invoke<SaveResult>("pdf_ops", { srcPath: path, outPath, ops });
        setNote(`已导出 ${r.path}（${Math.round(r.bytes / 1024)} KB）`);
      } catch (e) {
        setNote(`导出失败：${e instanceof Error ? e.message : String(e)}`);
      } finally {
        setBusy(false);
      }
    },
    [buildOps, path],
  );

  const doSaveAs = useCallback(async () => {
    setBusy(true);
    try {
      const out = await invoke<string | null>("image_save_dialog", {
        defaultName: defaultOutputName(name, "pdf"),
        filters: [["PDF", ["pdf"]]],
      });
      setBusy(false);
      if (!out) return;
      await doExport(out);
    } catch (e) {
      setBusy(false);
      setNote(`另存失败：${e instanceof Error ? e.message : String(e)}`);
    }
  }, [name, doExport]);

  const reveal = useCallback(async () => {
    if (!note) return;
    const m = note.match(/已导出 (.+?)（/);
    if (m) await source.revealInExplorer?.(m[1]);
  }, [note, source]);

  /**
   * 文本层替换：只认 Tj 算子、整串精确相等。
   * 后端会先统计命中数，0 命中直接报错——不会静默产出一个内容没变的文件。
   */
  const doReplaceText = useCallback(async () => {
    if (!replaceFrom) {
      setNote("请填写要替换的原文");
      return;
    }
    setBusy(true);
    setNote(null);
    try {
      const r = await invoke<SaveResult>("pdf_replace_text", {
        srcPath: path,
        outPath: null,
        page: replacePage,
        from: replaceFrom,
        to: replaceTo,
      });
      setNote(
        `已替换 ${r.replaced ?? 0} 处并导出 ${r.path}（${Math.round(r.bytes / 1024)} KB）`,
      );
    } catch (e) {
      setNote(`替换失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
    }
  }, [path, replacePage, replaceFrom, replaceTo]);

  const summary = useMemo(() => {
    const blanks = pages.filter((p) => p.source === null).length;
    const rotated = pages.filter((p) => p.rotation !== 0).length;
    const kept = pages.length - blanks;
    return `${kept} 页${blanks ? ` + ${blanks} 空白页` : ""}${
      rotated ? ` · ${rotated} 页已旋转` : ""
    }`;
  }, [pages]);

  return (
    <div className="ed-editor">
      <div className="ed-toolbar">
        <span className="ed-toolbar-name" title={path}>{name}</span>
        <div className="toolbar-spacer" />
        <button
          type="button"
          className="tb-btn"
          onClick={() => setToolsOpen(true)}
          disabled={busy}
          title="文档工具（合并 / 裁剪 / 水印 / 页码 / 文档信息 / 密码）"
          aria-label="打开 PDF 文档工具"
        >
          <Wrench className="icon-sm" /> 文档工具
        </button>
        <button
          type="button"
          className="tb-btn"
          onClick={() => void doExport(null)}
          disabled={busy || !!error}
          title="导出为新文件"
          aria-label="导出 PDF"
        >
          <Save className="icon-sm" /> 导出
        </button>
        <button
          type="button"
          className="tb-btn"
          onClick={() => void doSaveAs()}
          disabled={busy || !!error}
          title="另存为…"
          aria-label="另存 PDF"
        >
          <FolderOpen className="icon-sm" /> 另存为
        </button>
        <button type="button" className="tb-btn" onClick={onClose} title="退出编辑" aria-label="退出编辑">
          退出
        </button>
      </div>

      <div className="ed-body">
        <div className="ed-panel">
          {error ? (
            <div className="preview-state-center">
              <div className="preview-state-error">
                <p>{error}</p>
                <small>{path}</small>
              </div>
            </div>
          ) : (
            <ol className="pdf-pages">
              {pages.map((p, i) => (
                <li
                  key={`${p.source ?? "blank"}-${i}`}
                  className={`pdf-page ${selected.has(i) ? "is-sel" : ""}`}
                >
                  <label className="pdf-page-check">
                    <input
                      type="checkbox"
                      checked={selected.has(i)}
                      onChange={() => toggle(i)}
                      aria-label={`选择第 ${i + 1} 页`}
                    />
                    <span className="pdf-page-label">
                      {p.source === null ? "空白页" : `第 ${p.source} 页`}
                      {p.rotation ? ` · ${p.rotation}°` : ""}
                    </span>
                  </label>
                  <span className="pdf-page-ops">
                    <button
                      type="button"
                      className="tb-btn"
                      onClick={() => move(i, -1)}
                      disabled={i === 0}
                      title="上移"
                      aria-label={`第 ${i + 1} 页上移`}
                    >
                      <ChevronUp className="icon-sm" />
                    </button>
                    <button
                      type="button"
                      className="tb-btn"
                      onClick={() => move(i, 1)}
                      disabled={i === pages.length - 1}
                      title="下移"
                      aria-label={`第 ${i + 1} 页下移`}
                    >
                      <ChevronDown className="icon-sm" />
                    </button>
                    <button
                      type="button"
                      className="tb-btn"
                      onClick={() => rotate(i)}
                      title="旋转 90°"
                      aria-label={`第 ${i + 1} 页旋转`}
                    >
                      <RotateCw className="icon-sm" />
                    </button>
                  </span>
                </li>
              ))}
            </ol>
          )}
        </div>

        <aside className="ed-side">
          <div className="sb-head">页面</div>
          <div className="ed-status">{summary}</div>
          <div className="ed-actions">
            <button
              type="button"
              className="ed-btn"
              onClick={removeSelected}
              disabled={selected.size === 0}
            >
              删除选中（{selected.size}）
            </button>
            <button type="button" className="ed-btn" onClick={insertBlank}>
              <FilePlus2 className="icon-sm" /> 末尾插空白页
            </button>
            <button type="button" className="ed-btn" onClick={reset} disabled={!info}>
              还原
            </button>
          </div>
          <div className="ed-status">
            导出为新文件，原 PDF 不会被改动。
          </div>

          <div className="sb-head">文本替换</div>
          <div className="ed-actions">
            <label className="ed-field">
              <span>页码</span>
              <input
                type="number"
                min={1}
                max={info?.pages ?? 1}
                value={replacePage}
                onChange={(e) =>
                  setReplacePage(Math.max(1, Number(e.target.value) || 1))
                }
              />
            </label>
            <label className="ed-field">
              <span>原文</span>
              <input
                value={replaceFrom}
                placeholder="整串匹配，如「旧标题」"
                onChange={(e) => setReplaceFrom(e.target.value)}
              />
            </label>
            <label className="ed-field">
              <span>新文</span>
              <input
                value={replaceTo}
                placeholder="留空即删除"
                onChange={(e) => setReplaceTo(e.target.value)}
              />
            </label>
            <button
              type="button"
              className="ed-btn"
              disabled={busy || !!error || !replaceFrom}
              onClick={() => void doReplaceText()}
            >
              替换并导出
            </button>
          </div>
          <div className="ed-status">
            只处理文本层里的整串文字（扫描件与 TJ 数组排版会明确报「未找到」）。
          </div>
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
      {toolsOpen && (
        <PdfToolsDialog
          path={path}
          name={name}
          source={source}
          onClose={() => setToolsOpen(false)}
        />
      )}
    </div>
  );
}

function defaultOutputName(name: string, ext: string): string {
  const dot = name.lastIndexOf(".");
  const stem = dot > 0 ? name.slice(0, dot) : name;
  return `${stem}.edited.${ext}`;
}
