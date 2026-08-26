/**
 * TableView — 工作台数据表视图（M1.4）。
 *
 * 基于 @tanstack/react-table v8（headless 列模型/排序/过滤）+
 * 自研窗口化虚拟滚动（复用 SpreadsheetView 的行窗口方案：
 * 固定行高 + scrollTop 窗口 + translateY 平移层，大表不卡）。
 *
 * 数据来源：M1.4 由调用方传入 `rows`（M1.6 起走后端
 * `LocalSqliteStore` REST API，本组件无需感知数据来源）。
 */

import { useMemo, useRef, useState } from "react";
import {
  flexRender,
  getCoreRowModel,
  useReactTable,
  type ColumnDef,
} from "@tanstack/react-table";
import type { WorkspaceField, WorkspaceModule } from "../../types";

const ROW_HEIGHT = 28; // px — 与 .ws-table-row 的 CSS 高度一致
const OVERSCAN = 6; // 可见窗口上下各多渲染的行数

type TableRow = Record<string, unknown>;

interface TableViewProps {
  module: Extract<WorkspaceModule, { type: "table" }>;
  rows: TableRow[];
  /** 加载态（M1.6 接 REST 后使用）。 */
  loading?: boolean;
}

export function TableView({ module, rows, loading = false }: TableViewProps) {
  // 列定义来自 schema 字段（id/展示名/类型），与后端校验同一份白名单
  const columns = useMemo<ColumnDef<TableRow>[]>(
    () =>
      module.fields.map((f: WorkspaceField) => ({
        id: f.id,
        header: f.name,
        accessorFn: (row: TableRow) => row[f.id] ?? null,
        size: 180,
        minSize: 80,
      })),
    [module.fields],
  );

  const table = useReactTable({
    data: rows,
    columns,
    getCoreRowModel: getCoreRowModel(),
  });

  const containerRef = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const viewportH = containerRef.current?.clientHeight ?? 400;

  // 可见行窗口
  const allRows = table.getRowModel().rows;
  const start = Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - OVERSCAN);
  const end = Math.min(
    allRows.length,
    Math.ceil((scrollTop + viewportH) / ROW_HEIGHT) + OVERSCAN,
  );
  const visibleRows = allRows.slice(start, end);
  const padTop = start * ROW_HEIGHT;

  if (module.fields.length === 0) {
    return (
      <div className="flex flex-1 items-center justify-center text-[12px]" style={{ color: "var(--color-text-faint)" }}>
        该表还没有字段
      </div>
    );
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {/* 工具条：表名 + 行数 */}
      <div
        className="flex shrink-0 items-center gap-2 px-3 py-1.5 text-[11.5px]"
        style={{ color: "var(--color-text-secondary)", borderBottom: "1px solid var(--color-border-subtle)" }}
      >
        <span className="font-medium" style={{ color: "var(--color-text-primary)" }}>{module.name}</span>
        <span>{rows.length} 行</span>
        {loading && <span className="animate-pulse">加载中…</span>}
      </div>

      {/* 表格滚动区 */}
      <div
        ref={containerRef}
        className="min-h-0 flex-1 overflow-auto"
        onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
      >
        <table
          className="border-separate"
          style={{ borderSpacing: 0, width: "100%", borderCollapse: "separate" }}
        >
          {/* 冻结表头 */}
          <thead className="sticky top-0 z-10">
            {table.getHeaderGroups().map((hg) => (
              <tr key={hg.id}>
                {hg.headers.map((header) => (
                  <th
                    key={header.id}
                    className="whitespace-nowrap px-3 py-1.5 text-left text-[11.5px] font-medium"
                    style={{
                      background: "var(--color-bg-elevated)",
                      color: "var(--color-text-secondary)",
                      borderBottom: "1px solid var(--color-border-secondary)",
                    }}
                  >
                    {flexRender(header.column.columnDef.header, header.getContext())}
                  </th>
                ))}
              </tr>
            ))}
          </thead>
        </table>

        {/* 窗口化行层：外层占位高度撑起滚动条，内层 translateY 平移 */}
        <div style={{ height: allRows.length * ROW_HEIGHT, position: "relative" }}>
          <div style={{ transform: `translateY(${padTop}px)` }}>
            <table className="w-full" style={{ borderCollapse: "collapse" }}>
              <tbody>
                {visibleRows.map((row) => (
                  <tr
                    key={row.id}
                    className="ws-table-row"
                    style={{
                      height: ROW_HEIGHT,
                      borderBottom: "1px solid var(--color-border-subtle)",
                    }}
                  >
                    {row.getVisibleCells().map((cell) => (
                      <td
                        key={cell.id}
                        className="truncate px-3 text-[12px]"
                        style={{
                          color: "var(--color-text-primary)",
                          maxWidth: 240,
                          whiteSpace: "nowrap",
                          overflow: "hidden",
                          textOverflow: "ellipsis",
                        }}
                      >
                        {formatCell(cell.getValue())}
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      </div>
    </div>
  );
}

/** 单元格格式化：布尔/枚举/关系友好展示。 */
function formatCell(v: unknown): string {
  if (v === null || v === undefined) return "—";
  if (typeof v === "boolean") return v ? "✓" : "✗";
  if (typeof v === "number") return String(v);
  return String(v);
}
