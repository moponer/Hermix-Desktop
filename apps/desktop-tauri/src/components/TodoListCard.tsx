// Inline task list card rendered in the message stream.
// one card per plan / group. The agent emits a
// `group_started` event when it begins a new plan (first `todo.create`
// for a fresh group_id) and subsequent `upsert` / `remove` events for
// items in that plan update this card in place. Multiple plans coexist
// in the same session; each card is anchored to the assistant message
// that declared it (or to the end of the stream when the server did
// not stamp a message_id).
// Replaces the v1 global TodoPanel which collapsed every todo in the
// session into a single flat list. The flat view was wrong for the
// multi-turn use case: a user starting a new conversation mid-session
// about a different topic would see the old plan in the same panel
// alongside the new one, with no visual separation.
// Design constraints (matching the master design spec):
// - "无蓝紫" — gray/zinc family only, no slate/blue/violet.
// - Plain click to toggle done <-> pending; no drag-reorder.
// - Priority visual = a 4px left bar (gray for medium, brighter
// for high, dimmer for low). No badges.
// - Group title row above the items when set (LLM-provided label
// like "rename all files").

import { useState, useCallback, useMemo, type MutableRefObject } from "react";
import { ChevronDown, ChevronRight, ListTodo, Loader2 } from "lucide-react";
import { clsx } from "clsx";
import * as api from "../api";
import type { TodoItem } from "../api";
import { toastError } from "../hooks/useToast";

const PRIORITY_BAR_COLOR: Record<string, string> = {
  high: "var(--color-text-primary)",
  medium: "var(--color-text-secondary)",
  low: "var(--color-text-faint)",
};

function priorityColor(p: string): string {
  return PRIORITY_BAR_COLOR[p] ?? PRIORITY_BAR_COLOR.medium;
}

function priorityLabel(p: string): string {
  return p === "high" ? "高" : p === "low" ? "低" : "中";
}

function statusGlyph(s: string): string {
  return s === "completed" ? "✓" : s === "failed" ? "✗" : s === "in_progress" ? "◐" : "○";
}

export interface TodoListCardProps {
  groupId: string;
  groupTitle?: string | null;
  /** Items in this group, keyed by id. The parent owns the store. */
  itemsById: Record<string, TodoItem>;
  /** Patch + hydrate through the same path the LLM uses. */
  onPatch?: (id: string, patch: { status?: string; title?: string; priority?: string }) => Promise<unknown>;
  /**
   * Shared ref between TodoListCard and the parent (App.tsx).
   * Card adds an item id before an optimistic patch and removes it
   * when the patch settles.  The parent skips WS echo upserts for
   * ids that are currently in-flight, preventing rapid-toggle races.
   */
  justPatchedRef?: MutableRefObject<Set<string>>;
}

export function TodoListCard({ groupId, groupTitle, itemsById, onPatch, justPatchedRef }: TodoListCardProps) {
  const [collapsed, setCollapsed] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);

  const items = useMemo(() => {
    const arr = Object.values(itemsById);
    const tier = (s: string) =>
      s === "completed" ? 3 : s === "failed" ? 3 : s === "in_progress" ? 1 : 2;
    const priRank = (p: string) => (p === "high" ? 0 : p === "medium" ? 1 : 2);
    return arr.sort((a, b) => {
      const t = tier(a.status) - tier(b.status);
      if (t !== 0) return t;
      const r = priRank(a.priority) - priRank(b.priority);
      if (r !== 0) return r;
      return a.created_at.localeCompare(b.created_at);
    });
  }, [itemsById]);

  const completedCount = items.filter((i) => i.status === "completed" || i.status === "failed").length;
  const totalCount = items.length;

  const toggle = useCallback(
    async (item: TodoItem) => {
      if (busyId === item.id) return;
      setBusyId(item.id);
      justPatchedRef?.current.add(item.id);
      const nextStatus = item.status === "completed" ? "pending" : "completed";
      try {
        if (onPatch) {
          await onPatch(item.id, { status: nextStatus });
        } else {
          await api.patchTodo(item.id, { status: nextStatus });
        }
      } catch (e) {
        console.error("[TodoListCard] patch failed", e);
toastError("任务状态更新失败");
        // Clean up so the server's eventual echo for this item is
        // NOT skipped — otherwise rapid-toggle retry is silently lost.
        justPatchedRef?.current.delete(item.id);
      } finally {
        setBusyId(null);
      }
    },
    [busyId, onPatch],
  );

  // groupId drives the React key in the parent — consumed, not dead.
  void groupId;

  return (
    <div className="todo-card">
      <button
        className="todo-card-header"
        onClick={() => setCollapsed((c) => !c)}
        type="button"
      >
        {collapsed
          ? <ChevronRight size={12} className="todo-card-chevron" />
          : <ChevronDown size={12} className="todo-card-chevron" />}
        <ListTodo size={13} className="todo-card-icon" />
        <span className="todo-card-title">
          {groupTitle || "任务清单"}
        </span>
        {totalCount > 0 && (
          <span className="todo-card-count">
            {completedCount}/{totalCount}
          </span>
        )}
      </button>

      {!collapsed && (
        <div className="todo-card-body">
          {items.length === 0 && (
            <p className="todo-card-empty">还没有任务</p>
          )}
          {items.map((item) => (
            <TodoRow
              key={item.id}
              item={item}
              busy={busyId === item.id}
              onToggle={() => toggle(item)}
              allItems={itemsById}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function TodoRow({
  item,
  busy,
  onToggle,
  allItems,
}: {
  item: TodoItem;
  busy: boolean;
  onToggle: () => void;
  allItems: Record<string, TodoItem>;
}) {
  const isDone = item.status === "completed" || item.status === "failed";
  const dependsOnLabels = item.depends_on
    .map((id) => allItems[id]?.title ?? id.slice(0, 6))
    .filter(Boolean);

  return (
    <div
      className={clsx(
        "todo-row",
        isDone && "todo-row-done",
        busy && "todo-row-busy",
      )}
      style={{ borderLeftColor: priorityColor(item.priority) }}
    >
      <button
        type="button"
        className="todo-row-check"
        onClick={onToggle}
        disabled={busy}
        title={isDone ? "标记为未完成" : "标记为已完成"}
        aria-pressed={isDone}
      >
        {busy
          ? <Loader2 size={12} className="todo-row-spin" />
          : <span className="todo-row-glyph">{statusGlyph(item.status)}</span>}
      </button>
      <div className="todo-row-body">
        <div className="todo-row-title">{item.title}</div>
        {(item.description || dependsOnLabels.length > 0) && (
          <div className="todo-row-meta">
            {item.description && (
              <span className="todo-row-desc">{item.description}</span>
            )}
            {dependsOnLabels.length > 0 && (
              <span className="todo-row-deps">
                依赖: {dependsOnLabels.join(", ")}
              </span>
            )}
          </div>
        )}
      </div>
      <span
        className="todo-row-priority"
        title={`优先级: ${priorityLabel(item.priority)}`}
      >
        {priorityLabel(item.priority)}
      </span>
    </div>
  );
}
