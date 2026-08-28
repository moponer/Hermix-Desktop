/**
 * TableView — 工作台数据表视图（M1.4）。
 *
 * 基于 @tanstack/react-table v8（headless 列模型）+
 * 自研窗口化虚拟滚动（固定行高 + scrollTop 窗口 + translateY 平移层，
 * 大表不卡；视口高度走 ResizeObserver，窗口缩放后即时生效）。
 *
 * 写操作（M7 接通）：新增/编辑弹 FormView、删除两段确认（无 hover 取消，
 * 键盘可达）。能力由父层传入——只读工作台不渲染任何写入口。
 */

import { useEffect, useMemo, useRef, useState } from "react";
import {
  flexRender,
  getCoreRowModel,
  useReactTable,
  type ColumnDef,
} from "@tanstack/react-table";
import { Pencil, Plus, Search, Trash2 } from "lucide-react";
import type { WorkspaceField, WorkspaceModule } from "../../types";
import * as api from "../../api";
import { FormView } from "./FormView";

const ROW_HEIGHT = 28; // px — 与 .ws-table-row 的 CSS 高度一致
const OVERSCAN = 6; // 可见窗口上下各多渲染的行数
/** 删除两段确认的自动复位（ms）——超时未确认回到普通态。 */
const DELETE_CONFIRM_RESET_MS = 3000;
/** 关键字搜索防抖（ms）——输入停顿后才触发服务端查询。 */
const SEARCH_DEBOUNCE_MS = 300;

type TableRow = Record<string, unknown>;

export interface WorkspacePerms {
  canWrite: boolean;
  canDelete: boolean;
}

interface TableViewProps {
  module: Extract<WorkspaceModule, { type: "table" }>;
  rows: TableRow[];
  /** 加载态。 */
  loading?: boolean;
  /** 服务端全表行数：超过已加载 rows.length 时提示截断。 */
  totalCount?: number | null;
  /** 权限（缺省可读写）；只读时不渲染写入口。 */
  perms?: WorkspacePerms;
  /** 新增行（values 已过 FormView 必填校验与类型转换）。 */
  onInsert?: (moduleId: string, values: Record<string, unknown>) => Promise<void>;
  /** 按主键更新行。 */
  onUpdate?: (moduleId: string, rowId: string, patch: Record<string, unknown>) => Promise<void>;
  /** 按主键删行。 */
  onDeleteRow?: (moduleId: string, rowId: string) => Promise<void>;
  /** Relation 字段候选：目标表 id → 行清单（FormView 关系下拉）。 */
  relationCandidates?: Record<string, { id: string; label: string }[]>;
  /** 查询回调（P1）：关键字（空串=全表）或页码变化时触发，上层拉数据。 */
  onQuery?: (keyword: string, page: number) => void;
  /** 分页大小（默认 100，与服务端 limit/offset 配合）。 */
  pageSize?: number;
  /** 工作台 id（P5 附件预览 URL 需要）。 */
  workspaceId?: string;
}

interface EditingState {
  mode: "add" | "edit";
  row?: TableRow;
}

export function TableView({
  module,
  rows,
  loading = false,
  totalCount = null,
  perms = { canWrite: true, canDelete: true },
  onInsert,
  onUpdate,
  onDeleteRow,
  relationCandidates,
  onQuery,
  pageSize = 100,
  workspaceId,
}: TableViewProps) {
  // P1：关键字搜索（防抖）+ 分页（页码 0 基，配合服务端 limit/offset）
  const [searchInput, setSearchInput] = useState("");
  const [page, setPage] = useState(0);
  const searchTimer = useRef<number | null>(null);

  const handleSearchChange = (v: string) => {
    setSearchInput(v);
    if (searchTimer.current !== null) window.clearTimeout(searchTimer.current);
    // 输入停顿后才触发服务端查询；回到第 1 页
    searchTimer.current = window.setTimeout(() => {
      setPage(0);
      onQuery?.(v.trim(), 0);
    }, SEARCH_DEBOUNCE_MS);
  };

  const handlePageChange = (next: number) => {
    setPage(next);
    onQuery?.(searchInput.trim(), next);
  };

  const totalPages = totalCount !== null ? Math.max(1, Math.ceil(totalCount / pageSize)) : 1;

  // 列定义来自 schema 字段（id/展示名/类型），与后端校验同一份白名单。
  // attachment 字段：files/ 路径 → 图片缩略图或文件名（P5）。
  const columns = useMemo<ColumnDef<TableRow>[]>(
    () =>
      module.fields.map((f: WorkspaceField) => ({
        id: f.id,
        header: f.name,
        accessorFn: (row: TableRow) => row[f.id] ?? null,
        size: 180,
        minSize: 80,
        ...(f.type === "attachment"
          ? {
              cell: ({ getValue }: { getValue: () => unknown }) => {
                const v = String(getValue() ?? "");
                if (!v) return null;
                if (v.startsWith("files/") && workspaceId) {
                  const url = api.workspaceFileUrl(workspaceId, v);
                  if (/\.(png|jpe?g|gif|webp|svg)$/i.test(v)) {
                    return (
                      <img
                        src={url}
                        alt={v}
                        className="h-4 w-auto rounded align-middle"
                        title={v}
                      />
                    );
                  }
                  return <span className="text-[11px]">{v.split("/").pop()}</span>;
                }
                return <span className="text-[11px]">{v}</span>;
              },
            }
          : {}),
      })),
    [module.fields, workspaceId],
  );

  const table = useReactTable({
    data: rows,
    columns,
    getCoreRowModel: getCoreRowModel(),
  });

  const containerRef = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  // 视口高度不读渲染期 DOM：初始 fallback，挂载/窗口变化经 ResizeObserver 更新
  const [viewportH, setViewportH] = useState(400);
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    setViewportH(el.clientHeight);
    const ro = new ResizeObserver(() => setViewportH(el.clientHeight));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const [editing, setEditing] = useState<EditingState | null>(null);
  const [confirmingDeleteId, setConfirmingDeleteId] = useState<string | null>(null);

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

  const showOpsColumn = Boolean(onInsert || onUpdate || onDeleteRow);
  const truncated = totalCount !== null && totalCount > rows.length;

  const submitEditing = async (values: Record<string, unknown>) => {
    if (!editing) return;
    if (editing.mode === "add") {
      await onInsert?.(module.id, values);
    } else {
      await onUpdate?.(module.id, String(editing.row?.["id"] ?? ""), values);
    }
    // 走到这里即成功（失败由 FormView 捕获并保持表单开启，上层已 toast）
    setEditing(null);
  };

  return (
    <div className="relative flex min-h-0 flex-1 flex-col">
      {/* 工具条：表名 + 行数（含截断提示）+ 新增入口 */}
      <div
        className="flex shrink-0 items-center gap-2 px-3 py-1.5 text-[11.5px]"
        style={{ color: "var(--color-text-secondary)", borderBottom: "1px solid var(--color-border-subtle)" }}
      >
        <span className="font-medium" style={{ color: "var(--color-text-primary)" }}>{module.name}</span>
        <span>
          {rows.length} 行{truncated ? `（已截断，共 ${totalCount} 行）` : ""}
        </span>
        {loading && <span className="animate-pulse">加载中…</span>}

        {/* P1：关键字搜索（防抖）+ 分页 */}
        {onQuery && (
          <div className="ml-auto flex items-center gap-1.5">
            <div className="relative">
              <Search
                size={11}
                className="pointer-events-none absolute left-1.5 top-1/2 -translate-y-1/2"
                style={{ color: "var(--color-text-faint)" }}
              />
              <input
                value={searchInput}
                onChange={(e) => handleSearchChange(e.target.value)}
                placeholder="搜索…"
                className="w-36 rounded-md border py-0.5 pl-6 pr-1.5 text-[11.5px] outline-none"
                style={{
                  borderColor: "var(--color-border-secondary)",
                  background: "var(--color-bg-input, var(--color-bg-base))",
                  color: "var(--color-text-primary)",
                }}
              />
            </div>
            <div className="flex items-center gap-0.5">
              <button
                onClick={() => handlePageChange(Math.max(0, page - 1))}
                disabled={page <= 0}
                className="rounded px-1.5 py-0.5 transition-opacity hover:opacity-80 disabled:opacity-40"
                style={{ color: "var(--color-text-secondary)" }}
                title="上一页"
              >
                ‹
              </button>
              <span className="px-1 tabular-nums" style={{ color: "var(--color-text-faint)" }}>
                {page + 1}/{totalPages}
              </span>
              <button
                onClick={() => handlePageChange(Math.min(totalPages - 1, page + 1))}
                disabled={page >= totalPages - 1}
                className="rounded px-1.5 py-0.5 transition-opacity hover:opacity-80 disabled:opacity-40"
                style={{ color: "var(--color-text-secondary)" }}
                title="下一页"
              >
                ›
              </button>
            </div>
          </div>
        )}

        {perms.canWrite && onInsert && (
          <button
            onClick={() => setEditing({ mode: "add" })}
            className="flex items-center gap-1 rounded-md border px-2 py-0.5 transition-opacity hover:opacity-80"
            style={{
              borderColor: "var(--color-border-secondary)",
              color: "var(--color-text-primary)",
            }}
          >
            <Plus size={12} /> 新增行
          </button>
        )}
        {!perms.canWrite && (
          <span className="rounded-md border px-2 py-0.5" style={{ borderColor: "var(--color-border-secondary)", color: "var(--color-text-faint)" }}>
            只读
          </span>
        )}
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
                {showOpsColumn && (
                  <th
                    className="w-20 whitespace-nowrap px-3 py-1.5 text-left text-[11.5px] font-medium"
                    style={{
                      background: "var(--color-bg-elevated)",
                      color: "var(--color-text-secondary)",
                      borderBottom: "1px solid var(--color-border-secondary)",
                    }}
                  >
                    操作
                  </th>
                )}
              </tr>
            ))}
          </thead>
        </table>

        {/* 窗口化行层：外层占位高度撑起滚动条，内层 translateY 平移 */}
        <div style={{ height: allRows.length * ROW_HEIGHT, position: "relative" }}>
          <div style={{ transform: `translateY(${padTop}px)` }}>
            <table className="w-full" style={{ borderCollapse: "collapse" }}>
              <tbody>
                {visibleRows.map((row) => {
                  const rowId = String(row.original["id"] ?? "");
                  const confirming = confirmingDeleteId === rowId;
                  return (
                    <tr
                      key={row.id}
                      className="ws-table-row group"
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
                      {showOpsColumn && (
                        <td className="whitespace-nowrap px-2 text-[11px]">
                          {confirming ? (
                            <>
                              <button
                                onClick={() => {
                                  void onDeleteRow?.(module.id, rowId).finally(() =>
                                    setConfirmingDeleteId(null),
                                  );
                                }}
                                disabled={!perms.canDelete}
                                title={perms.canDelete ? undefined : "该工作台禁止删除行"}
                                className="mr-1 font-semibold underline disabled:cursor-not-allowed disabled:opacity-40"
                                style={{ color: "var(--color-error)" }}
                              >
                                确认删除
                              </button>
                              <button onClick={() => setConfirmingDeleteId(null)}>取消</button>
                            </>
                          ) : (
                            <span className="inline-flex items-center gap-1 opacity-30 transition-opacity group-hover:opacity-100">
                              {perms.canWrite && onUpdate && (
                                <button
                                  onClick={() => setEditing({ mode: "edit", row: row.original })}
                                  title="编辑行"
                                  className="rounded p-0.5 hover:opacity-70"
                                >
                                  <Pencil size={11} />
                                </button>
                              )}
                              {perms.canDelete && onDeleteRow && (
                                <button
                                  onClick={() => {
                                    setConfirmingDeleteId(rowId);
                                    window.setTimeout(
                                      () =>
                                        setConfirmingDeleteId((cur) =>
                                          cur === rowId ? null : cur,
                                        ),
                                      DELETE_CONFIRM_RESET_MS,
                                    );
                                  }}
                                  title="删除行"
                                  className="rounded p-0.5 hover:opacity-70"
                                  style={{ color: "var(--color-error)" }}
                                >
                                  <Trash2 size={11} />
                                </button>
                              )}
                            </span>
                          )}
                        </td>
                      )}
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        </div>

        {allRows.length === 0 && !loading && (
          <div
            className="flex items-center justify-center py-8 text-[12px]"
            style={{ color: "var(--color-text-faint)" }}
          >
            {perms.canWrite ? "还没有数据，点「新增行」或直接告诉 Hermix 记一条" : "暂无数据"}
          </div>
        )}
      </div>

      {/* 新增 / 编辑面板（内联覆盖层） */}
      {editing && (
        <div
          className="absolute inset-0 z-20 flex items-start justify-center pt-6"
          style={{ background: "color-mix(in srgb, var(--color-bg-base) 72%, transparent)" }}
        >
          <div
            className="max-h-full w-[520px] overflow-y-auto rounded-lg border shadow-lg"
            style={{
              borderColor: "var(--color-border-secondary)",
              background: "var(--color-bg-elevated)",
            }}
          >
            <FormView
              module={module}
              initialValues={editing.mode === "edit" ? editing.row : undefined}
              submitLabel={editing.mode === "add" ? "新增" : "保存"}
              relationCandidates={relationCandidates}
              workspaceId={workspaceId}
              onCancel={() => setEditing(null)}
              onSubmit={(values) => submitEditing(values)}
            />
          </div>
        </div>
      )}
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
