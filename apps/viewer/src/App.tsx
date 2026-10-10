// Hermix 查看器主界面：单 WebView 内多标签 + 按类型分发渲染。
// 决策笔记 2026-10-09-standalone-file-viewer：不做每标签一窗口（WebView2
// 实例 ~100MB/个）；Rust 后端流式读文件，前端只做标签切换与渲染。
// 一期边界：md/text/code/image/pdf 直渲；office/unknown 走系统程序兜底。

import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { mdToHtml, escapeHtml } from "./md";
import { AiSidebar } from "./AiSidebar";

type FileContent =
  | { kind: "text"; name: string; variant: string; content: string }
  | { kind: "image"; name: string; mime: string; data_base64: string }
  | { kind: "pdf"; name: string; data_base64: string }
  | { kind: "unsupported"; name: string; extension: string; reason: string };

interface Tab {
  id: number;
  path: string;
  title: string;
  loading: boolean;
  error: string | null;
  content: FileContent | null;
}

let nextTabId = 1;

/** 扩展名 → 类型徽标文案（纯前端资源表，决策笔记「类型图标」的最小实现）。 */
function badgeOf(name: string): string {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  const table: Record<string, string> = {
    md: "MD", markdown: "MD", txt: "TXT", log: "LOG", json: "JSON",
    yaml: "YML", yml: "YML", toml: "TOML", csv: "CSV",
    rs: "RS", py: "PY", js: "JS", ts: "TS", tsx: "TSX", jsx: "JSX",
    go: "GO", c: "C", cpp: "C++", java: "JAVA", sh: "SH", sql: "SQL",
    png: "PNG", jpg: "JPG", jpeg: "JPG", gif: "GIF", webp: "WEBP", svg: "SVG",
    pdf: "PDF",
  };
  return table[ext] ?? ext.slice(0, 4).toUpperCase() ?? "FILE";
}

export function App() {
  const [tabs, setTabs] = useState<Tab[]>([]);
  const [activeId, setActiveId] = useState<number | null>(null);
  const [dragOver, setDragOver] = useState(false);
  const [aiOpen, setAiOpen] = useState(false);
  const [selection, setSelection] = useState("");
  const opened = useRef<Set<string>>(new Set());

  // 选中文本追踪（AI 右栏「解释选中」等快捷指令的上下文来源）。
  // 监听整个文档的 selectionchange，仅在 active 标签是文本类时保留。
  useEffect(() => {
    const onSel = () => {
      const sel = window.getSelection()?.toString() ?? "";
      setSelection(sel.length > 8000 ? sel.slice(0, 8000) : sel);
    };
    document.addEventListener("selectionchange", onSel);
    return () => document.removeEventListener("selectionchange", onSel);
  }, []);

  const openPath = useCallback((path: string) => {
    if (!path || opened.current.has(path)) {
      // 已打开：切到那个标签。
      setTabs((ts) => {
        const hit = ts.find((t) => t.path === path);
        if (hit) setActiveId(hit.id);
        return ts;
      });
      return;
    }
    opened.current.add(path);
    const id = nextTabId++;
    const title = path.split(/[\\/]/).pop() ?? path;
    setTabs((ts) => [
      ...ts,
      { id, path, title, loading: true, error: null, content: null },
    ]);
    setActiveId(id);
    invoke<FileContent>("viewer_read_file", { path })
      .then((content) =>
        setTabs((ts) =>
          ts.map((t) => (t.id === id ? { ...t, loading: false, content } : t)),
        ),
      )
      .catch((e) =>
        setTabs((ts) =>
          ts.map((t) =>
            (t.id === id ? { ...t, loading: false, error: String(e) } : t),
          ),
        ),
      );
  }, []);

  const closeTab = useCallback((id: number) => {
    setTabs((ts) => {
      const closed = ts.find((t) => t.id === id);
      if (closed) opened.current.delete(closed.path);
      const rest = ts.filter((t) => t.id !== id);
      setActiveId((cur) =>
        cur === id ? (rest.length ? rest[rest.length - 1].id : null) : cur,
      );
      return rest;
    });
  }, []);

  // 启动参数 / 单实例投递的待开文件。
  useEffect(() => {
    void invoke<string[]>("viewer_take_pending").then((paths) =>
      paths.forEach(openPath),
    );
    // 第二实例投递（窗口已开时再开新文件）。
    // Tauri v2 事件名与 Rust 侧 app.emit 对齐。
    import("@tauri-apps/api/event").then(({ listen }) =>
      listen<string>("viewer-open-file", (e) => openPath(e.payload)).then(
        (unlisten) => unlisten,
      ),
    );
  }, [openPath]);

  // 拖放打开（Windows WebView2 drop 拿到的是 File，名称经 viewer_exists 校验
  // 的路径从 dataTransfer files 的 path 取——Tauri v2 拖放带完整路径）。
  const onDrop = useCallback(
    (e: React.DragEvent) => {
      e.preventDefault();
      setDragOver(false);
      for (const f of Array.from(e.dataTransfer.files)) {
        const any = f as File & { path?: string };
        if (any.path) openPath(any.path);
      }
    },
    [openPath],
  );

  const active = tabs.find((t) => t.id === activeId) ?? null;

  return (
    <div
      className="app"
      onDragOver={(e) => {
        e.preventDefault();
        setDragOver(true);
      }}
      onDragLeave={() => setDragOver(false)}
      onDrop={onDrop}
    >
      <div className="tabbar">
        {tabs.map((t) => (
          <div
            key={t.id}
            className={`tab ${t.id === activeId ? "active" : ""}`}
            onClick={() => setActiveId(t.id)}
            title={t.path}
          >
            <span className="badge">{badgeOf(t.title)}</span>
            <span className="tab-title">{t.title}</span>
            <button
              className="tab-close"
              onClick={(ev) => {
                ev.stopPropagation();
                closeTab(t.id);
              }}
              aria-label="关闭标签"
            >
              ×
            </button>
          </div>
        ))}
        {tabs.length === 0 && <div className="tabbar-hint">拖入文件开始查看</div>}
        <div className="tabbar-spacer" />
        <button
          className={`ai-toggle ${aiOpen ? "active" : ""}`}
          onClick={() => setAiOpen((v) => !v)}
          title={aiOpen ? "收起 AI 栏" : "展开 AI 栏"}
        >
          {aiOpen ? "» AI" : "« AI"}
        </button>
      </div>

      <div className="main-row">
        <div className="content">
        {!active ? (
          <div className="empty">
            <div className="empty-icon">📄</div>
            <p>把文件拖进窗口，或从资源管理器「打开方式」选择 Hermix 查看器</p>
            <p className="empty-sub">
              支持 Markdown / 文本 / 代码 / 图片 / PDF；其他类型可用系统程序打开
            </p>
          </div>
        ) : active.loading ? (
          <div className="empty">加载中…</div>
        ) : active.error ? (
          <div className="error-box">
            <p>{active.error}</p>
            <FileActions path={active.path} />
          </div>
        ) : active.content ? (
          <FileView content={active.content} path={active.path} />
        ) : (
          <div className="empty">加载中…</div>
        )}
        {dragOver && <div className="drop-overlay">松开以打开文件</div>}
        </div>
        {aiOpen && (
          <AiSidebar
            filePath={active?.path ?? null}
            selection={selection}
            onClose={() => setAiOpen(false)}
          />
        )}
      </div>
    </div>
  );
}

function FileActions({ path }: { path: string }) {
  return (
    <button
      className="btn"
      onClick={() => void invoke("viewer_open_with_system", { path })}
    >
      用系统程序打开
    </button>
  );
}

function FileView({ content, path }: { content: FileContent; path: string }) {
  switch (content.kind) {
    case "text":
      if (content.variant === "md") {
        return (
          <div className="md-body">
            <div dangerouslySetInnerHTML={{ __html: mdToHtml(content.content) }} />
            <FileActions path={path} />
          </div>
        );
      }
      return (
        <div className="text-body">
          <pre>
            <code>{content.content}</code>
          </pre>
          <FileActions path={path} />
        </div>
      );
    case "image":
      return (
        <div className="image-body">
          <img
            src={`data:${content.mime};base64,${content.data_base64}`}
            alt={escapeHtml(content.name)}
          />
          <FileActions path={path} />
        </div>
      );
    case "pdf":
      return (
        <embed
          className="pdf-frame"
          type="application/pdf"
          src={`data:application/pdf;base64,${content.data_base64}`}
        />
      );
    case "unsupported":
      return (
        <div className="empty">
          <div className="empty-icon">🗂</div>
          <p>
            .{escapeHtml(content.extension)} 文件一期预览不支持（{escapeHtml(content.reason)}）
          </p>
          <FileActions path={path} />
        </div>
      );
  }
}
