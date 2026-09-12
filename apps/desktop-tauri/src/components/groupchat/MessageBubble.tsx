/**
 * Message bubble component — renders a single chat message.
 *
 * Supports consecutive message grouping: messages from the same sender
 * within 5 minutes are collapsed (no repeated avatar/name).
 */

import { useState, memo } from "react";
import { Crown, Copy, AlertCircle, ArrowRightCircle } from "lucide-react";
import { clsx } from "clsx";
import type { AgentPersona } from "../agents/types";
import { MarkdownBody } from "../MarkdownBody";
import { AgentAvatar } from "./AgentAvatar";
import { agentTone, LEADER_SENDER_ID, SYSTEM_SENDER_ID, withAlpha, type ChatMsg } from "./types";
import type * as api from "@/api";

const CONSECUTIVE_TIMEOUT_MS = 5 * 60 * 1000; // 5 minutes

function isConsecutiveMsg(prev: ChatMsg | undefined, curr: ChatMsg): boolean {
  if (!prev) return false;
  return (
    prev.senderId === curr.senderId &&
    curr.timestamp - prev.timestamp < CONSECUTIVE_TIMEOUT_MS
  );
}

/* ── Dispatch error card ── */
export const DispatchErrorCard = memo(function DispatchErrorCard({
  agentName, reason, isLeader, onRetry,
}: {
  agentName: string; reason: string; isLeader: boolean; onRetry?: () => void;
}) {
  return (
    <div className="flex justify-center animate-in py-1">
      <div className="inline-flex items-start gap-2.5 px-4 py-2.5 rounded-xl border max-w-[80%]"
        style={{ background: "var(--color-error-soft)", borderColor: "var(--color-error-soft)" }}>
        <AlertCircle className="icon-md shrink-0 mt-0.5" style={{ color: "var(--color-error)" }} />
        <div className="min-w-0 flex-1">
          <p className="text-[13px] font-medium" style={{ color: "var(--color-error)" }}>
            {isLeader ? "总管调度失败" : `${agentName} 回复失败`}
          </p>
          <p className="text-[12px] text-text-muted mt-0.5 break-words">{reason}</p>
          {onRetry && (
            <button type="button" onClick={onRetry}
              className="mt-1.5 text-[12px] font-medium text-text-secondary hover:text-text-primary transition-colors underline underline-offset-2">
              重试
            </button>
          )}
        </div>
      </div>
    </div>
  );
});

/* ── Handoff separator ── */
export const HandoffSeparator = memo(function HandoffSeparator({
  toAgentName, reason, kind,
}: {
  toAgentName: string; reason: string; kind: string;
}) {
  const label = kind === "transfer" ? `已转交 ${toAgentName} 接管` : `委托 ${toAgentName} 协助`;
  return (
    <div className="flex items-center gap-2 py-1.5 animate-in">
      <div className="flex-1" />
      <div className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-[12px] font-medium shrink-0"
        style={{ background: "var(--color-bg-tertiary)", color: "var(--color-text-muted)" }}
        title={reason}>
        <ArrowRightCircle className="icon-sm" />
        <span className="truncate">{label}</span>
      </div>
      <div className="flex-1" />
    </div>
  );
});

/* ── Task completed ribbon ── */
export const TaskCompletedCard = memo(function TaskCompletedCard() {
  return (
    <div className="flex items-center gap-2 py-1.5 animate-in">
      <div className="flex-1" />
      <div className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-[12px] font-medium shrink-0"
        style={{
          background: "var(--color-success-soft)",
          color: "var(--color-success)",
          border: "1px solid var(--color-success-soft)",
        }}
        data-testid="room-task-completed-card">
        <span className="w-1.5 h-1.5 rounded-full" style={{ background: "var(--color-success)" }} />
        <span>任务已完成 · 总管已整理</span>
      </div>
      <div className="flex-1" />
    </div>
  );
});

/* ── Agent expelled card ── */
export const AgentExpelledCard = memo(function AgentExpelledCard({
  agentName, reason,
}: {
  agentName: string; reason: string;
}) {
  return (
    <div className="flex justify-center animate-in py-1">
      <div className="inline-flex items-start gap-2.5 px-4 py-2.5 rounded-xl border max-w-[80%]"
        style={{ background: "var(--color-warning-soft)", borderColor: "var(--color-warning-soft)" }}
        data-testid="room-agent-expelled-card">
        <AlertCircle className="icon-md shrink-0 mt-0.5" style={{ color: "var(--color-warning)" }} />
        <div className="min-w-0 flex-1">
          <p className="text-[13px] font-medium" style={{ color: "var(--color-warning)" }}>
            {agentName || "成员"} 已被移出房间
          </p>
          {reason && <p className="text-[12px] text-text-muted mt-0.5 break-words">{reason}</p>}
        </div>
      </div>
    </div>
  );
});

/* ── Audit verdict chip ── */
export const AuditVerdictCard = memo(function AuditVerdictCard({
  agentName, reason, scoreDelta,
}: {
  agentName: string; reason: string; scoreDelta?: number;
}) {
  const deltaText = typeof scoreDelta === "number" && scoreDelta !== 0
    ? ` · ${scoreDelta > 0 ? "+" : ""}${scoreDelta} 分` : "";
  return (
    <div className="flex items-center gap-2 py-1 animate-in">
      <div className="flex-1" />
      <div className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-[12px] font-medium shrink-0 max-w-[80%]"
        style={{ background: "var(--color-error-soft)", color: "var(--color-error)" }}
        title={reason} data-testid="room-audit-verdict-card">
        <AlertCircle className="icon-sm" />
        <span className="truncate">{agentName || "成员"} 触发违规{deltaText}</span>
      </div>
      <div className="flex-1" />
    </div>
  );
});

/* ── Raw content toggle ── */
function RawContentToggle({ metadata }: { metadata: string }) {
  const [show, setShow] = useState(false);
  return (
    <div className="mt-1.5 pt-1.5">
      <button type="button" onClick={() => setShow((v) => !v)}
        className="text-[12px] text-text-faint hover:text-text-secondary transition-colors">
        {show ? "收起思考过程" : "展开思考过程"}
      </button>
      {show && (
        <pre className="mt-1.5 text-[12px] leading-snug text-text-muted whitespace-pre-wrap break-words overflow-x-auto max-h-40 overflow-y-auto rounded-lg bg-bg-tertiary p-2">
          {(() => {
            try {
              const parsed = JSON.parse(metadata);
              return parsed.raw ?? "（无原始内容）";
            } catch {
              return "（无法解析）";
            }
          })()}
        </pre>
      )}
    </div>
  );
}

/* ── Time + copy row ── */
function MessageMeta({ msg, showTime = true }: { msg: ChatMsg; showTime?: boolean }) {
  const time = new Date(msg.timestamp).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" });
  return (
    <span className="text-[11px] text-text-faint flex items-center gap-1">
      <button type="button" className="hover:text-text-secondary transition-colors"
        title="复制消息"
        onClick={() => navigator.clipboard.writeText(msg.content)}>
        <Copy size={10} />
      </button>
      {showTime && time}
    </span>
  );
}

/* ── System hint bubble ── */
const SystemHintBubble = memo(function SystemHintBubble({ msg }: { msg: ChatMsg }) {
  return (
    <div className="flex justify-center animate-in pt-1">
      <div className="inline-flex items-center gap-2 px-3.5 py-2 rounded-xl border"
        style={{ background: "var(--color-bg-tertiary)", borderColor: "var(--color-border-subtle)" }}>
        <span className="w-5 h-5 rounded-full flex items-center justify-center shrink-0 bg-text-faint/20">
          <span className="text-[12px] leading-none font-bold" style={{ opacity: 0.5 }}>!</span>
        </span>
        <span className="text-[13px] leading-relaxed text-text-faint text-center">{msg.content}</span>
      </div>
    </div>
  );
});

/* ── Leader bubble — uses accent color for crown chip & tint ── */
const LeaderBubble = memo(function LeaderBubble({
  msg, displayName, isConsecutive,
}: {
  msg: ChatMsg; displayName: string; isConsecutive: boolean;
}) {
  const tone = agentTone(LEADER_SENDER_ID);
  return (
    <div className={clsx("flex gap-2.5 items-start animate-in", isConsecutive && "mt-0.5")}>
      {/* Avatar — hidden for consecutive */}
      {!isConsecutive && (
        <div className="w-7 h-7 rounded-full flex items-center justify-center shrink-0"
          style={{ background: "var(--color-accent-soft)", color: "var(--color-accent)" }}
          title="项目总管">
          <Crown className="icon-md" strokeWidth={2.4} />
        </div>
      )}
      <div className={clsx("min-w-0 flex-1", isConsecutive && "mt-1")}>
        {/* Name + badge — hidden for consecutive */}
        {!isConsecutive && (
          <div className="flex items-center gap-1.5 mb-1">
            <span className="text-[13px] text-text-primary font-medium">{displayName}</span>
            <span className="text-[11px] px-1.5 py-0.5 rounded font-medium"
              style={{ background: "var(--color-accent-soft)", color: "var(--color-accent)" }}>
              总管
            </span>
            <span className="ml-auto">
              <MessageMeta msg={msg} />
            </span>
          </div>
        )}
        {/* Bubble */}
        <div className="rounded-lg px-3 py-2 text-[13px] leading-relaxed whitespace-pre-wrap break-words"
          style={{
            background: tone.bg,
            color: "var(--color-text-primary)",
            boxShadow: `inset 0 0 0 1px ${withAlpha(tone.ring, 0.15)}`,
          }}>
          <MarkdownBody content={msg.content} />
          {msg.metadata && <RawContentToggle metadata={msg.metadata} />}
        </div>
      </div>
    </div>
  );
});

/* ── Member / user bubble ── */
const MemberBubble = memo(function MemberBubble({
  msg, isUser, resolvedAgent, displayName, isConsecutive,
}: {
  msg: ChatMsg; isUser: boolean;
  resolvedAgent: AgentPersona | null; displayName: string; isConsecutive: boolean;
}) {
  const tone = resolvedAgent ? agentTone(resolvedAgent.id) : null;
  return (
    <div className={clsx(
      "flex gap-2.5 items-start animate-in",
      isUser ? "justify-end" : "",
      isConsecutive && (isUser ? "" : "mt-0.5"),
    )}>
      {/* Avatar — hidden for consecutive (non-user) */}
      {!isUser && !isConsecutive && resolvedAgent && (
        <AgentAvatar agent={resolvedAgent} size={28} />
      )}
      {/* Right-aligned wrapper for user */}
      {isUser && (
        <div className={clsx("max-w-[75%] flex flex-col", isConsecutive && "mt-1")}>
          {!isConsecutive && (
            <div className="flex items-center gap-1.5 mb-1 justify-end">
              <span className="text-[12px] text-text-primary font-medium">你</span>
              <MessageMeta msg={msg} />
            </div>
          )}
          <div
            className="rounded-lg rounded-tr-sm px-3 py-2 text-[13px] leading-relaxed whitespace-pre-wrap break-words"
            style={{
              background: "var(--color-accent-soft)",
              color: "var(--color-text-primary)",
              border: "1px solid var(--color-accent-soft-strong)",
            }}
          >
            <MarkdownBody content={msg.content} />
            {msg.metadata && <RawContentToggle metadata={msg.metadata} />}
          </div>
          {isConsecutive && (
            <div className="flex items-center gap-1.5 mt-1 justify-end">
              <MessageMeta msg={msg} />
            </div>
          )}
        </div>
      )}
      {/* Left-aligned wrapper for member */}
      {!isUser && (
        <div className={clsx("max-w-[75%] flex flex-col", isConsecutive && "mt-1")}>
          {/* Name + time — hidden for consecutive */}
          {!isConsecutive && (
            <div className="flex items-center gap-1.5 mb-1 px-1">
              <span className="text-[12px] font-medium truncate max-w-[120px]"
                style={{ color: tone?.text ?? "var(--color-text-secondary)" }}
                title={displayName}>{displayName}</span>
              <MessageMeta msg={msg} showTime />
            </div>
          )}
          {/* Bubble */}
          <div
            className={clsx(
              "rounded-lg px-3 py-2 text-[13px] leading-relaxed",
              isConsecutive ? "" : "rounded-bl-sm",
            )}
            style={tone
              ? { background: tone.bg, color: "var(--color-text-primary)", boxShadow: `inset 0 0 0 1px ${withAlpha(tone.ring, 0.15)}` }
              : { background: "var(--color-bg-tertiary)", color: "var(--color-text-primary)" }
            }
          >
            <MarkdownBody content={msg.content} />
            {!isUser && msg.metadata && <RawContentToggle metadata={msg.metadata} />}
          </div>
          {/* Time for consecutive */}
          {isConsecutive && (
            <div className="flex items-center gap-1.5 mt-1 px-1">
              <MessageMeta msg={msg} showTime />
            </div>
          )}
        </div>
      )}
    </div>
  );
});

/* ── Dispatcher ── */
export const MessageBubble = memo(function MessageBubble({
  msg, agents, ranks: _ranks, modelOverrides: _modelOverrides, compact: _compact = false,
  prevMsg,
}: {
  msg: ChatMsg; agents: AgentPersona[];
  ranks?: api.GroupChatRank[];
  modelOverrides?: Record<string, string | undefined>;
  compact?: boolean;
  prevMsg?: ChatMsg;
}) {
  const isUser = msg.senderId === "user";
  const isLeader = msg.senderId === LEADER_SENDER_ID;
  const isSystem = msg.senderId === SYSTEM_SENDER_ID;
  const isConsecutive = isConsecutiveMsg(prevMsg, msg);

  const resolvedAgent = !isUser && !isLeader
    ? agents.find((a) =>
        a.id === msg.senderId || `builtin:${a.id}` === msg.senderId || a.id === `builtin:${msg.senderId}`,
      )
    : null;

  const displayName = isLeader
    ? "项目总管"
    : resolvedAgent
      ? resolvedAgent.name || resolvedAgent.role || msg.senderName
      : msg.senderName;

  if (isSystem) return <SystemHintBubble msg={msg} />;
  if (isLeader) return <LeaderBubble msg={msg} displayName={displayName} isConsecutive={isConsecutive} />;
  return <MemberBubble msg={msg} isUser={isUser} resolvedAgent={resolvedAgent ?? null} displayName={displayName} isConsecutive={isConsecutive} />;
});
