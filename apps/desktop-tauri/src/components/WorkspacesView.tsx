/**
 * WorkspacesView — 工作台 Tab 容器（M1.3）。
 *
 * 多工作台 Tab 切换 / 创建 / 重命名 / 删除，全部走后端
 * `/api/workspaces`（镜像 `hermix_workspace::RegistryStore`）。
 * 内容区展示工作台详情（schema 摘要）；模块渲染器在 M1.4 接入。
 *
 * 交互约定：
 * - "+ 新建" → 内联输入框 → Enter 创建（空名拒绝）
 * - 每个 Tab 右上角 hover 显示 重命名(✎) / 删除(🗑)
 * - 删除两段式确认（点 🗑 → 变 "确认删除?" → 再点执行，失焦取消）
 */

import { useCallback, useEffect, useRef, useState } from "react";
import {
  Plus,
  Pencil,
  Trash2,
  Loader2,
  LayoutDashboard,
  X,
} from "lucide-react";
import type { Layout } from "react-grid-layout";
import * as api from "../api";
import type { WorkspaceDetail, WorkspaceEntry, WorkspaceModule } from "../types";
import { toastError, toastSuccess } from "../hooks/useToast";
import { WorkspaceRenderer } from "./workspace/WorkspaceRenderer";

type TabMode = "idle" | "creating" | "renaming" | "confirm-delete";

/** 每表默认查询上限（防大表拖垮前端）。 */
const TABLE_QUERY_LIMIT = 500;
/** 布局拖拽持久化防抖（ms）——react-grid-layout 拖拽过程频繁回调。 */
const LAYOUT_SAVE_DEBOUNCE_MS = 500;

function tableModules(schema: WorkspaceDetail["schema"]): Extract<WorkspaceModule, { type: "table" }>[] {
  return (schema.modules ?? []).filter(
    (m): m is Extract<WorkspaceModule, { type: "table" }> => m.type === "table",
  );
}

export function WorkspacesView() {
  const [entries, setEntries] = useState<WorkspaceEntry[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [detail, setDetail] = useState<WorkspaceDetail | null>(null);
  const [tableRows, setTableRows] = useState<Record<string, Record<string, unknown>[]>>({});
  const [loading, setLoading] = useState(true);
  const [mode, setMode] = useState<TabMode>("idle");
  const [draft, setDraft] = useState("");
  // 记录正在操作的 tab（renaming/confirm-delete 的目标）
  const [actingId, setActingId] = useState<string | null>(null);
  // 布局保存防抖 timer
  const layoutTimer = useRef<number | null>(null);

  const refresh = useCallback(async () => {
    try {
      const list = await api.listWorkspaces();
      setEntries(list);
      // 保持当前 tab 有效；失效则回退到第一个
      setActiveId((prev) =>
        prev && list.some((e) => e.id === prev) ? prev : list[0]?.id ?? null,
      );
    } catch (e) {
      toastError(`加载工作台失败: ${String(e)}`);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // activeId 变化 → 拉详情 + 各表行数据
  useEffect(() => {
    if (!activeId) {
      setDetail(null);
      setTableRows({});
      return;
    }
    let cancelled = false;
    api
      .getWorkspace(activeId)
      .then(async (d) => {
        if (cancelled) return;
        setDetail(d);
        // 拉取每个 table 模块的行（并发，各自上限）
        const rowsMap: Record<string, Record<string, unknown>[]> = {};
        const tables = tableModules(d.schema);
        await Promise.all(
          tables.map(async (t) => {
            try {
              rowsMap[t.id] = await api.workspaceQueryTable(activeId, t.id, {
                limit: TABLE_QUERY_LIMIT,
              });
            } catch (e) {
              if (!cancelled) toastError(`加载表「${t.name}」失败: ${String(e)}`);
            }
          }),
        );
        if (!cancelled) setTableRows(rowsMap);
      })
      .catch((e) => {
        if (!cancelled) toastError(`加载工作台详情失败: ${String(e)}`);
      });
    return () => {
      cancelled = true;
    };
  }, [activeId]);

  // 组件卸载时清理布局保存 timer
  useEffect(() => {
    return () => {
      if (layoutTimer.current !== null) window.clearTimeout(layoutTimer.current);
    };
  }, []);

  /** 布局拖拽/缩放 → 更新本地 schema 并防抖持久化回后端。 */
  const handleLayoutChange = useCallback((moduleId: string, layout: Layout) => {
    setDetail((prev) => {
      if (!prev) return prev;
      const prevLayout =
        prev.schema.layout && typeof prev.schema.layout === "object"
          ? (prev.schema.layout as Record<string, Layout>)
          : {};
      const schema = { ...prev.schema, layout: { ...prevLayout, [moduleId]: layout } };
      // 防抖保存（拖拽过程频繁回调，只在停止后落盘）
      if (layoutTimer.current !== null) window.clearTimeout(layoutTimer.current);
      layoutTimer.current = window.setTimeout(() => {
        void api
          .updateWorkspaceSchema(prev.id, schema)
          .catch((e) => toastError(`保存布局失败: ${String(e)}`));
      }, LAYOUT_SAVE_DEBOUNCE_MS);
      return { ...prev, schema };
    });
  }, []);

  const handleCreate = useCallback(async () => {
    const name = draft.trim();
    if (!name) return;
    try {
      const { id } = await api.createWorkspace(name);
      toastSuccess(`工作台「${name}」已创建`);
      setDraft("");
      setMode("idle");
      await refresh();
      setActiveId(id); // 直接切到新工作台
    } catch (e) {
      toastError(`创建工作台失败: ${String(e)}`);
    }
  }, [draft, refresh]);

  const handleRename = useCallback(
    async (id: string) => {
      const name = draft.trim();
      if (!name) {
        setMode("idle");
        setActingId(null);
        return;
      }
      try {
        await api.renameWorkspace(id, name);
        toastSuccess("已重命名");
        setMode("idle");
        setActingId(null);
        await refresh();
      } catch (e) {
        toastError(`重命名失败: ${String(e)}`);
      }
    },
    [draft, refresh],
  );

  const handleDelete = useCallback(
    async (id: string) => {
      try {
        await api.deleteWorkspace(id);
        toastSuccess("工作台已删除");
        setMode("idle");
        setActingId(null);
        await refresh();
      } catch (e) {
        toastError(`删除失败: ${String(e)}`);
      }
    },
    [refresh],
  );

  // 重置内联输入态（点击别处）
  const resetMode = useCallback(() => {
    setMode("idle");
    setActingId(null);
    setDraft("");
  }, []);

  return (
    <div className="flex h-full flex-col px-4 py-3">
      {/* ── Tab 栏 ── */}
      <div
        className="mb-3 flex items-center gap-1.5 overflow-x-auto rounded-lg border px-2 py-1.5"
        style={{
          borderColor: "var(--color-border-secondary)",
          background: "var(--color-bg-elevated)",
        }}
        onMouseLeave={resetMode}
      >
        <LayoutDashboard size={15} style={{ color: "var(--color-text-faint)" }} />
        {loading ? (
          <Loader2 size={14} className="animate-spin" style={{ color: "var(--color-text-faint)" }} />
        ) : entries.length === 0 ? (
          <span className="text-[12px]" style={{ color: "var(--color-text-faint)" }}>
            还没有工作台，点「+ 新建」创建第一个
          </span>
        ) : (
          entries.map((e) => (
            <TabItem
              key={e.id}
              entry={e}
              active={e.id === activeId}
              mode={actingId === e.id ? mode : "idle"}
              draft={draft}
              onSelect={() => {
                resetMode();
                setActiveId(e.id);
              }}
              onStartRename={() => {
                setActingId(e.id);
                setDraft(e.name);
                setMode("renaming");
              }}
              onStartDelete={() => {
                setActingId(e.id);
                setMode("confirm-delete");
              }}
              onCancel={resetMode}
              onDraftChange={setDraft}
              onSubmit={() => {
                if (mode === "renaming") void handleRename(e.id);
                else if (mode === "confirm-delete") void handleDelete(e.id);
              }}
            />
          ))
        )}

        {/* 新建按钮 / 输入框 */}
        {mode === "creating" ? (
          <input
            autoFocus
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void handleCreate();
              if (e.key === "Escape") resetMode();
            }}
            onBlur={resetMode}
            placeholder="工作台名称，回车创建"
            className="w-44 rounded-md border px-2 py-1 text-[12px] outline-none"
            style={{
              borderColor: "var(--color-border-secondary)",
              background: "var(--color-bg-input, var(--color-bg-base))",
              color: "var(--color-text-primary)",
            }}
          />
        ) : (
          <button
            onClick={() => {
              setMode("creating");
              setActingId(null);
              setDraft("");
            }}
            className="flex shrink-0 items-center gap-1 rounded-md px-2 py-1 text-[12px] transition-opacity hover:opacity-80"
            style={{ color: "var(--color-text-secondary)" }}
            title="新建工作台"
          >
            <Plus size={13} /> 新建
          </button>
        )}
      </div>

      {/* ── 内容区（M1.4 起由 WorkspaceRenderer 渲染 schema 模块） ── */}
      {detail ? (
        <WorkspaceRenderer
          detail={detail}
          tableRows={tableRows}
          onLayoutChange={handleLayoutChange}
        />
      ) : (
        <div
          className="flex flex-1 items-center justify-center rounded-lg border border-dashed text-[13px]"
          style={{
            borderColor: "var(--color-border-secondary)",
            color: "var(--color-text-faint)",
          }}
        >
          {entries.length === 0 ? "创建或选择一个工作台开始" : "加载中…"}
        </div>
      )}
    </div>
  );
}

// ────────────────────────────────────────────────────────────
// Tab 项
// ────────────────────────────────────────────────────────────
interface TabItemProps {
  entry: WorkspaceEntry;
  active: boolean;
  mode: TabMode;
  draft: string;
  onSelect: () => void;
  onStartRename: () => void;
  onStartDelete: () => void;
  onCancel: () => void;
  onDraftChange: (v: string) => void;
  onSubmit: () => void;
}

function TabItem({
  entry,
  active,
  mode,
  draft,
  onSelect,
  onStartRename,
  onStartDelete,
  onCancel,
  onDraftChange,
  onSubmit,
}: TabItemProps) {
  const [hovering, setHovering] = useState(false);

  if (mode === "renaming") {
    return (
      <input
        autoFocus
        value={draft}
        onChange={(e) => onDraftChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") onSubmit();
          if (e.key === "Escape") onCancel();
        }}
        onBlur={onCancel}
        className="w-40 rounded-md border px-2 py-1 text-[12px] outline-none"
        style={{
          borderColor: "var(--color-accent)",
          background: "var(--color-bg-input, var(--color-bg-base))",
          color: "var(--color-text-primary)",
        }}
      />
    );
  }

  if (mode === "confirm-delete") {
    return (
      <span
        className="flex shrink-0 items-center gap-1 rounded-md border px-2 py-1 text-[12px]"
        style={{
          borderColor: "var(--color-error)",
          color: "var(--color-error)",
          background: "color-mix(in srgb, var(--color-error) 8%, transparent)",
        }}
        onMouseLeave={onCancel}
      >
        <span>确认删除「{entry.name}」?</span>
        <button
          onClick={onSubmit}
          className="font-semibold underline"
          title="确认删除（不可恢复）"
        >
          删除
        </button>
        <button onClick={onCancel} title="取消">
          <X size={12} />
        </button>
      </span>
    );
  }

  return (
    <div
      className="group flex shrink-0 cursor-pointer items-center gap-1 rounded-md px-2.5 py-1 text-[12px] transition-colors"
      style={{
        background: active ? "var(--color-accent)" : "transparent",
        color: active ? "var(--color-accent-on-text)" : "var(--color-text-secondary)",
      }}
      onClick={onSelect}
      onMouseEnter={() => setHovering(true)}
      onMouseLeave={() => setHovering(false)}
      title={entry.name}
    >
      <span className="max-w-[10rem] truncate">{entry.name}</span>
      {hovering && (
        <span className="ml-0.5 flex items-center gap-0.5" onClick={(e) => e.stopPropagation()}>
          <button
            onClick={onStartRename}
            className="rounded p-0.5 hover:opacity-70"
            title="重命名"
          >
            <Pencil size={11} />
          </button>
          <button
            onClick={onStartDelete}
            className="rounded p-0.5 hover:opacity-70"
            title="删除"
          >
            <Trash2 size={11} />
          </button>
        </span>
      )}
    </div>
  );
}
