// components/settings/MemoryGraph.tsx
//
// 记忆图谱（"大脑宇宙"）：把 memory.db 的 facts/frames/reflections、
// 每日记忆天节点和 MEMORY.md/USER.md 条目节点画成一张力导向图。
// echarts 6 已是既有依赖（首次实际使用），不引新包。
//
// 主题适配规则（check:tokens 强制）：源码里不出现 hex/rgb 字面量，
// 所有颜色在运行时从 CSS 变量解析；主题切换监听 THEME_CHANGED_EVENT
// 重建配色。分类色相由 accent 派生（HSL 旋转），深浅主题各自调亮度。

import { useCallback, useEffect, useMemo, useRef } from "react";
import * as echarts from "echarts/core";
import { GraphChart, type GraphSeriesOption } from "echarts/charts";
import { TooltipComponent } from "echarts/components";
import { CanvasRenderer } from "echarts/renderers";
import type { EChartsCoreOption } from "echarts/core";
import { isDarkTheme } from "../../styles/themes/registry";
import { THEME_CHANGED_EVENT, useTheme } from "../../hooks/useTheme";
import type { MemoryGraphNode } from "../../api";
import { EmptyState, Spinner } from "../ui";
import { Brain } from "lucide-react";

echarts.use([GraphChart, TooltipComponent, CanvasRenderer]);

/** 图上展示的节点类别（kind → 中文名）。 */
export const KIND_LABELS: Record<string, string> = {
  fact: "事实",
  turn: "对话",
  memory_tool: "记忆工具",
  session: "会话总结",
  delegation: "子任务",
  compression_backfill: "压缩回填",
  focus: "焦点",
  reflection: "反思",
  daily: "每日记忆",
  md: "文本条目",
};

/** 把任意 CSS 颜色值解析成 rgb 三元组（借浏览器归一化，避免硬编码格式）。 */
function parseColorToRgb(value: string): [number, number, number] | null {
  const el = document.createElement("span");
  el.style.color = value;
  document.body.appendChild(el);
  const normalized = getComputedStyle(el).color;
  el.remove();
  const m = normalized.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)/);
  return m ? [Number(m[1]), Number(m[2]), Number(m[3])] : null;
}

function rgbToHsl([r, g, b]: [number, number, number]): [number, number, number] {
  const rf = r / 255, gf = g / 255, bf = b / 255;
  const max = Math.max(rf, gf, bf), min = Math.min(rf, gf, bf);
  const l = (max + min) / 2;
  if (max === min) return [0, 0, l];
  const d = max - min;
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  let h = 0;
  if (max === rf) h = ((gf - bf) / d + (gf < bf ? 6 : 0)) / 6;
  else if (max === gf) h = ((bf - rf) / d + 2) / 6;
  else h = ((rf - gf) / d + 4) / 6;
  return [h * 360, s, l];
}

const cssVar = (name: string, fallback = ""): string =>
  getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback;

/** 压成单行缩略文本：换行折叠为空格，超长截断加省略号。 */
function shortText(s: string | undefined, max: number): string {
  const t = (s ?? "").replace(/\s+/g, " ").trim();
  if (!t) return "";
  return t.length > max ? `${t.slice(0, max)}…` : t;
}

/** 类别调色板：accent 色相起步，每类旋转 34°。深色主题提亮、浅色压暗。 */
function categoryPalette(count: number, dark: boolean): string[] {
  const accent = parseColorToRgb(cssVar("--color-accent", "steelblue")) ?? [70, 130, 180];
  const [h, s] = rgbToHsl(accent);
  const l = dark ? 0.66 : 0.44;
  const sat = Math.max(0.42, Math.min(0.8, s));
  return Array.from({ length: count }, (_, i) =>
    `hsl(${Math.round((h + i * 34) % 360)}, ${Math.round(sat * 100)}%, ${Math.round(l * 100)}%)`,
  );
}

export interface MemoryGraphProps {
  nodes: MemoryGraphNode[];
  edges: Array<{ source: string; target: string; relation: string }>;
  loading: boolean;
  onSelectNode: (node: MemoryGraphNode) => void;
}

export function MemoryGraph({ nodes, edges, loading, onSelectNode }: MemoryGraphProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<echarts.ECharts | null>(null);
  const { resolved } = useTheme();
  const dark = isDarkTheme(resolved);
  const selectRef = useRef(onSelectNode);
  selectRef.current = onSelectNode;

  const kinds = useMemo(() => {
    const seen: string[] = [];
    for (const n of nodes) if (!seen.includes(n.kind)) seen.push(n.kind);
    return seen;
  }, [nodes]);

  const option = useMemo<EChartsCoreOption>(() => {
    const palette = categoryPalette(Math.max(kinds.length, 1), dark);
    const labelColor = cssVar("--color-text-secondary");
    const edgeColor = cssVar("--color-border-primary");
    const cardBg = cssVar("--color-bg-card");
    /* 常驻标签只给权重最高的前 N 个（2026-10-28）：全量常驻时 20+ 条长标签
       在力导图里必然互相压——hideOverlap 只能藏「完全重叠」的，部分压住的
       照样糊成一团（用户截图里的"你的任务：调研并整理…"叠字）。低权重节点
       只留圆点，悬停/点击再看全文，画面才干净。 */
    const labelBudget = 14;
    const labeled = new Set(
      [...nodes]
        .sort((a, b) => b.salience - a.salience)
        .slice(0, labelBudget)
        .map((n) => n.id),
    );
    const data = nodes.map((n) => {
      const color = palette[kinds.indexOf(n.kind) % palette.length];
      return {
        id: n.id,
        name: n.label,
        kind: n.kind,
        salience: n.salience,
        pinned: n.pinned,
        preview: n.preview,
        category: kinds.indexOf(n.kind),
        symbolSize: 10 + n.salience * 24 + (n.pinned ? 5 : 0),
        itemStyle: {
          color,
          // 卡片底色描边把节点从标签文字里"抠"出来，视觉干净且零 GPU 成本
          borderColor: cardBg,
          borderWidth: 2,
        },
        label: { show: labeled.has(n.id) },
      };
    });
    const series: GraphSeriesOption = {
      type: "graph",
      layout: "force",
      roam: true,
      draggable: true,
      data,
      links: edges.map((e) => ({ source: e.source, target: e.target })),
      categories: kinds.map((k) => ({ name: KIND_LABELS[k] ?? k })),
      // layoutAnimation:false：拖拽/缩放时不再实时重算力导（卡顿主源）
      force: { repulsion: 150, edgeLength: [44, 150], gravity: 0.12, layoutAnimation: false },
      scaleLimit: { min: 0.4, max: 4 },
      // 节点下方常驻内容缩略：记忆库不是「一屏圆点」，肉眼要能扫读。
      // hideOverlap 自动藏掉互相压住的标签，缩放时逐级浮现。
      labelLayout: { hideOverlap: true, moveOverlap: "shiftY" },
      label: {
        show: true,
        position: "bottom",
        color: labelColor,
        fontSize: 10,
        formatter: (p: unknown) => {
          const d = (p as { data?: { preview?: string; name?: string } }).data;
          // 常驻标签压到 16 字：24 字在 420px 高的图里横向跨度过大，
          // 两条就会撞在一起；全文在悬停（emphasis 60 字）与详情弹窗里。
          return shortText(d?.preview, 16) || shortText(d?.name, 16);
        },
      },
      emphasis: {
        focus: "adjacency",
        itemStyle: { borderWidth: 2.5 },
        label: {
          show: true,
          color: cssVar("--color-text-primary"),
          fontSize: 11,
          formatter: (p: unknown) => {
            const d = (p as { data?: { preview?: string; name?: string } }).data;
            return shortText(d?.preview, 60) || shortText(d?.name, 60);
          },
        },
      },
      lineStyle: { color: edgeColor, opacity: 0.35, width: 1, curveness: 0.1 },
    };
    return {
      backgroundColor: "transparent",
      tooltip: {
        confine: true,
        backgroundColor: cssVar("--color-bg-card"),
        borderColor: cssVar("--color-border-primary"),
        textStyle: { color: cssVar("--color-text-primary"), fontSize: 12 },
        formatter: (p: {
          dataType?: string;
          data?: { name?: string; kind?: string; salience?: number; preview?: string };
        }) => {
          if (p.dataType !== "node" || !p.data) return "";
          const kindLabel = KIND_LABELS[p.data.kind ?? ""] ?? p.data.kind ?? "";
          const body = (p.data.preview ?? "").replace(/\s+/g, " ").trim();
          const bodyHtml = body
            ? `<br/><span style="opacity:.75">${escapeHtml(shortText(body, 200))}</span>`
            : "";
          return `<b>${escapeHtml(p.data.name ?? "")}</b><br/>${escapeHtml(kindLabel)} · 权重 ${(p.data.salience ?? 0).toFixed(2)}${bodyHtml}`;
        },
      },
      series: [series],
    };
  }, [nodes, edges, kinds, dark]);

  const rebuild = useCallback(() => {
    const el = containerRef.current;
    if (!el) return;
    if (!chartRef.current) {
      chartRef.current = echarts.init(el);
      chartRef.current.on("click", (params) => {
        const p = params as { dataType?: string; data?: { id?: string } };
        if (p.dataType !== "node" || !p.data?.id) return;
        const hit = nodes.find((n) => n.id === p.data?.id);
        if (hit) selectRef.current(hit);
      });
    }
    chartRef.current.setOption(option, true);
  }, [option, nodes]);

  // 主题切换（含系统跟随态变化）后按新配色重建。
  useEffect(() => {
    rebuild();
    const onTheme = () => rebuild();
    window.addEventListener(THEME_CHANGED_EVENT, onTheme);
    return () => window.removeEventListener(THEME_CHANGED_EVENT, onTheme);
  }, [rebuild]);

  useEffect(() => {
    const el = containerRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => chartRef.current?.resize());
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  useEffect(() => () => {
    chartRef.current?.dispose();
    chartRef.current = null;
  }, []);

  if (loading && nodes.length === 0) {
    return (
      <div className="flex items-center justify-center h-[420px]" aria-busy="true">
        <Spinner />
      </div>
    );
  }
  if (nodes.length === 0) {
    return (
      <div className="h-[420px] flex items-center justify-center">
        <EmptyState
          icon={<Brain className="size-7" />}
          title="还没有可展示的记忆"
        />
      </div>
    );
  }
  return <div ref={containerRef} className="h-[420px] w-full" data-testid="memory-graph-canvas" />;
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => (
    { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c] ?? c
  ));
}
