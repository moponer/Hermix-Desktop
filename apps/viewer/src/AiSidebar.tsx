// AI 右栏（可折叠）：非流式对话，上下文自动携带当前文件路径/选中文本。
// 传输走 Rust 侧 ai_send 命令（reqwest 直连本地 server /v1/）——viewer 是
// 独立进程，不能用主程序前端的 stdio 桥。一期非流式（预览级定位）。

import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

interface Msg {
  role: "user" | "assistant";
  text: string;
}

/** 快捷指令（决策笔记「快捷指令」形态同 WorkBuddy：一键预填发送）。 */
const QUICK_ACTIONS: { label: string; tpl: (sel: string) => string }[] = [
  { label: "总结", tpl: () => "总结这个文件的主要内容" },
  {
    label: "检查",
    tpl: () => "检查这个文件有没有问题（错误、风险、遗漏），逐条列出",
  },
  {
    label: "解释选中",
    tpl: (sel) => `解释这段内容的含义：\n\n${sel.slice(0, 2000)}`,
  },
];

export function AiSidebar({
  filePath,
  selection,
  onClose,
}: {
  filePath: string | null;
  selection: string;
  onClose: () => void;
}) {
  const [messages, setMessages] = useState<Msg[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    listRef.current?.scrollTo(0, listRef.current.scrollHeight);
  }, [messages, busy]);

  const send = useCallback(
    async (text: string) => {
      const trimmed = text.trim();
      if (!trimmed || busy) return;
      setInput("");
      setError(null);
      setBusy(true);
      setMessages((m) => [...m, { role: "user", text: trimmed }]);
      try {
        const reply = await invoke<{ reply?: string }>("ai_send", {
          text: trimmed,
          filePath,
          selection: selection || null,
        });
        setMessages((m) => [
          ...m,
          { role: "assistant", text: reply.reply ?? "(空回复)" },
        ]);
      } catch (e) {
        setError(String(e instanceof Error ? e.message : e));
      } finally {
        setBusy(false);
      }
    },
    [busy, filePath, selection],
  );

  return (
    <aside className="ai-side">
      <div className="ai-head">
        <span className="ai-title">AI 助手</span>
        {filePath && (
          <span className="ai-file" title={filePath}>
            {filePath.split(/[\\/]/).pop()}
          </span>
        )}
        <button className="ai-close" onClick={onClose} aria-label="收起 AI 栏">
          »
        </button>
      </div>

      <div className="ai-msgs" ref={listRef}>
        {messages.length === 0 && (
          <div className="ai-empty">
            <p>基于当前文件提问</p>
            {filePath ? (
              <p className="ai-empty-sub">上下文：{filePath.split(/[\\/]/).pop()}</p>
            ) : (
              <p className="ai-empty-sub">先打开一个文件（未打开也可直接提问）</p>
            )}
          </div>
        )}
        {messages.map((m, i) => (
          <div key={i} className={`ai-msg ai-msg-${m.role}`}>
            {m.text}
          </div>
        ))}
        {busy && <div className="ai-msg ai-msg-assistant ai-busy">思考中…</div>}
      </div>

      {error && <div className="ai-error">{error}</div>}

      <div className="ai-quick">
        {QUICK_ACTIONS.map((q) => (
          <button
            key={q.label}
            className="ai-quick-btn"
            disabled={busy}
            onClick={() => void send(q.tpl(selection))}
          >
            {q.label}
          </button>
        ))}
      </div>

      <div className="ai-input-row">
        <textarea
          className="ai-input"
          rows={2}
          placeholder="问点什么…（Enter 发送，Shift+Enter 换行）"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              void send(input);
            }
          }}
        />
        <button
          className="ai-send"
          disabled={busy || !input.trim()}
          onClick={() => void send(input)}
          aria-label="发送"
        >
          ➤
        </button>
      </div>
    </aside>
  );
}
