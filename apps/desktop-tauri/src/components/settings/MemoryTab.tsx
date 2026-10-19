// components/settings/MemoryTab.tsx
//
// 设置 → 记忆管理。四个视图：记忆图谱（echarts 力导向，"大脑宇宙"）、
// 角色（提示词全文展示 + 编辑）、记忆文本（全局 MEMORY.md / USER.md）、
// 每日记忆（daily_memory.db 逐日内容）。
// 数据全部来自后端 /api/memory/* 与 /api/persona/*（见 api/rest.ts），
// 本组件不落 localStorage —— 记忆是后端权威，不是前端偏好。

import { useCallback, useEffect, useMemo, useState } from "react";
import { Brain, Pin, PinOff, RefreshCw, Search, Trash2 } from "lucide-react";
import * as api from "../../api";
import { Button, ConfirmDialog, Modal, SegmentedControl, SkeletonCard } from "../ui";
import { MemoryGraph, KIND_LABELS } from "./MemoryGraph";
import { PersonasPanel } from "./PersonasPanel";
import { MemoryTextPanel } from "./MemoryTextPanel";
import { DailyMemoryPanel } from "./DailyMemoryPanel";

type View = "graph" | "personas" | "text" | "daily";

const VIEW_OPTIONS: Array<{ value: View; label: string }> = [
  { value: "graph", label: "记忆图谱" },
  { value: "personas", label: "角色" },
  { value: "text", label: "记忆文本" },
  { value: "daily", label: "每日记忆" },
];

export function MemoryTab() {
  const [view, setView] = useState<View>("graph");
  return (
    <div className="space-y-4">
      <SegmentedControl
        label="记忆管理视图"
        options={VIEW_OPTIONS}
        value={view}
        onChange={setView}
        as="tablist"
        layout="fill"
      />
      {view === "graph" && <GraphView />}
      {view === "personas" && <PersonasPanel />}
      {view === "text" && <MemoryTextPanel />}
      {view === "daily" && <DailyMemoryPanel />}
    </div>
  );
}

// ── 记忆图谱视图 ──────────────────────────────────────────────────

function GraphView() {
  const [graph, setGraph] = useState<api.MemoryGraphResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  // 对话流水（turn 原始对话 / session 会话尾注 / focus 每轮结论）默认隐藏：
  // 这三类占库内节点 94%+ 且多为问候语级碎片，默认展示会把真正的记忆
  // （memory_tool/fact/reflection/daily/md）淹没。筛选条随时可开。
  const [hiddenKinds, setHiddenKinds] = useState<Set<string>>(
    () => new Set(["turn", "session", "focus"]),
  );
  const [selected, setSelected] = useState<api.MemoryGraphNode | null>(null);
  const [item, setItem] = useState<api.MemoryItemResponse | null>(null);
  const [itemLoading, setItemLoading] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [actionBusy, setActionBusy] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [g, hits] = await Promise.all([
        // 160：带常驻标签的力导图在 200+ 节点开始卡顿；配合
        // labelLayout.hideOverlap，160 是流畅与信息量的平衡点。
        api.getMemoryGraph(160),
        query.trim() ? api.searchMemory(query.trim(), 60) : Promise.resolve({ results: [] as api.MemoryGraphNode[] }),
      ]);
      // 搜索命中集之外的节点弱化处理：直接过滤，图谱聚焦命中子图。
      if (query.trim()) {
        const hitIds = new Set(hits.results.map((r) => r.id));
        const hitEdges = g.edges.filter((e) => hitIds.has(e.source) && hitIds.has(e.target));
        setGraph({ ...g, nodes: g.nodes.filter((n) => hitIds.has(n.id)), edges: hitEdges });
      } else {
        setGraph(g);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [query]);

  useEffect(() => {
    const t = setTimeout(load, query ? 350 : 0);
    return () => clearTimeout(t);
  }, [load, query]);

  const nodes = useMemo(
    () => (graph?.nodes ?? []).filter((n) => !hiddenKinds.has(n.kind)),
    [graph, hiddenKinds],
  );
  const nodeIds = useMemo(() => new Set(nodes.map((n) => n.id)), [nodes]);
  const edges = useMemo(
    () => (graph?.edges ?? []).filter((e) => nodeIds.has(e.source) && nodeIds.has(e.target)),
    [graph, nodeIds],
  );
  const allKinds = useMemo(() => {
    const counts = new Map<string, number>();
    for (const n of graph?.nodes ?? []) counts.set(n.kind, (counts.get(n.kind) ?? 0) + 1);
    return [...counts.entries()].sort((a, b) => b[1] - a[1]);
  }, [graph]);

  const openDetail = useCallback(async (node: api.MemoryGraphNode) => {
    setSelected(node);
    setItem(null);
    setItemLoading(true);
    try {
      setItem(await api.getMemoryItem(node.kind, node.id));
    } catch {
      setItem(null);
    } finally {
      setItemLoading(false);
    }
  }, []);

  const togglePin = useCallback(async () => {
    if (!selected || !selected.id.startsWith("frame:")) return;
    setActionBusy(true);
    try {
      await api.pinMemory(selected.id, !selected.pinned);
      setSelected({ ...selected, pinned: !selected.pinned });
      await load();
    } finally {
      setActionBusy(false);
    }
  }, [selected, load]);

  const doDelete = useCallback(async () => {
    if (!selected) return;
    setActionBusy(true);
    try {
      const kind = selected.id.startsWith("fact:") ? "fact"
        : selected.id.startsWith("refl:") ? "reflection" : selected.kind;
      await api.deleteMemoryItem(kind, selected.id);
      setConfirmDelete(false);
      setSelected(null);
      await load();
    } finally {
      setActionBusy(false);
    }
  }, [selected, load]);

  const stats = graph?.stats;

  return (
    <div className="space-y-3">
      {/* 概览行 */}
      <div className="flex flex-wrap items-center gap-x-4 gap-y-1 rounded-lg bg-[var(--color-bg-card)] shadow-[var(--shadow-paper)] px-4 py-2.5 text-[length:var(--text-meta)] text-[var(--color-text-secondary)]">
        <span className="flex items-center gap-1.5 font-medium text-[var(--color-text-primary)]">
          <Brain className="size-4 text-[var(--color-accent)]" /> 记忆总览
        </span>
        {stats ? (
          <>
            <Stat label="事实" value={stats.facts} />
            <Stat label="记忆帧" value={stats.frames} />
            <Stat label="反思" value={stats.reflections} />
            <Stat label="每日记忆" value={stats.daily_days} />
            <Stat label="文本条目" value={stats.md_entries} />
            <Stat label="库大小" value={formatBytes(stats.db_bytes)} />
          </>
        ) : (
          <span>{loading ? "…" : "—"}</span>
        )}
        <Button variant="ghost" size="sm" onClick={load} disabled={loading} className="ml-auto">
          <RefreshCw className={`size-3.5 ${loading ? "animate-spin" : ""}`} /> 刷新
        </Button>
      </div>

      {error && (
        <div className="rounded-lg bg-[var(--color-bg-card)] shadow-[var(--shadow-paper)] px-4 py-3 text-[length:var(--text-meta)] text-[var(--color-text-secondary)]">
          记忆接口不可用（{error}）。刚更新后端时需要重启应用后重试。
        </div>
      )}

      {/* 搜索 + 类别过滤 */}
      <div className="flex flex-wrap items-center gap-2">
        <div className="relative flex-1 min-w-[200px]">
          <Search className="pointer-events-none absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-[var(--color-text-muted)]" />
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="搜索记忆内容…"
            className="h-8 w-full rounded-[var(--radius-md)] bg-[var(--color-bg-tertiary)] pl-8 pr-3 text-[length:var(--text-ui)] text-[var(--color-text-primary)] outline-none transition-shadow placeholder:text-[var(--color-text-muted)] focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]/40"
          />
        </div>
        {allKinds.map(([kind, count]) => {
          const hidden = hiddenKinds.has(kind);
          return (
            <button
              key={kind}
              onClick={() => setHiddenKinds((prev) => {
                const next = new Set(prev);
                if (next.has(kind)) next.delete(kind); else next.add(kind);
                return next;
              })}
              className={`rounded-full px-2.5 py-0.5 text-[length:var(--text-meta)] transition-colors ${
                hidden
                  ? "bg-[var(--color-bg-tertiary)] text-[var(--color-text-muted)]"
                  : "bg-[var(--color-accent-soft-strong)] text-[var(--color-accent)]"
              }`}
            >
              {KIND_LABELS[kind] ?? kind} · {count}
            </button>
          );
        })}
      </div>

      {graph?.truncated && (
        <p className="text-[length:var(--text-meta)] text-[var(--color-text-muted)]">
          记忆数量超出展示上限，仅显示最近/最高权重的部分；用搜索可精确定位。
        </p>
      )}

      <div className="rounded-lg bg-[var(--color-bg-card)] shadow-[var(--shadow-paper)] p-1">
        <MemoryGraph nodes={nodes} edges={edges} loading={loading} onSelectNode={openDetail} />
      </div>
      <p className="text-[length:var(--text-meta)] text-[var(--color-text-muted)]">
        滚轮缩放 · 拖拽移动 · 点击节点查看全文。节点大小代表记忆权重，描边为置顶。
      </p>

      {/* 详情弹窗 */}
      <Modal
        open={selected !== null}
        onClose={() => setSelected(null)}
        title={
          selected ? (
            <span className="flex items-center gap-2">
              <Brain className="size-4 text-[var(--color-accent)]" />
              {KIND_LABELS[selected.kind] ?? selected.kind}
            </span>
          ) : null
        }
        footer={
          selected && (
            <div className="flex items-center gap-2">
              {selected.id.startsWith("frame:") && (
                <Button variant="secondary" size="sm" onClick={togglePin} disabled={actionBusy}>
                  {selected.pinned ? <PinOff className="size-3.5" /> : <Pin className="size-3.5" />}
                  {selected.pinned ? "取消置顶" : "置顶"}
                </Button>
              )}
              {selected.id.startsWith("fact:") || selected.id.startsWith("frame:") || selected.id.startsWith("refl:") ? (
                <Button variant="danger" size="sm" onClick={() => setConfirmDelete(true)} disabled={actionBusy}>
                  <Trash2 className="size-3.5" /> 删除
                </Button>
              ) : null}
              <span className="ml-auto text-[length:var(--text-meta)] text-[var(--color-text-muted)]">
                {item?.file ? item.file : `权重 ${selected.salience.toFixed(2)}`}
              </span>
            </div>
          )
        }
      >
        {itemLoading ? (
          <SkeletonCard lines={4} />
        ) : item ? (
          <pre className="max-h-[50vh] overflow-auto whitespace-pre-wrap rounded-md bg-[var(--color-bg-tertiary)] p-3 text-[length:var(--text-meta)] leading-relaxed text-[var(--color-text-primary)]">
            {item.content}
          </pre>
        ) : (
          <p className="py-6 text-center text-[length:var(--text-meta)] text-[var(--color-text-muted)]">
            内容加载失败，可能已被删除或仍在写入。
          </p>
        )}
      </Modal>

      <ConfirmDialog
        open={confirmDelete}
        title="删除这条记忆？"
        body="删除后不会出现在召回中（底层为可逆的软删除）。"
        onConfirm={doDelete}
        onCancel={() => setConfirmDelete(false)}
      />
    </div>
  );
}

function Stat({ label, value }: { label: string; value: number | string }) {
  return (
    <span>
      {label} <b className="font-medium text-[var(--color-text-primary)]">{value}</b>
    </span>
  );
}

function formatBytes(n: number): string {
  if (!n) return "—";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}
