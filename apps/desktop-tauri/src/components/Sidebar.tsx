import {
  MessageSquarePlus,
  ChevronDown, ChevronRight,
  MessageSquare, Users, Plus, FolderOpen,
  Boxes, MoreHorizontal, Loader2,
  // Popover icons
  Pencil, Pin, Archive, Trash2, FolderOpenDot, Copy, FolderMinus,
  Settings, PanelLeftClose, Search,
  type LucideIcon,
} from "lucide-react";
import { clsx } from "clsx";
import { useState, useMemo, useEffect, useCallback, useRef } from "react";
import { createPortal } from "react-dom";
import { getVersion } from "@tauri-apps/api/app";
import { ConnectionIndicator } from "./ConnectionIndicator";
import { SessionPickerDialog } from "./SessionPickerDialog";
import {
  resolveVisibleCount,
  resolveRevealAction,
  LIST_PREVIEW_COUNT,
  type ListReveal,
} from "./sidebarListReveal";
import { invoke } from "@tauri-apps/api/core";
import type { Session, NavPage } from "../types";
import { useWorkspaces } from "../hooks/useWorkspaces";
import { usePinnedArchived } from "../hooks/usePinnedArchived";
import { openInExplorer, copyText } from "../desktopBridge";
import { storage, sessionKey } from "../utils/storageCompat";
import { applyScale } from "../utils/uiScale";
import { formatRelativeTime } from "../utils/time";
import * as api from "../api";
import { toastSuccess, toastError, toastInfo } from "../hooks/useToast";
import { useUserProfile } from "../hooks/useUserProfile";
import { UserAvatar } from "./UserAvatar";
import { EditProfileDialog } from "./EditProfileDialog";
import { UserMenuPopover } from "./UserMenuPopover";

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
  /** Called when a group chat room is selected, created, or deleted in the sidebar.
   *  Pass `null` to clear the current selection (e.g. after deletion). */
  onRoomSelect?: (roomId: string | null) => void;
  /** Create a blank chat with this workspace already selected. */
  onNewChatInWorkspace?: (workspacePath: string) => void;
  onOpenSettings?: () => void;
  /** 折叠左侧栏。这一行是侧栏自身的头部，折叠入口归此最直观。 */
  onCollapseSidebar?: () => void;
  /** 打开会话搜索浮层。搜索是"动作"而非"页面"，所以不占导航位。 */
  onOpenSearch?: () => void;
}

/* ── Group Chat Room types (API-backed) ── */

interface ChatRoom {
  id: string;
  name: string;
  agentIds: string[];
  createdAt: number;
}

/* ── Nav item — 核心功能 ── */

// "搜索"已从主导航移除：它的本质是动作而非位置，做成独立页面会让用户
// 为了搜一次付两次页面切换的代价，还会丢掉当前视图。现改为标题栏图标
// 触发浮层（见 SessionPickerDialog），搜完即走。
const NAV_ITEMS: { icon: React.ElementType; label: string; page: NavPage }[] = [
  { icon: MessageSquare, label: "对话",   page: "chat" },
  { icon: Users,    label: "群聊",    page: "groupchat" },
  { icon: Boxes,    label: "应用",    page: "customapps" },
];

// 已移除：工具箱整合进系统配置面板，保留旧名称便于搜索残留引用。

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
  onOpenSettings,
  onCollapseSidebar,
  onOpenSearch,
}: SidebarProps) {
  const { workspaces, add: addWorkspace, remove: removeWorkspace, rename: renameWorkspace } = useWorkspaces();
  const pinnedArchived = usePinnedArchived();
  const { isPinned, isArchived, togglePin, toggleArchive } = pinnedArchived;

  const { profile, setProfile } = useUserProfile();
  const [editOpen, setEditOpen] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const userCardRef = useRef<HTMLDivElement>(null);

  // 应用版本（来自 tauri.conf.json 的 version）。纯浏览器调试下没有 Tauri
  // command，取不到就显示空串 —— 版本是装饰性信息，缺了不该让侧栏报错。
  const [version, setVersion] = useState("");
  useEffect(() => {
    getVersion()
      .then(setVersion)
      .catch(() => {});
  }, []);

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

  // 列表归集状态。"对话"分组与每个项目的会话列表各持一份，互不干扰。
  const [chatsReveal, setChatsReveal] = useState<ListReveal>("preview");
  const [wsReveal, setWsReveal] = useState<Record<string, ListReveal>>({});
  // "查看全部"弹窗的打开目标：null 关闭；{ wsPath: null } 表示对话分组；
  // { wsPath: string } 表示某个项目。
  const [pickerTarget, setPickerTarget] = useState<{ wsPath: string | null; title: string } | null>(null);

  // 列表变短后（删会话、切换项目）旧的展开状态会残留，导致"明明只剩 3 条
  // 却记着已展开"。归零到折叠态即可，代价只是一次多余的重渲染。
  useEffect(() => {
    if (ungroupedSessions.length <= LIST_PREVIEW_COUNT && chatsReveal !== "preview") {
      setChatsReveal("preview");
    }
  }, [ungroupedSessions.length, chatsReveal]);

  // 项目列表同理，但要逐项判断：一个项目变短不该影响其他项目的展开状态。
  // 只清真正需要复位的那些 key，避免每次会话变动都替换整个对象
  // （那会让所有项目行重渲染）。
  useEffect(() => {
    setWsReveal((prev) => {
      const stale = Object.keys(prev).filter((id) => {
        const ws = workspaces.find((w) => w.id === id);
        if (!ws) return true; // 项目已移除，连状态一起丢
        return (sessionsByWs[ws.path] || []).length <= LIST_PREVIEW_COUNT;
      });
      if (stale.length === 0) return prev;
      const next = { ...prev };
      for (const id of stale) delete next[id];
      return next;
    });
  }, [workspaces, sessionsByWs]);

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

  /* ── 聊天室 ── */
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
      {/* 侧栏头部：版本 + 折叠 / 搜索。
          品牌不放这里 —— 窗口标题栏已有 logo + 应用名，再来一份就是重复。
          这一行的职责只有两个：报告自己跑的是哪个版本，以及提供作用于
          侧栏自身的动作（折叠）。搜索紧邻折叠，都是「这一列」的高频操作。 */}
      <div className="sidebar-head">
        {version ? (
          <span className="sidebar-head-version" title={`Hermix v${version}`}>v{version}</span>
        ) : (
          <span className="sidebar-head-version" />
        )}
        <div className="sidebar-head-actions">
          {onCollapseSidebar && (
            <button
              type="button"
              onClick={onCollapseSidebar}
              className="icon-btn"
              aria-label="折叠侧边栏"
              title="折叠侧边栏"
            >
              <PanelLeftClose className="icon-md" />
            </button>
          )}
          {onOpenSearch && (
            <button
              type="button"
              onClick={onOpenSearch}
              className="icon-btn"
              aria-label="搜索对话"
              title="搜索对话"
            >
              <Search className="icon-md" />
            </button>
          )}
        </div>
      </div>

      {/* Top action list — 菜单式, 首项 = 新建对话 */}
      <nav className="nav sidebar-topnav" style={{ flexShrink: 0 }}>
        <button
          type="button"
          onClick={onNewChat}
          className="nav-item nav-item-primary"
          title="新建对话"
          aria-label="新建对话"
        >
          <MessageSquarePlus className="icon-lg" />
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
            <Icon className="icon-lg" />
            <span>{label}</span>
          </button>
        )        )}

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
                ? <ChevronRight className="icon-md shrink-0" />
                : <ChevronDown className="icon-md shrink-0" />}
              <span>对话</span>
              <span className="section-header-count">{ungroupedSessions.length}</span>
            </button>
          </div>
          {!chatsCollapsed && (
            <div className="sidebar-section-body">
              {ungroupedSessions.length === 0 ? (
                <p className="sidebar-section-empty">暂无对话</p>
              ) : (
                sortByPinArchive(ungroupedSessions, "session")
                  .slice(0, resolveVisibleCount(ungroupedSessions.length, chatsReveal))
                  .map((s) => {
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
                          ? <Loader2 className="icon-md animate-spin text-accent shrink-0" />
                          : isRemoving ? <Loader2 className="icon-md animate-spin" /> : null}
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
              <ListRevealButton
                action={resolveRevealAction(ungroupedSessions.length, chatsReveal)}
                label="对话"
                onExpand={() => setChatsReveal("expanded")}
                onCollapse={() => setChatsReveal("preview")}
                onOpenPicker={() =>
                  setPickerTarget({ wsPath: null, title: "全部对话" })
                }
              />
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
                ? <ChevronRight className="icon-md shrink-0" />
                : <ChevronDown className="icon-md shrink-0" />}
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
                const reveal = wsReveal[ws.id] ?? "preview";
                const collapsed = collapsedWs.has(ws.id);
                const wsRemoving = removingIds.has(ws.id);
                const wsDeleting = deletingIds.has(ws.id);
                const wsDisabled = wsRemoving || wsDeleting;
                const wsPinned = isPinned(ws.id, "workspace");
                return (
                  <div key={ws.id} className={clsx("sidebar-project", wsRemoving && "is-removing", wsDeleting && "is-deleting")}>
                    <div className="sidebar-project-row">
                      <button onClick={() => !wsDisabled && wsSessions.length > 0 && toggleWsCollapsed(ws.id)} className="sidebar-project-toggle" disabled={wsDisabled || wsSessions.length === 0}>
                        <FolderOpen className="icon-md list-row-icon" />
                        {wsRemoving ? <Loader2 className="icon-md animate-spin" /> : null}
                        <span className={clsx("sidebar-project-name", wsRemoving && "opacity-60")}>{ws.name}</span>
                        {wsPinned && <Pin size={10} className="sidebar-row-pin" />}
                        {collapsed
                          ? <ChevronRight className="icon-sm list-row-chevron ml-auto" />
                          : <ChevronDown  className="icon-sm list-row-chevron ml-auto" />}
                      </button>
                      <div className="sidebar-row-actions">
                        <SidebarMenu label="项目操作" items={workspaceMenu(ws)} />
                      </div>
                    </div>
                    {!collapsed && wsSessions.length > 0 && (
                      <div className="sidebar-project-sessions">
                        {sortByPinArchive(wsSessions, "session")
                          .slice(0, resolveVisibleCount(wsSessions.length, reveal))
                          .map((s) => {
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
                                  ? <Loader2 className="icon-md animate-spin text-accent shrink-0" />
                                  : psRemoving ? <Loader2 className="icon-md animate-spin" /> : null}
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
                        <ListRevealButton
                          inset
                          action={resolveRevealAction(wsSessions.length, reveal)}
                          label={ws.name}
                          onExpand={() => setWsReveal((p) => ({ ...p, [ws.id]: "expanded" }))}
                          onCollapse={() => setWsReveal((p) => ({ ...p, [ws.id]: "preview" }))}
                          onOpenPicker={() =>
                            setPickerTarget({ wsPath: ws.path, title: ws.name })
                          }
                        />
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
                ? <ChevronRight className="icon-md shrink-0" />
                : <ChevronDown className="icon-md shrink-0" />}
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
                      <Plus className="icon-md" />
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
                      {roomRemoving ? <Loader2 className="icon-md animate-spin" /> : <Users className="icon-md" />}
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

      {/* Bottom: user card + pop menu */}
      <div className="px-3 pb-3 pt-2" style={{ flexShrink: 0 }}>
        <div
          ref={userCardRef}
          className="user-card"
          role="button"
          tabIndex={0}
          style={{ cursor: "pointer" }}
          onClick={() => setMenuOpen((v) => !v)}
          onDoubleClick={() => {
            /* 紧急重置缩放：双击 user-card 恢复 100%（防死锁）。
               走 applyScale 而非直接写 style，保证布局层收到重算事件。 */
            applyScale(1);
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") { e.preventDefault(); setMenuOpen((v) => !v); }
          }}
        >
          <UserAvatar name={profile.displayName} avatarDataUrl={profile.avatarDataUrl} size={28} />
          <span className="user-card-name" title={profile.displayName}>
            {profile.displayName}
          </span>
          <ConnectionIndicator />
          {onOpenSettings && (
            <button
              onClick={(e) => { e.stopPropagation(); onOpenSettings(); }}
              className="user-card-action"
              title="设置"
            >
              <Settings className="icon-md" />
            </button>
          )}
        </div>
        <UserMenuPopover
          open={menuOpen}
          onClose={() => setMenuOpen(false)}
          anchorRef={userCardRef}
          onNavigateAnalytics={() => onPageChange("analytics")}
          onDisconnect={() => {
            /* restart sidecar = soft disconnect/reconnect.
               必须给可见反馈：静默失败时按钮看起来毫无作用（"点断开连接
               无效"），用户无法区分是没执行、还是执行了但连不上。 */
            toastInfo("正在断开并重连后端服务…");
            invoke("restart_sidecar")
              .then(() => toastInfo("后端服务已重连"))
              .catch((e) => {
                console.error("[Sidebar] restart_sidecar failed:", e);
                toastError("断开重连失败，请稍后重试");
              });
          }}
        />
        {editOpen && (
          <EditProfileDialog
            current={profile}
            onSave={(next) => { setProfile(next); setEditOpen(false); }}
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
      <SessionPickerDialog
        open={pickerTarget !== null}
        title={pickerTarget?.title ?? "全部对话"}
        // 对话分组传全量未分组会话；项目场景传该项目的会话。搜索范围随之
        // 收窄到当前列表，符合"我只在这个列表里找"的直觉。
        sessions={
          pickerTarget
            ? pickerTarget.wsPath === null
              ? ungroupedSessions
              : sessionsByWs[pickerTarget.wsPath] || []
            : []
        }
        onSelect={onSelectSession}
        onClose={() => setPickerTarget(null)}
      />
    </aside>
  );
}

/* ── Scoped helpers ── */

/**
 * 列表底部那一个按钮。三态归集（折叠 / 就地展开 / 查看全部）的交互出口，
 * 全部逻辑集中在 `resolveRevealAction`，这里只负责把 action 映射成外观。
 */
function ListRevealButton({
  action,
  label,
  inset,
  onExpand,
  onCollapse,
  onOpenPicker,
}: {
  action: ReturnType<typeof resolveRevealAction>;
  /** 列表名，用于 title 提示（"查看全部对话" / "查看全部 <项目名>"）。 */
  label: string;
  /** 项目内嵌列表需要额外缩进以对齐子项文本。 */
  inset?: boolean;
  onExpand: () => void;
  onCollapse: () => void;
  onOpenPicker: () => void;
}) {
  if (!action) return null;
  const cls = clsx("sidebar-show-more", inset && "sidebar-show-more-inset");

  if (action.kind === "collapse") {
    return (
      <button type="button" className={cls} onClick={onCollapse} title="收起">
        收起
      </button>
    );
  }
  if (action.kind === "expand") {
    return (
      <button
        type="button"
        className={cls}
        onClick={onExpand}
        title={`查看更多（还有 ${action.hidden} 条）`}
      >
        查看更多（{action.hidden}）
      </button>
    );
  }
  return (
    <button
      type="button"
      className={cls}
      onClick={onOpenPicker}
      title={`在弹窗中查看全部「${label}」并搜索`}
    >
     查看全部（{action.hidden}）…
    </button>
  );
}


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
        <MoreHorizontal className="icon-lg" />
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
          "relative z-10 w-full max-w-base flex flex-col",
          "rounded-xl border shadow-2xl",
          "bg-bg-secondary border-border-subtle",
        )}
      >
        <div className="flex items-start gap-3 px-5 py-4 border-b border-border-subtle">
          <div
            className="w-8 h-8 rounded-lg flex items-center justify-center shrink-0"
            style={{ background: "hsla(210, 70%, 55%, 0.12)" }}
          >
            <Pencil className="icon-lg" style={{ color: "hsl(210, 70%, 50%)" }} />
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
              "flex items-center gap-1.5 px-4 py-2 rounded-lg text-[13px] font-bold",
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
              "flex items-center gap-1.5 px-4 py-2 rounded-lg text-[13px] font-semibold",
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
          "relative z-10 w-full max-w-base flex flex-col",
          "rounded-xl border shadow-2xl",
          "bg-bg-secondary border-border-subtle",
        )}
      >
        <div className="flex items-start gap-3 px-5 py-4 border-b border-border-subtle">
          <div
            className="w-8 h-8 rounded-lg flex items-center justify-center shrink-0"
            style={{ background: "hsla(150, 60%, 50%, 0.12)" }}
          >
            <Trash2 className="icon-lg" style={{ color: "hsl(150, 60%, 45%)" }} />
          </div>
          <div className="flex-1 min-w-0">
            <h2 className="text-[14px] font-bold text-text-primary">{state.title}</h2>
            <p className="mt-1 text-[12px] text-text-muted whitespace-pre-wrap leading-relaxed">
              {state.body}
            </p>
            <p className="mt-2 text-[11px] text-text-faint">
              此操作不可恢复
            </p>
          </div>
        </div>
        <div className="flex items-center justify-end gap-2.5 px-5 py-4">
          <button
            type="button"
            onClick={onCancel}
            className={clsx(
              "flex items-center gap-1.5 px-4 py-2 rounded-lg text-[13px] font-semibold",
              "text-text-muted hover:text-text-secondary hover:bg-bg-tertiary transition-colors",
            )}
          >
            取消
          </button>
          <button
            type="button"
            onClick={onConfirm}
            className={clsx(
              "flex items-center gap-1.5 px-4 py-2 rounded-lg text-[13px] font-bold",
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

