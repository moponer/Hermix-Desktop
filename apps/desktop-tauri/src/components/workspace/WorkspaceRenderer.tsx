/**
 * WorkspaceRenderer — 工作台 schema 渲染器（M1.4/M1.5）。
 *
 * 读 `WorkspaceDetail.schema.modules` 渲染模块导航 + 当前模块内容：
 * - table 模块 → TableView（含行增删改，能力来自 perms × 回调组合）
 * - dashboard 模块 → DashboardView（react-grid-layout 布局容器 +
 *   widgetData 数据注入）
 *
 * 纯函数式渲染：schema 即 props，布局/模块变更由上层更新 schema 后
 * 整体重渲染，本组件无内部业务状态依赖。工作台切换时由父层以
 * key={workspaceId} 重挂载，模块选中态不跨工作台漂移。
 */

import { useState } from "react";
import type { Layout } from "react-grid-layout";
import type { WorkspaceDetail, WorkspaceModule, WorkspaceSchema } from "../../types";
import { TableView, type WorkspacePerms } from "./TableView";
import { DashboardView } from "./DashboardView";

/** 解析权限视图（M7.3 契约）：缺省 = 可读写 + 允许删除（与后端一致）。 */
export function workspacePermissions(schema: WorkspaceSchema): WorkspacePerms {
  const p = schema.permissions;
  const canWrite = p?.mode !== "readonly";
  return { canWrite, canDelete: canWrite && p?.allow_delete !== false };
}

interface WorkspaceRendererProps {
  detail: WorkspaceDetail;
  /** 各表行数据（M1.6 起由 REST 查询填充）。 */
  tableRows?: Record<string, Record<string, unknown>[]>;
  /** 各表服务端总行数（截断提示用）。 */
  tableCounts?: Record<string, number | null>;
  loading?: boolean;
  /** 布局变更回调（持久化回后端 layout 层）。 */
  onLayoutChange?: (moduleId: string, layout: Layout) => void;
  /** 权限（缺省按 schema.permissions 解析）。 */
  perms?: WorkspacePerms;
  /** 仪表盘组件数据：widgetId → 数据（stat=数值 / list=行数组）。 */
  widgetData?: Record<string, unknown>;
  onInsert?: (moduleId: string, values: Record<string, unknown>) => Promise<void>;
  onUpdate?: (moduleId: string, rowId: string, patch: Record<string, unknown>) => Promise<void>;
  onDeleteRow?: (moduleId: string, rowId: string) => Promise<void>;
  /** Relation 字段候选：目标表 id → 行清单。 */
  relationCandidates?: Record<string, { id: string; label: string }[]>;
  /** 查询回调（P1）：关键字/页码变化透传给 TableView，上层拉数据。 */
  onQuery?: (moduleId: string, keyword: string, page: number) => void;
  /** 分页大小（P1）。 */
  pageSize?: number;
  /** 工作台 id（P5 附件预览/上传需要）。 */
  workspaceId?: string;
}

export function WorkspaceRenderer({
  detail,
  tableRows = {},
  tableCounts = {},
  loading = false,
  onLayoutChange,
  perms,
  widgetData = {},
  onInsert,
  onUpdate,
  onDeleteRow,
  relationCandidates,
  onQuery,
  pageSize,
  workspaceId,
}: WorkspaceRendererProps) {
  const modules = detail.schema.modules ?? [];
  // 只随 mount 取初值；工作台切换靠父层 key 重挂载复位
  const [activeId, setActiveId] = useState<string | null>(modules[0]?.id ?? null);
  const active = modules.find((m) => m.id === activeId) ?? modules[0] ?? null;

  if (modules.length === 0) {
    return (
      <div
        className="flex flex-1 items-center justify-center rounded-lg border border-dashed text-[12px]"
        style={{ borderColor: "var(--color-border-secondary)", color: "var(--color-text-faint)" }}
      >
        该工作台还没有模块 —— 在对话中告诉 Hermix「新增一个 XX 模块」即可
      </div>
    );
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-2">
      {/* 模块导航 */}
      <div
        className="flex shrink-0 items-center gap-1 overflow-x-auto rounded-lg border px-2 py-1"
        style={{ borderColor: "var(--color-border-secondary)", background: "var(--color-bg-elevated)" }}
      >
        {modules.map((m) => (
          <ModuleTab
            key={m.id}
            module={m}
            active={m.id === active?.id}
            onClick={() => setActiveId(m.id)}
          />
        ))}
      </div>

      {/* 当前模块内容 */}
      <div className="min-h-0 flex-1">
        {active ? (
          <ModuleContent
            module={active}
            rows={tableRows[active.id] ?? []}
            totalCount={tableCounts[active.id] ?? null}
            loading={loading}
            perms={perms ?? workspacePermissions(detail.schema)}
            widgetData={widgetData}
            relationCandidates={relationCandidates}
            layout={(detail.schema.layout as Record<string, Layout> | undefined)?.[active.id]}
            onLayoutChange={
              onLayoutChange
                ? (l) => onLayoutChange(active.id, l)
                : undefined
            }
            onInsert={onInsert}
            onUpdate={onUpdate}
            onDeleteRow={onDeleteRow}
            onQuery={onQuery ? (keyword, page) => onQuery(active.id, keyword, page) : undefined}
            pageSize={pageSize}
            workspaceId={workspaceId}
          />
        ) : null}
      </div>
    </div>
  );
}

function ModuleTab({
  module,
  active,
  onClick,
}: {
  module: WorkspaceModule;
  active: boolean;
  onClick: () => void;
}) {
  const icon = module.type === "table" ? "🗂️" : "📊";
  return (
    <button
      onClick={onClick}
      className="flex shrink-0 items-center gap-1 rounded-md px-2.5 py-1 text-[12px] transition-colors hover:opacity-80"
      style={{
        background: active ? "var(--color-accent)" : "transparent",
        color: active ? "var(--color-accent-on-text)" : "var(--color-text-secondary)",
      }}
      title={module.name}
    >
      <span>{icon}</span>
      <span className="max-w-[12rem] truncate">{module.name}</span>
    </button>
  );
}

function ModuleContent({
  module,
  rows,
  totalCount,
  loading,
  perms,
  widgetData,
  relationCandidates,
  layout,
  onLayoutChange,
  onInsert,
  onUpdate,
  onDeleteRow,
  onQuery,
  pageSize,
  workspaceId,
}: {
  module: WorkspaceModule;
  rows: Record<string, unknown>[];
  totalCount: number | null;
  loading: boolean;
  perms: WorkspacePerms;
  widgetData: Record<string, unknown>;
  relationCandidates?: Record<string, { id: string; label: string }[]>;
  layout?: Layout;
  onLayoutChange?: (l: Layout) => void;
  onInsert?: (moduleId: string, values: Record<string, unknown>) => Promise<void>;
  onUpdate?: (moduleId: string, rowId: string, patch: Record<string, unknown>) => Promise<void>;
  onDeleteRow?: (moduleId: string, rowId: string) => Promise<void>;
  onQuery?: (keyword: string, page: number) => void;
  pageSize?: number;
  workspaceId?: string;
}) {
  if (module.type === "table") {
    return (
      <TableView
        module={module}
        rows={rows}
        loading={loading}
        totalCount={totalCount}
        perms={perms}
        onInsert={onInsert}
        onUpdate={onUpdate}
        onDeleteRow={onDeleteRow}
        relationCandidates={relationCandidates}
        onQuery={onQuery}
        pageSize={pageSize}
        workspaceId={workspaceId}
      />
    );
  }
  // dashboard 模块 — react-grid-layout 布局容器 + 组件数据注入
  return (
    <DashboardView
      module={module}
      layout={layout}
      onLayoutChange={onLayoutChange}
      widgetData={widgetData}
    />
  );
}
