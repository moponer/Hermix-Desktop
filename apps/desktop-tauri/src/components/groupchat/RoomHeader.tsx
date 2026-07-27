/**
 * Room header — the hero strip at the top of the chat area.
 *
 * Shows room name, member count, rank summary, avatar wall, and
 * action buttons (task / members / settings).
 *
 * Extracted from GroupChatView.tsx during the 2026-07 refactor.
 */

import { memo } from "react";
import { Users, Settings, ListChecks } from "lucide-react";
import { clsx } from "clsx";
import type { AgentPersona } from "../agents/types";
import type * as api from "@/api";
import { AgentAvatar, LeaderAvatar } from "./AgentAvatar";
import { RoomRankSummary } from "./RankBadge";
import { agentTone, type ChatRoom } from "./types";

export const RoomHeader = memo(function RoomHeader({
  room,
  roomAgents,
  ranks,
  roomTone,
  onOpenTask,
  onOpenMembers,
  onOpenSettings,
}: {
  room: ChatRoom;
  roomAgents: AgentPersona[];
  ranks?: api.GroupChatRank[];
  roomTone: ReturnType<typeof agentTone>;
  onOpenTask: () => void;
  onOpenMembers: () => void;
  onOpenSettings: () => void;
}) {
  return (
    <div
      className="relative px-5 pt-3 pb-2.5"
      style={{ background: roomTone.hero }}
    >
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2 mb-1">
            <span
              className="text-[12px] font-bold uppercase tracking-wider px-1.5 py-0.5 rounded-md"
              style={{
                background: roomTone.dot + "33",
                color: roomTone.text,
              }}
            >
              群聊
            </span>
            <h3 className="text-[18px] font-bold text-text-primary truncate">
              {room.name}
            </h3>
          </div>
          <p className="text-[13px] text-text-muted flex items-center gap-1.5 flex-wrap">
            <span>
              {room.leader && roomAgents.length > 0
                ? `总管 + ${roomAgents.length} 位成员 · 协作中`
                : room.leader
                  ? "总管已就位 · 在下方邀请成员加入"
                  : roomAgents.length > 0
                    ? `${roomAgents.length} 位成员 · 协作中`
                    : "暂未邀请成员 · 在下方邀请加入"}
            </span>
            <RoomRankSummary ranks={ranks} />
          </p>
        </div>
        <div className="flex items-center gap-1.5 shrink-0">
          <button
            onClick={onOpenTask}
            className={clsx(
              "btn btn-secondary btn-sm relative",
              room.activeTask && "border",
            )}
            style={
              room.activeTask
                ? {
                    borderColor: "hsla(43, 96%, 56%, 0.40)",
                    background: "hsla(43, 96%, 56%, 0.10)",
                    color: "hsl(33, 90%, 32%)",
                  }
                : undefined
            }
            title={
              room.activeTask
                ? `当前任务：${room.activeTask.original_prompt.slice(0, 30)}…`
                : "无活跃任务"
            }
          >
            <ListChecks size={12} />
            任务
            {room.activeTask && (
              <span
                className="absolute -top-0.5 -right-0.5 w-2 h-2 rounded-full animate-pulse"
                style={{ background: "hsl(43, 96%, 48%)" }}
              />
            )}
          </button>
          <button onClick={onOpenMembers} className="btn btn-secondary btn-sm">
            <Users size={12} /> 成员管理
          </button>
          <button onClick={onOpenSettings} className="btn btn-secondary btn-sm">
            <Settings size={12} /> 规则
          </button>
        </div>
      </div>

      {/* Avatar wall */}
      {(room.leader || roomAgents.length > 0) && (
        <div className="mt-3 flex items-center gap-3">
          <div className="flex items-center">
            {room.leader && (
              <div className="relative" style={{ marginLeft: 0, zIndex: 100 }}>
                <LeaderAvatar size={30} ringed />
              </div>
            )}
            {roomAgents.slice(0, 5).map((a, i) => {
              const t = agentTone(a.id);
              return (
                <div
                  key={a.id}
                  className="group/agent relative"
                  style={{ marginLeft: -10, zIndex: roomAgents.length - i }}
                >
                  <AgentAvatar agent={a} size={30} ringed />
                  <span
                    className="absolute -bottom-0.5 -right-0.5 w-2.5 h-2.5 rounded-full ring-2"
                    style={{
                      background: t.dot,
                      ...({
                        "--tw-ring-color": "var(--color-bg-primary)",
                      } as React.CSSProperties),
                    }}
                  />
                </div>
              );
            })}
          </div>
          <div className="text-[12px] text-text-muted leading-tight min-w-0">
            <p className="font-semibold text-text-secondary">
              {room.leader ? "项目总管 · 在岗" : "在线成员"}
            </p>
            <p className="truncate">
              {room.leader && roomAgents.length > 0
                ? `总管 + ${roomAgents
                    .slice(0, 2)
                    .map((a) => a.name)
                    .join("、")}${roomAgents.length > 2 ? ` 等 ${roomAgents.length} 人` : ""}`
                : room.leader
                  ? "等待团队成员加入"
                  : roomAgents.slice(0, 3).map((a) => a.name).join("、") +
                    (roomAgents.length > 3 ? ` 等 ${roomAgents.length} 人` : "")}
            </p>
          </div>
        </div>
      )}
    </div>
  );
});
