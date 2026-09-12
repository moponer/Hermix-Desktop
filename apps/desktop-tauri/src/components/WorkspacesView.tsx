/**
 * WorkspacesView — 工作台画布（M1.3 起）。
 *
 * 画布语义：这里只渲染 LLM 造出来的工作台内容，自身不带任何管理菜单。
 * 顶部一行是工作台名称 tab（没有工作台时显示提示），点 tab 切换画布；
 * 新建 / 重命名 / 删除 / 模板 / 导入 / 导出 / 提醒 / 设置 全部收在
 * 左侧栏「工作台」二级菜单，状态经 useWorkspaceRegistry 共享。
 *
 * 本组件只负责：把激活工作台的派生数据（表行 / 计数 / 组件数据）拉齐，
 * 交给 WorkspaceRenderer 渲染，并把行级写操作回调下去。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Loader2, LayoutDashboard, Lock } from "lucide-react";
import type { Layout } from "react-grid-layout";
import * as echarts from "echarts";
import * as api from "../api";
import type { WorkspaceModule } from "../types";
import { toastError, toastSuccess } from "../hooks/useToast";
import { WorkspaceRenderer, workspacePermissions } from "./workspace/WorkspaceRenderer";
import {
  applyWorkspaceDetail,
  setActiveWorkspace,
  useWorkspaceRegistry,
} from "../hooks/useWorkspaceRegistry";

/** 每表默认查询上限（防大表拖垮前端）。 */
const TABLE_QUERY_LIMIT = 500;
/** 布局拖拽持久化防抖（ms）——react-grid-layout 拖拽过程频繁回调。 */
const LAYOUT_SAVE_DEBOUNCE_MS = 500;
/** 行标签展示时排除的系统列。 */
const ROW_SYS_KEYS = new Set(["id", "created_at", "updated_at", "updated_by"]);

function tableModules(schema: { modules?: WorkspaceModule[] }): Extract<WorkspaceModule, { type: "table" }>[] {
  return (schema.modules ?? []).filter(
    (m): m is Extract<WorkspaceModule, { type: "table" }> => m.type === "table",
  );
}

function dashboardModules(schema: { modules?: WorkspaceModule[] }): Extract<WorkspaceModule, { type: "dashboard" }>[] {
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

/**
 * list 组件受控查询解析：`list [from <表>] [limit <N>]`（default 50，hard cap 500）。
 * 走现有 workspaceQueryTable（filters 留空），表未指定时用 schema 第一个表。
 */
export function parseListWidgetQuery(
  query: string,
): { table?: string; limit: number } | null {
  const m = /^list(?:\s+from\s+(\w+))?(?:\s+limit\s+(\d+))?$/i.exec(query.trim());
  if (!m) return null;
  const limit = Math.min(500, Math.max(1, m[2] ? Number(m[2]) : 50));
  return { table: m[1], limit };
}

/** 支持的图表类型（ECharts 已装，按类型构造 option，无需新依赖）。 */
export const CHART_TYPES = [
  "bar",
  "line",
  "pie",
  "area",
  "funnel",
  "gauge",
  "scatter",
] as const;
export type ChartType = (typeof CHART_TYPES)[number];

/**
 * chart 组件受控查询解析：`chart [<类型>] [from <表>] group by <字段> [limit <N>]`
 * 类型 ∈ bar/line/pie/area/funnel/gauge/scatter，缺省 bar（兼容旧语法）。
 *  - group by 缺省 → 用表第一列文本字段（fallback）：schema 驱动表通常首个
 *    文本字段就是分类轴，LLM 不显式指 group_by 时也能出图。
 *  - 表未指定 → 用 schema 第一个表。
 */
export function parseChartWidgetQuery(
  query: string,
): { table?: string; groupBy: string | null; limit: number; chartType: ChartType } | null {
  const m =
    /^chart(?:\s+(bar|line|pie|area|funnel|gauge|scatter))?(?:\s+from\s+(\w+))?(?:\s+group\s+by\s+(\w+))?(?:\s+limit\s+(\d+))?$/i.exec(
      query.trim(),
    );
  if (!m) return null;
  const limit = Math.min(100, Math.max(1, m[4] ? Number(m[4]) : 20));
  const chartType = ((m[1]?.toLowerCase() as ChartType | undefined) ?? "bar") as ChartType;
  return { table: m[2], groupBy: m[3] ?? null, limit, chartType };
}

/**
 * chart 组件的最小 ECharts option——分组计数 → 横轴=label，纵轴=count 柱状图。
 * 主题自适应（dark/light 留 ChartWidget 内置主题切换处理）。
 */
function buildBarOption(
  title: string,
  groups: { label: string; count: number }[],
): echarts.EChartsOption {
  return {
    grid: { left: 36, right: 12, top: 16, bottom: 36, containLabel: true },
    tooltip: { trigger: "axis", axisPointer: { type: "shadow" } },
    xAxis: {
      type: "category",
      data: groups.map((g) => g.label),
      axisLabel: { interval: 0, rotate: groups.length > 6 ? 30 : 0, fontSize: 11 },
    },
    yAxis: { type: "value", minInterval: 1, axisLabel: { fontSize: 11 } },
    series: [
      {
        name: title,
        type: "bar",
        data: groups.map((g) => g.count),
        barMaxWidth: 32,
      },
    ],
  };
}

/**
 * 分组计数 → ECharts option，按图表类型分发（组件目录的表达力来源）。
 *
 * 数据形态统一是 `{label, count}[]`（后端 group_count 输出），各类型只是呈现
 * 方式不同：`gauge` 取总和（单个指针，分组无意义）；`pie`/`funnel` 关闭 label
 * 靠 tooltip，因仪表盘卡片高度有限（默认 4 行 ≈ 120px）放不下图例。
 */
export function buildChartOption(
  title: string,
  groups: { label: string; count: number }[],
  type: ChartType = "bar",
): echarts.EChartsOption {
  const labels = groups.map((g) => g.label);
  const values = groups.map((g) => g.count);
  const cartesian = {
    grid: { left: 36, right: 12, top: 16, bottom: 36, containLabel: true },
    tooltip: { trigger: "axis" as const, axisPointer: { type: "shadow" as const } },
    xAxis: {
      type: "category" as const,
      data: labels,
      axisLabel: { interval: 0, rotate: labels.length > 6 ? 30 : 0, fontSize: 11 },
    },
    yAxis: { type: "value" as const, minInterval: 1, axisLabel: { fontSize: 11 } },
  };

  switch (type) {
    case "line":
      return { ...cartesian, series: [{ name: title, type: "line", smooth: true, data: values }] };
    case "area":
      return {
        ...cartesian,
        series: [{ name: title, type: "line", smooth: true, data: values, areaStyle: {} }],
      };
    case "pie":
      return {
        tooltip: { trigger: "item" as const },
        series: [
          {
            name: title,
            type: "pie",
            radius: ["40%", "66%"],
            data: groups.map((g) => ({ name: g.label, value: g.count })),
            label: { show: false },
          },
        ],
      };
    case "funnel":
      return {
        tooltip: { trigger: "item" as const },
        series: [
          {
            name: title,
            type: "funnel",
            top: 8,
            bottom: 8,
            data: groups.map((g) => ({ name: g.label, value: g.count })),
            label: { fontSize: 11 },
          },
        ],
      };
    case "gauge": {
      const total = values.reduce((s, n) => s + n, 0);
      return {
        series: [
          {
            type: "gauge",
            min: 0,
            max: Math.max(total, 1) * 1.2,
            startAngle: 210,
            endAngle: -30,
            detail: { fontSize: 18, offsetCenter: [0, "62%"] },
            title: { fontSize: 11, offsetCenter: [0, "86%"] },
            data: [{ value: total, name: title }],
          },
        ],
      };
    }
    case "scatter":
      return {
        grid: cartesian.grid,
        tooltip: { trigger: "item" as const },
        xAxis: cartesian.xAxis,
        yAxis: cartesian.yAxis,
        series: [
          {
            name: title,
            type: "scatter",
            symbolSize: 12,
            data: values.map((v, i) => [i, v]),
          },
        ],
      };
    default:
      return buildBarOption(title, groups);
  }
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
  const { entries, activeId, detail, loading, revision } = useWorkspaceRegistry();
  const [tableRows, setTableRows] = useState<Record<string, Record<string, unknown>[]>>({});
  /** 各表服务端总行数（rows 超 500 截断时的提示依据）。 */
  const [tableCounts, setTableCounts] = useState<Record<string, number | null>>({});
  /** 仪表盘组件数据：widgetId → 数据（stat=数值 / list=行数组）。 */
  const [widgetData, setWidgetData] = useState<Record<string, unknown>>({});
  /** 仪表盘 chart 组件 ECharts option：widgetId → option。 */
  const [widgetChartOptions, setWidgetChartOptions] = useState<
    Record<string, echarts.EChartsOption>
  >({});
  // 布局保存防抖 timer
  const layoutTimer = useRef<number | null>(null);
  // 数据加载只读最新详情，但不应因 schema 本地更新（拖拽布局）而重跑
  const detailRef = useRef(detail);
  detailRef.current = detail;
  const detailId = detail?.id ?? null;

  // 工作台切换（或侧栏写操作 bump revision）→ 重拉派生数据
  useEffect(() => {
    const current = detailRef.current;
    if (!activeId || !current || current.id !== activeId) {
      setTableRows({});
      setTableCounts({});
      setWidgetData({});
      setWidgetChartOptions({});
      return;
    }
    let cancelled = false;
    const tables = tableModules(current.schema);
    void Promise.all([
      // 行数据（各表独立上限，失败逐表提示不阻塞其余）
      ...tables.map(async (t) => {
        try {
          const rows = await api.workspaceQueryTable(activeId, t.id, { limit: TABLE_QUERY_LIMIT });
          if (!cancelled) setTableRows((prev) => ({ ...prev, [t.id]: rows }));
        } catch (e) {
          if (!cancelled) toastError(`加载表「${t.name}」失败: ${String(e)}`);
        }
      }),
      // 表计数（仅作截断提示，失败降级为未知）
      ...tables.map(async (t) => {
        try {
          const n = await api.workspaceCountRows(activeId, t.id);
          if (!cancelled) setTableCounts((prev) => ({ ...prev, [t.id]: n }));
        } catch {
          /* 提示性数据，忽略 */
        }
      }),
      // stat 组件计数（受控查询 count[from T][where f=v]，未识别的 DSL 留空）
      ...dashboardModules(current.schema).flatMap((dash) =>
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
                q.field ? [{ field: q.field, op: "eq", value: q.value ?? "" }] : [],
              );
              if (!cancelled) setWidgetData((prev) => ({ ...prev, [w.id]: n }));
            } catch {
              /* 组件留空态 */
            }
          }),
      ),
      // list 组件数据（受控查询 list [from T] [limit N]，未识别 DSL 留空）
      ...dashboardModules(current.schema).flatMap((dash) =>
        dash.widgets
          .filter((w) => w.type === "list")
          .map(async (w) => {
            const q = parseListWidgetQuery(w.query);
            if (!q) return;
            const table = q.table ?? tables[0]?.id;
            if (!table) return;
            try {
              const rows = await api.workspaceQueryTable(activeId, table, { limit: q.limit });
              if (!cancelled) setWidgetData((prev) => ({ ...prev, [w.id]: rows }));
            } catch {
              /* 组件留空态 */
            }
          }),
      ),
      // chart 组件数据（受控查询 chart [from T] group by <f> [limit N]）
      ...dashboardModules(current.schema).flatMap((dash) =>
        dash.widgets
          .filter((w) => w.type === "chart")
          .map(async (w) => {
            const q = parseChartWidgetQuery(w.query);
            if (!q) return;
            const tableId = q.table ?? tables[0]?.id;
            const table = tables.find((t) => t.id === tableId);
            if (!table) return;
            // group by 缺省 → 表第一列文本字段（schema 驱动表通常首个文本字段就是分类轴）
            const groupBy = q.groupBy ?? table.fields.find((f) => f.type === "text")?.id;
            if (!groupBy) return;
            try {
              const { groups } = await api.workspaceGroupCount(activeId, table.id, groupBy, q.limit);
              if (!cancelled) {
                setWidgetChartOptions((prev) => ({
                  ...prev,
                  [w.id]: buildChartOption(w.title, groups, q.chartType),
                }));
              }
            } catch {
              /* 组件留空态 */
            }
          }),
      ),
    ]);
    return () => {
      cancelled = true;
    };
  }, [activeId, detailId, revision]);

  // 组件卸载时清理布局保存 timer
  useEffect(() => {
    return () => {
      if (layoutTimer.current !== null) window.clearTimeout(layoutTimer.current);
    };
  }, []);

  /** 布局拖拽/缩放 → 更新本地 schema 并防抖持久化回后端（仅 layout 层，
   *  避免整 schema 快照覆盖 LLM 并发的模块/字段修改）。 */
  const handleLayoutChange = useCallback(
    (moduleId: string, layout: Layout) => {
      if (!detail) return;
      const prevLayout =
        detail.schema.layout && typeof detail.schema.layout === "object"
          ? (detail.schema.layout as Record<string, Layout>)
          : {};
      const nextLayout = { ...prevLayout, [moduleId]: layout };
      applyWorkspaceDetail({ ...detail, schema: { ...detail.schema, layout: nextLayout } });
      if (layoutTimer.current !== null) window.clearTimeout(layoutTimer.current);
      layoutTimer.current = window.setTimeout(() => {
        void api
          .updateWorkspaceLayout(detail.id, nextLayout)
          .catch((e) => toastError(`保存布局失败: ${String(e)}`));
      }, LAYOUT_SAVE_DEBOUNCE_MS);
    },
    [detail],
  );

  const perms = useMemo(
    () => (detail ? workspacePermissions(detail.schema) : { canWrite: true, canDelete: true }),
    [detail],
  );

  /** 写操作后重拉单表：行数据 + 计数 + 引用该表的 stat 组件。 */
  const reloadTable = useCallback(
    async (tableId: string) => {
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
    },
    [activeId, detail],
  );

  // P1：关键字/页码查询——TableView 搜索框/分页触发，服务端 limit/offset 拉取
  const PAGE_SIZE = 100;
  const handleTableQuery = useCallback(
    async (tableId: string, keyword: string, page: number) => {
      if (!activeId) return;
      try {
        const rows = await api.workspaceQueryTable(activeId, tableId, {
          keyword: keyword || undefined,
          limit: PAGE_SIZE,
          offset: page * PAGE_SIZE,
        });
        setTableRows((prev) => ({ ...prev, [tableId]: rows }));
      } catch (e) {
        toastError(`搜索失败: ${String(e)}`);
      }
      try {
        const n = await api.workspaceCountRows(activeId, tableId, [], keyword || undefined);
        setTableCounts((prev) => ({ ...prev, [tableId]: n }));
      } catch {
        /* 提示性计数，忽略 */
      }
    },
    [activeId],
  );

  // ── 行 CRUD ——错误 toast 后向上抛，FormView 保持表单开启 ──
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

  return (
    <div className="flex h-full flex-col px-4 py-3">
      {/* ── Tab 栏：只有工作台名称；管理菜单在左侧栏 ── */}
      <div
        className="mb-3 flex shrink-0 items-center gap-1.5 overflow-x-auto rounded-lg border px-2 py-1.5"
        style={{
          borderColor: "var(--color-border-secondary)",
          background: "var(--color-bg-elevated)",
        }}
      >
        {entries.length === 0 ? (
          <span className="text-[12px]" style={{ color: "var(--color-text-faint)" }}>
            还没有工作台 —— 在左侧栏「工作台」菜单新建
          </span>
        ) : (
          entries.map((e) => (
            <button
              key={e.id}
              onClick={() => setActiveWorkspace(e.id)}
              className="flex shrink-0 items-center rounded-md px-2.5 py-1 text-[12px] transition-colors"
              style={{
                background: e.id === activeId ? "var(--color-accent)" : "transparent",
                color: e.id === activeId ? "var(--color-accent-on-text)" : "var(--color-text-secondary)",
              }}
              title={e.name}
            >
              <span className="max-w-[12rem] truncate">{e.name}</span>
            </button>
          ))
        )}
        {detail && !perms.canWrite && (
          <span
            className="ml-auto shrink-0 rounded-md border px-1.5 py-0.5 text-[11px]"
            title="该工作台为只读（permissions.mode=readonly），不能增删改数据"
            style={{ borderColor: "var(--color-border-secondary)", color: "var(--color-text-faint)" }}
          >
            <Lock size={10} className="inline-block align-[-1px] mr-0.5" />只读
          </span>
        )}
        {loading && !detail && entries.length > 0 && (
          <Loader2 className="icon-md animate-spin" style={{ color: "var(--color-text-faint)" }} />
        )}
      </div>

      {/* ── 画布 ── */}
      {detail ? (
        /* key=activeId：工作台切换时重挂载，模块选中态不跨工作台漂移 */
        <WorkspaceRenderer
          key={detail.id}
          detail={detail}
          tableRows={tableRows}
          tableCounts={tableCounts}
          perms={perms}
          widgetData={widgetData}
          widgetChartOptions={widgetChartOptions}
          relationCandidates={relationCandidates}
          onLayoutChange={handleLayoutChange}
          onInsert={perms.canWrite ? handleInsert : undefined}
          onUpdate={perms.canWrite ? handleUpdateRow : undefined}
          onDeleteRow={handleDeleteRow}
          onQuery={handleTableQuery}
          pageSize={PAGE_SIZE}
          workspaceId={detail.id}
        />
      ) : (
        <div
          className="flex flex-1 items-center justify-center rounded-lg border border-dashed"
          style={{ borderColor: "var(--color-border-secondary)" }}
        >
          {entries.length === 0 ? (
            <div className="flex flex-col items-center gap-2 px-6 py-8 text-center">
              <LayoutDashboard size={28} style={{ color: "var(--color-text-faint)" }} />
              <div className="text-[14px] font-medium" style={{ color: "var(--color-text-primary)" }}>
                创建你的第一个工作台
              </div>
              <div className="max-w-xs text-[12px] leading-relaxed" style={{ color: "var(--color-text-faint)" }}>
                在左侧栏「工作台」菜单新建空白工作台或从台账模板开始；也可以在对话中描述需求，AI 会替你建设
              </div>
            </div>
          ) : (
            <span className="text-[13px]" style={{ color: "var(--color-text-faint)" }}>
              加载中…
            </span>
          )}
        </div>
      )}
    </div>
  );
}
