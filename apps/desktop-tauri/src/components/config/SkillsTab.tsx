// SkillsTab.tsx —— 技能 Tab：GET /api/skills + 启停（R2-4 拆分第七步）。
import { useCallback, useEffect, useMemo, useState } from "react";
import { RefreshCw, Sparkles } from "lucide-react";
import * as api from "../../api";
import type { SkillInfo } from "../../api";
import { CATEGORY_LABELS } from "./toolTabsData";
import { SkillCard } from "./SkillCard";
import { SearchBar, EmptyHint, SkeletonGrid, SummaryBar, ToolGroupTitle, ToolHint } from "./toolTabsShared";
import { ALL, ENABLED, failure } from "../strings";

// ── Skills tab: GET /api/skills + toggle ────────────────────────────

export function SkillsTab({ query, onQuery }: { query: string; onQuery: (q: string) => void }) {
  const [skills, setSkills] = useState<SkillInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  /** 分类 chip 筛选。null = 全部。搜索框管关键词，这个管分组。 */
  const [catFilter, setCatFilter] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const list = await api.getSkills();
      setSkills(list);
    } catch (e) {
      setError(failure("加载技能失败", "请重试。", e instanceof Error ? e.message : undefined));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const q = query.trim().toLowerCase();
  // 关键词与分类 chip 是两个独立维度：先按词筛，再按分类筛。
  // chip 上的计数取**分类筛之前**的结果，否则选中一个分类后其它 chip 全部归零。
  const byQuery = q
    ? skills.filter((s) =>
        // 带上 display_name：界面显示的是中文名，用户按看到的字搜才搜得到
        `${s.name} ${s.display_name ?? ""} ${s.description} ${s.category}`
          .toLowerCase()
          .includes(q),
      )
    : skills;
  const filtered = catFilter
    ? byQuery.filter((s) => (s.category || "uncategorized") === catFilter)
    : byQuery;

  // Group filtered skills by category
  const grouped = useMemo(() => {
    const map: Record<string, SkillInfo[]> = {};
    for (const s of filtered) {
      const key = s.category || "uncategorized";
      if (!map[key]) map[key] = [];
      map[key].push(s);
    }
    return map;
  }, [filtered]);

  const catLabel = useCallback((key: string) => CATEGORY_LABELS[key] || key, []);

  // chip 计数基于 byQuery（未按分类筛），否则选中任一分类后其余 chip 全部归零，
  // 用户会以为其它分类没有技能。
  const countsByCat = useMemo(() => {
    const m: Record<string, number> = {};
    for (const s of byQuery) {
      const k = s.category || "uncategorized";
      m[k] = (m[k] ?? 0) + 1;
    }
    return m;
  }, [byQuery]);

  // 分组顺序取自 countsByCat（未按分类筛）—— 否则选中一个分类后，
  // chip 行会只剩一个 chip，用户就没法切换到别的分类了。
  const categoryOrder = useMemo(() => {
    const priority = ["software-development", "devops", "productivity", "finance", "lifestyle"];
    const keys = Object.keys(countsByCat);
    return priority.filter((k) => keys.includes(k)).concat(
      keys.filter((k) => !priority.includes(k)).sort(),
    );
  }, [countsByCat]);

  const enabledCount = useMemo(() => skills.filter((s) => s.enabled).length, [skills]);

  return (
    /* 统一骨架：工具栏 → 筛选 chips → 统计 → 分类分组；间距交给 space-y-5。 */
    <div className="space-y-5">
      <div className="flex items-center gap-2">
        <div className="flex-1">
          <SearchBar query={query} onQuery={onQuery} />
        </div>
        <button
          onClick={refresh}
          disabled={loading}
          className="btn-secondary btn-sm flex-shrink-0"
          title="刷新技能列表"
          aria-label="刷新技能列表">
          <RefreshCw className={`icon-md ${loading ? "animate-spin" : ""}`} />
        </button>
      </div>

      {/* 分类 chip：搜索框只能筛关键词，分组再多时仍需一键跨组跳。
          无描边药丸——未选中=浅灰实心，选中=主色实心。
          单行横向滚动（2026-10-28）：flex-wrap 在 10+ 个分类时会把最后一两个
          挤成孤行（截图里「写作 1」单独吊在第二行），比溢出更难看；
          滚动条按全局样式隐藏，滚轮/触控板横扫即可。 */}
      {!loading && categoryOrder.length > 1 && (
        <div className="flex items-center gap-1.5 flex-nowrap overflow-x-auto [-ms-overflow-style:none] [scrollbar-width:none] [&::-webkit-scrollbar]:hidden">
          <button
            onClick={() => setCatFilter(null)}
            aria-pressed={catFilter === null}
            className={`chip-flat${catFilter === null ? " chip-flat--on" : ""}`}
          >
            {ALL} <span className="ml-0.5 tabular-nums opacity-65">{byQuery.length}</span>
          </button>
          {categoryOrder.map((c) => (
            <button
              key={c}
              onClick={() => setCatFilter(catFilter === c ? null : c)}
              aria-pressed={catFilter === c}
              className={`chip-flat${catFilter === c ? " chip-flat--on" : ""}`}
            >
              {catLabel(c)} <span className="ml-0.5 tabular-nums opacity-65">{countsByCat[c] ?? 0}</span>
            </button>
          ))}
        </div>
      )}

      {!loading && skills.length > 0 && (
        <SummaryBar
          total={skills.length}
          enabled={enabledCount}
          enabledLabel={ENABLED}
          totalLabel="个技能"
        />      )}

      {error && (
        <div className="px-3 py-2 rounded-lg bg-bg-card text-[length:var(--text-meta)] text-text-error">
          {error}
        </div>
      )}

      {loading ? (
        <SkeletonGrid count={4} />
      ) : filtered.length === 0 ? (
        <EmptyHint
          icon={Sparkles}
          text={
            skills.length === 0
              ? "后端还没有加载任何技能。把 SKILL.md 放到 skills 目录下后会自动出现。"
              : "没有匹配的技能。"
          }
        />
      ) : (
        categoryOrder.map((cat) => (
          <section key={cat} className="@container">
            <ToolGroupTitle title={catLabel(cat)} count={grouped[cat].length} />
            {/* 容器查询：设置面板固定 ~760px，`xl:`（视口 1280）曾在这里
                强行放三列，卡片被压到 240px。 */}
            <div className="grid grid-cols-1 gap-2 @2xl:grid-cols-2 @5xl:grid-cols-3">
              {grouped[cat].map((s) => (
                <SkillCard key={s.name} skill={s} catLabel={catLabel} onToggle={(name) => {
                  setSkills((prev) => prev.map((p) => p.name === name ? { ...p, enabled: !p.enabled } : p));
                }} />
              ))}
            </div>
          </section>
        ))
      )}

      <ToolHint>禁用会从文件系统中删除该技能。</ToolHint>
    </div>
  );
}

// ── SkillCard: self-contained expand + toggle ───────────────────