import { useCallback, useEffect, useRef, useState } from "react";
import { Gauge } from "lucide-react";
import { getSessionUsage, type SessionUsageTotals } from "../api";
import { PopoverHeader, PopoverShell } from "./PopoverShell";
import { usePopover } from "../hooks/usePopover";
import { formatContextTokens } from "./ModelSelector";

/**
 * 输入框上方的 Token 消耗明细入口（图标 + 焦点/悬停弹出的卡片）。
 *
 * 数据来源两条，口径分开：
 *  - **数字**：`GET /v1/sessions/{id}/usage` —— `token_usage` 按会话求和
 *    （一行 = 一次 API 调用，见 2026-10-27 使用统计口径笔记）。
 *  - **上下文**：`contextUsage` prop（流式实时 / 会话恢复值），不参与求和。
 *
 * 口径约定（与后端一致）：`promptTokens` 含缓存命中部分，未命中 =
 * `promptTokens − cacheReadTokens`；`completionTokens` 含思考部分，
 * 回复 = `completionTokens − reasoningTokens`。
 */

const fmtInt = (n: number) => n.toLocaleString("en-US");

interface TokenUsagePopoverProps {
  sessionId: string | null;
  /** 当前上下文占用（流式实时值优先，恢复会话时来自 REST）。 */
  contextUsage?: { usedTokens: number; maxTokens: number } | null;
  /** 生成中 → 卡片打开时每 3s 刷新一次（每次调用落库，DB 即最新账）。 */
  isStreaming?: boolean;
}

function Row({
  label,
  value,
  dotClass,
  indent = false,
}: {
  label: string;
  value: string;
  dotClass?: string;
  indent?: boolean;
}) {
  return (
    <div className={`flex items-center justify-between gap-4 ${indent ? "pl-3.5" : ""}`}>
      <span className="flex items-center gap-1.5 text-text-secondary">
        {dotClass ? (
          <span className={`inline-block h-1.5 w-1.5 rounded-full ${dotClass}`} aria-hidden />
        ) : (
          <span className="inline-block w-1.5" aria-hidden />
        )}
        {label}
      </span>
      <span className="tabular-nums text-text-primary">{value}</span>
    </div>
  );
}

/** 命中/写入/未命中 三段堆叠条（宽度是运行时几何，允许 inline style）。 */
function StackedBar({
  hit,
  write,
  miss,
}: {
  hit: number;
  write: number;
  miss: number;
}) {
  const total = hit + write + miss;
  if (total <= 0) {
    return <div className="h-1.5 rounded-full bg-bg-quaternary" aria-hidden />;
  }
  const pct = (n: number) => `${(n / total) * 100}%`;
  return (
    <div className="flex h-1.5 overflow-hidden rounded-full bg-bg-quaternary" aria-hidden>
      <div className="h-full bg-success" style={{ width: pct(hit) }} />
      <div className="h-full bg-warning" style={{ width: pct(write) }} />
      <div className="h-full bg-error" style={{ width: pct(miss) }} />
    </div>
  );
}

export function TokenUsagePopover({
  sessionId,
  contextUsage,
  isStreaming,
}: TokenUsagePopoverProps) {
  const [open, setOpen, containerRef, triggerRef] = usePopover();
  const [data, setData] = useState<SessionUsageTotals | null>(null);
  const [failed, setFailed] = useState(false);

  // 悬停开合：触发器离开后留 160ms 宽限，移到卡片上则取消关闭
  // （卡片经 portal 渲染到 body，是独立子树，必须自带 enter/leave）。
  const closeTimer = useRef<number | null>(null);
  const cancelClose = useCallback(() => {
    if (closeTimer.current !== null) {
      window.clearTimeout(closeTimer.current);
      closeTimer.current = null;
    }
  }, []);
  const scheduleClose = useCallback(() => {
    cancelClose();
    closeTimer.current = window.setTimeout(() => setOpen(false), 160);
  }, [cancelClose, setOpen]);
  useEffect(() => cancelClose, [cancelClose]);

  // 切会话先清空，避免上一会话的数字闪现。
  useEffect(() => {
    setData(null);
    setFailed(false);
  }, [sessionId]);

  useEffect(() => {
    if (!open || !sessionId) return;
    let alive = true;
    const load = async () => {
      try {
        const totals = await getSessionUsage(sessionId);
        if (alive) {
          setData(totals);
          setFailed(false);
        }
      } catch {
        if (alive) setFailed(true);
      }
    };
    void load();
    // 生成中每 3s 刷新：每次 API 调用结束即落库，卡片读数始终是当前账。
    if (!isStreaming) return () => { alive = false; };
    const timer = window.setInterval(() => void load(), 3000);
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
  }, [open, sessionId, isStreaming]);

  const ctx =
    contextUsage && contextUsage.maxTokens > 0 ? contextUsage : null;
  const ctxPct = ctx
    ? Math.min(100, (ctx.usedTokens / ctx.maxTokens) * 100)
    : null;
  const ctxColor =
    ctxPct === null
      ? "text-text-faint"
      : ctxPct >= 95
        ? "text-text-error"
        : ctxPct >= 85
          ? "text-text-warning"
          : "text-text-secondary";
  const ctxDot =
    ctxPct === null
      ? ""
      : ctxPct >= 95
        ? "bg-error"
        : ctxPct >= 85
          ? "bg-warning"
          : "bg-success";

  if (!sessionId) {
    // 没有会话就没有明细可看：整块不渲染（此前这里留一个孤零零的破折号）
    return null;
  }

  const prompt = data?.promptTokens ?? 0;
  const cacheRead = Math.min(data?.cacheReadTokens ?? 0, prompt);
  const cacheWrite = data?.cacheCreationTokens ?? 0;
  const uncached = Math.max(0, prompt - cacheRead);
  const output = data?.completionTokens ?? 0;
  const reasoning = Math.min(data?.reasoningTokens ?? 0, output);
  const reply = Math.max(0, output - reasoning);
  const total = prompt + output;
  const denom = cacheRead + uncached;
  const hitRate = denom > 0 ? (cacheRead / denom) * 100 : null;

  return (
    <>
      <div className="mb-1.5 flex items-center justify-end gap-2 px-1">
        <button
          ref={triggerRef as React.RefObject<HTMLButtonElement>}
          type="button"
          aria-label="Token 消耗明细"
          aria-haspopup="dialog"
          aria-expanded={open}
          onClick={() => setOpen(true)}
          onMouseEnter={() => {
            cancelClose();
            setOpen(true);
          }}
          onMouseLeave={scheduleClose}
          onFocus={() => {
            cancelClose();
            setOpen(true);
          }}
          onBlur={scheduleClose}
          className={`icon-btn h-5 w-5 rounded-md ${open ? "text-text-secondary" : "text-text-faint"}`}
        >
          <Gauge className="icon-xs" aria-hidden />
        </button>
        {/* 2026-10-28：这里原本还有一枚「圆点 + 14%」的上下文指示。
            明细卡片里已经包含上下文统计（usedTokens / maxTokens），
            旁边再挂一个百分比属于重复，已移除；只留打开明细的 Gauge 按钮。 */}
      </div>

      {open && (
        <PopoverShell
          containerRef={containerRef}
          triggerRef={triggerRef}
          align="right"
          side="top"
          width="xl"
        >
          <div onMouseEnter={cancelClose} onMouseLeave={scheduleClose}>
            <PopoverHeader
              right={
                <span className="tabular-nums text-text-secondary">
                  总计 {fmtInt(total)}
                </span>
              }
            >
              <span className="flex items-center gap-1.5">
                <Gauge className="icon-xs text-text-faint" aria-hidden />
                Token 消耗明细
              </span>
            </PopoverHeader>

            <div className="space-y-1 px-2.5 pb-2 text-[length:var(--text-meta)]">
              {failed && (
                <div className="py-2 text-center text-text-faint">读取失败，稍后重试</div>
              )}
              {!failed && !data && (
                <div className="py-2 text-center text-text-faint">读取中…</div>
              )}
              {!failed && data && data.calls === 0 && (
                <div className="py-2 text-center text-text-faint">本会话暂无用量</div>
              )}
              {!failed && data && data.calls > 0 && (
                <>
                  <Row label="输入" value={fmtInt(prompt)} dotClass="bg-[var(--color-tone-blue)]" />
                  <Row label="缓存命中" value={fmtInt(cacheRead)} dotClass="bg-success" indent />
                  <Row label="缓存未命中" value={fmtInt(uncached)} dotClass="bg-error" indent />
                  <Row label="缓存写入" value={fmtInt(cacheWrite)} dotClass="bg-warning" indent />

                  <div className="pt-1.5">
                    <Row label="输出" value={fmtInt(output)} dotClass="bg-[var(--color-tone-violet)]" />
                  </div>
                  <Row label="思考过程" value={fmtInt(reasoning)} indent />
                  <Row label="回复内容" value={fmtInt(reply)} indent />
                </>
              )}
            </div>

            <div className="space-y-1.5 border-t border-border-primary px-2.5 py-2 text-[length:var(--text-meta)]">
              <div className="flex items-center justify-between">
                <span className="text-text-secondary">缓存命中率</span>
                <span
                  className={`tabular-nums font-semibold ${
                    hitRate === null
                      ? "text-text-faint"
                      : hitRate >= 50
                        ? "text-text-success"
                        : "text-text-secondary"
                  }`}
                >
                  {hitRate === null ? "—" : `${hitRate.toFixed(1)}%`}
                </span>
              </div>
              <StackedBar hit={cacheRead} write={cacheWrite} miss={uncached} />
              <div className="flex items-center gap-3 text-text-faint">
                <span className="flex items-center gap-1">
                  <span className="h-1.5 w-1.5 rounded-full bg-success" aria-hidden />
                  命中
                </span>
                <span className="flex items-center gap-1">
                  <span className="h-1.5 w-1.5 rounded-full bg-warning" aria-hidden />
                  写入
                </span>
                <span className="flex items-center gap-1">
                  <span className="h-1.5 w-1.5 rounded-full bg-error" aria-hidden />
                  未命中
                </span>
                {(data?.untrustedCalls ?? 0) > 0 && (
                  <span className="ml-auto">{data?.untrustedCalls} 次上报异常</span>
                )}
              </div>
            </div>

            {ctx && (
              <div className="space-y-1.5 border-t border-border-primary px-2.5 py-2 text-[length:var(--text-meta)]">
                <div className="flex items-center justify-between">
                  <span className="text-text-secondary">上下文</span>
                  <span className={`tabular-nums ${ctxColor}`}>
                    {formatContextTokens(ctx.usedTokens) ?? fmtInt(ctx.usedTokens)} /{" "}
                    {formatContextTokens(ctx.maxTokens) ?? fmtInt(ctx.maxTokens)}
                    {" · "}
                    {Math.round(ctxPct ?? 0)}%
                  </span>
                </div>
                <div className="h-1.5 overflow-hidden rounded-full bg-bg-quaternary" aria-hidden>
                  <div
                    className={`h-full rounded-full ${ctxDot || "bg-success"}`}
                    style={{ width: `${ctxPct ?? 0}%` }}
                  />
                </div>
              </div>
            )}
          </div>
        </PopoverShell>
      )}
    </>
  );
}
