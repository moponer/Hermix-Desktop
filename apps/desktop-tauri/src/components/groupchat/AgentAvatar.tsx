/**
 * Avatar components for the groupchat module.
 *
 * All colors derive from theme tokens (accent / bg surfaces / text levels)
 * so they adapt correctly across default, sunset, dark, etc.
 */

import { memo } from "react";
import { Crown } from "lucide-react";
import { clsx } from "clsx";
import type { AgentPersona } from "../agents/types";
import { agentTone } from "./types";

/* ── Member avatar ── */
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
        ringed && "ring-[1.5px] ring-bg-primary",
      )}
      style={{
        width: size,
        height: size,
        background: tone.bg,
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
 * Crown glyph; uses theme accent color instead of hardcoded gold. */
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
        ringed && "ring-[1.5px] ring-bg-primary",
      )}
      style={{
        width: size,
        height: size,
        background: "var(--color-accent-soft)",
        color: "var(--color-accent)",
        fontSize: size * 0.55,
        lineHeight: 1,
        boxShadow: "inset 0 0 0 1px var(--color-accent-soft-strong)",
      }}
      title="项目总管（系统内置）"
    >
      <Crown size={size * 0.55} strokeWidth={2.4} />
    </div>
  );
}

/* ── Stacked avatar pile (max 3 + overflow) ── */
export const AgentStack = memo(function AgentStack({
  agents,
  max = 3,
}: {
  agents: AgentPersona[];
  max?: number;
}) {
  if (agents.length === 0) {
    return (
      <span className="text-[12px] text-text-faint italic">暂无成员</span>
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
            className="rounded-full bg-bg-tertiary text-text-muted text-[12px] font-semibold flex items-center justify-center ring-2 ring-bg-primary"
            style={{ width: 20, height: 20, marginLeft: -7 }}
          >
            +{overflow}
          </div>
        )}
      </div>
      <span className="ml-2 text-[12px] text-text-muted truncate">
        {agents.length} 位成员
      </span>
    </div>
  );
});
