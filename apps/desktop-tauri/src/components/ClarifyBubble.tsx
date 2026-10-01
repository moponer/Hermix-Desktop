// Clarify card (role="clarify") + its composer — extracted from MessageBubble.tsx.
import { useState } from "react";
import { HelpCircle, AlertCircle } from "lucide-react";
import { formatRelativeTime } from "../utils/time";
import type { ChatMessage, ClarifyMessageMeta } from "../types";
export function ClarifyCard({
  message,
  onAnswer,
}: {
  message: ChatMessage;
  onAnswer?: (requestId: string, answer: string) => void;
}) {
  const meta = message.clarify;
  if (!meta) {
    // Defensive: a `role: "clarify"` message without meta is a server
    // contract violation. Show a small banner rather than a silent gap.
    return (
      <div className="h-enter flex items-center gap-2 text-meta text-text-faint italic">
        <AlertCircle className="icon-sm" />
        <span>澄清请求数据缺失</span>
      </div>
    );
  }
  const answered = meta.answer != null && meta.answer !== "";
  // 已答复的询问卡直接消失，不留"已答复"占位（与审批卡一致）。
  if (answered) return null;

  return (
    <div className="flex gap-2.5 h-enter items-start">
      <div
        className="shrink-0 mt-0.5 w-[22px] h-[22px] rounded-md flex items-center justify-center"
        style={{ background: "color-mix(in srgb, var(--color-info) 12%, transparent)" }}
        title="需要确认"
      >
        <HelpCircle className="icon-sm text-accent" />
      </div>
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-1.5 mb-1 flex-wrap">
          <span className="text-ui font-semibold text-text-primary">需要您确认</span>
          <span className="h-tiny">· {formatRelativeTime(message.timestamp)}</span>
          {answered && <span className="pill pill-success">已答复</span>}
        </div>

        <div className="msg-card msg-stack">
          <div className="msg-channel" data-channel="clarify-question">
            <div
              className="prose max-w-none text-ui text-text-primary whitespace-pre-wrap"
              // Cap a runaway server-side question (>2k chars) inside a
              // scrollable container so it can't push the rest of the
              // timeline out of the viewport.
              style={{ maxHeight: "240px", overflow: "auto" }}
            >
              {meta.question?.trim() || (
                <span className="h-tiny italic text-text-faint">（空问题）</span>
              )}
            </div>
          </div>

          <ClarifyComposer
            meta={meta}
            onSubmit={(answer) => onAnswer?.(meta.requestId, answer)}
          />
        </div>
      </div>
    </div>
  );
}

export function ClarifyComposer({
  meta,
  onSubmit,
}: {
  meta: ClarifyMessageMeta;
  onSubmit: (answer: string) => void;
}) {
  const [text, setText] = useState("");
  const hasChoices = meta.choices && meta.choices.length > 0;
  const canSubmit = text.trim().length > 0;

  return (
    <div className="msg-channel" data-channel="clarify-input">
      {hasChoices && (
        <div className="flex flex-wrap gap-1.5 mb-2">
          {meta.choices.map((c, i) => (
            <button
              key={`choice-${c}-${i}`}
              type="button"
              onClick={() => onSubmit(c)}
              className="approval-btn approval-btn-secondary"
              style={{ height: 26, padding: "0 10px", fontSize: 11.5 }}
            >
              {c}
            </button>
          ))}
        </div>
      )}
      <div className="flex items-center gap-2">
        <input
          type="text"
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && canSubmit) {
              e.preventDefault();
              onSubmit(text.trim());
              setText("");
            }
          }}
          placeholder={hasChoices ? "或直接输入自定义回答" : "请输入您的回答"}
          className="flex-1 px-2.5 py-1.5 rounded-md text-ui outline-none"
          style={{
            background: "var(--color-bg-tertiary)",
            color: "var(--color-text-primary)",
            border: "1px solid var(--color-border-primary)",
          }}
        />
        <button
          type="button"
          disabled={!canSubmit}
          onClick={() => {
            if (!canSubmit) return;
            onSubmit(text.trim());
            setText("");
          }}
          className="inline-flex items-center gap-1 px-2.5 py-1.5 rounded-md text-meta font-medium transition-colors disabled:opacity-30 disabled:cursor-not-allowed"
          style={{
            background: "var(--color-accent)",
            color: "var(--color-accent-on-text)",
          }}
        >
          提交
        </button>
      </div>
    </div>
  );
}
