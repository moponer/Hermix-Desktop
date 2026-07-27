/**
 * Streaming / typing indicator components.
 *
 * Extracted from GroupChatView.tsx during the 2026-07 refactor.
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
}: {
  info: { name: string; role: "leader" | "member" };
  agent: AgentPersona | null;
}) {
  const isLeader = info.role === "leader";
  return (
    <div className="flex gap-2 items-start animate-in">
      {isLeader ? (
        <LeaderAvatar size={30} />
      ) : agent ? (
        <AgentAvatar agent={agent} size={30} />
      ) : (
        <div
          className="w-[30px] h-[30px] rounded-full bg-bg-tertiary"
          title={info.name}
        />
      )}
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-1.5 mb-0.5">
          <span className="text-[13px] text-text-primary font-medium">
            {info.name}
          </span>
          {isLeader && (
            <span
              className="text-[12px] uppercase tracking-wider px-1 py-px rounded font-semibold"
              style={{
                background: "hsla(43, 96%, 56%, 0.18)",
                color: "hsl(33, 90%, 38%)",
              }}
            >
              总管
            </span>
          )}
        </div>
        <div
          className="inline-flex items-center gap-1.5 rounded-xl px-3 py-1.5 text-[13px]"
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
