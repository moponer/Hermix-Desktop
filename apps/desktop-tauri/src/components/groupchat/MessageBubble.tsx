/**
 * Message bubble component — renders a single chat message with
 * type-specific styling (user / leader / member / system).
 *
 * Also includes DispatchErrorCard and HandoffSeparator for the
 * new refactor events (DispatchFailed, Handoff).
 *
 * Extracted from GroupChatView.tsx.
 */

import { useState, memo } from "react";
import { Crown, Copy, AlertCircle, ArrowRightCircle } from "lucide-react";
import { clsx } from "clsx";
import type { AgentPersona } from "../agents/types";
import { MarkdownBody } from "../MarkdownBody";
import { AgentAvatar } from "./AgentAvatar";
import { agentTone, LEADER_SENDER_ID, SYSTEM_SENDER_ID, withAlpha, type ChatMsg } from "./types";
import type * as api from "@/api";

/* ── Dispatch error card ──
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

/* ── Handoff separator ──
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

/* ── Task completed ribbon ──
 * Centered chip rendered when the room_completed event arrives.
 * High-signal marker that the in-flight task finished — the user
 * doesn't have to open the TaskPanel to see the outcome.
 */
export const TaskCompletedCard = memo(function TaskCompletedCard() {
  return (
    <div className="flex items-center gap-2 py-1.5 animate-in">
      <div className="flex-1" />
      <div
        className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-[12px] font-medium shrink-0"
        style={{
          background: "color-mix(in srgb, var(--color-success, #16a34a) 12%, transparent)",
          color: "var(--color-success-text, #15803d)",
          border: "1px solid color-mix(in srgb, var(--color-success, #16a34a) 30%, transparent)",
        }}
        data-testid="room-task-completed-card"
      >
        <span className="w-1.5 h-1.5 rounded-full" style={{ background: "var(--color-success, #16a34a)" }} />
        <span>任务已完成 · 总管 已整理</span>
      </div>
      <div className="flex-1" />
    </div>
  );
});

/* ── Agent expelled card ──
 * Inline warning when a member is removed by the auditor. Mirrors
 * DispatchErrorCard visual style but communicates a peer-level
 * status change (not a one-off dispatch failure).
 */
export const AgentExpelledCard = memo(function AgentExpelledCard({
  agentName,
  reason,
}: {
  agentName: string;
  reason: string;
}) {
  return (
    <div className="flex justify-center animate-in py-1">
      <div
        className="inline-flex items-start gap-2.5 px-4 py-2.5 rounded-xl border max-w-[80%]"
        style={{
          background: "var(--color-warning-bg, rgba(217,119,6,0.10))",
          borderColor: "rgba(217,119,6,0.30)",
        }}
        data-testid="room-agent-expelled-card"
      >
        <AlertCircle
          size={14}
          className="shrink-0 mt-0.5"
          style={{ color: "var(--color-warning-text, #b45309)" }}
        />
        <div className="min-w-0 flex-1">
          <p
            className="text-[13px] font-medium"
            style={{ color: "var(--color-warning-text, #b45309)" }}
          >
            {agentName || "成员"} 已被移出房间
          </p>
          {reason && (
            <p className="text-[12px] text-text-muted mt-0.5 break-words">
              {reason}
            </p>
          )}
        </div>
      </div>
    </div>
  );
});

/* ── Audit verdict chip ──
 * Inline notice when an agent triggers a `violation`-level audit
 * verdict. Lower-severity verdicts (info/warning) skip the chip
 * to avoid flooding the stream.
 */
export const AuditVerdictCard = memo(function AuditVerdictCard({
  agentName,
  reason,
  scoreDelta,
}: {
  agentName: string;
  reason: string;
  scoreDelta?: number;
}) {
  const deltaText =
    typeof scoreDelta === "number" && scoreDelta !== 0
      ? ` · ${scoreDelta > 0 ? "+" : ""}${scoreDelta} 分`
      : "";
  return (
    <div className="flex items-center gap-2 py-1 animate-in">
      <div className="flex-1" />
      <div
        className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-[12px] font-medium shrink-0 max-w-[80%]"
        style={{
          background: "var(--color-error-bg, rgba(239,68,68,0.10))",
          color: "var(--color-error-text, #b91c1c)",
        }}
        title={reason}
        data-testid="room-audit-verdict-card"
      >
        <AlertCircle size={11} />
        <span className="truncate">
          {agentName || "成员"} 触发违规{deltaText}
        </span>
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

/* ── Sub-bubbles ──
 * MessageBubble is split into memoised sub-bubbles (system / leader /
 * user-or-member). Each branch has its own prop surface but shares only
 * the dispatch logic, so the parent does sender resolution + prop
 * computation, then hands a flat prop bag to one of the three
 * subcomponents. Re-renders of a streamed leader chunk
 * no longer recompute the (more common) member bubble.
 */

/** Centered hint shown for system-injected messages (fleet failure). */
const SystemHintBubble = memo(function SystemHintBubble({
  msg,
}: {
  msg: ChatMsg;
}) {
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
});

/** Left-aligned leader bubble — crown avatar, 总管 chip, gold tint. */
const LeaderBubble = memo(function LeaderBubble({
  msg,
  displayName,
}: {
  msg: ChatMsg;
  displayName: string;
  compact?: boolean;
}) {
  const tone = agentTone(LEADER_SENDER_ID);
  return (
    <div className="flex gap-2 items-start animate-in">
      <div
        className="w-6 h-6 rounded-full flex items-center justify-center shrink-0"
        style={{
          background: "hsla(33, 90%, 90%, 0.6)",
          color: "hsl(33, 90%, 38%)",
        }}
        title="项目总管"
      >
        <Crown size={12} strokeWidth={2.4} />
      </div>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-1.5 mb-0.5">
          <span className="text-[13px] text-text-primary font-medium">
            {displayName}
          </span>
          <span
            className="text-[11px] px-1 py-px rounded font-medium"
            style={{
              background: "hsla(43, 96%, 56%, 0.18)",
              color: "hsl(33, 90%, 38%)",
            }}
          >
            总管
          </span>
          <span className="text-[11px] text-text-faint ml-auto">
            <MessageMeta msg={msg} />
          </span>
        </div>
        <div
          className="rounded-lg px-3 py-1.5 text-[13px] leading-snug whitespace-pre-wrap break-words"
          style={{
            background: tone.bg,
            color: "var(--color-text-primary)",
            boxShadow: `inset 0 0 0 1px ${withAlpha(tone.ring, 0.15)}`,
          }}
        >
          <MarkdownBody content={msg.content} />
          {msg.metadata && <RawContentToggle metadata={msg.metadata} />}
        </div>
      </div>
    </div>
  );
});

/** Right-aligned user / left-aligned member bubble. */
const MemberBubble = memo(function MemberBubble({
  msg,
  isUser,
  resolvedAgent,
  displayName,
}: {
  msg: ChatMsg;
  isUser: boolean;
  resolvedAgent: AgentPersona | null;
  displayName: string;
  memberRank?: api.GroupChatRank | undefined;
  memberModelOverride?: string | undefined;
  compact?: boolean;
}) {
  const tone = resolvedAgent ? agentTone(resolvedAgent.id) : null;
  return (
    <div
      className={clsx("flex gap-2 items-start animate-in", isUser && "justify-end")}
    >
      {!isUser && resolvedAgent && <AgentAvatar agent={resolvedAgent} size={24} />}
      <div className={clsx("max-w-[80%]", "flex flex-col", isUser && "items-end")}>
        {!isUser && (
          <div className="flex items-center gap-1.5 mb-0.5 px-1">
            <span
              className="text-[12px] font-medium truncate max-w-[140px]"
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
            "rounded-lg px-3 py-1.5 text-[13px] leading-snug",
            isUser ? "rounded-br-sm" : "rounded-bl-sm",
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
                    boxShadow: `inset 0 0 0 1px ${withAlpha(tone.ring, 0.15)}`,
                  }
                : {
                    background: "var(--color-bg-tertiary)",
                    color: "var(--color-text-primary)",
                  }
          }
        >
          <MarkdownBody content={msg.content} />
          {!isUser && msg.metadata && <RawContentToggle metadata={msg.metadata} />}
        </div>
        {isUser && (
          <span className="text-[11px] text-text-faint mt-0.5 px-1">
            <MessageMeta msg={msg} />
          </span>
        )}
      </div>
    </div>
  );
});

/** Dispatcher: resolve sender, compute derived props, route to the
 *  right sub-bubble. Kept memoised so unrelated state changes (e.g.
 *  the room list being reloaded) don't recompute the dispatch logic. */
export const MessageBubble = memo(function MessageBubble({
  msg,
  agents,
  ranks: _ranks,
  modelOverrides: _modelOverrides,
  compact: _compact = false,
}: {
  msg: ChatMsg;
  agents: AgentPersona[];
  ranks?: api.GroupChatRank[];
  modelOverrides?: Record<string, string | undefined>;
  compact?: boolean;
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

  if (isSystem) return <SystemHintBubble msg={msg} />;
  if (isLeader) return <LeaderBubble msg={msg} displayName={displayName} />;
  return (
    <MemberBubble
      msg={msg}
      isUser={isUser}
      resolvedAgent={resolvedAgent ?? null}
      displayName={displayName}
    />
  );
});
