/**
 * Message list container — renders the scrollable message stream.
 *
 * All hardcoded colors replaced with theme tokens.
 */

import { useRef, useEffect, memo } from "react";
import { Users, UserPlus } from "lucide-react";
import type { AgentPersona } from "../agents/types";
import { MessageBubble, DispatchErrorCard, HandoffSeparator, TaskCompletedCard, AgentExpelledCard, AuditVerdictCard } from "./MessageBubble";
import { StreamingAgentRow } from "./TypingIndicator";
import type * as api from "@/api";
import { type ChatMsg, type AgentTone, type InlineEvent } from "./types";

export type { InlineEvent };

/** Classic IRC-style message row — uses tokens for all colors. */
const ClassicMessageRow = memo(function ClassicMessageRow({ msg }: { msg: ChatMsg }) {
  const time = new Date(msg.timestamp).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" });
  const isUser = msg.senderId === "user";
  const isSystem = msg.senderId === "__system__";

  if (isSystem) {
    return (
      <div className="text-center text-[12px] text-text-faint font-mono py-0.5">
        — {msg.content} —
      </div>
    );
  }

  // Leader name uses accent color; user uses primary; member uses secondary
  const nameColor = isUser
    ? "var(--color-text-primary)"
    : msg.senderId === "__leader__"
      ? "var(--color-accent)"
      : "var(--color-text-secondary)";

  return (
    <div className="font-mono text-[13px] leading-snug px-1 py-0.5 hover:bg-bg-hover/50 rounded">
      <span className="text-text-faint">[{time}] </span>
      <span className="font-semibold" style={{ color: nameColor }}>{msg.senderName}</span>
      <span className="text-text-faint">: </span>
      <span className="text-text-primary break-words whitespace-pre-wrap">{msg.content}</span>
    </div>
  );
});

const ClassicInlineEvent = memo(function ClassicInlineEvent({ ev }: { ev: InlineEvent }) {
  const text = (() => {
    switch (ev.kind) {
      case "dispatch_failed": return `*** ${ev.isLeader ? "总管" : ev.agentName} 回复失败: ${ev.reason}`;
      case "handoff": return `*** 已转交 ${ev.toAgentName ?? ""} 接管 (${ev.reason})`;
      case "task_completed": return `*** 任务已完成`;
      case "agent_expelled": return `*** ${ev.agentName} 已被移出房间 (${ev.reason})`;
      case "audit_verdict": return `*** ${ev.agentName} 触发违规 (${ev.reason})`;
      default: return null;
    }
  })();
  if (!text) return null;
  return (
    <div className="font-mono text-[12px] text-text-muted italic px-1 py-0.5">{text}</div>
  );
});

const ClassicTypingRow = memo(function ClassicTypingRow({ name }: { name: string }) {
  return (
    <div className="font-mono text-[12px] text-text-faint italic px-1 py-0.5">
      * {name} 正在输入...
    </div>
  );
});

export const MessageList = memo(function MessageList({
  messages,
  agents,
  streamingAgents,
  inlineEvents,
  roomTone: _roomTone,
  roomAgents,
  hasLeader,
  roomStyle = "modern",
  ranks,
  modelOverrides,
  onRetry,
  onOpenMembers,
}: {
  messages: ChatMsg[];
  agents: AgentPersona[];
  streamingAgents: Map<string, { name: string; role: "leader" | "member" }>;
  inlineEvents: InlineEvent[];
  roomTone: AgentTone | null;
  roomAgents: AgentPersona[];
  hasLeader: boolean;
  roomStyle?: string;
  ranks?: api.GroupChatRank[];
  modelOverrides?: Record<string, string | undefined>;
  onRetry?: () => void;
  onOpenMembers?: () => void;
}) {
  const chatEndRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    chatEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages, streamingAgents, inlineEvents]);

  const isClassic = roomStyle === "classic";
  const isCompact = roomStyle === "compact";
  const compactSpacing = isCompact ? "space-y-1.5" : "space-y-2";
  const compactPadding = isCompact ? "px-4 py-2" : "px-4 py-3";

  // ── Classic style ──
  if (isClassic) {
    return (
      <div className={`flex-1 overflow-y-auto ${compactPadding} ${compactSpacing} min-h-0`}>
        {messages.length === 0 && inlineEvents.length === 0 ? (
          <div className="flex flex-col items-center justify-center h-full text-center">
            <div className="flex items-center justify-center mb-3" style={{ width: 64, height: 64 }}>
              <Users size={24} strokeWidth={1.5} style={{ color: "var(--color-accent)" }} />
            </div>
            <p className="text-[14px] font-mono text-text-muted mb-1">== 聊天室已就绪 ==</p>
            <p className="text-[12px] font-mono text-text-faint max-w-[280px]">
              {hasLeader ? "总管已在线，邀请成员开始协作" : "邀请成员后开始对话"}
            </p>
          </div>
        ) : (
          <>
            {messages.map((msg) => <ClassicMessageRow key={msg.id} msg={msg} />)}
            {inlineEvents.map((ev) => <ClassicInlineEvent key={ev.id} ev={ev} />)}
            {streamingAgents.size > 0 && Array.from(streamingAgents.entries()).map(([agentId, info]) => (
              <ClassicTypingRow key={agentId} name={info.name} />
            ))}
          </>
        )}
        <div ref={chatEndRef} className="h-0 w-0" />
      </div>
    );
  }

  // ── Modern / Compact styles ──
  return (
    <div className={`flex-1 overflow-y-auto min-h-0 ${compactPadding} ${compactSpacing}`}>
      {messages.length === 0 && inlineEvents.length === 0 ? (
        <div className="flex flex-col items-center justify-center h-full text-center" style={{ minHeight: "100%" }}>
          <div className="flex items-center justify-center mb-3" style={{ width: 64, height: 64 }}>
            <Users size={24} strokeWidth={1.5} style={{ color: "var(--color-accent)" }} />
          </div>
          <p className="text-[16px] font-semibold text-text-primary mb-2">
            {roomAgents.length > 0 ? "开始对话" : "还没有协作成员"}
          </p>
          <p className="text-[13px] text-text-secondary mb-4 max-w-[280px] leading-relaxed">
            {roomAgents.length > 0
              ? `${roomAgents.length} 位成员正在待命，发送消息开始协作`
              : "邀请成员加入后即可开始协作"}
          </p>
          {onOpenMembers && (
            <button onClick={onOpenMembers}
              className="flex items-center gap-1.5 px-5 rounded-lg text-[13px] font-medium transition-all active:scale-95"
              style={{ height: 36, background: "var(--color-accent)", color: "var(--color-accent-on-text)" }}>
              <UserPlus size={14} />邀请成员
            </button>
          )}
        </div>
      ) : (
        <>
          {messages.map((msg) => (
            <MessageBubble key={msg.id} msg={msg} agents={agents} ranks={ranks}
              modelOverrides={modelOverrides} compact={isCompact} />
          ))}
          {inlineEvents.map((ev) => {
            switch (ev.kind) {
              case "dispatch_failed":
                return <DispatchErrorCard key={ev.id} agentName={ev.agentName} reason={ev.reason}
                  isLeader={ev.isLeader} onRetry={onRetry} />;
              case "handoff":
                return <HandoffSeparator key={ev.id} toAgentName={ev.toAgentName ?? ""}
                  reason={ev.reason} kind={ev.handoffKind ?? "transfer"} />;
              case "task_completed": return <TaskCompletedCard key={ev.id} />;
              case "agent_expelled":
                return <AgentExpelledCard key={ev.id} agentName={ev.agentName} reason={ev.reason} />;
              case "audit_verdict":
                return <AuditVerdictCard key={ev.id} agentName={ev.agentName} reason={ev.reason}
                  scoreDelta={ev.auditScoreDelta} />;
              default: return null;
            }
          })}
          {streamingAgents.size > 0 && (
            <div className={`${compactSpacing} pt-1`}>
              {Array.from(streamingAgents.entries()).map(([agentId, info]) => {
                const found = agents.find((a) => a.id === agentId);
                return <StreamingAgentRow key={agentId} info={info} agent={found ?? null} compact={isCompact} />;
              })}
            </div>
          )}
        </>
      )}
      <div ref={chatEndRef} className="h-0 w-0" />
    </div>
  );
});
