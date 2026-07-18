/**
 * Room list — the left sidebar showing all chat rooms.
 *
 * Includes the room search, create-room form, and room items with
 * rank chips and avatar stacks.
 *
 * Extracted from GroupChatView.tsx during the 2026-07 refactor.
 */

import { memo } from "react";
import { Users, Plus, Trash2, MessageCircle, X, Search, AlertCircle } from "lucide-react";
import { clsx } from "clsx";
import type { AgentPersona } from "../agents/types";
import type * as api from "@/api";
import { AgentStack } from "./AgentAvatar";
import { RoomRankChip } from "./RankBadge";
import type { ChatRoom } from "./types";

/** Toast-style inline error banner for the create-room form. */
function InlineError({ message, onDismiss }: { message: string; onDismiss: () => void }) {
  return (
    <div
      role="alert"
      className="flex items-start gap-2 px-2.5 py-2 rounded-lg text-[12px] leading-snug"
      style={{
        background: "var(--color-error-bg, rgba(239,68,68,0.10))",
        color: "var(--color-error-text, #b91c1c)",
      }}
    >
      <AlertCircle size={13} className="shrink-0 mt-[1px]" />
      <span className="flex-1 break-words">{message}</span>
      <button
        onClick={onDismiss}
        className="shrink-0 opacity-60 hover:opacity-100"
        title="关闭"
        type="button"
        aria-label="关闭"
      >
        <X size={12} />
      </button>
    </div>
  );
}

const RoomItem = memo(function RoomItem({
  room,
  agents,
  active,
  onSelect,
  onDelete,
  ranks,
}: {
  room: ChatRoom;
  agents: AgentPersona[];
  active: boolean;
  onSelect: () => void;
  onDelete: () => void;
  ranks?: api.GroupChatRank[];
}) {
  const roomAgents = agents.filter((a) => room.agentIds.includes(a.id));
  return (
    <button
      onClick={onSelect}
      className={clsx(
        "relative w-full text-left px-3 py-2.5 rounded-xl transition-all group",
        active ? "bg-bg-hover shadow-[0_1px_2px_rgba(0,0,0,0.04)]" : "hover:bg-bg-hover",
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
            onClick={(e) => {
              e.stopPropagation();
              if (window.confirm("确认解散该会话？这将清空全部聊天内容和产物，此操作不可撤销。"))
                onDelete();
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.stopPropagation();
                e.preventDefault();
                onDelete();
              }
            }}
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

export const RoomList = memo(function RoomList({
  rooms,
  agents,
  activeRoomId,
  ranksByRoom,
  roomSearch,
  showNewRoom,
  newRoomName,
  createRoomError,
  creatingRoom,
  onSelectRoom,
  onDeleteRoom,
  onToggleNewRoom,
  onNewRoomNameChange,
  onCreateRoom,
  onRoomSearchChange,
}: {
  rooms: ChatRoom[];
  agents: AgentPersona[];
  activeRoomId: string | null;
  ranksByRoom: Record<string, api.GroupChatRank[]>;
  roomSearch: string;
  showNewRoom: boolean;
  newRoomName: string;
  createRoomError: string | null;
  creatingRoom: boolean;
  onSelectRoom: (id: string) => void;
  onDeleteRoom: (id: string) => void;
  onToggleNewRoom: (show: boolean) => void;
  onNewRoomNameChange: (v: string) => void;
  onCreateRoom: () => void;
  onRoomSearchChange: (v: string) => void;
}) {
  const filteredRooms = rooms.filter(
    (r) => !roomSearch || r.name.toLowerCase().includes(roomSearch.toLowerCase()),
  );

  return (
    <div className="w-60 shrink-0 flex flex-col bg-bg-primary">
      <div className="px-4 py-3 flex items-center justify-between relative z-10">
        <div className="flex items-center gap-2">
          <div
            className="w-6 h-6 rounded-lg flex items-center justify-center"
            style={{
              background: "var(--color-bg-tertiary)",
              color: "var(--color-text-secondary)",
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
          onClick={() => onToggleNewRoom(true)}
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
            <InlineError message={createRoomError} onDismiss={() => onToggleNewRoom(false)} />
          )}
          <input
            autoFocus
            value={newRoomName}
            onChange={(e) => onNewRoomNameChange(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !creatingRoom) {
                e.preventDefault();
                onCreateRoom();
              }
              if (e.key === "Escape") {
                onToggleNewRoom(false);
                onNewRoomNameChange("");
              }
            }}
            placeholder="聊天室名称"
            disabled={creatingRoom}
            className="w-full rounded-lg bg-bg-tertiary px-3 py-2 text-[13px] text-text-primary placeholder:text-text-faint outline-none disabled:opacity-50"
          />
          <div className="flex gap-1.5">
            <button
              onClick={onCreateRoom}
              disabled={!newRoomName.trim() || creatingRoom}
              className="flex-1 py-1.5 rounded-lg text-[12px] font-medium transition-opacity disabled:opacity-40 disabled:cursor-not-allowed"
              style={{
                background: "var(--color-btn-primary-bg)",
                color: "var(--color-btn-primary-text)",
              }}
            >
              {creatingRoom ? "创建中…" : "创建"}
            </button>
            <button
              onClick={() => {
                onToggleNewRoom(false);
                onNewRoomNameChange("");
              }}
              disabled={creatingRoom}
              className="flex-1 py-1.5 rounded-lg text-[12px] font-medium transition-opacity disabled:opacity-40 disabled:cursor-not-allowed"
              style={{
                background: "var(--color-btn-secondary-bg)",
                color: "var(--color-btn-secondary-text)",
              }}
            >
              取消
            </button>
          </div>
        </div>
      )}

      {rooms.length > 3 && (
        <div className="px-3 pb-1">
          <div className="flex items-center gap-1.5 bg-bg-tertiary rounded-lg px-2.5 py-1.5">
            <Search size={12} className="text-text-faint shrink-0" />
            <input
              type="text"
              value={roomSearch}
              onChange={(e) => onRoomSearchChange(e.target.value)}
              placeholder="搜索房间…"
              className="flex-1 bg-transparent text-[11px] text-text-primary placeholder:text-text-faint outline-none"
            />
            {roomSearch && (
              <button
                type="button"
                onClick={() => onRoomSearchChange("")}
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
        {filteredRooms.map((room) => (
          <RoomItem
            key={room.id}
            room={room}
            agents={agents}
            active={room.id === activeRoomId}
            onSelect={() => onSelectRoom(room.id)}
            onDelete={() => onDeleteRoom(room.id)}
            ranks={ranksByRoom[room.id]}
          />
        ))}
      </div>
    </div>
  );
});
