import { useState, useEffect, useMemo } from "react";
import { BarChart3, TrendingUp, Cpu, DollarSign, Zap, Wrench, Database, Percent, Archive } from "lucide-react";
import { getAnalytics, type AnalyticsResponse } from "../api";

// Cache hit rate proxy: of all prompt tokens (freshly-written `input`
// + cache-served `cache_read`), what fraction came from the cache.
// Providers that don't report cache stats (MiniMax today) yield 0 —
// which is honestly "no cache hits observed". Defined at module scope
// so both `AnalyticsView` and the `DailyCurve` helper can use it.
function cacheHitRate(cacheRead: number, input: number): number {
  const denom = cacheRead + input;
  return denom > 0 ? (cacheRead / denom) * 100 : 0;
}

function hitRateColor(pct: number): string {
  return pct >= 80 ? "text-green-400" : pct >= 50 ? "text-amber-400" : "text-zinc-400";
}

export function AnalyticsView() {
  const [data, setData] = useState<AnalyticsResponse | null>(null);
  const [days, setDays] = useState(30);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    setLoading(true);
    getAnalytics(days)
      .then(setData)
      .catch(() => setData(null))
      .finally(() => setLoading(false));
  }, [days]);

  if (loading) {
    return (
      <div className="flex items-center justify-center h-full">
        <div className="text-text-faint">加载中...</div>
      </div>
    );
  }

  if (!data) {
    return (
      <div className="flex items-center justify-center h-full">
        <div className="text-text-faint">无法加载统计数据</div>
      </div>
    );
  }

  const { totals, daily, by_model, by_tool, skills } = data;

  const formatNumber = (n: number) => {
    if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
    if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
    return n.toString();
  };

  const formatCost = (n: number) => `$${n.toFixed(4)}`;

  // True when the analytics backend has been upgraded to write real
  // token usage (P1-3, 2026-06-27). Before that date the totals came
  // back as zeros and the user got a useless "you used 0 tokens" panel.
  // Detect by inspecting the totals vs session count — if there are
  // sessions but no tokens, it's a legacy install.
  const hasRealTokens = totals.total_sessions === 0
    || totals.total_input > 0
    || totals.total_output > 0
    || daily.some((d) => d.input_tokens > 0 || d.output_tokens > 0);

  return (
    <div className="h-full overflow-auto p-4 space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <BarChart3 className="w-5 h-5 text-accent" />
          <h2 className="text-lg font-semibold">使用统计</h2>
          {!hasRealTokens && totals.total_sessions > 0 && (
            <span className="text-xs text-text-faint ml-2">
              (token 数据从 2026-06-27 起收集)
            </span>
          )}
        </div>
        <select
          value={days}
          onChange={(e) => setDays(Number(e.target.value))}
          className="px-3 py-1.5 rounded-lg bg-bg-secondary border border-border-primary text-sm"
        >
          <option value={7}>近 7 天</option>
          <option value={30}>近 30 天</option>
          <option value={90}>近 90 天</option>
        </select>
      </div>

      {/* Totals Cards */}
      <div className="grid grid-cols-2 md:grid-cols-3 gap-3">
        <StatCard
          icon={<Cpu className="w-4 h-4" />}
          label="输入 Token"
          value={formatNumber(totals.total_input)}
          color="text-zinc-400"
        />
        <StatCard
          icon={<Zap className="w-4 h-4" />}
          label="输出 Token"
          value={formatNumber(totals.total_output)}
          color="text-green-400"
        />
        <StatCard
          icon={<TrendingUp className="w-4 h-4" />}
          label="会话数"
          value={totals.total_sessions.toString()}
          color="text-amber-400"
        />
        <StatCard
          icon={<DollarSign className="w-4 h-4" />}
          label="预估费用"
          value={formatCost(totals.total_estimated_cost)}
          color="text-yellow-400"
        />
        <StatCard
          icon={<Database className="w-4 h-4" />}
          label="缓存命中 Token"
          value={formatNumber(totals.total_cache_read)}
          color="text-green-400"
        />
        <StatCard
          icon={<Percent className="w-4 h-4" />}
          label="缓存命中率"
          value={`${cacheHitRate(totals.total_cache_read, totals.total_input).toFixed(1)}%`}
          color={hitRateColor(cacheHitRate(totals.total_cache_read, totals.total_input))}
        />
      </div>

      {/* P1-3: Daily time curve — pure SVG, zero deps. Renders input +
          output tokens stacked; sessions overlay as a line so the user
          can spot "high activity but low tokens" days (short chats). */}
      {daily.length > 0 && (
        <DailyCurve daily={daily} />
      )}

      {/* Model Usage */}
      {by_model.length > 0 && (
        <div className="bg-bg-secondary rounded-xl p-4 border border-border-primary">
          <h3 className="text-sm font-medium mb-3">模型使用</h3>
          <div className="space-y-2">
            {by_model.map((m) => (
              <div key={m.model} className="flex items-center justify-between text-sm">
                <span className="text-text-secondary truncate max-w-[200px]">{m.model}</span>
                <div className="flex items-center gap-4">
                  <span className="text-text-faint">{formatNumber(m.input_tokens + m.output_tokens)} tok</span>
                  <span className="text-text-faint">{m.sessions} 次</span>
                  <span className={hitRateColor(cacheHitRate(m.cache_read_tokens, m.input_tokens))}>
                    {cacheHitRate(m.cache_read_tokens, m.input_tokens).toFixed(0)}% 命中
                  </span>
                  <span className="text-accent">{formatCost(m.estimated_cost)}</span>
                </div>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* P1-3: Tool usage breakdown — sorted desc by call count, top 10. */}
      {by_tool.length > 0 && (
        <div className="bg-bg-secondary rounded-xl p-4 border border-border-primary">
          <div className="flex items-center gap-2 mb-3">
            <Wrench className="w-4 h-4 text-text-secondary" />
            <h3 className="text-sm font-medium">工具调用 Top 10</h3>
            <span className="text-xs text-text-faint">
              ({by_tool.reduce((acc, t) => acc + t.call_count, 0)} 总调用)
            </span>
          </div>
          <div className="space-y-2">
            {by_tool.slice(0, 10).map((t) => (
              <div key={t.tool} className="flex items-center justify-between text-sm">
                <span className="text-text-secondary truncate max-w-[200px]">{t.tool}</span>
                <div className="flex items-center gap-4">
                  <span className="text-text-faint">{formatNumber(t.call_count)} 次</span>
                  <span className="text-text-faint">{t.session_count} 会话</span>
                  <span className="text-accent">{t.daily_avg.toFixed(1)}/天</span>
                </div>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Skills Usage */}
      {skills.top_skills.length > 0 && (
        <div className="bg-bg-secondary rounded-xl p-4 border border-border-primary">
          <h3 className="text-sm font-medium mb-3">技能使用</h3>
          <div className="space-y-2">
            {skills.top_skills.map((s) => (
              <div key={s.skill} className="flex items-center justify-between text-sm">
                <span className="text-text-secondary">{s.skill}</span>
                <div className="flex items-center gap-4">
                  <span className="text-text-faint">{s.total_count} 次</span>
                  <span className="text-accent">{s.percentage.toFixed(1)}%</span>
                </div>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* E5 (Phase 3): compression telemetry. These numbers answer the
          user's "does compression lose my memory?" worry concretely:
          how many compressions ran, and how many history turns were
          preserved (folded into summaries), not discarded. The
          underlying mechanism keeps every archived turn as a real
          message row in a compression child-session, so 0 loss. */}
      <div className="bg-bg-secondary rounded-xl p-4 border border-border-primary">
        <div className="flex items-center gap-2 mb-3">
          <Archive className="w-4 h-4 text-text-secondary" />
          <h3 className="text-sm font-medium">上下文压缩</h3>
        </div>
        <div className="grid grid-cols-2 gap-3">
          <div>
            <div className="text-xs text-text-faint">压缩次数</div>
            <div className="text-lg font-semibold">{totals.total_compactions}</div>
          </div>
          <div>
            <div className="text-xs text-text-faint">保留的历史轮次</div>
            <div className="text-lg font-semibold">{formatNumber(totals.total_archived_turns)}</div>
          </div>
        </div>
        <p className="text-xs text-text-faint mt-3 leading-relaxed">
          每次压缩都会创建一个子会话，完整保留被折叠的原始对话（可经 lineage 树回看），不会丢失记忆。
        </p>
      </div>

      {/* Empty state */}
      {by_model.length === 0 && by_tool.length === 0 && skills.top_skills.length === 0 && (
        <div className="text-center text-text-faint py-8">
          暂无使用数据。开始对话后这里会显示统计信息。
        </div>
      )}
    </div>
  );
}

/**
 * P1-3: stacked bar + session line chart in pure SVG. We don't pull
 * in a chart library (d3, visx, recharts) — the data shape is small
 * and the user only needs "is today bigger than yesterday". Layout:
 *
 *   ┌────────────────────────────────────────────────────────┐
 *   │                                          ╷              │
 *   │  ▓▓ ▓▓ ▓▓ ░░ ▓▓ ▓▓ ░░ ▓▓ ▓▓ ▓▓ ▓▓         │ session    │
 *   │  ▓▓ ▓▓ ▓▓ ░░ ▓▓ ▓▓ ░░ ▓▓ ▓▓ ▓▓ ▓▓         │ line       │
 *   │                                                        │
 *   │  06-01    06-05    06-10    06-15    06-20    06-27    │
 *   └────────────────────────────────────────────────────────┘
 *      ▓ input tokens   ░ output tokens   ─ sessions
 */
function DailyCurve({ daily }: { daily: import("../api").AnalyticsDailyEntry[] }) {
  const width = 720;
  const height = 220;
  const margin = { top: 16, right: 24, bottom: 32, left: 48 };
  const innerW = width - margin.left - margin.right;
  const innerH = height - margin.top - margin.bottom;

  const maxTokens = useMemo(() => {
    const m = Math.max(
      1,
      ...daily.map((d) => d.input_tokens + d.output_tokens),
    );
    return m;
  }, [daily]);

  const maxSessions = useMemo(() => {
    const m = Math.max(1, ...daily.map((d) => d.sessions));
    return m;
  }, [daily]);

  const barCount = daily.length;
  const barWidth = barCount > 0 ? innerW / barCount : innerW;
  const barInnerWidth = Math.max(2, barWidth * 0.7);

  // X labels: show every Nth day so we don't crowd 90 ticks.
  const labelEvery = barCount > 30 ? 7 : barCount > 14 ? 3 : 1;

  return (
    <div className="bg-bg-secondary rounded-xl p-4 border border-border-primary">
      <div className="flex items-center justify-between mb-3">
        <h3 className="text-sm font-medium">Token 用量时间曲线</h3>
        <div className="flex items-center gap-3 text-xs text-text-faint">
          <span className="flex items-center gap-1">
            <span className="inline-block w-2 h-2 bg-zinc-400 rounded-sm" />输入
          </span>
          <span className="flex items-center gap-1">
            <span className="inline-block w-2 h-2 bg-green-400 rounded-sm" />输出
          </span>
          <span className="flex items-center gap-1">
            <span className="inline-block w-3 h-0.5 bg-amber-400" />会话
          </span>
          <span className="flex items-center gap-1">
            <span className="inline-block w-3 h-0.5 bg-accent" />缓存命中率
          </span>
        </div>
      </div>
      <svg viewBox={`0 0 ${width} ${height}`} className="w-full h-auto">
        {/* Y axis tokens */}
        <g transform={`translate(${margin.left}, ${margin.top})`}>
          {/* horizontal gridlines — 4 ticks */}
          {[0, 0.25, 0.5, 0.75, 1].map((p) => (
            <line
              key={p}
              x1={0}
              x2={innerW}
              y1={innerH * (1 - p)}
              y2={innerH * (1 - p)}
              stroke="currentColor"
              className="text-border-primary"
              strokeWidth={0.5}
              strokeDasharray={p === 0 ? "0" : "2,3"}
            />
          ))}
          {[0, 0.5, 1].map((p) => (
            <text
              key={p}
              x={-8}
              y={innerH * (1 - p) + 4}
              textAnchor="end"
              className="fill-text-faint"
              fontSize={10}
            >
              {formatTokensShort(maxTokens * p)}
            </text>
          ))}

          {/* bars */}
          {daily.map((d, i) => {
            const totalH = ((d.input_tokens + d.output_tokens) / maxTokens) * innerH;
            const inputH = (d.input_tokens / maxTokens) * innerH;
            const outputH = (d.output_tokens / maxTokens) * innerH;
            const x = i * barWidth + (barWidth - barInnerWidth) / 2;
            return (
              <g key={d.day}>
                <rect
                  x={x}
                  y={innerH - inputH}
                  width={barInnerWidth}
                  height={inputH}
                  className="fill-zinc-400"
                  fillOpacity={0.85}
                />
                <rect
                  x={x}
                  y={innerH - totalH}
                  width={barInnerWidth}
                  height={outputH}
                  className="fill-green-400"
                  fillOpacity={0.85}
                />
                {/* session line tick */}
                <circle
                  cx={x + barInnerWidth / 2}
                  cy={innerH - (d.sessions / maxSessions) * innerH}
                  r={2}
                  className="fill-amber-400"
                />
              </g>
            );
          })}

          {/* cache hit-rate line — 0..100% scale mapped to full height.
              Distinct from the session line (amber dots): this is the
              accent-coloured polyline so the user sees "cache saved X%
              of input" trending over time. */}
          <polyline
            fill="none"
            stroke="currentColor"
            className="text-accent"
            strokeWidth={1.5}
            strokeDasharray="3,2"
            points={daily
              .map((d, i) => {
                const hr = cacheHitRate(d.cache_read_tokens, d.input_tokens);
                const x = i * barWidth + barWidth / 2;
                const y = innerH - (hr / 100) * innerH;
                return `${x},${y}`;
              })
              .join(" ")}
          />

          {/* x labels */}
          {daily.map((d, i) => {
            if (i % labelEvery !== 0 && i !== daily.length - 1) return null;
            const x = i * barWidth + barWidth / 2;
            const label = d.day.slice(5); // MM-DD
            return (
              <text
                key={`x-${d.day}`}
                x={x}
                y={innerH + 18}
                textAnchor="middle"
                className="fill-text-faint"
                fontSize={10}
              >
                {label}
              </text>
            );
          })}
        </g>
      </svg>
    </div>
  );
}

function formatTokensShort(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(0)}K`;
  return n.toFixed(0);
}

function StatCard({
  icon,
  label,
  value,
  color,
}: {
  icon: React.ReactNode;
  label: string;
  value: string;
  color: string;
}) {
  return (
    <div className="bg-bg-secondary rounded-xl p-3 border border-border-primary">
      <div className="flex items-center gap-2 mb-1">
        <span className={color}>{icon}</span>
        <span className="text-xs text-text-faint">{label}</span>
      </div>
      <div className="text-lg font-semibold">{value}</div>
    </div>
  );
}