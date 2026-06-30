import type { ExecutionTimelineEvent, SectionGateReport } from "../types";

interface ExecutionTimelineProps {
  events: ExecutionTimelineEvent[];
}

// 2026-06-29 (C3 marker protocol): mirror of the backend
// `Section::label()` (see `crates/hermix-agent/src/context.rs`).
// We render the chip text on the front-end, so we need a label
// table — but it's intentionally a *display* label (matches the
// `serde(rename=...)` tag, not the prompt `### ` marker), so a
// drift would render the wrong chip text, not break the gate
// itself. The unit test in `context.rs::section_label_distinct_from_marker`
// pins the backend labels.
const SECTION_LABEL: Record<
  "identity" | "tool_list" | "tool_enforce" | "working_dir" | "timestamp",
  string
> = {
  identity: "Identity",
  tool_list: "Active tools",
  tool_enforce: "Tool enforcement",
  working_dir: "Working directory",
  timestamp: "Current time",
};

interface TimelineGroup {
  key: string;
  tool: string;
  kind: "tool" | "status";
  status: "running" | "done";
  events: ExecutionTimelineEvent[];
}

function buildGroups(events: ExecutionTimelineEvent[]): TimelineGroup[] {
  const groups: TimelineGroup[] = [];
  const activeToolGroup = new Map<string, number>();

  for (const ev of events) {
    if (ev.type === "status") {
      groups.push({
        key: `status-${groups.length}`,
        tool: "status",
        kind: "status",
        status: "done",
        events: [ev],
      });
      continue;
    }

    const tool = ev.tool?.trim() || "tool";
    let groupIndex = activeToolGroup.get(tool);
    const needsNewGroup = ev.type === "tool_start" || groupIndex === undefined;

    if (needsNewGroup) {
      groups.push({
        key: `${tool}-${groups.length}`,
        tool,
        kind: "tool",
        status: ev.type === "tool_complete" ? "done" : "running",
        events: [],
      });
      groupIndex = groups.length - 1;
      activeToolGroup.set(tool, groupIndex);
    }

    const safeGroupIndex = groupIndex ?? groups.length - 1;
    const group = groups[safeGroupIndex];
    group.events.push(ev);
    if (ev.type === "tool_complete") {
      group.status = "done";
      activeToolGroup.delete(tool);
    }
  }

  return groups;
}

function statusDot(status: "running" | "done"): string {
  return status === "done" ? "bg-emerald-400" : "bg-amber-400 animate-pulse";
}

export function ExecutionTimeline({ events }: ExecutionTimelineProps) {
  if (!events.length) return null;
  const groups = buildGroups(events);

  return (
    <div className="mt-2 rounded-xl border border-white/[0.06] bg-white/[0.03] px-3 py-2">
      <div className="text-[11px] text-text-muted mb-1">执行时间线</div>
      <div className="space-y-2">
        {groups.map((group) => (
          <details key={group.key} className="rounded-lg border border-border-subtle bg-bg-card px-2 py-1.5">
            <summary className="cursor-pointer list-none flex items-center gap-2 text-xs text-text-primary">
              {group.kind === "tool" ? (
                <>
                  <span className={`inline-block h-1.5 w-1.5 rounded-full ${statusDot(group.status)}`} />
                  <span className="font-mono">{group.tool}</span>
                  <span className="text-text-muted">{group.status}</span>
                </>
              ) : (
                <>
                  <span className="inline-block h-1.5 w-1.5 rounded-full bg-info" />
                  <span>状态更新</span>
                </>
              )}
            </summary>
            <div className="mt-2 space-y-1.5">
              {group.events.map((ev, idx) => (
                <div key={`${group.key}-${idx}`} className="text-xs text-text-secondary">
                  <div className="text-text-muted">
                    {ev.type}
                    {ev.chunk_index && ev.chunk_total
                      ? ` (${ev.chunk_index}/${ev.chunk_total})`
                      : ""}
                  </div>
                  {ev.type === "section_gate" && ev.section_gate_report
                    ? <SectionGateRow report={ev.section_gate_report} />
                    : null}
                  {ev.arguments && (
                    <pre className="mt-1 whitespace-pre-wrap break-all rounded bg-bg-tertiary px-2 py-1 text-[11px] text-text-muted">
                      {ev.arguments}
                    </pre>
                  )}
                  {ev.content && ev.type !== "section_gate" && (
                    <pre className="mt-1 max-h-56 overflow-auto whitespace-pre-wrap break-all rounded bg-bg-tertiary px-2 py-1 text-[11px] text-text-secondary">
                      {ev.content}
                    </pre>
                  )}
                </div>
              ))}
            </div>
          </details>
        ))}
      </div>
    </div>
  );
}

/**
 * 2026-06-29 (C3 marker protocol): render the `SectionGateReport`
 * inline. Patched sections get a green-bordered chip ("the gate
 * auto-spliced this in"), pending sections get a dim border ("the
 * gate saw it was missing but the current `RouteContext` doesn't
 * require it"). Empty report collapses to a single "all sections
 * present" line.
 */
function SectionGateRow({ report }: { report: SectionGateReport }) {
  if (report.patched.length === 0 && report.pending.length === 0) {
    return (
      <div className="mt-1 text-[11px] text-text-muted">
        SectionGate: all required sections present
      </div>
    );
  }
  return (
    <div className="mt-1 space-y-1">
      {report.patched.length > 0 ? (
        <div className="flex flex-wrap gap-1">
          <span className="text-[11px] text-text-muted">patched:</span>
          {report.patched.map((g, idx) => (
            <span
              key={`p-${idx}-${g.section}`}
              className="inline-flex items-center rounded border border-emerald-500/40 bg-emerald-500/10 px-1.5 py-0.5 text-[11px] text-emerald-200"
              title={g.reason}
            >
              {SECTION_LABEL[g.section] ?? g.section}
            </span>
          ))}
        </div>
      ) : null}
      {report.pending.length > 0 ? (
        <div className="flex flex-wrap gap-1">
          <span className="text-[11px] text-text-muted">pending:</span>
          {report.pending.map((g, idx) => (
            <span
              key={`d-${idx}-${g.section}`}
              className="inline-flex items-center rounded border border-white/10 bg-white/[0.04] px-1.5 py-0.5 text-[11px] text-text-muted"
              title={g.reason}
            >
              {SECTION_LABEL[g.section] ?? g.section}
            </span>
          ))}
        </div>
      ) : null}
    </div>
  );
}
