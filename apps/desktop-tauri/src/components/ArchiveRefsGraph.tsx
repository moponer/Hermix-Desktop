/**
 * Hand-rolled SVG timeline for the v2 archive refs (TencentDB Agent
 * Memory 借鉴 Mermaid Canvas 的本地化复刻 — see
 * `docs/MEMORY_ARCHITECTURE_COMPARISON.md` §3).
 *
 * Design notes:
 * - **No `mermaid` dependency.** The cross-project "做好自己" rule says
 *   keep the bundle lean (gzip ≤ 8 KB). ~120 lines of SVG beats 200 KB
 *   of mermaid + d3.
 * - **Horizontal timeline.** Refs come back from
 *   `getSessionArchiveRefs` in push-order = user/assistant alternation.
 *   User bubbles float above the baseline, assistant bubbles below —
 *   the eye can scan the alternation as a conversation rhythm.
 * - **Circle radius ∝ sqrt(charCount).** Linear scaling would let one
 *   50k-char paste dominate the canvas; sqrt keeps shape readable.
 * - **Single accent color, alpha-only variation.** Honours the visual
 *   iron rule "渐变色禁止" — no `linear-gradient`, no `bg-gradient-to-*`.
 * - **Click copies the file path with anchor** (same behaviour as the
 *   list view, so the two modes stay interchangeable).
 * - **Pure presentational.** All data flows in via props; no internal
 *   state beyond the hover tooltip.
 */
import { useMemo, useState } from "react";
import type { ArchiveRef } from "../api";

const BASELINE_Y = 80;
const TOP_Y = 30;
const BOTTOM_Y = 130;
const MIN_R = 6;
const R_K = 0.5; // sqrt(charCount) * R_K

function estimateRole(filePath: string): "user" | "assistant" | "other" {
  // Archive filename pattern: `HHMMSS-<hash>.md`. We don't have the
  // role in the ref shape today, so fall back to the order-based
  // alternation the parent already uses. The parent splits refs by
  // pushing `node:user-*` / `node:assistant-*` prefixes — surface
  // that through the ref's anchor. Until that's wired, we colour all
  // bubbles the same accent and rely on the index-based alternation
  // for vertical placement.
  const name = filePath.split(/[\\/]/).pop() ?? "";
  if (/user/i.test(name)) return "user";
  if (/assistant|asst|ai/i.test(name)) return "assistant";
  return "other";
}

export function ArchiveRefsGraph({ refs }: { refs: ArchiveRef[] }) {
  const [hover, setHover] = useState<string | null>(null);
  const [copiedId, setCopiedId] = useState<string | null>(null);

  const layout = useMemo(() => {
    if (refs.length === 0) {
      return { width: 320, points: [] as Array<{ ref: ArchiveRef; cx: number; cy: number; r: number; above: boolean }> };
    }
    const padX = 24;
    const usableW = Math.max(80, refs.length > 1 ? (refs.length - 1) * 60 : 60);
    const width = padX * 2 + usableW;
    const points = refs.map((r, i) => {
      const cx = refs.length > 1 ? padX + (usableW * i) / (refs.length - 1) : width / 2;
      const above = i % 2 === 0; // user-side floats above; alternates down
      const r2 = MIN_R + Math.sqrt(r.charCount) * R_K * 0.5;
      const cy = above ? TOP_Y + r2 : BOTTOM_Y - r2;
      return { ref: r, cx, cy, r: r2, above };
    });
    return { width, points };
  }, [refs]);

  if (refs.length === 0) {
    return (
      <div
        className="px-3 py-2"
        style={{ fontSize: 11, color: "var(--color-text-secondary)" }}
      >
        暂无归档节点
      </div>
    );
  }

  const handleCopy = async (ref: ArchiveRef) => {
    const anchor =
      ref.lineRange && ref.lineRange[0] === ref.lineRange[1]
        ? `#L${ref.lineRange[0]}`
        : ref.lineRange
          ? `#L${ref.lineRange[0]}-L${ref.lineRange[1]}`
          : "";
    const target = `${ref.filePath}${anchor}`;
    try {
      await navigator.clipboard.writeText(target);
      setCopiedId(ref.nodeId);
      setTimeout(
        () => setCopiedId((cur) => (cur === ref.nodeId ? null : cur)),
        1200,
      );
    } catch {
      // Same fallback as the list view — clipboard may be unavailable
      // in some Tauri sandboxes; the SVG tooltip still shows the path.
    }
  };

  const accent = "var(--color-accent, #475569)";
  const accentSoft = "var(--color-accent-soft, rgba(71,85,105,0.18))";

  return (
    <div className="px-3 py-2">
      <div
        className="overflow-x-auto"
        style={{ borderRadius: 6, border: "1px solid var(--color-border)" }}
      >
        <svg
          role="img"
          aria-label={`归档时间线，共 ${refs.length} 条消息`}
          viewBox={`0 0 ${layout.width} 160`}
          width={layout.width}
          height={160}
          style={{ display: "block", maxWidth: "100%" }}
        >
          {/* Baseline */}
          <line
            x1={0}
            y1={BASELINE_Y}
            x2={layout.width}
            y2={BASELINE_Y}
            stroke="var(--color-border)"
            strokeWidth={1}
            strokeDasharray="3 3"
          />
          {/* Alternation connector — user↔assistant rhythm */}
          {layout.points.map((p, i) => {
            const next = layout.points[i + 1];
            if (!next) return null;
            return (
              <line
                key={`conn-${p.ref.nodeId}`}
                x1={p.cx}
                y1={p.cy}
                x2={next.cx}
                y2={next.cy}
                stroke="var(--color-border)"
                strokeWidth={1}
                opacity={0.6}
              />
            );
          })}
          {/* Nodes */}
          {layout.points.map((p) => {
            const isHover = hover === p.ref.nodeId;
            const isCopied = copiedId === p.ref.nodeId;
            const role = estimateRole(p.ref.filePath);
            return (
              <g
                key={p.ref.nodeId}
                onMouseEnter={() => setHover(p.ref.nodeId)}
                onMouseLeave={() =>
                  setHover((cur) => (cur === p.ref.nodeId ? null : cur))
                }
                onClick={() => handleCopy(p.ref)}
                style={{ cursor: "pointer" }}
              >
                <circle
                  cx={p.cx}
                  cy={p.cy}
                  r={p.r}
                  fill={isCopied ? "var(--color-success)" : isHover ? accent : accentSoft}
                  stroke={accent}
                  strokeWidth={isHover ? 2 : 1}
                  opacity={isHover ? 1 : 0.85}
                />
                {/* Side label: node_id (truncated) + charCount */}
                <text
                  x={p.cx}
                  y={p.above ? p.cy - p.r - 4 : p.cy + p.r + 10}
                  textAnchor="middle"
                  fontSize={9}
                  fontFamily="ui-monospace, SFMono-Regular, Menlo, monospace"
                  fill="var(--color-text-secondary)"
                >
                  {p.ref.nodeId.slice(0, 6)}
                  {p.ref.charCount > 0 && (
                    <tspan dx={4} fill="var(--color-text-tertiary, var(--color-text-secondary))">
                      {p.ref.charCount >= 1000
                        ? `${(p.ref.charCount / 1000).toFixed(1)}k`
                        : p.ref.charCount}
                    </tspan>
                  )}
                </text>
                {/* Hover tooltip */}
                {isHover && (
                  <g>
                    <rect
                      x={Math.max(0, p.cx - 90)}
                      y={p.above ? p.cy + p.r + 2 : p.cy - p.r - 26}
                      width={180}
                      height={22}
                      rx={4}
                      fill="var(--color-bg-elevated, #1e1f24)"
                      stroke="var(--color-border)"
                      strokeWidth={1}
                      opacity={0.95}
                    />
                    <text
                      x={Math.max(6, p.cx - 84)}
                      y={p.above ? p.cy + p.r + 16 : p.cy - p.r - 12}
                      fontSize={10}
                      fontFamily="ui-monospace, SFMono-Regular, Menlo, monospace"
                      fill="var(--color-text-primary)"
                    >
                      {role === "user" ? "↑ " : role === "assistant" ? "↓ " : "• "}
                      node:{p.ref.nodeId}
                      {p.ref.lineRange && (
                        <tspan dx={4} fill="var(--color-text-secondary)">
                          L{p.ref.lineRange[0]}
                          {p.ref.lineRange[0] !== p.ref.lineRange[1]
                            ? `-L${p.ref.lineRange[1]}`
                            : ""}
                        </tspan>
                      )}
                    </text>
                  </g>
                )}
              </g>
            );
          })}
        </svg>
      </div>
      <p
        className="mt-1"
        style={{ fontSize: 10, color: "var(--color-text-secondary)" }}
      >
        点击节点复制路径 · 圆面积 ∝ √(字符数) · 左上偶数位 / 右下奇数位交替
      </p>
    </div>
  );
}
