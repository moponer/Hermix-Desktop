/**
 * Room header — two-line header with action menu.
 *
 * Theme-aware: accent bar, mode toggle, all surfaces via tokens.
 */

import { memo, useState, useEffect, useRef, useCallback } from "react";
import { MoreHorizontal, FileText, ListChecks, Users, Settings, Trash2, MessagesSquare, Zap } from "lucide-react";
import type { AgentPersona } from "../agents/types";
import type * as api from "@/api";
import { RoomDescriptionEditor } from "./RoomDescriptionEditor";
import { agentTone, type ChatRoom } from "./types";

type MenuItem = {
  key: string;
  label: string;
  icon: typeof FileText;
  onClick: () => void;
  danger?: boolean;
};

export const RoomHeader = memo(function RoomHeader({
  room,
  roomAgents,
  roomTone: _roomTone,
  onOpenTask,
  onOpenMembers,
  onOpenSettings,
  onDescriptionChange,
  onChatModeChange,
  onDeleteRoom,
}: {
  room: ChatRoom;
  roomAgents: AgentPersona[];
  ranks?: api.GroupChatRank[];
  roomTone: ReturnType<typeof agentTone>;
  onOpenTask: () => void;
  onOpenMembers: () => void;
  onOpenSettings: () => void;
  onDescriptionChange?: (next: string | null) => Promise<void> | void;
  onChatModeChange?: (mode: "task_collab" | "free_chat") => Promise<void> | void;
  onDeleteRoom: () => void;
}) {
  const [menuOpen, setMenuOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!menuOpen) return;
    const onClickAway = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) setMenuOpen(false);
    };
    const onEsc = (e: KeyboardEvent) => { if (e.key === "Escape") setMenuOpen(false); };
    window.addEventListener("mousedown", onClickAway);
    window.addEventListener("keydown", onEsc);
    return () => {
      window.removeEventListener("mousedown", onClickAway);
      window.removeEventListener("keydown", onEsc);
    };
  }, [menuOpen]);

  const closeMenu = useCallback(() => setMenuOpen(false), []);

  const menuItems: MenuItem[] = [
    { key: "desc",  label: "编辑描述", icon: FileText,      onClick: closeMenu },
    { key: "task",  label: "任务",     icon: ListChecks,    onClick: () => { closeMenu(); onOpenTask(); } },
    { key: "members", label: "成员",   icon: Users,         onClick: () => { closeMenu(); onOpenMembers(); } },
    { key: "settings", label: "规则",  icon: Settings,      onClick: () => { closeMenu(); onOpenSettings(); } },
    { key: "delete",  label: "删除房间", icon: Trash2,      onClick: () => { closeMenu(); onDeleteRoom(); }, danger: true },
  ];

  const memberCount = roomAgents.length;
  const hasLeader = Boolean(room.leader);
  const subtitle = hasLeader
    ? memberCount > 0 ? `${memberCount} 名成员 · 协作中` : "总管已就位 · 等待成员加入"
    : memberCount > 0 ? `${memberCount} 名成员` : "暂无成员";

  return (
    <div className="relative flex items-start justify-between gap-3 px-6 pt-4 pb-3">
      <div className="min-w-0 flex-1">
        {/* Room name with accent bar */}
        <div className="flex items-center gap-2">
          <span
            className="rounded-r-full shrink-0"
            style={{ width: 4, height: 22, background: "var(--color-accent)" }}
          />
          <h3 className="text-[17px] font-semibold text-text-primary truncate">
            {room.name}
          </h3>
        </div>

        {/* Subtitle */}
        <p className="text-[12px] text-text-muted mt-1 ml-6">
          {subtitle}
          {room.activeTask && (
            <span className="ml-2 inline-flex items-center gap-0.5" style={{ color: "var(--color-warning)" }}>
              <ListChecks size={10} />
              <span className="text-[11px]">任务进行中</span>
            </span>
          )}
        </p>

        {/* Description */}
        <div className="ml-6 mt-1">
          {room.description ? (
            <RoomDescriptionEditor description={room.description} onSave={onDescriptionChange} />
          ) : onDescriptionChange ? (
            <RoomDescriptionEditor description={null} onSave={onDescriptionChange} />
          ) : null}
        </div>

        {/* Mode toggle */}
        {onChatModeChange && (
          <div className="ml-6 mt-2.5 flex items-center gap-0.5 p-0.5 rounded-lg"
            style={{ background: "var(--color-bg-tertiary)" }}>
            <button type="button"
              onClick={() => onChatModeChange("free_chat")}
              className="flex items-center gap-1 px-2.5 py-1 rounded-md text-[12px] font-medium transition-all"
              style={{
                background: room.chatMode === "free_chat" ? "var(--color-bg-primary)" : "transparent",
                color: room.chatMode === "free_chat" ? "var(--color-text-primary)" : "var(--color-text-muted)",
                boxShadow: room.chatMode === "free_chat" ? "0 1px 2px rgba(0,0,0,0.06)" : "none",
              }}
              title="闲聊模式：所有成员自由对话，无需总管调度"
            >
              <MessagesSquare size={12} />闲聊
            </button>
            <button type="button"
              onClick={() => onChatModeChange("task_collab")}
              className="flex items-center gap-1 px-2.5 py-1 rounded-md text-[12px] font-medium transition-all"
              style={{
                background: room.chatMode === "task_collab" ? "var(--color-bg-primary)" : "transparent",
                color: room.chatMode === "task_collab" ? "var(--color-text-primary)" : "var(--color-text-muted)",
                boxShadow: room.chatMode === "task_collab" ? "0 1px 2px rgba(0,0,0,0.06)" : "none",
              }}
              title="协作模式：总管负责分派任务和汇总结果"
            >
              <Zap size={12} />协作
            </button>
          </div>
        )}
      </div>

      {/* ··· dropdown */}
      <div className="relative shrink-0" ref={menuRef}>
        <button onClick={() => setMenuOpen((v) => !v)}
          className="w-8 h-8 rounded-lg flex items-center justify-center text-text-muted hover:bg-bg-hover transition-colors"
          title="更多操作" aria-label="更多操作">
          <MoreHorizontal size={16} />
        </button>
        {menuOpen && (
          <div className="absolute right-0 top-full mt-1 min-w-[160px] rounded-lg border border-border-primary bg-bg-elevated shadow-pop py-1 z-[1000]" role="menu">
            {menuItems.map((item) => {
              const Icon = item.icon;
              return (
                <button key={item.key} type="button" onClick={item.onClick}
                  className={`w-full flex items-center gap-2.5 px-3 py-2 text-left text-[13px] hover:bg-bg-hover transition-colors ${item.danger ? "text-error" : "text-text-primary"}`}
                  role="menuitem">
                  <Icon size={14} className="shrink-0 opacity-80" />
                  <span>{item.label}</span>
                </button>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
});
