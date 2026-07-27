/**
 * Message bubble component — renders a single chat message with
 * type-specific styling (user / leader / member / system).
 *
 * Also includes DispatchErrorCard and HandoffSeparator for the
 * new 2026-07 refactor events (DispatchFailed, Handoff).
 *
 * Extracted from GroupChatView.tsx during the 2026-07 refactor.
 */

import { useState, memo } from "react";
import { Crown, Copy, AlertCircle, ArrowRightCircle } from "lucide-react";
import { clsx } from "clsx";
import type { AgentPersona } from "../agents/types";
import { MarkdownBody } from "../MarkdownBody";
import { AgentAvatar } from "./AgentAvatar";
import { agentTone, LEADER_SENDER_ID, SYSTEM_SENDER_ID, type ChatMsg } from "./types";

/* ── Dispatch error card (2026-07 refactor) ──
 * Inline error card shown when a dispatch fails. The user sees
 * *why* nobody replied instead of an empty response.
 */
export const DispatchErrorCard = memo(function DispatchErrorCard({
  agentName,
  reason,
  isLeader,
  onRetry,
}: {
  agentName: string;
  reason: string;
  isLeader: boolean;
  onRetry?: () => void;
}) {
  return (
    <div className="flex justify-center animate-in py-1">
      <div
        className="inline-flex items-start gap-2.5 px-4 py-2.5 rounded-xl border max-w-[80%]"
        style={{
          background: "var(--color-error-bg, rgba(239,68,68,0.08))",
          borderColor: "rgba(239,68,68,0.25)",
        }}
      >
        <AlertCircle
          size={14}
          className="shrink-0 mt-0.5"
          style={{ color: "var(--color-error-text, #b91c1c)" }}
        />
        <div className="min-w-0 flex-1">
          <p
            className="text-[13px] font-medium"
            style={{ color: "var(--color-error-text, #b91c1c)" }}
          >
            {isLeader ? "总管调度失败" : `${agentName} 回复失败`}
          </p>
          <p className="text-[12px] text-text-muted mt-0.5 break-words">
            {reason}
          </p>
          {onRetry && (
            <button
              type="button"
              onClick={onRetry}
              className="mt-1.5 text-[12px] font-medium text-text-secondary hover:text-text-primary transition-colors underline underline-offset-2"
            >
              重试
            </button>
          )}
        </div>
      </div>
    </div>
  );
});

/* ── Handoff separator (2026-07 refactor) ──
 * Centered divider shown when the leader hands off control to a
 * specialist agent. Renders "已转交 XX 接管" so the user
 * understands the conversation changed hands.
 */
export const HandoffSeparator = memo(function HandoffSeparator({
  toAgentName,
  reason,
  kind,
}: {
  toAgentName: string;
  reason: string;
  kind: string;
}) {
  const label =
    kind === "transfer"
      ? `已转交 ${toAgentName} 接管`
      : `委托 ${toAgentName} 协助`;
  return (
    <div className="flex items-center gap-2 py-1.5 animate-in">
      <div className="flex-1" />
      <div
        className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-[12px] font-medium shrink-0"
        style={{
          background: "var(--color-bg-tertiary)",
          color: "var(--color-text-muted)",
        }}
        title={reason}
      >
        <ArrowRightCircle size={11} />
        <span>{label}</span>
      </div>
      <div className="flex-1" />
    </div>
  );
});

/* ── Raw content toggle ──
 * Collapsible section for agent messages with metadata containing
 * the unstripped `raw` output (includes thinking blocks).
 */
function RawContentToggle({ metadata }: { metadata: string }) {
  const [show, setShow] = useState(false);
  return (
    <div className="mt-1.5 pt-1.5">
      <button
        type="button"
        onClick={() => setShow((v) => !v)}
        className="text-[12px] text-text-faint hover:text-text-secondary transition-colors"
      >
        {show ? "收起思考过程" : "展开思考过程"}
      </button>
      {show && (
        <pre className="mt-1.5 text-[12px] leading-snug text-text-muted whitespace-pre-wrap break-words overflow-x-auto max-h-40 overflow-y-auto rounded-lg bg-bg-tertiary p-2">
          {(() => {
            try {
              const parsed = JSON.parse(metadata);
              return parsed.raw ?? "(无原始内容)";
            } catch {
              return "(无法解析)";
            }
          })()}
        </pre>
      )}
    </div>
  );
}

/* ── Time + copy row ── */
function MessageMeta({ msg }: { msg: ChatMsg }) {
  return (
    <span className="text-[12px] text-text-faint flex items-center gap-1">
      <button
        type="button"
        className="hover:text-text-secondary transition-colors"
        title="复制消息"
        onClick={() => navigator.clipboard.writeText(msg.content)}
      >
        <Copy size={10} />
      </button>
      {new Date(msg.timestamp).toLocaleTimeString("zh-CN", {
        hour: "2-digit",
        minute: "2-digit",
      })}
    </span>
  );
}

/* ── Main message bubble ──
 * Handles 4 message types with distinct visual treatment:
 * - user: right-aligned, no avatar, transparent bg
 * - leader: left-aligned, crown avatar, gold "总管" chip, subtle tint
 * - system: centered, grey toast (fleet failure hint)
 * - member: left-aligned, agent avatar, hue-tinted bubble
 */
export const MessageBubble = memo(function MessageBubble({
  msg,
  agents,
}: {
  msg: ChatMsg;
  agents: AgentPersona[];
}) {
  const isUser = msg.senderId === "user";
  const isLeader = msg.senderId === LEADER_SENDER_ID;
  const isSystem = msg.senderId === SYSTEM_SENDER_ID;

  const resolvedAgent = !isUser && !isLeader
    ? agents.find(
        (a) =>
          a.id === msg.senderId ||
          `builtin:${a.id}` === msg.senderId ||
          a.id === `builtin:${msg.senderId}`,
      )
    : null;

  const displayName = isLeader
    ? "项目总管"
    : resolvedAgent
      ? resolvedAgent.name || resolvedAgent.role || msg.senderName
      : msg.senderName;

  const tone = resolvedAgent ? agentTone(resolvedAgent.id) : null;

  // ── System hint (fleet failure) ──
  if (isSystem) {
    return (
      <div className="flex justify-center animate-in pt-1">
        <div
          className="inline-flex items-center gap-2 px-3.5 py-2 rounded-xl border"
          style={{
            background: "var(--color-bg-tertiary)",
            borderColor: "var(--color-border-subtle)",
          }}
        >
          <span className="w-5 h-5 rounded-full flex items-center justify-center shrink-0 bg-text-faint/20">
            <span
              className="text-[12px] leading-none font-bold"
              style={{ opacity: 0.5 }}
            >
              !
            </span>
          </span>
          <span className="text-[13px] leading-relaxed text-text-faint text-center">
            {msg.content}
          </span>
        </div>
      </div>
    );
  }

  // ── Leader message ──
  if (isLeader) {
    const leaderTone = agentTone(LEADER_SENDER_ID);
    return (
      <div className="flex gap-2 items-start animate-in">
        <div
          className="w-7 h-7 rounded-full flex items-center justify-center shrink-0"
          style={{
            background: "hsla(33, 90%, 90%, 0.6)",
            color: "hsl(33, 90%, 38%)",
          }}
          title="项目总管"
        >
          <Crown size={14} strokeWidth={2.4} />
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-1.5 mb-0.5">
            <span className="text-[13px] text-text-primary font-medium">
              {displayName}
            </span>
            <span
              className="text-[12px] uppercase tracking-wider px-1 py-px rounded font-semibold"
              style={{
                background: "hsla(43, 96%, 56%, 0.18)",
                color: "hsl(33, 90%, 38%)",
              }}
            >
              总管
            </span>
            <span className="text-[12px] text-text-faint ml-auto">
              <MessageMeta msg={msg} />
            </span>
          </div>
          <div
            className="rounded-xl px-3.5 py-2 text-[14px] leading-snug whitespace-pre-wrap break-words"
            style={{
              background: leaderTone.bg,
              color: "var(--color-text-primary)",
              boxShadow: `inset 0 0 0 1px ${leaderTone.ring}33`,
            }}
          >
            <MarkdownBody content={msg.content} />
            {msg.metadata && <RawContentToggle metadata={msg.metadata} />}
          </div>
        </div>
      </div>
    );
  }

  // ── User / member message ──
  return (
    <div
      className={clsx("flex gap-2 items-start animate-in", isUser && "justify-end")}
    >
      {!isUser && resolvedAgent && <AgentAvatar agent={resolvedAgent} size={30} />}
      <div className={clsx("max-w-[68%] flex flex-col", isUser && "items-end")}>
        {!isUser && (
          <div className="flex items-center gap-1.5 mb-1 px-1">
            <span
              className="text-[12px] font-semibold truncate max-w-[120px]"
              style={{ color: tone?.text ?? "var(--color-text-secondary)" }}
              title={displayName}
            >
              {displayName}
            </span>
            <MessageMeta msg={msg} />
          </div>
        )}
        <div
          className={clsx(
            "rounded-2xl px-3.5 py-2.5 text-[14px] leading-snug",
            isUser ? "rounded-br-md" : "rounded-bl-md",
          )}
          style={
            isUser
              ? {
                  background: "transparent",
                  color: "var(--color-text-primary)",
                }
              : tone
                ? {
                    background: tone.bg,
                    color: "var(--color-text-primary)",
                    boxShadow: `inset 0 0 0 1px ${tone.ring}33`,
                  }
                : {
                    background: "var(--color-bg-tertiary)",
                    color: "var(--color-text-primary)",
                  }
          }
        >
          <div className="text-[14px] leading-snug">
            <MarkdownBody content={msg.content} />
          </div>
          {!isUser && msg.metadata && <RawContentToggle metadata={msg.metadata} />}
        </div>
        {isUser && (
          <p className="text-[12px] text-text-faint mt-1 px-1">
            <MessageMeta msg={msg} />
          </p>
        )}
      </div>
    </div>
  );
});
