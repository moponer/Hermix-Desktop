// AI 右栏（可折叠）：非流式对话，上下文自动携带当前文件路径/选中文本。
//
// 传输走 Rust 侧 ai_send：**直连 OpenAI 兼容端点**——继承主程序时读主程序
// 同一份 config.yaml + .env，独立模型时用查看器自己配的端点。早先的实现
// 连的是本地 server 的 http://127.0.0.1:3000/v1/，而桌面端 sidecar 跑的是
// `--stdio`（不绑端口），于是「只开主程序」时这条链接不上，AI 栏形同虚设。

import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { isImageExt } from "@hermix/preview";
import { Bot, ChevronDown, ChevronsRight } from "lucide-react";
import { ContextMenu } from "./ContextMenu";
import { ProviderIcon } from "@hermix/preview/ProviderIcon";
import {
  GENERAL_PROMPTS,
  IMAGE_PROMPTS,
  aiOutputName,
  type AiPromptTemplate,
} from "./aiPrompts";
import {
  loadAiModels,
  loadAiStatus,
  loadSettings,
  saveSettings,
  type AiModelOption,
  type AiStatus,
  type ViewerSettings,
} from "./settings";

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
  /** 流式中的部分回复：null = 还没有 delta（显示思考动效）。
   *  与 busy 分开：busy 覆盖「请求已发、等首字节」+「流式接收中」两段。 */
  const [streamText, setStreamText] = useState<string | null>(null);
  /** 流式文本的同步镜像：catch 里要把已到的部分内容留住，state 闭包读不到。 */
  const streamRef = useRef<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** 当前生效的 AI 后端（设置 → AI 助手）。面板顶部要让人一眼看出在跟谁说话。 */
  const [backend, setBackend] = useState<AiStatus | null>(null);
  /** 主程序里启用过的模型清单 + 当前查看器 AI 设置：头部直接切模型，
   *  不用再绕设置对话框。custom 模式的模型是手填的（无清单），下拉置灰。 */
  const [models, setModels] = useState<AiModelOption[]>([]);
  const [aiSettings, setAiSettings] = useState<ViewerSettings | null>(null);
  const listRef = useRef<HTMLDivElement>(null);
  // 会话 id 随面板生命周期：首条消息建会话，后续复用（多轮上下文在
  // server 侧）；面板收起即视为对话结束，再开是新会话。
  const sessionIdRef = useRef<string | null>(null);

  useEffect(() => {
    listRef.current?.scrollTo(0, listRef.current.scrollHeight);
  }, [messages, busy, streamText]);

  useEffect(() => {
    void loadAiStatus().then(setBackend);
    void loadAiModels().then(setModels);
    void loadSettings().then(setAiSettings);
  }, []);

  // 流式：Rust 侧每个 delta 发一次 ai-delta。busy 且 streamText 为 null =
  // 还没收到首字节（思考动效）；非 null = 打字机进行中。
  useEffect(() => {
    let un: UnlistenFn | undefined;
    void listen<string>("ai-delta", (e) => {
      streamRef.current = (streamRef.current ?? "") + e.payload;
      setStreamText(streamRef.current);
    }).then((u) => {
      un = u;
    });
    return () => {
      un?.();
    };
  }, []);

  /** 头部下拉选模型：写 settings.ai.selectedModel 并刷新生效状态。
   *  空值 = 跟随主程序默认。存不进去不阻塞：本次会话仍按旧值走。 */
  const pickModel = useCallback(async (id: string) => {
    setAiSettings((cur) => {
      if (!cur) return cur;
      const next: ViewerSettings = {
        ...cur,
        ai: { ...cur.ai, selectedModel: id === "" ? null : id },
      };
      void saveSettings(next)
        .then((saved) => setAiSettings(saved))
        .then(() => loadAiStatus())
        .then(setBackend)
        .catch(() => undefined);
      return next;
    });
  }, []);

  const send = useCallback(
    async (text: string) => {
      const trimmed = text.trim();
      if (!trimmed || busy) return;
      setInput("");
      setError(null);
      setBusy(true);
      // 流式状态复位：null = 未收到首字节（思考动效）。
      streamRef.current = null;
      setStreamText(null);
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
        // 出错但流式已到一部分：把已到的部分留为正式消息，别白看半截。
        if (streamRef.current) {
          const partial = streamRef.current;
          setMessages((m) => [...m, { role: "assistant", text: partial }]);
        }
        setError(String(e instanceof Error ? e.message : e));
      } finally {
        setBusy(false);
        streamRef.current = null;
        setStreamText(null);
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

  // 头部模型按钮的当前值：custom 模式无清单（模型是手填的），显示生效模型。
  const customMode = aiSettings?.ai.mode === "custom";
  const selectedModel = customMode ? null : (aiSettings?.ai.selectedModel ?? null);
  // 按钮上只显示模型 id（「免费模型 · Kilo · 」这类供应商前缀在弹层里
  // 十项重复九次，纯噪音）；完整 id 悬停可见。
  const modelLabel = customMode
    ? (backend?.model || "独立模型")
    : selectedModel
      ? (models.find((m) => m.id === selectedModel)?.model ?? selectedModel)
      : "跟随主程序默认";
  const [modelMenu, setModelMenu] = useState<{ x: number; y: number } | null>(null);

  return (
    <aside className="ai-side">
      <div className="ai-head">
        <span className="ai-title">AI 助手</span>
        {/* 模型选择占标题栏剩余宽度：inherit 模式列出主程序启用过的模型
            （跟随默认 + 手选）；custom 模式没有清单（端点/模型在设置里
            手填），按钮显示生效值。文件名不占这里——标签上已有，空态里
            也提示上下文。
            弹层用自有 ContextMenu 而不是原生 select：原生弹层的样式系统
            说了算，且选项里「免费模型 · Kilo · 」前缀十项重复九次——
            自有弹层只显示模型 id、当前项打勾、随主题。 */}
        <button
          type="button"
          className={`ai-model-btn${backend && !backend.ready ? " is-bad" : ""}`}
          onClick={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            setModelMenu({ x: r.left, y: r.bottom + 4 });
          }}
          disabled={customMode || !aiSettings}
          title={
            customMode
              ? "独立模型模式：端点与模型在 设置 → AI 助手 中配置"
              : backend?.ready
                ? `对话模型（生效：${backend.model}）`
                : "对话模型"
          }
          aria-label="选择对话模型"
          aria-expanded={modelMenu !== null}
        >
          <Bot className="icon-sm" aria-hidden="true" />
          <span className="ai-model-cur">{modelLabel}</span>
          <ChevronDown className="icon-sm" aria-hidden="true" />
        </button>
        <button
          type="button"
          className="ai-close"
          onClick={onClose}
          aria-label="收起 AI 栏"
          title="收起 AI 栏"
        >
          <ChevronsRight className="icon-sm" />
        </button>
      </div>

      {/* 模型清单：只显示模型 id（供应商名重复无信息量），当前项打勾，
          主程序默认项右侧标注。不可用的置灰——选了只会 401。
          行首图标：内置供应商走品牌 logo，免费渠道（custom-*）统一齿轮
          （ProviderIcon，主程序同款解析）。 */}
      {modelMenu && (
        <ContextMenu
          x={modelMenu.x}
          y={modelMenu.y}
          onClose={() => setModelMenu(null)}
          groups={[
            [
              {
                id: "__follow",
                label: "跟随主程序默认",
                icon: <Bot size={14} />,
                checked: !customMode && !selectedModel,
                onSelect: () => void pickModel(""),
              },
            ],
            models.map((m) => ({
              id: m.id,
              label: `${m.model}${m.isDefault ? "（默认）" : ""}${
                m.ready ? "" : "（不可用）"
              }`,
              icon: <ProviderIcon providerId={m.provider} size={14} />,
              checked: !customMode && selectedModel === m.id,
              disabled: customMode || !m.ready,
              onSelect: () => void pickModel(m.id),
            })),
          ]}
        />
      )}

      {/* 未就绪的原因要摆出来，别让用户对着一个永远失败的输入框猜；
          就绪时模型信息已由头部下拉承担，不再占一行。 */}
      {backend && !backend.ready && (
        <div className="ai-backend is-bad" title={backend.baseUrl}>
          未就绪：{backend.problem ?? "配置不完整"}
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
        {/* 等首字节：三点跳动；流式进行中：打字机文本 + 闪烁光标。 */}
        {busy && streamText === null && (
          <div className="ai-msg ai-msg-assistant ai-thinking" aria-live="polite">
            <span className="ai-dots" aria-hidden="true">
              <i />
              <i />
              <i />
            </span>
            思考中…
          </div>
        )}
        {busy && streamText !== null && (
          <div className="ai-msg ai-msg-assistant ai-streaming" aria-live="polite">
            {streamText}
            <span className="ai-cursor" aria-hidden="true" />
          </div>
        )}
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
