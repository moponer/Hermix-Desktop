import {
  MessageSquarePlus, Search, Settings,
  ChevronDown, ChevronRight,
  MessageSquare, Users, Plus, FolderOpen,
  Boxes, MoreHorizontal, Loader2, LayoutDashboard,
  // Popover icons
  Pencil, Pin, Archive, Trash2, FolderOpenDot, Copy, FolderMinus,
  type LucideIcon,
} from "lucide-react";
import { clsx } from "clsx";
import { useState, useMemo, useEffect, useCallback, useRef } from "react";
import { createPortal } from "react-dom";
import { ConnectionIndicator } from "./ConnectionIndicator";
import type { Session, NavPage } from "../types";
import { useWorkspaces } from "../hooks/useWorkspaces";
import { usePinnedArchived } from "../hooks/usePinnedArchived";
import { openInExplorer, copyText } from "../desktopBridge";
import { storage, sessionKey } from "../utils/storageCompat";
import { formatRelativeTime } from "../utils/time";
import * as api from "../api";
import { toastSuccess, toastError } from "../hooks/useToast";
import { useUserProfile } from "../hooks/useUserProfile";
import { UserAvatar } from "./UserAvatar";
import { EditProfileDialog } from "./EditProfileDialog";

interface SidebarProps {
  page: NavPage;
  onPageChange: (page: NavPage) => void;
  onOpenSettings?: () => void;
  sessions: Session[];
  activeSessionId: string | null;
  /** Session ID that currently has an active WS stream (model working). */
  streamingSessionId?: string | null;
  onNewChat: () => void;
  onSelectSession: (id: string) => void;
  onDeleteSession: (id: string) => void;
  /** Called when a group chat room is selected, created, or deleted in the sidebar.
   *  Pass `null` to clear the current selection (e.g. after deletion). */
  onRoomSelect?: (roomId: string | null) => void;
  /** Create a blank chat with this workspace already selected. */
  onNewChatInWorkspace?: (workspacePath: string) => void;
}

/* ── Group Chat Room types (API-backed) ── */

interface ChatRoom {
  id: string;
  name: string;
  agentIds: string[];
  createdAt: number;
}


/* ── Nav item — 4 核心功能 ── */

const NAV_ITEMS: { icon: React.ElementType; label: string; page: NavPage }[] = [
  { icon: MessageSquare, label: "对话",   page: "chat" },
  { icon: Search,   label: "搜索",    page: "search" },
  { icon: Users,    label: "群聊",    page: "groupchat" },
  { icon: Boxes,    label: "应用",    page: "customapps" },
  { icon: LayoutDashboard, label: "工作台", page: "workspaces" },
];

// 已移除：工具箱整合进系统配置面板，保留旧名称便于搜索残留引用。

/* ── Main Sidebar ── */

export function Sidebar({
  page,
  onPageChange,
  onOpenSettings,
  sessions,
  activeSessionId,
  streamingSessionId,
  onNewChat,
  onSelectSession,
  onDeleteSession,
  onRoomSelect,
  onNewChatInWorkspace,
}: SidebarProps) {
  const { workspaces, add: addWorkspace, remove: removeWorkspace, rename: renameWorkspace } = useWorkspaces();
  const pinnedArchived = usePinnedArchived();
  const { isPinned, isArchived, togglePin, toggleArchive } = pinnedArchived;

  const { profile, setProfile } = useUserProfile();
  const [editOpen, setEditOpen] = useState(false);

  // Sort: pinned first, then archived last, otherwise keep original order.
  // React.useMemo on the array is enough; the hook already re-renders us
  // when its localStorage state changes.
  const sortByPinArchive = useCallback(
    <T extends { id: string }>(items: T[], scope: "session" | "workspace" | "room"): T[] => {
      return [...items].sort((a, b) => {
        const ap = isPinned(a.id, scope) ? 0 : 1;
        const bp = isPinned(b.id, scope) ? 0 : 1;
        if (ap !== bp) return ap - bp;
        const aa = isArchived(a.id, scope) ? 1 : 0;
        const ba = isArchived(b.id, scope) ? 1 : 0;
        return aa - ba;
      });
    },
    [isPinned, isArchived],
  );

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

  // Auto-destroy workspaces that have no sessions — an empty project has
  // nothing to show, so it disappears from the list entirely. Sessions are
  // stored independently, so this only drops the path→name mapping; the
  // orphan-reconciliation effect re-creates the workspace if a session in
  // that path shows up again.
  useEffect(() => {
    const emptyIds = workspaces
      .filter((ws) => (sessionsByWs[ws.path] || []).length === 0)
      .map((ws) => ws.id);
    if (emptyIds.length === 0) return;
    emptyIds.forEach((id) => removeWorkspace(id));
  }, [workspaces, sessionsByWs, removeWorkspace]);

  const [chatsCollapsed, setChatsCollapsed] = useState(false);
  const [spacesCollapsed, setSpacesCollapsed] = useState(false);

  // SystemConfig 已由 系统配置 按钮直接跳转，无需 popup 状态。

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

  // Rename dialog state — replaces window.prompt with an in-app modal.
  const [renameState, setRenameState] = useState<{
    title: string;
    current: string;
    onConfirm: (next: string) => void;
  } | null>(null);

  const runDelete = useCallback((id: string, label: string, onConfirm: () => Promise<void> | void) => {
    if (removingIds.has(id) || deletingIds.has(id)) return;
    setRemovingIds((prev) => new Set(prev).add(id));
    window.setTimeout(async () => {
      setDeletingIds((prev) => new Set(prev).add(id));
      try {
        await onConfirm();
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


  /* ── 子项 / 项目组菜单构造器 ── */
  // 单条会话菜单（参考图 5）：重命名 / 置顶 / 归档 / 删除 — 无分割线
  const sessionMenu = (s: Session): MenuItemSpec[] => [
    { key: "rename", label: "重命名", icon: Pencil, onClick: () => promptRenameSession(s.id, s.title || "新建对话") },
    { key: "pin", label: isPinned(s.id, "session") ? "取消置顶" : "置顶", icon: Pin, onClick: () => togglePin(s.id, "session") },
    { key: "archive", label: isArchived(s.id, "session") ? "取消归档" : "归档", icon: Archive, onClick: () => toggleArchive(s.id, "session") },
    { key: "delete", label: "删除", icon: Trash2, onClick: () => requestDelete(s.id, s.title || "新建对话", "将清理该对话的全部消息记录。", () => onDeleteSession(s.id)), danger: true },
  ];

  // 项目组容器行菜单（参考图 4）：新建会话 / 重命名项目 / 置顶项目 / 移除 / 在文件资源管理器中打开 / 复制路径 — 无分割线
  const workspaceMenu = (ws: { id: string; name: string; path: string }): MenuItemSpec[] => [
    { key: "new-chat", label: "新建会话", icon: Plus, onClick: () => onNewChatInWorkspace?.(ws.path) },
    { key: "rename", label: "重命名项目", icon: Pencil, onClick: () => promptRenameWorkspace(ws.id, ws.name) },
    { key: "pin", label: isPinned(ws.id, "workspace") ? "取消置顶" : "置顶项目", icon: Pin, onClick: () => togglePin(ws.id, "workspace") },
    { key: "remove", label: "移除", icon: FolderMinus, onClick: () => requestDelete(ws.id, ws.name, "将移除该项目及其在侧边栏的分组。", () => removeWorkspace(ws.id)), danger: true },
    { key: "reveal", label: "在文件资源管理器中打开", icon: FolderOpenDot, onClick: () => openInExplorer(ws.path) },
    { key: "copy-path", label: "复制路径", icon: Copy, onClick: () => handleCopy(ws.path, "已复制路径") },
  ];

  // 聊天室菜单：与 session 同样 4 项 — 无分割线
  const roomMenu = (room: ChatRoom): MenuItemSpec[] => [
    { key: "rename", label: "重命名", icon: Pencil, onClick: () => promptRenameRoom(room.id, room.name) },
    { key: "pin", label: isPinned(room.id, "room") ? "取消置顶" : "置顶", icon: Pin, onClick: () => togglePin(room.id, "room") },
    { key: "archive", label: isArchived(room.id, "room") ? "取消归档" : "归档", icon: Archive, onClick: () => toggleArchive(room.id, "room") },
    { key: "delete", label: "删除", icon: Trash2, onClick: () => requestDelete(room.id, room.name, "将解散该聊天室。", doDeleteRoom(room.id)), danger: true },
  ];

  // 聊天室删除:必须先 await 后端 API,否则 GroupChatView 的 rooms 状态(挂在另一个 useState)仍是陈旧列表,
  // 删除后右侧仍会渲染已删的房间。等后端确认删除后再通知 App 清除 selectedRoomId,触发 GroupChatView 切回空态。
  const doDeleteRoom = useCallback((id: string) => async () => {
    await api.deleteGroupChatRoom(id);
    setChatRooms((prev) => prev.filter((r) => r.id !== id));
    onRoomSelect?.(null);
  }, [onRoomSelect]);

  const handleCopy = useCallback(async (text: string, successMsg: string) => {
    const ok = await copyText(text);
    if (ok) toastSuccess(successMsg);
    else toastError("复制失败，请检查剪贴板权限");
  }, []);

  const promptRenameSession = useCallback((id: string, current: string) => {
    setRenameState({
      title: "重命名对话",
      current,
      onConfirm: async (next) => {
        try {
          const ok = await api.renameSession(id, next);
          if (ok) toastSuccess("已重命名");
          else toastError("重命名失败");
        } catch (e) {
          console.error("[Sidebar] rename session failed", e);
          toastError("重命名失败");
        }
      },
    });
  }, []);

  const promptRenameWorkspace = useCallback((id: string, current: string) => {
    setRenameState({
      title: "重命名项目",
      current,
      onConfirm: (next) => {
        renameWorkspace(id, next);
        toastSuccess("已重命名");
      },
    });
  }, [renameWorkspace]);

  const promptRenameRoom = useCallback((id: string, current: string) => {
    setRenameState({
      title: "重命名聊天室",
      current,
      onConfirm: (next) => {
        setChatRooms((prev) => prev.map((r) => (r.id === id ? { ...r, name: next } : r)));
        toastSuccess("已重命名");
      },
    });
  }, []);

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
                ? <ChevronRight size={14} className="shrink-0" />
                : <ChevronDown size={14} className="shrink-0" />}
              <span>对话</span>
              <span className="section-header-count">{ungroupedSessions.length}</span>
            </button>
          </div>
          {!chatsCollapsed && (
            <div className="sidebar-section-body">
              {ungroupedSessions.length === 0 ? (
                <p className="sidebar-section-empty">暂无对话</p>
              ) : (
                sortByPinArchive(ungroupedSessions, "session").map((s) => {
                  const isRemoving = removingIds.has(s.id);
                  const isDeleting = deletingIds.has(s.id);
                  const disabled = isRemoving || isDeleting;
                  const archived = isArchived(s.id, "session");
                  return (
                    <div
                      key={s.id}
                      className={clsx(
                        "sidebar-row",
                        s.id === activeSessionId && "is-active",
                        s.id === streamingSessionId && "is-streaming",
                        archived && "is-archived",
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
                        {s.id === streamingSessionId
                          ? <Loader2 size={13} className="animate-spin text-accent shrink-0" />
                          : isRemoving ? <Loader2 size={13} className="animate-spin" /> : null}
                        <span className={clsx("truncate-1", isRemoving && "opacity-60")}>{s.title || "新建对话"}</span>
                        <span className="sidebar-row-meta">{formatRelativeTime(s.updated_at)}</span>
                        {isPinned(s.id, "session") && <Pin size={10} className="sidebar-row-pin" />}
                      </button>
                      <div className="sidebar-row-actions">
                        <SidebarMenu label="对话操作" items={sessionMenu(s)} />
                      </div>
                    </div>
                  );
                })
              )}
              {ungroupedSessions.length >= 5 && (
                <button type="button" className="sidebar-show-more" title="查看全部对话">
                  查看更多({ungroupedSessions.length})
                </button>
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
                ? <ChevronRight size={14} className="shrink-0" />
                : <ChevronDown size={14} className="shrink-0" />}
              <span>项目</span>
              <span className="section-header-count">{workspaces.length}</span>
            </button>
          </div>
          {!spacesCollapsed && (
            <div className="sidebar-section-body">
              {workspaces.length === 0 ? (
                <p className="sidebar-section-empty">还没有项目</p>
              ) : (
                sortByPinArchive(workspaces, "workspace").map((ws) => {
                const wsSessions = sessionsByWs[ws.path] || [];
                const collapsed = collapsedWs.has(ws.id);
                const wsRemoving = removingIds.has(ws.id);
                const wsDeleting = deletingIds.has(ws.id);
                const wsDisabled = wsRemoving || wsDeleting;
                const wsPinned = isPinned(ws.id, "workspace");
                return (
                  <div key={ws.id} className={clsx("sidebar-project", wsRemoving && "is-removing", wsDeleting && "is-deleting")}>
                    <div className="sidebar-project-row">
                      <button onClick={() => !wsDisabled && wsSessions.length > 0 && toggleWsCollapsed(ws.id)} className="sidebar-project-toggle" disabled={wsDisabled || wsSessions.length === 0}>
                        <FolderOpen size={13} className="list-row-icon" />
                        {wsRemoving ? <Loader2 size={13} className="animate-spin" /> : null}
                        <span className={clsx("sidebar-project-name", wsRemoving && "opacity-60")}>{ws.name}</span>
                        {wsPinned && <Pin size={10} className="sidebar-row-pin" />}
                        {collapsed
                          ? <ChevronRight size={12} className="list-row-chevron ml-auto" />
                          : <ChevronDown  size={12} className="list-row-chevron ml-auto" />}
                      </button>
                      <div className="sidebar-row-actions">
                        <SidebarMenu label="项目操作" items={workspaceMenu(ws)} />
                      </div>
                    </div>
                    {!collapsed && wsSessions.length > 0 && (
                      <div className="sidebar-project-sessions">
                        {sortByPinArchive(wsSessions, "session").map((s) => {
                          const psRemoving = removingIds.has(s.id);
                          const psDeleting = deletingIds.has(s.id);
                          const psDisabled = psRemoving || psDeleting;
                          const psArchived = isArchived(s.id, "session");
                          return (
                            <div
                              key={s.id}
                              className={clsx(
                                "sidebar-row sidebar-project-session",
                                s.id === activeSessionId && "is-active",
                                s.id === streamingSessionId && "is-streaming",
                                psArchived && "is-archived",
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
                                {s.id === streamingSessionId
                                  ? <Loader2 size={13} className="animate-spin text-accent shrink-0" />
                                  : psRemoving ? <Loader2 size={13} className="animate-spin" /> : null}
                                <span className={clsx("truncate-1", psRemoving && "opacity-60")}>{s.title || "新建对话"}</span>
                                <span className="sidebar-row-meta">{formatRelativeTime(s.updated_at)}</span>
                                {isPinned(s.id, "session") && <Pin size={10} className="sidebar-row-pin" />}
                              </button>
                              <div className="sidebar-row-actions">
                                <SidebarMenu label="对话操作" items={sessionMenu(s)} />
                              </div>
                            </div>
                          );
                        })}
                        {wsSessions.length >= 5 && (
                          <button type="button" className="sidebar-show-more sidebar-show-more-inset" title="查看全部会话">
                            查看更多({wsSessions.length})
                          </button>
                        )}
                      </div>
                    )}
                  </div>
                );
              }))}
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
                ? <ChevronRight size={14} className="shrink-0" />
                : <ChevronDown size={14} className="shrink-0" />}
              <span>聊天室</span>
              {chatRooms.length > 0 && (
                <span className="section-header-count">{chatRooms.length}</span>
              )}
            </button>
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
              {sortByPinArchive(chatRooms, "room").map((room) => {
                const roomRemoving = removingIds.has(room.id);
                const roomDeleting = deletingIds.has(room.id);
                const roomDisabled = roomRemoving || roomDeleting;
                const roomArchived = isArchived(room.id, "room");
                return (
                  <div
                    key={room.id}
                    onClick={() => !roomDisabled && (onRoomSelect?.(room.id), onPageChange("groupchat"))}
                    className={clsx("sidebar-row", roomArchived && "is-archived", roomRemoving && "is-removing", roomDeleting && "is-deleting")}
                    aria-busy={roomDisabled}
                  >
                    <div className={clsx("sidebar-row-main", roomRemoving && "opacity-60")}>
                      {roomRemoving ? <Loader2 size={13} className="animate-spin" /> : <Users size={13} />}
                      <span className="truncate-1">{room.name}</span>
                      {isPinned(room.id, "room") && <Pin size={10} className="sidebar-row-pin" />}
                    </div>
                    <span className="sidebar-row-meta">{room.agentIds.length} 个成员</span>
                    <div className="sidebar-row-actions">
                      <SidebarMenu label="聊天室操作" items={roomMenu(room)} />
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </section>
      </div>

      {/* Bottom: user + settings */}
      <div className="px-3 pb-3 pt-2 stack-v" style={{ gap: 8, flexShrink: 0 }}>
        <div
          className="user-card"
          role="button"
          tabIndex={0}
          style={{ cursor: "pointer" }}
          onClick={() => setEditOpen(true)}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              setEditOpen(true);
            }
          }}
        >
          <UserAvatar name={profile.displayName} avatarDataUrl={profile.avatarDataUrl} size={28} />
          <span className="user-card-name" title={profile.displayName}>
            {profile.displayName}
          </span>
          <ConnectionIndicator />
          <button
            onClick={(e) => {
              e.stopPropagation();
              onOpenSettings?.();
            }}
            className="user-card-action"
            title="设置"
          >
            <Settings size={13} />
          </button>
        </div>
        {editOpen && (
          <EditProfileDialog
            current={profile}
            onSave={(next) => {
              setProfile(next);
              setEditOpen(false);
            }}
            onClose={() => setEditOpen(false)}
          />
        )}
      </div>
      <DeleteConfirmDialog
        state={confirmState}
        onConfirm={() => { const s = confirmState; setConfirmState(null); s?.onConfirm(); }}
        onCancel={() => setConfirmState(null)}
      />
      <RenameDialog
        state={renameState}
        onConfirm={(next) => { const s = renameState; setRenameState(null); s?.onConfirm(next); }}
        onCancel={() => setRenameState(null)}
      />
    </aside>
  );
}

/* ── Scoped helpers ── */

type MenuItemSpec = {
  key: string;
  label: string;
  onClick: () => void;
  danger?: boolean;
  /** Optional left-side icon — Lucide component (size 14). */
  icon?: LucideIcon;
  /** When true, render a thin divider above this item. */
  dividerBefore?: boolean;
};

function useSidebarMenu() {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      const target = e.target as Node;
      if (ref.current?.contains(target) || popoverRef.current?.contains(target)) return;
      setOpen(false);
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
  return { open, setOpen, ref, popoverRef };
}

/**
 * Popover body — shared by the per-row action menus (rename / pin / archive /
 * delete). Section headers no longer use the Popover: their "+" actions were
 * removed and collapse/expand is now an inline chevron toggle.
 * Renders the items list with optional left icons and per-item dividers.
 */
function SidebarPopoverBody({ items, onClose, popoverRef, position }: {
  items: MenuItemSpec[];
  onClose: () => void;
  popoverRef: React.RefObject<HTMLDivElement | null>;
  position: { right: number; top: number };
}) {
  // Clamp to viewport so the menu never overflows the bottom edge.
  const top = Math.min(position.top, window.innerHeight - 200);
  return (
    <div
      ref={popoverRef}
      className="sidebar-popover"
      role="menu"
      style={{ position: "fixed", right: position.right, top, zIndex: 9999 }}
    >
      {items.map((item) => {
        const Icon = item.icon;
        return (
          <div key={item.key}>
            {item.dividerBefore && <div className="sidebar-popover-divider" />}
            <button
              type="button"
              onClick={() => { onClose(); item.onClick(); }}
              className={clsx("sidebar-popover-item", item.danger && "is-danger")}
              role="menuitem"
            >
              {Icon ? <Icon className="sidebar-popover-icon" aria-hidden /> : null}
              <span>{item.label}</span>
            </button>
          </div>
        );
      })}
    </div>
  );
}

/**
 * Three-dots menu (...) — used on sidebar rows.
 * Popover body is shared (see SidebarPopoverBody).
 */
function SidebarMenu({ label, items }: { label: string; items: MenuItemSpec[] }) {
  const { open, setOpen, ref, popoverRef } = useSidebarMenu();
  const [pos, setPos] = useState<{ right: number; top: number } | null>(null);

  const handleToggle = () => {
    if (!open && ref.current) {
      const rect = ref.current.getBoundingClientRect();
      setPos({ right: window.innerWidth - rect.right, top: rect.bottom + 4 });
    }
    setOpen((v) => !v);
  };

  return (
    <div ref={ref} className="relative">
      <button
        type="button"
        onClick={handleToggle}
        className="sidebar-action-btn"
        title={label}
        aria-label={label}
      >
        <MoreHorizontal size={16} />
      </button>
      {open && pos && createPortal(
        <SidebarPopoverBody
          items={items}
          onClose={() => setOpen(false)}
          popoverRef={popoverRef}
          position={pos}
        />,
        document.body,
      )}
    </div>
  );
}

function RenameDialog({
  state,
  onConfirm,
  onCancel,
}: {
  state: { title: string; current: string; onConfirm: (next: string) => void } | null;
  onConfirm: (next: string) => void;
  onCancel: () => void;
}) {
  const [value, setValue] = useState("");

  // Reset input when dialog opens.
  useEffect(() => {
    if (state) setValue(state.current);
  }, [state]);

  if (!state) return null;

  const trimmed = value.trim();
  const canConfirm = trimmed.length > 0 && trimmed !== state.current;

  return createPortal(
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
            style={{ background: "hsla(210, 70%, 55%, 0.12)" }}
          >
            <Pencil size={16} style={{ color: "hsl(210, 70%, 50%)" }} />
          </div>
          <div className="flex-1 min-w-0">
            <h2 className="text-[14px] font-bold text-text-primary">{state.title}</h2>
            <input
              autoFocus
              value={value}
              onChange={(e) => setValue(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && canConfirm) onConfirm(trimmed);
                if (e.key === "Escape") onCancel();
              }}
              placeholder="输入新名称"
              className="mt-2 w-full px-3 py-2 rounded-lg bg-bg-tertiary text-[13px] text-text-primary placeholder:text-text-faint outline-none focus:ring-1 focus:ring-accent/30 transition"
            />
          </div>
        </div>
        <div className="flex items-center gap-2.5 px-5 py-4">
          <button
            type="button"
            onClick={() => onConfirm(trimmed)}
            disabled={!canConfirm}
            className={clsx(
              "flex items-center gap-1.5 px-4 py-2 rounded-lg text-[12.5px] font-bold",
              "text-accent-on-text transition-opacity",
              canConfirm ? "hover:opacity-90" : "opacity-50 cursor-not-allowed",
            )}
            style={{ background: "var(--color-accent)" }}
          >
            确认
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
        </div>
      </div>
    </div>,
    document.body,
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
  // Render into <body> via portal so the `fixed inset-0` is relative to
  // the viewport, not to the sidebar's transform containing block
  // (App.tsx wraps <Sidebar> in a `translate-x-*` container for the
  // mobile slide-in animation, which would otherwise trap this dialog
  // inside the sidebar width and push it to the left edge).
  return createPortal(
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
            <p className="mt-2 text-[10.5px] text-text-faint">
              此操作不可恢复
            </p>
          </div>
        </div>
        <div className="flex items-center justify-end gap-2.5 px-5 py-4">
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
        </div>
      </div>
    </div>,
    document.body,
  );
}

