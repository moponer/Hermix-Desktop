/* ════════════════════════════════════════════════════════════════
   Test dialog (quick preview)
   ════════════════════════════════════════════════════════════════ */

import { useState, useEffect, useRef } from "react";
import { X, MessageSquare, ArrowRight } from "lucide-react";
import { clsx } from "clsx";
import { PALETTES, type AgentPersona } from "./types";
import { resolveProviderConfig } from "./providerConfig";

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
      // P0-6: Same auto-detect logic as runGeneration (provider map
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
    <div className="fixed inset-0 z-50 flex items-center justify-center p-4" style={{ background: "var(--color-overlay-scrim)" }} onClick={onClose}>
      <div
        onClick={(e) => e.stopPropagation()}
        className="w-full max-w-md h-[600px] max-h-[80vh] rounded-2xl bg-bg-card overflow-hidden flex flex-col shadow-2xl"
      >
        {/* Header */}
        <div className="flex items-center gap-2.5 p-3.5">
          <div
            className="w-9 h-9 rounded-lg flex items-center justify-center text-lg shrink-0"
            style={{ background: g.avatar }}
          >
            {agent.avatar}
          </div>
          <div className="min-w-0 flex-1">
            <p className="text-[13px] font-semibold text-text-primary truncate">{agent.name}</p>
            <p className="text-[10.5px] text-text-muted truncate">{agent.role}</p>
          </div>
          <button onClick={onClose} aria-label="关闭" className="p-1.5 rounded-md text-text-muted hover:text-text-primary hover:bg-bg-hover">
            <X size={14} />
          </button>
        </div>

        {/* Messages */}
        <div className="flex-1 overflow-y-auto p-3.5 space-y-2">
          {messages.length === 0 ? (
            <div className="h-full flex flex-col items-center justify-center text-center text-text-muted">
              <MessageSquare size={28} className="mb-2 opacity-30" />
              <p className="text-[12px]">开始和 {agent.name} 聊几句</p>
            </div>
          ) : (
            messages.map((m, i) => (
              <div key={`msg-${m.role}-${i}`} className={clsx("flex", m.role === "user" ? "justify-end" : "justify-start")}>
                <div
                  className={clsx(
                    "max-w-[80%] rounded-xl px-3 py-2 text-[12.5px] leading-relaxed",
                    m.role === "user"
                      ? "bg-bg-tertiary text-text-primary"
                      : "text-text-primary"
                  )}
                  style={m.role === "agent" ? { background: g.chip.bg } : undefined}
                >
                  {m.text || (streaming && i === messages.length - 1 ? "..." : "")}
                </div>
              </div>
            ))
          )}
        </div>

        {/* Input */}
        <div className="p-3 flex gap-2">
          <input
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); send(); }}}
            placeholder={`问 ${agent.name} 任何问题...`}
            disabled={streaming}
            className="flex-1 rounded-lg bg-bg-tertiary px-3 py-2 text-[12.5px] text-text-primary placeholder:text-text-faint outline-none"
          />
          <button
            onClick={send}
            disabled={!input.trim() || streaming}
            aria-label="发送"
            className="p-2 rounded-lg disabled:opacity-40"
            style={{ background: "var(--color-btn-primary-bg)", color: "var(--color-btn-primary-text)" }}
          >
            <ArrowRight size={14} />
          </button>
        </div>
      </div>
    </div>
  );
}