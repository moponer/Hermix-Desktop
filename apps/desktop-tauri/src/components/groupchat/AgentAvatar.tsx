/**
 * Avatar components for the groupchat module.
 *
 * Extracted from GroupChatView.tsx during the 2026-07 refactor.
 */

import { memo } from "react";
import { Crown } from "lucide-react";
import { clsx } from "clsx";
import type { AgentPersona } from "../agents/types";
import { agentHue, agentTone } from "./types";

/* ── Member avatar ──
 * Renders an emoji glyph or first-letter initial over a stable
 * per-agent color. Used standalone (30px) in chat and stacked (20px)
 * in room-list chips.
 */
export function AgentAvatar({
  agent,
  size = 32,
  ringed = false,
}: {
  agent: AgentPersona;
  size?: number;
  ringed?: boolean;
}) {
  const tone = agentTone(agent.id);
  const initial = agent.name?.[0]?.toUpperCase() || "?";
  const glyph = agent.avatar || initial;
  const isEmoji = glyph.length <= 4 && /\p{Emoji}/u.test(glyph);
  return (
    <div
      className={clsx(
        "rounded-full flex items-center justify-center shrink-0 font-semibold select-none",
        ringed && "ring-[1.5px] ring-white/80 dark:ring-zinc-800/80",
      )}
      style={{
        width: size,
        height: size,
        background: `hsl(${agentHue(agent.id)}, 55%, 55%, 0.35)`,
        color: tone.text,
        fontSize: isEmoji ? size * 0.55 : size * 0.42,
        lineHeight: 1,
      }}
      title={agent.name}
    >
      {glyph}
    </div>
  );
}

/* ── Leader Avatar ──
 * Visual identity for the built-in 项目总管 (P1). Distinct from
 * member avatars: Crown glyph, amber (gold) tinted palette, fixed
 * z-index boost so it always sits at the front of the pile.
 */
export function LeaderAvatar({
  size = 30,
  ringed = false,
}: {
  size?: number;
  ringed?: boolean;
}) {
  return (
    <div
      className={clsx(
        "rounded-full flex items-center justify-center shrink-0 font-semibold select-none",
        ringed && "ring-[1.5px] ring-white/80 dark:ring-zinc-800/80",
      )}
      style={{
        width: size,
        height: size,
        background: "hsla(43, 96%, 56%, 0.35)",
        color: "hsl(33, 90%, 38%)",
        fontSize: size * 0.55,
        lineHeight: 1,
        boxShadow: "inset 0 0 0 1px hsla(43, 96%, 50%, 0.55)",
      }}
      title="项目总管（系统内置）"
    >
      <Crown size={size * 0.55} strokeWidth={2.4} />
    </div>
  );
}

/* ── Stacked avatar pile (max 3 + overflow) ──
 * Used in room-list rows to summarize who's in the room without
 * dumping a wall of emoji names. Overlap is achieved with negative
 * margin + z-index so the leftmost avatar sits on top.
 */
export const AgentStack = memo(function AgentStack({
  agents,
  max = 3,
}: {
  agents: AgentPersona[];
  max?: number;
}) {
  if (agents.length === 0) {
    return (
      <span className="text-[10.5px] text-text-faint italic">暂无成员</span>
    );
  }
  const shown = agents.slice(0, max);
  const overflow = agents.length - shown.length;
  return (
    <div className="flex items-center min-w-0">
      <div className="flex items-center shrink-0">
        {shown.map((a, i) => (
          <div
            key={a.id}
            style={{ marginLeft: i === 0 ? 0 : -5, zIndex: shown.length - i }}
          >
            <AgentAvatar agent={a} size={20} ringed />
          </div>
        ))}
        {overflow > 0 && (
          <div
            className="rounded-full bg-bg-tertiary text-text-muted text-[9px] font-semibold flex items-center justify-center ring-2 ring-bg-primary"
            style={{ width: 20, height: 20, marginLeft: -7 }}
          >
            +{overflow}
          </div>
        )}
      </div>
      <span className="ml-2 text-[10.5px] text-text-muted truncate">
        {agents.length} 位成员
      </span>
    </div>
  );
});
