import {
  MessageSquarePlus, Search, Sparkles, Clock, Settings,
  Trash2, X, User, ChevronDown, ChevronRight,
  MessageSquare, Bot, Users, Plus, FolderOpen, Briefcase,
  BarChart3,
} from "lucide-react";
import { clsx } from "clsx";
import { useState, useMemo, useEffect, useCallback, useRef } from "react";
import { BackendStatus } from "./BackendStatus";
import type { Session, NavPage } from "../types";
import { useWorkspaces } from "../hooks/useWorkspaces";
import { pickDirectory } from "../desktopBridge";
import { storage, sessionKey } from "../utils/storageCompat";
import * as api from "../api";

interface SidebarProps {
  page: NavPage;
  onPageChange: (page: NavPage) => void;
  sessions: Session[];
  activeSessionId: string | null;
  /** Session ID that currently has an active WS stream (model working). */
  streamingSessionId?: string | null;
  onNewChat: () => void;
  onSelectSession: (id: string) => void;
  onDeleteSession: (id: string) => void;
  isCreatingSession?: boolean;
  /** Called when a group chat room is selected or created in the sidebar. */
  onRoomSelect?: (roomId: string) => void;
}

const USER = { email: "Messao@hermix" };

/* ── Group Chat Room types (API-backed) ── */

interface ChatRoom {
  id: string;
  name: string;
  agentIds: string[];
  createdAt: number;
}

/* ── Session list item (chat / workspace child) ── */
//
// 主人 v3 反馈：列表项只保留标题，去掉「多少条」「几小时前」「几天前」
// 这类冗余 metadata。视觉噪声越多，扫视列表越慢。判定一个对话是否在
// 跑模型用右侧的小脉冲点就够了。
//
// PLAN_UI_ENHANCEMENT §3.3 (2026-06-22): 激活项用小圆点 + 不显示时间；
// 非激活项显示相对时间（"2 天前" / "15 分钟前"）。这是激活态的最高
// 视觉信号，避免和"正在生成"脉冲点混淆。

function formatRelative(iso: string | undefined): string {
  if (!iso) return "";
  const t = new Date(iso).getTime();
  if (!Number.isFinite(t)) return "";
  const diff = Date.now() - t;
  if (diff < 0) return "";
  const m = Math.floor(diff / 60_000);
  if (m < 1) return "刚刚";
  if (m < 60) return `${m} 分钟前`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h} 小时前`;
  const d = Math.floor(h / 24);
  if (d < 30) return `${d} 天前`;
  const mo = Math.floor(d / 30);
  if (mo < 12) return `${mo} 个月前`;
  const y = Math.floor(d / 365);
  return `${y} 年前`;
}

function SessionItem({
  s, active, isStreaming, onSelect, onDelete,
}: {
  s: Session;
  active: boolean;
  /** Whether this session currently has a model producing output. */
  isStreaming: boolean;
  onSelect: (id: string) => void;
  onDelete: (id: string) => void;
}) {
  return (
    <div
      onClick={() => onSelect(s.id)}
      className={clsx("list-item", active && "list-item-active")}
    >
      {/* Title */}
      <div className="list-item-main">
        <p className="list-item-title">{s.title || "新建对话"}</p>
        {isStreaming && (
          <span className="list-item-pulse" title="模型正在输出">
            <span className="list-item-pulse-dot" />
          </span>
        )}
      </div>
      {/* Right meta: 激活项用静态圆点；非激活项显示相对时间 */}
      {active ? (
        <span className="list-item-active-dot" title="正在对话" />
      ) : (
        <span className="list-item-time" title={s.updated_at}>{formatRelative(s.updated_at)}</span>
      )}
      <button
        onClick={(e) => {
          e.stopPropagation();
          // 2026-06-26: the trash button used to fire `onDelete` with
          // zero confirmation. One stray click on the wrong row and
          // the conversation was gone — and the user had no idea
          // because the backend's 500 was swallowed by
          // `handleDeleteSession`'s missing `try/catch`. Now we ask
          // before we destroy.
          if (window.confirm(`确认删除会话 “${s.title || "新建对话"}”？此操作不可撤销。`)) {
            onDelete(s.id);
          }
        }}
        className="list-item-action"
        title="删除"
      >
        <Trash2 size={12} />
      </button>
    </div>
  );
}

/* ── Nav item (5 主功能) ── */

const NAV_ITEMS: { icon: React.ElementType; label: string; page: NavPage }[] = [
  { icon: MessageSquare, label: "对话",   page: "chat" },
  { icon: Search,   label: "搜索",    page: "search" },
  { icon: Sparkles, label: "扩展",    page: "plugins" },
  { icon: Clock,    label: "自动化",  page: "automation" },
  { icon: Bot,      label: "专家",    page: "agents" },
  { icon: Users,    label: "群聊",    page: "groupchat" },
  { icon: BarChart3, label: "统计",   page: "analytics" },
];

/* ── Main Sidebar ── */

export function Sidebar({
  page,
  onPageChange,
  sessions,
  activeSessionId,
  streamingSessionId,
  onNewChat,
  onSelectSession,
  onDeleteSession,
  isCreatingSession = false,
  onRoomSelect,
}: SidebarProps) {
  const { workspaces, add: addWorkspace, remove: removeWorkspace } = useWorkspaces();

  // Group sessions by their bound workspace.
  // Source of truth: session.workspace (backend DB), not localStorage.
  // localStorage can be cleared or out of sync after app restart.
  const { sessionsByWs, ungrouped, orphanPaths } = useMemo(() => {
    const byWs: Record<string, Session[]> = {};
    const rest: Session[] = [];
    const orphans = new Set<string>();
    for (const s of sessions) {
      let wsPath: string | null = s.workspace ?? null;
      if (!wsPath) {
        try { wsPath = storage.getItem(sessionKey(s.id, "workspace")); } catch {}
      }
      if (wsPath && workspaces.some((w) => w.path === wsPath)) {
        (byWs[wsPath] ||= []).push(s);
      } else if (wsPath) {
        // Workspace path exists but not in workspaces list — mark as orphan
        // so the effect below can auto-add it.
        orphans.add(wsPath);
        (byWs[wsPath] ||= []).push(s);
      } else {
        rest.push(s);
      }
    }
    return { sessionsByWs: byWs, ungrouped: rest, orphanPaths: orphans };
  }, [sessions, workspaces]);

  // Auto-add orphan workspace paths that sessions reference but aren't in
  // the workspaces list yet (e.g. backend has workspace, localStorage was
  // cleared). Runs once per orphan path thanks to the Set dedup.
  const addedOrphansRef = useRef<Set<string>>(new Set());
  useEffect(() => {
    for (const path of orphanPaths) {
      if (addedOrphansRef.current.has(path)) continue;
      addedOrphansRef.current.add(path);
      addWorkspace("", path);
    }
  }, [orphanPaths, addWorkspace]);

  const [collapsedWs, setCollapsedWs] = useState<Set<string>>(new Set());
  // P0-10: Workspace section collapse state (persisted to localStorage).
  const [wsSectionCollapsed, setWsSectionCollapsed] = useState<boolean>(() => {
    try { return storage.getItem(sessionKey("ui", "ws_section_collapsed")) === "1"; } catch { return false; }
  });
  const toggleWsSection = useCallback(() => {
    setWsSectionCollapsed((prev) => {
      const next = !prev;
      try { storage.setItem(sessionKey("ui", "ws_section_collapsed"), next ? "1" : "0"); } catch {}
      return next;
    });
  }, []);
  const toggleWsCollapsed = (id: string) => {
    setCollapsedWs((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id); else next.add(id);
      return next;
    });
  };

  const [ungroupedCollapsed, setUngroupedCollapsed] = useState(false);
  const [groupChatCollapsed, setGroupChatCollapsed] = useState(false);
  const [chatRooms, setChatRooms] = useState<ChatRoom[]>([]);
  const [newRoomName, setNewRoomName] = useState("");
  const [showNewRoom, setShowNewRoom] = useState(false);
  const [creatingRoom, setCreatingRoom] = useState(false);
  const [createRoomError, setCreateRoomError] = useState<string | null>(null);

  // Load rooms from API on mount
  useEffect(() => {
    api.listGroupChatRooms().then((rooms) => {
      setChatRooms(rooms.map((r) => ({
        id: r.id,
        name: r.name,
        agentIds: r.agent_ids,
        createdAt: new Date(r.created_at).getTime(),
      })));
    }).catch(() => {});
  }, []);

  const handleAddWorkspace = async () => {
    const path = await pickDirectory();
    if (!path) return;
    const name = path.split(/[\\/]/).filter(Boolean).pop() || "工作区";
    addWorkspace(name, path);
  };

  const handleCreateRoom = useCallback(async () => {
    const trimmed = newRoomName.trim();
    if (!trimmed || creatingRoom) return;
    setCreatingRoom(true);
    setCreateRoomError(null);
    try {
      const room = await api.createGroupChatRoom(trimmed);
      setChatRooms((prev) => [...prev, {
        id: room.id,
        name: room.name,
        agentIds: room.agent_ids,
        createdAt: new Date(room.created_at).getTime(),
      }]);
      setNewRoomName("");
      setShowNewRoom(false);
      onRoomSelect?.(room.id);
      onPageChange("groupchat");
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      console.error("Failed to create room:", e);
      setCreateRoomError(`创建聊天室失败:${msg}`);
    } finally {
      setCreatingRoom(false);
    }
  }, [newRoomName, creatingRoom, onPageChange, onRoomSelect]);

  return (
    <aside className="pane pane-left">
      {/* Brand 已移至顶部 TitleBar,这里直接开始 CTA,避免双层标题栏 */}

      {/* New chat — primary CTA */}
      <div className="px-3 pt-3 pb-3" style={{ flexShrink: 0 }}>
        <button
          onClick={onNewChat}
          className="btn btn-primary w-full"
          disabled={isCreatingSession}
          title="新建对话"
        >
          <MessageSquarePlus size={16} />
          <span>{isCreatingSession ? "创建中..." : "新建对话"}</span>
        </button>
      </div>

      {/* Nav */}
      <nav className="nav" style={{ flexShrink: 0 }}>
        {NAV_ITEMS.map(({ icon: Icon, label, page: p }) => (
          <button
            key={p}
            onClick={() => onPageChange(p)}
            className={clsx("nav-item", page === p && "nav-item-active")}
          >
            <Icon size={16} />
            <span>{label}</span>
          </button>
        ))}
      </nav>

      {/* Spacer + scrollable workspaces / chat rooms */}
      <div className="px-3 flex-1 overflow-y-auto min-h-0">
        {/* Workspaces section */}
        <div className="section-header">
          <button
            onClick={toggleWsSection}
            className="section-header-toggle"
          >
            {wsSectionCollapsed
              ? <ChevronRight size={12} className="section-header-chevron" />
              : <ChevronDown  size={12} className="section-header-chevron" />}
            <Briefcase size={12} className="section-header-icon" />
            <span>工作区</span>
            {workspaces.length > 0 && (
              <span className="section-header-count">{workspaces.length}</span>
            )}
          </button>
          <button onClick={handleAddWorkspace} className="section-header-add" title="添加工作区" aria-label="添加工作区">
            <Plus size={13} />
          </button>
        </div>

        {!wsSectionCollapsed && (
          <>
          {workspaces.length === 0 ? (
            <p className="list-group-empty">
              还没有工作区<br />
              <span style={{ fontSize: "10.5px" }}>点 + 选择本地目录后开始受限对话</span>
            </p>
          ) : (
          <div className="stack-v" style={{ gap: 2 }}>
            {workspaces.filter((ws) => (sessionsByWs[ws.path] ?? []).length > 0).map((ws) => {
              const wsSessions = sessionsByWs[ws.path] ?? [];
              const collapsed = collapsedWs.has(ws.id);
              return (
                <div key={ws.id}>
                  <div className="list-row">
                    <button
                      onClick={() => toggleWsCollapsed(ws.id)}
                      className="list-row-toggle"
                    >
                      {collapsed
                        ? <ChevronRight size={11} className="list-row-chevron" />
                        : <ChevronDown  size={11} className="list-row-chevron" />}
                      <FolderOpen size={12} className="list-row-icon" />
                      <span className="list-row-name">{ws.name}</span>
                      <span className="list-row-count">{wsSessions.length}</span>
                    </button>
                    <button
                      onClick={() => removeWorkspace(ws.id)}
                      className="list-row-action"
                      title="删除工作区"
                    >
                      <X size={11} />
                    </button>
                  </div>
                  {!collapsed && (
                    <div className="list-group">
                      {wsSessions.length === 0 ? (
                        <p className="list-group-empty">该工作区还没有对话</p>
                      ) : (
                        wsSessions.map((s) => (
                          <SessionItem
                            key={s.id}
                            s={s}
                            active={s.id === activeSessionId}
                            isStreaming={s.id === streamingSessionId}
                            onSelect={onSelectSession}
                            onDelete={onDeleteSession}
                          />
                        ))
                      )}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
          )}
          </>
        )}

        {/* Ungrouped sessions */}
        {ungrouped.length > 0 && (
          <>
            <div className="section-header">
              <button
                onClick={() => setUngroupedCollapsed((v) => !v)}
                className="section-header-toggle"
              >
                {ungroupedCollapsed
                  ? <ChevronRight size={12} className="section-header-chevron" />
                  : <ChevronDown  size={12} className="section-header-chevron" />}
                <span>未分组</span>
                <span className="section-header-count">{ungrouped.length}</span>
              </button>
            </div>
            {!ungroupedCollapsed && (
              <div className="stack-v" style={{ gap: 2 }}>
                {ungrouped.map((s) => (
                  <SessionItem
                    key={s.id}
                    s={s}
                    active={s.id === activeSessionId}
                    isStreaming={s.id === streamingSessionId}
                    onSelect={onSelectSession}
                    onDelete={onDeleteSession}
                  />
                ))}
              </div>
            )}
          </>
        )}

        {/* Chat rooms */}
        <div className="section-header">
          <button
            onClick={() => setGroupChatCollapsed((v) => !v)}
            className="section-header-toggle"
          >
            {groupChatCollapsed
              ? <ChevronRight size={12} className="section-header-chevron" />
              : <ChevronDown  size={12} className="section-header-chevron" />}
            <span>聊天室</span>
            {chatRooms.length > 0 && (
              <span className="section-header-count">{chatRooms.length}</span>
            )}
          </button>
          <button onClick={() => { setGroupChatCollapsed(false); setShowNewRoom(true); }} className="section-header-add" title="新建聊天室">
            <Plus size={13} />
          </button>
        </div>

        {!groupChatCollapsed && (
          <div className="stack-v" style={{ gap: 2 }}>
            {showNewRoom && (
              <div style={{ display: "flex", flexDirection: "column", gap: 4, padding: "4px 8px" }}>
                {createRoomError && (
                  <div
                    role="alert"
                    style={{
                      fontSize: 11,
                      padding: "4px 8px",
                      borderRadius: 6,
                      background: "rgba(239,68,68,0.10)",
                      color: "#b91c1c",
                      display: "flex",
                      alignItems: "center",
                      gap: 4,
                    }}
                  >
                    <span style={{ flex: 1 }}>{createRoomError}</span>
                    <button
                      onClick={() => setCreateRoomError(null)}
                      style={{ opacity: 0.6 }}
                      title="关闭"
                      type="button"
                    >
                      <X size={10} />
                    </button>
                  </div>
                )}
                <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
                  <input
                    autoFocus
                    value={newRoomName}
                    onChange={(e) => {
                      setNewRoomName(e.target.value);
                      if (createRoomError) setCreateRoomError(null);
                    }}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" && !creatingRoom) handleCreateRoom();
                      if (e.key === "Escape") { setShowNewRoom(false); setNewRoomName(""); setCreateRoomError(null); }
                    }}
                    placeholder="聊天室名称"
                    disabled={creatingRoom}
                    className="input"
                    style={{ flex: 1, padding: "5px 10px", fontSize: 12.5 }}
                  />
                  <button
                    onClick={handleCreateRoom}
                    disabled={!newRoomName.trim() || creatingRoom}
                    className="icon-btn"
                    title="创建"
                    style={{ opacity: !newRoomName.trim() || creatingRoom ? 0.4 : 1 }}
                   aria-label="创建">
                    <Plus size={14} />
                  </button>
                  <button
                    onClick={() => { setShowNewRoom(false); setNewRoomName(""); setCreateRoomError(null); }}
                    disabled={creatingRoom}
                    className="icon-btn"
                    title="取消"
                  >
                    <X size={14} />
                  </button>
                </div>
              </div>
            )}
            {chatRooms.length === 0 && !showNewRoom && (
              <p className="list-group-empty">暂无聊天室</p>
            )}
            {chatRooms.map((room) => (
              <div
                key={room.id}
                onClick={() => { onRoomSelect?.(room.id); onPageChange("groupchat"); }}
                className="list-item"
              >
                <Users size={13} className="list-item-icon" />
                <div className="list-item-body">
                  <p className="list-item-title">{room.name}</p>
                </div>
                <span className="list-item-meta" style={{ marginTop: 2 }}>
                  {room.agentIds.length} 个成员
                </span>
              </div>
            ))}
          </div>
        )}
      </div>

      {/* Bottom: status + user + settings */}
      <div className="px-3 pb-3 pt-2 stack-v" style={{ gap: 8, flexShrink: 0 }}>
        <BackendStatus />
        <div className="user-card">
          <div className="user-card-avatar">
            <User size={13} />
          </div>
          <span className="user-card-name" title={USER.email}>
            {USER.email}
          </span>
          <button
            onClick={() => onPageChange("settings")}
            className="user-card-action"
            title="设置"
          >
            <Settings size={13} />
          </button>
        </div>
      </div>
    </aside>
  );
}
