/**
 * DashboardWidgets — 仪表盘组件（M1.5）。
 *
 * 三种组件：Stat（统计卡）/ Chart（ECharts 图表）/ List（列表）。
 * 数据经 `data` prop 注入（M1.6 起由后端查询填充）；`query` 是
 * 受控查询片段（后端白名单校验），组件自身不执行任何 SQL。
 */

import { useEffect, useRef, useState } from "react";
import * as echarts from "echarts";
import type { DashboardWidget } from "../../types";
import { THEME_CHANGED_EVENT } from "../../hooks/useTheme";
import { isDarkTheme } from "../../styles/themes/registry";

/** 组件通用外壳。 */
function WidgetShell({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div
      className="flex h-full flex-col overflow-hidden rounded-lg border"
      style={{ borderColor: "var(--color-border-secondary)", background: "var(--color-bg-elevated)" }}
    >
      <div className="shrink-0 px-3 py-1.5 text-[12px] font-medium" style={{ color: "var(--color-text-secondary)", borderBottom: "1px solid var(--color-border-subtle)" }}>
        {title}
      </div>
      <div className="min-h-0 flex-1">{children}</div>
    </div>
  );
}

// ── Stat ────────────────────────────────────────────────────

export function StatWidget({ widget, value }: { widget: DashboardWidget; value?: number | string }) {
  return (
    <WidgetShell title={widget.title}>
      <div className="flex h-full items-center justify-center">
        <span className="text-[28px] font-semibold tabular-nums" style={{ color: "var(--color-text-primary)" }}>
          {value ?? "—"}
        </span>
      </div>
    </WidgetShell>
  );
}

// ── Chart（ECharts） ─────────────────────────────────────────

export function ChartWidget({
  widget,
  option,
}: {
  widget: DashboardWidget;
  /** ECharts option。 */
  option?: echarts.EChartsOption;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const chartRef = useRef<echarts.ECharts | null>(null);
  // 主题切换 → 销毁重建实例，让内置 'dark' 主题与文字色整套换血
  const [, bumpTheme] = useState(0);

  useEffect(() => {
    const onThemeChange = () => bumpTheme((n) => n + 1);
    window.addEventListener(THEME_CHANGED_EVENT, onThemeChange);
    return () => window.removeEventListener(THEME_CHANGED_EVENT, onThemeChange);
  }, []);

  const isDark = isDarkTheme(document.documentElement.dataset.theme);

  useEffect(() => {
    if (!ref.current) return;
    const chart = echarts.init(ref.current, isDark ? "dark" : undefined);
    chartRef.current = chart;
    const ro = new ResizeObserver(() => chart.resize());
    ro.observe(ref.current);
    return () => {
      ro.disconnect();
      chart.dispose();
      chartRef.current = null;
    };
  }, [isDark]);

  useEffect(() => {
    chartRef.current?.setOption(
      option ?? {
        // 空数据占位：标题取自组件 title，坐标轴随数据接入后填充
        title: { text: "", show: false },
        xAxis: { type: "category", data: [] },
        yAxis: { type: "value" },
        series: [],
      },
      true,
    );
  }, [option]);

  return (
    <WidgetShell title={widget.title}>
      <div ref={ref} className="h-full w-full" />
    </WidgetShell>
  );
}

// ── List ────────────────────────────────────────────────────

/** 系统列不进展示值（UUID/时间戳混进列表毫无信息量）。 */
const RESERVED_ROW_KEYS = new Set(["id", "created_at", "updated_at", "updated_by"]);

export function ListWidget({
  widget,
  rows,
}: {
  widget: DashboardWidget;
  rows?: Record<string, unknown>[];
}) {
  const list = rows ?? [];
  return (
    <WidgetShell title={widget.title}>
      <div className="h-full overflow-y-auto px-3 py-1.5">
        {list.length === 0 ? (
          <div className="flex h-full items-center justify-center text-[12px]" style={{ color: "var(--color-text-faint)" }}>
            暂无数据
          </div>
        ) : (
          <ul className="flex flex-col gap-1">
            {list.map((row, i) => {
              const values = Object.entries(row)
                .filter(([k, v]) => !RESERVED_ROW_KEYS.has(k) && v !== null && v !== undefined)
                .map(([, v]) => String(v));
              return (
                <li key={row["id"] ? String(row["id"]) : i} className="truncate text-[12px]" style={{ color: "var(--color-text-primary)" }}>
                  {values.join(" · ")}
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </WidgetShell>
  );
}

/** 按组件类型分发渲染。 */
export function DashboardWidgetView({
  widget,
  data,
  chartOption,
}: {
  widget: DashboardWidget;
  data?: unknown;
  chartOption?: echarts.EChartsOption;
}) {
  switch (widget.type) {
    case "stat":
      return <StatWidget widget={widget} value={data as number | string | undefined} />;
    case "chart":
      return <ChartWidget widget={widget} option={chartOption} />;
    case "list":
      return <ListWidget widget={widget} rows={data as Record<string, unknown>[] | undefined} />;
  }
}
