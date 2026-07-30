/**
 * Room list — the left sidebar showing all chat rooms.
 *
 * Redesigned 2026-07-28 per GROUPCHAT_UI_REDESIGN.md:
 * - Title: "协作空间" (unified naming)
 * - Room items: 56px height, 4px brand-blue selected bar + light-blue bg
 * - Subtitle replaces avatar stack (last message / member status)
 * - No rank chips, no task pulse, no announcement pin in the list
 * - Simple ＋ button instead of icon block
 *
 * Extracted from GroupChatView.tsx during the 2026-07 refactor.
 */

import { memo } from "react";
import { Plus, Trash2, MessageCircle, X, Search, AlertCircle } from "lucide-react";
import { clsx } from "clsx";
import type { AgentPersona } from "../agents/types";
import type * as api from "@/api";
import type { ChatRoom } from "./types";

/** Toast-style inline error banner for the create-room form. */
function InlineError({ message, onDismiss }: { message: string; onDismiss: () => void }) {
  return (
    <div
      role="alert"
      className="flex items-start gap-2 px-2.5 py-2 rounded-lg text-[13px] leading-snug"
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

/** Derive a subtitle for the room list item.
 *  Priority: active task → member count → "还没有消息" */
function roomSubtitle(room: ChatRoom, memberCount: number): string {
  if (room.activeTask) {
    const prompt = room.activeTask.original_prompt;
    return `任务进行中 · ${prompt.slice(0, 20)}${prompt.length > 20 ? "…" : ""}`;
  }
  if (memberCount > 0) {
    return `${memberCount} 名成员 · 协作中`;
  }
  return "还没有消息";
}

const RoomItem = memo(function RoomItem({
  room,
  memberCount,
  active,
  onSelect,
  onDelete,
}: {
  room: ChatRoom;
  memberCount: number;
  active: boolean;
  onSelect: () => void;
  onDelete: () => void;
}) {
  return (
    <button
      onClick={onSelect}
      className={clsx(
        "relative w-full text-left px-4 py-2 rounded-lg transition-all group",
        active ? "" : "hover:bg-bg-hover",
      )}
      style={{
        minHeight: 56,
        background: active ? "var(--color-accent-soft)" : undefined,
      }}
      type="button"
    >
      {/* Selected: 4px brand-blue bar on the left */}
      {active && (
        <span
          className="absolute left-0 top-1/2 -translate-y-1/2 rounded-r-full"
          style={{
            width: 4,
            height: 32,
            background: "var(--color-accent)",
          }}
        />
      )}
      {/* Room name */}
      <div
        className={clsx(
          "text-[14px] font-semibold truncate",
          active ? "text-text-primary" : "text-text-secondary",
        )}
      >
        {room.name}
      </div>
      {/* Subtitle */}
      <div className="text-[12px] text-text-muted truncate mt-0.5">
        {roomSubtitle(room, memberCount)}
      </div>
      {/* Delete button — hover only */}
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
        className="absolute right-2 top-1/2 -translate-y-1/2 opacity-0 group-hover:opacity-100 w-6 h-6 rounded flex items-center justify-center text-text-muted hover:text-error hover:bg-error-bg transition-all cursor-pointer"
        title="删除房间"
      >
        <Trash2 size={13} />
      </span>
    </button>
  );
});

export const RoomList = memo(function RoomList({
  rooms,
  agents,
  activeRoomId,
  ranksByRoom: _ranksByRoom,
  roomSearch,
  showNewRoom,
  newRoomName,
  newRoomStyle: _newRoomStyle,
  createRoomError,
  creatingRoom,
  onSelectRoom,
  onDeleteRoom,
  onToggleNewRoom,
  onNewRoomNameChange,
  onNewRoomStyleChange: _onNewRoomStyleChange,
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
  newRoomStyle: string;
  createRoomError: string | null;
  creatingRoom: boolean;
  onSelectRoom: (id: string) => void;
  onDeleteRoom: (id: string) => void;
  onToggleNewRoom: (show: boolean) => void;
  onNewRoomNameChange: (v: string) => void;
  onNewRoomStyleChange: (v: string) => void;
  onCreateRoom: () => void;
  onRoomSearchChange: (v: string) => void;
}) {
  const filteredRooms = rooms.filter(
    (r) => !roomSearch || r.name.toLowerCase().includes(roomSearch.toLowerCase()),
  );

  return (
    <div
      className="shrink-0 flex flex-col"
      style={{ width: 240, background: "var(--color-bg-primary)" }}
    >
      {/* Title bar — "协作空间" + ＋ button */}
      <div
        className="flex items-center justify-between px-4"
        style={{ height: 56 }}
      >
        <h2 className="text-[14px] font-semibold text-text-primary">协作空间</h2>
        <button
          onClick={() => onToggleNewRoom(true)}
          className="w-7 h-7 rounded-lg flex items-center justify-center transition-all hover:bg-bg-hover"
          style={{ color: "var(--color-accent)" }}
          title="新建房间"
        >
          <Plus size={18} strokeWidth={2} />
        </button>
      </div>

      {/* Divider */}
      <div className="mx-2 border-t" style={{ borderColor: "var(--color-border-subtle)" }} />

      {/* Create room form */}
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
            placeholder="房间名称"
            disabled={creatingRoom}
            className="w-full rounded-lg bg-bg-tertiary px-3 py-2 text-[14px] text-text-primary placeholder:text-text-faint outline-none disabled:opacity-50"
          />
          <div className="flex gap-1.5">
            <button
              onClick={onCreateRoom}
              disabled={!newRoomName.trim() || creatingRoom}
              className="flex-1 py-1.5 rounded-lg text-[13px] font-medium transition-opacity disabled:opacity-40 disabled:cursor-not-allowed"
              style={{
                background: "var(--color-accent)",
                color: "var(--color-accent-on-text)",
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
              className="flex-1 py-1.5 rounded-lg text-[13px] font-medium transition-opacity disabled:opacity-40 disabled:cursor-not-allowed"
              style={{
                background: "var(--color-bg-tertiary)",
                color: "var(--color-text-secondary)",
              }}
            >
              取消
            </button>
          </div>
        </div>
      )}

      {/* Search — only show when > 3 rooms */}
      {rooms.length > 3 && (
        <div className="px-3 pb-1">
          <div className="flex items-center gap-1.5 bg-bg-tertiary rounded-lg px-2.5 py-1.5">
            <Search size={12} className="text-text-faint shrink-0" />
            <input
              type="text"
              value={roomSearch}
              onChange={(e) => onRoomSearchChange(e.target.value)}
              placeholder="搜索房间…"
              className="flex-1 bg-transparent text-[12px] text-text-primary placeholder:text-text-faint outline-none"
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

      {/* Room list */}
      <div className="flex-1 overflow-y-auto px-2 py-1 space-y-0.5 min-h-0">
        {rooms.length === 0 && !showNewRoom && (
          <div className="flex flex-col items-center py-10 text-center">
            <MessageCircle size={32} className="text-text-faint mb-3" />
            <p className="text-[14px] text-text-muted">暂无房间</p>
            <p className="text-[12px] text-text-faint mt-1">点击 ＋ 创建一个房间</p>
          </div>
        )}
        {filteredRooms.map((room) => {
          const memberCount = agents.filter((a) => room.agentIds.includes(a.id)).length;
          return (
            <RoomItem
              key={room.id}
              room={room}
              memberCount={memberCount}
              active={room.id === activeRoomId}
              onSelect={() => onSelectRoom(room.id)}
              onDelete={() => onDeleteRoom(room.id)}
            />
          );
        })}
      </div>
    </div>
  );
});
