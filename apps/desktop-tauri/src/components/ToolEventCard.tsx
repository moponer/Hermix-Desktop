import { useEffect, useMemo, useRef, useState } from "react";
import {
  Terminal, Globe, Wrench, Loader2,
  ChevronDown, ChevronRight,
  X as XIcon, Copy, CheckCheck, Circle,
} from "lucide-react";
import { formatDuration } from "../utils/time";
import type { ExecutionTimelineEvent } from "../types";

interface ToolEventCardProps {
  events: ExecutionTimelineEvent[];
}

interface ToolGroup {
  key: string;
  tool: string;
  status: "running" | "done" | "error";
  events: ExecutionTimelineEvent[];
  startedAt?: string;
  endedAt?: string;
  /** `tool_start` arguments — primary arg hint comes from this. */
  args?: string;
  /** One-line status note (e.g. "正在处理") captured between start and
   *  complete. Attached, never rendered as a standalone card. */
  statusNote?: string;
}

/**
 * Inspect a `tool_complete` payload and decide whether the
 * invocation ended in error. Mirrors the helper of the same name
 * in `messageParser.ts` — duplicated here so the ACUI timeline
 * renderer stays zero-dep on `messageParser` (which pulls in
 * MarkdownBody, etc. via the inline renderer chain). Kept
 * structurally identical: any change here MUST be mirrored in
 * `messageParser.detectToolError` and vice versa.
 */
function detectToolError(content?: string): string | null {
  if (!content) return null;
  const trimmed = content.trim();
  if (!trimmed) return null;
  if (/^(Failed|Error|ERR|Tool execution failed)/i.test(trimmed)) {
    return trimmed.length > 120 ? trimmed.slice(0, 117) + "…" : trimmed;
  }
  if (!trimmed.startsWith("{")) return null;
  try {
    const parsed = JSON.parse(trimmed);
    if (typeof parsed.error === "string" && parsed.error.trim().length > 0) {
      const e = parsed.error;
      return e.length > 120 ? e.slice(0, 117) + "…" : e;
    }
    if (
      typeof parsed.exit_code === "number" &&
      parsed.exit_code !== 0 &&
      typeof parsed.stderr === "string" &&
      parsed.stderr.trim().length > 0
    ) {
      const e = parsed.stderr;
      return e.length > 120 ? e.slice(0, 117) + "…" : e;
    }
    if (typeof parsed.exit_code === "number" && parsed.exit_code !== 0) {
      return `exit code ${parsed.exit_code}`;
    }
    return null;
  } catch {
    return null;
  }
}

/**
 * Heuristic completion check for browser tools whose results are
 * never followed by a `tool_complete` event in the timeline
 * stream. Added 2026-06-18 after the "browser_vision / browser_navigate
 * 永久显示运行中" regression (see MEMORY.md §"image-placeholder
 * residue"). The Rust backend `BrowserVisionHandler` and
 * `BrowserGetImagesHandler` emit a JSON envelope with a `status`
 * field that the frontend can read. If we see one of those statuses
 * inside an unfinished group, mark it done so the badge doesn't
 * stick on "执行中".
 */
function isBrowserCompletionMarker(tool: string, content?: string): boolean {
  if (!content) return false;
  const t = tool.toLowerCase();
  if (!(t.includes("browser") || t.includes("vision") || t.includes("get_images"))) {
    return false;
  }
  const trimmed = content.trim();
  if (!trimmed.startsWith("{")) return false;
  try {
    const parsed = JSON.parse(trimmed);
    if (typeof parsed.status === "string") {
      const s = parsed.status.toLowerCase();
      // Backend-known success statuses
      if (
        s === "vision_analysis" ||
        s === "image_list" ||
        s === "navigated" ||
        s === "snapshot_taken" ||
        s === "clicked" ||
        s === "typed" ||
        s === "scrolled" ||
        s === "console_cleared" ||
        s === "console_read" ||
        s === "ok"
      ) {
        return true;
      }
    }
  } catch {
    return false;
  }
  return false;
}

/**
 * Group events into one row per tool invocation.
 *
 * Rules:
 *  - `status` events don't get their own card. We attach the first one
 *    to the most recent running tool group as `statusNote`, then drop
 *    the rest. The previous behavior of rendering "正在处理 …" as a
 *    separate card *between* running and done rows is what made the
 *    timeline look chaotic: each tool call became 3 stacked cards.
 *  - `tool_start` opens a fresh group (keyed by tool name + start time
 *    so the same tool called twice doesn't merge into one).
 *  - `tool_stdout` / `tool_stderr` chunks are accumulated into the
 *    active group for the LiveTerminal body.
 *  - `tool_complete` closes the active group — the status flips to
 *    `"error"` when the close payload carries an error signal, else
 *    `"done"`. The previous code always flipped to `"done"`, which
 *    painted a green "成功" pill over failed terminal exits on reload.
 *  - Groups that started but never received a `tool_complete` are
 *    promoted to `"error"` with a "执行已中断" note — these are the
 *    rows that previously stuck on "运行中" forever after a session
 *    reload (the live WS is gone, the process is gone, the user only
 *    sees a stale "执行中" badge and a stray "terminal 运行中" sub-row).
 *  - `file_change` is metadata, not a tool run — dropped (rendered
 *    separately by `FileChangeBlock` in MessageBubble).
 */
function groupEvents(events: ExecutionTimelineEvent[]): ToolGroup[] {
  const groups: ToolGroup[] = [];
  const active = new Map<string, number>(); // tool name → group index

  for (const ev of events) {
    if (ev.type === "file_change") continue;

    if (ev.type === "status") {
      const note = (ev.content ?? "").trim();
      if (!note) continue;
      // Attach to the most recent running group (if any). Otherwise
      // discard — orphan status notes ("正在思考", "初始化完成", etc.)
      // were the source of the "extra card" noise.
      const lastRunning = [...active.entries()].find(([_, idx]) => {
        const g = groups[idx];
        return g && g.status === "running";
      });
      if (lastRunning) {
        const g = groups[lastRunning[1]];
        if (g && !g.statusNote) g.statusNote = note;
      }
      continue;
    }

    const tool = ev.tool?.trim() || "tool";
    let idx = active.get(tool);
    const fresh = ev.type === "tool_start" || idx === undefined;

    if (fresh) {
      const grp: ToolGroup = {
        key: `${tool}-${groups.length}-${ev.created_at ?? ""}`,
        tool,
        status: ev.type === "tool_complete" ? "done" : "running",
        events: [],
        startedAt: ev.created_at,
        args: ev.arguments,
      };
      groups.push(grp);
      idx = groups.length - 1;
      if (ev.type !== "tool_complete") active.set(tool, idx);
    }

    const safeIdx = idx ?? groups.length - 1;
    const grp = groups[safeIdx];
    grp.events.push(ev);
    if (ev.type === "tool_start" && !grp.args) grp.args = ev.arguments;
    if (ev.type === "tool_complete") {
      const errReason = detectToolError(ev.content);
      grp.status = errReason ? "error" : "done";
      grp.endedAt = ev.created_at;
      if (errReason && !grp.statusNote) grp.statusNote = errReason;
      active.delete(tool);
      continue;
    }
    // Some browser tools (vision, get_images) emit a JSON status
    // envelope as their last streamed chunk and never follow it with
    // a `tool_complete` event. Without this branch the group stays
    // green-and-spinning forever and the user sees a permanent
    // "执行中" badge. See isBrowserCompletionMarker above for context.
    if (grp.status === "running" && isBrowserCompletionMarker(tool, ev.content)) {
      grp.status = "done";
      grp.endedAt = ev.created_at;
      active.delete(tool);
    }
  }
  // Stranded pass — same idea as the inline `groupTimeline`. Any
  // group that opened but never received a close event is read as
  // "interrupted" on reload, not "still running". Otherwise the
  // badge would stay green-and-spinning forever for any tool that
  // was in-flight when the session was last closed.
  for (const idx of active.values()) {
    const grp = groups[idx];
    if (grp && grp.status === "running") {
      grp.status = "error";
      if (!grp.statusNote) grp.statusNote = "执行已中断（无完成事件）";
    }
  }
  return groups;
}

function durationOf(group: ToolGroup): string {
  if (!group.startedAt || !group.endedAt) return "";
  const t0 = new Date(group.startedAt).getTime();
  const t1 = new Date(group.endedAt).getTime();
  if (Number.isNaN(t0) || Number.isNaN(t1)) return "";
  return formatDuration(Math.max(0, t1 - t0));
}

function iconFor(tool: string) {
  const t = tool.toLowerCase();
  if (t.includes("terminal") || t.includes("shell") || t.includes("exec") || t.includes("bash")) return Terminal;
  if (t.includes("browser") || t.includes("web") || t.includes("http") || t.includes("fetch")) return Globe;
  return Wrench;
}

/**
 * Tool "kind" — drives the row's `data-tool-kind` attribute, which
 * the CSS uses to decide whether to render the shell-style `$`
 * prefix on the primary arg. Only `shell` gets the `$` so file
 * paths and URLs don't get a misleading prompt symbol.
 *
 *   terminal / bash / exec / execute_command → "shell"
 *   search_files / search / grep             → "search"
 *   browser / fetch / http                   → "url"
 *   read_file / write_file / edit / patch /
 *     delete_file / list_files               → "file"
 *   everything else                          → "tool"
 */
function toolKind(tool: string): "shell" | "search" | "url" | "file" | "tool" {
  const t = tool.toLowerCase();
  if (t.includes("terminal") || t.includes("shell") || t.includes("bash") || t.includes("exec") || t.includes("execute_command")) return "shell";
  if (t.includes("search") || t.includes("grep")) return "search";
  if (t.includes("browser") || t.includes("web") || t.includes("http") || t.includes("fetch")) return "url";
  if (t.includes("read_file") || t.includes("write_file") || t.includes("patch") || t.includes("edit") || t.includes("delete_file") || t.includes("list_files")) return "file";
  return "tool";
}

/** Friendly Chinese verb for the tool name — mirrors `toolVerb` in
 *  MessageBubble.tsx so both renderers read identically. */
const TOOL_VERB: Record<string, string> = {
  read_file: "已读取",
  write_file: "已写入",
  edit: "已编辑",
  edit_file: "已编辑",
  patch: "已编辑",
  delete_file: "已删除",
  search_files: "已搜索",
  list_files: "已查看",
  terminal: "已执行命令",
  execute_command: "已执行命令",
  bash: "已执行命令",
  exec: "已执行命令",
  browser: "已获取网页",
  web_fetch: "已获取网页",
  fetch_url: "已获取网页",
  computer_use: "已操作桌面",
  code: "已生成代码",
  create: "已创建",
  file_change: "已变更",
};

function toolVerb(tool: string): string {
  const t = tool.toLowerCase();
  for (const [key, verb] of Object.entries(TOOL_VERB)) {
    if (t.includes(key)) return verb;
  }
  return `已使用 ${tool}`;
}

/**
 * Primary argument extracted for the row's secondary line. Mirrors
 * `toolPrimaryArg` in MessageBubble.tsx so the ACUI timeline and the
 * inline `role: "tool"` card read identically. Order:
 *
 *   1. Tool-kind-specific keys (path / command / url / query / …)
 *   2. Stdout-derived hint for tools that don't carry their input in
 *      `args` (e.g. python-pptx wrappers that stash the real command
 *      in stdout as JSON)
 *   3. First non-empty string-valued arg
 */
function primaryArg(tool: string, argsJson?: string, stdoutHint?: string): string | null {
  let parsed: Record<string, unknown> | null = null;
  if (argsJson) {
    try {
      const v = JSON.parse(argsJson);
      if (v && typeof v === "object" && !Array.isArray(v)) parsed = v as Record<string, unknown>;
    } catch {}
  }
  const str = (k: string): string | null => {
    if (!parsed) return null;
    const v = parsed[k];
    return typeof v === "string" && v.length > 0 ? v : null;
  };
  const trim = (s: string, n: number) => s.length > n ? s.slice(0, n - 1) + "…" : s;
  const t = tool.toLowerCase();
  if (t.includes("read_file") || t.includes("write_file") || t.includes("patch") || t.includes("edit") || t.includes("delete_file") || t.includes("list_files")) {
    return trim(str("path") ?? str("file_path") ?? str("directory") ?? "", 80) || stdoutFallback(t, stdoutHint);
  }
  if (t.includes("terminal") || t.includes("bash") || t.includes("exec") || t.includes("command")) {
    return trim(str("command") ?? str("cmd") ?? str("script") ?? "", 80) || stdoutFallback(t, stdoutHint);
  }
  if (t.includes("python") || t.includes("py_run") || t.includes("run_python")) {
    const code = str("code") ?? str("script") ?? str("command");
    if (code) {
      const firstLine = code.split(/\r?\n/).map((l) => l.trim()).find((l) => l.length > 0) ?? "";
      return trim(firstLine, 80) || null;
    }
    return stdoutFallback(t, stdoutHint);
  }
  if (t.includes("search_files") || t.includes("search")) {
    const p = str("path") ?? str("directory");
    const q = str("query") ?? str("pattern") ?? str("q");
    if (p && q) return `${trim(p, 40)}  ·  "${trim(q, 30)}"`;
    return trim(p ?? q ?? "", 80) || null;
  }
  if (t.includes("browser") || t.includes("fetch") || t.includes("web") || t.includes("http")) {
    return trim(str("url") ?? str("uri") ?? "", 80) || null;
  }
  if (parsed) {
    for (const v of Object.values(parsed)) {
      if (typeof v === "string" && v.length > 0) return trim(v, 60);
    }
  }
  return stdoutFallback(t, stdoutHint);
}

/**
 * See MessageBubble.tsx::stdoutFallback. Duplicated here rather than
 * imported to keep the ACUI timeline renderer zero-dep on the
 * MessageBubble module (which pulls in MarkdownBody, Prism, …).
 */
function stdoutFallback(_toolLower: string, stdout?: string): string | null {
  if (!stdout) return null;
  const trimmed = stdout.trim();
  if (!trimmed) return null;
  const okArrow = trimmed.match(/^OK\s*->\s*(\S+)/);
  if (okArrow && okArrow[1]) return okArrow[1];
  const verbArrow = trimmed.match(/^(?:Wrote|Saved|Created|Generated|Written)\s+(?:to\s+)?[`"']?(\S+?)[`"']?\s*(?:\||$)/i);
  if (verbArrow && verbArrow[1]) return verbArrow[1];
  const firstLine = trimmed.split(/\r?\n/).map((l) => l.trim()).find((l) => l.length > 0);
  if (!firstLine) return null;
  return firstLine.length > 80 ? firstLine.slice(0, 79) + "…" : firstLine;
}

/** Unwrap common JSON wrappers (`{"result":"…","exit_code":0}`) that
 *  some tools wrap their output in. Mirrors `unwrapToolOutput` in
 *  MessageBubble.tsx. */
function unwrapToolOutput(raw: string): string {
  if (!raw) return raw;
  const trimmed = raw.trim();
  if (!trimmed.startsWith("{")) return raw;
  try {
    const parsed = JSON.parse(trimmed);
    if (typeof parsed.result === "string") return parsed.result;
    if (typeof parsed.error === "string") return parsed.error;
    if (typeof parsed.output === "string") return parsed.output;
    return raw;
  } catch {
    return raw;
  }
}

/**
 * Coalesce adjacent groups that share the same tool name AND status.
 *
 * Without this, an LLM that retries a failing tool (e.g. browser_navigate
 * that can't reach CDP) produces N near-identical cards in a row — visually
 * noisy and a strong signal the agent is stuck in a loop. We collapse them
 * into a single "tool ×N" row whose body is the concatenation of the
 * individual groups' events. Running groups are never coalesced (they
 * belong to the present).
 */
function coalesceAdjacent(groups: ToolGroup[]): ToolGroup[] {
  if (groups.length <= 1) return groups;
  const out: ToolGroup[] = [];
  for (const g of groups) {
    const last = out[out.length - 1];
    if (
      last
      && g.status === "done"            // only collapse finished groups
      && last.status === "done"
      && last.tool === g.tool            // same tool name
      && last.status === g.status        // same outcome (both done / both error)
      && !last.events.some((e) => (e.type as string) === "tool_stdout" || (e.type as string) === "tool_stderr")
      && !g.events.some((e) => (e.type as string) === "tool_stdout" || (e.type as string) === "tool_stderr")
    ) {
      // Merge into `last` — concatenate events, mark as a duplicate run.
      const wasN = (last.events.length > 0 && (last as ToolGroup & { _runCount?: number })._runCount) || 1;
      last.events = [...last.events, ...g.events];
      (last as ToolGroup & { _runCount?: number })._runCount = wasN + 1;
    } else {
      out.push(g);
    }
  }
  return out;
}

/** Compact, WorkBuddy-style one-row-per-tool card. Mirrors the visual
 *  treatment of `ToolMessageCard` in MessageBubble.tsx so the ACUI
 *  timeline and the inline `role: "tool"` cards look identical. */
export function ToolEventCard({ events }: ToolEventCardProps) {
  const grouped = useMemo(() => groupEvents(events), [events]);
  const groups = useMemo(() => coalesceAdjacent(grouped), [grouped]);
  if (groups.length === 0) return null;

  return (
    <div className="space-y-1.5">
      {groups.map((g) => (
        <ToolRow key={g.key} group={g} />
      ))}
    </div>
  );
}

function ToolRow({ group }: { group: ToolGroup }) {
  const Icon = iconFor(group.tool);
  const isRunning = group.status === "running";
  const isError = group.status === "error";
  // Default collapsed. Previously auto-opened while running so the user
  // could watch stdout stream in, but this dumped the mac-style window
  // chrome (3 dots + "输出" label + JSON dump) into the timeline for
  // every fired tool, making the rail feel like a "毛坯房". Now we
  // keep the body tucked away until the user explicitly clicks the
  // row. Running tools still show a spinner + accent-tinted icon to
  // signal "actively working".
  const [open, setOpen] = useState(false);
  const label = group.tool && group.tool.trim() ? group.tool : toolVerb(group.tool);

  // Concatenate all stdout/stderr chunks into one terminal block.
  // Declared BEFORE `primary` so the stdout-aware fallback in
  // `primaryArg` (e.g. python-pptx wrappers that stash the real
  // command in stdout as JSON) sees the latest accumulated bytes.
  const stdout = useMemo(
    () =>
      group.events
        .filter((ev) => ev.type === "tool_stdout")
        .map((ev) => ev.content ?? "")
        .join(""),
    [group.events],
  );
  const primary = primaryArg(group.tool, group.args, stdout);
  const dur = durationOf(group);
  // The `complete` event sometimes carries a final summary in
  // `content` even when no stdout was streamed — surface it inside
  // the expanded body so the user can see what the tool reported.
  const summary = useMemo(() => {
    const ev = group.events.find((e) => e.type === "tool_complete");
    return ev?.content ?? "";
  }, [group.events]);
  const hasBody = stdout.length > 0 || (summary && summary !== stdout);

  // Inline result peek — first meaningful line, truncated to ~60 chars.
  // Shown as `| peek` after the tool name when the card is collapsed.
  const peek = useMemo(() => {
    if (!hasBody) return "";
    const src = stdout || summary;
    // Take the first non-empty line that isn't pure JSON wrapper noise.
    const lines = src.split("\n").map(l => l.trim()).filter(Boolean);
    const first = lines[0] ?? "";
    // Collapse JSON blobs to a short token so the peek stays readable.
    const collaped = first.length > 80
      ? first.slice(0, 76) + "..."
      : first;
    return collaped;
  }, [stdout, summary, hasBody]);

  return (
    <div
      className={`tw-msg h-enter ${
        isRunning ? "tw-msg--run" : isError ? "tw-msg--err" : "tw-msg--ok"
      }`}
    >
      <button
        type="button"
        onClick={() => hasBody && setOpen((v) => !v)}
        className="tw-msg-row"
        data-tool-kind={toolKind(group.tool)}
        title={primary ?? label}
      >
        {/* Left cluster: icon + tool name + primary + peek */}
        <span className="tw-msg-main">
          <span className="tw-msg-icon">
            {isRunning ? (
              <Loader2 size={12} className="tw-msg-spin" />
            ) : (
              <Icon size={12} className="tw-msg-icon-svg" />
            )}
          </span>

          <span className="tw-msg-verb">{label}</span>
          {/* ×N badge: shown when coalesceAdjacent merged N identical
              tool calls into one row. Helps the user spot LLM retry loops
              at a glance — "3 个失败" + a single peek is louder than 3
              identical cards. */}
          {((group as ToolGroup & { _runCount?: number })._runCount ?? 1) > 1 && (
            <span className="tw-msg-xn">
              ×{(group as ToolGroup & { _runCount?: number })._runCount}
            </span>
          )}
          {primary && <span className="tw-msg-primary">{primary}</span>}

          {/* Inline separator + result peek (visible when collapsed) */}
          {hasBody && (
            <>
              <span className="tw-msg-sep">|</span>
              <span className="tw-msg-peek">{peek}</span>
            </>
          )}
        </span>

        {/* Right cluster: duration + badge + chevron */}
        <span className="tw-msg-right">
          {dur && <span className="tw-msg-dur">{dur}</span>}
          {isRunning && !group.endedAt ? (
            <span className="tw-msg-badge tw-msg-run-badge">
              <Loader2 size={9} className="animate-spin" />
              运行中
            </span>
          ) : isError ? (
            <span className="tw-msg-badge tw-msg-err-badge">
              <XIcon size={9} />
              {group.statusNote?.startsWith("执行已中断") ? "已中断" : "失败"}
            </span>
          ) : (
            <span className="tw-msg-badge tw-msg-done-badge">
              {/* 2026-06-18 主人反馈: 之前用绿色 ✓ + "成功"
                  会贴在所有正常完成的工具上, 不管执行结果是否
                  真正成功 (e.g. exit code != 0 也报 "成功")。
                  改为蓝色填充小圆 + "已完成" — 中性"工具执行完毕",
                  不再断言 success。真正的 success/failure 状态由
                  结果内容决定 (e.g. exit code 1, 错误文本),
                  不与本徽章视觉重叠。 */}
              <Circle size={8} fill="currentColor" strokeWidth={0} />
              已完成
            </span>
          )}
          {/* Chevron — suppressed for error tools because the
              error message is already visible in the statusNote
              on the summary row; expanding just repeats the same
              content (and the red body would look like a
              duplicate). */}
          {hasBody && !isError && (
            <span className="tw-msg-chev">
              {open ? <ChevronDown size={11} /> : <ChevronRight size={11} />}
            </span>
          )}
        </span>
      </button>

      {/* Expandable output body — suppressed for error tools:
          same rationale as the chevron above. The error is
          already shown in the statusNote/peek on the summary
          line; expanding to a red body is just noise. */}
      {hasBody && open && !isError && (
        <div className="tw-msg-body">
          <LiveTerminal
            content={stdout || summary}
            // Live streaming is gated on *both* the group's status
            // AND a missing close timestamp. `isRunning` alone is
            // not enough: a reloaded session always has groups in
            // `"running"` state for any tool that was in-flight
            // when the live WS died — the user shouldn't see the
            // "等待输出…" empty hint for those.
            running={isRunning && !group.endedAt}
          />
        </div>
      )}
    </div>
  );
}

/**
 * Collapsible result panel — same shape as MessageBubble's
 * LiveTerminal. No "运行中 / 输出 / 已中断" header, no green dot, no
 * line-number gutter. The parent tool row's icon + status badge
 * already carries the run state, so a duplicate indicator inside
 * the body is noise. The row's `peek` already shows the first line
 * of output, so the user has context before they click the chevron.
 *
 * Empty state stays subtle: "等待输出…" while the run is live,
 * "(无输出)" once it's done. A hover-only copy chip at the top-right
 * keeps the affordance without a permanent header row.
 */
function LiveTerminal({ content, running }: { content: string; running: boolean }) {
  const displayContent = useMemo(() => unwrapToolOutput(content), [content]);
  // Copy-to-clipboard state. We hold a "copied" flag for ~1.4s
  // after a successful copy so the button can flip its icon +
  // tint green — the visual equivalent of "saved".
  const [copied, setCopied] = useState(false);
  const copyTimerRef = useRef<number | null>(null);

  // Cancel any pending "copied" reset on unmount so we don't
  // call setState on a dead component.
  useEffect(() => () => {
    if (copyTimerRef.current) window.clearTimeout(copyTimerRef.current);
  }, []);

  const handleCopy = async () => {
    const text = displayContent || (running ? "" : "(无输出)");
    if (!text) return;
    try {
      if (navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(text);
      } else {
        // Tauri webview fallback: a hidden textarea + execCommand
        // — keeps the affordance working even when the modern
        // clipboard API is unavailable (e.g. http context).
        const ta = document.createElement("textarea");
        ta.value = text;
        ta.style.position = "fixed";
        ta.style.left = "-9999px";
        document.body.appendChild(ta);
        ta.select();
        document.execCommand("copy");
        document.body.removeChild(ta);
      }
      setCopied(true);
      if (copyTimerRef.current) window.clearTimeout(copyTimerRef.current);
      copyTimerRef.current = window.setTimeout(() => setCopied(false), 1400);
    } catch {
      // Silent failure — the user can still select the text manually.
    }
  };

  const hasContent = displayContent.length > 0;

  return (
    <div
      className="tw-msg-output"
      data-running={running ? "true" : "false"}
      data-empty={hasContent ? "false" : "true"}
    >
      <button
        type="button"
        className="tw-msg-output-copy"
        onClick={(e) => { e.stopPropagation(); handleCopy(); }}
        data-copied={copied ? "true" : "false"}
        title={copied ? "已复制" : "复制输出"}
        aria-label={copied ? "已复制" : "复制输出"}
      >
        {copied ? <CheckCheck size={10} /> : <Copy size={10} />}
      </button>
      {hasContent ? (
        <pre className="tw-msg-output-text">{displayContent}</pre>
      ) : (
        <span className="tw-msg-output-empty">
          {running ? "等待输出…" : "(无输出)"}
        </span>
      )}
    </div>
  );
}
