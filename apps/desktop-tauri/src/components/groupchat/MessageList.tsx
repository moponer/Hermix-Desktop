/**
 * Message list container — renders the scrollable message stream
 * with typing indicators and empty-state hero.
 *
 * Extracted from GroupChatView.tsx during the 2026-07 refactor.
 */

import { useRef, useEffect, memo } from "react";
import { Users, MessageSquare } from "lucide-react";
import type { AgentPersona } from "../agents/types";
import { MessageBubble, DispatchErrorCard, HandoffSeparator } from "./MessageBubble";
import { StreamingAgentRow } from "./TypingIndicator";
import type { ChatMsg, AgentTone, InlineEvent } from "./types";

// Re-export for backward compatibility — callers that imported
// InlineEvent from MessageList still work.
export type { InlineEvent };

export const MessageList = memo(function MessageList({
  messages,
  agents,
  streamingAgents,
  inlineEvents,
  roomTone,
  roomAgents,
  hasLeader,
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
  onRetry?: () => void;
  onOpenMembers?: () => void;
}) {
  const chatEndRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    chatEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages, streamingAgents, inlineEvents]);

  return (
    <div className="flex-1 overflow-y-auto px-5 py-4 space-y-3.5">
      {messages.length === 0 && inlineEvents.length === 0 ? (
        <div className="flex flex-col items-center justify-center py-12 text-center">
          <div
            className="w-20 h-20 rounded-2xl flex items-center justify-center text-4xl mb-4 shadow-sm"
            style={{
              background: roomTone?.bg ?? "var(--color-bg-tertiary)",
              boxShadow: roomTone
                ? `inset 0 0 0 1px ${roomTone.ring}55`
                : undefined,
            }}
          >
            <MessageSquare size={36} className="text-text-faint" />
          </div>
          <p className="text-[15px] font-semibold text-text-primary mb-1">
            开始一场群聊
          </p>
          <p className="text-[13px] text-text-muted mb-5 max-w-[280px] leading-relaxed">
            {roomAgents.length > 0
              ? `${hasLeader ? "项目总管已就位 · " : ""}${roomAgents
                  .slice(0, 2)
                  .map((a) => a.name)
                  .join("、")} 等 ${roomAgents.length} 位成员正在待命`
              : hasLeader
                ? "项目总管已就位，等待团队成员加入后开始协作"
                : "邀请成员后，他们会基于各自角色协作回复你的消息"}
          </p>
          {onOpenMembers && (
            <button onClick={onOpenMembers} className="btn btn-primary btn-sm">
              <Users size={13} /> 管理成员
            </button>
          )}
        </div>
      ) : (
        <>
          {messages.map((msg) => (
            <MessageBubble key={msg.id} msg={msg} agents={agents} />
          ))}

          {/* Inline transient events (dispatch errors, handoffs) */}
          {inlineEvents.map((ev) =>
            ev.kind === "dispatch_failed" ? (
              <DispatchErrorCard
                key={ev.id}
                agentName={ev.agentName}
                reason={ev.reason}
                isLeader={ev.isLeader}
                onRetry={onRetry}
              />
            ) : (
              <HandoffSeparator
                key={ev.id}
                toAgentName={ev.toAgentName ?? ""}
                reason={ev.reason}
                kind={ev.handoffKind ?? "transfer"}
              />
            ),
          )}

          {/* Live typing indicators */}
          {streamingAgents.size > 0 && (
            <div className="space-y-3.5 pt-1">
              {Array.from(streamingAgents.entries()).map(([agentId, info]) => {
                const found = agents.find((a) => a.id === agentId);
                return (
                  <StreamingAgentRow
                    key={agentId}
                    info={info}
                    agent={found ?? null}
                  />
                );
              })}
            </div>
          )}
        </>
      )}
      <div ref={chatEndRef} className="h-0 w-0" />
    </div>
  );
});
