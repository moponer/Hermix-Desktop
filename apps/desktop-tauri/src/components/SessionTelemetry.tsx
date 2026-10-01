// 会话级遥测条 —— 消息列顶部一行，极淡，双列。
//
// 2026-10-01 从每轮的折叠头里搬出来的：折叠头原本是
// `已工作完成 思考 5 段 · tool_search、… 2 个失败 2 个跳过 9 行`，
// 一行挤了六种信息，且「段数 / 动词 / 失败数 / 行数」每轮换一个组合。
// 这类**会话级**的统计放到消息列顶部，折叠头只留「这一轮花了多久 +
// 有没有问题」两件事。
//
// 诚实性约束（不可妥协）：
// - `token/s` **只在有真实分子和分母时显示**。分母必须是本轮真实经过
//   的秒数，不能拿整轮历史除。没有数据就不显示，绝不显示一个编出来的数。
// - 空闲时只显示耗时。速率只在流式期间有意义，静下来再显示就是
//   陈旧数字。
import { useEffect, useRef, useState } from "react";

/** 秒 → 「1 小时 8 分 28 秒」/「11 分 57 秒」/「42 秒」。 */
export function formatElapsed(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return "0 秒";
  const total = Math.floor(ms / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  if (h > 0) return `${h} 小时 ${m} 分 ${s} 秒`;
  if (m > 0) return `${m} 分 ${s} 秒`;
  return `${s} 秒`;
}

export function SessionTelemetry({
  sessionStartedAt,
  isStreaming,
  completionTokens,
  turnStartedAt,
}: {
  sessionStartedAt?: string;
  isStreaming?: boolean;
  /** 本轮 completion tokens（`usage` 事件推的逐轮值）。 */
  completionTokens?: number;
  /** 本轮开始的墙钟毫秒。没有它就不算速率。 */
  turnStartedAt?: number | null;
}) {
  // 每秒重算一次，让「已工作 11 分 57 秒」自己走。
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!isStreaming) return;
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [isStreaming]);

  const startedMs = sessionStartedAt ? new Date(sessionStartedAt).getTime() : NaN;
  if (!Number.isFinite(startedMs)) return null;
  const elapsed = Math.max(0, now - startedMs);

  // 速率：分子分母都必须是真的。turnStartedAt 缺、或还没跑满 1 秒，
  // 都显示不了 —— 一个基于 0.2 秒分母算出来的「1.2k token/s」是噪声。
  let rate: number | null = null;
  if (isStreaming && turnStartedAt && completionTokens != null) {
    const secs = (now - turnStartedAt) / 1000;
    if (secs >= 1 && completionTokens > 0) {
      rate = Math.round(completionTokens / secs);
    }
  }

  return (
    <div className="session-telemetry" role="status" aria-live="off">
      <span className="session-telemetry-elapsed">已执行 {formatElapsed(elapsed)}</span>
      <span className="session-telemetry-right">
        {rate != null ? (
          <span className="session-telemetry-rate" title="本轮 completion tokens / 本轮已用秒数">
            {rate} token/s
          </span>
        ) : null}
      </span>
    </div>
  );
}

/**
 * 记录本轮开始的墙钟时刻，供 `SessionTelemetry` 算速率。
 * 流式开始时记一次，之后保持不变直到下一轮 —— 中途重算会把速率
 * 越摊越低，看起来像"变慢了"。
 */
export function useTurnStart(isStreaming: boolean): number | null {
  const [turnStart, setTurnStart] = useState<number | null>(null);
  const wasStreaming = useRef(false);
  useEffect(() => {
    if (isStreaming && !wasStreaming.current) setTurnStart(Date.now());
    if (!isStreaming) wasStreaming.current = false;
    wasStreaming.current = isStreaming;
  }, [isStreaming]);
  return turnStart;
}
