// ToolsetsTab.tsx —— 工具集 Tab：后端工具注册表分组（R2-4 拆分第五步）。
import { useCallback, useEffect, useMemo, useState } from "react";
import { Boxes, X } from "lucide-react";
import * as api from "../../api";
import type { ToolsetInfo } from "../../api";
import { pickIconByName } from "./toolTabsData";
import { ToolCardIcon } from "./ToolCardIcon";
import { MgmtTag } from "./MgmtTag";
import { ToggleSwitch } from "../ToggleSwitch";
import { SearchBar, Grid, EmptyHint, SkeletonGrid, SummaryBar } from "./toolTabsShared";
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
    <>
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
        <div className="mb-4 px-3 py-2 rounded-lg bg-bg-card text-[length:var(--text-meta)] text-text-error flex items-start justify-between gap-2">
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
        <div className="mb-4 px-3 py-2 rounded-lg bg-bg-card text-[length:var(--text-meta)] text-text-warning flex items-start justify-between gap-2">
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
      )}

      <p className="text-[length:var(--text-meta)] text-text-muted pb-6">工具集由后端 ToolRegistry 自动注册；关闭后该组的所有工具会从 LLM 的工具列表里消失，下次 agent 调用就不会看到。</p>
    </>
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
  const Icon = pickIconByName(toolset.name);
  // 工具清单默认只露 4 个：8 个工具的工具集一屏塞满会把网格撑得很乱。
  // 「全部工具」展开是原型里确认过的交互，卡片自带这个能力，不需要另外加工具。
  const [expanded, setExpanded] = useState(false);
  const tools = toolset.tools;
  const shown = expanded ? tools : tools.slice(0, 4);
  const hidden = tools.length - shown.length;
  return (
    <div className="m-lift group flex flex-col gap-2.5 px-3 py-3 rounded-xl bg-bg-card hover:bg-bg-card-hover">
      <div className="flex items-start gap-3">
        <ToolCardIcon category={toolset.label}>
          <Icon className="icon-lg" />
        </ToolCardIcon>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2 flex-wrap">
            <span className="text-[length:var(--text-ui)] font-medium text-text-primary truncate">{toolset.label}</span>
            <MgmtTag tone="neutral">{tools.length} 个工具</MgmtTag>
          </div>
          <p className="text-[length:var(--text-meta)] text-text-muted mt-0.5 line-clamp-2 group-hover:line-clamp-none">
            {toolset.description || "（无描述）"}
          </p>
          {tools.length > 0 && (
            <div className="flex flex-wrap gap-1 mt-1.5">
              {shown.map((tool) => (
                <span
                  key={tool}
                  className="font-mono text-[length:var(--text-tiny)] text-text-secondary bg-[color-mix(in_srgb,var(--color-text-primary)_5%,transparent)] rounded px-1.5 py-0.5"
                >
                  {tool}
                </span>
              ))}
              {hidden > 0 && !expanded && (
                <span className="text-[length:var(--text-tiny)] text-text-muted self-center">+{hidden}</span>
              )}
            </div>
          )}
          {tools.length > 4 && (
            <button
              type="button"
              onClick={() => setExpanded((v) => !v)}
              className="mt-1.5 text-[length:var(--text-tiny)] font-medium text-text-muted hover:text-text-primary"
              aria-expanded={expanded}
            >
              {expanded ? "收起" : "全部工具"}
            </button>
          )}
        </div>
        <ToggleSwitch
          enabled={enabled}
          onToggle={() => onToggle(toolset.name, !enabled)}
          disabled={saving}
          offLabel={`关闭 ${toolset.label} 工具集`}
          onLabel={`启用 ${toolset.label} 工具集`}
          className="shrink-0 mt-0.5"
        />
      </div>
    </div>
  );
}
