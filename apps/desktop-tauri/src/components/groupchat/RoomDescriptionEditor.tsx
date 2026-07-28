/**
 * Room description editor — inline view/edit toggle for the room's
 * free-form description (project background / goal / constraints).
 *
 * Rendered as a single-line preview that expands into a textarea
 * on click. Saves on blur or when the user clicks the check button;
 * clears via the trash button when a description already exists.
 *
 * Kept as a memoised sub-component so the surrounding RoomHeader
 * doesn't re-render whenever the user types in the textarea.
 */

import { memo, useCallback, useEffect, useRef, useState } from "react";
import { Edit3, X, Check } from "lucide-react";

type Props = {
  description: string | null;
  onSave?: (next: string | null) => Promise<void> | void;
};

export const RoomDescriptionEditor = memo(function RoomDescriptionEditor({
  description,
  onSave,
}: Props) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const [saving, setSaving] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  // Keep draft in sync when the underlying description changes
  // (e.g. after a PATCH round-trip) but only when we're NOT editing
  // — otherwise the user's in-flight typing would be clobbered.
  useEffect(() => {
    if (!editing) setDraft(description ?? "");
  }, [description, editing]);

  const beginEdit = useCallback(() => {
    if (!onSave) return;
    setDraft(description ?? "");
    setEditing(true);
    // Focus + autosize the textarea after the toggle renders.
    queueMicrotask(() => {
      const ta = textareaRef.current;
      if (!ta) return;
      ta.focus();
      ta.setSelectionRange(ta.value.length, ta.value.length);
      ta.style.height = "auto";
      ta.style.height = `${ta.scrollHeight}px`;
    });
  }, [description, onSave]);

  const cancel = useCallback(() => {
    setEditing(false);
    setDraft(description ?? "");
  }, [description]);

  const commit = useCallback(async () => {
    if (!onSave) return;
    const next = draft.trim();
    // Skip the network round-trip when nothing changed — saves a
    // pointless PATCH when the user opened the editor and just
    // closed it without touching the text.
    const currentTrimmed = (description ?? "").trim();
    if (next === currentTrimmed) {
      setEditing(false);
      return;
    }
    setSaving(true);
    try {
      await onSave(next.length === 0 ? null : next);
      setEditing(false);
    } catch (e) {
      // Stay in edit mode so the user can recover; the parent
      // surfaces the error via toast.
      console.error("Failed to save description", e);
    } finally {
      setSaving(false);
    }
  }, [description, draft, onSave]);

  const clear = useCallback(async () => {
    if (!onSave) return;
    setSaving(true);
    try {
      await onSave(null);
      setEditing(false);
    } finally {
      setSaving(false);
    }
  }, [onSave]);

  if (!editing) {
    const hasText = Boolean(description);
    return (
      <div
        className="flex items-start gap-1.5 text-[12px] leading-snug text-text-muted"
        data-testid="room-description-preview"
      >
        {hasText ? (
          <span className="flex-1 whitespace-pre-wrap break-words line-clamp-2">
            {description}
          </span>
        ) : (
          <button
            onClick={beginEdit}
            className="text-[12px] font-medium hover:underline"
            style={{ color: "var(--color-accent)" }}
            title="添加描述"
            data-testid="room-description-add"
          >
            + 添加描述
          </button>
        )}
        {hasText && onSave && (
          <button
            onClick={beginEdit}
            className="shrink-0 px-1 rounded hover:bg-bg-hover text-text-muted"
            title="编辑描述"
            data-testid="room-description-edit"
          >
            <Edit3 size={11} />
          </button>
        )}
      </div>
    );
  }

  return (
    <div
      className="rounded-lg p-2 text-[12px]"
      style={{
        background:
          "color-mix(in srgb, var(--color-accent) 6%, transparent)",
        border:
          "1px solid color-mix(in srgb, var(--color-accent) 20%, transparent)",
      }}
      data-testid="room-description-editor"
    >
      <textarea
        ref={textareaRef}
        value={draft}
        onChange={(e) => {
          setDraft(e.target.value);
          const ta = e.currentTarget;
          ta.style.height = "auto";
          ta.style.height = `${ta.scrollHeight}px`;
        }}
        onKeyDown={(e) => {
          // Ctrl/Cmd+Enter commits; Escape cancels. Plain Enter
          // inserts a newline so multi-line descriptions work.
          if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
            e.preventDefault();
            void commit();
          } else if (e.key === "Escape") {
            e.preventDefault();
            cancel();
          }
        }}
        placeholder="项目背景 / 目标 / 约束 …（Ctrl+Enter 保存，Esc 取消）"
        disabled={saving}
        rows={2}
        className="w-full bg-transparent resize-none outline-none text-text-secondary placeholder:text-text-muted placeholder:italic"
        style={{ minHeight: "2.4em" }}
      />
      <div className="flex items-center justify-end gap-1 mt-1">
        {description && (
          <button
            onClick={() => void clear()}
            disabled={saving}
            className="btn btn-secondary btn-sm"
            title="清空描述"
            data-testid="room-description-clear"
          >
            <X size={11} /> 清空
          </button>
        )}
        <button
          onClick={cancel}
          disabled={saving}
          className="btn btn-secondary btn-sm"
        >
          取消
        </button>
        <button
          onClick={() => void commit()}
          disabled={saving}
          className="btn btn-primary btn-sm"
          data-testid="room-description-save"
        >
          <Check size={11} /> {saving ? "保存中…" : "保存"}
        </button>
      </div>
    </div>
  );
});
