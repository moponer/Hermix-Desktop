// AI 右栏（可折叠）：非流式对话，上下文自动携带当前文件路径/选中文本。
//
// 传输走 Rust 侧 ai_send：**直连 OpenAI 兼容端点**——继承主程序时读主程序
// 同一份 config.yaml + .env，独立模型时用查看器自己配的端点。早先的实现
// 连的是本地 server 的 http://127.0.0.1:3000/v1/，而桌面端 sidecar 跑的是
// `--stdio`（不绑端口），于是「只开主程序」时这条链接不上，AI 栏形同虚设。

import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { isImageExt } from "@hermix/preview";
import {
  GENERAL_PROMPTS,
  IMAGE_PROMPTS,
  aiOutputName,
  type AiPromptTemplate,
} from "./aiPrompts";
import { loadAiStatus, type AiStatus } from "./settings";

interface Msg {
  role: "user" | "assistant";
  text: string;
}

/** 外部请求发送一条消息（文件列表右键「加入 AI 对话」走这条）。 */
export interface PendingSend {
  text: string;
  /** 每次请求一个唯一值：同一条消息不会被发两遍。 */
  nonce: number;
}

export function AiSidebar({
  filePath,
  selection,
  onClose,
  pending,
  onPendingHandled,
}: {
  filePath: string | null;
  selection: string;
  onClose: () => void;
  pending?: PendingSend | null;
  onPendingHandled?: () => void;
}) {
  const [messages, setMessages] = useState<Msg[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** 当前生效的 AI 后端（设置 → AI 助手）。面板顶部要让人一眼看出在跟谁说话。 */
  const [backend, setBackend] = useState<AiStatus | null>(null);
  const listRef = useRef<HTMLDivElement>(null);
  // 会话 id 随面板生命周期：首条消息建会话，后续复用（多轮上下文在
  // server 侧）；面板收起即视为对话结束，再开是新会话。
  const sessionIdRef = useRef<string | null>(null);

  useEffect(() => {
    listRef.current?.scrollTo(0, listRef.current.scrollHeight);
  }, [messages, busy]);

  useEffect(() => {
    void loadAiStatus().then(setBackend);
  }, []);

  const send = useCallback(
    async (text: string) => {
      const trimmed = text.trim();
      if (!trimmed || busy) return;
      setInput("");
      setError(null);
      setBusy(true);
      setMessages((m) => [...m, { role: "user", text: trimmed }]);
      try {
        // 直连模式没有服务端会话，**历史由前端带**——否则每一轮都是
        // 孤立的一问一答，模型看不到上文。
        const history = messages.map((m) => ({ role: m.role, text: m.text }));
        const reply = await invoke<{ reply: string; sessionId: string }>("ai_send", {
          text: trimmed,
          filePath,
          selection: selection || null,
          sessionId: sessionIdRef.current,
          history,
        });
        sessionIdRef.current = reply.sessionId;
        setMessages((m) => [
          ...m,
          { role: "assistant", text: reply.reply || "(空回复)" },
        ]);
      } catch (e) {
        setError(String(e instanceof Error ? e.message : e));
      } finally {
        setBusy(false);
      }
    },
    [busy, filePath, messages, selection],
  );

  // 外部触发的发送（右键「加入 AI 对话」）。nonce 保证同一条只发一次。
  const handledNonce = useRef(-1);
  useEffect(() => {
    if (!pending || handledNonce.current === pending.nonce) return;
    handledNonce.current = pending.nonce;
    void send(pending.text);
    onPendingHandled?.();
  }, [pending, send, onPendingHandled]);

  // 模板：图片给改图组，通用组两种文件都适用。
  const isImage = !!filePath && isImageExt(filePath);
  const fileName = filePath ? (filePath.split(/[\\/]/).pop() ?? filePath) : "当前文件";
  const outName = aiOutputName(fileName);

  const sendTemplate = useCallback(
    (t: AiPromptTemplate) => {
      void send(t.build(fileName, outName));
    },
    [send, fileName, outName],
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

      {/* 在跟谁说话：继承主程序的模型还是查看器自己的独立模型。配置不全时
          直接把原因摆出来，别让用户对着一个永远失败的输入框猜。 */}
      {backend && (
        <div className={`ai-backend ${backend.ready ? "" : "is-bad"}`} title={backend.baseUrl}>
          {backend.ready
            ? `${backend.source} · ${backend.model}`
            : `未就绪：${backend.problem ?? "配置不完整"}`}
        </div>
      )}

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

      {filePath && (
        <div className="ai-note">AI 的产物会另存为新文件，原文件不会被改动</div>
      )}

      <div className="ai-quick">
        {isImage && (
          <div className="ai-quick-group">
            <span className="ai-quick-label">改图</span>
            {IMAGE_PROMPTS.map((t) => (
              <button
                key={t.id}
                className="ai-quick-btn"
                disabled={busy}
                title={t.hint}
                onClick={() => sendTemplate(t)}
              >
                {t.label}
              </button>
            ))}
          </div>
        )}
        <div className="ai-quick-group">
          <span className="ai-quick-label">通用</span>
          {GENERAL_PROMPTS.map((t) => (
            <button
              key={t.id}
              className="ai-quick-btn"
              disabled={busy}
              title={t.hint}
              onClick={() => sendTemplate(t)}
            >
              {t.label}
            </button>
          ))}
          {selection && (
            <button
              className="ai-quick-btn"
              disabled={busy}
              title="解释选中的文本"
              onClick={() => void send(`解释这段内容的含义：\n\n${selection.slice(0, 2000)}`)}
            >
              解释选中
            </button>
          )}
        </div>
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
