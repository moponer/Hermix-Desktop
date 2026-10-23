/* ════════════════════════════════════════════════════════════════
   Test dialog (quick preview)
   ════════════════════════════════════════════════════════════════ */

import { useState, useEffect, useRef } from "react";
import { MessageSquare, ArrowRight } from "lucide-react";
import { clsx } from "clsx";
import { PALETTES, type AgentPersona } from "./types";
import { AgentAvatar } from "./AgentAvatar";
import { resolveProviderConfig } from "./providerConfig";
import { IconButton, Modal } from "../ui";

interface TestDialogProps {
  agent: AgentPersona;
  onClose: () => void;
}

export function TestDialog({ agent, onClose }: TestDialogProps) {
  const g = PALETTES[agent.colorTheme];
  const [messages, setMessages] = useState<Array<{ role: "user" | "agent"; text: string }>>([]);
  const [input, setInput] = useState("");
  const [streaming, setStreaming] = useState(false);
  const abortRef = useRef<AbortController | null>(null);

  // Auto-play the sample dialog
  useEffect(() => {
    if (agent.sampleDialog && messages.length === 0) {
      setMessages([
        { role: "user", text: agent.sampleDialog.q },
        { role: "agent", text: agent.sampleDialog.a },
      ]);
    }
  }, [agent.sampleDialog]);

  const send = async () => {
    const text = input.trim();
    if (!text || streaming) return;
    setInput("");
    setMessages((prev) => [...prev, { role: "user", text }]);
    setStreaming(true);
    abortRef.current = new AbortController();
    try {
      // extracted to providerConfig.ts to dedupe).
      const { apiUrl, apiKey } = await resolveProviderConfig();
      const res = await fetch(`${apiUrl}/v1/chat/completions`, {
        method: "POST",
        headers: { "Content-Type": "application/json", ...(apiKey ? { Authorization: `Bearer ${apiKey}` } : {}) },
        body: JSON.stringify({
          messages: [
            { role: "system", content: agent.systemPrompt },
            ...messages.map((m) => ({ role: m.role === "user" ? "user" : "assistant", content: m.text })),
            { role: "user", content: text },
          ],
          stream: true,
        }),
        signal: abortRef.current.signal,
      });
      if (!res.ok || !res.body) throw new Error(`API ${res.status}`);
      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let acc = "";
      setMessages((prev) => [...prev, { role: "agent", text: "" }]);
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        const chunk = decoder.decode(value, { stream: true });
        for (const line of chunk.split("\n").filter((l) => l.startsWith("data: "))) {
          const payload = line.slice(6).trim();
          if (payload === "[DONE]") continue;
          try {
            const json = JSON.parse(payload);
            const delta = json.choices?.[0]?.delta?.content || "";
            acc += delta;
            setMessages((prev) => {
              const next = [...prev];
              next[next.length - 1] = { role: "agent", text: acc };
              return next;
            });
          } catch {}
        }
      }
    } catch (e) {
      setMessages((prev) => [...prev, { role: "agent", text: `(错误:${(e as Error).message})` }]);
    } finally {
      setStreaming(false);
    }
  };

  return (
    /* 弹窗壳收编到 ui/Modal：焦点陷阱 / Esc / 焦点归还 / role=dialog +
       aria-modal 一次性补齐（此前这个弹窗连 role="dialog" 都没有）。 */
    <Modal
      open
      onClose={onClose}
      width="base"
      scrim
      height="600px"
      title={
        <span className="flex items-center gap-2.5">
          <AgentAvatar name={agent.name} colorTheme={agent.colorTheme} className="shrink-0" />
          <span className="flex min-w-0 flex-col gap-0.5">
            <span className="truncate">{agent.name}</span>
            <span className="truncate text-[length:var(--text-meta)] font-normal text-[var(--color-text-muted)]">
              {agent.role}
            </span>
          </span>
        </span>
      }
      bodyClassName="overflow-y-auto p-4 space-y-2"
      footer={
        <div className="flex w-full items-center gap-2">
          <input
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); send(); } }}
            placeholder={`问 ${agent.name} 任何问题…`}
            disabled={streaming}
            aria-label={`问 ${agent.name} 任何问题`}
            className="flex-1 rounded-lg bg-bg-tertiary px-3 py-2 text-[length:var(--text-ui)] text-text-primary placeholder:text-text-faint outline-none"
          />
          <IconButton
            icon={<ArrowRight className="icon-md" />}
            label="发送"
            shape="circle"
            size="base"
            variant="primary"
            onClick={send}
            disabled={!input.trim() || streaming}
          />
        </div>
      }
    >
      {messages.length === 0 ? (
        <div className="h-full flex flex-col items-center justify-center text-center text-text-muted">
          <MessageSquare size={28} className="mb-2 opacity-30" />
          <p className="text-[length:var(--text-meta)]">开始和 {agent.name} 聊几句</p>
        </div>
      ) : (
        messages.map((m, i) => (
          <div key={`msg-${m.role}-${i}`} className={clsx("flex", m.role === "user" ? "justify-end" : "justify-start")}>
            <div
              className={clsx(
                "max-w-[80%] rounded-xl px-3 py-2 text-[length:var(--text-ui)] leading-relaxed",
                m.role === "user"
                  ? "bg-bg-tertiary text-text-primary"
                  : "text-text-primary"
              )}
              style={m.role === "agent" ? { background: g.chip.bg } : undefined}
            >
              {m.text || (streaming && i === messages.length - 1 ? "…" : "")}
            </div>
          </div>
        ))
      )}
    </Modal>
  );
}
