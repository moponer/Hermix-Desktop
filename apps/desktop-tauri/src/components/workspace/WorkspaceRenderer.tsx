/**
 * WorkspaceRenderer — 工作台 schema 渲染器（M1.4/M1.5）。
 *
 * 读 `WorkspaceDetail.schema.modules` 渲染模块导航 + 当前模块内容：
 * - table 模块 → TableView（grid 视图；form/kanban/calendar 视图切换在
 *   M1.6+ 接入，M1.5 已备好 FormView 组件）
 * - dashboard 模块 → DashboardView（react-grid-layout 布局容器）
 *
 * 纯函数式渲染：schema 即 props，布局/模块变更由上层（对话建设管线）
 * 更新 schema 后整体重渲染，本组件无内部业务状态依赖。
 */

import { useState } from "react";
import type { Layout } from "react-grid-layout";
import type { WorkspaceDetail, WorkspaceModule } from "../../types";
import { TableView } from "./TableView";
import { DashboardView } from "./DashboardView";

interface WorkspaceRendererProps {
  detail: WorkspaceDetail;
  /** 各表行数据（M1.6 起由 REST 查询填充）。 */
  tableRows?: Record<string, Record<string, unknown>[]>;
  loading?: boolean;
  /** 布局变更回调（M1.6 持久化回后端）。 */
  onLayoutChange?: (moduleId: string, layout: Layout) => void;
}

export function WorkspaceRenderer({
  detail,
  tableRows = {},
  loading = false,
  onLayoutChange,
}: WorkspaceRendererProps) {
  const modules = detail.schema.modules ?? [];
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
            loading={loading}
            layout={(detail.schema.layout as Record<string, Layout> | undefined)?.[active.id]}
            onLayoutChange={
              onLayoutChange
                ? (l) => onLayoutChange(active.id, l)
                : undefined
            }
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
      title={module.id}
    >
      <span>{icon}</span>
      <span className="max-w-[12rem] truncate">{module.name}</span>
    </button>
  );
}

function ModuleContent({
  module,
  rows,
  loading,
  layout,
  onLayoutChange,
}: {
  module: WorkspaceModule;
  rows: Record<string, unknown>[];
  loading: boolean;
  layout?: Layout;
  onLayoutChange?: (l: Layout) => void;
}) {
  if (module.type === "table") {
    return <TableView module={module} rows={rows} loading={loading} />;
  }
  // dashboard 模块 — react-grid-layout 布局容器
  return (
    <DashboardView
      module={module}
      layout={layout}
      onLayoutChange={onLayoutChange}
    />
  );
}
