import { useState, useEffect, useMemo } from "react";
import { BarChart3, TrendingUp, Cpu, DollarSign, Zap, Wrench, Database, Percent, Archive, AlertTriangle, Bug } from "lucide-react";
import {
  getAnalytics,
  getAnalyticsDebug,
  type AnalyticsResponse,
  type AnalyticsDebugResponse,
} from "../api";

// Cache hit rate proxy: of all prompt tokens (freshly-written `input`
// + cache-served `cache_read`), what fraction came from the cache.
// `effectiveHitRate` below prefers this real ratio, but falls back to
// a backend-provided estimate when the provider under-reports cache
// reads, and returns `null` when there is no token data at
// all so the UI can show "—" instead of a misleading 0%.
function cacheHitRate(cacheRead: number, input: number): number {
  const denom = cacheRead + input;
  return denom > 0 ? (cacheRead / denom) * 100 : 0;
}

/**
 * Effective cache-hit rate (0..100) for a totals/model/daily entry.
 *  • real data present  → real cache_read / (cache_read + input) ratio.
 *  • no real cache reads → backend `estimated` fraction (0..1) × 100.
 *  • no token data at all → `null` (caller shows "—").
 */
function effectiveHitRate(
  cacheRead: number,
  input: number,
  estimated?: number,
): number | null {
  if (cacheRead > 0 || input > 0) {
    return cacheHitRate(cacheRead, input);
  }
  if (estimated != null) return estimated * 100;
  return null;
}

function hitRateColor(pct: number): string {
  return pct >= 80 ? "text-green-400" : pct >= 50 ? "text-amber-400" : "text-zinc-400";
}

function hitRateLabel(pct: number | null): string {
  return pct == null ? "—" : `${pct.toFixed(1)}%`;
}

export function AnalyticsView() {
  const [data, setData] = useState<AnalyticsResponse | null>(null);
  const [days, setDays] = useState(30);
  const [loading, setLoading] = useState(true);
  // Surface the real backend error instead of a generic
  // "无法加载统计数据". `null` means "loaded fine (or no data)".
  const [lastError, setLastError] = useState<string | null>(null);

  // diagnostic panel state.
  const [debugOpen, setDebugOpen] = useState(false);
  const [debug, setDebug] = useState<AnalyticsDebugResponse | null>(null);
  const [debugLoading, setDebugLoading] = useState(false);
  const [debugError, setDebugError] = useState<string | null>(null);

  useEffect(() => {
    setLoading(true);
    setLastError(null);
    getAnalytics(days)
      .then(setData)
      .catch((e: unknown) => {
        setData(null);
        setLastError(extractErrorCause(e));
      })
      .finally(() => setLoading(false));
  }, [days]);

  const loadDebug = () => {
    setDebugLoading(true);
    setDebugError(null);
    getAnalyticsDebug()
      .then(setDebug)
      .catch((e: unknown) => setDebugError(extractErrorCause(e)))
      .finally(() => setDebugLoading(false));
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-full">
        <div className="text-text-faint">加载中...</div>
      </div>
    );
  }

  // a real backend error — show it loudly so the user can act.
  if (lastError) {
    return (
      <div className="h-full overflow-auto p-4 space-y-4">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <BarChart3 className="w-5 h-5 text-accent" />
            <h2 className="text-lg font-semibold">使用统计</h2>
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
        <div className="rounded-xl border border-red-500/40 bg-red-500/5 p-4">
          <div className="flex items-start gap-3">
            <AlertTriangle className="w-5 h-5 text-red-400 mt-0.5 shrink-0" />
            <div className="min-w-0">
              <div className="text-sm font-medium text-red-300">统计数据加载失败</div>
              <div className="mt-1 text-xs text-red-200/80 break-words whitespace-pre-wrap font-mono">
                {lastError}
              </div>
              <button
                onClick={() => setDebugOpen(true)}
                className="mt-3 inline-flex items-center gap-1.5 text-xs px-2.5 py-1.5 rounded-lg bg-bg-secondary border border-border-primary hover:border-red-500/40"
              >
                <Bug className="w-3.5 h-3.5" /> 打开诊断面板
              </button>
            </div>
          </div>
        </div>
        {debugOpen && (
          <DebugPanel
            debug={debug}
            loading={debugLoading}
            error={debugError}
            onClose={() => setDebugOpen(false)}
            onReload={loadDebug}
          />
        )}
      </div>
    );
  }

  if (!data) {
    return (
      <div className="flex items-center justify-center h-full">
        <div className="text-text-faint">暂无统计数据</div>
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
  // back as zeros and the user got a useless "you used 0 tokens" panel.
  // Detect by inspecting the totals vs session count — if there are
  // sessions but no tokens, it's a legacy install (now back-filled by
  // the message-content estimate, so this is rarely true anymore).
  const hasRealTokens = totals.total_sessions === 0
    || totals.total_input > 0
    || totals.total_output > 0
    || daily.some((d) => d.input_tokens > 0 || d.output_tokens > 0);

  const totalsHit = effectiveHitRate(
    totals.total_cache_read,
    totals.total_input,
    totals.estimated_cache_hit_rate,
  );

  return (
    <div className="h-full overflow-auto p-4 space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <BarChart3 className="w-5 h-5 text-accent" />
          <h2 className="text-lg font-semibold">使用统计</h2>
          {!hasRealTokens && totals.total_sessions > 0 && (
            <span className="text-xs text-text-faint ml-2">
              (token 数据从 起收集)
            </span>
          )}
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={() => {
              setDebugOpen(true);
              if (!debug) loadDebug();
            }}
            title="诊断统计数据库"
            className="p-1.5 rounded-lg bg-bg-secondary border border-border-primary hover:border-accent/50"
          >
            <Bug className="w-4 h-4 text-text-secondary" />
          </button>
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
          value={hitRateLabel(totalsHit)}
          color={totalsHit == null ? "text-text-faint" : hitRateColor(totalsHit)}
        />
      </div>

      {/* Daily time curve — pure SVG, zero deps. Renders input +
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
            {by_model.map((m) => {
              const hit = effectiveHitRate(
                m.cache_read_tokens,
                m.input_tokens,
                m.estimated_cache_hit_rate,
              );
              const estNote = m.estimated_cache_hit_rate != null && m.cache_read_tokens === 0
                ? " (估算)"
                : "";
              return (
                <div key={m.model} className="flex items-center justify-between text-sm">
                  <span className="text-text-secondary truncate max-w-[200px]">{m.model}</span>
                  <div className="flex items-center gap-4">
                    <span className="text-text-faint">{formatNumber(m.input_tokens + m.output_tokens)} tok</span>
                    <span className="text-text-faint">{m.sessions} 次</span>
                    <span className={hit == null ? "text-text-faint" : hitRateColor(hit)}>
                      {hitRateLabel(hit)}{estNote}
                    </span>
                    <span className="text-accent">{formatCost(m.estimated_cost)}</span>
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      )}

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

      {/* E5: compression telemetry. These numbers answer the
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

      {/* diagnostic panel (toggled from the header bug button
          or the error banner). */}
      {debugOpen && (
        <DebugPanel
          debug={debug}
          loading={debugLoading}
          error={debugError}
          onClose={() => setDebugOpen(false)}
          onReload={loadDebug}
        />
      )}
    </div>
  );
}

/** Pull a human-readable cause out of a failed `fetchJSON`. The backend
 *  returns `AnalyticsErrorResponse { error, cause }` on 500; otherwise
 *  we show the raw status text. */
function extractErrorCause(e: unknown): string {
  if (e instanceof Error) {
    try {
      const parsed = JSON.parse(e.message) as { error?: string; cause?: string };
      if (parsed.cause) return `${parsed.error ?? "error"}: ${parsed.cause}`;
      if (parsed.error) return parsed.error;
    } catch {
      // not JSON — fall through
    }
    return e.message;
  }
  return String(e);
}

/** one-shot diagnostic of the stats DB. */
function DebugPanel({
  debug,
  loading,
  error,
  onClose,
  onReload,
}: {
  debug: AnalyticsDebugResponse | null;
  loading: boolean;
  error: string | null;
  onClose: () => void;
  onReload: () => void;
}) {
  return (
    <div className="rounded-xl border border-border-primary bg-bg-secondary p-4 space-y-3">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Bug className="w-4 h-4 text-text-secondary" />
          <h3 className="text-sm font-medium">统计诊断</h3>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={onReload}
            className="text-xs px-2.5 py-1 rounded-lg border border-border-primary hover:border-accent/50"
          >
            刷新
          </button>
          <button
            onClick={onClose}
            className="text-xs px-2.5 py-1 rounded-lg border border-border-primary hover:border-accent/50"
          >
            关闭
          </button>
        </div>
      </div>
      {loading && <div className="text-xs text-text-faint">读取数据库...</div>}
      {error && (
        <div className="text-xs text-red-300 font-mono break-words whitespace-pre-wrap">{error}</div>
      )}
      {debug && (
        <div className="space-y-3">
          <div className="text-xs text-text-faint break-all">
            <span className="text-text-secondary">DB:</span> {debug.db_path}
          </div>
          <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
            {(["sessions", "messages", "token_usage"] as const).map((name) => {
              const t = debug.tables[name];
              return (
                <div key={name} className="rounded-lg border border-border-primary p-2.5 text-xs">
                  <div className="font-medium text-text-secondary mb-1">{name}</div>
                  <div className="text-text-faint">
                    存在: {t.exists ? "是" : "否"} · 行数: {t.row_count ?? "—"}
                  </div>
                  {t.error && <div className="text-red-300 mt-1 break-words">{t.error}</div>}
                  {t.sample.length > 0 && (
                    <pre className="mt-1.5 max-h-40 overflow-auto text-[10px] text-text-faint whitespace-pre-wrap">
                      {JSON.stringify(t.sample, null, 2)}
                    </pre>
                  )}
                </div>
              );
            })}
          </div>
          <div className="text-xs">
            <div className="font-medium text-text-secondary mb-1">迁移 (schema_migrations)</div>
            {debug.migrations.length === 0 ? (
              <div className="text-text-faint">无记录</div>
            ) : (
              <div className="space-y-0.5 font-mono text-text-faint">
                {debug.migrations.map((m) => (
                  <div key={m.id}>
                    {m.id} · {m.applied_at ?? "—"} · {m.status ?? "ok"}
                  </div>
                ))}
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

/**
 * Stacked bar + session line chart in pure SVG. We don't pull
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
              of input" trending over time. Uses the estimated rate as a
              fallback when a day reported 0 real cache reads. */}
          <polyline
            fill="none"
            stroke="currentColor"
            className="text-accent"
            strokeWidth={1.5}
            strokeDasharray="3,2"
            points={daily
              .map((d, i) => {
                const hr = effectiveHitRate(
                  d.cache_read_tokens,
                  d.input_tokens,
                  d.estimated_cache_hit_rate,
                );
                const x = i * barWidth + barWidth / 2;
                const y = innerH - ((hr ?? 0) / 100) * innerH;
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
