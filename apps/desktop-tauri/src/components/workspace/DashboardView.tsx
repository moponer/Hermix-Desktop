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
import type * as echarts from "echarts";
import type { WorkspaceModule } from "../../types";
import { DashboardWidgetView } from "./DashboardWidgets";

const COLS = 12;
const ROW_HEIGHT = 30;

/**
 * 组件默认尺寸档位 — LLM 未给 layout 时的兜底铺排，也作为 SKILL 里的推荐档位。
 * 依据内容高度：stat 只需一行大数字，chart 需要足够绘图区，list 需能容 5+ 行。
 */
const DEFAULT_SIZE: Record<string, { w: number; h: number }> = {
  stat: { w: 3, h: 2 },
  chart: { w: 6, h: 4 },
  list: { w: 4, h: 4 },
};

interface DashboardViewProps {
  module: Extract<WorkspaceModule, { type: "dashboard" }>;
  /** 当前布局（来自 schema.layout[module.id]）。 */
  layout?: Layout;
  /** 拖拽/缩放后回调（M1.6 持久化）。 */
  onLayoutChange?: (layout: Layout) => void;
  /** 组件数据：widgetId → 数据（M1.6 起由后端查询填充）。 */
  widgetData?: Record<string, unknown>;
  /** Chart 组件 ECharts option：widgetId → option（M1.6 起由 WorkspacesView 生成）。 */
  widgetChartOptions?: Record<string, echarts.EChartsOption>;
}

export function DashboardView({
  module,
  layout,
  onLayoutChange,
  widgetData,
  widgetChartOptions,
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

  // 无布局时按组件类型档位流式铺排：行内放不下就换行，行高取该行最高组件，
  // 保证同排顶边对齐（替代早期"一律 3×4"的一刀切兜底）。
  const autoLayout: LayoutItem[] = (() => {
    const items: LayoutItem[] = [];
    let x = 0;
    let y = 0;
    let rowH = 0;
    for (const w of widgets) {
      const size = DEFAULT_SIZE[w.type] ?? { w: 4, h: 4 };
      const width = Math.min(size.w, COLS);
      if (x + width > COLS) {
        x = 0;
        y += rowH;
        rowH = 0;
      }
      items.push({ i: w.id, x, y, w: width, h: size.h, minW: 2, minH: 2 });
      x += width;
      rowH = Math.max(rowH, size.h);
    }
    return items;
  })();

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
              <DashboardWidgetView
                widget={w}
                data={widgetData?.[w.id]}
                chartOption={widgetChartOptions?.[w.id]}
              />
            </div>
          ))}
        </GridLayout>
      )}
    </div>
  );
}
