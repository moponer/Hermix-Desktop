import {
  MessageSquarePlus, Search, Sparkles, Clock, Settings,
  User, ChevronDown, ChevronRight,
  MessageSquare, Bot, Users, Plus, FolderOpen,
  BarChart3, Boxes, Trash2, MoreHorizontal, Loader2,
} from "lucide-react";
import { clsx } from "clsx";
import { useState, useMemo, useEffect, useCallback, useRef } from "react";
import { BackendStatus } from "./BackendStatus";
import type { Session, NavPage } from "../types";
import { useWorkspaces } from "../hooks/useWorkspaces";
import { pickDirectory } from "../desktopBridge";
import { storage, sessionKey } from "../utils/storageCompat";
import * as api from "../api";
import { toastSuccess, toastError } from "../hooks/useToast";

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
  onDeleteSession,
  onRoomSelect,
  onNewChatInWorkspace,
}: SidebarProps) {
  const { workspaces, add: addWorkspace, remove: removeWorkspace } = useWorkspaces();

  // Group sessions by their bound workspace.
  // Source of truth: session.workspace (backend DB), not localStorage.
  // localStorage can be cleared or out of sync after app restart.
  const { sessionsByWs, ungrouped: ungroupedSessions, orphanPaths } = useMemo(() => {
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
  const [chatsCollapsed, setChatsCollapsed] = useState(false);
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

  // Delete flow: in-app confirm dialog (replaces native window.confirm) +
  // collapse animation + success/error toast.
  // confirmState  = pending in-app confirm dialog (null = hidden).
  // removingIds   = user confirmed, playing collapse animation (still in list).
  // deletingIds   = animation done, real delete dispatched (parent will unmount).
  const [removingIds, setRemovingIds] = useState<Set<string>>(new Set());
  const [deletingIds, setDeletingIds] = useState<Set<string>>(new Set());
  const [confirmState, setConfirmState] = useState<{
    title: string;
    body: string;
    onConfirm: () => void;
  } | null>(null);

  const runDelete = useCallback((id: string, label: string, onConfirm: () => void) => {
    if (removingIds.has(id) || deletingIds.has(id)) return;
    setRemovingIds((prev) => new Set(prev).add(id));
    window.setTimeout(() => {
      setDeletingIds((prev) => new Set(prev).add(id));
      try {
        onConfirm();
        toastSuccess(`已删除「${label}」`);
      } catch (e) {
        console.error("[Sidebar] delete failed", e);
        toastError(`删除「${label}」失败`);
      }
      // Give parent state a tick to filter the item out, then clean our sets.
      window.setTimeout(() => {
        setRemovingIds((prev) => { const n = new Set(prev); n.delete(id); return n; });
        setDeletingIds((prev) => { const n = new Set(prev); n.delete(id); return n; });
      }, 50);
    }, 320);
  }, [removingIds, deletingIds]);

  const requestDelete = useCallback((id: string, label: string, impact: string, onConfirm: () => void) => {
    if (removingIds.has(id) || deletingIds.has(id)) return;
    setConfirmState({
      title: `删除「${label}」`,
      body: `${impact}\n此操作不可恢复。`,
      onConfirm: () => runDelete(id, label, onConfirm),
    });
  }, [removingIds, deletingIds, runDelete]);

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

  const handleDeleteAllSessions = useCallback(() => {
    setConfirmState({
      title: "删除所有对话",
      body: "将清理全部未归类对话及各项目下的对话记录。\n此操作不可恢复。",
      onConfirm: () => {
        for (const s of ungroupedSessions) onDeleteSession(s.id);
        for (const ws of workspaces) {
          for (const s of sessionsByWs[ws.path] || []) onDeleteSession(s.id);
        }
        toastSuccess("已删除全部对话");
      },
    });
  }, [ungroupedSessions, workspaces, sessionsByWs, onDeleteSession]);

  const handleDeleteAllWorkspaces = useCallback(() => {
    setConfirmState({
      title: "删除所有项目",
      body: "将移除全部项目及其在侧边栏的分组。\n此操作不可恢复。",
      onConfirm: () => {
        for (const ws of workspaces) removeWorkspace(ws.id);
        toastSuccess("已删除全部项目");
      },
    });
  }, [workspaces, removeWorkspace]);

  const handleDeleteAllRooms = useCallback(() => {
    setConfirmState({
      title: "删除所有聊天室",
      body: "将解散全部聊天室（当前为本地列表，后端同步待接入）。\n此操作不可恢复。",
      onConfirm: () => {
        // TODO: wire to API delete room when available; currently local-only list.
        setChatRooms([]);
        toastSuccess("已删除全部聊天室");
      },
    });
  }, []);

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

  /* ── Section menus ── */
  const chatsMenu: MenuItemSpec[] = [
    { key: "delete-all", label: "删除全部对话", onClick: handleDeleteAllSessions, danger: true },
  ];
  const workspacesMenu: MenuItemSpec[] = [
    { key: "add", label: "添加项目", onClick: handleAddWorkspace },
    { key: "delete-all", label: "删除全部项目", onClick: handleDeleteAllWorkspaces, danger: true },
  ];
  const roomsMenu: MenuItemSpec[] = [
    { key: "add", label: "新建聊天室", onClick: () => { setGroupChatCollapsed(false); setShowNewRoom(true); } },
    { key: "delete-all", label: "删除全部聊天室", onClick: handleDeleteAllRooms, danger: true },
  ];

  return (
    <aside className="pane pane-left">
      {/* Brand 已移至顶部 TitleBar,这里直接开始 CTA,避免双层标题栏 */}

      {/* Top action list — 菜单式, 首项 = 新建对话 */}
      <nav className="nav sidebar-topnav" style={{ flexShrink: 0 }}>
        <button
          type="button"
          onClick={onNewChat}
          className="nav-item nav-item-primary"
          title="新建对话"
          aria-label="新建对话"
        >
          <MessageSquarePlus size={16} />
          <span>新建对话</span>
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

      {/* Three independent scrollable sections */}
      <div className="sidebar-sections">
        {/* ── 对话 ── */}
        <section className="sidebar-section">
          <div className="sidebar-section-header">
            <button
              onClick={() => setChatsCollapsed((v) => !v)}
              className="section-header-toggle"
              aria-expanded={!chatsCollapsed}
            >
              {chatsCollapsed
                ? <ChevronRight size={12} className="section-header-chevron" />
                : <ChevronDown  size={12} className="section-header-chevron" />}
              <span>对话</span>
              <span className="section-header-count">{ungroupedSessions.length}</span>
            </button>
            <div className="sidebar-section-actions">
              <SidebarMenu label="对话操作" items={chatsMenu} />
              <SidebarDeleteBtn label="删除全部对话" onClick={handleDeleteAllSessions} />
            </div>
          </div>
          {!chatsCollapsed && (
            <div className="sidebar-section-body">
              {ungroupedSessions.length === 0 ? (
                <p className="sidebar-section-empty">暂无对话</p>
              ) : (
                ungroupedSessions.map((s) => {
                  const isRemoving = removingIds.has(s.id);
                  const isDeleting = deletingIds.has(s.id);
                  const disabled = isRemoving || isDeleting;
                  return (
                    <div
                      key={s.id}
                      className={clsx(
                        "sidebar-row",
                        s.id === activeSessionId && "is-active",
                        s.id === streamingSessionId && "is-streaming",
                        isRemoving && "is-removing",
                        isDeleting && "is-deleting"
                      )}
                      aria-busy={disabled}
                    >
                      <button
                        onClick={() => !disabled && onSelectSession(s.id)}
                        className="sidebar-row-main"
                        disabled={disabled}
                      >
                        {isRemoving ? <Loader2 size={13} className="animate-spin" /> : null}
                        <span className={clsx("truncate-1", isRemoving && "opacity-60")}>{s.title || "新建对话"}</span>
                      </button>
                      <div className="sidebar-row-actions">
                        <SidebarMenu
                          label="对话操作"
                          items={[
                            { key: "delete", label: "删除对话", onClick: () => requestDelete(s.id, s.title || "新建对话", "将清理该对话的全部消息记录。", () => onDeleteSession(s.id)), danger: true },
                          ]}
                        />
                        <SidebarDeleteBtn
                          label="删除对话"
                          onClick={() => requestDelete(s.id, s.title || "新建对话", "将清理该对话的全部消息记录。", () => onDeleteSession(s.id))}
                        />
                      </div>
                    </div>
                  );
                })
              )}
            </div>
          )}
        </section>

        {/* ── 项目 ── */}
        <section className="sidebar-section">
          <div className="sidebar-section-header">
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
            <div className="sidebar-section-actions">
              <SidebarMenu label="项目操作" items={workspacesMenu} />
              <SidebarDeleteBtn label="删除全部项目" onClick={handleDeleteAllWorkspaces} />
            </div>
          </div>
          {!spacesCollapsed && (
            <div className="sidebar-section-body">
              {workspaces.length === 0 ? (
                <p className="sidebar-section-empty">还没有项目</p>
              ) : workspaces.map((ws) => {
                const wsSessions = sessionsByWs[ws.path] || [];
                const collapsed = collapsedWs.has(ws.id);
                const wsRemoving = removingIds.has(ws.id);
                const wsDeleting = deletingIds.has(ws.id);
                const wsDisabled = wsRemoving || wsDeleting;
                return (
                  <div key={ws.id} className={clsx("sidebar-project", wsRemoving && "is-removing", wsDeleting && "is-deleting")}>
                    <div className="sidebar-project-row">
                      <button onClick={() => !wsDisabled && toggleWsCollapsed(ws.id)} className="sidebar-project-toggle" disabled={wsDisabled}>
                        {collapsed
                          ? <ChevronRight size={11} className="list-row-chevron" />
                          : <ChevronDown  size={11} className="list-row-chevron" />}
                        <FolderOpen size={12} className="list-row-icon" />
                        {wsRemoving ? <Loader2 size={13} className="animate-spin" /> : null}
                        <span className={clsx("sidebar-project-name", wsRemoving && "opacity-60")}>{ws.name}</span>
                      </button>
                      <div className="sidebar-row-actions">
                        <SidebarMenu
                          label="项目操作"
                          items={[
                            { key: "new-chat", label: "新建对话", onClick: () => onNewChatInWorkspace?.(ws.path) },
                            { key: "delete", label: "删除项目", onClick: () => requestDelete(ws.id, ws.name, "将移除该项目及其在侧边栏的分组。", () => removeWorkspace(ws.id)), danger: true },
                          ]}
                        />
                        <SidebarDeleteBtn
                          label="删除项目"
                          onClick={() => requestDelete(ws.id, ws.name, "将移除该项目及其在侧边栏的分组。", () => removeWorkspace(ws.id))}
                        />
                      </div>
                    </div>
                    {!collapsed && wsSessions.length > 0 && (
                      <div className="sidebar-project-sessions">
                        {wsSessions.map((s) => {
                          const psRemoving = removingIds.has(s.id);
                          const psDeleting = deletingIds.has(s.id);
                          const psDisabled = psRemoving || psDeleting;
                          return (
                            <div
                              key={s.id}
                              className={clsx(
                                "sidebar-row sidebar-project-session",
                                s.id === activeSessionId && "is-active",
                                s.id === streamingSessionId && "is-streaming",
                                psRemoving && "is-removing",
                                psDeleting && "is-deleting"
                              )}
                              aria-busy={psDisabled}
                            >
                              <button
                                onClick={() => !psDisabled && onSelectSession(s.id)}
                                className="sidebar-row-main"
                                disabled={psDisabled}
                              >
                                {psRemoving ? <Loader2 size={13} className="animate-spin" /> : null}
                                <span className={clsx("truncate-1", psRemoving && "opacity-60")}>{s.title || "新建对话"}</span>
                              </button>
                              <div className="sidebar-row-actions">
                                <SidebarMenu
                                  label="对话操作"
                                  items={[
                                    { key: "delete", label: "删除对话", onClick: () => requestDelete(s.id, s.title || "新建对话", "将清理该对话的全部消息记录。", () => onDeleteSession(s.id)), danger: true },
                                  ]}
                                />
                                <SidebarDeleteBtn
                                  label="删除对话"
                                  onClick={() => requestDelete(s.id, s.title || "新建对话", "将清理该对话的全部消息记录。", () => onDeleteSession(s.id))}
                                />
                              </div>
                            </div>
                          );
                        })}
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

        {/* ── 聊天室 ── */}
        <section className="sidebar-section">
          <div className="sidebar-section-header">
            <button
              onClick={() => setGroupChatCollapsed((v) => !v)}
              className="section-header-toggle"
              aria-expanded={!groupChatCollapsed}
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
            <div className="sidebar-section-actions">
              <SidebarMenu label="聊天室操作" items={roomsMenu} />
            </div>
          </div>
          {!groupChatCollapsed && (
            <div className="sidebar-section-body">
              {showNewRoom && (
                <div className="sidebar-new-room">
                  {createRoomError && (
                    <div className="sidebar-new-room-error" role="alert">
                      <span>{createRoomError}</span>
                      <button
                        onClick={() => setCreateRoomError(null)}
                        title="关闭"
                        type="button"
                      >
                        <span aria-hidden>×</span>
                      </button>
                    </div>
                  )}
                  <div className="sidebar-new-room-row">
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
                    />
                    <button
                      onClick={handleCreateRoom}
                      disabled={!newRoomName.trim() || creatingRoom}
                      className="icon-btn"
                      title="创建"
                      aria-label="创建"
                    >
                      <Plus size={14} />
                    </button>
                    <button
                      onClick={() => { setShowNewRoom(false); setNewRoomName(""); setCreateRoomError(null); }}
                      disabled={creatingRoom}
                      className="icon-btn"
                      title="取消"
                      aria-label="取消"
                    >
                      <span aria-hidden>×</span>
                    </button>
                  </div>
                </div>
              )}
              {chatRooms.length === 0 && !showNewRoom && (
                <p className="sidebar-section-empty">暂无聊天室</p>
              )}
              {chatRooms.map((room) => {
                const roomRemoving = removingIds.has(room.id);
                const roomDeleting = deletingIds.has(room.id);
                const roomDisabled = roomRemoving || roomDeleting;
                const doDeleteRoom = () => setChatRooms((prev) => prev.filter((r) => r.id !== room.id));
                return (
                  <div
                    key={room.id}
                    onClick={() => !roomDisabled && (onRoomSelect?.(room.id), onPageChange("groupchat"))}
                    className={clsx("sidebar-row", roomRemoving && "is-removing", roomDeleting && "is-deleting")}
                    aria-busy={roomDisabled}
                  >
                    <div className={clsx("sidebar-row-main", roomRemoving && "opacity-60")}>
                      {roomRemoving ? <Loader2 size={13} className="animate-spin" /> : <Users size={13} />}
                      <span className="truncate-1">{room.name}</span>
                    </div>
                    <span className="sidebar-row-meta">{room.agentIds.length} 个成员</span>
                    <div className="sidebar-row-actions">
                      <SidebarMenu
                        label="聊天室操作"
                        items={[
                          { key: "delete", label: "删除聊天室", onClick: () => requestDelete(room.id, room.name, "将解散该聊天室。", doDeleteRoom), danger: true },
                        ]}
                      />
                      <SidebarDeleteBtn
                        label="删除聊天室"
                        onClick={() => requestDelete(room.id, room.name, "将解散该聊天室。", doDeleteRoom)}
                      />
                    </div>
                  </div>
                );
              })}
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
      <DeleteConfirmDialog
        state={confirmState}
        onConfirm={() => { const s = confirmState; setConfirmState(null); s?.onConfirm(); }}
        onCancel={() => setConfirmState(null)}
      />
    </aside>
  );
}

/* ── Scoped helpers ── */

type MenuItemSpec = { key: string; label: string; onClick: () => void; danger?: boolean };

function useSidebarMenu() {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    const onEsc = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    window.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onEsc);
    return () => {
      window.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onEsc);
    };
  }, [open]);
  return { open, setOpen, ref };
}

function SidebarMenu({ label, items }: { label: string; items: MenuItemSpec[] }) {
  const { open, setOpen, ref } = useSidebarMenu();
  return (
    <div ref={ref} className="relative">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="sidebar-action-btn"
        title={label}
        aria-label={label}
      >
        <MoreHorizontal size={14} />
      </button>
      {open && (
        <div
          className="absolute right-0 top-full mt-1 min-w-[140px] rounded-lg border border-border-primary bg-bg-elevated shadow-pop py-1 z-[1000]"
          role="menu"
        >
          {items.map((item) => (
            <button
              key={item.key}
              type="button"
              onClick={() => { setOpen(false); item.onClick(); }}
              className={`w-full flex items-center gap-2 px-3 py-1.5 text-left text-[12.5px] hover:bg-bg-hover transition-colors ${
                item.danger ? "text-[color:var(--color-error)]" : "text-text-primary"
              }`}
              role="menuitem"
            >
              {item.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

function SidebarDeleteBtn({ label, onClick }: { label: string; onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="sidebar-action-btn"
      title={label}
      aria-label={label}
    >
      <Trash2 size={13} />
    </button>
  );
}

function DeleteConfirmDialog({
  state,
  onConfirm,
  onCancel,
}: {
  state: { title: string; body: string; onConfirm: () => void } | null;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  if (!state) return null;
  return (
    <div className="fixed inset-0 z-[200] flex items-center justify-center p-4">
      <button
        type="button"
        aria-label="取消"
        onClick={onCancel}
        className="absolute inset-0 bg-black/40 backdrop-blur-sm"
      />
      <div
        className={clsx(
          "relative z-10 w-full max-w-[420px] flex flex-col",
          "rounded-2xl border shadow-2xl",
          "bg-bg-secondary border-border-subtle",
        )}
      >
        <div className="flex items-start gap-3 px-5 py-4 border-b border-border-subtle">
          <div
            className="w-8 h-8 rounded-lg flex items-center justify-center shrink-0"
            style={{ background: "hsla(150, 60%, 50%, 0.12)" }}
          >
            <Trash2 size={16} style={{ color: "hsl(150, 60%, 45%)" }} />
          </div>
          <div className="flex-1 min-w-0">
            <h2 className="text-[14px] font-bold text-text-primary">{state.title}</h2>
            <p className="mt-1 text-[12px] text-text-muted whitespace-pre-wrap leading-relaxed">
              {state.body}
            </p>
          </div>
        </div>
        <div className="flex items-center gap-2.5 px-5 py-4">
          <button
            type="button"
            onClick={onConfirm}
            className={clsx(
              "flex items-center gap-1.5 px-4 py-2 rounded-lg text-[12.5px] font-bold",
              "text-white transition-opacity hover:opacity-90",
            )}
            style={{ background: "hsl(0, 70%, 50%)" }}
          >
            删除
          </button>
          <button
            type="button"
            onClick={onCancel}
            className={clsx(
              "flex items-center gap-1.5 px-4 py-2 rounded-lg text-[12.5px] font-semibold",
              "text-text-muted hover:text-text-secondary hover:bg-bg-tertiary transition-colors",
            )}
          >
            取消
          </button>
          <span className="ml-auto text-[10.5px] text-text-faint">此操作不可恢复</span>
        </div>
      </div>
    </div>
  );
}

