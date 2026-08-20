/**
 * Streaming / typing indicator components.
 *
 * Extracted from GroupChatView.tsx.
 */

import type { AgentPersona } from "../agents/types";
import { AgentAvatar, LeaderAvatar } from "./AgentAvatar";

/** Three bouncing dots with staggered delays. */
export function TypingDots() {
  return (
    <span className="flex gap-0.5 items-center">
      <span
        className="w-1 h-1 rounded-full bg-text-muted animate-bounce"
        style={{ animationDelay: "0ms" }}
      />
      <span
        className="w-1 h-1 rounded-full bg-text-muted animate-bounce"
        style={{ animationDelay: "150ms" }}
      />
      <span
        className="w-1 h-1 rounded-full bg-text-muted animate-bounce"
        style={{ animationDelay: "300ms" }}
      />
    </span>
  );
}

/** One row for a currently-streaming agent: avatar + name + typing dots. */
export function StreamingAgentRow({
  info,
  agent,
  compact = false,
}: {
  info: { name: string; role: "leader" | "member" };
  agent: AgentPersona | null;
  compact?: boolean;
}) {
  const isLeader = info.role === "leader";
  const avatarSize = compact ? 24 : 30;
  const textSize = compact ? "text-[12px]" : "text-[13px]";
  const padding = compact ? "px-2.5 py-1" : "px-3 py-1.5";
  return (
    <div className="flex gap-2 items-start animate-in">
      {isLeader ? (
        <LeaderAvatar size={avatarSize} />
      ) : agent ? (
        <AgentAvatar agent={agent} size={avatarSize} />
      ) : (
        <div
          className="rounded-full bg-bg-tertiary"
          style={{ width: avatarSize, height: avatarSize }}
          title={info.name}
        />
      )}
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-1.5 mb-0.5">
          <span className={`${textSize} text-text-primary font-medium`}>
            {info.name}
          </span>
          {isLeader && (
            <span
              className="text-[12px] uppercase tracking-wider px-1 py-px rounded font-semibold"
              style={{
                background: "var(--color-accent-soft)",
                color: "var(--color-accent)",
              }}
            >
              总管
            </span>
          )}
        </div>
        <div
          className={`inline-flex items-center gap-1.5 rounded-xl ${padding} ${textSize}`}
          style={{
            background: "var(--color-bg-tertiary)",
            color: "var(--color-text-muted)",
          }}
        >
          <TypingDots />
          <span>{isLeader ? "总管正在拆解任务…" : "正在思考…"}</span>
        </div>
      </div>
    </div>
  );
}
