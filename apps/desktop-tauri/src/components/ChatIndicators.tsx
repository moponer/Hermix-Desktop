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

/**
 * AgentActivityIndicator — shows below the streaming message when
 * the LLM stream has settled but the agent loop may still be working
 * (executing tools, deciding the next turn). Without this, the
 * user sees a blank gap between turns and may assume Hermix stopped
 * responding. The indicator pulses with the appropriate hint:
 *
 *   pendingToolsCount > 0  → "正在执行 N 个工具…"
 *   postStreamActivity    → "AI 正在分析结果 / 准备下一步…"
 *   fully idle             → (renders nothing — the "已完成" pill
 *                             handles that case instead)
 *
 * Layout is intentionally compact: a single 11.5px pill, WorkBuddy-
 * style, so it doesn't compete visually with the main content.
 */
export function AgentActivityIndicator({
  pendingToolsCount,
  visible,
}: {
  pendingToolsCount: number;
  visible: boolean;
}) {
  if (!visible) return null;
  const runningTools = pendingToolsCount > 0;
  return (
    <div className="msg-channel agent-activity" data-state={runningTools ? "tools" : "thinking"}>
      <div className="agent-activity-pill">
        {runningTools ? (
          <>
            <span className="agent-activity-dot-stack">
              <Loader2 size={11} className="animate-spin text-text-accent" />
            </span>
            <span>正在执行 {pendingToolsCount} 个工具…</span>
          </>
        ) : (
          <>
            <span className="agent-activity-dot-stack">
              <span className="agent-activity-bounce-dot" />
              <span className="agent-activity-bounce-dot agent-activity-bounce-dot--delay1" />
              <span className="agent-activity-bounce-dot agent-activity-bounce-dot--delay2" />
            </span>
            <span>AI 正在分析结果 / 准备下一步…</span>
          </>
        )}
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
export function WaitForInputIndicator({ visible }: { visible: boolean }) {
  if (!visible) return null;
  return (
    <div className="msg-channel wait-for-input" data-state="idle">
      <span className="wait-for-input-label">可以继续提问</span>
      <div className="wait-for-input-dots" aria-label="等待输入" role="status">
        <span className="wait-for-input-dot" />
        <span className="wait-for-input-dot wait-for-input-dot--delay1" />
        <span className="wait-for-input-dot wait-for-input-dot--delay2" />
      </div>
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
  onEdit,
  onRemove,
  onSend,
}: {
  index: number;
  total: number;
  text: string;
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
    <li className="group flex items-start gap-1.5 rounded px-2 py-1 hover:bg-bg-tertiary/50 transition-colors">
      <span className="shrink-0 mt-0.5 text-[10px] font-mono tabular-nums text-text-faint w-4 text-center">
        {index + 1}
      </span>

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
            className="w-full text-[12px] bg-bg-primary text-text-primary border border-border-subtle rounded px-1.5 py-1 outline-none resize-none focus:border-accent"
          />
          <div className="flex items-center gap-1.5 mt-1">
            <button
              onClick={commit}
              className="text-[10px] px-1.5 py-0.5 rounded font-medium"
              style={{
                background: "var(--color-accent)",
                color: "var(--color-accent-on-text)",
              }}
            >
              保存
            </button>
            <button
              onClick={cancel}
              className="text-[10px] text-text-muted hover:text-text-primary px-1.5 py-0.5 rounded hover:bg-bg-tertiary"
            >
              取消
            </button>
            <span className="text-[10px] text-text-faint">
              Enter 保存 · Shift+Enter 换行 · Esc 取消
            </span>
          </div>
        </div>
      ) : (
        <>
          <button
            type="button"
            onClick={() => canEdit && setEditing(true)}
            disabled={!canEdit}
            title={text}
            className={`flex-1 min-w-0 text-left text-[12px] text-text-primary whitespace-pre-wrap break-words leading-snug ${canEdit ? "cursor-text" : "cursor-default"}`}
          >
            {text.length > 80 ? text.slice(0, 80) + "…" : text}
          </button>

          <div className="shrink-0 flex items-center gap-0.5 opacity-0 group-hover:opacity-100 focus-within:opacity-100 transition-opacity">
            {canEdit && (
              <button
                onClick={() => setEditing(true)}
                title="编辑 (Enter)"
                aria-label="编辑队列项"
                className="p-1 rounded text-text-muted hover:text-text-primary hover:bg-bg-tertiary"
              >
                <Pencil size={11} />
              </button>
            )}
            {canSend && (
              <button
                onClick={onSend}
                title="立即发送"
                aria-label="立即发送此条"
                className="p-1 rounded text-text-muted hover:text-accent hover:bg-bg-tertiary"
              >
                <Send size={11} />
              </button>
            )}
            {canRemove && (
              <button
                onClick={onRemove}
                title="删除"
                aria-label="删除此条"
                className="p-1 rounded text-text-muted hover:text-red-400 hover:bg-bg-tertiary"
              >
                <X size={11} />
              </button>
            )}
          </div>
        </>
      )}
    </li>
  );
}
