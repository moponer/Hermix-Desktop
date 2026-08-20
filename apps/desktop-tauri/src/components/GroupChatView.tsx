/**
 * GroupChatView — container component for the group chat module.
 *
 * Split from the original ~1648-line GroupChatView into modular
 * components under components/groupchat/. This file now handles only
 * state management + layout composition (~300 lines).
 *
 * See docs/GROUPCHAT_REFACTOR_PLAN.md §3.2 for the split plan.
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { Users } from "lucide-react";
import type { AgentPersona } from "./agents/types";
import { BUILTIN_EXPERTS, type ExpertDef } from "../data/builtinExperts";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";
import * as api from "../api";
import { RoomSettingsDrawer } from "./RoomSettingsDrawer";
import { RoomMemberDialog } from "./RoomMemberDialog";
import { TaskPanel, type TaskEngineEvent } from "./TaskPanel";
import { ChatComposerInput, type ChatComposerInputHandle } from "./ChatComposerInput";
import { ImperativeDialog, type ImperativeDialogHandle } from "./ImperativeDialog";

import { RoomList } from "./groupchat/RoomList";
import { RoomHeader } from "./groupchat/RoomHeader";
import { MessageList } from "./groupchat/MessageList";
import { useGroupChatWebSocket } from "./groupchat/useGroupChatWebSocket";
import { agentTone, type ChatRoom } from "./groupchat/types";
import { toastError } from "../hooks/useToast";

/* ── Agent loading helpers ── */

function pickColorForExpert(
  id: string,
): "violet" | "rose" | "cyan" | "emerald" | "amber" | "teal" | "indigo" | "fuchsia" {
  const themes = ["violet", "rose", "cyan", "emerald", "amber", "teal", "indigo", "fuchsia"] as const;
  let h = 0;
  for (const ch of id) h = ((h << 5) - h + ch.charCodeAt(0)) | 0;
  return themes[Math.abs(h) % themes.length];
}

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

function loadAgents(): AgentPersona[] {
  const userAgents: AgentPersona[] = (() => {
    try {
      return JSON.parse(storage.getItem(STORAGE_KEYS.agents) || "[]");
    } catch {
      return [];
    }
  })();
  const builtin = BUILTIN_EXPERTS.map(expertToPersona);
  const userIds = new Set(userAgents.map((a) => a.id));
  return [...builtin.filter((b) => !userIds.has(b.id)), ...userAgents];
}

/* ── Main component ── */

interface GroupChatViewProps {
  initialRoomId?: string | null;
}

export function GroupChatView({ initialRoomId }: GroupChatViewProps = {}) {
  const [rooms, setRooms] = useState<ChatRoom[]>([]);
  // F6 — `agents` is essentially static (computed once from localStorage
  // + BUILTIN_EXPERTS). We previously kept it in `useState` and polled
  // every 5s, but the polling added CPU cost for no real-world benefit.
  // Plain const keeps the API stable (rooms filter against this list)
  // without the React state churn. To support cross-tab agent edits,
  // add a `window.storage` listener here that forces a re-derivation.
  const agents: AgentPersona[] = loadAgents();
  const [activeRoomId, setActiveRoomId] = useState<string | null>(initialRoomId ?? null);

  const [showNewRoom, setShowNewRoom] = useState(false);
  const [newRoomName, setNewRoomName] = useState("");
  const [newRoomStyle, setNewRoomStyle] = useState<string>("modern");
  const [createRoomError, setCreateRoomError] = useState<string | null>(null);
  const [creatingRoom, setCreatingRoom] = useState(false);
  const [roomSearch, setRoomSearch] = useState("");

  // Per-room rank snapshots
  const [ranksByRoom, setRanksByRoom] = useState<Record<string, api.GroupChatRank[]>>({});

  // Task-engine events per room
  const [engineEventsByRoom, setEngineEventsByRoom] = useState<Record<string, TaskEngineEvent[]>>({});

  // Dialogs
  const memberDialogRef = useRef<ImperativeDialogHandle>(null);
  const settingsDialogRef = useRef<ImperativeDialogHandle>(null);
  const taskDialogRef = useRef<ImperativeDialogHandle>(null);
  const composerRef = useRef<ChatComposerInputHandle>(null);

  // Engine event callback for the WS hook
  const appendEngineEvent = useCallback((roomId: string, ev: TaskEngineEvent) => {
    setEngineEventsByRoom((prev) => {
      const list = prev[roomId] ?? [];
      const next = [...list, ev];
      if (next.length > 60) next.splice(0, next.length - 60);
      return { ...prev, [roomId]: next };
    });
  }, []);

  // WS hook — manages messages, streaming, inline events
  const {
    messages,
    streamingAgents,
    loading,
    inlineEvents,
    sendMessage: wsSendMessage,
    setMessages,
  } = useGroupChatWebSocket(activeRoomId, appendEngineEvent);

  /* ── Data loading ── */

  const loadRooms = useCallback(async () => {
    try {
      const apiRooms = await api.listGroupChatRooms();
      setRooms(
        apiRooms.map((r) => ({
          id: r.id,
          name: r.name,
          agentIds: r.agent_ids,
          leaderId: r.leader_id,
          leader: r.leader ?? null,
          memberModels: Object.fromEntries(
            (r.members ?? [])
              .filter((m) => m.model_override)
              .map((m) => [m.agent_id, m.model_override!]),
          ),
          memberDisplayNames: Object.fromEntries(
            (r.members ?? [])
              .filter(
                (m): m is api.GroupChatMember & { display_name: string } =>
                  Boolean(m.display_name) && (m.display_name?.trim().length ?? 0) > 0,
              )
              .map((m) => [m.agent_id, m.display_name]),
          ),
          announcement: r.announcement ?? null,
          announcementPublishedAt: r.announcement_published_at ?? null,
          description: r.description ?? null,
style: r.style ?? null,
chatMode: (r.chat_mode === "free_chat" ? "free_chat" : "task_collab") as "task_collab" | "free_chat",
activeTask: r.active_task ?? null,
        })),
      );
    } catch {
      setRooms([]);
    }
  }, []);

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

  /* ── Effects ── */

  useEffect(() => {
    loadRooms();
  }, [loadRooms]);

  useEffect(() => {
    loadAllRanks(rooms.map((r) => r.id));
  }, [rooms, loadAllRanks]);

  useEffect(() => {
    if (initialRoomId && initialRoomId !== activeRoomId) {
      setActiveRoomId(initialRoomId);
    }
  }, [initialRoomId]);

  // Re-fetch rooms when activeRoomId points to a room not in the list
  useEffect(() => {
    if (activeRoomId && !rooms.some((r) => r.id === activeRoomId)) {
      loadRooms();
    }
  }, [activeRoomId, rooms, loadRooms]);

  // Auto-select first room
  useEffect(() => {
    if (!activeRoomId && rooms.length > 0) {
      setActiveRoomId(rooms[0].id);
    }
  }, [activeRoomId, rooms]);

  // F6 — agents polling removed. The 5-second interval was a safety
  // net for localStorage edits in another tab; in practice agents are
  // created/removed through the chat UI which already calls
  // `setAgents(...)` directly. The mount-time `useState(loadAgents)`
  // initializer covers the cold-start case. If cross-tab sync becomes
  // a real requirement, wire a `window.storage` event listener here.
  void loadAgents;

  /* ── Handlers ── */

  const handleCreateRoom = useCallback(async () => {
    const trimmed = newRoomName.trim();
    if (!trimmed) return;
    setCreatingRoom(true);
    setCreateRoomError(null);
    try {
      const room = await api.createGroupChatRoom(trimmed, newRoomStyle);
      setRooms((prev) => [
        ...prev,
        {
          id: room.id,
          name: room.name,
          agentIds: room.agent_ids,
          leaderId: room.leader_id,
          leader: room.leader ?? null,
          memberModels: {},
          memberDisplayNames: {},
          announcement: room.announcement ?? null,
          announcementPublishedAt: room.announcement_published_at ?? null,
          description: room.description ?? null,
style: room.style ?? null,
chatMode: (room.chat_mode === "free_chat" ? "free_chat" : "task_collab") as "task_collab" | "free_chat",
activeTask: room.active_task ?? null,
        },
      ]);
      setActiveRoomId(room.id);
      setNewRoomName("");
      setNewRoomStyle("modern");
      setShowNewRoom(false);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      console.error("Failed to create room:", e);
      setCreateRoomError(`创建聊天室失败:${msg}`);
    } finally {
      setCreatingRoom(false);
    }
  }, [newRoomName]);

  const handleNewRoomNameChange = useCallback((v: string) => {
    setNewRoomName(v);
    setCreateRoomError((prev) => (prev ? null : prev));
  }, []);

  const handleDeleteRoom = useCallback(async (id: string) => {
    if (!window.confirm("确认解散该会话？这将清空全部聊天内容和产物，此操作不可撤销。"))
      return;
    try {
      await api.deleteGroupChatRoom(id);
      setRooms((prev) => {
        const next = prev.filter((r) => r.id !== id);
        setActiveRoomId((curr) => (curr === id ? next[0]?.id ?? null : curr));
        return next;
      });
      setMessages((prev) => prev.filter((m) => m.roomId !== id));
    } catch (e) {
      console.error("Failed to delete room:", e);
toastError("删除聊天室失败");
    }
  }, []);

  // used by the announcement setter in TaskPanel's onAnnouncementChanged:
  // optimistically update local state, then let the server reconcile
  // via the next list_rooms poll / WebSocket event. Errors surface as
  // toasts and leave the local state untouched so the user can retry.
  const handleDescriptionChange = useCallback(
    async (roomId: string, next: string | null) => {
      try {
        if (next === null) {
          await api.clearGroupChatDescription(roomId);
        } else {
          await api.updateGroupChatDescription(roomId, next);
        }
        setRooms((prev) =>
          prev.map((r) => (r.id === roomId ? { ...r, description: next } : r)),
        );
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        console.error("Failed to update description:", e);
      toastError("更新房间描述失败:" + msg);
    }
  },
  [],
);

  // Chat mode toggle: switch between "task_collab" (leader dispatches)
  // and "free_chat" (members respond freely). Optimistic local update +
  // server PATCH. Errors revert and toast.
  const handleChatModeChange = useCallback(
    async (roomId: string, mode: "task_collab" | "free_chat") => {
      const prev = rooms.find((r) => r.id === roomId);
      if (!prev || prev.chatMode === mode) return;
      setRooms((curr) =>
        curr.map((r) => (r.id === roomId ? { ...r, chatMode: mode } : r)),
      );
      try {
        await api.updateGroupChatChatMode(roomId, mode);
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        console.error("Failed to update chat mode:", e);
        toastError("切换聊天模式失败:" + msg);
        // Revert
        if (prev) {
          setRooms((curr) =>
            curr.map((r) => (r.id === roomId ? { ...r, chatMode: prev.chatMode } : r)),
          );
        }
      }
    },
    [rooms],
  );

  const handleSendMessage = useCallback(async (textOverride?: string) => {
    const text = (textOverride ?? composerRef.current?.getValue() ?? "").trim();
    if (!text || !activeRoomId) return;
    composerRef.current?.clear();
    await wsSendMessage(text);
  }, [activeRoomId, wsSendMessage]);

  const handleRetry = useCallback(() => {
    const lastUserMsg = [...messages].reverse().find((m) => m.senderId === "user");
    if (lastUserMsg) {
      handleSendMessage(lastUserMsg.content);
    }
  }, [messages, handleSendMessage]);

  // Stable dialog-open callbacks — prevent child re-renders during streaming
  const openTask = useCallback(() => taskDialogRef.current?.open(), []);
  const openMembers = useCallback(() => memberDialogRef.current?.open(), []);
  const openSettings = useCallback(() => settingsDialogRef.current?.open(), []);

  /* ── Derived state ── */

  const activeRoom = rooms.find((r) => r.id === activeRoomId);
  const roomMessages = messages.filter((m) => m.roomId === activeRoomId);
  const roomAgents = activeRoom
    ? agents.filter((a) => activeRoom.agentIds.includes(a.id))
    : [];
  const roomTone = activeRoom ? agentTone(activeRoom.id) : null;
  const roomInlineEvents = activeRoomId ? inlineEvents : [];

  /* ── Render ── */

  return (
    <>
      {/* Full-height flex: left sidebar (RoomList) + right chat panel */}
      <div className="h-full flex overflow-hidden" style={{ background: "var(--color-bg-app)" }}>
        {/* ── Left sidebar ── */}
        <RoomList
          rooms={rooms}
          agents={agents}
          activeRoomId={activeRoomId}
          ranksByRoom={ranksByRoom}
          roomSearch={roomSearch}
          showNewRoom={showNewRoom}
          newRoomName={newRoomName}
          createRoomError={createRoomError}
          creatingRoom={creatingRoom}
          onSelectRoom={setActiveRoomId}
          onDeleteRoom={handleDeleteRoom}
          onToggleNewRoom={setShowNewRoom}
          onNewRoomNameChange={handleNewRoomNameChange}
          onCreateRoom={handleCreateRoom}
          onRoomSearchChange={setRoomSearch}
        />

        {/* ── Right chat panel ── */}
        <div
          className="flex-1 flex flex-col min-w-0 overflow-hidden"
          style={{ background: "var(--color-bg-primary)" }}
        >
          {activeRoom && roomTone ? (
            <>
              <RoomHeader
                room={activeRoom}
                roomAgents={roomAgents}
                ranks={ranksByRoom[activeRoom.id]}
                roomTone={roomTone}
                onOpenTask={openTask}
                onOpenMembers={openMembers}
                onOpenSettings={openSettings}
                onDescriptionChange={(next) => handleDescriptionChange(activeRoom.id, next)}
                onChatModeChange={(mode) => handleChatModeChange(activeRoom.id, mode)}
                onDeleteRoom={() => handleDeleteRoom(activeRoom.id)}
              />

              {/* Divider between header and messages */}
              <div className="border-t" style={{ borderColor: "var(--color-border-subtle)" }} />

              <MessageList
                messages={roomMessages}
                agents={agents}
                streamingAgents={streamingAgents}
                inlineEvents={roomInlineEvents}
                roomTone={roomTone}
                roomAgents={roomAgents}
                hasLeader={!!activeRoom.leader}
                roomStyle={activeRoom.style ?? "modern"}
                onRetry={handleRetry}
                onOpenMembers={openMembers}
                ranks={ranksByRoom[activeRoom.id]}
                modelOverrides={activeRoom.memberModels}
              />

              {/* Input area — bottom border + subtle bg lift */}
              <div
                className="px-5 py-3 border-t"
                style={{ borderColor: "var(--color-border-subtle)", background: "var(--color-bg-secondary)" }}
              >
                <ChatComposerInput
                  ref={composerRef}
                  onSend={handleSendMessage}
                  isStreaming={loading}
                  disabled={roomAgents.length === 0}
                  placeholder={
                    roomAgents.length > 0
                      ? `给总管发送消息…`
                      : "添加成员后可发送消息"
                  }
                />
                {loading && (
                  <div className="flex items-center gap-1.5 mt-2 text-[12px] text-text-muted">
                    <span className="flex gap-0.5">
                      {[0, 150, 300].map((d) => (
                        <span
                          key={d}
                          className="w-1 h-1 rounded-full bg-text-muted animate-bounce"
                          style={{ animationDelay: `${d}ms` }}
                        />
                      ))}
                    </span>
                    <span>成员们正在讨论…</span>
                  </div>
                )}
              </div>
            </>
          ) : (
            <div className="flex-1 flex items-center justify-center">
              <div className="text-center">
                <div className="inline-flex items-center justify-center mb-3 rounded-full"
                  style={{ width: 56, height: 56, background: "var(--color-accent-soft)" }}>
                  <Users size={24} style={{ color: "var(--color-accent)" }} />
                </div>
                <p className="text-[15px] font-semibold text-text-primary mb-1">选择或创建房间</p>
                <p className="text-[12px] text-text-muted">在左侧创建房间并邀请成员开始协作</p>
              </div>
            </div>
          )}
        </div>
      </div>

      {/* Dialogs */}
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
