/**
 * Rank chip and summary components (P4).
 *
 * Renders a one-room rank summary as a compact chip. Extracted from
 * GroupChatView.tsx during the 2026-07 refactor.
 */

import { Trophy } from "lucide-react";
import type * as api from "@/api";

// Allowed accent hues only — NO blue/indigo/violet/purple (project iron law).
// silver remapped from 220 (blue) → 150 (emerald); star from 268 (violet) → 340 (rose);
// diamond remapped from 195 (cyan) → 168 (teal) — 195 read as blue per iron law.
const TIER_HUE: Record<string, number> = {
  bronze: 28,
  silver: 150,
  gold: 43,
  platinum: 175,
  diamond: 168,
  star: 340,
  king: 0,
  king_legend: 320,
};

const TIER_ORDER = [
  "bronze", "silver", "gold", "platinum",
  "diamond", "star", "king", "king_legend",
] as const;

const TIER_CN: Record<string, string> = {
  bronze: "青铜",
  silver: "白银",
  gold: "黄金",
  platinum: "铂金",
  diamond: "钻石",
  star: "星耀",
  king: "王者",
  king_legend: "传奇",
};

export function tierColor(tier: string): { bg: string; text: string; ring: string } {
  const hue = TIER_HUE[tier] ?? 168;
  return {
    bg: `hsl(${hue}, 55%, 55%, 0.16)`,
    text: `hsl(${hue}, 60%, 38%)`,
    ring: `hsl(${hue}, 55%, 50%, 0.45)`,
  };
}

function summarizeRanks(ranks: api.GroupChatRank[]): Array<{
  tier: string;
  tier_cn: string;
  count: number;
  color: ReturnType<typeof tierColor>;
}> {
  const counts = new Map<string, number>();
  for (const r of ranks) counts.set(r.tier, (counts.get(r.tier) ?? 0) + 1);
  return TIER_ORDER
    .filter((t) => counts.has(t))
    .map((t) => ({
      tier: t,
      tier_cn: TIER_CN[t] ?? t,
      count: counts.get(t) ?? 0,
      color: tierColor(t),
    }));
}

/** Compact chip showing the top member's tier — used in room list rows. */
export function RoomRankChip({ ranks }: { ranks: api.GroupChatRank[] | undefined }) {
  if (!ranks || ranks.length === 0) return null;
  const top = ranks[0];
  const c = tierColor(top.tier);
  return (
    <span
      className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded-md text-[12px] font-semibold shrink-0"
      style={{ background: c.bg, color: c.text, boxShadow: `inset 0 0 0 1px ${c.ring}` }}
      title={`最高段位：${top.tier_cn} · 积分 ${top.score}`}
    >
      <span aria-hidden>{top.badge_emoji}</span>
      <span>{top.tier_cn}</span>
    </span>
  );
}


/** Single-agent rank chip — used inline in MessageBubble so the tier
 *  is visible next to the speaker's name in the chat stream itself,
 *  not only in the room list / room header. Renders nothing when
 *  the rank row is missing or unranked (no tier assigned yet). */
export function AgentRankBadge({ rank }: { rank: api.GroupChatRank | undefined }) {
  if (!rank || !rank.tier) return null;
  const c = tierColor(rank.tier);
  return (
    <span
      className="inline-flex items-center gap-0.5 px-1 py-px rounded text-[11px] font-semibold shrink-0"
      style={{ background: c.bg, color: c.text, boxShadow: `inset 0 0 0 1px ${c.ring}` }}
      title={`段位：${rank.tier_cn} · 积分 ${rank.score}`}
      data-testid="agent-rank-badge"
      data-tier={rank.tier}
    >
      <span aria-hidden>{rank.badge_emoji}</span>
      <span>{rank.tier_cn}</span>
    </span>
  );
}

/** Full tier breakdown — used in the active room header. */
export function RoomRankSummary({ ranks }: { ranks: api.GroupChatRank[] | undefined }) {
  if (!ranks || ranks.length === 0) return null;
  const parts = summarizeRanks(ranks);
  return (
    <span className="inline-flex items-center gap-1.5 text-[12px] text-text-muted">
      <Trophy size={11} className="text-text-muted" />
      {parts.map((p, i) => (
        <span key={p.tier} className="inline-flex items-center gap-0.5">
          {i > 0 && <span className="text-text-faint">·</span>}
          <span
            className="px-1 rounded"
            style={{ background: p.color.bg, color: p.color.text }}
          >
            {p.tier_cn}×{p.count}
          </span>
        </span>
      ))}
    </span>
  );
}
