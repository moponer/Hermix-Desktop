import { AlertTriangle, CheckCircle2, Loader2, ShieldAlert, ShieldCheck, TimerReset, Wrench } from "lucide-react";

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
  | "pressure"      // pressure-only signal (no breaker trip yet): just warn
  | "quota_exhausted"; // 2026-07-19: compression-model Token Plan quota hit 0

/**
 * Compression-process phase, surfaced by the Rust backend
 * (`hermix_agent::compression::BreakerSnapshot::phase`) so the
 * banner can render mid-attempt transitions instead of jumping
 * straight from the pressure warning to the post-attempt result.
 *
 * Mirrors the values written by
 * `compression_ops::auto_compress_if_over_threshold` (starting →
 * in_progress_{fast,llm} → complete | failed).
 *
 * 2026-07-22: introduced to fix the "警告卡片 → 突然完成" UX gap
 * the user reported — `phase === "idle"` (or missing) keeps the
 * pre-existing static copy.
 */
export type CompactionPhase =
  | "idle"
  | "starting"
  | "in_progress_fast"
  | "in_progress_llm"
  | "complete"
  | "failed";

export interface CompactionStatus {
  state: CompactionState;
  /** Used tokens / threshold, both formatted (e.g. "145k / 160k"). */
  usedText?: string;
  thresholdText?: string;
  /** Formatted max context window (e.g. "205k"). When present, the banner
   * footer shows `used / max` so it matches the ChatView progress bar. */
  maxText?: string;
  /** Raw numeric values so the banner can detect pressure even when
   * the backend snapshot reports `closed` (pre-compression state). */
  usedTokens?: number;
  thresholdTokens?: number;
  maxTokens?: number;
  /**
   * Unit suffix for `usedText` / `thresholdText`. Day 3 the Rust backend
   * renamed the underlying budget from chars to tokens; the UI mirrors
   * that rename. Default: `"tokens"`.
   */
  unit?: string;
  /** Percent of max context window currently consumed (0–100). */
  percent?: number;
  /** Number of consecutive compaction failures, when state involves a breaker. */
  consecutiveFailures?: number;
  /** Last failure reason — short, safe to display. */
  lastFailureReason?: string;
  /** Wall-clock time of the last state change (Unix ms). */
  lastChangedAt?: number;
  /**
   * 2026-07-22: compression-process phase (see [`CompactionPhase`])
   * — optional so older snapshots without the field still render.
   * When present and not `"idle"`, the banner switches to a
   * phase-specific icon + title + body so the user sees the
   * "正在准备压缩" → "正在压缩中" → "压缩完成" transition instead
   * of a single static card.
   */
  phase?: CompactionPhase;
}

interface CompactionStatusBannerProps {
  status: CompactionStatus | null;
  /** Callback when the user dismisses the banner. */
  onDismiss?: () => void;
}

/**
 * 2026-07-22: phase-specific banner copy. Overrides `STATE_COPY` for
 * the duration of a single compression attempt so the user sees the
 * "正在准备压缩" → "正在压缩中" → "压缩完成" transitions instead
 * of jumping straight from the pressure warning to the post-attempt
 * result. `spinning: true` swaps the icon for a `Loader2` so the
 * mid-attempt states read as "actively working".
 */
const PHASE_COPY: Record<
  CompactionPhase,
  {
    icon: typeof ShieldCheck;
    tone: string;
    title: string;
    body: string;
    spinning?: boolean;
  }
> = {
  idle: {
    // `idle` is the no-op entry — when `status.phase` is undefined
    // or `"idle"` we fall through to `STATE_COPY` and render the
    // usual breaker-aware copy. Keep this entry typed-correct so
    // the record is exhaustive, but its values are never used.
    icon: ShieldCheck,
    tone: "",
    title: "",
    body: "",
  },
  starting: {
    icon: Loader2,
    tone: "text-sky-700 dark:text-sky-300 bg-sky-50 dark:bg-sky-900/20 border-sky-200 dark:border-sky-700/50",
    title: "上下文压缩:正在准备",
    body: "检测到上下文使用率已超阈值,即将开始压缩,请稍候…",
    spinning: true,
  },
  in_progress_fast: {
    icon: Loader2,
    tone: "text-sky-700 dark:text-sky-300 bg-sky-50 dark:bg-sky-900/20 border-sky-200 dark:border-sky-700/50",
    title: "上下文压缩:正在压缩中(快速通道)",
    body: "中等压力 — 正在执行去重 + 评分裁剪 + 静态摘要,不调用 LLM。",
    spinning: true,
  },
  in_progress_llm: {
    icon: Loader2,
    tone: "text-sky-700 dark:text-sky-300 bg-sky-50 dark:bg-sky-900/20 border-sky-200 dark:border-sky-700/50",
    title: "上下文压缩:正在压缩中(LLM 摘要)",
    body: "高压力 — 正在调用 LLM 生成结构化摘要以腾出空间,请稍候几秒。",
    spinning: true,
  },
  complete: {
    icon: CheckCircle2,
    tone: "text-emerald-700 dark:text-emerald-300 bg-emerald-50 dark:bg-emerald-900/20 border-emerald-200 dark:border-emerald-700/50",
    title: "上下文压缩:已完成",
    body: "本轮压缩已结束,新一轮对话可以继续正常进行。",
  },
  failed: {
    icon: TimerReset,
    tone: "text-amber-700 dark:text-amber-300 bg-amber-50 dark:bg-amber-900/20 border-amber-200 dark:border-amber-700/50",
    title: "上下文压缩:本轮失败",
    body: "压缩未成功,已记录失败原因;下次触发时会再次尝试。",
  },
};

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
  quota_exhausted: {
    icon: ShieldAlert,
    tone: "text-rose-700 dark:text-rose-300 bg-rose-50 dark:bg-rose-900/20 border-rose-200 dark:border-rose-700/50",
    title: "上下文压缩:Token 配额已耗尽",
    body: "压缩模型无法调用,长对话将不会自动压缩。请升级 Token Plan 套餐或购买积分后重试。",
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
  // 2026-07-22: when the backend reports a non-`"idle"` phase we
  // override the breaker-state copy with the phase-specific copy so
  // the banner shows the user that compression is actively in flight
  // (spinner + "正在压缩中") or has just landed ("压缩完成" /
  // "压缩失败"). The breaker-state copy is still consulted for the
  // header pressure footer (`usedLabel`) below.
  const phaseKey: CompactionPhase = status.phase ?? "idle";
  const phaseCopy = phaseKey !== "idle" ? PHASE_COPY[phaseKey] : null;
  const copy = phaseCopy ?? STATE_COPY[effectiveState];
  if (!copy) return null;
  const Icon = copy.icon;
  const unit = status.unit ?? "tokens";
  // Show the same denominator as the ChatView progress bar (max context
  // window) so the two indicators never disagree. Falls back to the
  // threshold denominator for older snapshots that don't carry max_tokens.
  const usedLabel =
    status.usedText && (status.maxText ?? status.thresholdText)
      ? `${status.usedText} / ${status.maxText ?? status.thresholdText} ${unit}${status.percent !== undefined ? ` (${Math.round(status.percent)}%)` : ""}`
      : null;
  return (
    <div className={`mb-2 px-3 py-2.5 rounded-xl border text-[13px] flex items-start gap-2.5 ${copy.tone}`}>
      <Icon
        size={16}
        className={`shrink-0 mt-0.5 ${"spinning" in copy && copy.spinning ? "animate-spin" : ""}`}
      />
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