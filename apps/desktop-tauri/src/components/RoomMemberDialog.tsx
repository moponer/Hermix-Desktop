import { useState, useCallback, useEffect, useMemo } from "react";
import { X, Users, UserPlus, LogOut, Check, AlertCircle, Search, Cpu, Crown, Pencil, Tag, ThumbsUp, ThumbsDown } from "lucide-react";
import { clsx } from "clsx";
import * as api from "../api";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";
import type { AgentPersona } from "./agents/types";
import { useSafeTimeout } from "../hooks/useSafeTimeout";
import { withAlpha } from "./groupchat/types";

interface RoomMemberDialogProps {
  roomId: string;
  roomName: string;
  agents: AgentPersona[];
  memberIds: string[];
  /** Per-member model override from the server. Maps agent_id → model
   *  id (e.g. "claude-sonnet-4.5") or `undefined` if no override set. */
  memberModels?: Record<string, string | undefined>;
  /** Per-member display name (别名) from the server. Maps agent_id →
   *  alias or `undefined` if not set (use agent's global name). */
  memberDisplayNames?: Record<string, string | undefined>;
  /** The built-in leader (项目总管) virtual identity, if the room
   *  has one. Older rooms (pre-P1) won't have this; we just hide
   *  the leader row in that case. */
  leader?: api.GroupChatLeader | null;
  onClose: () => void;
  /** Notify the parent that the room's membership changed so it can
   *  re-fetch rooms from the server (covers the rare case where the
   *  server's role/time fields differ from what we have locally). */
  onMembershipChanged?: () => void;
}

interface ModelOption {
  /** "provider:modelId" — this is what the agent loader consumes. */
  fullId: string;
  providerId: string;
  modelId: string;
}

/** Pull the user-configured model list from localStorage. These are
 *  exactly the "已经配置可以用的" models — anything not in this list
 *  would fail to load on the server, so we don't offer it. */
function loadEnabledModels(): ModelOption[] {
  try {
    const raw = storage.getItem(STORAGE_KEYS.modelConfig);
    if (!raw) return [];
    const cfg = JSON.parse(raw) as { enabled?: { providerId: string; modelId: string }[] };
    return (cfg.enabled ?? []).map((e) => ({
      fullId: `${e.providerId}:${e.modelId}`,
      providerId: e.providerId,
      modelId: e.modelId,
    }));
  } catch {
    return [];
  }
}

type Tab = "current" | "invite";

/** Modal dialog for managing room membership. Two tabs:
 *  ── 当前 (current): list of members currently in the room, each
 *     with a "请出" action on the right that kicks the agent
 *  ── 邀请 (invite): list of agents NOT in the room, each with an
 *     "邀请加入" action on the right that invites the agent
 *
 *  The actions are optimistic: local state updates immediately, the
 *  server call happens in the background, and on failure the change
 *  is reverted. A single inline error banner at the top surfaces
 *  the most recent failure (one banner, not one per row — the
 *  user's mental model is "one dialog, one status"). */
export function RoomMemberDialog({
  roomId,
  roomName,
  agents,
  memberIds,
  memberModels,
  memberDisplayNames,
  leader,
  onClose,
  onMembershipChanged,
}: RoomMemberDialogProps) {
  const safeSetTimeout = useSafeTimeout();
  const [tab, setTab] = useState<Tab>("current");
  const [localMemberIds, setLocalMemberIds] = useState<string[]>(memberIds);
  const [busyAgentId, setBusyAgentId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const [pendingRoles, setPendingRoles] = useState<Record<string, "member" | "observer">>({});
  // Local override map keyed by agent_id. `undefined` = use agent's default.
  const [overrides, setOverrides] = useState<Record<string, string | undefined>>(memberModels ?? {});
  // Local display-name map keyed by agent_id. `undefined` = use agent's
  // global name. Optimistic — flips locally on save, reverts on failure.
  const [displayNames, setDisplayNames] = useState<Record<string, string | undefined>>(memberDisplayNames ?? {});
  // P3 — per-agent rank snapshot keyed by agent_id. Loaded once when
  // the dialog opens; not auto-refreshed (rank updates are rare).
  const [ranks, setRanks] = useState<Record<string, api.GroupChatRank>>({});
  // Enabled models from localStorage. Loaded once on mount.
  const enabledModels = useMemo(() => loadEnabledModels(), []);

  // Keep local in sync if the parent re-loads rooms (e.g. external refresh)
  useEffect(() => { setLocalMemberIds(memberIds); }, [memberIds]);
  useEffect(() => { setOverrides(memberModels ?? {}); }, [memberModels]);
  useEffect(() => { setDisplayNames(memberDisplayNames ?? {}); }, [memberDisplayNames]);
  // P3 — fetch rank snapshots when the room opens. Failures are
  // swallowed (rank is decorative — empty state still works).
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const rows = await api.fetchRoomRanks(roomId);
        if (cancelled) return;
        setRanks(Object.fromEntries(rows.map((r) => [r.agent_id, r])));
      } catch {
        if (!cancelled) setRanks({});
      }
    })();
    return () => { cancelled = true; };
  }, [roomId]);

  // ESC closes the dialog
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const memberSet = useMemo(() => new Set(localMemberIds), [localMemberIds]);

  // Filtered lists — current members / invite candidates — based on the
  // shared search box. Empty `search` shows everything.
  const matchAgent = useCallback((a: AgentPersona, q: string) => {
    if (!q) return true;
    return (
      a.name.toLowerCase().includes(q) ||
      a.role.toLowerCase().includes(q) ||
      a.identity.toLowerCase().includes(q)
    );
  }, []);

  const memberList = useMemo(() => {
    const q = search.trim().toLowerCase();
    return localMemberIds
      .map((id) => agents.find((a) => a.id === id))
      .filter((a): a is AgentPersona => Boolean(a))
      .filter((a) => matchAgent(a, q));
  }, [agents, localMemberIds, search, matchAgent]);

  const inviteList = useMemo(() => {
    const q = search.trim().toLowerCase();
    return agents
      .filter((a) => !memberSet.has(a.id))
      .filter((a) => matchAgent(a, q));
  }, [agents, memberSet, search, matchAgent]);

  const handleKick = useCallback(async (agentId: string) => {
    if (!memberSet.has(agentId)) return; // not a member, nothing to do
    const prev = localMemberIds;
    setLocalMemberIds((curr) => curr.filter((id) => id !== agentId));
    setError(null);
    setBusyAgentId(agentId);
    try {
      await api.kickAgentFromRoom(roomId, agentId);
      onMembershipChanged?.();
    } catch (e) {
      setLocalMemberIds(prev); // revert
      setError(`请出失败:${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusyAgentId(null);
    }
  }, [localMemberIds, memberSet, roomId, onMembershipChanged]);

  // P1.4 — vote on a member. Fires optimistically and updates
  // the local rank snapshot on success.
  // P1.5 — shows a brief toast when tier transitions (升段/掉段).
  const [toast, setToast] = useState<string | null>(null);
  const handleVote = useCallback(async (agentId: string, direction: "up" | "down") => {
    try {
      const updated = await api.sendGroupChatVote(roomId, agentId, direction);
      setRanks((prev) => ({ ...prev, [agentId]: updated }));
      // P1.5 — tier transition toast
      if (updated.previous_tier) {
        const isUp = direction === "up";
        setToast(`${isUp ? "升段!" : "掉段..."} ${updated.previous_tier} → ${updated.tier_cn}`);
        safeSetTimeout(() => setToast(null), 2500);
      }
    } catch (e) {
      console.warn("vote failed:", e);
    }
  }, [roomId]);

  const handleInvite = useCallback(async (agentId: string) => {
    if (memberSet.has(agentId)) return;
    const prev = localMemberIds;
    const role = pendingRoles[agentId] || "member";
    setLocalMemberIds((curr) => [...curr, agentId]);
    setError(null);
    setBusyAgentId(agentId);
    try {
      await api.inviteAgentToRoom(roomId, agentId, role);
      onMembershipChanged?.();
    } catch (e) {
      setLocalMemberIds(prev);
      setError(`邀请失败:${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusyAgentId(null);
    }
  }, [localMemberIds, memberSet, roomId, onMembershipChanged, pendingRoles]);

  /** Set or clear the per-room model override for one member. */
  const handleSetModel = useCallback(async (agentId: string, model: string | null) => {
    const prev = overrides[agentId];
    // Optimistic update — local state flips immediately, server call in background.
    setOverrides((curr) => {
      const next = { ...curr };
      if (model && model.length > 0) next[agentId] = model;
      else delete next[agentId];
      return next;
    });
    setError(null);
    setBusyAgentId(agentId);
    try {
      await api.updateMemberModel(roomId, agentId, model);
      onMembershipChanged?.();
    } catch (e) {
      setOverrides((curr) => {
        const next = { ...curr };
        if (prev !== undefined) next[agentId] = prev;
        else delete next[agentId];
        return next;
      });
      setError(`设置模型失败:${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusyAgentId(null);
    }
  }, [overrides, roomId, onMembershipChanged]);

  /** Set or clear the per-room display name (别名) for one member.
   *  Empty / whitespace-only is treated as "clear alias" — the server
   *  normalises it to `null` so the row falls back to the agent's
   *  global name. Optimistic + revert on failure. */
  const handleSetDisplayName = useCallback(async (agentId: string, displayName: string | null) => {
    const trimmed = displayName?.trim() ?? "";
    const normalized: string | null = trimmed.length > 0 ? trimmed : null;
    const prev = displayNames[agentId];
    setDisplayNames((curr) => {
      const next = { ...curr };
      if (normalized) next[agentId] = normalized;
      else delete next[agentId];
      return next;
    });
    setError(null);
    setBusyAgentId(agentId);
    try {
      await api.updateMemberDisplayName(roomId, agentId, normalized);
      onMembershipChanged?.();
    } catch (e) {
      setDisplayNames((curr) => {
        const next = { ...curr };
        if (prev !== undefined) next[agentId] = prev;
        else delete next[agentId];
        return next;
      });
      setError(`设置别名失败:${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusyAgentId(null);
    }
  }, [displayNames, roomId, onMembershipChanged]);

  const counts = { current: memberList.length, invite: inviteList.length };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center p-4"
      style={{ background: "var(--color-overlay-scrim)", contain: "layout style paint" }}
      onClick={onClose}
    >
      <div
        onClick={(e) => e.stopPropagation()}
        className="w-full max-w-2xl max-h-[85vh] rounded-2xl bg-bg-elevated border border-border-primary shadow-pop overflow-hidden flex flex-col relative"
        style={{ transform: "translateZ(0)" }}
      >
        {/* P1.5 — tier transition toast */}
        {toast && (
          <div className="absolute top-2 left-1/2 -translate-x-1/2 z-10 px-4 py-2 rounded-xl text-[13px] font-semibold animate-in fade-in slide-in-from-top-2"
            style={{
              background: "hsla(43, 96%, 56%, 0.95)",
              color: "hsl(33, 90%, 15%)",
              boxShadow: "0 2px 12px hsla(43, 96%, 56%, 0.4)",
            }}
          >
            {toast}
          </div>
        )}
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 bg-bg-secondary">
          <div className="flex items-center gap-2 min-w-0">
            <Users size={16} className="text-text-secondary shrink-0" />
            <h3 className="text-[14px] font-semibold text-text-primary truncate">
              {roomName} — 成员管理
            </h3>
          </div>
          <button
            onClick={onClose}
            className="p-1 rounded-md hover:bg-bg-hover text-text-muted shrink-0"
            aria-label="关闭"
          >
            <X size={16} />
          </button>
        </div>

        {/* Tabs */}
        <div className="flex bg-bg-secondary" role="tablist">
          {(["current", "invite"] as const).map((t) => (
            <button
              key={t}
              role="tab"
              aria-selected={tab === t}
              onClick={() => { setTab(t); setError(null); }}
              className={clsx(
                "flex-1 px-4 py-2.5 text-[12.5px] font-medium transition-colors flex items-center justify-center gap-1.5",
                tab === t
                  ? "text-accent border-b-2 border-accent"
                  : "text-text-muted hover:text-text-primary"
              )}
            >
              {t === "current" ? <Users size={12} /> : <UserPlus size={12} />}
              <span>{t === "current" ? "当前" : "邀请"}</span>
              <span
                className="text-[12px] px-1.5 py-px rounded-md font-semibold"
                style={{
                  background: tab === t ? "var(--color-accent-soft, rgba(71,85,105,0.10))" : "var(--color-bg-tertiary)",
                  color: tab === t ? "var(--color-accent)" : "var(--color-text-muted)",
                }}
              >
                {counts[t]}
              </span>
            </button>
          ))}
        </div>

        {/* Search + error */}
        <div className="px-4 py-2.5 bg-bg-secondary space-y-2">
          <div className="relative">
            <Search
              size={12}
              className="absolute left-2.5 top-1/2 -translate-y-1/2 text-text-faint pointer-events-none"
            />
            <input
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder={tab === "current" ? "搜索当前成员..." : "搜索可邀请的成员..."}
              className="w-full pl-7 pr-3 py-1.5 rounded-lg bg-bg-tertiary text-[13px] text-text-primary placeholder:text-text-faint outline-none focus:ring-1 focus:ring-accent/40"
            />
          </div>
          {error && (
            <div
              role="alert"
              className="flex items-start gap-2 px-2.5 py-1.5 rounded-lg text-[13px] leading-snug"
              style={{ background: "var(--color-error-soft)", color: "var(--color-error)" }}
            >
              <AlertCircle size={12} className="shrink-0 mt-[1px]" />
              <span className="flex-1 break-words">{error}</span>
              <button onClick={() => setError(null)} className="text-current opacity-60 hover:opacity-100">
                <X size={11} />
              </button>
            </div>
          )}
        </div>

        {/* Content */}
        <div className="flex-1 overflow-y-auto">
          {tab === "current" && (
            <>
              {/* Leader row — fixed at the top of the current list. The
                  built-in 总管 cannot be invited/kicked; model can be
                  overridden like any other member. Crown badge marks
                  it as a virtual identity the user controls indirectly. */}
              {leader && (
                <LeaderRow
                  leader={leader}
                  enabledModels={enabledModels}
                  override={overrides[leader.agent_id] ?? leader.model_override}
                  onSetModel={handleSetModel}
                />
              )}
              <MemberList
                items={memberList}
                agents={agents}
                emptyText={agents.length === 0 ? "请先在「专家」页面创建专家" : "暂无成员 · 切到「邀请」标签添加"}
                actionLabel="请出"
                actionIcon={<LogOut size={11} />}
                actionStyle="danger"
                busyAgentId={busyAgentId}
                onAction={handleKick}
                enabledModels={enabledModels}
                overrides={overrides}
                onSetModel={handleSetModel}
                displayNames={displayNames}
                onSetDisplayName={handleSetDisplayName}
                ranks={ranks}
                onVote={handleVote}
              />
            </>
          )}
          {tab === "invite" && (
            <ul className="bg-bg-secondary">
              {inviteList.length === 0 ? (
                <li className="flex flex-col items-center justify-center py-10 text-text-faint text-[13px] gap-2">
                  <Users size={20} className="opacity-40" />
                  <p>{agents.length === 0 ? "请先在「专家」页面创建专家" : "所有专家都已在房间内"}</p>
                </li>
              ) : inviteList.map((agent) => {
                const isBusy = busyAgentId === agent.id;
                const currentRole = pendingRoles[agent.id] || "member";
                return (
                  <li
                    key={agent.id}
                    className="flex items-center gap-3 px-4 py-2.5 hover:bg-bg-hover/40 transition-colors"
                  >
                    <AgentAvatar agent={agent} size={28} />
                    <div className="flex-1 min-w-0">
                      <p className="text-[14px] font-medium text-text-primary truncate">
                        {agent.name}
                      </p>
                      <p className="text-[12px] text-text-faint truncate">
                        {agent.role || agent.identity || "专家"}
                      </p>
                    </div>
                    <select
                      value={currentRole}
                      onChange={(e) => setPendingRoles((prev) => ({
                        ...prev,
                        [agent.id]: e.target.value as "member" | "observer",
                      }))}
                      disabled={isBusy}
                      className="text-[12px] px-1.5 py-1 rounded-md bg-bg-tertiary text-text-secondary border border-border-subtle outline-none"
                      title="总管由系统内置，无需指定"
                    >
                      <option value="member">成员</option>
                      <option value="observer">观察者</option>
                    </select>
                    <button
                      type="button"
                      disabled={isBusy}
                      onClick={() => handleInvite(agent.id)}
                      className="shrink-0 inline-flex items-center gap-1 px-2.5 py-1 rounded-lg text-[13px] font-medium transition-all hover:opacity-90"
                      style={{ background: "var(--color-btn-primary-bg)", color: "var(--color-btn-primary-text)" }}
                    >
                      <Check size={11} />
                      <span>{isBusy ? "处理中..." : "邀请"}</span>
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-end px-4 py-3 bg-bg-secondary">
          <button
            onClick={onClose}
            className="px-3 py-1.5 rounded-lg text-[13px] font-medium"
            style={{ background: "var(--color-btn-secondary-bg)", color: "var(--color-btn-secondary-text)" }}
          >
            完成
          </button>
        </div>
      </div>
    </div>
  );
}

/** Fixed leader row at the top of the current tab.
 *
 *  Visual distinction rules (no gradient — see project 视觉铁律):
 *   - solid tinted background (amber 8%) to set it apart from
 *     user-invited members
 *   - 👑 glyph avatar instead of agent letter
 *   - "系统内置" tag replaces the action button — leader can't be
 *     invited, kicked, or have its model overridden
 *
 *  Keeping the row the same height as member rows (44px content
 *  area) means the list stays aligned even as the user scrolls. */
function LeaderRow({
  leader,
  enabledModels,
  override,
  onSetModel,
}: {
  leader: api.GroupChatLeader;
  enabledModels: ModelOption[];
  override: string | null | undefined;
  onSetModel: (agentId: string, model: string | null) => void;
}) {
  return (
    <div
      className="flex items-center gap-2.5 px-4 py-2.5 bg-bg-secondary"
      style={{ background: "hsla(43, 96%, 56%, 0.08)" }}
      title="项目总管 · 系统内置 · 不可邀请、不可请出、不可修改模型"
    >
      {/* Leader avatar — 👑 glyph over amber tinted bg, no gradient. */}
      <div
        className="shrink-0 rounded-full flex items-center justify-center font-semibold"
        style={{
          width: 28,
          height: 28,
          background: "hsla(43, 96%, 56%, 0.18)",
          color: "hsl(33, 90%, 38%)",
          fontSize: 16,
          lineHeight: 1,
          boxShadow: "inset 0 0 0 1px hsla(43, 96%, 50%, 0.55)",
        }}
      >
        <Crown size={14} strokeWidth={2.4} />
      </div>
      <div className="flex-1 min-w-0">
        <p className="text-[14px] font-medium text-text-primary truncate flex items-center gap-1.5">
          <span>{leader.display_name}</span>
          <span
            className="text-[12px] font-bold uppercase tracking-wider px-1.5 py-px rounded-md"
            style={{
              background: "hsla(43, 96%, 56%, 0.18)",
              color: "hsl(33, 90%, 38%)",
            }}
          >
            系统内置
          </span>
        </p>
        <p className="text-[12px] text-text-faint truncate">
          负责决策、任务分解与结果验收 · 每群聊实例独立
        </p>
      </div>
      {/* No action button — model selector for the leader instead */}
      <select
        value={override ?? ""}
        onChange={(e) => {
          const val = e.target.value;
          onSetModel(leader.agent_id, val || null);
        }}
        className="text-[12px] px-1.5 py-1 rounded-md bg-bg-tertiary text-text-secondary border border-border-subtle outline-none max-w-[140px]"
      >
        <option value="">默认模型</option>
        {enabledModels.map((m) => (
          <option key={m.fullId} value={m.fullId}>
            {m.modelId}
          </option>
        ))}
      </select>
    </div>
  );
}

interface MemberListProps {
  items: AgentPersona[];
  agents: AgentPersona[];
  emptyText: string;
  actionLabel: string;
  actionIcon: React.ReactNode;
  actionStyle: "primary" | "danger";
  busyAgentId: string | null;
  onAction: (agentId: string) => void;
  /** Enabled model options from the user's SettingsView. */
  enabledModels: ModelOption[];
  /** Per-agent model override (or `undefined` for "agent default"). */
  overrides: Record<string, string | undefined>;
  /** Fire-and-forget model update — optimistic in the parent. */
  onSetModel: (agentId: string, model: string | null) => void;
  /** Per-agent display name (别名) override (or `undefined` for
   *  "agent's global name"). */
  displayNames: Record<string, string | undefined>;
  /** Fire-and-forget display-name update — optimistic in the parent.
   *  Empty string clears the alias (revert to agent's global name). */
  onSetDisplayName: (agentId: string, displayName: string | null) => void;
  /** P3 — per-agent rank snapshot for badge rendering. Members without
   *  a row in this map render without a badge (e.g. freshly-migrated
   *  rooms before the next rank fetch). */
  ranks: Record<string, api.GroupChatRank>;
  /** P1.4 — vote on a member (+20/-20). Fires optimistically and
   *  updates the rank snapshot on success. */
  onVote: (agentId: string, direction: "up" | "down") => Promise<void>;
}

function MemberList({
  items, agents: _agents, emptyText, actionLabel, actionIcon, actionStyle,
  busyAgentId, onAction, enabledModels, overrides, onSetModel,
  displayNames, onSetDisplayName, ranks, onVote,
}: MemberListProps) {
  // P1.4 — loading indicator for vote action
  const [votingId, setVotingId] = useState<string | null>(null);
  const handleVote = async (agentId: string, direction: "up" | "down") => {
    setVotingId(agentId);
    try {
      await onVote(agentId, direction);
    } finally {
      setVotingId(null);
    }
  };

  if (items.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center py-10 text-text-faint text-[13px] gap-2">
        <Users size={20} className="opacity-40" />
        <p>{emptyText}</p>
      </div>
    );
  }
  return (
    <ul className="bg-bg-secondary">
      {items.map((agent) => {
        const isBusy = busyAgentId === agent.id;
        const override = overrides[agent.id];
        const agentDefault = agent.model ? `${agent.model}` : undefined;
        const alias = displayNames[agent.id];
        const rank = ranks[agent.id];
        return (
          <li
            key={agent.id}
            className="flex items-center gap-2.5 px-4 py-2.5 hover:bg-bg-hover/40 transition-colors"
          >
            <AgentAvatar agent={agent} size={28} />
            <div className="flex-1 min-w-0">
              <p className="text-[14px] font-medium text-text-primary truncate flex items-center gap-1.5">
                {alias ?? agent.name}
                {alias && (
                  <span
                    className="inline-flex items-center gap-0.5 text-[12px] font-bold uppercase tracking-wider px-1 py-px rounded shrink-0"
                    style={{
                      background: "hsla(220, 0%, 55%, 0.12)",
                      color: "hsl(220, 0%, 40%)",
                    }}
                    title={`本房间别名 · 原始名：${agent.name}`}
                  >
                    <Tag size={9} strokeWidth={2.4} />
                    别名
                  </span>
                )}
                {rank && (
                  <span
                    className="inline-flex items-center gap-0.5 text-[12px] font-semibold px-1.5 py-px rounded shrink-0"
                    style={{
                      background: "hsla(43, 96%, 56%, 0.18)",
                      color: "hsl(33, 90%, 38%)",
                      boxShadow: "inset 0 0 0 1px hsla(43, 96%, 50%, 0.35)",
                    }}
                    title={`${rank.tier_cn} · 积分 ${rank.score} · 完成 ${rank.total_tasks} 次任务`}
                  >
                    <span aria-hidden>{rank.badge_emoji}</span>
                    <span>{rank.tier_cn}</span>
                  </span>
                )}
              </p>
              <p className="text-[12px] text-text-faint truncate">
                {alias ? `${agent.name} · ` : ""}{agent.role || agent.identity || "专家"}
              </p>
            </div>
            {/* P1.4 — vote buttons (thumbs up +20 / down -20). */}
            <div className="flex items-center gap-0.5 shrink-0">
              <button
                type="button"
                disabled={isBusy || votingId === agent.id}
                onClick={() => handleVote(agent.id, "up")}
                className="p-1 rounded-md text-text-faint hover:text-emerald-500 hover:bg-emerald-50 transition-colors disabled:opacity-40"
                title={`给 ${alias ?? agent.name} 点赞 +20`}
                aria-label="点赞"
              >
                <ThumbsUp size={12} />
              </button>
              <button
                type="button"
                disabled={isBusy || votingId === agent.id}
                onClick={() => handleVote(agent.id, "down")}
                className="p-1 rounded-md text-text-faint hover:text-red-500 hover:bg-red-50 transition-colors disabled:opacity-40"
                title={`给 ${alias ?? agent.name} 差评 -20`}
                aria-label="差评"
              >
                <ThumbsDown size={12} />
              </button>
            </div>
            {/* Inline edit for display name (别名). Click the pencil to
                open an input; Enter saves, Esc cancels, blank input
                clears the alias (reverts to agent's global name). */}
            <button
              type="button"
              disabled={isBusy}
              onClick={() => {
                const next = window.prompt(
                  `设置「${alias ?? agent.name}」的本房间别名（清空恢复默认）`,
                  alias ?? "",
                );
                if (next === null) return;
                onSetDisplayName(agent.id, next);
              }}
              className="shrink-0 p-1 rounded-md text-text-faint hover:text-text-primary hover:bg-bg-hover transition-colors disabled:opacity-50"
              title="设置本房间别名"
              aria-label="设置别名"
            >
              <Pencil size={12} />
            </button>
            {/* Per-room model select. First option = "agent default",
                remaining = enabled models from SettingsView. If no
                enabled models are configured yet, show a disabled
                placeholder so the user knows where to set them. */}
            <div className="flex items-center gap-1" title="房间级模型覆盖（不影响 agent 全局默认）">
              <Cpu size={10} className="text-text-faint shrink-0" />
              <select
                value={override ?? ""}
                disabled={isBusy}
                onChange={(e) => onSetModel(agent.id, e.target.value || null)}
                className="text-[12px] px-1.5 py-1 rounded-md bg-bg-tertiary text-text-secondary border border-border-subtle outline-none focus:ring-1 focus:ring-accent/40 max-w-[160px]"
              >
                <option value="">
                  {agentDefault ? `默认 · ${truncateModelId(agentDefault)}` : "默认（agent 未设）"}
                </option>
                {enabledModels.map((m) => (
                  <option key={m.fullId} value={m.fullId}>
                    {m.providerId}:{truncateModelId(m.modelId)}
                  </option>
                ))}
                {enabledModels.length === 0 && (
                  <option value="" disabled>
                    （请先在设置中启用模型）
                  </option>
                )}
              </select>
            </div>
            <button
              type="button"
              disabled={isBusy}
              onClick={() => onAction(agent.id)}
              className={clsx(
                "shrink-0 inline-flex items-center gap-1 px-2.5 py-1 rounded-lg text-[13px] font-medium transition-all",
                isBusy && "opacity-60 cursor-wait",
                "hover:opacity-90"
              )}
              style={
                actionStyle === "primary"
                  ? { background: "var(--color-btn-primary-bg)", color: "var(--color-btn-primary-text)" }
                  : {
                      background: "var(--color-error-bg, rgba(239,68,68,0.10))",
                      color: "var(--color-error-text, #b91c1c)",
                      boxShadow: "inset 0 0 0 1px currentColor",
                    }
              }
            >
              {actionIcon}
              <span>{isBusy ? "处理中..." : actionLabel}</span>
            </button>
          </li>
        );
      })}
    </ul>
  );
}

/** Trim long OpenRouter-style model ids to a more readable form. */
function truncateModelId(s: string): string {
  if (s.length <= 22) return s;
  return s.slice(0, 19) + "...";
}

function AgentAvatar({ agent, size }: { agent: AgentPersona; size: number }) {
  const letter = agent.avatar || agent.name[0] || "?";
  // Stable hash → HSL hue (single solid color, no gradient per project rule).
  // Iron law: NO blue/indigo/violet/purple. `h % 360` could yield 220
  // (blue) / 268 (violet), so map to a fixed safe palette (orange / amber /
  // green / teal / cyan / rose / magenta / red) — same set as TaskPanel's
  // agentHsl and the groupchat agentHue.
  let h = 0;
  for (let i = 0; i < agent.id.length; i++) h = (h * 31 + agent.id.charCodeAt(i)) >>> 0;
  const SAFE_HUES = [28, 43, 150, 175, 195, 340, 320, 0];
  const hue = SAFE_HUES[Math.abs(h) % SAFE_HUES.length];
  const bg = `hsl(${hue}, 55%, 55%, 0.35)`;
  const text = `hsl(${hue}, 60%, 35%)`;
  const ring = `hsl(${hue}, 60%, 55%)`;
  return (
    <div
      className="shrink-0 rounded-full flex items-center justify-center font-semibold"
      style={{
        width: size,
        height: size,
        background: bg,
        color: text,
        fontSize: size * 0.42,
        boxShadow: `inset 0 0 0 1px ${withAlpha(ring, 0.33)}`,
      }}
    >
      {letter}
    </div>
  );
}
