import React from "react";
import {
  ChevronDown, ChevronRight, ListTodo, Terminal, Globe, Wrench,
  AlertCircle, AlertTriangle, CheckCircle2, Circle, Loader2, Copy, Check,
  ShieldCheck, ShieldAlert, HelpCircle, X, Sparkles, Brain,
  FileText, FilePlus, ArrowRight,
  CheckCheck, Search, FolderOpen, Edit3, Trash2, Eye,
  FileSearch, Database, GitBranch,
  Hammer, Bot,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { MarkdownBody } from "./MarkdownBody";
import type {
  ChatMessage,
  ClarifyMessageMeta,
} from "../types";
import { formatRelativeTime } from "../utils/time";
import {
  parseContent, groupTimeline, extractFileChanges,
  type FileChangeEntry,
} from "./messageParser";
import { Avatar } from "./Avatar";
// InlinePathPreview was retired 2026-06-22 — every path click now
// routes through ChatView's `onPathClick` → RightSidebar preview.
// See the comment near the markdown-body render below for the bug
// story ("产物栏展开跑到对话界面下面去了").
import { ThoughtStreamCard } from "./ThoughtStreamCard";

// Re-export so `import { ThoughtStreamCard } from "./MessageBubble"` keeps
// working — consistent with `PrefetchIntentChip`'s public surface below.
export { ThoughtStreamCard };

interface MessageBubbleProps {
  message: ChatMessage;
  /** Workspace root used to anchor relative paths the LLM mentions. */
  workspaceRoot?: string | null;
  /**
   * Set true only when this is the actively-streaming assistant message.
   * Gates the "等待回复" (waiting for final reply) placeholder so it
   * only fires on the live turn — never on historical/reloaded turns
   * that happen to have empty body + completed tools. See
   * `AssistantMessage` for the full reasoning.
   */
  isStreaming?: boolean;
  onApproveApproval?: (requestId: string) => void;
  onDenyApproval?: (requestId: string) => void;
  onAnswerClarify?: (requestId: string, answer: string) => void;
  /** Click handler for absolute/relative paths mentioned in tool
   *  rows and the file-change list.  Used to pop the inline file
   *  preview.  Only meaningful on top-level `role: "tool"` messages
   *  and assistant replies. */
  onPathClick?: (path: string, e: React.MouseEvent) => void;
  /**
   * Whether this assistant message should render the avatar + name +
   * metadata header. We show the header on the FIRST assistant
   * message in a turn (the one immediately after a user/tool input)
   * and hide it on continuation messages within the same turn. The
   * body content, reasoning block, and tool timeline keep rendering
   * either way — only the header chrome is suppressed. This avoids
   * the "avatar / name printed N times for one user question" feel
   * and matches the WorkBuddy pattern of one identification per turn.
   *
   * Defaults to `true` so non-ChatView callers (e.g. tests, isolated
   * previews) get the full visual treatment; ChatView explicitly
   * passes `false` for continuation assistant messages.
   */
  showHeader?: boolean;
  /**
   * PLAN_UI_ENHANCEMENT §1.1 — 专家切换分界时间戳（毫秒）。
   * 当用户切换专家后，本字段作为分水岭：
   *   - 历史消息 (timestamp < cutoffAt) 沿用旧规则（Hermix 内置头像/原始时间）
   *   - 新消息 (timestamp >= cutoffAt) 按新专家的规则渲染
   * 缺省表示本会话从未切换过专家，所有消息走默认路径。
   */
  expertCutoffAt?: number | null;
}

export function MessageBubble({
  message, workspaceRoot,
  onApproveApproval, onDenyApproval, onAnswerClarify,
  onPathClick,
  showHeader = true,
  isStreaming = false,
  expertCutoffAt,
}: MessageBubbleProps) {
  if (message.role === "user") return <UserMessage message={message} />;
  if (message.role === "system") return <SystemMessage message={message} />;
  if (message.role === "approval") {
    return (
      <ApprovalCard
        message={message}
        workspaceRoot={workspaceRoot}
        onApprove={onApproveApproval}
        onDeny={onDenyApproval}
      />
    );
  }
  if (message.role === "clarify") {
    return (
      <ClarifyCard
        message={message}
        onAnswer={onAnswerClarify}
      />
    );
  }
  if (message.role === "tool") {
    return (
      <ToolMessageCard
        message={message}
        workspaceRoot={workspaceRoot}
        onPathClick={onPathClick}
      />
    );
  }
  return (
    <AssistantMessage
      message={message}
      workspaceRoot={workspaceRoot}
      showHeader={showHeader}
      isStreaming={isStreaming}
      expertCutoffAt={expertCutoffAt}
    />
  );
}

// ── Stop-reason chip ─────────────────────────────────────────────
// 2026-06-17 主人反馈 "已完成几个字的设计难看":
//   stop 态不渲染任何 chip (已完成是废话, 时间戳已经在左侧). 其他 stop
//   原因保留, 但全部去掉 pill-* 彩色文本 → 改为纯 muted 文字 + 极小图标,
//   视觉重量与左侧时间戳持平.
function StopReasonChip({ reason, meta }: { reason: string; meta?: string }) {
  // stop → render nothing (the message has clearly settled; no label needed)
  if (reason === "stop") return null;

  const map: Record<string, { label: string; cls: string }> = {
    length:      { label: "已截断",         cls: "text-text-warning" },
    interrupted: { label: "已中断",         cls: "text-text-faint italic" },
    tool_calls:  { label: "工具调用中",      cls: "text-text-faint italic" },
    error:       { label: "出错了",         cls: meta === "stream-error" || meta === "non-error" ? "text-text-faint italic" : "text-text-error" },
    timeout:     { label: "超时",           cls: "text-text-warning italic" },
    slow:        { label: "响应较慢",        cls: "text-text-warning" },
    partial:     { label: "未完成",         cls: "text-text-faint italic" },
  };
  const info = map[reason] ?? { label: reason, cls: "text-text-faint italic" };
  return (
    <span className={`h-tiny ${info.cls}`}>{info.label}</span>
  );
}

// ── H3: Prefetch intent chip (shows H2 prefetch result) ──

interface PrefetchIntentChipProps {
  intent: string;
  confidence: number;
}

export function PrefetchIntentChip({ intent, confidence }: PrefetchIntentChipProps) {
  const pct = Math.round(confidence * 100);
  return (
    <div className="prefetch-chip h-enter" title={`意图预判：${intent}`}>
      <div className="prefetch-dot" />
      <Sparkles size={9} className="text-success" />
      <span className="prefetch-label">预取</span>
      <span className="prefetch-label" style={{ color: "var(--color-text-primary)" }}>{intent}</span>
      <span className="prefetch-confidence">{pct}%</span>
    </div>
  );
}

// ── User message ─────────────────────────────────────────────────
function UserMessage({ message }: { message: ChatMessage }) {
  return (
    <div className="flex justify-end h-enter items-start">
      <div className="msg-bubble-user whitespace-pre-wrap break-words">
        {message.content}
      </div>
    </div>
  );
}

// ── Assistant message ────────────────────────────────────────────
function AssistantMessage({
  message, workspaceRoot: _workspaceRoot, showHeader = true, onPathClick, isStreaming, expertCutoffAt: _expertCutoffAt,
}: {
  message: ChatMessage;
  workspaceRoot?: string | null;
  onPathClick?: (path: string, e: React.MouseEvent) => void;
  /**
   * Whether to render the avatar + name + time + model + stop chip +
   * copy button. We hide these on continuation messages within the
   * same turn so a single user question → N assistant outputs (with
   * interleaved tool calls) reads as one cohesive response, not as
   * a chatty multi-avatar thread. The body content is unchanged.
   */
  showHeader?: boolean;
  /**
   * Whether THIS message is the live streaming message. Only the
   * currently-streaming message is allowed to show the "等待回复"
   * ("waiting for reply") placeholder — historical/reloaded messages
   * never display it, even if their tool list happens to be fully
   * done with empty body. Without this guard, every historical
   * assistant turn in a long conversation that called tools would
   * render a "等待回复..." line below it.
   */
  isStreaming?: boolean;
  /**
   * PLAN_UI_ENHANCEMENT §1.1 — 专家切换分界时间戳（毫秒）。
   * 当本消息的 timestamp < cutoffAt 时，认为是切换前历史消息。
   * 渲染层可以用它来决定用 Hermix 内置头像 vs 专家头像，
   * 以及保留原始时间 vs 按专家时间轴显示。
   */
  expertCutoffAt?: number | null;
}) {
  const parsed = useMemo(() => parseContent(message.content), [message.content]);
  const timeline = useMemo(
    () => (message.execution_timeline ? groupTimeline(message.execution_timeline) : []),
    [message.execution_timeline],
  );
  const fileChanges = useMemo(
    () => extractFileChanges(message.execution_timeline),
    [message.execution_timeline],
  );
  const toolCalls = message.tool_calls ?? [];
  /** True when there's at least one tool still in "running" state */
  const hasRunningTool = useMemo(
    () => toolCalls.some((tc) => tc.status === "running" || !tc.status),
    [toolCalls],
  );
  /** True when there are any tools at all (running or done) */
  const hasTools = useMemo(
    () => toolCalls.length > 0,
    [toolCalls],
  );
  const [copied, setCopied] = useState(false);

  // Inline path preview was retired 2026-06-22 in favour of routing
  // every path click through ChatView's `onPathClick` → RightSidebar.
  // The dialog form (and the `transform: ...; h-enter` ancestor it
  // spawned under) used to "fall below" the chat column, breaking
  // the "产物栏/阅读界面在同一栏" mental model the user expects.
  // Now path clicks always open / switch the right sidebar's preview.

  const copyMessage = async () => {
    await navigator.clipboard.writeText(message.content);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  };

  return (
    // Outer wrapper: with header we use the avatar+column flex; without
    // header we keep the left margin (so body text aligns with the first
    // assistant message) but skip the avatar column and the header row.
    <div className={showHeader ? "flex gap-2.5 h-enter items-start" : "h-enter"}>
      {showHeader && (
        <Avatar role="assistant" size={22} className="shrink-0 mt-0.5" />
      )}
      <div className={showHeader ? "flex-1 min-w-0" : "pl-[30px]"}>
        {/* Header: time · model · stop chip · copy.
            Hidden on continuation assistant messages so a single
            user turn reads as one cohesive response, not as a
            chatty multi-avatar thread.
            The "Hermix" name is intentionally NOT shown — the avatar
            is the model's identity. The meta line (time / model / stop
            chip) lives alongside, separated by ·  dots. */}
        {showHeader && (
          <div className="flex items-center gap-1.5 mb-1 text-text-faint">
            <span className="h-tiny">{formatRelativeTime(message.timestamp)}</span>
            {message.model && <span className="h-tiny">· {message.model}</span>}
            {message.stopReason && <StopReasonChip reason={message.stopReason} meta={message.stopReasonMeta} />}
            <button
              onClick={copyMessage}
              className="ml-auto p-0.5 rounded text-text-faint hover:text-text-primary transition-colors"
              title="复制消息"
            >
              {copied ? <Check size={11} /> : <Copy size={11} />}
            </button>
          </div>
        )}

        {/* Reasoning content is merged inline as body text — no separate
            collapsible block. Strip <think> tags so raw text flows naturally
            with the visible reply. */}
        {message.reasoningContent && message.reasoningContent.trim().length > 0 && (
          <div className="reasoning-inline">
            {message.reasoningContent.replace(/<\/?think>/gi, "").trim()}
          </div>
        )}

        {/* The message card: channel stack */}
        <div className="msg-card msg-stack">
          {/* Tasks */}
          {parsed.tasks.length > 0 && (
            <div className="msg-channel" data-channel="tasks">
              <TaskListBlock tasks={parsed.tasks} />
            </div>
          )}

          {/* Main body — file paths route to the right sidebar's
              preview mode (the "阅读界面" in the same column the user
              expects), NOT to an in-place dialog. Previously this
              passed `openPath` which mounted an absolute-positioned
              InlinePathPreview over the viewport. The popover used
              to "fall below" the chat column because the parent
              `<div className="h-enter">` carried a `transform` from
              the h-fade-up animation, silently promoting itself to
              the containing block for `position: fixed` children. */}
          {parsed.body && (
            <div className="msg-channel" data-channel="body">
              <MarkdownBody
                content={parsed.body}
                onPathClick={onPathClick ?? (() => {})}
              />
            </div>
          )}

          {/* File changes summary (WorkBuddy-style compact row) */}
          {fileChanges.length > 0 && (
            <div className="msg-channel" data-channel="file-changes">
              <FileChangeBlock
                changes={fileChanges}
                onPathClick={onPathClick}
              />
            </div>
          )}

          {/* Tool calls are rendered exclusively as inline
              `role: "tool"` messages (created by `onToolStart` /
              `expandTimelineToMessages`). The streaming assistant's
              `execution_timeline` no longer carries tool_* events —
              the rail was the source of the "一件事三行" duplicate
              (tool card + status note + tool card). Fall through to
              the legacy `tool_calls` fallback for any pre-existing
              data that hasn't been migrated.

              主人原话 (2026-06-22)："我看到状态改了，但是我的要求是
              不显示这个！" — 在 reload 路径下，assistant 消息的
              `content` 已经包含了工具结果，再渲染一遍 tool 名 +
              "完成" 状态完全是冗余。所以**所有 tool call 都已经
              完成时，整个 fallback 行直接隐藏**；只有还有
              running 中的工具时才显示，让用户知道 agent 还在干活。 */}
          {timeline.length === 0 && toolCalls.length > 0 && hasRunningTool && (
            <div className="msg-channel" data-channel="tools">
              <ToolCallsFallback calls={toolCalls} />
            </div>
          )}

          {/* Gap thinking animation: only shown when tools are running
              but we have NOT yet rendered any tool cards for this message.
              Once tool cards appear (each with their own "运行中" / "成功"
              status badge), this ThinkingDots becomes redundant visual noise
              and must be suppressed to avoid the repetitive pattern:

                🔧 terminal  运行中
                正在处理 ...      ← BAD: duplicates the badge info

              In practice `onToolStart` inserts a tool card immediately,
              so this branch rarely fires during streaming — it exists as
              a safety net for edge cases where the tool metadata arrives
              before the card is mounted. */}
          {hasRunningTool && !hasTools && (
            <div className="msg-channel" data-channel="thinking">
              <ThinkingDots label="正在处理" />
            </div>
          )}
          {/* All tools done, body still empty → waiting for final text.
              Only fire for the LIVE streaming message. Reloaded history
              never shows this, even when tools are all done — the user
              is not actually waiting on anything anymore. */}
          {hasTools && !hasRunningTool && !parsed.body && parsed.tasks.length === 0 && isStreaming && (
            <div className="msg-channel" data-channel="thinking">
              <ThinkingDots label="等待回复" />
            </div>
          )}
        </div>

        {/* Backend badge (outside card so it reads as meta, not part of answer) */}
        {message.execution_backend && (
          <div className="mt-2 px-1 h-tiny text-text-faint">
            {message.execution_backend === "sandbox" ? "沙箱执行" : "本地执行"}
          </div>
        )}
      </div>

      {/* Inline path popover retired 2026-06-22 — see comment above
          `openPath` in this component. All path clicks now flow
          through `onPathClick` (passed by ChatView → App.tsx) into
          the right sidebar's preview mode. */}
    </div>
  );
}

// ── System message ───────────────────────────────────────────────
//
// Defense-in-depth: this branch is no longer reachable through the
// normal `getSessionMessages` path (server + api.ts both filter out
// role='system'). It still exists because (a) the `role: "system"`
// type is still part of the ChatMessage union (e.g. the agent
// designer flow in AgentsView synthesizes one in-memory), and
// (b) if a future call site forgets to filter, we want a 200-char
// preview banner — not a 23k-char raw prompt flooding the chat.
function SystemMessage({ message }: { message: ChatMessage }) {
  const full = message.content ?? "";
  const PREVIEW = 200;
  const preview = full.length > PREVIEW ? full.slice(0, PREVIEW) + "…" : full;
  return (
    <div className="flex gap-2 items-center justify-center my-2 h-enter">
      <AlertCircle size={12} className="text-text-faint" />
      <span className="h-tiny italic text-text-faint">{preview}</span>
    </div>
  );
}

// ── Thinking block ───────────────────────────────────────────────

// ── File change block ─────────────────────────────────────────────
// Flat list of file links. No pills, no icons, no per-file colour
// coding — the user said "文件名是蓝色" and "字号与'文件变更'一致".
// The block just renders the title row + a comma-separated list of
// clickable filenames, each opening the inline path preview.

function FileChangeBlock({
  changes,
  onPathClick,
}: {
  changes: FileChangeEntry[];
  onPathClick?: (path: string, e: React.MouseEvent) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const VISIBLE = 4;
  const visible = expanded ? changes : changes.slice(0, VISIBLE);
  const hidden = Math.max(0, changes.length - VISIBLE);
  return (
    <div className="file-change-block">
      <div className="file-change-header">
        <FileText size={12} className="shrink-0" />
        <span>文件变更</span>
        <span className="file-change-count">（{changes.length}个）</span>
      </div>
      <ul className="file-change-list">
        {visible.map((c, i) => {
          const filename = c.path.split(/[\\/]/).pop() ?? c.path;
          return (
            <li key={`${c.path}-${i}`}>
              <button
                type="button"
                className="file-change-link"
                title={c.path}
                onClick={(e) => onPathClick?.(c.path, e)}
              >
                {filename}
              </button>
            </li>
          );
        })}
      </ul>
      {!expanded && hidden > 0 && (
        <button
          type="button"
          className="file-pill-toggle"
          onClick={() => setExpanded(true)}
        >
          查看全部变更
          <ArrowRight size={11} />
        </button>
      )}
      {expanded && hidden > 0 && (
        <button
          type="button"
          className="file-pill-toggle"
          onClick={() => setExpanded(false)}
        >
          收起
          <ChevronDown size={11} className="rotate-180" />
        </button>
      )}
    </div>
  );
}

// ── Task list ────────────────────────────────────────────────────
function TaskListBlock({ tasks }: { tasks: Array<{ done: boolean; text: string }> }) {
  const done = tasks.filter((t) => t.done).length;
  const total = tasks.length;
  const pct = total === 0 ? 0 : Math.round((done / total) * 100);
  return (
    <div className="tasks-block">
      <div className="tasks-progress">
        <div className="tasks-progress-bar" style={{ width: `${pct}%` }} />
      </div>
      <div className="tasks-header">
        <ListTodo size={12} />
        <span>任务列表</span>
        <span className="tasks-header-count">{done}/{total} · {pct}%</span>
      </div>
      <div className="tasks-list">
        {tasks.map((t, i) => (
          <div key={i} className={`task-row ${t.done ? "done" : ""}`}>
            {t.done ? (
              <CheckCircle2 size={15} className="text-success mt-0.5 shrink-0" />
            ) : (
              <Circle size={15} className="text-text-faint mt-0.5 shrink-0" />
            )}
            <span className="task-text">{t.text}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

// ── Body markdown ────────────────────────────────────────────────
//
// Body rendering is shared between MessageBubble and StreamingBubble
// so a fenced code block in a streaming reply renders with the
// language label + copy button + oneDark highlight the moment the
// closing ``` arrives. See ./MarkdownBody.tsx for the renderer.

// ── Tool message card (role="tool") ──────────────────────────────
//
// Style goals (matching WorkBuddy's tool listing):
//   - Compact: one row per tool, no avatar or card wrapper
//   - Collapsible: click the row to expand the terminal block
//   - Minimal chrome: just the tool icon + name + status pill + time
//   - Terminal output lives inside the expanded body, muted background

/* ════════════════════════════════════════════════════════════════════
   工具分类体系 v4 — 全面覆盖，零"未知操作"
   ════════════════════════════════════════════════════════════════════
   Each category has:
     • A list of keyword patterns matched against the lower-cased tool name
     • A Chinese verb displayed as the action label
     • A lucide-react icon component
     • A solid background / "tile" colour for the icon tile
     • A `kind` string for CSS data-* attributes

   Resolution order:
     1. The first category whose keywords include a substring of the tool
        name wins — we always select the most-specific match first.
     2. Empty / placeholder ("tool") → infer from args shape.
     3. No match at all → "other" category (NEVER "unknown").

   The "other" category catches EVERY tool name that wasn't explicitly
   mapped. It renders the literal tool name as the verb so the user
   always sees what actually ran — no generic placeholder.

   Last updated: 2026-06-22 (v4 comprehensive rewrite)
   ─────────────────────────────────────────────────────────────── */

interface ToolCategory {
  keywords: string[];
  verb: string;
  bg: string;
  kind: "shell" | "read" | "write" | "edit" | "delete" | "search-web" | "search-file" | "list" | "fetch" | "parse" | "image" | "code" | "git" | "build" | "test" | "install" | "deploy" | "db" | "desktop" | "reason" | "ask" | "clipboard" | "network" | "other";
}

const TOOL_CATEGORIES: ToolCategory[] = [
  // 1. 终端/命令执行 (绿色)
  {
    keywords: ["terminal", "bash", "shell", "exec", "execute_command", "run_command", "run_script", "cmd", "powershell", "pwsh"],
    verb: "执行命令",
    bg: "#22c55e",
    kind: "shell",
  },
  // 2. 文件读取 (蓝色)
  {
    keywords: ["read_file", "read_pdf", "read_xls", "read_xlsx", "read_docx", "read_doc", "read_pptx", "read_image", "view_file", "cat", "open_file", "preview", "inspect"],
    verb: "读取文件",
    bg: "#3b82f6",
    kind: "read",
  },
  // 3. 文件写入/创建 (蓝色)
  {
    keywords: ["write_file", "create_file", "save_file", "add_file", "new_file", "output_file", "append_file"],
    verb: "写入文件",
    bg: "#3b82f6",
    kind: "write",
  },
  // 4. 文件编辑/修改 (蓝色)
  {
    keywords: ["edit_file", "edit", "patch", "update_file", "modify_file", "rewrite", "replace", "change_file"],
    verb: "编辑文件",
    bg: "#3b82f6",
    kind: "edit",
  },
  // 5. 删除文件 (红色)
  {
    keywords: ["delete_file", "remove_file", "unlink", "rm_file"],
    verb: "删除文件",
    bg: "#ef4444",
    kind: "delete",
  },
  // 6. 操作文件(通用) (蓝色)
  {
    keywords: ["file_operation", "file_op", "move_file", "rename_file", "copy_file"],
    verb: "操作文件",
    bg: "#3b82f6",
    kind: "edit",
  },
  // 7. 网络搜索 (紫色)
  {
    keywords: ["search_web", "web_search", "google", "bing", "duckduckgo", "search_internet", "search_query"],
    verb: "搜索网络",
    bg: "#a855f7",
    kind: "search-web",
  },
  // 8. 文件搜索 (紫色)
  {
    keywords: ["search_files", "file_search", "file_query", "grep", "find_file", "locate", "search", "search_directory"],
    verb: "搜索文件",
    bg: "#a855f7",
    kind: "search-file",
  },
  // 9. 列表目录 (蓝色)
  {
    keywords: ["list_files", "list_dir", "list_directory", "ls", "dir", "tree", "walk", "readdir", "glob"],
    verb: "查看目录",
    bg: "#3b82f6",
    kind: "list",
  },
  // 10. 网页访问 (青色)
  {
    keywords: ["browser", "web_fetch", "fetch_url", "fetch_page", "http_get", "http_request", "curl", "wget", "download", "webpage"],
    verb: "获取网页",
    bg: "#06b6d4",
    kind: "fetch",
  },
  // 11. 网络工具 (青色)
  {
    keywords: ["network", "ping", "dns", "socket", "traceroute", "nslookup"],
    verb: "网络诊断",
    bg: "#06b6d4",
    kind: "network",
  },
  // 12. PDF 解析 (蓝色)
  {
    keywords: ["parse_pdf", "extract_pdf", "pdf_to_text", "pdf_parse", "read_pdf"],
    verb: "解析 PDF",
    bg: "#3b82f6",
    kind: "parse",
  },
  // 13. 图片处理 (绿色)
  {
    keywords: ["image_edit", "edit_image", "resize_image", "convert_image", "process_image", "image_process"],
    verb: "处理图片",
    bg: "#22c55e",
    kind: "image",
  },
  // 14. 代码生成 (橙色)
  {
    keywords: ["code", "generate_code", "create_code", "write_code", "implement"],
    verb: "生成代码",
    bg: "#f97316",
    kind: "code",
  },
  // 15. 运行脚本/代码 (绿色)
  {
    keywords: ["python", "py_run", "run_python", "node_run", "run_node", "run_js", "run_script", "script"],
    verb: "运行代码",
    bg: "#22c55e",
    kind: "shell",
  },
  // 16. Git 操作 (橙色)
  {
    keywords: ["git", "commit", "push", "pull", "branch", "merge", "clone", "checkout", "diff", "log", "status", "stash", "rebase", "fetch"],
    verb: "Git 操作",
    bg: "#f97316",
    kind: "git",
  },
  // 17. 构建/编译 (橙色)
  {
    keywords: ["build", "compile", "cargo", "npm_run", "pnpm_run", "yarn_run", "make", "cmake", "gradle", "mvn", "webpack", "vite", "tsc"],
    verb: "构建项目",
    bg: "#f97316",
    kind: "build",
  },
  // 18. 测试运行 (绿色)
  {
    keywords: ["test", "pytest", "jest", "vitest", "mocha", "spec", "run_test", "unit_test"],
    verb: "运行测试",
    bg: "#22c55e",
    kind: "test",
  },
  // 19. 安装依赖 (绿色)
  {
    keywords: ["install", "add_package", "add_dep", "npm_install", "pnpm_add", "pip_install", "cargo_add"],
    verb: "安装依赖",
    bg: "#22c55e",
    kind: "install",
  },
  // 20. 部署发布 (橙色)
  {
    keywords: ["deploy", "publish", "release", "upload", "ship", "rollout"],
    verb: "部署发布",
    bg: "#f97316",
    kind: "deploy",
  },
  // 21. 数据库操作 (青色)
  {
    keywords: ["sql", "database", "db", "sqlite", "postgres", "mongo", "select", "insert_query", "update_db", "query_db", "db_query"],
    verb: "操作数据库",
    bg: "#06b6d4",
    kind: "db",
  },
  // 22. 操作桌面 (灰色)
  {
    keywords: ["computer_use", "desktop", "screenshot", "mouse", "keyboard", "gui"],
    verb: "操作桌面",
    bg: "#6b7280",
    kind: "desktop",
  },
  // 23. 思考/推理 (紫色)
  {
    keywords: ["think", "reason", "reflect", "analyze", "plan_task", "plan"],
    verb: "正在思考",
    bg: "#a855f7",
    kind: "reason",
  },
  // 24. 询问用户 (黄色)
  {
    keywords: ["ask_user", "ask", "question", "confirm", "clarify"],
    verb: "询问用户",
    bg: "#eab308",
    kind: "ask",
  },
  // 25. 剪贴板 (灰色)
  {
    keywords: ["clipboard", "paste", "copy_to_clipboard", "read_clipboard"],
    verb: "读取剪贴板",
    bg: "#6b7280",
    kind: "clipboard",
  },
  // 26. 格式化/检查 (灰色)
  {
    keywords: ["lint", "format", "prettier", "eslint", "check", "validate", "audit"],
    verb: "代码检查",
    bg: "#6b7280",
    kind: "other",
  },
  // 27. 工具/通用 (灰色 — 最后兜底)
  {
    keywords: [],  // empty array = catch-all, never matches by keyword
    verb: "调用工具",
    bg: "#6b7280",
    kind: "other",
  },
];

function classifyTool(tool: string): ToolCategory {
  const t = tool.toLowerCase().trim();
  if (!t || t === "tool") return TOOL_CATEGORIES[TOOL_CATEGORIES.length - 1]; // catch-all
  for (const cat of TOOL_CATEGORIES) {
    if (cat.keywords.length === 0) continue; // skip catch-all in normal loop
    for (const kw of cat.keywords) {
      if (t.includes(kw)) return cat;
    }
  }
  return TOOL_CATEGORIES[TOOL_CATEGORIES.length - 1]; // catch-all
}

/**
 * Infer the tool verb from args shape when the tool name is a
 * placeholder ("tool"). This handles the common LLM pattern where
 * `tool="tool"` but carries real args in the payload.
 */
function inferVerbFromArgs(argsJson?: string): string | null {
  if (!argsJson) return null;
  try {
    const parsed = JSON.parse(argsJson);
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      if ("command" in parsed || "cmd" in parsed || "script" in parsed) return "执行命令";
      if ("path" in parsed || "file_path" in parsed || "directory" in parsed || "file" in parsed) return "操作文件";
      if ("url" in parsed || "uri" in parsed) return "获取网页";
      if ("query" in parsed || "pattern" in parsed || "q" in parsed) return "搜索";
      if ("code" in parsed) return "执行代码";
    }
  } catch {}
  return null;
}

/**
 * Chinese action phrase for a tool name.
 *
 * Resolution order:
 *   1. Look up tool name in TOOL_CATEGORIES → return mapped verb
 *   2. Placeholder "tool" → infer from args shape
 *   3. Empty / placeholder + empty args → tool name itself (no "未知操作")
 */
function toolVerb(tool: string, argsJson?: string): string {
  const t = tool.toLowerCase().trim();
  if (!t || t === "tool") {
    return inferVerbFromArgs(argsJson) ?? "调用工具";
  }
  for (const cat of TOOL_CATEGORIES) {
    if (cat.keywords.length === 0) continue;
    for (const kw of cat.keywords) {
      if (t.includes(kw)) return cat.verb;
    }
  }
  // Last resort — return the literal tool name
  return tool;
}

/**
 * Extract a human-readable one-line result summary from the tool's
 * output. Everything is a user-visible success/error summary — never
 * raw JSON or wrapper noise.
 *
 * Returns null when no meaningful result can be extracted (the row
 * simply doesn't show a result peek).
 */
function toolResultPeek(output?: string, argsJson?: string): string | null {
  if (!output) return null;

  // Peel JSON wrapper
  let text = output;
  if (text.trimStart().startsWith("{")) {
    try {
      const parsed = JSON.parse(text);
      if (parsed && typeof parsed === "object") {
        // Error field first (tool errored)
        if (typeof parsed.error === "string" && parsed.error.length > 0) {
          return parsed.error.length > 120 ? parsed.error.slice(0, 117) + "…" : parsed.error;
        }
        // Stderr with content (actual execution failure)
        if (typeof parsed.stderr === "string" && parsed.stderr.trim().length > 0) {
          const first = parsed.stderr.trim().split(/\r?\n/)[0];
          return first.length > 120 ? first.slice(0, 117) + "…" : first;
        }
        // Stdout / result
        const inner = (typeof parsed.stdout === "string" && parsed.stdout) ||
                      (typeof parsed.result === "string" && parsed.result) ||
                      (typeof parsed.output === "string" && parsed.output) ||
                      (typeof parsed.message === "string" && parsed.message) ||
                      "";
        if (inner) {
          text = inner;
          // Recurse in case the inner text is still wrapped
          const recursed = toolResultPeek(text, argsJson);
          if (recursed) return recursed;
          return null;
        }
        // Exit code != 0 without stderr
        if (typeof parsed.exit_code === "number" && parsed.exit_code !== 0) {
          return `退出码 ${parsed.exit_code}`;
        }
        // Success with no stdout
        if (typeof parsed.exit_code === "number" && parsed.exit_code === 0) {
          const std = typeof parsed.stdout === "string" ? parsed.stdout.trim() : "";
          if (std && std.length > 0) {
            text = std;
          } else {
            return null; // no output, nothing to show
          }
        }
      }
    } catch {}
  }

  const trimmed = text.trim();
  if (!trimmed) return null;

  // "OK -> D:\path\to\file.pptx | size: 33596 bytes | slides: 3"
  const okArrow = trimmed.match(/^OK\s*->\s*(\S+)/);
  if (okArrow) {
    // Also try to extract "slides: N" or "size: N" suffix
    const detail = trimmed.match(/\|\s*(slides|pages|size):\s*(\d+)/i);
    if (detail) {
      return detail[1].toLowerCase() === "slides" || detail[1].toLowerCase() === "pages"
        ? `已生成 (${detail[2]} ${detail[1] === "slides" ? "页" : "页"})`
        : `已生成 (${detail[2]} ${detail[1].toLowerCase()})`;
    }
    return "已完成";
  }

  // "Wrote 823 bytes to D:\data\发票汇总.csv"
  const wroteBytes = trimmed.match(/^(?:Wrote|Written|Saved|Created|Generated)\s+(\d+)\s+bytes?\s+(?:to\s+)?/i);
  if (wroteBytes) return `已写入 ${wroteBytes[1]} bytes`;

  // "Saved D:\path\to\file"  (no byte count)
  const saved = trimmed.match(/^(?:Saved|Wrote|Created|Generated)\s+(?:to\s+)?[`"']?(\S+)/i);
  if (saved) {
    const path = saved[1];
    const filename = path.split(/[\\/]/).pop() ?? path;
    return `已保存 ${filename}`;
  }

  // "saved: D:\path\to\file.xlsx 5632 bytes"
  const savedColon = trimmed.match(/^saved:\s*(\S+)\s+(\d+)\s+bytes/i);
  if (savedColon) return `已写入 ${savedColon[2]} bytes`;

  // First non-empty line, capped at 100 chars
  const firstLine = trimmed.split(/\r?\n/).map((l) => l.trim()).find((l) => l.length > 0);
  if (!firstLine) return null;
  // Don't show raw JSON as result
  if (firstLine.startsWith("{") || firstLine.startsWith("[")) return null;

  return firstLine.length > 100 ? firstLine.slice(0, 97) + "…" : firstLine;
}

/**
 * Primary argument extracted for the row's secondary line. Order:
 *   1. Tool-kind-specific keys (path / command / url / query / …)
 *   2. Stdout-derived hint for tools that don't carry their input in
 *      `args` (e.g. python-pptx / py-runner wrappers that stash the
 *      real command in stdout as JSON)
 *   3. First non-empty string-valued arg
 *
 * `stdoutHint` is the tool's accumulated output. When `args` is empty
 * or has no recognisable command field (the python-pptx case in the
 * "毕业纪念.pptx" screenshot, where args was `{}` and stdout began
 * with `OK -> D:\data\毕业纪念.pptx`), we pull a friendly hint from
 * stdout so the row doesn't read as "已执行命令" + blank.
 */
function toolPrimaryArg(tool: string, argsJson?: string, stdoutHint?: string): string | null {
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
  // Path-based tools
  if (t.includes("read_file") || t.includes("write_file") || t.includes("patch") || t.includes("edit") || t.includes("delete_file") || t.includes("list_files")) {
    return trim(str("path") ?? str("file_path") ?? str("directory") ?? "", 80) || stdoutFallback(t, stdoutHint);
  }
  if (t.includes("terminal") || t.includes("bash") || t.includes("exec") || t.includes("command")) {
    return trim(str("command") ?? str("cmd") ?? str("script") ?? "", 80) || stdoutFallback(t, stdoutHint);
  }
  if (t.includes("python") || t.includes("py_run") || t.includes("run_python")) {
    // Python-style tools usually carry the script under `code` /
    // `script`; if not, the stdout often begins with the interpreter
    // banner or a one-line result.
    const code = str("code") ?? str("script") ?? str("command");
    if (code) {
      // Don't dump a 50-line script — show its first non-empty line.
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
  if (t.includes("computer_use")) {
    return trim(str("action") ?? "", 40) || null;
  }
  // 字面 `tool` 是 LLM 的占位符——args 实际可能是任何东西。
  // 按 args 形状推断是什么工具，按"command → 终端；path → 文件路径；
  // query → 搜索"等分支走，让"调用工具 + (空白)"这种无法动作的
  // 行彻底消失。
  if (t === "tool" && parsed) {
    const cmd = str("command") ?? str("cmd") ?? str("script");
    if (cmd) return trim(cmd, 80);
    const path = str("path") ?? str("file_path") ?? str("directory") ?? str("file");
    if (path) return trim(path, 80);
    const q = str("query") ?? str("pattern") ?? str("q");
    if (q) return trim(q, 60);
    const url = str("url") ?? str("uri");
    if (url) return trim(url, 80);
    const code = str("code");
    if (code) return trim(code.split(/\r?\n/)[0] ?? "", 80) || null;
  }
  // Fallback: first non-empty string value
  if (parsed) {
    for (const v of Object.values(parsed)) {
      if (typeof v === "string" && v.length > 0) return trim(v, 60);
    }
  }
  return stdoutFallback(t, stdoutHint);
}

/**
 * Try to lift a one-line hint from the tool's accumulated output when
 * the args payload is opaque (no recognisable command / path / url
 * field). We recognise two common wrapper patterns:
 *
 *   python-pptx style:  `OK -> D:\path\to\file.pptx | size: 33596 bytes | slides: 3`
 *   json tool wrapper:  `{"result":"…","exit_code":0}`  (already unwrapped earlier)
 *   generic tail:       first 80 chars of the first non-empty stdout line
 *
 * Returns null if stdout is empty or starts with no recognisable signal —
 * the caller falls back to a tool-kind-agnostic label.
 */
function stdoutFallback(toolLower: string, stdout?: string): string | null {
  if (!stdout) return null;
  // First, peel off common JSON wrappers the backend uses for tool
  // results (`{"exit_code":0,"interpreter":"python","language":"python",
  //  "stderr":"","stdout":"saved: D:\\qcc\\发票汇总.xls 5632 bytes\r\n"}`).
  // Without this, the row reads as "已执行 {"exit_code":0,...}" —
  // pure JSON noise that the user cannot act on. We pull the
  // human-readable `stdout` (or `result`) out and recurse so
  // `OK -> path` / `Saved path` patterns still match.
  if (stdout.trimStart().startsWith("{")) {
    try {
      const parsed = JSON.parse(stdout);
      if (parsed && typeof parsed === "object") {
        const inner =
          (typeof parsed.stdout === "string" && parsed.stdout) ||
          (typeof parsed.result === "string" && parsed.result) ||
          (typeof parsed.output === "string" && parsed.output) ||
          (typeof parsed.message === "string" && parsed.message) ||
          "";
        if (inner) {
          const recursed = stdoutFallback(toolLower, inner);
          if (recursed) return recursed;
        }
      }
    } catch {
      // not JSON, fall through
    }
  }
  const trimmed = stdout.trim();
  if (!trimmed) return null;
  // python-pptx / generic "OK ->" preamble
  const okArrow = trimmed.match(/^OK\s*->\s*(\S+)/);
  if (okArrow && okArrow[1]) return okArrow[1];
  // Wrote / Saved / Created / Generated <filepath>
  const verbArrow = trimmed.match(/^(?:Wrote|Saved|Created|Generated|Written)\s+(?:to\s+)?[`"']?(\S+?)[`"']?\s*(?:\||$)/i);
  if (verbArrow && verbArrow[1]) return verbArrow[1];
  // "saved: D:\data\发票汇总.xls 5632 bytes"  — the python-runner
  // wrapper used by the qcc/invoice-aggregation flow. Lift the path.
  const saved = trimmed.match(/^saved:\s*(\S+?\.xlsx?)\b/i);
  if (saved && saved[1]) return saved[1];
  // First non-empty line, capped at 80 chars. We deliberately don't
  // try to parse JSON here — `LiveTerminal` already unwraps the
  // {"result":"…"} wrapper for the body, so by the time we'd see it
  // it's either clean prose or the original JSON dump.
  const firstLine = trimmed.split(/\r?\n/).map((l) => l.trim()).find((l) => l.length > 0);
  if (!firstLine) return null;
  // Last-resort: if the first non-empty line is itself a JSON
  // object, don't surface the raw JSON to the user. Render a short
  // summary instead. This is the "out of unwraps" case.
  if (firstLine.startsWith("{") || firstLine.startsWith("[")) {
    // 第一行还是 JSON wrapper（peel 没解开）——返回 null 而不是
    // 占位"已执行"，否则会出现"调用工具 已执行"这种啥也没说
    // 的行。让 verb slot 单独显示，由 verb 自己处理 fallback。
    return null;
  }
  return firstLine.length > 80 ? firstLine.slice(0, 79) + "…" : firstLine;
}

/**
 * WorkBuddy-style tool row:
 *
 *   [icon] 已读取 <repo-parent>\...  ✅
 *   [icon] 已搜索 .tsx · "query"  ✅
 *   [icon] 已执行命令 cargo build  🔄
 *
 * Single compact line per tool, status badge on right.
 * Click to expand/collapse the terminal output body.
 */
/**
 * Extract a human-readable one-line summary from a tool error payload.
 * Handles common wrapper formats so the user never sees raw JSON like
 * `{"error":"Tool execution failed: Failed to connect to Chrome CDP..."}`
 * in the tool row.
 */
function summarizeError(output?: string): string | null {
  if (!output) return null;
  const trimmed = output.trim();
  // Already plain text (no JSON wrapper)
  if (!trimmed.startsWith("{")) return trimmed.length > 120 ? trimmed.slice(0, 117) + "…" : trimmed;
  try {
    const parsed = JSON.parse(trimmed);
    if (typeof parsed.error === "string") return parsed.error;
    if (typeof parsed.message === "string") return parsed.message;
    if (typeof parsed.stderr === "string") return parsed.stderr;
    // Nested wrapper: {"result":"...","exit_code":1} — not really an error shape
    if (typeof parsed.result === "string") return null;
    return null;
  } catch {
    return null;
  }
}

function ToolMessageCard({
  message,
  workspaceRoot: _workspaceRoot,
  onPathClick,
}: {
  message: ChatMessage;
  workspaceRoot?: string | null;
  onPathClick?: (path: string, e: React.MouseEvent) => void;
}) {
  const meta = message.tool;
  if (!meta) return null;
  const Icon = toolIcon(meta.tool);
  const isRunning = meta.status === "running";
  const isError = meta.status === "error";
  const [open, setOpen] = useState(false);
  const hasOutput = meta.output != null && meta.output.length > 0;
  // Two-piece label: `<verb> <object>` (e.g. "读取 PDF D:\data\发票汇总.xls").
  //   - `verb` comes from `toolVerb` (registered Chinese verb, or the
  //     literal English name for unknown tools).
  //   - `object` is `argsHint` — the most meaningful string we
  //     can lift from the tool's args (path / url / command / query).
  //     If `argsHint` is empty (e.g. a no-arg tool), we just show
  //     the verb alone so the row doesn't read "读取文件 (空)".
  //
  // 主人原话 (2026-06-22): "执行的内容不要写死啊, 应该是根据实际
  // 情况显示的" — verb 不再硬塞 "调用工具" 占位符。当 LLM 输出
  // `tool="tool"` 这种空占位时，根据 args 形状 (command/path/url/
  // query/code) 推断 verb; args 也空就显示 "未知操作"。
  const verb = toolVerb(meta.tool, meta.args);
  // The row has two visually distinct slots:
  //
  //   1. Verb slot (left):  always shows `verb` alone — e.g. "读取 PDF",
  //      "执行命令", "调用工具". No args, no object, nothing else.
  //   2. Primary slot (middle, optional): shows the most meaningful
  //      string we can lift from args / stdout — file path, URL,
  //      command, query, or — on error — a one-line error summary
  //      instead of the raw JSON payload.
  //
  // Earlier this code concatenated them into a single label string
  // and rendered that in the verb slot, then rendered `primary`
  // again in its own span — which produced visible duplicates like
  // "调用工具 D:\data\发票汇总.xls D:\data\发票汇总.xls" once `primary`
  // already had the path. Keep them as two slots and never merge.
  const primary = isError
    ? (summarizeError(meta.output) ?? toolPrimaryArg(meta.tool, meta.args, meta.output))
    : toolPrimaryArg(meta.tool, meta.args, meta.output);
  const stats = extractLineStats(meta.tool, meta.args);
  // Path-shaped primary arg is clickable.  We only auto-recognise
  // the standard file ops; everything else (e.g. python scripts,
  // arbitrary text) is left as plain text to avoid spurious "open
  // file" affordances.
  const isPathLike = isPathArg(meta.tool, meta.args, meta.output);
  const primaryPath = isPathLike ? (meta.output || meta.args) : null;
  const handlePrimaryClick = (e: React.MouseEvent) => {
    if (isPathLike && primaryPath && onPathClick) {
      const path = extractFirstPath(primaryPath);
      if (path) {
        e.stopPropagation();
        onPathClick(path, e);
        return;
      }
    }
    if (hasOutput) setOpen((v) => !v);
  };

  // Result peek — a one-line human-readable summary of what the
  // tool produced (or why it failed). Extracted from the output
  // in priority order: error message → success summary → first line.
  const resultPeek = useMemo(() => {
    if (isError) {
      return meta.errorReason
        ?? summarizeError(meta.output)
        ?? (meta.output ? toolResultPeek(meta.output, meta.args) : null)
        ?? "执行出错";
    }
    if (!meta.output || meta.output.trim().length === 0) return null;
    // For running tools, don't show a result peek (it's not done yet)
    if (isRunning && !meta.completedAt) return null;
    return toolResultPeek(meta.output, meta.args);
  }, [meta.output, meta.args, meta.status, meta.completedAt, meta.errorReason, isError, isRunning]);

  // Colored tile background for the icon
  const cat = classifyTool(meta.tool);
  const iconBg = cat.bg;

  return (
    <div className="tw-msg h-enter">
      <button
        type="button"
        onClick={hasOutput ? () => setOpen((v) => !v) : undefined}
        className="tw-msg-row"
        data-tool-kind={toolKind(meta.tool)}
        data-state={isError ? "err" : isRunning ? "run" : "ok"}
      >
        {/* Colored icon tile — matches the FileTypeIcon style:
            a small rounded square with a brand-colored background
            and a white lucide icon. */}
        <span
          className="tw-msg-icon-tile"
          style={{ background: iconBg }}
        >
          <Icon size={11} className="tw-msg-icon-svg" />
        </span>

        {/* Verb (e.g. "执行命令") — right next to the icon tile */}
        <span className="tw-msg-verb">{verb}</span>
        {primary && (
          <span
            className={isPathLike ? "tw-msg-primary tw-msg-primary--link" : "tw-msg-primary"}
            title={isPathLike ? `点击打开 ${primary}` : primary}
            onClick={isPathLike ? handlePrimaryClick : undefined}
          >
            {primary}
          </span>
        )}

        {/* Result peek — compact one-line summary between primary
            arg and the right-side status group. Only shown for
            completed tools with a meaningful result.
            Format:   | tail 不是内部或外部命令 (error - red)
                      | 已写入 823 bytes (success - muted)
                      | 找到 12 个结果 (search - muted) */}
        {resultPeek && !isError && (
          <span className="tw-msg-result-peek">{resultPeek}</span>
        )}

        {/* Right side: line stats + status indicator. */}
        <span className="tw-msg-right">
          {(stats.added != null || stats.removed != null) && (
            <span className="tw-msg-stats">
              {stats.added != null && stats.added > 0 && (
                <span className="tw-msg-stat tw-msg-stat--add">+{stats.added}</span>
              )}
              {stats.removed != null && stats.removed > 0 && (
                <span className="tw-msg-stat tw-msg-stat--del">−{stats.removed}</span>
              )}
            </span>
          )}
          {/* 2026-06-29 主人反馈: "已完成" / "失败" 用蓝/红色字符太刺眼,
              全部改成小图标 + 文字形式, 颜色统一调到 muted/success/warning
              (蓝/红色字符完全不渲染). 已完成 → ✓; 失败 → ⚠; 运行中 → ⏵;
              已中断 → ⚠. */}
          <span className={`tw-msg-state tw-msg-state--${isError ? "err" : isRunning ? "run" : "done"}`}>
            {isRunning && !meta.completedAt ? (
              <>
                <Loader2 size={11} className="animate-spin shrink-0" />
                运行中
              </>
            ) : isError ? (
              <>
                <AlertCircle size={11} className="shrink-0" />
                失败
              </>
            ) : isRunning ? (
              <>
                <AlertTriangle size={11} className="shrink-0" />
                已中断
              </>
            ) : (
              <>
                <Check size={11} className="shrink-0" />
                已完成
              </>
            )}
          </span>
          {hasOutput && (
            <span className="tw-msg-chev">{open ? <ChevronDown size={11} /> : <ChevronRight size={11} />}</span>
          )}
        </span>
      </button>

      {/* Error snippet — shown only when the primary text doesn't
          already contain the error summary. This prevents duplicate
          red text (primary shows "Binary file detected..." and the
          err-hint below shows the same string). */}
      {isError && resultPeek && primary !== resultPeek && (
        <div
          className="tw-msg-err-hint"
          onClick={() => setOpen(true)}
          title="点击展开完整输出"
        >
          <AlertCircle size={10} className="shrink-0" />
          <span className="truncate">{resultPeek}</span>
          <ChevronRight size={11} className="shrink-0 tw-msg-chev-err" />
        </div>
      )}

      {/* Expandable output body — suppressed for error tools.
          The error snippet above (tw-msg-err-hint) already shows
          what went wrong; expanding to the full red body just
          repeats the same content.

          For `cua_screenshot`, the image preview is rendered
          INLINE (always visible, no expand needed) — the user
          almost always wants to *see* the screenshot, not read
          the raw base64 payload. A second collapsible block
          under the image lets them peek at the JSON metadata if
          they care about coordinates / file size. */}
      {!isError && meta.tool === "cua_screenshot" && (
        <ScreenshotPreview output={meta.output} />
      )}
      {!isError && meta.tool === "cua_list_windows" && (
        <WindowListPreview output={meta.output} />
      )}
      {!isError && meta.tool === "cua_window_at_point" && (
        <WindowAtPointPreview output={meta.output} />
      )}
      {!isError && meta.tool === "cua_ui_tree" && (
        <UiTreePreview output={meta.output} />
      )}
      {!isError && meta.tool === "cua_ui_find" && (
        <UiFindPreview output={meta.output} />
      )}
      {!isError && meta.tool === "cua_ui_click" && (
        <UiClickPreview output={meta.output} />
      )}
      {!isError && meta.tool === "cua_ui_focus" && (
        <UiSingleNamePreview output={meta.output} verb="聚焦" />
      )}
      {!isError && meta.tool === "cua_ui_invoke" && (
        <UiSingleNamePreview output={meta.output} verb="触发" />
      )}
      {!isError && meta.tool === "cua_ocr_recognize" && (
        <OcrPreview output={meta.output} />
      )}
      {hasOutput && open && !isError && meta.tool !== "cua_screenshot" && meta.tool !== "cua_list_windows" && meta.tool !== "cua_window_at_point" && meta.tool !== "cua_ui_tree" && meta.tool !== "cua_ui_find" && meta.tool !== "cua_ui_click" && meta.tool !== "cua_ui_focus" && meta.tool !== "cua_ui_invoke" && meta.tool !== "cua_ocr_recognize" && (
        <div className="tw-msg-body">
          <LiveTerminal
            content={meta.output!}
            running={isRunning && !meta.completedAt}
          />
        </div>
      )}
    </div>
  );
}

/**
 * Inline screenshot preview for `cua_screenshot` tool results.
 *
 * The backend returns a JSON envelope of the form
 * `{ width, height, monitor, monitor_name, bytes, png_base64 }`
 * (see `hermix_cua::capture::ScreenshotResult` /
 * `commands_cua::cua_screenshot`). When `output` parses to that
 * shape we render an `<img>` with a `data:image/png;base64,…`
 * source. Falls back to nothing (and lets the parent's text
 * body take over) when the JSON doesn't match — e.g. the tool
 * returned an error payload, or the user is on an older build.
 *
 * Why we always render the image regardless of the row's
 * expanded state: the image IS the result. Forcing the user to
 * click to see what the screenshot looks like defeats the
 * point of the tool. Text-only tool rows keep their
 * click-to-expand behavior — only the screenshot path is
 * auto-shown.
 */
function ScreenshotPreview({ output }: { output?: string }) {
  const parsed = useMemo(() => {
    if (!output) return null;
    try {
      const v = JSON.parse(output);
      if (
        v && typeof v === "object" && !Array.isArray(v) &&
        typeof v.png_base64 === "string" && v.png_base64.length > 0
      ) {
        return v as {
          png_base64: string;
          width?: number;
          height?: number;
          bytes?: number;
          monitor_name?: string;
        };
      }
    } catch {
      // not JSON, or wrong shape — fall through
    }
    return null;
  }, [output]);
  if (!parsed) return null;
  const w = typeof parsed.width === "number" ? parsed.width : 0;
  const h = typeof parsed.height === "number" ? parsed.height : 0;
  const bytes = typeof parsed.bytes === "number" ? parsed.bytes : 0;
  const kb = (bytes / 1024).toFixed(0);
  const monitorLabel = parsed.monitor_name ? ` · ${parsed.monitor_name}` : "";
  return (
    <div className="mt-1.5 rounded-md overflow-hidden border border-border-primary/40 bg-bg-secondary/20">
      <img
        src={`data:image/png;base64,${parsed.png_base64}`}
        alt={w && h ? `桌面截图 ${w}×${h}` : "桌面截图"}
        className="w-full h-auto max-h-[480px] object-contain bg-black/40"
        draggable={false}
      />
      <div className="flex items-center justify-between gap-2 px-2 py-1 text-[11px] text-text-faint bg-bg-secondary/40 border-t border-border-primary/30">
        <span className="font-mono">{w}×{h}{monitorLabel}</span>
        <span>{kb} KB</span>
      </div>
    </div>
  );
}

/**
 * Inline preview for `cua_list_windows` tool results.
 *
 * Backend returns:
 *   { matched, truncated, windows: [{hwnd, title, class_name,
 *     process_id, is_visible, rect:{x,y,width,height}}, ...] }
 *
 * The list is the primary deliverable — like the screenshot, we
 * always render it inline (no expand-click required). The
 * truncated/matched count sits in the footer so the user can
 * tell at a glance whether the agent got back a full census
 * or hit the cap.
 */
function WindowListPreview({ output }: { output?: string }) {
  const parsed = useMemo(() => {
    if (!output) return null;
    try {
      const v = JSON.parse(output);
      if (
        v && typeof v === "object" && !Array.isArray(v) &&
        Array.isArray(v.windows)
      ) {
        return v as {
          matched: number;
          truncated: boolean;
          windows: Array<{
            hwnd: number;
            title: string;
            class_name: string;
            process_id: number;
            is_visible: boolean;
            rect: { x: number; y: number; width: number; height: number };
          }>;
        };
      }
    } catch {
      // not JSON, or wrong shape — fall through
    }
    return null;
  }, [output]);
  if (!parsed) return null;
  const { matched, truncated, windows } = parsed;
  return (
    <div className="mt-1.5 rounded-md overflow-hidden border border-border-primary/40 bg-bg-secondary/20">
      <div className="px-2 py-1 text-[11px] text-text-faint bg-bg-secondary/40 border-b border-border-primary/30 flex items-center justify-between">
        <span>顶层窗口</span>
        <span className="font-mono">
          {matched} 个{truncated ? "（已截断）" : ""}
        </span>
      </div>
      {windows.length === 0 ? (
        <div className="px-3 py-3 text-[12px] text-text-faint text-center italic">
          没有匹配的窗口
        </div>
      ) : (
        <ul className="divide-y divide-border-primary/30 max-h-72 overflow-auto">
          {windows.map((w, i) => (
            <li
              key={`${w.hwnd}-${i}`}
              className="px-2.5 py-1.5 flex items-center gap-2 text-[12px] hover:bg-bg-tertiary/40"
              data-visible={w.is_visible ? "true" : "false"}
            >
              {/* Visibility dot */}
              <span
                className={`shrink-0 w-1.5 h-1.5 rounded-full ${
                  w.is_visible ? "bg-success" : "bg-text-faint/50"
                }`}
                title={w.is_visible ? "可见" : "隐藏"}
              />
              {/* Title + class */}
              <div className="flex-1 min-w-0">
                <div
                  className="truncate text-text-primary"
                  title={w.title || "(无标题)"}
                >
                  {w.title || <span className="italic text-text-faint">（无标题）</span>}
                </div>
                <div className="flex items-center gap-1.5 text-[10.5px] text-text-faint mt-0.5">
                  <span className="font-mono">{w.class_name || "—"}</span>
                  <span>·</span>
                  <span className="font-mono">pid {w.process_id}</span>
                </div>
              </div>
              {/* HWND + rect */}
              <div className="shrink-0 text-right font-mono text-[10.5px] text-text-faint leading-tight">
                <div title="窗口句柄（可传给 cua_click_window）">
                  hwnd <span className="text-text-secondary">0x{w.hwnd.toString(16)}</span>
                </div>
                <div title="GetWindowRect">
                  {w.rect.width}×{w.rect.height}
                </div>
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/**
 * Inline preview for `cua_window_at_point` tool results.
 *
 * Backend returns:  { x, y, window: WindowInfo | null }
 *
 * The hit-window gets a compact two-line summary so the user can
 * see "the LLM asked about pixel (x,y) and learned it was on app
 * Z" without expanding the row.
 */
function WindowAtPointPreview({ output }: { output?: string }) {
  const parsed = useMemo(() => {
    if (!output) return null;
    try {
      const v = JSON.parse(output);
      if (
        v && typeof v === "object" && !Array.isArray(v) &&
        typeof v.x === "number" && typeof v.y === "number"
      ) {
        return v as {
          x: number;
          y: number;
          window: null | {
            hwnd: number;
            title: string;
            class_name: string;
            process_id: number;
            is_visible: boolean;
            rect: { x: number; y: number; width: number; height: number };
          };
        };
      }
    } catch {
      // not JSON, or wrong shape — fall through
    }
    return null;
  }, [output]);
  if (!parsed) return null;
  const { x, y, window: w } = parsed;
  return (
    <div className="mt-1.5 rounded-md overflow-hidden border border-border-primary/40 bg-bg-secondary/20">
      <div className="px-2.5 py-1.5 text-[12px] flex items-center gap-2">
        <span className="text-text-faint">点</span>
        <span className="font-mono text-text-secondary">({x}, {y})</span>
        <span className="text-text-faint">·</span>
        {w ? (
          <span className="truncate text-text-primary" title={w.title || w.class_name}>
            {w.title || <span className="italic text-text-faint">（无标题）</span>}
            <span className="ml-1.5 text-text-faint font-mono text-[10.5px]">
              {w.class_name} · pid {w.process_id}
            </span>
          </span>
        ) : (
          <span className="italic text-text-faint">没有窗口</span>
        )}
      </div>
    </div>
  );
}

/**
 * Inline tree preview for `cua_ui_tree` tool results.
 *
 * Backend returns:
 *   { max_depth, element_count, elements: [UiElement, ...] }
 *
 * Each UiElement carries `name`, `control_type`, `class_name`,
 * `automation_id`, `rect`, `depth`, `index`, etc. We render the
 * flat Vec<UiElement> as an indented tree using `depth` as the
 * nesting cue — the backend walks the control-view depth-first
 * so siblings are contiguous and `depth` jumps by exactly 1.
 *
 * Why this is always inline (no expand-click): the whole point
 * of `cua_ui_tree` is to let the user eyeball what UIA sees. A
 * collapsed pre block would defeat the purpose.
 */
function UiTreePreview({ output }: { output?: string }) {
  const parsed = useMemo(() => {
    if (!output) return null;
    try {
      const v = JSON.parse(output);
      if (
        v && typeof v === "object" && !Array.isArray(v) &&
        Array.isArray(v.elements)
      ) {
        return v as {
          max_depth: number;
          element_count: number;
          elements: Array<{
            name: string;
            class_name: string;
            automation_id: string;
            control_type: string;
            localized_control_type: string;
            rect: { x: number; y: number; width: number; height: number };
            is_enabled: boolean;
            is_offscreen: boolean;
            has_keyboard_focus: boolean;
            depth: number;
            index: number;
            process_id: number;
            framework_id: string;
          }>;
        };
      }
    } catch {
      // not JSON, fall through
    }
    return null;
  }, [output]);
  if (!parsed) return null;
  const { max_depth, elements, element_count } = parsed;
  return (
    <div className="mt-1.5 rounded-md overflow-hidden border border-border-primary/40 bg-bg-secondary/20">
      <div className="px-2 py-1 text-[11px] text-text-faint bg-bg-secondary/40 border-b border-border-primary/30 flex items-center justify-between">
        <span>UI 树（UIA control view）</span>
        <span className="font-mono">
          {element_count} 个 / depth ≤ {max_depth}
        </span>
      </div>
      {elements.length === 0 ? (
        <div className="px-3 py-3 text-[12px] text-text-faint text-center italic">
          空树
        </div>
      ) : (
        <ul className="divide-y divide-border-primary/30 max-h-80 overflow-auto font-mono text-[11.5px]">
          {elements.map((el) => {
            const indentPx = Math.min(el.depth, 8) * 12;
            const disabledCls = el.is_enabled ? "" : "text-text-faint italic";
            const focusMarker = el.has_keyboard_focus ? (
              <span className="ml-1.5 text-text-secondary" title="当前键盘焦点">●</span>
            ) : null;
            const offscreenMarker = el.is_offscreen ? (
              <span className="ml-1 text-text-faint/60" title="离屏">⊘</span>
            ) : null;
            const rect = el.rect;
            const rectLabel = rect.width > 0 || rect.height > 0
              ? `${rect.width}×${rect.height} @ (${rect.x},${rect.y})`
              : "—";
            return (
              <li
                key={`${el.index}-${el.depth}-${el.name}`}
                className={`px-2 py-1 hover:bg-bg-tertiary/40 ${disabledCls}`}
                style={{ paddingLeft: 8 + indentPx }}
              >
                <div className="flex items-baseline gap-1.5 truncate">
                  <span className="shrink-0 text-text-faint text-[10px]">
                    [{el.control_type}]
                  </span>
                  <span className="truncate text-text-primary" title={el.name || "(未命名)"}>
                    {el.name || <span className="italic text-text-faint">（未命名）</span>}
                    {focusMarker}
                    {offscreenMarker}
                  </span>
                </div>
                <div className="flex items-center gap-2 mt-0.5 text-[10px] text-text-faint">
                  {el.automation_id && (
                    <span title="automation_id">
                      id=<span className="text-text-secondary">{el.automation_id}</span>
                    </span>
                  )}
                  {el.class_name && (
                    <span title="class_name">{el.class_name}</span>
                  )}
                  {el.framework_id && (
                    <span title="framework_id">{el.framework_id}</span>
                  )}
                  <span className="ml-auto" title="BoundingRectangle">{rectLabel}</span>
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

/**
 * Inline flat-list preview for `cua_ui_find` tool results.
 *
 * Backend returns:
 *   { count, truncated, elements: [UiElement, ...] }
 *
 * `cua_ui_find` returns a flat list (search hit list, not a
 * tree) so we use the same row layout as `UiTreePreview` but
 * without the depth indent and with the `truncated` flag in
 * the footer.
 */
function UiFindPreview({ output }: { output?: string }) {
  const parsed = useMemo(() => {
    if (!output) return null;
    try {
      const v = JSON.parse(output);
      if (
        v && typeof v === "object" && !Array.isArray(v) &&
        Array.isArray(v.elements)
      ) {
        return v as {
          count: number;
          truncated: boolean;
          elements: Array<{
            name: string;
            class_name: string;
            automation_id: string;
            control_type: string;
            rect: { x: number; y: number; width: number; height: number };
            is_enabled: boolean;
            has_keyboard_focus: boolean;
            process_id: number;
          }>;
        };
      }
    } catch {
      // not JSON, fall through
    }
    return null;
  }, [output]);
  if (!parsed) return null;
  const { count, truncated, elements } = parsed;
  return (
    <div className="mt-1.5 rounded-md overflow-hidden border border-border-primary/40 bg-bg-secondary/20">
      <div className="px-2 py-1 text-[11px] text-text-faint bg-bg-secondary/40 border-b border-border-primary/30 flex items-center justify-between">
        <span>UIA 查找结果</span>
        <span className="font-mono">
          {count} 个{truncated ? "（已截断）" : ""}
        </span>
      </div>
      {elements.length === 0 ? (
        <div className="px-3 py-3 text-[12px] text-text-faint text-center italic">
          未找到匹配元素
        </div>
      ) : (
        <ul className="divide-y divide-border-primary/30 max-h-72 overflow-auto">
          {elements.map((el, i) => {
            const focusMarker = el.has_keyboard_focus ? "●" : "";
            const disabledCls = el.is_enabled ? "" : "text-text-faint italic";
            return (
              <li
                key={`${el.automation_id}-${i}-${el.rect.x}-${el.rect.y}`}
                className={`px-2.5 py-1.5 hover:bg-bg-tertiary/40 text-[12px] ${disabledCls}`}
              >
                <div className="flex items-baseline gap-1.5 truncate">
                  <span className="shrink-0 text-text-faint font-mono text-[10px]">
                    [{el.control_type}]
                  </span>
                  <span className="truncate text-text-primary" title={el.name || "(未命名)"}>
                    {el.name || <span className="italic text-text-faint">（未命名）</span>}
                    {focusMarker && (
                      <span className="ml-1 text-text-secondary" title="当前焦点">{focusMarker}</span>
                    )}
                  </span>
                </div>
                <div className="flex items-center gap-2 mt-0.5 text-[10.5px] text-text-faint font-mono">
                  {el.automation_id && <span>id={el.automation_id}</span>}
                  {el.class_name && <span>· {el.class_name}</span>}
                  {el.process_id > 0 && <span>· pid {el.process_id}</span>}
                  <span className="ml-auto">
                    {el.rect.width}×{el.rect.height} @ ({el.rect.x},{el.rect.y})
                  </span>
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

/**
 * Compact summary card for `cua_ui_click` tool results.
 *
 * Backend returns:  { name, clicked_at: {x, y} }
 *
 * The click point is the meat of the result — it's the exact
 * pixel the agent picked from the UIA clickable-point contract.
 * We render that prominently so the user can verify "yes the
 * agent clicked on the OK button at (840, 412)" without
 * expanding the raw JSON.
 */
function UiClickPreview({ output }: { output?: string }) {
  const parsed = useMemo(() => {
    if (!output) return null;
    try {
      const v = JSON.parse(output);
      if (
        v && typeof v === "object" && !Array.isArray(v) &&
        typeof v.name === "string" && v.clicked_at &&
        typeof v.clicked_at.x === "number" && typeof v.clicked_at.y === "number"
      ) {
        return v as { name: string; clicked_at: { x: number; y: number } };
      }
    } catch {
      // not JSON, fall through
    }
    return null;
  }, [output]);
  if (!parsed) return null;
  const { name, clicked_at } = parsed;
  return (
    <div className="mt-1.5 rounded-md overflow-hidden border border-border-primary/40 bg-bg-secondary/20">
      <div className="px-2.5 py-1.5 text-[12px] flex items-center gap-2">
        <span className="text-text-faint">点击</span>
        <span className="text-text-primary truncate" title={name}>{name}</span>
        <span className="text-text-faint">·</span>
        <span className="font-mono text-text-secondary">
          ({clicked_at.x}, {clicked_at.y})
        </span>
      </div>
    </div>
  );
}

/**
 * Compact one-liner for `cua_ui_focus` and `cua_ui_invoke`
 * tool results. Both return `{ name }` and nothing else
 * meaningful, so we collapse to a single row.
 */
function UiSingleNamePreview({ output, verb }: { output?: string; verb: string }) {
  const parsed = useMemo(() => {
    if (!output) return null;
    try {
      const v = JSON.parse(output);
      if (
        v && typeof v === "object" && !Array.isArray(v) &&
        typeof v.name === "string"
      ) {
        return v.name as string;
      }
    } catch {
      // not JSON, fall through
    }
    return null;
  }, [output]);
  if (parsed === null) return null;
  return (
    <div className="mt-1.5 rounded-md overflow-hidden border border-border-primary/40 bg-bg-secondary/20">
      <div className="px-2.5 py-1.5 text-[12px] flex items-center gap-2">
        <span className="text-text-faint">{verb}</span>
        <span className="text-text-primary truncate" title={parsed}>{parsed}</span>
      </div>
    </div>
  );
}

/**
 * Inline preview for `cua_ocr_recognize` tool results.
 *
 * Backend returns:
 *   { text, lines: [{text, words: [{text, rect}]}, ...],
 *     line_count, avg_confidence }
 *
 * The full text is the headline; per-line detail (word count +
 * the line's bounding rect reconstructed from word rects) sits
 * in a scrollable list so the user can pick out the exact
 * location of any phrase.
 */
function OcrPreview({ output }: { output?: string }) {
  const parsed = useMemo(() => {
    if (!output) return null;
    try {
      const v = JSON.parse(output);
      if (
        v && typeof v === "object" && !Array.isArray(v) &&
        typeof v.text === "string" && Array.isArray(v.lines)
      ) {
        return v as {
          text: string;
          lines: Array<{
            text: string;
            words: Array<{
              text: string;
              rect: { x: number; y: number; width: number; height: number };
            }>;
          }>;
          line_count: number;
          avg_confidence: number | null;
        };
      }
    } catch {
      // not JSON, fall through
    }
    return null;
  }, [output]);
  if (!parsed) return null;
  const { text, lines, line_count, avg_confidence } = parsed;
  const confLabel = avg_confidence !== null && avg_confidence !== undefined
    ? ` · 置信度 ${(avg_confidence * 100).toFixed(0)}%`
    : "";
  return (
    <div className="mt-1.5 rounded-md overflow-hidden border border-border-primary/40 bg-bg-secondary/20">
      <div className="px-2 py-1 text-[11px] text-text-faint bg-bg-secondary/40 border-b border-border-primary/30 flex items-center justify-between">
        <span>OCR 识别结果</span>
        <span className="font-mono">
          {line_count} 行{confLabel}
        </span>
      </div>
      {text.trim().length === 0 ? (
        <div className="px-3 py-3 text-[12px] text-text-faint text-center italic">
          未识别到文字
        </div>
      ) : (
        <>
          <pre className="px-2.5 py-1.5 text-[12px] font-mono text-text-primary whitespace-pre-wrap break-words max-h-40 overflow-auto border-b border-border-primary/30">
            {text}
          </pre>
          {lines.length > 0 && (
            <ul className="divide-y divide-border-primary/30 max-h-48 overflow-auto">
              {lines.map((line, i) => {
                // Reconstruct the line bounding rect from its words.
                let minX = Number.POSITIVE_INFINITY, minY = Number.POSITIVE_INFINITY;
                let maxR = 0, maxB = 0;
                for (const w of line.words) {
                  const r = w.rect;
                  if (r.x < minX) minX = r.x;
                  if (r.y < minY) minY = r.y;
                  if (r.x + r.width > maxR) maxR = r.x + r.width;
                  if (r.y + r.height > maxB) maxB = r.y + r.height;
                }
                const rectLabel =
                  line.words.length === 0
                    ? "—"
                    : `${maxR - minX}×${maxB - minY} @ (${minX},${minY})`;
                return (
                  <li
                    key={`ocr-line-${i}-${minX}-${minY}`}
                    className="px-2.5 py-1 text-[12px] hover:bg-bg-tertiary/40"
                  >
                    <div className="flex items-baseline gap-2">
                      <span className="shrink-0 text-text-faint font-mono text-[10px] w-6 text-right">
                        {i + 1}
                      </span>
                      <span className="flex-1 truncate text-text-primary" title={line.text}>
                        {line.text || <span className="italic text-text-faint">（空行）</span>}
                      </span>
                      <span className="shrink-0 font-mono text-[10px] text-text-faint">
                        {line.words.length}词 · {rectLabel}
                      </span>
                    </div>
                  </li>
                );
              })}
            </ul>
          )}
        </>
      )}
    </div>
  );
}

/**
 * Heuristic: does the tool's primary arg point to a real file on
 * disk we could open?  We keep this conservative — false positives
 * trigger a useless "open" click and add a misleading underline.
 */
function isPathArg(tool: string, argsJson?: string, output?: string): boolean {
  const t = tool.toLowerCase();
  const pathTools = ["read_file", "write_file", "edit_file", "patch", "edit", "delete_file", "create_file", "list_files", "search_files"];
  if (!pathTools.some((p) => t.includes(p))) return false;
  // Must have an actual path-looking string in args or output.
  return extractFirstPath(argsJson ?? "") !== null || extractFirstPath(output ?? "") !== null;
}

const PATH_HINT_RE = /(?:(?:[A-Za-z]:[\\/])|(?:[\\/]))(?:[\w.\-]+[\\/])+[\w.\-]+\.[a-z0-9]{1,5}|(?:[A-Za-z]:[\\/])[\w.\-]+\.[a-z0-9]{1,5}/g;
function extractFirstPath(blob: string): string | null {
  PATH_HINT_RE.lastIndex = 0;
  const m = PATH_HINT_RE.exec(blob);
  return m ? m[0] : null;
}

/**
 * Git-style diff stats from a tool's JSON args.
 *
 * - write_file: additions = new file content line count
 * - patch / edit_file: additions = new_string lines, deletions = old_string lines
 * - everything else: empty stats
 */
function extractLineStats(tool: string, argsJson?: string): { added?: number; removed?: number } {
  if (!argsJson) return {};
  let args: Record<string, unknown>;
  try {
    args = JSON.parse(argsJson);
  } catch {
    return {};
  }
  const t = tool.toLowerCase();
  if (t.includes("write_file") && typeof args.content === "string") {
    return { added: args.content.split("\n").length };
  }
  if ((t.includes("patch") || t.includes("edit_file")) && typeof args.new_string === "string") {
    const added = args.new_string.split("\n").length;
    const removed = typeof args.old_string === "string" ? args.old_string.split("\n").length : 0;
    return { added, removed };
  }
  return {};
}

/* ── Thinking dots — shows during processing gaps ──────────────── */
function ThinkingDots({ label = "正在处理" }: { label?: string }) {
  return (
    <div className="tdots h-enter">
      <span className="tdots-label">{label}</span>
      <span className="tdots-dot" style={{ animationDelay: "0ms" }} />
      <span className="tdots-dot" style={{ animationDelay: "200ms" }} />
      <span className="tdots-dot" style={{ animationDelay: "400ms" }} />
    </div>
  );
}

/**
 * Terminal-style output block. Renders stdout / stderr text in a
 * monospace, dark-background box that pins to the bottom while a
 * tool is still running. The pre-wrap keeps the original line
 * structure (so progress bars and stack traces stay readable), and
 * the auto-scroll effect doesn't fire on the initial paint — it
 * only follows new chunks after the user has had a chance to read
 * the head of the output.
 */
/**
 * Strip common JSON wrappers that tools wrap around their output.
 *
 * The Bash / command-exec tool often wraps output as:
 *   {"result":"<actual output>\r\n","exit_code":0}
 * or:
 *   {"error":"error message"}
 *
 * We extract the inner `result` or `error` field and unescape
 * JSON escape sequences so the user sees clean terminal output
 * instead of raw JSON.
 */
function unwrapToolOutput(raw: string): string {
  if (!raw) return raw;
  const trimmed = raw.trim();
  if (!trimmed.startsWith("{")) return raw;
  try {
    const parsed = JSON.parse(trimmed);
    if (typeof parsed.result === "string") return parsed.result;
    if (typeof parsed.error === "string") return parsed.error;
    if (typeof parsed.output === "string") return parsed.output;
    // Fall back to raw if no known field
    return raw;
  } catch {
    return raw;
  }
}

/**
 * Collapsible result panel. Renders the tool's stdout / stderr in a
 * clean monospace box — no "运行中 / 输出" header bar, no line-number
 * gutter, no green pulsing dot. The status of the run (running /
 * done / error / 中断) is already surfaced by the parent tool row's
 * icon + status badge, so a duplicate indicator inside the body is
 * pure noise. The parent row's `peek` already shows the first line
 * of the result, so when the user opens the panel they can read the
 * rest without another "你正在看 terminal 输出" label above the text.
 *
 * Empty state stays subtle: "等待输出…" while the run is live,
 * "(无输出)" once it's done. A hover-only copy chip at the top-right
 * keeps the affordance without adding a permanent header row.
 */
function LiveTerminal({ content, running }: { content: string; running: boolean }) {
  // Unwrap JSON result wrappers so the user sees clean terminal output.
  const displayContent = useMemo(() => unwrapToolOutput(content), [content]);
  // Copy-to-clipboard state. ~1.4s "已复制" flash so the affordance
  // gives clear feedback without nagging the user with a toast.
  const [copied, setCopied] = useState(false);
  const copyTimerRef = useRef<number | null>(null);

  // Cancel any pending "copied" reset on unmount so we don't fire
  // setState on a dead component.
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
        // Tauri webview fallback (e.g. http context without the
        // modern clipboard API): hidden textarea + execCommand.
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

function StatusPill({ status }: { status: "running" | "done" | "error" | undefined }) {
  // Defense-in-depth: any undefined/garbage status is treated as a
  // completed tool call. The historical bug was that wire-loaded
  // `tool_calls` arrived without a `status` field (Rust's `ToolCall`
  // struct doesn't carry one) and StatusPill's ternary fell through
  // to "运行中". Even with the projection in `api.ts#getSessionMessages`,
  // some legacy code paths still pass `tc.status` straight through —
  // this guard makes the whole component fail-safe.
  const safe: "running" | "done" | "error" =
    status === "running" || status === "done" || status === "error"
      ? status
      : "done";
  const cls =
    safe === "done" ? "pill pill-success" :
    safe === "error" ? "pill pill-error" :
    "pill pill-warning";
  const label = safe === "done" ? "完成" : safe === "error" ? "失败" : "运行中";
  return (
    <span className={cls}>
      {safe === "running" && <Loader2 size={9} className="animate-spin" />}
      {label}
    </span>
  );
}

function toolIcon(tool: string) {
  const t = tool.toLowerCase().trim();
  if (!t || t === "tool") return Wrench;

  const cat = classifyTool(t);
  switch (cat.kind) {
    case "shell":
    case "install":
    case "test":
      return Terminal;
    case "read":
    case "parse":
      return Eye;
    case "write":
      return FilePlus;
    case "edit":
      return Edit3;
    case "delete":
      return Trash2;
    case "search-web":
      return Search;
    case "search-file":
      return FileSearch;
    case "list":
      return FolderOpen;
    case "fetch":
    case "network":
      return Globe;
    case "image":
      return Eye;
    case "code":
      return Wrench;
    case "git":
      return GitBranch;
    case "build":
    case "deploy":
      return Hammer;
    case "db":
      return Database;
    case "desktop":
      return Bot;
    case "reason":
      return Brain;
    case "ask":
      return HelpCircle;
    case "clipboard":
      return FileText;
    default:
      return Wrench;
  }
}

/**
 * Tool "kind" — drives the row's `data-tool-kind` attribute.
 * Uses the comprehensive TOOL_CATEGORIES system.
 */
function toolKind(tool: string): string {
  const t = tool.toLowerCase().trim();
  if (!t || t === "tool") return "tool";
  const cat = classifyTool(t);
  return cat.kind;
}

function ToolCallsFallback({ calls }: { calls: NonNullable<ChatMessage["tool_calls"]> }) {
  return (
    <div className="tool-rail">
      {calls.map((tc, i) => (
        <div key={i} className="tool-row tool-done">
          <span className="rail-dot" />
          <div className="tool-head cursor-default">
            <Wrench size={12} className="text-text-muted" />
            <span className="tool-name">{tc.name}</span>
            <StatusPill status={tc.status} />
          </div>
        </div>
      ))}
    </div>
  );
}

// ── Approval card (role="approval") ───────────────────────────────
//
// Inlines a permission decision UI for a high-risk tool the agent wants
// to run. Lives between two assistant messages (one frozen on the left,
// a fresh empty placeholder on the right) and blocks the WS round-trip
// until the user clicks allow/deny.
//
// Why not AssistantMessage fallback: approval messages have
// `content: ""`, no `tool_calls`, no `execution_timeline` — they'd
// render as an empty Hermes bubble (avatar + "Hermes · 刚刚" + no
// body). Routing them here gives them a real visual identity that
// reads as "the agent is asking, not yet answering".
function riskVisual(risk?: string) {
  // Matches the server's `RiskLevel` enum
  // (`hermix-core::traits::RiskLevel`):
  //   "safe"     → green shield
  //   "caution"  → amber shield
  //   "dangerous"→ red shield
  const key = (risk ?? "caution").toLowerCase();
  if (key === "dangerous") {
    return {
      Icon: ShieldAlert,
      iconClass: "text-error",
      iconBg: "rgba(239, 68, 68, 0.10)",
      pillLabel: "高危操作",
      pillClass: "pill pill-error",
    };
  }
  if (key === "safe") {
    return {
      Icon: ShieldCheck,
      iconClass: "text-success",
      iconBg: "rgba(16, 185, 129, 0.10)",
      pillLabel: "安全",
      pillClass: "pill pill-success",
    };
  }
  return {
    Icon: ShieldCheck,
    iconClass: "text-warning",
    iconBg: "rgba(245, 158, 11, 0.10)",
    pillLabel: "需确认",
    pillClass: "pill pill-warning",
  };
}

function summarizeArgs(args: unknown): string {
  if (args == null) return "（无参数）";
  if (typeof args === "string") {
    return args.length > 600 ? args.slice(0, 600) + "\n…" : args;
  }
  try {
    const s = JSON.stringify(args, null, 2);
    return s.length > 600 ? s.slice(0, 600) + "\n…" : s;
  } catch {
    return String(args);
  }
}

/**
 * Render tool args as a clean key→value list instead of a raw
 * JSON code block.  Objects become aligned rows; primitives /
 * arrays fall back to `summarizeArgs` inside a muted pre block.
 */
/**
 * Generate a short human-readable description from tool args so the user
 * immediately understands what the pending operation will do — no need to
 * decode raw JSON.
 *
 * Examples:
 *   computer_use { action:"key", chord:"win" }  →  "按下 Win 键"
 *   computer_use { action:"type", text:"hello" } → "输入文本 "hello""
 *   bash { command:"ls -la" }                →  "运行命令: ls -la"
 *   write_file { path:"/foo/bar.rs" }        →  "写入文件: /foo/bar.rs"
 */
function describeToolArgs(tool: string, args: unknown): string {
  if (args == null) return "（无参数）";
  if (typeof args !== "object" || Array.isArray(args)) {
    return summarizeArgs(args);
  }
  // Tool-kind-specific snippets
  if (tool === "computer_use" || tool === "computer") {
    const a = args as Record<string, unknown>;
    const action = a.action;
    if (action === "key" && typeof a.chord === "string") return `按下 ${a.chord} 键`;
    if (action === "type" && typeof a.text === "string") return `输入文本 "${a.text}"`;
    if (action === "click" && typeof a.x === "number" && typeof a.y === "number") return `点击坐标 (${a.x}, ${a.y})`;
  }
  if (tool === "terminal" || tool === "bash") {
    const cmd = (args as Record<string, unknown>).command;
    if (typeof cmd === "string") return `运行命令: ${cmd}`;
  }
  if (tool === "write_file" || tool === "patch") {
    const p = (args as Record<string, unknown>).path;
    if (typeof p === "string") return `${tool === "patch" ? "修改" : "写入"}文件: ${p}`;
  }
  if (tool === "read_file") {
    const p = (args as Record<string, unknown>).path;
    if (typeof p === "string") return `读取文件: ${p}`;
  }
  return summarizeArgs(args);
}

function ApprovalCard({
  message,
  workspaceRoot: _workspaceRoot,
  onApprove,
  onDeny,
}: {
  message: ChatMessage;
  workspaceRoot?: string | null;
  onApprove?: (requestId: string) => void;
  onDeny?: (requestId: string) => void;
}) {
  const meta = message.approval;
  if (!meta) {
    return (
      <div className="h-enter flex items-center gap-2 text-[12px] text-text-faint italic px-2 py-1.5">
        <AlertCircle size={12} />
        <span>权限请求数据缺失</span>
      </div>
    );
  }
  const vis = riskVisual(meta.riskLevel);
  const { Icon } = vis;
  const decided = meta.decision === "approved" || meta.decision === "denied";
  const approved = meta.decision === "approved";
  const desc = describeToolArgs(meta.tool, meta.args);

  return (
    <div className={`approval-card h-enter ${decided ? (approved ? "approval-card--ok" : "approval-card--deny") : ""}`}>
      {/* Left accent bar */}
      <div className="approval-accent" style={{ background: decided
        ? (approved ? "var(--color-success)" : "var(--color-error)")
        : vis.iconBg.includes("239")
          ? "var(--color-error)"
          : vis.iconBg.includes("185")
            ? "var(--color-success)"
            : "var(--color-warning, #f59e0b)"
      }} />

      <div className="approval-inner">
        {/* Icon + tool row */}
        <div className="approval-head">
          <span
            className="approval-icon"
            style={{ background: vis.iconBg }}
            title={vis.pillLabel}
          >
            {decided ? (
              approved
                ? <Check size={12} className="text-success" />
                : <X size={12} className="text-error" />
            ) : (
              <Icon size={12} className={vis.iconClass} />
            )}
          </span>

          <div className="approval-body">
            <span className="approval-tool">{meta.tool}</span>
            <span className="approval-desc">{desc}</span>
          </div>

          {/* Right side */}
          <div className="approval-actions">
            {decided ? (
              <span className={`approval-decision ${approved ? "is-ok" : "is-err"}`}>
                {approved ? "已允许" : "已拒绝"}
              </span>
            ) : (
              <>
                <button
                  type="button"
                  onClick={() => onDeny?.(meta.requestId)}
                  className="approval-btn approval-btn--deny"
                >
                  拒绝
                </button>
                <button
                  type="button"
                  onClick={() => onApprove?.(meta.requestId)}
                  className="approval-btn approval-btn--ok"
                >
                  允许
                </button>
              </>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

// ── Clarify card (role="clarify") ──────────────────────────────────
//
// Inlines a question UI for the agent's `clarify` tool. The agent is
// blocked waiting on the answer; resolving the WS round-trip routes
// the user's choice back as `clarify_response`.
function ClarifyCard({
  message,
  onAnswer,
}: {
  message: ChatMessage;
  onAnswer?: (requestId: string, answer: string) => void;
}) {
  const meta = message.clarify;
  if (!meta) {
    // Defensive: a `role: "clarify"` message without meta is a server
    // contract violation. Show a small banner rather than a silent gap.
    return (
      <div className="h-enter flex items-center gap-2 text-[12px] text-text-faint italic">
        <AlertCircle size={12} />
        <span>澄清请求数据缺失</span>
      </div>
    );
  }
  const answered = meta.answer != null && meta.answer !== "";

  return (
    <div className="flex gap-2.5 h-enter items-start">
      <div
        className="shrink-0 mt-0.5 w-[22px] h-[22px] rounded-md flex items-center justify-center"
        style={{ background: "rgba(59, 130, 246, 0.10)" }}
        title="需要确认"
      >
        <HelpCircle size={12} className="text-accent" />
      </div>
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-1.5 mb-1 flex-wrap">
          <span className="text-[12.5px] font-semibold text-text-primary">需要您确认</span>
          <span className="h-tiny">· {formatRelativeTime(message.timestamp)}</span>
          {answered && <span className="pill pill-success">已答复</span>}
        </div>

        <div className="msg-card msg-stack">
          <div className="msg-channel" data-channel="clarify-question">
            <div
              className="prose max-w-none text-[13px] text-text-primary whitespace-pre-wrap"
              // Cap a runaway server-side question (>2k chars) inside a
              // scrollable container so it can't push the rest of the
              // timeline out of the viewport.
              style={{ maxHeight: "240px", overflow: "auto" }}
            >
              {meta.question?.trim() || (
                <span className="h-tiny italic text-text-faint">（空问题）</span>
              )}
            </div>
          </div>

          {answered ? (
            <div className="msg-channel" data-channel="clarify-answer">
              <div className="channel-label">
                <Check size={11} className="text-success" />
                您的回答
              </div>
              <div className="text-[12.5px] text-text-secondary whitespace-pre-wrap break-words">
                {meta.answer}
              </div>
            </div>
          ) : (
            <ClarifyComposer
              meta={meta}
              onSubmit={(answer) => onAnswer?.(meta.requestId, answer)}
            />
          )}
        </div>
      </div>
    </div>
  );
}

function ClarifyComposer({
  meta,
  onSubmit,
}: {
  meta: ClarifyMessageMeta;
  onSubmit: (answer: string) => void;
}) {
  const [text, setText] = useState("");
  const hasChoices = meta.choices && meta.choices.length > 0;
  const canSubmit = text.trim().length > 0;

  return (
    <div className="msg-channel" data-channel="clarify-input">
      {hasChoices && (
        <div className="flex flex-wrap gap-1.5 mb-2">
          {meta.choices.map((c, i) => (
            <button
              key={i}
              type="button"
              onClick={() => onSubmit(c)}
              className="px-2.5 py-1 rounded-md text-[12px] transition-colors"
              style={{
                background: "var(--color-bg-tertiary)",
                color: "var(--color-text-primary)",
                border: "1px solid var(--color-border-primary)",
              }}
            >
              {c}
            </button>
          ))}
        </div>
      )}
      <div className="flex items-center gap-2">
        <input
          type="text"
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && canSubmit) {
              e.preventDefault();
              onSubmit(text.trim());
              setText("");
            }
          }}
          placeholder={hasChoices ? "或直接输入自定义回答" : "请输入您的回答"}
          className="flex-1 px-2.5 py-1.5 rounded-md text-[12.5px] outline-none"
          style={{
            background: "var(--color-bg-tertiary)",
            color: "var(--color-text-primary)",
            border: "1px solid var(--color-border-primary)",
          }}
        />
        <button
          type="button"
          disabled={!canSubmit}
          onClick={() => {
            if (!canSubmit) return;
            onSubmit(text.trim());
            setText("");
          }}
          className="inline-flex items-center gap-1 px-2.5 py-1.5 rounded-md text-[12px] font-medium transition-colors disabled:opacity-30 disabled:cursor-not-allowed"
          style={{
            background: "var(--color-accent)",
            color: "var(--color-accent-on-text)",
          }}
        >
          提交
        </button>
      </div>
    </div>
  );
}
