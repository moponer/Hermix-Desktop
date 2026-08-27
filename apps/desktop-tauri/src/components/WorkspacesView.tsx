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

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Plus,
  Pencil,
  Trash2,
  Loader2,
  LayoutDashboard,
  LayoutGrid,
  X,
} from "lucide-react";
import type { Layout } from "react-grid-layout";
import * as api from "../api";
import type {
  WorkspaceDetail,
  WorkspaceEntry,
  WorkspaceModule,
  TemplateMeta,
} from "../types";
import { toastError, toastSuccess } from "../hooks/useToast";
import { WorkspaceRenderer, workspacePermissions } from "./workspace/WorkspaceRenderer";

type TabMode = "idle" | "creating" | "renaming" | "confirm-delete";

/** 每表默认查询上限（防大表拖垮前端）。 */
const TABLE_QUERY_LIMIT = 500;
/** 布局拖拽持久化防抖（ms）——react-grid-layout 拖拽过程频繁回调。 */
const LAYOUT_SAVE_DEBOUNCE_MS = 500;
/** 行标签展示时排除的系统列。 */
const ROW_SYS_KEYS = new Set(["id", "created_at", "updated_at", "updated_by"]);

function tableModules(schema: WorkspaceDetail["schema"]): Extract<WorkspaceModule, { type: "table" }>[] {
  return (schema.modules ?? []).filter(
    (m): m is Extract<WorkspaceModule, { type: "table" }> => m.type === "table",
  );
}

function dashboardModules(schema: WorkspaceDetail["schema"]): Extract<WorkspaceModule, { type: "dashboard" }>[] {
  return (schema.modules ?? []).filter(
    (m): m is Extract<WorkspaceModule, { type: "dashboard" }> => m.type === "dashboard",
  );
}

/**
 * stat 卡受控查询的最小解析器：`count` / `count from <表>` /
 * `count [from <表>] where <字段>=<值>`。
 * 其余 DSL 形态后端尚无白名单解释器，返回 null 保持组件空态——不猜语义。
 */
export function parseCountStatQuery(
  query: string,
): { table?: string; field?: string; value?: string } | null {
  const m = /^count(?:\s+from\s+(\w+))?(?:\s+where\s+(\w+)\s*=\s*(.+))?$/i.exec(query.trim());
  if (!m) return null;
  const value = m[3]?.trim().replace(/^['"](.*)['"]$/, "$1");
  return { table: m[1], field: m[2], value };
}

/** Relation 下拉的行展示标签：常见命名字段优先，退化为首个非系统文本列。 */
function rowLabel(row: Record<string, unknown>): string {
  for (const key of ["name", "title"]) {
    if (typeof row[key] === "string" && row[key]) return row[key] as string;
  }
  for (const [k, v] of Object.entries(row)) {
    if (ROW_SYS_KEYS.has(k)) continue;
    if (typeof v === "string" && v) return v;
  }
  return String(row["id"] ?? "");
}

export function WorkspacesView() {
  const [entries, setEntries] = useState<WorkspaceEntry[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [detail, setDetail] = useState<WorkspaceDetail | null>(null);
  const [tableRows, setTableRows] = useState<Record<string, Record<string, unknown>[]>>({});
  /** 各表服务端总行数（rows 超 500 截断时的提示依据）。 */
  const [tableCounts, setTableCounts] = useState<Record<string, number | null>>({});
  /** 仪表盘组件数据：widgetId → 数据（当前支持 stat 计数）。 */
  const [widgetData, setWidgetData] = useState<Record<string, unknown>>({});
  const [loading, setLoading] = useState(true);
  const [mode, setMode] = useState<TabMode>("idle");
  const [draft, setDraft] = useState("");
  // 记录正在操作的 tab（renaming/confirm-delete 的目标）
  const [actingId, setActingId] = useState<string | null>(null);
  // 布局保存防抖 timer
  const layoutTimer = useRef<number | null>(null);
  // 模板选择（M6.5）：懒加载一次 + 面板开关
  const [templates, setTemplates] = useState<TemplateMeta[] | null>(null);
  const [showTemplates, setShowTemplates] = useState(false);
  const [creatingTemplate, setCreatingTemplate] = useState<string | null>(null);

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

  // activeId 变化 → 拉详情 + 各表行数据 + 计数 + stat 组件计数
  useEffect(() => {
    if (!activeId) {
      setDetail(null);
      setTableRows({});
      setTableCounts({});
      setWidgetData({});
      return;
    }
    let cancelled = false;
    api
      .getWorkspace(activeId)
      .then(async (d) => {
        if (cancelled) return;
        setDetail(d);
        const tables = tableModules(d.schema);
        await Promise.all([
          // 行数据（各表独立上限，失败逐表提示不阻塞其余）
          Promise.all(
            tables.map(async (t) => {
              try {
                const rows = await api.workspaceQueryTable(activeId, t.id, {
                  limit: TABLE_QUERY_LIMIT,
                });
                if (cancelled) return;
                setTableRows((prev) => ({ ...prev, [t.id]: rows }));
              } catch (e) {
                if (!cancelled) toastError(`加载表「${t.name}」失败: ${String(e)}`);
              }
            }),
          ),
          // 表计数（仅作截断提示，失败降级为未知）
          Promise.all(
            tables.map(async (t) => {
              try {
                const n = await api.workspaceCountRows(activeId, t.id);
                if (cancelled) return;
                setTableCounts((prev) => ({ ...prev, [t.id]: n }));
              } catch {
                /* 提示性数据，忽略 */
              }
            }),
          ),
          // stat 组件计数（受控查询 count[from T][where f=v]，未识别的 DSL 留空）
          Promise.all(
            dashboardModules(d.schema).flatMap((dash) =>
              dash.widgets
                .filter((w) => w.type === "stat")
                .map(async (w) => {
                  const q = parseCountStatQuery(w.query);
                  if (!q) return;
                  const table = q.table ?? tables[0]?.id;
                  if (!table) return;
                  try {
                    const n = await api.workspaceCountRows(
                      activeId,
                      table,
                      q.field
                        ? [{ field: q.field, op: "eq", value: q.value ?? "" }]
                        : [],
                    );
                    if (cancelled) return;
                    setWidgetData((prev) => ({ ...prev, [w.id]: n }));
                  } catch {
                    /* 组件留空态 */
                  }
                }),
            ),
          ),
        ]);
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

  /** 布局拖拽/缩放 → 更新本地 schema 并防抖持久化回后端（仅 layout 层，
   *  避免整 schema 快照覆盖 LLM 并发的模块/字段修改）。 */
  const handleLayoutChange = useCallback((moduleId: string, layout: Layout) => {
    setDetail((prev) => {
      if (!prev) return prev;
      const prevLayout =
        prev.schema.layout && typeof prev.schema.layout === "object"
          ? (prev.schema.layout as Record<string, Layout>)
          : {};
      const nextLayout = { ...prevLayout, [moduleId]: layout };
      const schema = { ...prev.schema, layout: { ...nextLayout } };
      if (layoutTimer.current !== null) window.clearTimeout(layoutTimer.current);
      layoutTimer.current = window.setTimeout(() => {
        void api
          .updateWorkspaceLayout(prev.id, nextLayout)
          .catch((e) => toastError(`保存布局失败: ${String(e)}`));
      }, LAYOUT_SAVE_DEBOUNCE_MS);
      return { ...prev, schema };
    });
  }, []);

  const perms = useMemo(
    () => (detail ? workspacePermissions(detail.schema) : { canWrite: true, canDelete: true }),
    [detail],
  );

  /** 写操作后重拉单表：行数据 + 计数 + 引用该表的 stat 组件。 */
  const reloadTable = useCallback(async (tableId: string) => {
    if (!activeId || !detail) return;
    try {
      const rows = await api.workspaceQueryTable(activeId, tableId, { limit: TABLE_QUERY_LIMIT });
      setTableRows((prev) => ({ ...prev, [tableId]: rows }));
    } catch (e) {
      toastError(`刷新表数据失败: ${String(e)}`);
    }
    try {
      const n = await api.workspaceCountRows(activeId, tableId);
      setTableCounts((prev) => ({ ...prev, [tableId]: n }));
    } catch {
      /* 提示性计数，忽略 */
    }
    for (const dash of dashboardModules(detail.schema)) {
      for (const w of dash.widgets) {
        if (w.type !== "stat") continue;
        const q = parseCountStatQuery(w.query);
        if (!q || (q.table ?? tableModules(detail.schema)[0]?.id) !== tableId) continue;
        try {
          const n = await api.workspaceCountRows(
            activeId,
            tableId,
            q.field ? [{ field: q.field, op: "eq", value: q.value ?? "" }] : [],
          );
          setWidgetData((prev) => ({ ...prev, [w.id]: n }));
        } catch {
          /* 组件留空态 */
        }
      }
    }
  }, [activeId, detail]);

  // ── 行 CRUD（M7 前端接通）——错误 toast 后向上抛，FormView 保持表单开启 ──
  const handleInsert = useCallback(
    async (moduleId: string, values: Record<string, unknown>) => {
      if (!activeId) return;
      try {
        await api.workspaceInsertRows(activeId, moduleId, [values], "user");
        toastSuccess("已新增");
      } catch (e) {
        toastError(`新增失败: ${String(e)}`);
        throw e;
      }
      await reloadTable(moduleId);
    },
    [activeId, reloadTable],
  );

  const handleUpdateRow = useCallback(
    async (moduleId: string, rowId: string, patch: Record<string, unknown>) => {
      if (!activeId) return;
      try {
        await api.workspaceUpdateRow(activeId, moduleId, rowId, patch, "user");
        toastSuccess("已保存");
      } catch (e) {
        toastError(`保存失败: ${String(e)}`);
        throw e;
      }
      await reloadTable(moduleId);
    },
    [activeId, reloadTable],
  );

  /** 删除永不 reject（调用方是 fire-and-forget 按钮）。 */
  const handleDeleteRow = useCallback(
    async (moduleId: string, rowId: string) => {
      if (!activeId) return;
      try {
        await api.workspaceDeleteRow(activeId, moduleId, rowId);
        toastSuccess("已删除");
        await reloadTable(moduleId);
      } catch (e) {
        toastError(`删除失败: ${String(e)}`);
      }
    },
    [activeId, reloadTable],
  );

  /** Relation 下拉候选：已加载的各表行 → {id, label}。 */
  const relationCandidates = useMemo(() => {
    const out: Record<string, { id: string; label: string }[]> = {};
    for (const [tid, rows] of Object.entries(tableRows)) {
      out[tid] = rows
        .slice(0, 200)
        .map((r) => ({ id: String(r["id"] ?? ""), label: rowLabel(r) }))
        .filter((c) => c.id !== "");
    }
    return out;
  }, [tableRows]);

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

  // 打开模板面板（懒加载一次）。toggle 语义只用于顶栏按钮；空态引导用
  // showTemplatePanel 强制打开，避免「开着再点反而关闭」。
  const showTemplatePanel = useCallback(async () => {
    setShowTemplates(true);
    if (templates === null) {
      try {
        setTemplates(await api.listWorkspaceTemplates());
      } catch (e) {
        toastError(`加载模板失败: ${String(e)}`);
        setShowTemplates(false);
      }
    }
  }, [templates]);

  const toggleTemplatePanel = useCallback(() => {
    if (showTemplates) {
      setShowTemplates(false);
      return;
    }
    void showTemplatePanel();
  }, [showTemplates, showTemplatePanel]);

  // 按模板一键建工作台（复用 create_from_schema；成功后切到新 tab）
  const handleCreateFromTemplate = useCallback(
    async (tpl: TemplateMeta) => {
      if (creatingTemplate) return; // 防连点
      setCreatingTemplate(tpl.id);
      try {
        const { id, name, smoke } = await api.createWorkspaceFromTemplate(tpl.id);
        toastSuccess(`已按模板创建「${name}」（${smoke.length} 项自测通过）`);
        setShowTemplates(false);
        await refresh();
        setActiveId(id);
      } catch (e) {
        toastError(`按模板创建失败: ${String(e)}`);
      } finally {
        setCreatingTemplate(null);
      }
    },
    [creatingTemplate, refresh],
  );

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
        {detail && !perms.canWrite && (
          <span
            className="shrink-0 rounded-md border px-1.5 py-0.5 text-[11px]"
            title="该工作台为只读（permissions.mode=readonly），不能增删改数据"
            style={{ borderColor: "var(--color-border-secondary)", color: "var(--color-text-faint)" }}
          >
            🔒 只读
          </span>
        )}
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

        {/* 模板按钮（M6.5）：一键按预置台账创建 */}
        <div className="relative shrink-0">
          <button
            onClick={toggleTemplatePanel}
            className="flex items-center gap-1 rounded-md px-2 py-1 text-[12px] transition-opacity hover:opacity-80"
            style={{ color: "var(--color-text-secondary)" }}
            title="从模板创建工作台"
          >
            <LayoutGrid size={13} /> 模板
          </button>
          {showTemplates && (
            <div
              className="absolute right-0 top-full z-30 mt-1 w-72 rounded-lg border p-2 shadow-lg"
              style={{
                borderColor: "var(--color-border-secondary)",
                background: "var(--color-bg-elevated)",
              }}
              onMouseLeave={() => setShowTemplates(false)}
            >
              <div className="mb-1.5 text-[12px] font-medium" style={{ color: "var(--color-text-primary)" }}>
                预置工作台模板
              </div>
              {templates === null ? (
                <div className="flex items-center gap-1.5 py-2 text-[12px]" style={{ color: "var(--color-text-faint)" }}>
                  <Loader2 size={13} className="animate-spin" /> 加载中…
                </div>
              ) : templates.length === 0 ? (
                <div className="py-2 text-[12px]" style={{ color: "var(--color-text-faint)" }}>
                  暂无可用模板
                </div>
              ) : (
                <div className="flex max-h-72 flex-col gap-1.5 overflow-y-auto">
                  {templates.map((t) => (
                    <button
                      key={t.id}
                      disabled={creatingTemplate !== null}
                      onClick={() => void handleCreateFromTemplate(t)}
                      className="flex w-full flex-col gap-0.5 rounded-md border px-2.5 py-2 text-left transition-opacity hover:opacity-85 disabled:opacity-50"
                      style={{
                        borderColor: "var(--color-border-secondary)",
                        background: "var(--color-bg-base)",
                      }}
                    >
                      <span className="flex items-center gap-1.5 text-[12.5px] font-medium" style={{ color: "var(--color-text-primary)" }}>
                        {creatingTemplate === t.id ? (
                          <Loader2 size={12} className="animate-spin" />
                        ) : (
                          <LayoutGrid size={12} />
                        )}
                        {t.name}
                      </span>
                      <span className="text-[11.5px] leading-snug" style={{ color: "var(--color-text-secondary)" }}>
                        {t.description}
                      </span>
                      <span className="text-[11px]" style={{ color: "var(--color-text-faint)" }}>
                        {t.tables.map((tb) => tb.name).join(" · ")}
                      </span>
                    </button>
                  ))}
                </div>
              )}
            </div>
          )}
        </div>
      </div>

      {/* ── 内容区（M1.4 起由 WorkspaceRenderer 渲染 schema 模块） ── */}
      {detail ? (
        /* key=activeId：工作台切换时重挂载，模块选中态不跨工作台漂移 */
        <WorkspaceRenderer
          key={detail.id}
          detail={detail}
          tableRows={tableRows}
          tableCounts={tableCounts}
          perms={perms}
          widgetData={widgetData}
          relationCandidates={relationCandidates}
          onLayoutChange={handleLayoutChange}
          onInsert={perms.canWrite ? handleInsert : undefined}
          onUpdate={perms.canWrite ? handleUpdateRow : undefined}
          onDeleteRow={handleDeleteRow}
        />
      ) : (
        <div
          className="flex flex-1 items-center justify-center rounded-lg border border-dashed text-[13px]"
          style={{
            borderColor: "var(--color-border-secondary)",
            color: "var(--color-text-faint)",
          }}
        >
          {entries.length === 0 ? (
            /* M7.4 空工作台引导：从模板 or 对话创建 二选一 */
            <div className="flex flex-col items-center gap-3 px-6 py-8 text-center">
              <LayoutDashboard size={28} style={{ color: "var(--color-text-faint)" }} />
              <div>
                <div className="text-[13.5px] font-medium" style={{ color: "var(--color-text-primary)" }}>
                  创建你的第一个工作台
                </div>
                <div className="mt-1 max-w-xs text-[12px] leading-relaxed">
                  从预置台账模板一键开始，或在对话中描述需求，AI 会自动为你建设
                </div>
              </div>
              <div className="flex items-center gap-2">
                <button
                  onClick={() => void showTemplatePanel()}
                  className="flex items-center gap-1.5 rounded-md border px-3 py-1.5 text-[12.5px] transition-opacity hover:opacity-80"
                  style={{
                    borderColor: "var(--color-border-secondary)",
                    background: "var(--color-bg-elevated)",
                    color: "var(--color-text-primary)",
                  }}
                >
                  <LayoutGrid size={13} /> 从模板开始
                </button>
                <button
                  onClick={() => {
                    setMode("creating");
                    setActingId(null);
                    setDraft("");
                  }}
                  className="flex items-center gap-1.5 rounded-md border px-3 py-1.5 text-[12.5px] transition-opacity hover:opacity-80"
                  style={{
                    borderColor: "var(--color-border-secondary)",
                    color: "var(--color-text-secondary)",
                  }}
                >
                  <Plus size={13} /> 新建空白
                </button>
              </div>
            </div>
          ) : (
            "加载中…"
          )}
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
