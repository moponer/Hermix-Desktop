// ToolsetsTab.tsx —— 工具集 Tab：后端工具注册表分组（R2-4 拆分第五步）。
import { useCallback, useEffect, useMemo, useState } from "react";
import { Boxes, X } from "lucide-react";
import * as api from "../../api";
import type { ToolsetInfo } from "../../api";
import { InitialIcon } from "../ui/InitialIcon";

import { ToggleSwitch } from "../ToggleSwitch";
import { SearchBar, Grid, EmptyHint, SkeletonGrid, SummaryBar, ToolGroupTitle, ToolHint } from "./toolTabsShared";
import { ENABLED, SAVE_FAILED, failure } from "../strings";

// ── Toolsets tab: backend tool registry (a.k.a. toolset groups) ──────

export function ToolsetsTab({ query, onQuery }: { query: string; onQuery: (q: string) => void }) {
  const [toolsets, setToolsets] = useState<ToolsetInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // Local mirror of which toolsets are enabled, seeded from the
  // server's first response. We keep this as a `Set` for O(1)
  // membership checks; the server treats the canonical list as
  // the absolute state, so we always replace the whole set after
  // a successful PUT instead of merging.
  const [enabled, setEnabled] = useState<Set<string>>(() => new Set());
  // Per-toolset saving state. A toggle click is *optimistic* —
  // we flip the local `enabled` set immediately, mark the
  // toolset as `saving`, then on the PUT response we either
  // confirm (clear `saving`) or roll back (flip `enabled` back
  // and surface an error). The user can fire several toggles in
  // quick succession; the latest PUT's response wins.
  const [saving, setSaving] = useState<Set<string>>(() => new Set());
  // Warnings that are not fatal — e.g. the server reports
  // toolset names it did not recognise. We show them as a
  // dismissable banner above the grid.
  const [warning, setWarning] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    api
      .getToolsets()
      .then((list) => {
        if (cancelled) return;
        setToolsets(list);
        // Seed the local enabled set from the server's view of
        // reality. Toolsets that the server reports as
        // `enabled: false` (because the persisted
        // `enabled_toolsets.json` excluded them) start out off
        // in the UI as well.
        setEnabled(new Set(list.filter((t) => t.enabled).map((t) => t.name)));
      })
      .catch((e) => {
        if (!cancelled) setError(failure("加载工具集失败", "请重试。", e instanceof Error ? e.message : undefined));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Optimistic toggle handler. Fires the PUT, then on success
  // reconciles the local set to whatever the server reports as
  // the canonical post-toggle state. On failure we roll the
  // local flip back and surface the error. We do *not* abort
  // earlier in-flight PUTs — the server is the source of
  // truth, and a late response simply overwrites the local
  // set with whatever the server has by then.
  const handleToggle = useCallback(
    async (name: string, next: boolean) => {
      const previous = enabled.has(name);
      if (previous === next) return; // no-op click

      // Optimistic flip + mark saving
      setEnabled((prev) => {
        const out = new Set(prev);
        if (next) out.add(name);
        else out.delete(name);
        return out;
      });
      setSaving((prev) => new Set(prev).add(name));
      setError(null);
      setWarning(null);

      // Build the next canonical set: start from current
      // local, apply this flip, then submit.
      const nextSet = new Set(enabled);
      if (next) nextSet.add(name);
      else nextSet.delete(name);

      try {
        const res = await api.updateEnabledToolsets(Array.from(nextSet));
        // Reconcile to the server's canonical answer. This
        // also picks up server-side changes to other toolsets
        // (shouldn't happen, but defends against drift).
        setEnabled(new Set(res.enabled));
        if (res.unknown.length > 0) {
          setWarning(
            `以下工具集名称未被识别：${res.unknown.join("、")}`,
          );
        }
      } catch (e) {
        // Roll back the optimistic flip
        setEnabled((prev) => {
          const out = new Set(prev);
          if (next) out.delete(name);
          else out.add(name);
          return out;
        });
        setError(
          e instanceof Error
            ? failure(SAVE_FAILED, "请重试。", e.message)
            : failure(SAVE_FAILED, "请重试。"),
        );
      } finally {
        setSaving((prev) => {
          const out = new Set(prev);
          out.delete(name);
          return out;
        });
      }
    },
    [enabled],
  );

  const q = query.trim().toLowerCase();
  const filtered = q
    ? toolsets.filter((t) =>
        `${t.name} ${t.label} ${t.description} ${t.tools.join(" ")}`.toLowerCase().includes(q),
      )
    : toolsets;

  const enabledCount = useMemo(() => toolsets.filter((t) => t.enabled).length, [toolsets]);

  return (
    /* 统一骨架：工具栏 → 统计 → 分组；间距交给 space-y-5。 */
    <div className="space-y-5">
      <SearchBar query={query} onQuery={onQuery} />

      {!loading && toolsets.length > 0 && (
        <SummaryBar
          total={toolsets.length}
          enabled={enabledCount}
          enabledLabel={ENABLED}
          totalLabel="个工具集"
        />
      )}

      {error && (
        <div className="px-3 py-2 rounded-lg bg-bg-card text-[length:var(--text-meta)] text-text-error flex items-start justify-between gap-2">
          <span>{error}</span>
          <button
            type="button"
            onClick={() => setError(null)}
            className="text-text-muted hover:text-text-primary text-[length:var(--text-meta)]"
            aria-label="关闭错误提示"
          >
            <X className="icon-sm" />
          </button>
        </div>
      )}
      {warning && (
        <div className="px-3 py-2 rounded-lg bg-bg-card text-[length:var(--text-meta)] text-text-warning flex items-start justify-between gap-2">
          <span>{warning}</span>
          <button
            type="button"
            onClick={() => setWarning(null)}
            className="text-text-muted hover:text-text-primary text-[length:var(--text-meta)]"
            aria-label="关闭警告"
          >
            <X className="icon-sm" />
          </button>
        </div>
      )}

      {loading ? (
        <SkeletonGrid count={3} />
      ) : filtered.length === 0 ? (
        <EmptyHint
          icon={Boxes}
          text={
            toolsets.length === 0
              ? "后端 tool registry 还是空的。重启后端或在 main.rs 注册更多工具集后会自动显示。"
              : "没有匹配的工具集。"
          }
        />
      ) : (
        <section>
          <ToolGroupTitle title="全部工具集" count={filtered.length} />
          <Grid empty={false}>
          {filtered.map((t) => (
            <ToolsetCard
              key={t.name}
              toolset={t}
              enabled={enabled.has(t.name)}
              saving={saving.has(t.name)}
              onToggle={handleToggle}
            />
          ))}
          </Grid>
        </section>
      )}

      <ToolHint>关闭后该组工具不再提供给模型。</ToolHint>
    </div>
  );
}

function ToolsetCard({
  toolset,
  enabled,
  saving,
  onToggle,
}: {
  toolset: ToolsetInfo;
  enabled: boolean;
  saving: boolean;
  onToggle: (name: string, next: boolean) => void;
}) {

  // 工具清单默认只露 3 个：塞 4+ 个时灰底药丸把卡片下半截糊满（2026-10-28 截图），
  // 「+N」展开是原型里确认过的交互，卡片自带这个能力，不需要另外加工具。
  const [expanded, setExpanded] = useState(false);
  const tools = toolset.tools;
  const shown = expanded ? tools : tools.slice(0, 3);
  const hidden = tools.length - shown.length;
  /* 后端没有真描述时吐的是 "approval category — 1 tools" 这种拼装串
     （2026-10-28 用户截图点名"粗糙"）——识别到该模式整行不渲染：
     工具数已经在标题旁（"N 个工具"），信息不丢，卡片也不掉档次。 */
  const desc = (toolset.description ?? "").trim();
  const realDescription = /^(.+)\s+category\s+—\s+\d+\s+tools?$/.test(desc) ? null : desc || null;
  return (
    <div className="flex flex-col gap-2 rounded-[var(--radius-lg)] bg-[var(--color-bg-card)] px-2.5 py-2.5">
      <div className="flex items-center gap-2.5">
        <InitialIcon name={toolset.label} />
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="truncate text-[length:var(--text-ui)] font-medium text-[var(--color-text-primary)]">
              {toolset.label}
            </span>
            <span className="shrink-0 text-[length:var(--text-tiny)] text-[var(--color-text-faint)]">
              {tools.length} 个工具
            </span>
          </div>
          {/* 单行截断：旧版 hover 展开会让整个网格重排（鼠标下方目标乱跑）。 */}
          {realDescription && (
            <p className="mt-0.5 truncate text-[length:var(--text-meta)] text-[var(--color-text-muted)]">
              {realDescription}
            </p>
          )}
        </div>
        <ToggleSwitch
          enabled={enabled}
          onToggle={() => onToggle(toolset.name, !enabled)}
          disabled={saving}
          offLabel={`关闭 ${toolset.label} 工具集`}
          onLabel={`启用 ${toolset.label} 工具集`}
        />
      </div>
      {tools.length > 0 && (
        /* 工具名与标题左对齐（28px 图标 + 10px 间距 = 38px），不缩进会显得
           这行是"卡片的第四个部分"而不是"标题的补充"。 */
        <div className="flex flex-wrap items-center gap-1 pl-[38px]">
          {shown.map((tool) => (
            <span
              key={tool}
              className="rounded-[var(--radius-sm)] bg-[var(--color-bg-tertiary)] px-1.5 py-0.5 font-mono text-[length:var(--text-tiny)] text-[var(--color-text-secondary)]"
            >
              {tool}
            </span>
          ))}
          {tools.length > 3 && (
            <button
              type="button"
              onClick={() => setExpanded((v) => !v)}
              aria-expanded={expanded}
              className="rounded-[var(--radius-sm)] px-1.5 py-0.5 text-[length:var(--text-tiny)] font-medium text-[var(--color-text-muted)] transition-colors hover:bg-[var(--color-bg-tertiary)] hover:text-[var(--color-text-primary)]"
            >
              {expanded ? "收起" : `+${hidden}`}
            </button>
          )}
        </div>
      )}
    </div>
  );
}
