/**
 * ChatIndicators.tsx — leaf components extracted from ChatView.tsx
 * to break up its God Object.
 *
 * These three components are fully self-contained (react + lucide icons
 * only) and render below the message list / inside the queue panel.
 * Keeping them here shrinks ChatView.tsx and makes them independently
 * testable.
 */
import { useState, useRef, useEffect } from "react";
import { Loader2, X, Pencil, Send } from "lucide-react";
import { DELETE } from "./strings";

/**
 * AgentActivityIndicator — single source of truth for "agent is still
 * working" at the bottom of the chat. Unified style: always a
 * **rotating Loader2 spinner** + a single short label. No more mixed
 * bounce-dot / spinner / dot-stack variants — the user reads it as
 * one consistent "working" pill regardless of which sub-state
 * (streaming / tool execution / between-turn analysis) is active.
 *
 *   replying=true             → "正在回复…"
 *   runningTools > 0          → "正在执行 N 个工具…"
 *   postStreamActivity (rest) → "正在分析结果…"
 *   fully idle                → renders nothing (WaitForInputIndicator
 *                                handles idle with a distinct static pill)
 */
export function AgentActivityIndicator({
  pendingToolsCount,
  visible,
  replying = false,
}: {
  pendingToolsCount: number;
  visible: boolean;
  /** true when the LLM is actively streaming the answer body. */
  replying?: boolean;
}) {
  if (!visible) return null;
  const runningTools = pendingToolsCount > 0;
  // 文案选择: 流式 > 工具 > 分析. 三种共享同一动画 (旋转 Loader2),
  // 只是文字不同 — 视觉一致, 信息分情况.
  let label: string;
  let state: string;
  if (replying) {
    label = "正在回复…";
    state = "replying";
  } else if (runningTools) {
    // 明示"并行 N 个"——已完成的工具不计入此数（用户 2026-08-19
    // 反馈：底部显示"2 个"与顶部 3 个 tool card 视觉混淆，
    // 因为 1 个已完成 + 2 个仍运行 = 3 张卡，但 running 计数为 2）。
    // 改为"并行"提示用户"这是当前仍在并行跑的，"
    label = `正在并行执行 ${pendingToolsCount} 个工具…`;
    state = "tools";
  } else {
    label = "正在分析结果…";
    state = "thinking";
  }
  // 思考滚动文本已移除（2026-10-01 用户反馈）：与上方展开的思考过程
  // 完全重复，且宽度受限被截成半句，读起来像坏掉的 UI。折叠时的思考
  // 内容请直接展开思考块查看。
  return (
    <div className="msg-channel agent-activity" data-state={state}>
      <div className="agent-activity-pill">
        <Loader2 className="icon-sm animate-spin text-text-accent" />
        <span>{label}</span>
      </div>
    </div>
  );
}

/**
 * WaitForInputIndicator — shown at the very bottom of the message
 * list when (a) there's at least one assistant message, (b) no
 * stream is active, (c) no tools are pending, and (d) the agent
 * loop has fully ended (post-stream activity window expired). This
 * tells the user "Hermix finished and is waiting for your next
 * message" — solving the "is it stuck or done?" ambiguity that
 * silent completion causes.
 */
export function WaitForInputIndicator({
  visible,
  hint,
}: {
  visible: boolean;
  /** Optional override for the idle hint text. Defaults to "可以继续提问". */
  hint?: string;
}) {
  if (!visible) return null;
  // 静态文字, 无动画 — 与"工作中"指示器 (旋转 Loader2) 形成视觉对比:
  // 旋转 = 忙; 静止 = 等用户.
  return (
    <div className="msg-channel wait-for-input" data-state="idle">
      <span className="wait-for-input-label">{hint ?? "可以继续提问"}</span>
    </div>
  );
}

/**
 * QueueItemRow — one editable row inside the pending-message queue
 * panel. Supports inline edit (Enter save / Shift+Enter newline /
 * Esc cancel), send-now, and remove. Empty draft on commit drops
 * the row.
 */
export function QueueItemRow({
  index,
  total: _total,
  text,
  canEdit,
  canRemove,
  canSend,
  status,
  error,
  onEdit,
  onRemove,
  onSend,
}: {
  index: number;
  total: number;
  text: string;
  /** 等待 / 发送中 / 失败 —— 三态必须有可见差异，否则用户读不出队列在干什么。 */
  status?: "queued" | "sending" | "failed";
  error?: string;
  canEdit: boolean;
  canRemove: boolean;
  canSend: boolean;
  onEdit: (newText: string) => void;
  onRemove: () => void;
  onSend: () => void;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(text);
  const taRef = useRef<HTMLTextAreaElement>(null);

  // When entering edit mode, focus the textarea and seed draft.
  useEffect(() => {
    if (!editing) return;
    setDraft(text);
    const id = window.setTimeout(() => taRef.current?.focus(), 0);
    return () => window.clearTimeout(id);
  }, [editing, text]);

  // If the queue text changes underneath us, keep our draft in sync
  // — but only when not editing, otherwise we'd clobber the user's
  // in-progress input.
  useEffect(() => {
    if (!editing) setDraft(text);
  }, [text, editing]);

  const commit = () => {
    const trimmed = draft.trim();
    if (!trimmed) {
      onRemove();
    } else if (trimmed !== text) {
      onEdit(trimmed);
    }
    setEditing(false);
  };

  const cancel = () => {
    setDraft(text);
    setEditing(false);
  };

  return (
    <li
      className="group flex items-start gap-2 rounded-md px-1.5 py-1 transition-colors hover:bg-bg-quaternary/60"
      aria-label={`第 ${index + 1} 条排队消息`}
    >
      {/* 序号：让这段读起来是「待发清单」，而不是又一段正文。 */}
      <span className="mt-0.5 w-3 shrink-0 select-none text-right font-mono text-[length:var(--text-tiny,11px)] text-text-faint">
        {index + 1}
      </span>
      {/* 只有"正在发送 / 失败"才需要状态点。等待中的每行都点一颗灰点，
          会让列表读起来像调试输出，而不是待发消息。 */}
      {(status === "sending" || status === "failed") && (
        <span
          className={`mt-1.5 size-1.5 shrink-0 rounded-full ${
            status === "failed"
              ? "bg-[var(--color-error)]"
              : "bg-[var(--color-accent)] animate-pulse"
          }`}
          title={status === "failed" ? `发送失败${error ? `：${error}` : ""}` : "正在发送"}
          aria-label={status === "failed" ? "发送失败" : "正在发送"}
        />
      )}

      {editing ? (
        <div className="flex-1 min-w-0">
          <textarea
            ref={taRef}
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                commit();
              } else if (e.key === "Escape") {
                e.preventDefault();
                cancel();
              }
            }}
            rows={2}
            className="w-full text-meta bg-bg-primary text-text-primary rounded px-1.5 py-1 outline-none resize-none focus:border-accent"
          />
          <div className="flex items-center gap-1.5 mt-1">
            <button
              onClick={commit}
              className="text-tiny px-1.5 py-0.5 rounded font-medium bg-accent text-on-accent"
            >
              保存
            </button>
            <button
              onClick={cancel}
              className="text-tiny text-text-muted hover:text-text-primary px-1.5 py-0.5 rounded hover:bg-bg-tertiary"
            >
              取消
            </button>
            <span className="text-tiny text-text-faint">
              Enter 保存 · Shift+Enter 换行 · Esc 取消
            </span>
          </div>
        </div>
      ) : (
        <>
          <div className="min-w-0 flex-1">
            <button
              type="button"
              onClick={() => canEdit && setEditing(true)}
              disabled={!canEdit}
              title={text}
              /* 待发条目的墨色比输入正文低一档（secondary，悬停才升到 primary）：
                 否则两条文字块紧挨着，用户分不清哪条是"已经写好的"、哪条是"要发的"。 */
              className={`block w-full text-left text-[length:var(--text-ui)] leading-relaxed text-text-secondary line-clamp-2 whitespace-pre-wrap break-words ${canEdit ? "cursor-text hover:text-text-primary" : "cursor-default"}`}
            >
              {text}
            </button>
            {status === "failed" && error && (
              <div className="mt-0.5 text-[length:var(--text-tiny,11px)] text-[var(--color-error)]">
                {error}
              </div>
            )}
          </div>

          <div className="shrink-0 flex items-center gap-0.5 opacity-0 group-hover:opacity-100 focus-within:opacity-100 transition-opacity">
            {canEdit && (
              <button
                onClick={() => setEditing(true)}
                title="编辑 (Enter)"
                aria-label="编辑队列项"
                className="p-1 rounded text-text-muted hover:text-text-primary hover:bg-bg-tertiary"
              >
                <Pencil className="icon-sm" />
              </button>
            )}
            {canSend && (
              <button
                onClick={onSend}
                title="立即发送"
                aria-label="立即发送此条"
                className="p-1 rounded text-text-muted hover:text-accent hover:bg-bg-tertiary"
              >
                <Send className="icon-sm" />
              </button>
            )}
            {canRemove && (
              <button
                onClick={onRemove}
                title={DELETE}
                aria-label="删除此条"
                className="p-1 rounded text-text-muted hover:text-red-400 hover:bg-bg-tertiary"
              >
                <X className="icon-sm" />
              </button>
            )}
          </div>
        </>
      )}
    </li>
  );
}
