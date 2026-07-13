import { useState, useEffect, useCallback, useRef, memo } from "react";
import { Users, Plus, Trash2, MessageCircle, X, Settings, AlertCircle, Crown, ListChecks, Copy, Search } from "lucide-react";
import { clsx } from "clsx";
import type { AgentPersona } from "./AgentsView";
import { BUILTIN_EXPERTS, type ExpertDef } from "../data/builtinExperts";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";
import * as api from "../api";
import { RoomSettingsDrawer } from "./RoomSettingsDrawer";
import { RoomMemberDialog } from "./RoomMemberDialog";
import { TaskPanel, type TaskEngineEvent } from "./TaskPanel";
import { MarkdownBody } from "./MarkdownBody";
import { ChatComposerInput, type ChatComposerInputHandle } from "./ChatComposerInput";
import { ImperativeDialog, type ImperativeDialogHandle } from "./ImperativeDialog";

/**
 * Toast-style inline error banner. Rendered above the form so the
 * user sees *something* happen when the API rejects. Without this
 * the create button "click does nothing" — a silent catch
 * (console.error only) made the failure invisible.
 */
function InlineError({ message, onDismiss }: { message: string; onDismiss: () => void }) {
  return (
    <div
      role="alert"
      className="flex items-start gap-2 px-2.5 py-2 rounded-lg text-[12px] leading-snug"
      style={{ background: "var(--color-error-bg, rgba(239,68,68,0.10))", color: "var(--color-error-text, #b91c1c)" }}
    >
      <AlertCircle size={13} className="shrink-0 mt-[1px]" />
      <span className="flex-1 break-words">{message}</span>
      <button
        onClick={onDismiss}
        className="shrink-0 opacity-60 hover:opacity-100"
        title="关闭"
        type="button"
       aria-label="关闭">
        <X size={12} />
      </button>
    </div>
  );
}

/* ── Types ── */

interface ChatRoom {
  id: string;
  name: string;
  agentIds: string[];
  leaderId: string | null;
  /** The built-in leader (项目总管) virtual identity. Optional for
   *  backward compatibility with rooms created before P1. */
  leader?: api.GroupChatLeader | null;
  /** Per-member model overrides (agent_id → model id). Used by the
   *  member management dialog to pre-fill the model select. */
  memberModels: Record<string, string | undefined>;
  /** P3 — per-member display name (别名) overrides (agent_id → alias).
   *  Used by the member management dialog so the row shows the alias
   *  instead of the global name. `undefined` → use the agent's
   *  global name from `agents`. */
  memberDisplayNames: Record<string, string | undefined>;
  /** Pinned project announcement. `null` when no announcement has
   *  been published. Updated in place after the user edits / clears
   *  inside TaskPanel without a full re-fetch. */
  announcement: string | null;
  announcementPublishedAt: string | null;
  /** The currently in-flight task. `null` when no active task
   *  exists. We keep this on the room so the hero indicator ("📋 任务")
   *  can flip without a separate endpoint call. */
  activeTask: api.GroupChatTask | null;
}

/** Sentinel `sender_id` value that the server stamps on messages
 *  emitted by the built-in leader. Frontend uses this to switch into
 *  the leader-specific message bubble style. Kept in sync with
 *  `LEADER_AGENT_ID` in `hermix-gateway/src/groupchat/persistence.rs`. */
const LEADER_SENDER_ID = "__leader__";

/** Sentinel `sender_id` for system-level hint messages inserted by the
 *  server when all agent dispatch fails. Rendered as a centered grey
 *  toast in the chat stream. */
const SYSTEM_SENDER_ID = "__system__";

interface ChatMsg {
  id: string;
  roomId: string;
  senderId: string;
  senderName: string;
  content: string;
  messageType: string;
  timestamp: number;
  /** Optional metadata JSON — when present may contain `raw` field
   *  with unstripped agent output for collapsible thinking section. */
  metadata?: string | null;
}

/* ── Helpers ── */

/** Simple hash-based color picker matching AgentsView's `pickColor`. */
function pickColorForExpert(id: string): "violet" | "rose" | "cyan" | "emerald" | "amber" | "teal" | "indigo" | "fuchsia" {
  const themes = ["violet", "rose", "cyan", "emerald", "amber", "teal", "indigo", "fuchsia"] as const;
  let h = 0;
  for (const ch of id) h = ((h << 5) - h + ch.charCodeAt(0)) | 0;
  return themes[Math.abs(h) % themes.length];
}

/** Convert builtin ExpertDef to AgentPersona format for uniform handling. */
function expertToPersona(e: ExpertDef): AgentPersona {
  return {
    id: e.id,
    name: e.name,
    avatar: e.avatar,
    role: e.role,
    identity: e.description,
    soul: e.systemPrompt,
    skills: e.toolset ? [e.toolset] : [],
    specialty: e.category,
    dialogStyle: "professional",
    background: e.description,
    personality: [],
    systemPrompt: e.systemPrompt,
    colorTheme: pickColorForExpert(e.id),
    usageCount: 0,
    lastUsedAt: 0,
    createdAt: 0,
    updatedAt: 0,
  };
}

/** Load agents: builtin experts + user-created, deduped by id. */
function loadAgents(): AgentPersona[] {
  const userAgents: AgentPersona[] = (() => {
    try { return JSON.parse(storage.getItem(STORAGE_KEYS.agents) || "[]"); } catch { return []; }
  })();
  const builtin = BUILTIN_EXPERTS.map(expertToPersona);
  // Merge: user agents override builtin with same id, builtin fills gaps
  const userIds = new Set(userAgents.map((a) => a.id));
  const merged = [...builtin.filter((b) => !userIds.has(b.id)), ...userAgents];
  return merged;
}

function apiMsgToChatMsg(m: api.GroupChatMessage): ChatMsg {
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
 *
 * 渐变色禁止 — 每个 token 都是单一 HSL（含 alpha）。层次感靠 alpha
 * 透明度 + inset ring 边框 + 阴影，不靠 linear-gradient。
 */

interface AgentTone {
  /** soft tint — used for message bubble background (single color, low alpha) */
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

function agentHue(id: string): number {
  let h = 0;
  for (let i = 0; i < id.length; i++) {
    h = id.charCodeAt(i) + ((h << 5) - h);
  }
  return Math.abs(h) % 360;
}

function agentTone(id: string): AgentTone {
  const h = agentHue(id);
  return {
    // 0.10 alpha — barely tinted bg, lets text breathe
    bg: `hsl(${h}, 60%, 55%, 0.10)`,
    text: `hsl(${h}, 55%, 32%)`,
    ring: `hsl(${h}, 55%, 55%)`,
    dot: `hsl(${h}, 70%, 50%)`,
    // 0.04 alpha — whisper tint, barely there. Saturation dropped
    // to 30% so the room color is hinted, not shouted.
    hero: `hsl(${h}, 30%, 65%, 0.04)`,
  };
}

/* ── Avatar ──
 * Renders an emoji glyph or first-letter initial over a stable
 * per-agent color. Used standalone (30px) in chat and stacked (20px)
 * in room-list chips.
 */
function AgentAvatar({
  agent,
  size = 32,
  ringed = false,
}: { agent: AgentPersona; size?: number; ringed?: boolean }) {
  const tone = agentTone(agent.id);
  const initial = agent.name?.[0]?.toUpperCase() || "?";
  const glyph = agent.avatar || initial;
  // Emoji glyphs render larger than initials at the same font size —
  // scale them down so the visual weight matches.
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
 * member avatars in three ways:
 *   1. Crown glyph instead of initial / emoji
 *   2. Amber (gold) tinted palette — never uses the agent hue hash
 *   3. Fixed z-index boost so it always sits at the front of the pile
 *
 * Uses a single HSL with alpha — no gradient per project rule.
 * Sized to match AgentAvatar so avatar-wall geometry stays consistent. */
function LeaderAvatar({ size = 30, ringed = false }: { size?: number; ringed?: boolean }) {
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

/* ── Typing indicator ──
 * Renders one row per currently-streaming agent: avatar + name + a
 * 3-dot "thinking" pulse. Member and leader share the same component
 * — only the avatar and copy differ. Dots use staggered animation
 * delays so they bounce left-to-right.
 */
function TypingDots() {
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

function StreamingAgentRow({
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
        // Fallback: a plain colored dot when we can't find the agent
        // profile (e.g. member was added in another tab and the local
        // agent cache hasn't refreshed yet).
        <div
          className="w-[30px] h-[30px] rounded-full bg-bg-tertiary"
          title={info.name}
        />
      )}
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-1.5 mb-0.5">
          <span className="text-[12px] text-text-primary font-medium">
            {info.name}
          </span>
          {isLeader && (
            <span
              className="text-[9.5px] uppercase tracking-wider px-1 py-px rounded font-semibold"
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
          className="inline-flex items-center gap-1.5 rounded-xl px-3 py-1.5 text-[12px]"
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

/* ── Rank chip & summary (P4) ──
 *
 * Renders a one-room rank summary as a compact chip. The chip is
 * intentionally tiny (10.5px) so the room list row stays single-line
 * when it carries an agent stack. Tier order (bronze → king_legend)
 * drives the left-to-right ordering in the summary header so the
 * highest tier is always listed first.
 *
 * 渐变禁止 — each tier gets a single HSL hue at ~30 % alpha for the
 * background; the text/border use a slightly darker shade of the
 * same hue. The hue map is hard-coded because the server only ships
 * 8 tiers and adding a CSS variable per tier would be over-engineering.
 */
const TIER_HUE: Record<string, number> = {
  bronze: 28,         // bronze brown
  silver: 220,        // cool grey-blue
  gold: 43,           // amber
  platinum: 175,      // teal
  diamond: 195,       // cyan
  star: 268,          // purple
  king: 0,            // red
  king_legend: 320,   // magenta
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

function tierColor(tier: string): { bg: string; text: string; ring: string } {
  const hue = TIER_HUE[tier] ?? 220;
  return {
    bg: `hsl(${hue}, 55%, 55%, 0.16)`,
    text: `hsl(${hue}, 60%, 38%)`,
    ring: `hsl(${hue}, 55%, 50%, 0.45)`,
  };
}

function summarizeRanks(ranks: api.GroupChatRank[]): Array<{ tier: string; tier_cn: string; count: number; color: ReturnType<typeof tierColor> }> {
  const counts = new Map<string, number>();
  for (const r of ranks) counts.set(r.tier, (counts.get(r.tier) ?? 0) + 1);
  // Order by canonical tier ladder; tiers with 0 are skipped.
  return TIER_ORDER
    .filter((t) => counts.has(t))
    .map((t) => ({
      tier: t,
      tier_cn: TIER_CN[t] ?? t,
      count: counts.get(t) ?? 0,
      color: tierColor(t),
    }));
}

function RoomRankChip({ ranks }: { ranks: api.GroupChatRank[] | undefined }) {
  if (!ranks || ranks.length === 0) return null;
  const top = ranks[0]; // server already sorts by score DESC
  const c = tierColor(top.tier);
  return (
    <span
      className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded-md text-[10px] font-semibold shrink-0"
      style={{ background: c.bg, color: c.text, boxShadow: `inset 0 0 0 1px ${c.ring}` }}
      title={`最高段位：${top.tier_cn} · 积分 ${top.score}`}
    >
      <span aria-hidden>{top.badge_emoji}</span>
      <span>{top.tier_cn}</span>
    </span>
  );
}

function RoomRankSummary({ ranks }: { ranks: api.GroupChatRank[] | undefined }) {
  if (!ranks || ranks.length === 0) return null;
  const parts = summarizeRanks(ranks);
  return (
    <span className="inline-flex items-center gap-1.5 text-[10.5px] text-text-muted">
      <span aria-hidden>🏆</span>
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

/* ── Stacked avatar pile (max 3 + overflow) ──
 * Used in room-list rows to summarize who's in the room without
 * dumping a wall of emoji names. Overlap is achieved with negative
 * margin + z-index so the leftmost avatar sits on top.
 */
function AgentStack({ agents, max = 3 }: { agents: AgentPersona[]; max?: number }) {
  if (agents.length === 0) {
    return <span className="text-[10.5px] text-text-faint italic">暂无成员</span>;
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
}

/* ── Room list item ──
 * Each row is a button (whole-row click target). Selected state is
 * communicated three ways at once for unmistakable hierarchy:
 *   1. 3px accent rail on the leading edge
 *   2. solid bg-hover fill (no gradient — 渐变禁止)
 *   3. stronger text color
 * Trash button only surfaces on hover so the row stays calm by default.
 */
const RoomItem = memo(function RoomItem({ room, agents, active, onSelect, onDelete, ranks }: {
  room: ChatRoom;
  agents: AgentPersona[];
  active: boolean;
  onSelect: () => void;
  onDelete: () => void;
  /** P4 — per-room top-member rank chip. Undefined while loading or
   *  when the fetch failed (chip is then hidden, not skeleton). */
  ranks?: api.GroupChatRank[];
}) {
  const roomAgents = agents.filter((a) => room.agentIds.includes(a.id));
  return (
    <button
      onClick={onSelect}
      className={clsx(
        "relative w-full text-left px-3 py-2.5 rounded-xl transition-all group",
        active
          ? "bg-bg-hover shadow-[0_1px_2px_rgba(0,0,0,0.04)]"
          : "hover:bg-bg-hover",
      )}
      type="button"
    >
      {active && (
        <span
          className="absolute left-0 top-1/2 -translate-y-1/2 w-[3px] h-6 rounded-r-full"
          style={{ background: "var(--color-btn-primary-bg)" }}
        />
      )}
      <div className="flex items-center justify-between gap-2 mb-1.5">
        <span
          className={clsx(
            "text-[13px] font-semibold truncate",
            active ? "text-text-primary" : "text-text-secondary",
          )}
        >
          {room.name}
        </span>
        <div className="flex items-center gap-1 shrink-0">
          <RoomRankChip ranks={ranks} />
          <span
            role="button"
            tabIndex={0}
            onClick={(e) => { e.stopPropagation(); if (window.confirm("确认解散该会话？这将清空全部聊天内容和产物，此操作不可撤销。")) onDelete(); }}
            onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.stopPropagation(); e.preventDefault(); onDelete(); } }}
            className="opacity-0 group-hover:opacity-100 w-5 h-5 rounded-md flex items-center justify-center text-text-faint hover:text-error hover:bg-error-bg transition-all cursor-pointer"
            title="删除"
          >
            <Trash2 size={11} />
          </span>
        </div>
      </div>
      <AgentStack agents={roomAgents} max={3} />
    </button>
  );
});

/* ── Main View ── */

interface GroupChatViewProps {
  initialRoomId?: string | null;
}

export function GroupChatView({ initialRoomId }: GroupChatViewProps = {}) {
  const [rooms, setRooms] = useState<ChatRoom[]>([]);
  const [agents, setAgents] = useState<AgentPersona[]>(loadAgents);
  const [activeRoomId, setActiveRoomId] = useState<string | null>(initialRoomId ?? null);
  const [messages, setMessages] = useState<ChatMsg[]>([]);
  const chatEndRef = useRef<HTMLDivElement>(null);

  // Auto-scroll to bottom when new messages arrive
  useEffect(() => {
    chatEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  // Sync initialRoomId prop changes (e.g. sidebar selects a different room)
  useEffect(() => {
    if (initialRoomId && initialRoomId !== activeRoomId) {
      setActiveRoomId(initialRoomId);
    }
  }, [initialRoomId]);

  // ── Composer 输入框状态：下放到 ChatComposerInput 内部（避免父组件每次按键都重渲染）
  // 父组件需要读 / 清空时通过 composerRef
  const composerRef = useRef<ChatComposerInputHandle>(null);
  const [showNewRoom, setShowNewRoom] = useState(false);
  const [newRoomName, setNewRoomName] = useState("");
  const [loading, setLoading] = useState(false);
  const [showRaw, setShowRaw] = useState<Set<string>>(new Set());
  const memberDialogRef = useRef<ImperativeDialogHandle>(null);
  const settingsDialogRef = useRef<ImperativeDialogHandle>(null);
  const taskDialogRef = useRef<ImperativeDialogHandle>(null);
  // Live task-engine events keyed by room_id. Filled from WS stream
  // (S5). Capped to last 60 events per room to bound memory.
  const [engineEventsByRoom, setEngineEventsByRoom] = useState<Record<string, TaskEngineEvent[]>>({});
  // Per-agent streaming state keyed by agent_id (leader + members).
  // Keyed in a Map so we know each agent's display name + whether they
  // are the supervisor (leader has no chat bubble — only a typing dot).
  // The Map is rebuilt on every update to keep React's referential
  // equality happy.
  const [streamingAgents, setStreamingAgents] = useState<
    Map<string, { name: string; role: "leader" | "member" }>
  >(() => new Map());
  // P4 — per-room rank snapshot keyed by room_id. The list is sorted
  // by score DESC by the server; we just render the top member as a
  // chip in the room list and a tier breakdown in the active header.
  // Failures are swallowed (rank is decorative — empty state still works).
  const [ranksByRoom, setRanksByRoom] = useState<Record<string, api.GroupChatRank[]>>({});

  // Helper: append an engine event to a room's timeline.
  const appendEngineEvent = useCallback((roomId: string, ev: TaskEngineEvent) => {
    setEngineEventsByRoom((prev) => {
      const list = prev[roomId] ?? [];
      const next = [...list, ev];
      // Cap to last 60.
      if (next.length > 60) next.splice(0, next.length - 60);
      return { ...prev, [roomId]: next };
    });
  }, []);

  // S5.1: task-engine events are pushed by the server over the same
  // WS connection after `group_chat_done`. The 1.5s polling that used
  // to live here has been removed; the sender task in `handle_ws_groupchat`
  // now forwards `task_phase_change` / `bus_delivered` / `audit_verdict`
  // / `agent_expelled` / `room_completed` events directly until the
  // room finishes.
  // Per-UI-block transient errors. Two slots — the create-room form
  // and the message-send path both can fail; we want each block to
  // surface its own error inline without bleeding into the other.
  const [createRoomError, setCreateRoomError] = useState<string | null>(null);
  const [creatingRoom, setCreatingRoom] = useState(false);
  // L5 — room list search filter
  const [roomSearch, setRoomSearch] = useState("");

  // Load rooms from API
  const loadRooms = useCallback(async () => {
    try {
      const apiRooms = await api.listGroupChatRooms();
      setRooms(apiRooms.map((r) => ({
        id: r.id,
        name: r.name,
        agentIds: r.agent_ids,
        leaderId: r.leader_id,
        leader: r.leader ?? null,
        memberModels: Object.fromEntries(
          (r.members ?? [])
            .filter((m) => m.model_override)
            .map((m) => [m.agent_id, m.model_override!])
        ),
        // P3 — alias map. Only rooms with at least one alias get an
        // entry, so the dialog can use `displayNames[agent_id] ?? agent.name`
        // without worrying about ghost keys.
        memberDisplayNames: Object.fromEntries(
          (r.members ?? [])
            .filter((m): m is api.GroupChatMember & { display_name: string } =>
              Boolean(m.display_name) && (m.display_name?.trim().length ?? 0) > 0,
            )
            .map((m) => [m.agent_id, m.display_name]),
        ),
        announcement: r.announcement ?? null,
        announcementPublishedAt: r.announcement_published_at ?? null,
        activeTask: r.active_task ?? null,
      })));
    } catch {
      // Fallback to empty
      setRooms([]);
    }
  }, []);

  // Load messages for active room
  const loadMessages = useCallback(async (roomId: string) => {
    try {
      const apiMsgs = await api.getGroupChatMessages(roomId);
      setMessages(apiMsgs.map(apiMsgToChatMsg));
    } catch {
      setMessages([]);
    }
  }, []);

  // P4 — fetch rank snapshots for every room in parallel. Per-room
  // failures collapse to an empty list so one broken room doesn't
  // nuke the chip strip for the rest. The endpoint is light (P3
  // serves from in-memory state) so a one-shot fan-out is cheap.
  const loadAllRanks = useCallback(async (roomIds: string[]) => {
    if (roomIds.length === 0) {
      setRanksByRoom({});
      return;
    }
    const settled = await Promise.all(
      roomIds.map(async (id) => {
        try {
          const rows = await api.fetchRoomRanks(id);
          return [id, rows] as const;
        } catch {
          return [id, [] as api.GroupChatRank[]] as const;
        }
      }),
    );
    setRanksByRoom(Object.fromEntries(settled));
  }, []);

  useEffect(() => { loadRooms(); }, [loadRooms]);
  // P4 — refresh rank chips whenever the room list changes (initial
  // load, new room created, room deleted). We re-fetch every visible
  // room in parallel; rank changes are rare so polling isn't needed.
  useEffect(() => {
    loadAllRanks(rooms.map((r) => r.id));
  }, [rooms, loadAllRanks]);
  useEffect(() => {
    if (activeRoomId) loadMessages(activeRoomId);
    else setMessages([]);
    // Switching rooms means any in-flight typing state from the old
    // room is meaningless — the WS connection is per-room anyway, so
    // we'd never get a matching `agent_done` for the old room here.
    setStreamingAgents(new Map());
  }, [activeRoomId, loadMessages]);

  // Re-fetch rooms when activeRoomId points to a room not yet in the list
  // (e.g. sidebar created a new room while groupchat page was already open)
  useEffect(() => {
    if (activeRoomId && !rooms.some((r) => r.id === activeRoomId)) {
      loadRooms();
    }
  }, [activeRoomId, rooms, loadRooms]);

  // Auto-select first room when none selected and rooms are available
  useEffect(() => {
    if (!activeRoomId && rooms.length > 0) {
      setActiveRoomId(rooms[0].id);
    }
  }, [activeRoomId, rooms]);

  // Refresh agents periodically — only re-render when the list
  // actually changed (compare ids to avoid JSON serialisation cost).
  useEffect(() => {
    const interval = setInterval(() => {
      const fresh = loadAgents();
      setAgents((prev) => {
        if (prev.length !== fresh.length) return fresh;
        const prevIds = prev.map((a) => a.id).sort().join(",");
        const freshIds = fresh.map((a) => a.id).sort().join(",");
        return prevIds !== freshIds ? fresh : prev;
      });
    }, 5000);
    return () => clearInterval(interval);
  }, []);

  const activeRoom = rooms.find((r) => r.id === activeRoomId);
  const roomMessages = messages.filter((m) => m.roomId === activeRoomId);
  const roomAgents = activeRoom ? agents.filter((a) => activeRoom.agentIds.includes(a.id)) : [];

  const handleCreateRoom = async () => {
    const trimmed = newRoomName.trim();
    if (!trimmed) return;
    setCreatingRoom(true);
    setCreateRoomError(null);
    try {
      const room = await api.createGroupChatRoom(trimmed);
      setRooms((prev) => [...prev, {
        id: room.id,
        name: room.name,
        agentIds: room.agent_ids,
        leaderId: room.leader_id,
        leader: room.leader ?? null,
        memberModels: {},
        memberDisplayNames: {},
        announcement: room.announcement ?? null,
        announcementPublishedAt: room.announcement_published_at ?? null,
        activeTask: room.active_task ?? null,
      }]);
      setActiveRoomId(room.id);
      setNewRoomName("");
      setShowNewRoom(false);
    } catch (e) {
      // Surface the failure inline instead of dropping it on the
      // floor via console.error — the user previously saw the
      // button "do nothing" with no clue whether the click
      // registered, the API was unreachable, or the backend
      // rejected the name.
      const msg = e instanceof Error ? e.message : String(e);
      console.error("Failed to create room:", e);
      setCreateRoomError(`创建聊天室失败:${msg}`);
    } finally {
      setCreatingRoom(false);
    }
  };

  // Clear the create-form error as soon as the user edits the name —
  // a stale error from a previous attempt is more noise than signal
  // once the input has changed.
  const handleNewRoomNameChange = (v: string) => {
    setNewRoomName(v);
    if (createRoomError) setCreateRoomError(null);
  };

  const handleDeleteRoom = async (id: string) => {
    if (!window.confirm("确认解散该会话？这将清空全部聊天内容和产物，此操作不可撤销。")) return;
    try {
      await api.deleteGroupChatRoom(id);
      setRooms((prev) => {
        const next = prev.filter((r) => r.id !== id);
        // If we deleted the active room, switch to the first remaining
        setActiveRoomId((curr) => (curr === id ? next[0]?.id ?? null : curr));
        return next;
      });
      setMessages((prev) => prev.filter((m) => m.roomId !== id));
    } catch (e) {
      console.error("Failed to delete room:", e);
    }
  };

  const handleSendMessage = async (textOverride?: string) => {
    // 优先用 ref 读取（新版 API 由 ChatComposerInput 自己 trim 过）
    const text = (textOverride ?? composerRef.current?.getValue() ?? "").trim();
    if (!text || !activeRoomId) return;
    composerRef.current?.clear();
    setLoading(true);

    // Optimistic local message
    const localId = `local-${Date.now()}`;
    const localMsg: ChatMsg = {
      id: localId,
      roomId: activeRoomId,
      senderId: "user",
      senderName: "你",
      content: text,
      messageType: "chat",
      timestamp: Date.now(),
    };
    setMessages((prev) => [...prev, localMsg]);

    // ── WebSocket streaming path ──────────────────────────
    // Open a one-shot WS connection and stream each agent's reply.
    let ws: WebSocket | null = null;
    try {
      ws = new WebSocket(api.resolveGroupChatWebSocketUrl(activeRoomId));

      // Wait for the socket to open
      await new Promise<void>((resolve, reject) => {
        if (!ws) return reject(new Error("ws is null"));
        ws.onopen = () => resolve();
        ws.onerror = () => reject(new Error("WebSocket connection failed"));
        // Safety timeout
        const to = setTimeout(() => reject(new Error("WebSocket timeout")), 10_000);
        ws.onopen = () => { clearTimeout(to); resolve(); };
      });

      // Send the user message
      ws.send(JSON.stringify({ type: "send", text }));

      // A map of agent_id → ChatMsg for in-flight streaming messages
      const agentMsgs = new Map<string, ChatMsg>();

      // Process incoming events
      await new Promise<void>((resolve, reject) => {
        if (!ws) return reject(new Error("ws is null"));
        let settled = false;
        const settle = (err?: Error) => {
          if (settled) return;
          settled = true;
          if (err) reject(err);
          else resolve();
        };
        ws.onerror = () => settle(new Error("WebSocket error"));
        ws.onclose = () => settle();
        ws.onmessage = (event) => {
          try {
            const env = JSON.parse(event.data);
            const evt = env.event;
            if (!evt) return;

            if (evt.connected) {
              // Connection established, nothing to do
              return;
            }

            if (evt.group_chat_chunk) {
              const { agent_id, agent_name, content } = evt.group_chat_chunk;
              const isLeaderChunk = agent_id === LEADER_SENDER_ID;
              // Mark agent as currently streaming (drives the typing
              // indicator). The leader and members share the same Map
              // so we render one indicator row per active agent.
              setStreamingAgents((prev) => {
                if (prev.has(agent_id)) return prev;
                const next = new Map(prev);
                next.set(agent_id, {
                  name: agent_name || (isLeaderChunk ? "项目总管" : agent_id),
                  role: isLeaderChunk ? "leader" : "member",
                });
                return next;
              });
              // Leader's stream is **only** a thinking trace — its final
              // text is consumed by `parse_task_plan` server-side and
              // never saved as a chat message. So we skip the
              // `setMessages` append for the leader entirely. Members
              // append chunks into their running bubble as before.
              if (!isLeaderChunk) {
                setMessages((prev) => {
                  const existing = agentMsgs.get(agent_id);
                  if (existing) {
                    // Append to existing message
                    existing.content += content;
                    return prev.map((m) => m.id === existing.id ? { ...existing } : m);
                  } else {
                    // Create new agent message
                    const newMsg: ChatMsg = {
                      id: `stream-${agent_id}-${Date.now()}`,
                      roomId: activeRoomId,
                      senderId: agent_id,
                      senderName: agent_name || agent_id,
                      content,
                      messageType: "chat",
                      timestamp: Date.now(),
                    };
                    agentMsgs.set(agent_id, newMsg);
                    return [...prev, newMsg];
                  }
                });
              }
              return;
            }

            if (evt.group_chat_agent_done) {
              // Agent finished streaming — drop the typing indicator.
              // Leader finishes silently (no message to save); members
              // already have their buffered bubble in `messages`.
              const { agent_id } = evt.group_chat_agent_done;
              setStreamingAgents((prev) => {
                if (!prev.has(agent_id)) return prev;
                const next = new Map(prev);
                next.delete(agent_id);
                return next;
              });
              return;
            }

            // S5.1 — task-engine events arrive on the same socket after
            // group_chat_done. Forward into the room's timeline so the
            // TaskPanel can render them live. The server keeps the
            // socket open until `room_completed`.
            if (evt.task_phase_change) {
              const { node_id, description, phase, assigned_agent } = evt.task_phase_change;
              appendEngineEvent(activeRoomId, {
                kind: "task_phase_change",
                node_id,
                description,
                phase,
                assigned_agent,
              });
              return;
            }
            if (evt.bus_delivered) {
              const { agent_id, message_kind, text } = evt.bus_delivered;
              appendEngineEvent(activeRoomId, {
                kind: "bus_delivered",
                agent_id,
                message_kind,
                text,
              });
              return;
            }
            if (evt.audit_verdict) {
              const { agent_id, violation_id, level, score_delta, score_after, reason } = evt.audit_verdict;
              appendEngineEvent(activeRoomId, {
                kind: "audit_verdict",
                agent_id,
                violation_id,
                level,
                score_delta,
                score_after,
                reason,
              });
              return;
            }
            if (evt.agent_expelled) {
              const { agent_id, agent_name, reason } = evt.agent_expelled;
              appendEngineEvent(activeRoomId, {
                kind: "agent_expelled",
                agent_id,
                agent_name,
                reason,
              });
              return;
            }
            if (evt.room_completed) {
              appendEngineEvent(activeRoomId, { kind: "room_completed" });
              return;
            }

            if (evt.group_chat_done) {
              settle();
              return;
            }

            if (evt.error) {
              settle(new Error(evt.error.message || "Stream error"));
              return;
            }
          } catch (e) {
            // Ignore parse errors on individual events
          }
        };
      });

      // Close WS
      ws.close();

      // After streaming completes, reload messages from REST to get
      // properly persisted versions (with metadata, timestamps etc.)
      try {
        const apiMsgs = await api.getGroupChatMessages(activeRoomId);
        setMessages(apiMsgs.map(apiMsgToChatMsg));
      } catch {
        // Keep the streaming messages if reload fails
      }
    } catch (e) {
      // Rollback optimistic message and re-enable input
      setMessages((prev) => prev.filter((m) => m.id !== localId));
      composerRef.current?.clear();
      console.error("GroupChat streaming failed:", e);
    } finally {
      ws?.close();
      setLoading(false);
      // Clear any leftover typing indicators — the WS is gone so no
      // further `agent_done` event will arrive. Stale indicators
      // would freeze the UI in "thinking…" forever.
      setStreamingAgents(new Map());
    }
  };

  // Room tone — used to color the chat-area hero so each room feels
  // visually distinct without us having to author per-room themes.
  const roomTone = activeRoom ? agentTone(activeRoom.id) : null;

  return (
    <>
    <div className="h-full flex overflow-hidden bg-bg-primary p-3 gap-3">
      {/* ── Left column — room list ── */}
      <div className="w-60 shrink-0 flex flex-col bg-bg-primary">
        <div className="px-4 py-3 flex items-center justify-between relative z-10">
          <div className="flex items-center gap-2">
            <div
              className="w-6 h-6 rounded-lg flex items-center justify-center"
              style={{
                // 单一颜色 — 渐变禁止
                background: "hsl(220, 35%, 92%)",
                color: "hsl(220, 50%, 40%)",
              }}
            >
              <Users size={13} />
            </div>
            <h2 className="text-[14px] font-semibold text-text-primary">群聊</h2>
            <span className="text-[10px] text-text-faint font-mono px-1.5 py-0.5 rounded-md bg-bg-tertiary">
              {rooms.length}
            </span>
          </div>
          <button
            onClick={() => setShowNewRoom(true)}
            className="w-7 h-7 rounded-lg flex items-center justify-center text-white shrink-0 transition-all hover:scale-105 hover:shadow-md"
            style={{ background: "var(--color-btn-primary-bg)" }}
            title="新建聊天室"
          >
            <Plus size={14} />
          </button>
        </div>

        {showNewRoom && (
          <div className="px-3 py-2 space-y-1.5">
            {createRoomError && (
              <InlineError message={createRoomError} onDismiss={() => setCreateRoomError(null)} />
            )}
            <input
              autoFocus
              value={newRoomName}
              onChange={(e) => handleNewRoomNameChange(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !creatingRoom) { e.preventDefault(); handleCreateRoom(); }
                if (e.key === "Escape") { setShowNewRoom(false); setNewRoomName(""); setCreateRoomError(null); }
              }}
              placeholder="聊天室名称"
              disabled={creatingRoom}
              className="w-full rounded-lg bg-bg-tertiary px-3 py-2 text-[13px] text-text-primary placeholder:text-text-faint outline-none disabled:opacity-50"
            />
            <div className="flex gap-1.5">
              <button
                onClick={handleCreateRoom}
                disabled={!newRoomName.trim() || creatingRoom}
                className="flex-1 py-1.5 rounded-lg text-[12px] font-medium transition-opacity disabled:opacity-40 disabled:cursor-not-allowed"
                style={{ background: "var(--color-btn-primary-bg)", color: "var(--color-btn-primary-text)" }}
              >
                {creatingRoom ? "创建中…" : "创建"}
              </button>
              <button
                onClick={() => { setShowNewRoom(false); setNewRoomName(""); setCreateRoomError(null); }}
                disabled={creatingRoom}
                className="flex-1 py-1.5 rounded-lg text-[12px] font-medium transition-opacity disabled:opacity-40 disabled:cursor-not-allowed"
                style={{ background: "var(--color-btn-secondary-bg)", color: "var(--color-btn-secondary-text)" }}
              >
                取消
              </button>
            </div>
          </div>
        )}

        {/* L5 — room search */}
        {rooms.length > 3 && (
          <div className="px-3 pb-1">
            <div className="flex items-center gap-1.5 bg-bg-tertiary rounded-lg px-2.5 py-1.5">
              <Search size={12} className="text-text-faint shrink-0" />
              <input
                type="text"
                value={roomSearch}
                onChange={(e) => setRoomSearch(e.target.value)}
                placeholder="搜索房间…"
                className="flex-1 bg-transparent text-[11px] text-text-primary placeholder:text-text-faint outline-none"
              />
              {roomSearch && (
                <button
                  type="button"
                  onClick={() => setRoomSearch("")}
                  className="text-text-faint hover:text-text-secondary"
                >
                  <X size={11} />
                </button>
              )}
            </div>
          </div>
        )}

        <div className="flex-1 overflow-y-auto p-2 space-y-1">
          {rooms.length === 0 && !showNewRoom && (
            <div className="flex flex-col items-center py-10 text-center">
              <MessageCircle size={32} className="text-text-faint mb-3" />
              <p className="text-[13px] text-text-muted">暂无聊天室</p>
              <p className="text-[11px] text-text-faint mt-1">点击 + 创建一个聊天室</p>
            </div>
          )}
          {rooms
            .filter((r) => !roomSearch || r.name.toLowerCase().includes(roomSearch.toLowerCase()))
            .map((room) => (
            <RoomItem
              key={room.id}
              room={room}
              agents={agents}
              active={room.id === activeRoomId}
              onSelect={() => setActiveRoomId(room.id)}
              onDelete={() => handleDeleteRoom(room.id)}
              ranks={ranksByRoom[room.id]}
            />
          ))}
        </div>
      </div>

      {/* ── Chat area ──
       *
       * Container is plain bg-secondary. The hero strip at the top
       * carries the room's tinted color as a single flat block
       * (渐变禁止 — 用纯色 + 边框做层次).
       */}
      <div
        className="flex-1 flex flex-col min-w-0 rounded-2xl overflow-hidden border border-border-subtle"
        style={{ background: "var(--color-bg-secondary)" }}
      >
        {activeRoom && roomTone ? (
          <>
            {/* Hero strip — room name + member roster avatar wall */}
            <div
              className="relative px-5 pt-3 pb-2.5 border-b border-border-subtle"
              style={{ background: roomTone.hero }}
            >
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2 mb-1">
                    <span
                      className="text-[10px] font-bold uppercase tracking-wider px-1.5 py-0.5 rounded-md"
                      style={{
                        background: roomTone.dot + "33",
                        color: roomTone.text,
                      }}
                    >
                      群聊
                    </span>
                    <h3 className="text-[18px] font-bold text-text-primary truncate">
                      {activeRoom.name}
                    </h3>
                  </div>
                  <p className="text-[11.5px] text-text-muted flex items-center gap-1.5 flex-wrap">
                    <span>
                      {activeRoom.leader && roomAgents.length > 0
                        ? `总管 + ${roomAgents.length} 位成员 · 协作中`
                        : activeRoom.leader
                          ? "总管已就位 · 在下方邀请成员加入"
                          : roomAgents.length > 0
                            ? `${roomAgents.length} 位成员 · 协作中`
                            : "暂未邀请成员 · 在下方邀请加入"}
                    </span>
                    {/* P4 — inline tier breakdown so opening a room
                        shows the same rank state the room-list chip
                        previews. Hidden while ranks are loading. */}
                    <RoomRankSummary ranks={ranksByRoom[activeRoom.id]} />
                  </p>
                </div>
                <div className="flex items-center gap-1.5 shrink-0">
                  <button
                    onClick={() => taskDialogRef.current?.open()}
                    className={clsx(
                      "btn btn-secondary btn-sm relative",
                      activeRoom.activeTask && "border",
                    )}
                    style={activeRoom.activeTask ? {
                      borderColor: "hsla(43, 96%, 56%, 0.40)",
                      background: "hsla(43, 96%, 56%, 0.10)",
                      color: "hsl(33, 90%, 32%)",
                    } : undefined}
                    title={activeRoom.activeTask
                      ? `当前任务：${activeRoom.activeTask.original_prompt.slice(0, 30)}…`
                      : "无活跃任务"}
                  >
                    <ListChecks size={12} />
                    任务
                    {activeRoom.activeTask && (
                      <span
                        className="absolute -top-0.5 -right-0.5 w-2 h-2 rounded-full animate-pulse"
                        style={{ background: "hsl(43, 96%, 48%)" }}
                      />
                    )}
                  </button>
                  <button onClick={() => memberDialogRef.current?.open()} className="btn btn-secondary btn-sm">
                    <Users size={12} /> 成员管理
                  </button>
                  <button onClick={() => settingsDialogRef.current?.open()} className="btn btn-secondary btn-sm">
                    <Settings size={12} /> 规则
                  </button>
                </div>
              </div>

              {/* Avatar wall — overlapping circle pile with online dots.
                  Leader (项目总管) always sits at the front with a
                  gold tint; member agents follow. The wall renders
                  even when the room has zero user-invited agents,
                  because the leader is system-present from creation.
                  Hover a member avatar to reveal the × button — kicks
                  the agent from the room (the agent itself is preserved). */}
              {(activeRoom.leader || roomAgents.length > 0) && (
                <div className="mt-3 flex items-center gap-3">
                  <div className="flex items-center">
                    {/* Leader always first, gold tinted, fixed z-index */}
                    {activeRoom.leader && (
                      <div
                        className="relative"
                        style={{ marginLeft: 0, zIndex: 100 }}
                      >
                        <LeaderAvatar size={30} ringed />
                      </div>
                    )}
                    {roomAgents.slice(0, 5).map((a, i) => {
                      const t = agentTone(a.id);
                      return (
                        <div
                          key={a.id}
                          className="group/agent relative"
                          style={{ marginLeft: -10, zIndex: roomAgents.length - i }}
                        >
                          <AgentAvatar agent={a} size={30} ringed />
                          <span
                            className="absolute -bottom-0.5 -right-0.5 w-2.5 h-2.5 rounded-full ring-2"
                            style={{
                              background: t.dot,
                              // ring color follows parent surface so the dot
                              // visually "punches through" the avatar edge
                              ...({ "--tw-ring-color": "var(--color-bg-primary)" } as React.CSSProperties),
                            }}
                          />
                        </div>
                      );
                    })}
                  </div>
                  <div className="text-[10.5px] text-text-muted leading-tight min-w-0">
                    <p className="font-semibold text-text-secondary">
                      {activeRoom.leader ? "项目总管 · 在岗" : "在线成员"}
                    </p>
                    <p className="truncate">
                      {activeRoom.leader && roomAgents.length > 0
                        ? `总管 + ${roomAgents.slice(0, 2).map((a) => a.name).join("、")}${roomAgents.length > 2 ? ` 等 ${roomAgents.length} 人` : ""}`
                        : activeRoom.leader
                          ? "等待团队成员加入"
                          : roomAgents.slice(0, 3).map((a) => a.name).join("、") + (roomAgents.length > 3 ? ` 等 ${roomAgents.length} 人` : "")}
                    </p>
                  </div>
                </div>
              )}
            </div>

            {/* Announcement now lives inside the TaskPanel — the chat
                surface no longer renders it directly. */}

            {/* Messages */}
            <div className="flex-1 overflow-y-auto px-5 py-4 space-y-3.5">
              {roomMessages.length === 0 ? (
                <div className="flex flex-col items-center justify-center py-12 text-center">
                  <div
                    className="w-20 h-20 rounded-2xl flex items-center justify-center text-4xl mb-4 shadow-sm"
                    style={{ background: roomTone.bg, boxShadow: `inset 0 0 0 1px ${roomTone.ring}55` }}
                  >
                    💬
                  </div>
                  <p className="text-[15px] font-semibold text-text-primary mb-1">开始一场群聊</p>
                  <p className="text-[12px] text-text-muted mb-5 max-w-[280px] leading-relaxed">
                    {roomAgents.length > 0
                      ? `${activeRoom.leader ? "项目总管已就位 · " : ""}${roomAgents.slice(0, 2).map((a) => a.name).join("、")} 等 ${roomAgents.length} 位成员正在待命`
                      : activeRoom.leader
                        ? "项目总管已就位，等待团队成员加入后开始协作"
                        : "邀请成员后，他们会基于各自角色协作回复你的消息"}
                  </p>
                  <button
                    onClick={() => memberDialogRef.current?.open()}
                    className="btn btn-primary btn-sm"
                  >
                    <Users size={13} /> 管理成员
                  </button>
                </div>
              ) : (
                roomMessages.map((msg) => {
                  const isUser = msg.senderId === "user";
                  const isLeader = msg.senderId === LEADER_SENDER_ID;
                  const isSystem = msg.senderId === SYSTEM_SENDER_ID;
                  // Resolve display name: first from agent list (name/role), fallback to backend senderName
                  const resolvedAgent = !isUser && !isLeader ? agents.find((a) => a.id === msg.senderId || `builtin:${a.id}` === msg.senderId || a.id === `builtin:${msg.senderId}`) : null;
                  const displayName = isLeader ? "项目总管" : (resolvedAgent ? (resolvedAgent.name || resolvedAgent.role || msg.senderName) : msg.senderName);
                  const tone = resolvedAgent ? agentTone(resolvedAgent.id) : null;
                  // PLAN_UI_ENHANCEMENT §4.2 (2026-06-22): 总管消息样式与其他成员
                  // 保持一致，仅靠头像 + 名称 + 总管小 chip 区分身份。
                  // 不再用居中卡片/金底/特殊 box-shadow。
                  if (isLeader) {
                    // Use a subtle hue-derived tone for leader bubble — consistent with member styling.
                    const leaderTone = agentTone(LEADER_SENDER_ID);
                    return (
                      <div key={msg.id} className="flex gap-2 items-start animate-in">
                        <div
                          className="w-7 h-7 rounded-full flex items-center justify-center shrink-0"
                          style={{
                            background: "hsla(33, 90%, 90%, 0.6)",
                            color: "hsl(33, 90%, 38%)",
                          }}
                          title="项目总管"
                        >
                          <Crown size={14} strokeWidth={2.4} />
                        </div>
                        <div className="min-w-0 flex-1">
                          <div className="flex items-center gap-1.5 mb-0.5">
                            <span className="text-[12px] text-text-primary font-medium">
                              {displayName}
                            </span>
                            <span
                              className="text-[9.5px] uppercase tracking-wider px-1 py-px rounded font-semibold"
                              style={{
                                background: "hsla(43, 96%, 56%, 0.18)",
                                color: "hsl(33, 90%, 38%)",
                              }}
                            >
                              总管
                            </span>
                            <span className="text-[10px] text-text-faint ml-auto flex items-center gap-1">
                              <button
                                type="button"
                                className="hover:text-text-secondary transition-colors"
                                title="复制消息"
                                onClick={() => navigator.clipboard.writeText(msg.content)}
                              >
                                <Copy size={10} />
                              </button>
                              {new Date(msg.timestamp).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })}
                            </span>
                          </div>
                          <div
                            className="rounded-xl px-3.5 py-2 text-[13px] leading-snug whitespace-pre-wrap break-words"
                            style={{
                              background: leaderTone.bg,
                              color: "var(--color-text-primary)",
                              boxShadow: `inset 0 0 0 1px ${leaderTone.ring}33`,
                            }}
                          >
                            <MarkdownBody content={msg.content} />
                            {msg.metadata && (
                              <div className="mt-2 pt-2 border-t border-border-subtle">
                                <button
                                  type="button"
                                  onClick={() => setShowRaw((p) => {
                                    const next = new Set(p);
                                    if (next.has(msg.id)) next.delete(msg.id);
                                    else next.add(msg.id);
                                    return next;
                                  })}
                                  className="text-[10.5px] text-text-faint hover:text-text-secondary transition-colors"
                                >
                                  {showRaw.has(msg.id) ? "收起思考过程" : "展开思考过程"}
                                </button>
                                {showRaw.has(msg.id) && (
                                  <pre className="mt-1.5 text-[11px] leading-snug text-text-muted whitespace-pre-wrap break-words overflow-x-auto max-h-40 overflow-y-auto rounded-lg bg-bg-tertiary p-2">
                                    {(() => {
                                      try {
                                        const parsed = JSON.parse(msg.metadata);
                                        return parsed.raw ?? "(无原始内容)";
                                      } catch { return "(无法解析)"; }
                                    })()}
                                  </pre>
                                )}
                              </div>
                            )}
                          </div>
                        </div>
                      </div>
                    );
                  }
                  // ── System hint (fleet failure) ──────────────
                  if (isSystem) {
                    return (
                      <div key={msg.id} className="flex justify-center animate-in pt-1">
                        <div
                          className="inline-flex items-center gap-2 px-3.5 py-2 rounded-xl border"
                          style={{
                            background: "var(--color-bg-tertiary)",
                            borderColor: "var(--color-border-subtle)",
                          }}
                        >
                          <span className="w-5 h-5 rounded-full flex items-center justify-center shrink-0 bg-text-faint/20">
                            <span className="text-[10.5px] leading-none font-bold" style={{ opacity: 0.5 }}>!</span>
                          </span>
                          <span className="text-[11.5px] leading-relaxed text-text-faint text-center">
                            {msg.content}
                          </span>
                        </div>
                      </div>
                    );
                  }
                  return (
                    <div
                      key={msg.id}
                      className={clsx("flex gap-2 items-start animate-in", isUser && "justify-end")}
                    >
                      {!isUser && resolvedAgent && (
                        <AgentAvatar agent={resolvedAgent} size={30} />
                      )}
                      <div className={clsx("max-w-[68%] flex flex-col", isUser && "items-end")}>
                        {/* Name header row — avatar name + time aligned left */}
                        {!isUser && (
                          <div className="flex items-center gap-1.5 mb-1 px-1">
                            <span
                              className="text-[11px] font-semibold truncate max-w-[120px]"
                              style={{ color: tone?.text ?? "var(--color-text-secondary)" }}
                              title={displayName}
                            >
                              {displayName}
                            </span>
                            <span className="text-[10px] text-text-faint flex items-center gap-1">
                              <button
                                type="button"
                                className="hover:text-text-secondary transition-colors"
                                title="复制消息"
                                onClick={() => navigator.clipboard.writeText(msg.content)}
                              >
                                <Copy size={10} />
                              </button>
                              {new Date(msg.timestamp).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })}
                            </span>
                          </div>
                        )}
                        <div
                          className={clsx(
                            "rounded-2xl px-3.5 py-2.5 text-[13px] leading-snug",
                            isUser ? "rounded-br-md" : "rounded-bl-md",
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
                                    boxShadow: `inset 0 0 0 1px ${tone.ring}33`,
                                  }
                                : {
                                    background: "var(--color-bg-tertiary)",
                                    color: "var(--color-text-primary)",
                                  }
                          }
                        >
                          <div className="text-[13px] leading-snug">
                            <MarkdownBody content={msg.content} />
                          </div>
                          {/* Collapsible raw content for agent messages with metadata */}
                          {!isUser && msg.metadata && (
                            <div className="mt-1.5 pt-1.5 border-t border-border-subtle">
                              <button
                                type="button"
                                onClick={() => setShowRaw((p) => {
                                  const next = new Set(p);
                                  if (next.has(msg.id)) next.delete(msg.id);
                                  else next.add(msg.id);
                                  return next;
                                })}
                                className="text-[10.5px] text-text-faint hover:text-text-secondary transition-colors"
                              >
                                {showRaw.has(msg.id) ? "收起思考过程" : "展开思考过程"}
                              </button>
                              {showRaw.has(msg.id) && (
                                <pre className="mt-1.5 text-[11px] leading-snug text-text-muted whitespace-pre-wrap break-words overflow-x-auto max-h-40 overflow-y-auto rounded-lg bg-bg-tertiary p-2">
                                  {(() => {
                                    try {
                                      const parsed = JSON.parse(msg.metadata);
                                      return parsed.raw ?? "(无原始内容)";
                                    } catch { return "(无法解析)"; }
                                  })()}
                                </pre>
                              )}
                            </div>
                          )}
                        </div>
                        {isUser && (
                          <p className="text-[10px] text-text-faint mt-1 px-1 flex items-center gap-1">
                            <button
                              type="button"
                              className="hover:text-text-secondary transition-colors"
                              title="复制消息"
                              onClick={() => navigator.clipboard.writeText(msg.content)}
                            >
                              <Copy size={10} />
                            </button>
                            {new Date(msg.timestamp).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })}
                          </p>
                        )}
                      </div>
                    </div>
                  );
                })
              )}

              {/* ── Live typing indicators ──
                  One row per agent that is currently streaming. Sits
                  below the message list and above the auto-scroll
                  anchor so newly-arriving dots stay in view. */}
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
              <div ref={chatEndRef} className="h-0 w-0" />
            </div>

            {/* Input — PLAN_UI_ENHANCEMENT §4.1: 用共享 ChatComposerInput 替代内联 input。 */}
            <div className="px-5 py-3 border-t border-border-subtle bg-bg-primary">
              <ChatComposerInput
                ref={composerRef}
                onSend={handleSendMessage}
                isStreaming={loading}
                placeholder={
                  roomAgents.length > 0
                    ? `向 ${roomAgents.length} 位成员提问…`
                    : "邀请成员开始对话…"
                }
              />
              {loading && (
                <div className="flex items-center gap-1.5 mt-2 text-[11px] text-text-muted">
                  <span className="flex gap-0.5">
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
                  <span>成员们正在讨论…</span>
                </div>
              )}
            </div>
          </>
        ) : (
          <div className="flex-1 flex items-center justify-center">
            <div className="empty">
              <div className="empty-icon"><Users size={24} /></div>
              <p className="empty-title">选择或创建聊天室</p>
              <p className="empty-desc">在左侧创建聊天室并邀请成员开始对话</p>
            </div>
          </div>
        )}
      </div>
    </div>
    {/* Dialogs: always mounted, toggled via CSS display for instant open/close */}
    <ImperativeDialog ref={memberDialogRef}>
      {activeRoom && (
        <RoomMemberDialog
          roomId={activeRoom.id}
          roomName={activeRoom.name}
          agents={agents}
          memberIds={activeRoom.agentIds}
          memberModels={activeRoom.memberModels}
          memberDisplayNames={activeRoom.memberDisplayNames}
          leader={activeRoom.leader}
          onClose={() => memberDialogRef.current?.close()}
          onMembershipChanged={loadRooms}
        />
      )}
    </ImperativeDialog>
    <ImperativeDialog ref={settingsDialogRef}>
      {activeRoom && (
        <RoomSettingsDrawer
          roomId={activeRoom.id}
          roomName={activeRoom.name}
          onClose={() => settingsDialogRef.current?.close()}
        />
      )}
    </ImperativeDialog>
    <ImperativeDialog ref={taskDialogRef}>
      {activeRoom && (
        <TaskPanel
          roomId={activeRoom.id}
          roomName={activeRoom.name}
          activeTask={activeRoom.activeTask}
          agents={agents}
          liveEvents={engineEventsByRoom[activeRoom.id]}
          announcement={activeRoom.announcement}
          announcementPublishedAt={activeRoom.announcementPublishedAt}
          onClose={() => taskDialogRef.current?.close()}
          onTaskChanged={loadRooms}
          onAnnouncementChanged={(text, publishedAt) => {
            setRooms((prev) =>
              prev.map((r) =>
                r.id === activeRoom.id
                  ? { ...r, announcement: text, announcementPublishedAt: publishedAt }
                  : r,
              ),
            );
          }}
        />
      )}
    </ImperativeDialog>
    </>
  );
}