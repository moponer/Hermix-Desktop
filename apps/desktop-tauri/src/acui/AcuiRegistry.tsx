/**
 * ACUI registry — maps card `kind` to a React component.
 *
 * 协议层只关心数据；AcuiRegistry 把数据路由到具体的 React 渲染。
 * 默认注册 6 个 kind，复用现有 Inline* 组件 + ToolEventCard + builtin
 * awakening / self-check 卡片。
 *
 * 自定义 kind 走 fallback（原始 JSON 调试视图），而不是崩溃。
 *
 * 注册 / 注销 API：
 *   register("weather", WeatherCard)
 *   unregister("weather")
 *
 * 多 Renderer 场景下，每个 renderer 可以创建自己的 registry 实例
 * （不是全局 singleton），方便隔离测试 / 多 session。
 */

import type { ComponentType } from "react";
import type { AcuiShow } from "./types";
import { InlineApprovalCard, type ApprovalRequestShape } from "../components/InlineApprovalCard";
import { InlineClarifyCard, type ClarifyRequestShape } from "../components/InlineClarifyCard";
import { ToolEventCard } from "../components/ToolEventCard";
import type { ExecutionTimelineEvent } from "../types";
import { SelfCheckStepCard } from "./builtin/SelfCheckStepCard";
import { AwakeningCard } from "./builtin/AwakeningCard";

/**
 * Decision bus — what acui components call to send user decisions
 * (approval/deny/answer) back to the transport layer. The bus is
 * built by `useAcuiState.bind({ approve, deny, answer })` and is
 * passed down by the renderer. If a card renders outside a panel
 * that wired the bus, the callbacks are undefined and the card
 * degrades to "view-only" mode (no buttons, just display).
 */
export interface AcuiDecisionBus {
  /** Approve an approval card. The card id (NOT the approval request id —
   *  for the approval adapter they happen to be the same string, but
   *  `useAcuiState.bind` is responsible for that translation if needed). */
  approve?: (cardId: string) => void;
  deny?: (cardId: string) => void;
  answer?: (cardId: string, answer: string) => void;
}

/**
 * Component shape — receives the show payload, the decision bus, and
 * the recorded outcome (so the component can render the resolved footer
 * even if the server hasn't yet sent a confirmation patch).
 *
 * Components must be render-safe (no throws on missing optional fields).
 */
export type AcuiComponentProps = {
  show: AcuiShow;
  /** Decision bus, or undefined if the card is rendered outside an
   *  `AcuiPanel` (e.g. in tests or unit previews). */
  decide?: AcuiDecisionBus;
  /** Outcome recorded in the local decision log (looked up via
   *  `lookupAcuiOutcome`). `undefined` means "no decision yet". */
  outcome?:
    | { resolved: false }
    | { resolved: true; kind: "approved" | "denied" }
    | { resolved: true; kind: "answered"; answer: string };
  /** Set by the renderer when an `update` directive has not yet arrived
   *  but the underlying prop is no longer authoritative. Currently a
   *  no-op hint for components to display a "stale" state. */
  stale?: boolean;
  /**
   * Stream-completion flag forwarded from `<AcuiPanel>` /
   * `renderAcuiCards`. Mirrors `groupTimeline`'s `streamComplete`
   * opt. Defaults to `true` so test previews and post-settle renders
   * keep the legacy behaviour (stranded groups → `"error" +
   * "执行已中断"`). Live-stream callers pass `false` so the
   * `tool_event` ACUI doesn't promote a long-running tool to
   * "失败 / 执行已中断" before its real `tool_complete` arrives —
   * the source of the 失败 → 已完成 badge flutter on `delegate_task`.
   */
  streamComplete?: boolean;
};

export type AcuiComponent = ComponentType<AcuiComponentProps>;

export class AcuiRegistry {
  private components = new Map<string, AcuiComponent>();

  /** Overwrites any previous registration for the same kind. */
  register(kind: string, component: AcuiComponent): this {
    this.components.set(kind, component);
    return this;
  }

  /** Returns true if the kind was registered. */
  unregister(kind: string): boolean {
    return this.components.delete(kind);
  }

  /** Returns undefined if the kind is not registered. */
  resolve(kind: string): AcuiComponent | undefined {
    return this.components.get(kind);
  }

  list(): string[] {
    return Array.from(this.components.keys());
  }

  /** Create a default registry with the standard 6 kinds wired up. */
  static defaults(): AcuiRegistry {
    const r = new AcuiRegistry();
    r.register("approval", ApprovalAcui);
    r.register("clarify", ClarifyAcui);
    r.register("tool_event", ToolEventAcui);
    r.register("path_preview", PathPreviewAcui);
    // TickLoop awakening-phase widgets.
    r.register("self_check_step", SelfCheckStepCard);
    r.register("awakening", AwakeningCard);
    return r;
  }
}

// ---------------------------------------------------------------------------
// Default adapters
//
// Each adapter is a thin React component that takes an AcuiShow and
// extracts the typed props for the underlying inline card. The acui
// `kind` is decoupled from the component's intrinsic prop shape — the
// adapter does the translation.
// ---------------------------------------------------------------------------

const ApprovalAcui: AcuiComponent = ({ show, decide, outcome }) => {
  const props = (show.props ?? {}) as {
    requestId?: string;
    tool?: string;
    args?: unknown;
    riskLevel?: string;
    decision?: "approved" | "denied" | null;
  };
  const shape: ApprovalRequestShape = {
    requestId: props.requestId ?? show.id,
    tool: props.tool ?? "tool",
    args: props.args,
    riskLevel: props.riskLevel ?? "caution",
  };
  // Prefer the server-confirmed `decision` patch over the local
  // decision-log outcome, so the user's click → WS roundtrip is
  // observable. Fall back to the outcome so the card flips to its
  // resolved state instantly on click even before the server replies.
  const resolved =
    props.decision ??
    (outcome?.resolved && (outcome.kind === "approved" || outcome.kind === "denied")
      ? outcome.kind
      : null);

  // Wire approval clicks through the decision bus. If no bus is wired
  // (e.g. a test render), the buttons are still rendered but no-op.
  const onApprove = (requestId: string) => decide?.approve?.(requestId);
  const onDeny = (requestId: string) => decide?.deny?.(requestId);

  return (
    <div data-acui-kind="approval" data-stale={resolved == null ? undefined : "resolved"}>
      <InlineApprovalCard
        request={shape}
        onApprove={onApprove}
        onDeny={onDeny}
        resolved={resolved}
      />
    </div>
  );
};

const ClarifyAcui: AcuiComponent = ({ show, decide, outcome }) => {
  const props = (show.props ?? {}) as {
    requestId?: string;
    question?: string;
    choices?: string[];
    answer?: string | null;
  };
  const shape: ClarifyRequestShape = {
    requestId: props.requestId ?? show.id,
    question: props.question ?? "",
    choices: props.choices ?? [],
  };
  const resolved = props.answer
    ? { answer: props.answer }
    : outcome?.resolved && outcome.kind === "answered"
      ? { answer: outcome.answer }
      : null;

  const onAnswer = (requestId: string, answer: string) =>
    decide?.answer?.(requestId, answer);

  // 已答复的询问卡直接消失，不留占位（与历史 clarify 消息 + 审批卡一致）。
  if (resolved) return null;

  return (
    <div data-acui-kind="clarify">
      <InlineClarifyCard
        request={shape}
        onAnswer={onAnswer}
      />
    </div>
  );
};

const ToolEventAcui: AcuiComponent = ({ show, streamComplete }) => {
  const events =
    (show.props as { events?: ExecutionTimelineEvent[] }).events ?? [];
  if (events.length === 0) return null;
  return (
    <div data-acui-kind="tool_event">
      <ToolEventCard events={events} streamComplete={streamComplete} />
    </div>
  );
};

const PathPreviewAcui: AcuiComponent = ({ show }) => {
  const props = (show.props ?? {}) as {
    path?: string;
    size?: number;        // bytes
    modifiedAt?: string;  // ISO 8601
    preview?: string;     // first N chars of the file
    reason?: string;      // why this path is being surfaced
  };
  const path = props.path;
  if (!path) return <AcuiFallback show={show} />;

  const sizeText = typeof props.size === "number" ? formatBytes(props.size) : null;
  const modText = props.modifiedAt ? formatRelativeTime(props.modifiedAt) : null;
  const previewText =
    typeof props.preview === "string" && props.preview.length > 0
      ? props.preview.length > 160
        ? props.preview.slice(0, 160) + "…"
        : props.preview
      : null;

  return (
    <div
      data-acui-kind="path_preview"
      className="px-3 py-2 rounded-md bg-black/30 border-white/[0.06] space-y-1.5"
    >
      {/* Line 1: path + size + modified */}
      <div className="flex items-center gap-2 min-w-0">
        <span className="text-[length:var(--text-meta)] font-mono text-text-primary truncate flex-1" title={path}>
          {path}
        </span>
        {sizeText && (
          <span className="shrink-0 text-[length:var(--text-meta)] text-text-faint font-mono">{sizeText}</span>
        )}
        {modText && (
          <span className="shrink-0 text-[length:var(--text-meta)] text-text-faint">{modText}</span>
        )}
      </div>
      {/* Line 2: preview snippet */}
      {previewText && (
        <pre className="text-[length:var(--text-meta)] font-mono text-text-muted whitespace-pre-wrap break-all leading-snug">
          {previewText}
        </pre>
      )}
      {/* Line 3: reason */}
      {props.reason && (
        <div className="text-[length:var(--text-meta)] text-text-muted">{props.reason}</div>
      )}
    </div>
  );
};

// ---------------------------------------------------------------------------
// Fallback component — renders unknown kinds as a debug JSON block.
// ---------------------------------------------------------------------------

export const AcuiFallback: AcuiComponent = ({ show }) => (
  <div
    data-acui-kind={show.type}
    data-acui-fallback="true"
    className="text-[length:var(--text-meta)] font-mono text-text-muted px-3 py-2 rounded border-amber-500/30 bg-amber-500/5"
    title="ACUI: unknown kind, showing raw JSON"
  >
    <div className="text-amber-300/80 mb-1">unknown acui kind: {show.type}</div>
    <pre className="whitespace-pre-wrap break-all">
      {JSON.stringify(show.props, null, 2)}
    </pre>
  </div>
);

// ---------------------------------------------------------------------------
// Local helpers (file metadata formatting)
// ---------------------------------------------------------------------------

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

function formatRelativeTime(iso: string): string {
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return "";
  const deltaSec = Math.floor((Date.now() - t) / 1000);
  if (deltaSec < 60) return `${deltaSec}s 前`;
  if (deltaSec < 3600) return `${Math.floor(deltaSec / 60)}m 前`;
  if (deltaSec < 86400) return `${Math.floor(deltaSec / 3600)}h 前`;
  if (deltaSec < 86400 * 30) return `${Math.floor(deltaSec / 86400)}d 前`;
  return new Date(iso).toISOString().slice(0, 10);
}