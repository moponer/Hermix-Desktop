/**
 * RoomHeader — compact single-row header per design spec.
 *
 * Design: room name (18px/600) + meta line (members · description)
 * + "···" menu (task / members / mode / settings / delete).
 *
 * No avatar tile, no member chips, no segmented mode control,
 * no left accent bar — everything secondary lives behind "···".
 */

import {
  memo,
  useState,
  useEffect,
  useRef,
  useCallback,
} from "react";
import {
  MoreHorizontal,
  ListChecks,
  Users,
  Settings,
  Trash2,
  MessagesSquare,
  Zap,
} from "lucide-react";
import type { AgentPersona } from "../agents/types";
import type * as api from "@/api";
import { RoomDescriptionEditor } from "./RoomDescriptionEditor";
import type { ChatRoom } from "./types";

const HEADER_STYLE: React.CSSProperties = {
  background: "var(--color-bg-primary)",
  borderBottom: "1px solid var(--color-border-primary)",
};

export const RoomHeader = memo(function RoomHeader({
  room,
  roomAgents,
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
      if (menuRef.current && !menuRef.current.contains(e.target as Node))
        setMenuOpen(false);
    };
    const onEsc = (e: KeyboardEvent) => {
      if (e.key === "Escape") setMenuOpen(false);
    };
    window.addEventListener("mousedown", onClickAway);
    window.addEventListener("keydown", onEsc);
    return () => {
      window.removeEventListener("mousedown", onClickAway);
      window.removeEventListener("keydown", onEsc);
    };
  }, [menuOpen]);

  const closeMenu = useCallback(() => setMenuOpen(false), []);

  const memberCount = roomAgents.length;
  const hasMemberChip = memberCount > 0;

  const menuItems = [
    {
      key: "task",
      label: "任务面板",
      icon: <ListChecks size={14} />,
      onClick: () => { closeMenu(); onOpenTask(); },
    },
    {
      key: "members",
      label: "成员",
      icon: <Users size={14} />,
      onClick: () => { closeMenu(); onOpenMembers(); },
    },
    ...(onChatModeChange ? [{
      key: "mode",
      label: room.chatMode === "free_chat" ? "闲聊模式" : "协作模式",
      icon: room.chatMode === "free_chat"
        ? <MessagesSquare size={14} />
        : <Zap size={14} />,
      toggle: true,
      active: room.chatMode === "task_collab",
      onClick: () => {
        const next = room.chatMode === "free_chat" ? "task_collab" as const : "free_chat" as const;
        onChatModeChange(next);
        closeMenu();
      },
    }] : []),
    {
      key: "settings",
      label: "房间规则",
      icon: <Settings size={14} />,
      onClick: () => { closeMenu(); onOpenSettings(); },
    },
    {
      key: "delete",
      label: "删除房间",
      icon: <Trash2 size={14} />,
      onClick: () => { closeMenu(); onDeleteRoom(); },
      danger: true,
    },
  ];

  return (
    <div className="relative" style={HEADER_STYLE}>
      {/* Single row: name + meta + ··· */}
      <div className="px-5 py-3 flex items-start gap-3">
        {/* Name column */}
        <div className="flex-1 min-w-0 pt-0.5">
          <div className="flex items-center gap-2 min-w-0">
            <h3
              className="text-[18px] font-semibold text-text-primary truncate"
              title={room.name}
            >
              {room.name}
            </h3>
            {room.activeTask && (
              <span
                className="shrink-0 flex items-center gap-1 text-[10.5px] px-1.5 py-0.5 rounded-full font-medium"
                style={{
                  background: "color-mix(in srgb, var(--color-warning) 14%, transparent)",
                  color: "var(--color-warning)",
                }}
                title={`进行中：${room.activeTask.original_prompt}`}
              >
                <span className="w-1.5 h-1.5 rounded-full" style={{ background: "var(--color-warning)" }} />
                任务中
              </span>
            )}
          </div>

          {/* Meta line: members · description */}
          <div className="mt-0.5 flex items-center gap-1.5 text-[12px] text-text-muted flex-wrap">
            <span className="flex items-center gap-1">
              <Users size={11} className="shrink-0 opacity-70" />
              {hasMemberChip
                ? `${memberCount} 名成员`
                : "还没有协作成员"}
            </span>
            {onDescriptionChange && (
              <RoomDescriptionEditor
                description={room.description ?? null}
                onSave={onDescriptionChange}
              />
            )}
          </div>
        </div>

        {/* ··· menu */}
        <div className="shrink-0" ref={menuRef}>
          <button
            type="button"
            onClick={() => setMenuOpen((v) => !v)}
            className="w-7 h-7 rounded-lg flex items-center justify-center text-text-muted hover:bg-bg-hover hover:text-text-primary transition-colors"
            title="更多操作"
            aria-label="更多操作"
          >
            <MoreHorizontal size={15} />
          </button>
          {menuOpen && (
            <div
              className="absolute right-0 top-full mt-1 min-w-[172px] rounded-lg border border-border-primary bg-bg-elevated shadow-pop py-1 z-[1000]"
              role="menu"
            >
              {menuItems.map((item) => {
                const isToggle = "toggle" in item && item.toggle === true;
                return (
                  <button
                    key={item.key}
                    type="button"
                    onClick={item.onClick}
                    className={`w-full flex items-center gap-2.5 px-3 py-2 text-left text-[13px] transition-colors ${
                      "danger" in item && item.danger
                        ? "text-error hover:bg-error-soft"
                        : "text-text-primary hover:bg-bg-hover"
                    }`}
                    role={isToggle ? "menuitemcheckbox" : "menuitem"}
                    aria-checked={isToggle ? ("active" in item && item.active) : undefined}
                  >
                    <span className="shrink-0 opacity-70">{item.icon}</span>
                    <span className={isToggle && ("active" in item) && item.active ? "font-medium" : ""}>{item.label}</span>
                    {isToggle && ("active" in item) && item.active && (
                      <span className="ml-auto text-[10px]" style={{ color: "var(--color-accent)" }}>✓</span>
                    )}
                  </button>
                );
              })}
            </div>
          )}
        </div>
      </div>
    </div>
  );
});
