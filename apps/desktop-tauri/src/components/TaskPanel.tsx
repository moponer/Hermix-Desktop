import { useState, useCallback, useEffect, useMemo, useRef } from "react";
import { X, ListChecks, AlertCircle, CheckCircle2, Loader2, Clock, ChevronDown, ChevronRight, Scale, Skull, Megaphone, Pencil, Check } from "lucide-react";
import { clsx } from "clsx";
import * as api from "../api";
import type { AgentPersona } from "./agents/types";

// ── Task engine event types (mirror RoomEvent in Rust) ────────
//
// These arrive over the WS stream once S5 wiring is live. Each event
// is appended to a transient timeline so the user can see exactly
// what the engine did.

export type TaskEngineEvent =
  | { kind: "task_phase_change"; node_id: string; description: string; phase: string; assigned_agent: string }
  | { kind: "bus_delivered"; agent_id: string; message_kind: string; text: string }
  | { kind: "audit_verdict"; agent_id: string; violation_id: string; level: string; score_delta: number; score_after: number; reason: string }
  | { kind: "agent_expelled"; agent_id: string; agent_name: string; reason: string }
  | { kind: "room_completed" };

const MAX_TIMELINE = 30;

interface TaskPanelProps {
  roomId: string;
  roomName: string;
  /** Active task, or `null` when no in-flight task. */
  activeTask: api.GroupChatTask | null;
  agents: AgentPersona[];
  /** Live engine events (filled from WS stream). May be `undefined`
   *  in tests / when WS not connected. */
  liveEvents?: TaskEngineEvent[];
  /** Pinned project announcement. `null` when no announcement has
   *  been published yet. Lives in the panel so it sits next to the
   *  task tree and survives together. */
  announcement?: string | null;
  /** RFC 3339 timestamp of when the leader published it. */
  announcementPublishedAt?: string | null;
  onClose: () => void;
  /** Notify the parent that the active task changed (status flip,
   *  subtask completed) so it can re-fetch the room and keep the
   *  hero indicator in sync. */
  onTaskChanged?: () => Promise<void> | void;
  /** Notify the parent that the announcement text changed
   *  (publish/edit/clear) so other surfaces (chat list, sidebar)
   *  stay in sync without a full re-fetch. */
  onAnnouncementChanged?: (text: string | null, publishedAt: string | null) => void;
}

/** Right-side drawer showing the room's active task and its subtasks.
 *
 *  Layout:
 *  1. Header — task original_prompt + status badge.
 *  2. Subtasks — one row per subtask, each with assignee avatar,
 *     description, status badge, and a manual status-flip control
 *     (running/done/failed) so users can override state when the
 *     auto-dispatch flow misclassifies.
 *  3. Empty state — when no active task, surface a "no in-flight task"
 *     message with a hint pointing to the chat input.
 *
 *  Style: single solid colors per status. No gradients.
 */
export function TaskPanel({
  roomId,
  roomName,
  activeTask,
  agents,
  liveEvents,
  announcement,
  announcementPublishedAt,
  onClose,
  onTaskChanged,
  onAnnouncementChanged,
}: TaskPanelProps) {
  const [expandedSubs, setExpandedSubs] = useState<Record<string, boolean>>({});
  const [busySubtaskId, setBusySubtaskId] = useState<string | null>(null);
  const [showTimeline, setShowTimeline] = useState(true);
  const [showAudit, setShowAudit] = useState(true);
  const timelineRef = useRef<HTMLDivElement>(null);

  const agentById = useMemo(() => {
    const m = new Map<string, AgentPersona>();
    for (const a of agents) m.set(a.id, a);
    return m;
  }, [agents]);

  const toggleSub = useCallback((id: string) => {
    setExpandedSubs((prev) => ({ ...prev, [id]: !prev[id] }));
  }, []);

  const setSubtaskStatus = useCallback(
    async (subtaskId: string, status: api.GroupChatSubtaskStatus) => {
      setBusySubtaskId(subtaskId);
      try {
        await api.updateGroupChatSubtask(subtaskId, status);
        if (onTaskChanged) await onTaskChanged();
      } finally {
        setBusySubtaskId(null);
      }
    },
    [onTaskChanged],
  );

  const events = liveEvents ?? [];
  const timeline = events.slice(-MAX_TIMELINE);
  const audit = events.filter(
    (e): e is Extract<TaskEngineEvent, { kind: "audit_verdict" | "agent_expelled" }> =>
      e.kind === "audit_verdict" || e.kind === "agent_expelled",
  );

  // Auto-scroll timeline to bottom when new events arrive.
  useEffect(() => {
    timelineRef.current?.scrollTo({
      top: timelineRef.current.scrollHeight,
      behavior: "smooth",
    });
  }, [timeline.length]);

  return (
    <div className="fixed inset-y-0 right-0 z-50 flex animate-in">
      {/* Backdrop — purely for click-outside-to-close */}
      <button
        type="button"
        aria-label="关闭任务面板"
        onClick={onClose}
        className="flex-1 bg-black/30 backdrop-blur-sm"
      />
      <div
        className={clsx(
          "w-[420px] max-w-[90vw] h-full flex flex-col",
          "bg-bg-secondary border-l border-border-subtle",
          "shadow-2xl",
        )}
      >
        {/* Header */}
        <div className="flex items-center gap-2.5 px-4 py-3 border-b border-border-subtle">
          <ListChecks size={16} style={{ color: "hsl(43, 96%, 48%)" }} />
          <div className="flex-1 min-w-0">
            <h2 className="text-[14px] font-bold text-text-primary truncate">
              任务面板
            </h2>
            <p className="text-[10.5px] text-text-muted truncate">
              {roomName} · {activeTask ? "进行中" : "无活跃任务"}
            </p>
          </div>
          <button
            type="button"
            onClick={onClose}
            className="p-1.5 rounded-md text-text-muted hover:text-text-secondary hover:bg-bg-tertiary transition-colors"
          >
            <X size={14} />
          </button>
        </div>

        {/* Body */}
        <div className="flex-1 overflow-y-auto px-4 py-4 space-y-4">
          {/* Pinned project announcement — sits at the very top so the
              user sees the goal / scope before the task tree below it. */}
          <AnnouncementSection
            roomId={roomId}
            text={announcement ?? null}
            publishedAt={announcementPublishedAt ?? null}
            onChanged={onAnnouncementChanged}
          />

          {activeTask ? (
            <TaskDetail
              task={activeTask}
              agentById={agentById}
              expandedSubs={expandedSubs}
              onToggleSub={toggleSub}
              busySubtaskId={busySubtaskId}
              onSetSubtaskStatus={setSubtaskStatus}
            />
          ) : (
            <EmptyState />
          )}

          {/* Live engine timeline (S5/S6) */}
          {timeline.length > 0 && (
            <section className="rounded-md border border-border-subtle bg-bg-primary/40">
              <button
                type="button"
                onClick={() => setShowTimeline((v) => !v)}
                className="w-full flex items-center gap-2 px-3 py-2 text-left hover:bg-bg-tertiary/40 transition-colors"
              >
                {showTimeline ? (
                  <ChevronDown size={12} className="text-text-muted" />
                ) : (
                  <ChevronRight size={12} className="text-text-muted" />
                )}
                <span className="text-[11.5px] font-semibold text-text-secondary">
                  引擎实时事件
                </span>
                <span className="ml-auto text-[10.5px] text-text-muted">
                  {timeline.length} 条
                </span>
              </button>
              {showTimeline && (
                <div
                  ref={timelineRef}
                  className="max-h-[200px] overflow-y-auto px-2 py-2 space-y-1 border-t border-border-subtle"
                >
                  {timeline.map((e, i) => (
                    <TimelineRow key={i} event={e} agentById={agentById} />
                  ))}
                </div>
              )}
            </section>
          )}

          {/* Audit log (阎罗) */}
          {audit.length > 0 && (
            <section className="rounded-md border border-border-subtle bg-bg-primary/40">
              <button
                type="button"
                onClick={() => setShowAudit((v) => !v)}
                className="w-full flex items-center gap-2 px-3 py-2 text-left hover:bg-bg-tertiary/40 transition-colors"
              >
                {showAudit ? (
                  <ChevronDown size={12} className="text-text-muted" />
                ) : (
                  <ChevronRight size={12} className="text-text-muted" />
                )}
                <Scale size={12} style={{ color: "hsl(43, 96%, 48%)" }} />
                <span className="text-[11.5px] font-semibold text-text-secondary">
                  阎罗审计
                </span>
                <span className="ml-auto text-[10.5px] text-text-muted">
                  {audit.length} 条
                </span>
              </button>
              {showAudit && (
                <div className="px-3 py-2 space-y-1.5 border-t border-border-subtle">
                  {audit.map((e, i) => (
                    <AuditRow key={i} event={e} agentById={agentById} roomId={roomId} />
                  ))}
                </div>
              )}
            </section>
          )}
        </div>
      </div>
    </div>
  );
}

// ── Task Detail ────────────────────────────────────────

function TaskDetail({
  task,
  agentById,
  expandedSubs,
  onToggleSub,
  busySubtaskId,
  onSetSubtaskStatus,
}: {
  task: api.GroupChatTask;
  agentById: Map<string, AgentPersona>;
  expandedSubs: Record<string, boolean>;
  onToggleSub: (id: string) => void;
  busySubtaskId: string | null;
  onSetSubtaskStatus: (id: string, status: api.GroupChatSubtaskStatus) => void;
}) {
  return (
    <div className="flex flex-col gap-4">
      {/* Task summary card */}
      <div
        className="rounded-xl p-3 border"
        style={{
          background: "hsla(43, 96%, 56%, 0.08)",
          borderColor: "hsla(43, 96%, 56%, 0.25)",
        }}
      >
        <div className="flex items-center gap-2 mb-2">
          <StatusBadge status={task.status} />
          <span className="text-[10.5px] text-text-faint">
            {new Date(task.created_at).toLocaleString("zh-CN", {
              month: "2-digit",
              day: "2-digit",
              hour: "2-digit",
              minute: "2-digit",
            })}
          </span>
        </div>
        <p className="text-[13px] font-semibold text-text-primary leading-snug">
          {task.original_prompt}
        </p>
        {task.final_report && (
          <div className="mt-2 pt-2 border-t border-border-subtle">
            <p className="text-[10.5px] text-text-muted mb-1">📄 最终报告</p>
            <p className="text-[12px] text-text-secondary leading-relaxed whitespace-pre-wrap">
              {task.final_report}
            </p>
          </div>
        )}
      </div>

      {/* Subtask list */}
      <div>
        <h3 className="text-[10.5px] font-bold text-text-muted uppercase tracking-wider mb-2">
          子任务 · {task.subtasks.length}
        </h3>
        {task.subtasks.length === 0 ? (
          <p className="text-[12px] text-text-faint italic px-2 py-3">
            总管尚未分解任务
          </p>
        ) : (
          <div className="flex flex-col gap-1.5">
            {task.subtasks.map((s) => (
              <SubtaskRow
                key={s.id}
                subtask={s}
                agent={agentById.get(s.assigned_agent_id)}
                agentById={agentById}
                expanded={expandedSubs[s.id] ?? false}
                onToggle={() => onToggleSub(s.id)}
                busy={busySubtaskId === s.id}
                onSetStatus={(st) => onSetSubtaskStatus(s.id, st)}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

// ── Subtask Row ────────────────────────────────────────

function SubtaskRow({
  subtask,
  agent,
  agentById,
  expanded,
  onToggle,
  busy,
  onSetStatus,
}: {
  subtask: api.GroupChatSubtask;
  agent: AgentPersona | undefined;
  agentById: Map<string, AgentPersona>;
  expanded: boolean;
  onToggle: () => void;
  busy: boolean;
  onSetStatus: (status: api.GroupChatSubtaskStatus) => void;
}) {
  const tone = agent ? agentHsl(agent.id) : "hsl(220, 10%, 50%)";

  return (
    <div
      className={clsx(
        "rounded-lg border border-border-subtle",
        "bg-bg-tertiary/40 overflow-hidden",
      )}
    >
      <button
        type="button"
        onClick={onToggle}
        className="w-full flex items-center gap-2 px-2.5 py-2 text-left hover:bg-bg-tertiary/60 transition-colors"
      >
        {expanded ? <ChevronDown size={11} /> : <ChevronRight size={11} />}
        <span
          className="w-5 h-5 rounded-full flex items-center justify-center text-[9.5px] font-bold shrink-0"
          style={{
            background: `hsl(${tone}, 55%, 55%, 0.20)`,
            color: `hsl(${tone}, 60%, 40%)`,
          }}
          title={agent?.name ?? subtask.assigned_agent_id}
        >
          {agent?.name?.[0] ?? "?"}
        </span>
        <span className="flex-1 min-w-0 text-[12px] text-text-primary truncate">
          {subtask.description}
        </span>
        <StatusBadge status={subtask.status} small />
      </button>

      {expanded && (
        <div className="px-2.5 pb-2.5 pt-1 border-t border-border-subtle/50 flex flex-col gap-2">
          <div className="flex items-center gap-2 text-[10.5px] text-text-muted">
            <span>分配给：</span>
            <span className="font-semibold text-text-secondary">
              {agent?.name ?? subtask.assigned_agent_id}
            </span>
            {subtask.started_at && (
              <span className="ml-auto">
                开始于{" "}
                {new Date(subtask.started_at).toLocaleTimeString("zh-CN", {
                  hour: "2-digit",
                  minute: "2-digit",
                })}
              </span>
            )}
          </div>
          {subtask.result && (
            <div className="rounded-md px-2 py-1.5 bg-bg-primary/60 text-[11.5px] text-text-secondary leading-relaxed whitespace-pre-wrap max-h-32 overflow-y-auto">
              {subtask.result}
            </div>
          )}
          {subtask.review && (
            <ReviewBlock review={subtask.review} agentById={agentById} />
          )}
          {/* Manual override: flip status. Useful when the
              auto-dispatch classified wrong or the user wants to
              retry a subtask. */}
          <div className="flex items-center gap-1.5">
            <span className="text-[10px] text-text-muted">手动调整：</span>
            <OverrideButton
              active={subtask.status === "running"}
              disabled={busy}
              onClick={() => onSetStatus("running")}
              label="运行中"
            />
            <OverrideButton
              active={subtask.status === "done"}
              disabled={busy}
              onClick={() => onSetStatus("done")}
              label="完成"
              success
            />
            <OverrideButton
              active={subtask.status === "failed"}
              disabled={busy}
              onClick={() => onSetStatus("failed")}
              label="失败"
              danger
            />
          </div>
        </div>
      )}
    </div>
  );
}

// ── Review Block (阎罗 review feedback inside subtask) ──

function ReviewBlock({
  review,
  agentById,
}: {
  review: api.GroupChatSubtaskReview;
  agentById: Map<string, AgentPersona>;
}) {
  // Single solid color per outcome — no gradient. Green for passed,
  // red for failed, reviewer id in a muted tone. Comment below.
  const passed = review.passed;
  const accent = passed
    ? "hsl(150, 60%, 45%)"
    : "hsl(0, 70%, 55%)";
  const bg = passed
    ? "hsla(150, 60%, 45%, 0.10)"
    : "hsla(0, 70%, 55%, 0.10)";
  const ring = passed
    ? "hsla(150, 60%, 45%, 0.30)"
    : "hsla(0, 70%, 55%, 0.30)";
  const reviewer = agentById.get(review.reviewer_id);
  const label = passed ? "✓ 通过" : "✗ 打回修订";
  const Icon = passed ? CheckCircle2 : AlertCircle;
  return (
    <div
      className="rounded-md px-2 py-1.5"
      style={{ background: bg, boxShadow: `inset 0 0 0 1px ${ring}` }}
    >
      <div className="flex items-center gap-1.5 text-[10.5px]">
        <Icon size={11} style={{ color: accent }} />
        <span className="font-semibold" style={{ color: accent }}>
          审查{label}
        </span>
        <span className="text-text-faint">·</span>
        <span className="text-text-muted">
          审查人：{reviewer?.name ?? review.reviewer_id}
        </span>
        <span className="ml-auto text-text-faint">
          {new Date(review.reviewed_at).toLocaleTimeString("zh-CN", {
            hour: "2-digit",
            minute: "2-digit",
          })}
        </span>
      </div>
      {review.comment && (
        <p className="mt-1 text-[11px] text-text-secondary leading-relaxed whitespace-pre-wrap">
          {review.comment}
        </p>
      )}
    </div>
  );
}

// ── Empty State ────────────────────────────────────────

function EmptyState() {
  return (
    <div className="flex flex-col items-center justify-center h-full text-center px-6">
      <div
        className="w-14 h-14 rounded-full flex items-center justify-center mb-3"
        style={{ background: "hsla(43, 96%, 56%, 0.10)" }}
      >
        <ListChecks size={24} style={{ color: "hsl(43, 96%, 48%)" }} />
      </div>
      <h3 className="text-[13px] font-semibold text-text-primary mb-1">
        当前无活跃任务
      </h3>
      <p className="text-[11.5px] text-text-muted leading-relaxed">
        在下方输入框发送消息后，
        <br />
        总管会自动分解目标并派发子任务。
      </p>
    </div>
  );
}

// ── Helpers ─────────────────────────────────────────────

function StatusBadge({
  status,
  small = false,
}: {
  status: api.GroupChatTaskStatus | api.GroupChatSubtaskStatus;
  small?: boolean;
}) {
  const config = STATUS_CONFIG[status];
  const Icon = config.icon;
  return (
    <span
      className={clsx(
        "inline-flex items-center gap-1 rounded-full font-semibold",
        small ? "px-1.5 py-0.5 text-[9.5px]" : "px-2 py-0.5 text-[10.5px]",
      )}
      style={{
        background: config.bg,
        color: config.text,
      }}
    >
      <Icon size={small ? 9 : 11} />
      {config.label}
    </span>
  );
}

const STATUS_CONFIG: Record<
  api.GroupChatTaskStatus | api.GroupChatSubtaskStatus,
  { label: string; bg: string; text: string; icon: typeof Clock }
> = {
  // Task statuses
  pending: { label: "待开始", bg: "hsla(220, 10%, 50%, 0.15)", text: "hsl(220, 10%, 40%)", icon: Clock },
  decomposing: { label: "分解中", bg: "hsla(220, 60%, 50%, 0.15)", text: "hsl(220, 60%, 40%)", icon: Loader2 },
  executing: { label: "执行中", bg: "hsla(200, 70%, 50%, 0.15)", text: "hsl(200, 70%, 38%)", icon: Loader2 },
  aggregating: { label: "汇总中", bg: "hsla(200, 70%, 50%, 0.15)", text: "hsl(200, 70%, 38%)", icon: Loader2 },
  awaiting_approval: { label: "待审批", bg: "hsla(43, 96%, 56%, 0.20)", text: "hsl(43, 96%, 32%)", icon: AlertCircle },
  approved: { label: "已批准", bg: "hsla(140, 50%, 50%, 0.15)", text: "hsl(140, 50%, 32%)", icon: CheckCircle2 },
  completed: { label: "已完成", bg: "hsla(140, 60%, 50%, 0.15)", text: "hsl(140, 60%, 32%)", icon: CheckCircle2 },
  failed: { label: "失败", bg: "hsla(0, 60%, 55%, 0.15)", text: "hsl(0, 60%, 40%)", icon: AlertCircle },
  // Subtask statuses (subset above)
  running: { label: "运行中", bg: "hsla(200, 70%, 50%, 0.15)", text: "hsl(200, 70%, 38%)", icon: Loader2 },
  done: { label: "完成", bg: "hsla(140, 60%, 50%, 0.15)", text: "hsl(140, 60%, 32%)", icon: CheckCircle2 },
};

function OverrideButton({
  active,
  disabled,
  onClick,
  label,
  success,
  danger,
}: {
  active: boolean;
  disabled: boolean;
  onClick: () => void;
  label: string;
  success?: boolean;
  danger?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className={clsx(
        "px-2 py-0.5 rounded-md text-[10.5px] font-semibold transition-colors",
        active
          ? "text-white"
          : success
            ? "text-text-muted hover:text-green-500 hover:bg-green-500/10"
            : danger
              ? "text-text-muted hover:text-red-400 hover:bg-red-500/10"
              : "text-text-muted hover:text-text-secondary hover:bg-bg-tertiary",
        disabled && "opacity-50 cursor-not-allowed",
      )}
      style={
        active
          ? { background: success ? "hsl(140, 60%, 45%)" : danger ? "hsl(0, 60%, 50%)" : "hsl(200, 70%, 45%)" }
          : undefined
      }
    >
      {label}
    </button>
  );
}

/** Compute a stable hue from an agent id. Mirrors the logic in
 *  GroupChatView so the panel avatars match the chat avatars. */
function agentHsl(agentId: string): number {
  let h = 0;
  for (let i = 0; i < agentId.length; i++) {
    h = (h * 31 + agentId.charCodeAt(i)) & 0xffff;
  }
  return h % 360;
}

// ── Timeline row ────────────────────────────────────────

function TimelineRow({
  event,
  agentById: _agentById,
}: {
  event: TaskEngineEvent;
  agentById: Map<string, AgentPersona>;
}) {
  switch (event.kind) {
    case "task_phase_change": {
      const hue = agentHsl(event.assigned_agent);
      return (
        <div className="flex items-center gap-2 px-1.5 py-1 text-[11px] text-text-secondary">
          <span
            className="inline-block w-1.5 h-1.5 rounded-full flex-shrink-0"
            style={{ backgroundColor: `hsl(${hue}, 60%, 55%)` }}
          />
          <span className="font-mono text-[10px] text-text-muted truncate">
            {event.node_id}
          </span>
          <span className="text-text-muted">→</span>
          <span className="font-semibold">{event.phase}</span>
          <span className="text-text-muted truncate">· {event.description}</span>
        </div>
      );
    }
    case "bus_delivered":
      return (
        <div className="flex items-center gap-2 px-1.5 py-1 text-[11px] text-text-muted">
          <span className="font-mono text-[10px]">{event.agent_id}</span>
          <span className="text-text-secondary">{event.message_kind}</span>
          <span className="truncate">{event.text}</span>
        </div>
      );
    case "audit_verdict":
      return null; // rendered in AuditRow instead
    case "agent_expelled":
      return null;
    case "room_completed":
      return (
        <div className="flex items-center gap-2 px-1.5 py-1 text-[11px] text-text-secondary font-semibold">
          <CheckCircle2 size={11} style={{ color: "hsl(140, 60%, 50%)" }} />
          房间任务全部完成
        </div>
      );
  }
}

// ── Audit row (阎罗) ────────────────────────────────────────

function AuditRow({
  event,
  agentById: _agentById,
  roomId,
}: {
  event: Extract<TaskEngineEvent, { kind: "audit_verdict" | "agent_expelled" }>;
  agentById: Map<string, AgentPersona>;
  roomId: string;
}) {
  if (event.kind === "agent_expelled") {
    return (
      <div className="flex items-start gap-2 px-1.5 py-1.5 rounded-sm bg-bg-tertiary/60 border-l-2 border-border-strong">
        <Skull size={12} style={{ color: "hsl(0, 70%, 60%)" }} className="mt-0.5 flex-shrink-0" />
        <div className="flex-1 min-w-0">
          <div className="text-[11px] font-bold text-text-primary">
            {event.agent_id} 被开除
          </div>
          <div className="text-[10.5px] text-text-muted truncate">
            原因：{event.reason}
          </div>
        </div>
      </div>
    );
  }
  const isRedLine = event.level.toLowerCase() === "redline";
  const isCritical = event.level.toLowerCase() === "critical";
  const isSevere = event.level.toLowerCase() === "severe";
  const accent =
    isRedLine || isCritical
      ? "hsl(0, 70%, 60%)"
      : isSevere
      ? "hsl(28, 90%, 55%)"
      : "hsl(43, 96%, 48%)";
  const sign = event.score_delta > 0 ? "+" : "";
  return (
    <div
      className="flex items-start gap-2 px-1.5 py-1.5 rounded-sm bg-bg-tertiary/40 border-l-2"
      style={{ borderLeftColor: accent }}
    >
      <Scale size={11} style={{ color: accent }} className="mt-0.5 flex-shrink-0" />
      <div className="flex-1 min-w-0">
        <div className="flex items-baseline gap-1.5">
          <span className="text-[11px] font-semibold text-text-primary">
            {event.agent_id}
          </span>
          <span
            className="text-[10px] font-bold uppercase tracking-wide"
            style={{ color: accent }}
          >
            {event.level}
          </span>
          <span className="ml-auto text-[10px] font-mono text-text-muted">
            {sign}{event.score_delta} → {event.score_after}
          </span>
        </div>
        <div className="text-[10.5px] text-text-muted truncate">
          {event.reason}
        </div>
        {/* P4 — dispute button (only for non-redline violations) */}
        {!isRedLine && (
          <button
            type="button"
            className="mt-1 text-[10px] text-text-faint hover:text-orange-500 transition-colors"
            title="申诉此判罚"
            onClick={async () => {
              const reason = window.prompt("申诉理由（选填）") ?? "";
              try {
                await api.disputeViolation(roomId, event.violation_id, reason);
              } catch { /* best-effort */ }
            }}
          >
            申诉
          </button>
        )}
      </div>
    </div>
  );
}

// ── Announcement Section (P2 — moved from GroupChatView) ────
//
// Inlined here (rather than imported) because the section is tiny
// and it lets us share the agentHsl/relative-time helpers without
// a separate module boundary.
//
// States:
// - **Idle** (text set): collapsed single-line with expand button.
// - **Expanded** (text set): full multi-line content.
// - **Editing** (text set or unset): textarea + save/cancel.
// - **Empty** (text null): a small dashed prompt "尚未发布群公告".

function AnnouncementSection({
  roomId,
  text,
  publishedAt,
  onChanged,
}: {
  roomId: string;
  text: string | null;
  publishedAt: string | null;
  onChanged?: (text: string | null, publishedAt: string | null) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(text ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const beginEdit = useCallback(() => {
    setDraft(text ?? "");
    setEditing(true);
    setError(null);
  }, [text]);

  const cancelEdit = useCallback(() => {
    setEditing(false);
    setDraft(text ?? "");
    setError(null);
  }, [text]);

  const notify = useCallback(
    (next: string | null, publishedAt: string | null) => {
      if (onChanged) onChanged(next, publishedAt);
    },
    [onChanged],
  );

  const save = useCallback(async () => {
    const trimmed = draft.trim();
    setBusy(true);
    setError(null);
    try {
      if (trimmed.length === 0) {
        await api.clearAnnouncement(roomId);
        notify(null, null);
      } else {
        const resp = await api.publishAnnouncement(roomId, trimmed);
        notify(resp.announcement, resp.announcement_published_at);
      }
      setEditing(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }, [draft, roomId, notify]);

  const clear = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      await api.clearAnnouncement(roomId);
      notify(null, null);
      setEditing(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }, [roomId, notify]);

  // ── Empty state ────────────────────────────────────────
  if (!text && !editing) {
    return (
      <button
        type="button"
        onClick={beginEdit}
        className={clsx(
          "w-full text-left px-2.5 py-1.5 rounded-lg",
          "border border-dashed text-[11px]",
          "transition-colors duration-150",
          "flex items-center gap-1.5 group/ann",
          "border-border-subtle text-text-muted hover:text-text-secondary hover:border-border-strong",
        )}
      >
        <Megaphone size={11} className="opacity-50 group-hover/ann:opacity-80" />
        <span className="flex-1">点此发布群公告（项目目标 / 本周重点）</span>
        <Pencil size={10} className="opacity-0 group-hover/ann:opacity-60" />
      </button>
    );
  }

  // ── Editing ────────────────────────────────────────────
  if (editing) {
    return (
      <div
        className={clsx("rounded-xl p-3 flex flex-col gap-2", "border")}
        style={{
          // Gold tint, single color — 渐变禁止
          background: "hsla(43, 96%, 56%, 0.10)",
          borderColor: "hsla(43, 96%, 56%, 0.30)",
        }}
      >
        <div className="flex items-center gap-2 text-[11px] font-semibold text-text-secondary">
          <Megaphone size={13} style={{ color: "hsl(43, 96%, 40%)" }} />
          <span>编辑群公告</span>
        </div>
        <textarea
          autoFocus
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          rows={3}
          maxLength={500}
          placeholder="例：本群目标：实现 X 功能；本周内交付 Y。详细分工见下方任务树。"
          className={clsx(
            "w-full px-2.5 py-2 rounded-lg text-[12.5px] leading-relaxed resize-none",
            "bg-bg-primary border border-border-subtle",
            "focus:outline-none focus:border-border-strong",
          )}
        />
        {error && (
          <p className="text-[11px] text-red-400">{error}</p>
        )}
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={save}
            disabled={busy}
            className={clsx(
              "flex items-center gap-1 px-2.5 py-1 rounded-lg text-[11.5px] font-semibold",
              "text-white transition-opacity",
              busy ? "opacity-50" : "hover:opacity-90",
            )}
            style={{ background: "hsl(43, 96%, 48%)" }}
          >
            <Check size={11} />
            发布
          </button>
          <button
            type="button"
            onClick={cancelEdit}
            disabled={busy}
            className="px-2.5 py-1 rounded-lg text-[11.5px] text-text-muted hover:text-text-secondary transition-colors"
          >
            取消
          </button>
          {text && (
            <button
              type="button"
              onClick={clear}
              disabled={busy}
              className="ml-auto px-2.5 py-1 rounded-lg text-[11.5px] text-red-400 hover:text-red-500 transition-colors"
            >
              撤销公告
            </button>
          )}
        </div>
      </div>
    );
  }

  // ── Display (collapsed or expanded) ─────────────────────
  return (
    <div
      className={clsx(
        "rounded-lg px-2.5 py-1.5 flex items-start gap-2",
        "border",
      )}
      style={{
        background: "hsla(43, 96%, 56%, 0.06)",
        borderColor: "hsla(43, 96%, 56%, 0.20)",
      }}
    >
      <Megaphone
        size={12}
        className="shrink-0 mt-0.5"
        style={{ color: "hsl(43, 96%, 40%)" }}
      />
      <div className="flex-1 min-w-0">
        <p
          className={clsx(
            "text-[11.5px] leading-snug text-text-primary",
            !expanded && "line-clamp-1 cursor-pointer",
          )}
          onClick={() => !expanded && setExpanded(true)}
        >
          {text}
        </p>
        {publishedAt && (
          <span className="text-[9.5px] text-text-faint mt-0.5 block">
            {formatRelative(publishedAt)}
          </span>
        )}
      </div>
      <div className="flex items-center gap-0.5 shrink-0 self-center">
        {!expanded && text && text.length > 60 && (
          <button
            type="button"
            onClick={() => setExpanded(true)}
            className="px-1 py-0.5 text-[10px] text-text-muted hover:text-text-secondary transition-colors"
          >
            展开
          </button>
        )}
        <button
          type="button"
          onClick={beginEdit}
          className="p-0.5 rounded-md text-text-muted hover:text-text-secondary hover:bg-bg-tertiary transition-colors"
          title="编辑"
         aria-label="编辑">
          <Pencil size={10} />
        </button>
        <button
          type="button"
          onClick={clear}
          disabled={busy}
          className="p-0.5 rounded-md text-text-muted hover:text-red-400 hover:bg-bg-tertiary transition-colors disabled:opacity-40"
          title="撤销公告"
         aria-label="撤销公告">
          <X size={10} />
        </button>
      </div>
    </div>
  );
}

/** Format an RFC 3339 timestamp as a relative Chinese string.
 *  Returns "刚刚" / "X 分钟前" / "X 小时前" / locale date for older. */
function formatRelative(rfc3339: string): string {
  const then = new Date(rfc3339).getTime();
  if (Number.isNaN(then)) return "";
  const delta = Date.now() - then;
  if (delta < 60_000) return "刚刚";
  if (delta < 3_600_000) return `${Math.floor(delta / 60_000)} 分钟前`;
  if (delta < 86_400_000) return `${Math.floor(delta / 3_600_000)} 小时前`;
  return new Date(rfc3339).toLocaleDateString("zh-CN", {
    month: "2-digit",
    day: "2-digit",
  });
}