import { useState, useCallback } from "react";
import { X, ChevronDown, Copy, CheckCheck, ServerCrash, Coins, KeyRound, WifiOff, Clock, AlertTriangle } from "lucide-react";

/** Stream error taxonomy — mirrors `App.classifyStreamErrorKind`. */
export type ErrorKind =
  | "overloaded"
  | "quota"
  | "auth"
  | "network"
  | "timeout"
  | "bad_request"
  | "unknown";

// Severity is expressed by the left rail width (2px/3px/4px) and
// by the rail opacity mix, NOT by the body background — Linear /
// Vercel alert pattern: a thin accent rail says everything about
// importance, the body stays neutral so the message stays legible.
type ErrorTone = "amber-soft" | "amber" | "amber-strong";

type ErrorMetaEntry = {
  title: string;
  code: string;
  tip: string;
  tone: ErrorTone;
  Icon: typeof AlertTriangle;
};

/**
 * Static metadata per error category. Kept out of the component so
 * the same map drives the title, icon, and hint — the UI never has
 * to recompute labels at render time.
 */
const ERROR_META: Record<ErrorKind, ErrorMetaEntry> = {
  overloaded: {
    title: "Provider 暂时过载",
    code: "OVERLOADED",
    tip: "建议稍等片刻后重试；若反复出现可尝试切换其他模型。",
    tone: "amber",
    Icon: ServerCrash,
  },
  quota: {
    title: "用量已达上限",
    code: "QUOTA",
    tip: "当前账户配额或 RPM 已满，请稍后再试或在设置中更换 key。",
    tone: "amber-strong",
    Icon: Coins,
  },
  auth: {
    title: "API key 失效",
    code: "AUTH",
    tip: "请前往「设置 → API Keys」检查密钥是否正确或重新填写。",
    tone: "amber-strong",
    Icon: KeyRound,
  },
  network: {
    title: "网络中断",
    code: "NETWORK",
    tip: "本地网络或后端进程不通，请检查连接后重试。",
    tone: "amber-soft",
    Icon: WifiOff,
  },
  timeout: {
    title: "请求超时",
    code: "TIMEOUT",
    tip: "生成时间过长被中止，建议简化输入或换模型后重试。",
    tone: "amber-soft",
    Icon: Clock,
  },
  bad_request: {
    title: "请求无效",
    code: "BAD_REQUEST",
    tip: "可能是模型名或参数错误，请在输入框左侧切换模型后重试。",
    tone: "amber-strong",
    Icon: AlertTriangle,
  },
  unknown: {
    title: "发送失败",
    code: "ERROR",
    tip: "请稍后重试；如反复出现可打开技术详情对照日志。",
    tone: "amber",
    Icon: AlertTriangle,
  },
};

/**
 * StreamErrorCard — the error surface shown when a send fails
 * (backend 400/5xx, transport error, WS reject, etc.).
 *
 * Linear / Vercel style: a thin accent rail on the left, single
 * row of icon + title, one-line message below, and a collapsible
 * "技术详情" panel only when there's a raw backend error to show.
 * The previous Bootstrap-style chip + primary-color "重试" button
 * have been removed in favor of a ghost button.
 */
export function StreamErrorCard({
  message,
  detail,
  kind,
  onDismiss,
  onRetry,
}: {
  message: string;
  detail?: string;
  kind?: ErrorKind;
  onDismiss?: () => void;
  onRetry?: () => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const [copied, setCopied] = useState(false);

  const meta = ERROR_META[kind ?? "unknown"];

  const copyDetail = useCallback(() => {
    if (!detail) return;
    const blob = `[${meta.code}] ${message}\n\n${detail}`;
    if (typeof navigator !== "undefined" && navigator?.clipboard?.writeText) {
      navigator.clipboard.writeText(blob).then(
        () => {
          setCopied(true);
          window.setTimeout(() => setCopied(false), 1400);
        },
        () => {},
      );
    }
  }, [detail, message, meta.code]);

  return (
    <div className={`stream-error-card stream-error-card--${meta.tone}`} role="alert">
      <div className="stream-error-card__main">
        <div className="stream-error-card__head">
          <span className="stream-error-card__icon" aria-hidden="true">
            <meta.Icon className="icon-md" strokeWidth={2} />
          </span>
          <span className="stream-error-card__title">{meta.title}</span>
          <button
            type="button"
            className="stream-error-card__dismiss"
            onClick={() => onDismiss?.()}
            title="关闭"
            aria-label="关闭错误提示"
          >
            <X className="icon-md" strokeWidth={2} />
          </button>
        </div>
        <div className="stream-error-card__message">{message}</div>
        <div className="stream-error-card__tip">{meta.tip}</div>
        {detail && detail.trim().length > 0 && (
          <details
            className="stream-error-card__detail"
            open={expanded}
            onToggle={(e) => setExpanded((e.target as HTMLDetailsElement).open)}
          >
            <summary>
              <ChevronDown strokeWidth={2}
                className={`icon-sm ${`stream-error-card__chev ${expanded ? "is-open" : ""}`}`}
              />
              技术详情
            </summary>
            <pre>{detail.trim()}</pre>
          </details>
        )}
        {(onRetry || detail) && (
          <div className="stream-error-card__actions">
            {onRetry && (
              <button
                type="button"
                className="stream-error-card__btn"
                onClick={onRetry}
              >
                重试上一条
              </button>
            )}
            {detail && (
              <button
                type="button"
                className="stream-error-card__btn stream-error-card__btn--ghost"
                onClick={copyDetail}
                title="复制原始错误"
              >
                {copied ? (
                  <>
                    <CheckCheck className="icon-sm" strokeWidth={2} />
                    <span>已复制</span>
                  </>
                ) : (
                  <>
                    <Copy className="icon-sm" strokeWidth={2} />
                    <span>复制错误码</span>
                  </>
                )}
              </button>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
