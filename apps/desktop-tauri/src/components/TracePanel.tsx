import { useEffect, useRef, useState } from "react";
import { Camera, MousePointer, Keyboard, Type, AlertTriangle, Clock, Activity, Circle } from "lucide-react";
import { subscribeCuaTrace, type TraceEvent, type TraceResponse } from "../api/cua";
import { formatDuration } from "../utils/time";

interface TracePanelProps {
  /** CUA session id. Empty string collapses to the empty-state copy. */
  sessionId: string;
  /** Optional cap on rows rendered (default 100). Older rows are trimmed. */
  maxRows?: number;
}

/**
 * Stage 4.2 — Render the recorded computer-use action timeline for
 * a session. Polls `GET /api/cua/trace/{session_id}` every 3 s while
 * mounted, renders the rows in chronological order, and surfaces
 * the soft-warning chips that the backend already reports
 * (`missing`, `parse_errors`).
 *
 * The component is intentionally minimal — it does not own
 * navigation, selection, or replay. Stage 4.3+ will compose it with
 * the screenshot viewer; for now it's a read-only timeline.
 */
export function TracePanel({ sessionId, maxRows = 100 }: TracePanelProps) {
  const [resp, setResp] = useState<TraceResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const unsubscribeRef = useRef<(() => void) | null>(null);

  useEffect(() => {
    if (!sessionId.trim()) {
      setResp(null);
      return;
    }
    setLoading(true);
    const unsub = subscribeCuaTrace(sessionId, (next) => {
      setResp(next);
      setLoading(false);
    });
    unsubscribeRef.current = unsub;
    return () => {
      if (unsubscribeRef.current) {
        unsubscribeRef.current();
        unsubscribeRef.current = null;
      }
    };
  }, [sessionId]);

  if (!sessionId.trim()) {
    return (
      <EmptyState
        title="没有活动的 CUA 会话"
        body="当 agent 调用 cdp / uia 工具时，操作会按时间顺序记录在这里。"
      />
    );
  }

  if (loading && !resp) {
    return (
      <div className="rounded-xl border border-white/[0.06] bg-white/[0.03] px-3 py-2 text-xs text-text-muted">
        正在读取操作时间线…
      </div>
    );
  }

  if (!resp) {
    return (
      <EmptyState
        title="无法读取 trace 后端"
        body="稍候自动重试。如果持续为空，检查 /api/cua/trace 是否被反向代理拦下。"
      />
    );
  }

  if (resp.missing && resp.events.length === 0) {
    return (
      <EmptyState
        title="暂无操作记录"
        body="JSONL 还没生成——通常出现在会话刚开始或长时间静默之后。"
      />
    );
  }

  if (resp.events.length === 0) {
    return (
      <EmptyState
        title="该会话没有动作"
        body={`已扫描 ${resp.total_rows} 行 JSONL，都属于其它 session。`}
      />
    );
  }

  // Trim to the most recent `maxRows` rows. The recorder is
  // append-only so the array is already in chronological order;
  // showing the tail makes more sense than the head for an
  // operator looking at what just happened.
  const visible = resp.events.slice(-maxRows);

  return (
    <div className="rounded-xl border border-white/[0.06] bg-white/[0.03] px-3 py-2">
      <div className="flex items-center justify-between text-[11px] text-text-muted">
        <span className="flex items-center gap-1.5">
          <Activity className="h-3 w-3" />
          CUA 操作时间线
          <span className="text-text-muted">
            ({visible.length}{resp.events.length > visible.length ? ` / ${resp.events.length}` : ""} 条)
          </span>
        </span>
        {resp.parse_errors ? (
          <span
            className="inline-flex items-center gap-1 rounded border border-amber-500/40 bg-amber-500/10 px-1.5 py-0.5 text-amber-200"
            title="JSONL 中存在无法解析的行，可能是上次写入崩溃。"
          >
            <AlertTriangle className="h-3 w-3" />
            部分行损坏
          </span>
        ) : null}
      </div>
      <ul className="mt-2 space-y-1.5">
        {visible.map((ev, idx) => (
          <li key={`${ev.ts}-${idx}`}>
            <TraceRow ev={ev} />
          </li>
        ))}
      </ul>
    </div>
  );
}

function EmptyState({ title, body }: { title: string; body: string }) {
  return (
    <div className="rounded-xl border border-white/[0.06] bg-white/[0.03] px-3 py-3 text-xs text-text-muted">
      <div className="text-text-primary">{title}</div>
      <div className="mt-1">{body}</div>
    </div>
  );
}

/** Map a trace kind to a Tailwind palette + icon. Pure function. */
function kindStyle(kind: TraceEvent["kind"]): {
  icon: React.ComponentType<{ className?: string }>;
  border: string;
  bg: string;
  text: string;
  label: string;
} {
  if (typeof kind !== "string") {
    // Other { subkind } — keep the chip neutral and surface the subkind.
    return {
      icon: Circle,
      border: "border-white/15",
      bg: "bg-white/[0.04]",
      text: "text-text-secondary",
      label: kind.other.subkind,
    };
  }
  switch (kind) {
    case "screenshot":
      return {
        icon: Camera,
        border: "border-sky-500/30",
        bg: "bg-sky-500/10",
        text: "text-sky-200",
        label: "截图",
      };
    case "click":
      return {
        icon: MousePointer,
        border: "border-blue-500/30",
        bg: "bg-blue-500/10",
        text: "text-blue-200",
        label: "点击",
      };
    case "type_text":
      return {
        icon: Type,
        border: "border-emerald-500/30",
        bg: "bg-emerald-500/10",
        text: "text-emerald-200",
        label: "输入",
      };
    case "press_key":
      return {
        icon: Keyboard,
        border: "border-violet-500/30",
        bg: "bg-violet-500/10",
        text: "text-violet-200",
        label: "按键",
      };
    case "denied":
      return {
        icon: AlertTriangle,
        border: "border-rose-500/40",
        bg: "bg-rose-500/10",
        text: "text-rose-200",
        label: "安全拦截",
      };
    case "timeout":
      return {
        icon: Clock,
        border: "border-amber-500/40",
        bg: "bg-amber-500/10",
        text: "text-amber-200",
        label: "超时",
      };
    case "screen_info":
    default:
      return {
        icon: Activity,
        border: "border-white/15",
        bg: "bg-white/[0.04]",
        text: "text-text-secondary",
        label: typeof kind === "string" ? kind : "screen_info",
      };
  }
}

function TraceRow({ ev }: { ev: TraceEvent }) {
  const style = kindStyle(ev.kind);
  const Icon = style.icon;
  const time = formatTime(ev.ts);
  return (
    <div className="flex items-center gap-2 rounded-lg border border-border-subtle bg-bg-card px-2 py-1.5 text-xs">
      <span
        className={`inline-flex items-center gap-1 rounded border px-1.5 py-0.5 text-[11px] ${style.border} ${style.bg} ${style.text}`}
      >
        <Icon className="h-3 w-3" />
        {style.label}
      </span>
      <span className="font-mono text-text-muted">{time}</span>
      <span className="flex-1 truncate text-text-secondary" title={ev.summary}>
        {ev.summary || <span className="text-text-muted">（无描述）</span>}
      </span>
      <span className="font-mono text-text-muted">{formatDuration(ev.elapsed_ms)}</span>
      {ev.ok ? (
        <span
          className="inline-block h-1.5 w-1.5 rounded-full bg-emerald-400"
          title="ok"
        />
      ) : (
        <span
          className="inline-block h-1.5 w-1.5 rounded-full bg-rose-400"
          title={ev.error ?? "failed"}
        />
      )}
    </div>
  );
}

/** Render the UTC timestamp as `HH:MM:SS.mmm`. The recorder always
 * writes UTC; we display the same so reviewers don't have to do
 * timezone arithmetic to compare with backend logs. */
function formatTime(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const pad = (n: number, w = 2) => String(n).padStart(w, "0");
  return `${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}:${pad(d.getUTCSeconds())}.${pad(d.getUTCMilliseconds(), 3)}`;
}