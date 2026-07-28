﻿import {
  MessageSquarePlus, Search, Sparkles, Clock, Settings,
  X, User, ChevronDown, ChevronRight,
  MessageSquare, Bot, Users, Plus, FolderOpen,
  BarChart3, Boxes, ListFilter, List,
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
  /** Create a blank chat with this workspace already selected. */
  onNewChatInWorkspace?: (workspacePath: string) => void;
}

const USER = { email: "Messao@hermix" };


/* ── Group Chat Room types (API-backed) ── */

interface ChatRoom {
  id: string;
  name: string;
  agentIds: string[];
  createdAt: number;
}

// 用户 v3 反馈：列表项只保留标题，去掉「多少条」「几小时前」「几天前」
// 这类冗余 metadata。视觉噪声越多，扫视列表越慢。判定一个对话是否在
// 跑模型用右侧的小脉冲点就够了。
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


/* ── Nav item (5 主功能) ── */

const NAV_ITEMS: { icon: React.ElementType; label: string; page: NavPage }[] = [
  { icon: MessageSquare, label: "对话",   page: "chat" },
  { icon: Search,   label: "搜索",    page: "search" },
  { icon: Sparkles, label: "扩展",    page: "plugins" },
  { icon: Clock,    label: "自动化",  page: "automation" },
  { icon: Bot,      label: "专家",    page: "agents" },
  { icon: Users,    label: "群聊",    page: "groupchat" },
  { icon: BarChart3, label: "统计",   page: "analytics" },
{ icon: Boxes,   label: "应用",    page: "customapps" },
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
  isCreatingSession = false,
  onRoomSelect,
  onNewChatInWorkspace,
}: SidebarProps) {
  const { workspaces, add: addWorkspace, remove: removeWorkspace } = useWorkspaces();

  // Group sessions by their bound workspace.
  // Source of truth: session.workspace (backend DB), not localStorage.
  // localStorage can be cleared or out of sync after app restart.
  const { sessionsByWs, orphanPaths } = useMemo(() => {
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
  const [spacesCollapsed, setSpacesCollapsed] = useState(false);
  const toggleWsCollapsed = (id: string) => {
    setCollapsedWs((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id); else next.add(id);
      return next;
    });
  };

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
    }).catch((e) => { console.warn("[Sidebar] fetch rooms failed:", e); });
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

      {/* Top action list — 菜单式, 首项 = 新建对话 */}
      <nav className="nav sidebar-topnav" style={{ flexShrink: 0 }}>
        <button
          type="button"
          onClick={onNewChat}
          disabled={isCreatingSession}
          className={clsx("nav-item nav-item-primary", isCreatingSession && "nav-item-loading")}
          title="新建对话"
          aria-label="新建对话"
        >
          <MessageSquarePlus size={16} />
          <span>{isCreatingSession ? "创建中..." : "新建对话"}</span>
        </button>
      </nav>

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
                <section className="sidebar-projects">
          <div className="section-header sidebar-projects-header">
            <button
              onClick={() => setSpacesCollapsed((v) => !v)}
              className="section-header-toggle"
              aria-expanded={!spacesCollapsed}
            >
              {spacesCollapsed
                ? <ChevronRight size={12} className="section-header-chevron" />
                : <ChevronDown  size={12} className="section-header-chevron" />}
              <span>项目</span>
              <span className="section-header-count">{workspaces.length}</span>
            </button>
            <div className="sidebar-projects-actions no-drag">
              <button onClick={onNewChat} disabled={isCreatingSession} title="新建对话" aria-label="新建对话" className="sidebar-projects-action"><MessageSquarePlus size={12} /></button>
              <button onClick={() => {}} title="筛选" aria-label="筛选" className="sidebar-projects-action"><ListFilter size={12} /></button>
              <button onClick={() => {}} title="排序" aria-label="排序" className="sidebar-projects-action"><List size={12} /></button>
              <button onClick={handleAddWorkspace} title="添加项目" aria-label="添加项目" className="sidebar-projects-action"><Plus size={12} /></button>
            </div>
          </div>
          {!spacesCollapsed && (
            <div className="sidebar-projects-list">
              {workspaces.length === 0 ? (
                <p className="list-group-empty">
                  还没有项目<br />
                  <span style={{ fontSize: 12 }}>点 + 选择本地目录作为项目</span>
                </p>
              ) : workspaces.map((ws) => {
                const wsSessions = sessionsByWs[ws.path] || [];
                const collapsed = collapsedWs.has(ws.id);
                return (
                  <div key={ws.id} className="sidebar-project">
                    <div className="sidebar-project-row">
                      <button onClick={() => toggleWsCollapsed(ws.id)} className="sidebar-project-toggle">
                        {collapsed
                          ? <ChevronRight size={11} className="list-row-chevron" />
                          : <ChevronDown  size={11} className="list-row-chevron" />}
                        <FolderOpen size={12} className="list-row-icon" />
                        <span className="sidebar-project-name">{ws.name}</span>
                      </button>
                      <div className="sidebar-project-row-actions no-drag">
                        <button
                          onClick={() => onNewChatInWorkspace && onNewChatInWorkspace(ws.path)}
                          title={"在 " + ws.name + " 中新建对话"}
                          aria-label={"在 " + ws.name + " 中新建对话"}
                          className="sidebar-project-action"
                        >
                          <Plus size={11} />
                        </button>
                        <button
                          onClick={() => removeWorkspace(ws.id)}
                          title="删除项目"
                          aria-label="删除项目"
                          className="sidebar-project-action"
                        >
                          <X size={11} />
                        </button>
                      </div>
                    </div>
                    {!collapsed && wsSessions.length > 0 && (
                      <div className="sidebar-project-sessions">
                        {wsSessions.map((s) => (
                          <button
                            key={s.id}
                            onClick={() => onSelectSession(s.id)}
                            className={clsx("sidebar-project-session", s.id === activeSessionId && "is-active", s.id === streamingSessionId && "is-streaming")}
                          >
                            <span className="truncate-1">{s.title || "新建对话"}</span>
                            <span className="sidebar-project-session-time">{formatRelative(s.updated_at)}</span>
                          </button>
                        ))}
                      </div>
                    )}
                    {!collapsed && wsSessions.length === 0 && (
                      <p className="sidebar-project-empty">该项目尚无会话</p>
                    )}
                  </div>
                );
              })}
            </div>
          )}
        </section>
{/* Chat rooms */}

        {/* Chat rooms */}
        <section className="sidebar-tree-section">
        <div className="section-header">
          <button
            onClick={() => setGroupChatCollapsed((v) => !v)}
            className="section-header-toggle"
          >
            {groupChatCollapsed
              ? <ChevronRight size={12} className="section-header-chevron" />
              : <ChevronDown  size={12} className="section-header-chevron" />}
            <Users size={12} className="section-header-icon" />
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
                      fontSize: 12,
                      padding: "4px 8px",
                      borderRadius: 6,
                      background: "var(--color-error-soft)",
                      color: "var(--color-error)",
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
                    style={{ flex: 1, padding: "5px 10px", fontSize: 13 }}
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
        </section>
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

