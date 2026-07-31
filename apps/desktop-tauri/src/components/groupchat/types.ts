/**
 * Shared types and constants for the groupchat module.
 *
 * Extracted from the original monolithic GroupChatView.tsx during the
 * refactor (see docs/GROUPCHAT_REFACTOR_PLAN.md §3.2.1).
 */

import type * as api from "@/api";

/** A group chat room with its metadata, members, and active task. */
export interface ChatRoom {
  id: string;
  name: string;
  agentIds: string[];
  leaderId: string | null;
  /** The built-in leader (项目总管) virtual identity. Optional for
   *  backward compatibility with rooms created before P1. */
  leader?: api.GroupChatLeader | null;
  /** Per-member model overrides (agent_id → model id). */
  memberModels: Record<string, string | undefined>;
  /** Per-member display name (别名) overrides (agent_id → alias). */
  memberDisplayNames: Record<string, string | undefined>;
  /** Pinned project announcement. `null` when no announcement. */
  announcement: string | null;
  announcementPublishedAt: string | null;
  /** Free-form description of the room (project background, goal,
   *  constraints). `null` when no description is set. Distinct from
   *  `announcement` which is the leader-pinned "current goal". */
  description: string | null;
  /** UI rendering style: "modern" | "classic" | "compact".
   *  Defaults to "modern" when null/undefined (old rooms). */
  style: string | null;
  /** Chat mode: "task_collab" (leader dispatches tasks) or "free_chat"
   *  (members respond freely). Defaults to "task_collab" when null. */
  chatMode: "task_collab" | "free_chat";
  /** The currently in-flight task. `null` when no active task. */
  activeTask: api.GroupChatTask | null;
}

/** Sentinel `sender_id` for the built-in leader. */
export const LEADER_SENDER_ID = "__leader__";

/** Sentinel `sender_id` for system-level hint messages. */
export const SYSTEM_SENDER_ID = "__system__";

/** A chat message in the local UI state. */
export interface ChatMsg {
  id: string;
  roomId: string;
  senderId: string;
  senderName: string;
  content: string;
  messageType: string;
  timestamp: number;
  /** Optional metadata JSON — may contain `raw` field with unstripped
   *  agent output for collapsible thinking section. */
  metadata?: string | null;
}

/** Convert an API message to the local ChatMsg format. */
export function apiMsgToChatMsg(m: api.GroupChatMessage): ChatMsg {
  return {
    id: m.id,
    roomId: m.room_id,
    senderId: m.sender_id,
    senderName: m.sender_name,
    content: m.content,
    messageType: m.message_type,
    timestamp: new Date(m.created_at).getTime(),
    metadata: m.metadata ?? null,
  };
}

/* ── Agent visual identity ──
 *
 * Stable hue derived from agent id, so the same agent always wears
 * the same color across rooms. Drives avatar background, message
 * bubble tint, name badge, and the room hero strip background.
 */

export interface AgentTone {
  /** soft tint — used for message bubble background (low alpha) */
  bg: string;
  /** readable name color on tinted bg */
  text: string;
  /** border / avatar ring color */
  ring: string;
  /** small accent (online dot, badge) */
  dot: string;
  /** hero strip background — single tinted color, room-unique feel */
  hero: string;
}

// Allowed accent hues only — NO blue/indigo/violet/purple (they clash with
// the message-bubble palette). amber / gold / emerald / teal / rose / magenta
// / red (195 cyan dropped → 168 teal).
const AGENT_HUE_PALETTE = [28, 43, 150, 175, 168, 340, 320, 0];

/** Derive a stable accent hue from an agent id, drawn from a fixed palette so
 *  no agent ever renders in a banned blue/violet hue. */
export function agentHue(id: string): number {
  let h = 0;
  for (let i = 0; i < id.length; i++) {
    h = id.charCodeAt(i) + ((h << 5) - h);
  }
  return AGENT_HUE_PALETTE[Math.abs(h) % AGENT_HUE_PALETTE.length];
}

/** Build the full tone palette for an agent id. */
export function agentTone(id: string): AgentTone {
  const h = agentHue(id);
  return {
    bg: `hsl(${h}, 60%, 55%, 0.10)`,
    text: `hsl(${h}, 55%, 32%)`,
    ring: `hsl(${h}, 55%, 55%)`,
    dot: `hsl(${h}, 70%, 50%)`,
    hero: `hsl(${h}, 30%, 65%, 0.04)`,
  };
}

/** Inline transient events (dispatch errors, handoffs) injected into
 *  the message stream between regular messages. */
export interface InlineEvent {
  id: string;
  kind: "dispatch_failed" | "handoff" | "task_completed" | "agent_expelled" | "audit_verdict";
  agentName: string;
  reason: string;
  isLeader: boolean;
  toAgentName?: string;
  handoffKind?: string;
  /** Only for `audit_verdict` events — the numeric level (e.g. "violation"
   *  / "warning") so the renderer can pick a tone. */
  auditLevel?: string;
  /** Only for `audit_verdict` events — score delta for the chip text. */
  auditScoreDelta?: number;
}

/**
 * Convert `hsl(H, S%, L%)` to `hsla(H, S%, L%, A)` so callers can keep
 * the base colour as `tone.ring` (full saturation) and apply alpha at
 * the call site.
 *
 * Why this exists: the old call sites wrote `${tone.ring}33` — the
 * intent was `#RRGGBBAA`-style hex alpha suffix (0x33 = 20% opacity).
 * That pattern works for hex colours but NOT for `hsl(...)`; the CSS
 * parser treats the trailing `33` as a stray length and silently drops
 * the entire `box-shadow` declaration. Every groupchat bubble therefore
 * ended up with a fully-opaque border — visible as a thick horizontal
 * line below the message text instead of the intended 20% subtle ring.
 *
 * Pass-through when the input doesn't match `hsl(...)`, so the helper
 * is safe to call on any CSS colour.
 */
export function withAlpha(color: string, alpha: number): string {
  if (color.startsWith('hsl(') && color.endsWith(')')) {
    return `hsla(${color.slice(4, -1)}, ${alpha})`;
  }
  return color;
}
