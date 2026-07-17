import { AlertTriangle, ShieldAlert, ShieldCheck, Wrench } from "lucide-react";

/**
 * CompactionStatus — what the agent_loop compaction pipeline is doing
 * right now. Mirrors the planned `compact_circuit_breaker::BreakerState`
 * plus a separate "fallback" lane for when the breaker is closed but
 * the lightweight-prune fallback is engaged.
 *
 * Day 1 scope: pure UI placeholder. The values are passed in via props
 * by the parent (ChatView), which in turn reads them from `useStreamChat`'s
 * `compactionStatus` state. The state will become live once Day 2 wires
 * up the new `compaction_status` WS event from the backend.
 */
export type CompactionState =
  | "closed"        // breaker healthy: LLM-driven compaction running normally
  | "half_open"     // breaker probing after a cooldown
  | "open"          // breaker tripped: LLM compaction skipped, no fallback
  | "fallback"      // breaker open + lightweight-prune engaged (recommended)
  | "pressure";     // pressure-only signal (no breaker trip yet): just warn

export interface CompactionStatus {
  state: CompactionState;
  /** Used tokens / threshold, both formatted (e.g. "145k / 160k"). */
  usedText?: string;
  thresholdText?: string;
  /** Raw numeric values so the banner can detect pressure even when
   * the backend snapshot reports `closed` (pre-compression state). */
  usedTokens?: number;
  thresholdTokens?: number;
  /**
   * Unit suffix for `usedText` / `thresholdText`. Day 3 the Rust backend
   * renamed the underlying budget from chars to tokens; the UI mirrors
   * that rename. Default: `"tokens"`.
   */
  unit?: string;
  /** Percent of threshold currently consumed (0–100). */
  percent?: number;
  /** Number of consecutive compaction failures, when state involves a breaker. */
  consecutiveFailures?: number;
  /** Last failure reason — short, safe to display. */
  lastFailureReason?: string;
  /** Wall-clock time of the last state change (Unix ms). */
  lastChangedAt?: number;
}

interface CompactionStatusBannerProps {
  status: CompactionStatus | null;
  /** Callback when the user dismisses the banner. */
  onDismiss?: () => void;
}

const STATE_COPY: Record<
  CompactionState,
  { icon: typeof ShieldCheck; tone: string; title: string; body: string }
> = {
  closed: {
    icon: ShieldCheck,
    tone: "text-emerald-700 dark:text-emerald-300 bg-emerald-50 dark:bg-emerald-900/20 border-emerald-200 dark:border-emerald-700/50",
    title: "上下文自动压缩:正常",
    body: "达到阈值时,自动用 LLM 总结中间对话以腾出空间。",
  },
  half_open: {
    icon: Wrench,
    tone: "text-sky-700 dark:text-sky-300 bg-sky-50 dark:bg-sky-900/20 border-sky-200 dark:border-sky-700/50",
    title: "上下文自动压缩:恢复探测中",
    body: "熔断器冷却完毕,正在试探一次新的压缩。",
  },
  open: {
    icon: ShieldAlert,
    tone: "text-rose-700 dark:text-rose-300 bg-rose-50 dark:bg-rose-900/20 border-rose-200 dark:border-rose-700/50",
    title: "上下文自动压缩:已熔断",
    body: "压缩连续失败,已停止尝试,以免继续烧 token。",
  },
  fallback: {
    icon: Wrench,
    tone: "text-amber-700 dark:text-amber-300 bg-amber-50 dark:bg-amber-900/20 border-amber-200 dark:border-amber-700/50",
    title: "上下文自动压缩:已切换轻量裁剪",
    body: "熔断器断开,改为只删 tool_result 块以省 token,不调 LLM。",
  },
  pressure: {
    icon: AlertTriangle,
    tone: "text-amber-700 dark:text-amber-300 bg-amber-50 dark:bg-amber-900/20 border-amber-200 dark:border-amber-700/50",
    title: "上下文使用率偏高",
    body: "即将触发自动压缩,对话脉络保留,工具输出可能丢失。",
  },
};

export function CompactionStatusBanner({ status, onDismiss }: CompactionStatusBannerProps) {
  if (!status) return null;

  // UI-level state promotion: even if the backend breaker hasn't tripped,
  // crossing the compression threshold means the user is in a pressure
  // situation and should see the pressure copy instead of "normal".
  const isOverThreshold =
    status.state === "closed" &&
    typeof status.usedTokens === "number" &&
    typeof status.thresholdTokens === "number" &&
    status.thresholdTokens > 0 &&
    status.usedTokens >= status.thresholdTokens;

  const effectiveState = isOverThreshold ? "pressure" : status.state;
  const copy = STATE_COPY[effectiveState];
  if (!copy) return null;
  const Icon = copy.icon;
  const unit = status.unit ?? "tokens";
  const usedLabel =
    status.usedText && status.thresholdText
      ? `${status.usedText} / ${status.thresholdText} ${unit}${status.percent !== undefined ? ` (${Math.round(status.percent)}%)` : ""}`
      : null;
  return (
    <div className={`mb-2 px-3 py-2.5 rounded-xl border text-[13px] flex items-start gap-2.5 ${copy.tone}`}>
      <Icon size={16} className="shrink-0 mt-0.5" />
      <div className="flex-1 min-w-0 leading-snug">
        <div className="font-medium">{copy.title}</div>
        <div className="opacity-90 mt-0.5">{copy.body}</div>
        {(usedLabel || status.consecutiveFailures || status.lastFailureReason) && (
          <div className="opacity-80 mt-1 flex flex-wrap gap-x-3 gap-y-0.5">
            {usedLabel && <span>当前用量:{usedLabel}</span>}
            {typeof status.consecutiveFailures === "number" && status.consecutiveFailures > 0 && (
              <span>连续失败 {status.consecutiveFailures} 次</span>
            )}
            {status.lastFailureReason && <span>原因:{status.lastFailureReason}</span>}
          </div>
        )}
      </div>
      {onDismiss && (
        <button
          onClick={onDismiss}
          className="shrink-0 ml-1 p-0.5 rounded-md hover:bg-black/5 dark:hover:bg-white/5 transition-colors opacity-70 hover:opacity-100"
          title="关闭"
          aria-label="关闭压缩状态提示"
        >
          ×
        </button>
      )}
    </div>
  );
}