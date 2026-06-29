import { useEffect, useState } from "react";
import { ShieldAlert, Check, X, Terminal, Globe, Wrench, Clock, Timer } from "lucide-react";
import type { ApprovalDecision } from "../types";
import type { ApprovalMessageMeta } from "../types";

export type ApprovalRequestShape = ApprovalMessageMeta;

export interface InlineApprovalCardProps {
  request: ApprovalRequestShape;
  onApprove: (requestId: string) => void;
  onDeny: (requestId: string) => void;
  resolved?: ApprovalDecision | null;
}

/** Below this many seconds remaining, the countdown pill switches
 *  from neutral-gray "剩余 Xm Ys" to error-soft pulsing red
 *  "即将超时 0m Ys" — matching the design language of the
 *  "已超时" terminal pill. The threshold (30s) is intentionally
 *  tied to the *user's* attention budget, not the server's
 *  auto-deny deadline: under half a minute is the point at which
 *  a quick read-or-skip judgment feels appropriate. */
const COUNTDOWN_DANGER_THRESHOLD_MS = 30_000;

const RISK_BADGE: Record<string, { label: string; cls: string }> = {
  dangerous: { label: "高危", cls: "approval-pill approval-pill-dangerous" },
  caution: { label: "中危", cls: "approval-pill approval-pill-caution" },
  safe: { label: "低危", cls: "approval-pill approval-pill-safe" },
};

function shortArgs(args: unknown): string {
  if (args == null) return "";
  if (typeof args === "string") return args.slice(0, 120);
  try {
    const s = JSON.stringify(args);
    return s.slice(0, 120) + (s.length > 120 ? "…" : "");
  } catch {
    return String(args).slice(0, 120);
  }
}

function toolIcon(tool: string) {
  const t = tool.toLowerCase();
  if (t.includes("terminal") || t.includes("shell") || t.includes("exec") || t.includes("bash")) return Terminal;
  if (t.includes("browser") || t.includes("web") || t.includes("http") || t.includes("fetch")) return Globe;
  return Wrench;
}

/**
 * Redesigned approval card — max 2 lines, no markdown formatting,
 * clear visual hierarchy, color-coded action buttons.
 *
 * Line 1: Tool name pill + risk badge + status pill
 * Line 2: Short args text (truncated) + action buttons
 */
/** Format a millisecond duration as `4m 32s` / `0m 12s` / `0m 0s`.
 *  The compact `Xm Ys` shape is intentional — a decimal
 *  ("4.5 min remaining") would (a) require a wider pill, (b) be
 *  harder to scan at a glance, and (c) feel like a precise
 *  measurement when in fact the timer is grainy to the second.
 *  `0s` is normalized to `0m 0s` so the layout never shifts
 *  between digits. */
function formatCountdown(ms: number): string {
  const totalSec = Math.max(0, Math.round(ms / 1000));
  const m = Math.floor(totalSec / 60);
  const s = totalSec % 60;
  return `${m}m ${s.toString().padStart(2, "0")}s`;
}

/** Live countdown hook. Re-renders once per second while the
 *  deadline is still in the future; switches to 1Hz re-render even
 *  after it lapses (so the pill can hide the moment
 *  `resolved === "expired"` arrives). Returns `null` when no
 *  deadline is present, so the caller can skip the pill outright.
 *
 *  Implementation note: we deliberately use a 1Hz `setInterval`
 *  instead of `requestAnimationFrame`. The countdown is a
 *  whole-second grainy display — RAF would burn 60× more React
 *  commits per visible second for no perceptual gain (the visible
 *  text only changes when the integer second rolls over). */
function useCountdown(expiresAt: number | null | undefined): number | null {
  const [now, setNow] = useState<number>(() => Date.now());
  useEffect(() => {
    if (typeof expiresAt !== "number") return;
    // Tick every second while mounted. We don't bother pausing the
    // interval when the card scrolls off-screen — a 1Hz interval is
    // 0.1% CPU and the cost of an `if (!visible) return;` check is
    // more than the saved work.
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [expiresAt]);
  if (typeof expiresAt !== "number") return null;
  return Math.max(0, expiresAt - now);
}

export function InlineApprovalCard({ request, onApprove, onDeny, resolved }: InlineApprovalCardProps) {
  const riskKey = (request.riskLevel ?? "dangerous").toLowerCase();
  const badge = RISK_BADGE[riskKey] ?? RISK_BADGE.dangerous;
  const Icon = toolIcon(request.tool);
  const isPending = !resolved;
  const argsText = shortArgs(request.args);
  // Live countdown against the *server-emitted* deadline. `null` is
  // the "no countdown" sentinel — the card falls back to the
  // status-only pill in that case (pre-2026-06-14 servers, or
  // messages reloaded from a legacy session row).
  const remainingMs = useCountdown(request.expiresAt);
  const showCountdown = isPending && typeof remainingMs === "number";
  const countdownIsDanger =
    typeof remainingMs === "number" &&
    remainingMs <= COUNTDOWN_DANGER_THRESHOLD_MS;

  return (
    <div className="h-enter">
      <div className="flex-1 min-w-0">
        {/* Header — compact: avatar · name · meta */}
        <div className="flex items-center gap-2 mb-1.5">
          <div className="approval-avatar">
            <ShieldAlert size={12} />
          </div>
          <span className="text-[12.5px] font-semibold text-text-primary">Hermix 请求确认</span>
          <span className="h-tiny">{"\u00b7"} {isPending ? "等待你的决定" : "已决定"}</span>
        </div>

        {/* Card surface */}
        <div
          className="msg-card approval-card"
          data-resolved={resolved ?? undefined}
          data-pending-risk={isPending ? riskKey : undefined}
        >
          {/* Line 1: Tool pill + Risk badge + Status */}
          <div className="approval-pills">
            <span className="approval-tool-pill">
              <Icon size={11} className="shrink-0" />
              <span className="font-mono">{request.tool}</span>
            </span>
            <span className={badge.cls}>
              <ShieldAlert size={9} />
              {badge.label}
            </span>
            {isPending && !showCountdown && (
              <span className="approval-pill approval-pill-pending ml-auto">
                等待决定
              </span>
            )}
            {showCountdown && (
              <span
                className={
                  countdownIsDanger
                    ? "approval-pill approval-pill-countdown approval-pill-countdown--danger ml-auto"
                    : "approval-pill approval-pill-countdown ml-auto"
                }
                // Live region: announce remaining time when the
                // urgency flips to "danger" so screen-reader users
                // hear the same alarm a sighted user sees. The
                // per-second churn is suppressed via
                // `aria-live="off"` when the pill is in the calm
                // state — the visible text changes every second,
                // but those are not the right moments to talk
                // about.
                aria-live={countdownIsDanger ? "polite" : "off"}
                data-danger={countdownIsDanger || undefined}
              >
                {countdownIsDanger ? (
                  <Timer size={9} className="approval-countdown-icon" />
                ) : (
                  <Timer size={9} />
                )}
                {countdownIsDanger
                  ? `即将超时 ${formatCountdown(remainingMs!)}`
                  : `剩余 ${formatCountdown(remainingMs!)}`}
              </span>
            )}
            {resolved === "approved" && (
              <span className="approval-pill approval-pill-approved ml-auto">
                <Check size={9} />
                已允许
              </span>
            )}
            {resolved === "denied" && (
              <span className="approval-pill approval-pill-denied ml-auto">
                <X size={9} />
                已拒绝
              </span>
            )}
            {resolved === "expired" && (
              <span className="approval-pill approval-pill-expired ml-auto">
                <Clock size={9} />
                已超时
              </span>
            )}
          </div>

          {/* Line 2: Args text + Action buttons */}
          <div className="approval-line2">
            {argsText && (
              <span className="approval-args-text">{argsText}</span>
            )}
            {isPending ? (
              <div className="approval-actions">
                <button
                  onClick={() => onDeny(request.requestId)}
                  className="approval-btn approval-btn-danger"
                >
                  <X size={12} />
                  拒绝
                </button>
                <button
                  onClick={() => onApprove(request.requestId)}
                  className="approval-btn approval-btn-approve"
                >
                  <Check size={12} />
                  允许
                </button>
              </div>
            ) : (
              <div className="approval-resolved-footer">
                <span className="h-tiny text-text-faint">
                  {resolved === "approved"
                    ? "操作已继续执行"
                    : resolved === "expired"
                      ? "你的点击没赶上,操作已被自动取消;如仍想执行,请重新发起"
                      : "操作已中止"}
                </span>
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
