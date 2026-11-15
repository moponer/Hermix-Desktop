// 文本编辑器（重量模式专属）。
//
// 形态：一个大文本框 + 顶部工具条。目标很窄：**改文本、另存为新文件**。
// 不做富文本、不做语法高亮编辑——那是 IDE 的活，查看器只负责把内容
// 摆出来并让人安全地写回一个**新**文件。
//
// 编码保真：读的时候用共享层的 `decodeTextBytes`（BOM / UTF-16 嗅探 /
// 严格 UTF-8 / 严格 GBK 的判定只有一份），写的时候把识别到的编码带回
// Rust（Rust 侧 utf-16 带 BOM、gbk 走 Win32 WideCharToMultiByte）。
//
// 原文件只读：所有写盘都走 Rust 的 `write_text`，它带源路径守卫——
// 在另存对话框里选中源文件本身会被拒绝。

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { decodeTextBytes } from "@hermix/preview";
import type { FileSource } from "@hermix/preview";
import { Save, FolderOpen } from "lucide-react";

interface SaveResult {
  path: string;
  bytes: number;
  encodingUsed: string | null;
}

/** 可选编码（与 Rust `resolve_encoding` 的接受名一致）。 */
const ENCODINGS = [
  { id: "utf-8", label: "UTF-8" },
  { id: "gbk", label: "GBK（简体中文）" },
  { id: "utf-16le", label: "UTF-16 LE" },
  { id: "utf-16be", label: "UTF-16 BE" },
] as const;

export function TextEditor({
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
  const [text, setText] = useState("");
  const [encoding, setEncoding] = useState("utf-8");
  const [original, setOriginal] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      const r = await source.readBytes(path);
      if (cancelled) return;
      if (!r.ok) {
        setError(r.message);
        return;
      }
      const decoded = decodeTextBytes(r.bytes);
      if (decoded.kind === "binary") {
        setError("这是二进制文件，不能按文本编辑");
        return;
      }
      if (decoded.kind === "empty") {
        setText("");
        setOriginal("");
        setEncoding("utf-8");
        return;
      }
      setText(decoded.text);
      setOriginal(decoded.text);
      setEncoding(decoded.encoding);
    })();
    return () => {
      cancelled = true;
    };
  }, [path, source]);

  const dirty = text !== original;

  const doExport = useCallback(
    async (outPath: string | null) => {
      setBusy(true);
      setNote(null);
      try {
        const r = await invoke<SaveResult>("write_text", {
          srcPath: path,
          outPath,
          text,
          encoding,
        });
        setNote(
          `已导出 ${r.path}（${Math.round(r.bytes / 1024)} KB，${r.encodingUsed ?? encoding}）`,
        );
      } catch (e) {
        setNote(`导出失败：${e instanceof Error ? e.message : String(e)}`);
      } finally {
        setBusy(false);
      }
    },
    [path, text, encoding],
  );

  const doSaveAs = useCallback(async () => {
    setBusy(true);
    try {
      const out = await invoke<string | null>("image_save_dialog", {
        defaultName: defaultOutputName(name),
        filters: [["文本", ["txt", "md", "log", "json", "yaml", "yml", "csv"]]],
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

  return (
    <div className="ed-editor">
      <div className="ed-toolbar">
        <span className="ed-toolbar-name" title={path}>{name}</span>
        {dirty && <span className="ed-dirty">已修改</span>}
        <div className="toolbar-spacer" />
        <label className="ed-encoding">
          <span>编码</span>
          <select value={encoding} onChange={(e) => setEncoding(e.target.value)}>
            {ENCODINGS.map((e) => (
              <option key={e.id} value={e.id}>
                {e.label}
              </option>
            ))}
          </select>
        </label>
        <button
          type="button"
          className="tb-btn"
          onClick={() => void doExport(null)}
          disabled={busy || !!error}
          title="导出为新文件"
          aria-label="导出文本"
        >
          <Save className="icon-sm" /> 导出
        </button>
        <button
          type="button"
          className="tb-btn"
          onClick={() => void doSaveAs()}
          disabled={busy || !!error}
          title="另存为…"
          aria-label="另存文本"
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
            <textarea
              className="text-editor-area"
              value={text}
              onChange={(e) => setText(e.target.value)}
              spellCheck={false}
              aria-label="文本内容"
            />
          )}
        </div>

        <aside className="ed-side">
          <div className="sb-head">状态</div>
          <div className="ed-status">
            {text.length} 字符 · {text.split("\n").length} 行
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

function defaultOutputName(name: string): string {
  const dot = name.lastIndexOf(".");
  const stem = dot > 0 ? name.slice(0, dot) : name;
  const ext = dot > 0 ? name.slice(dot + 1) : "txt";
  return `${stem}.edited.${ext}`;
}
