/**
 * DashboardView — 工作台仪表盘布局容器（M1.5）。
 *
 * 用 react-grid-layout 2.x 渲染 schema 里的 dashboard 模块：
 * - `layout` 来自 `schema.layout[moduleId]`（react-grid-layout 可序列化输出）
 * - 无布局时按列顺序自动铺排（x=0, 逐行堆叠），拖拽后 onLayoutChange 上报
 *   （M1.6 持久化回后端）
 * - 组件按 widget.type 分发到 StatWidget / ChartWidget / ListWidget
 *
 * 2.x API 注意：GridConfig 收拢了 cols/rowHeight/margin；width 必填
 * （用 useContainerWidth 测量容器）；Layout 是 readonly LayoutItem[]。
 */

import GridLayout, {
  type Layout,
  type LayoutItem,
  useContainerWidth,
} from "react-grid-layout";
import "react-grid-layout/css/styles.css";
import type { WorkspaceModule } from "../../types";
import { DashboardWidgetView } from "./DashboardWidgets";

const COLS = 12;
const ROW_HEIGHT = 30;

interface DashboardViewProps {
  module: Extract<WorkspaceModule, { type: "dashboard" }>;
  /** 当前布局（来自 schema.layout[module.id]）。 */
  layout?: Layout;
  /** 拖拽/缩放后回调（M1.6 持久化）。 */
  onLayoutChange?: (layout: Layout) => void;
  /** 组件数据：widgetId → 数据（M1.6 起由后端查询填充）。 */
  widgetData?: Record<string, unknown>;
}

export function DashboardView({
  module,
  layout,
  onLayoutChange,
  widgetData,
}: DashboardViewProps) {
  const { width, containerRef, mounted } = useContainerWidth();
  const widgets = module.widgets ?? [];

  if (widgets.length === 0) {
    return (
      <div
        className="flex h-full items-center justify-center rounded-lg border border-dashed text-[12px]"
        style={{ borderColor: "var(--color-border-secondary)", color: "var(--color-text-faint)" }}
      >
        「{module.name}」还没有组件 —— 在对话中告诉 Hermix 添加统计卡/图表
      </div>
    );
  }

  // 无布局时自动铺排：每个组件 3 列宽 × 4 行高，逐行堆叠
  const autoLayout: LayoutItem[] = widgets.map((w, i) => {
    const perRow = Math.floor(COLS / 3);
    const col = (i % perRow) * 3;
    const row = Math.floor(i / perRow) * 4;
    return { i: w.id, x: col, y: row, w: 3, h: 4, minW: 2, minH: 2 };
  });

  const current: Layout = layout && layout.length > 0 ? layout : autoLayout;

  return (
    <div ref={containerRef} className="min-h-0 flex-1 overflow-y-auto">
      {mounted && width > 0 && (
        <GridLayout
          className="layout"
          width={width}
          layout={current}
          gridConfig={{
            cols: COLS,
            rowHeight: ROW_HEIGHT,
            margin: [8, 8],
            containerPadding: [4, 4],
          }}
          dragConfig={{ enabled: true }}
          resizeConfig={{ enabled: true }}
          onLayoutChange={(l) => onLayoutChange?.(l)}
        >
          {widgets.map((w) => (
            <div key={w.id}>
              <DashboardWidgetView widget={w} data={widgetData?.[w.id]} />
            </div>
          ))}
        </GridLayout>
      )}
    </div>
  );
}
