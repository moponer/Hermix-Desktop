import { useRef } from "react";
import type { ApprovalDecision } from "../types";

/**
 * 单条审批的结算回调：用户在卡片上点了 Allow / Deny / Always 之后，
 * `settleApproval` 用它兑现 `onApprovalRequest` 返回的 Promise。
 */
export type ApprovalResolver = (
  decision: Exclude<ApprovalDecision, null>,
  remember?: boolean,
  note?: string,
) => void;

/**
 * 审批桥接的**纯簿记**：resolver 表、四类定时器表、REST 兜底的会话归属表。
 *
 * 这是从 `App.tsx` 搬出来的第一刀（2026-10-31）。搬之前它们是 7 个散落的
 * `useRef`（共 72 个使用点、跨 3000 行），簿记语义（谁负责 set、谁负责
 * get+delete、unmount 谁清）完全靠读代码拼出来。搬进一个对象后：
 * - 语义集中：这 7 张表的生命周期完全一致（组件级，unmount 全清）；
 * - 可单测：`renderHook` 下可以直接测 resolver 取还与定时器清理的竞态；
 * - `App.tsx` 的后续拆分（session store、流事件）不再被这些 ref 绊住。
 *
 * **刻意不做的事**：不在这里做任何业务判断（自动过期、自救探测、REST
 * 兜底都留在调用方）——这是簿记，不是策略。策略进这里，测试就得伪造
 * WebSocket。
 */
export interface ApprovalBridge {
  /** 单条审批：requestId → 结算回调。`takeResolver` 语义由调用方 get+delete。 */
  resolvers: Map<string, ApprovalResolver>;
  /** 批量审批：batchId → 结算回调（Allow / Deny / Always 整批结算）。 */
  batchResolvers: Map<string, ApprovalResolver>;
  /** 审批响应的 WS 重发定时器——审批结算时清理。 */
  retryTimers: Map<string, ReturnType<typeof setInterval>>;
  /** 前端超时定时器：服务端 deadline（30s）到了用户还没点 → 卡片标"已过期"
   *  （后端已自动拒绝，agent 已继续，卡片不能再永远显示"等待你的决定"）。 */
  timeoutTimers: Map<string, ReturnType<typeof setTimeout>>;
  /**
   * 「静默审批」自救定时器：审批事件可能因 WebView 渲染冻结、传输中途
   * 断链或投影目标落空而永远没变成卡片，agent 却在服务端干等到 600s
   * 超时。到期仍未决 → REST 直查服务端注册表兜底（注册表与会话/连接
   * 解耦，任意存活连接都能查、都能解）。
   */
  rescueTimers: Map<string, ReturnType<typeof setTimeout>>;
  /**
   * 每个待审批请求归属的 sessionId（与 agent 循环压入请求的顺序一致，
   * JS Map 保证插入顺序）。REST 兜底 approve/deny 时用来定位正确的
   * `/v1/sessions/<sid>/approval` 端点——请求可能属于用户已切走的 session。
   */
  pendingSessions: Map<string, string>;
  /**
   * Pending 1s REST-fallback probes for approval decisions. Tracked so
   * unmount clears them — otherwise the callback (and its `restSid`
   * closure) can fire a POST after the window is tearing down.
   */
  restTimers: Set<number>;
}

function createApprovalBridge(): ApprovalBridge {
  return {
    resolvers: new Map(),
    batchResolvers: new Map(),
    retryTimers: new Map(),
    timeoutTimers: new Map(),
    rescueTimers: new Map(),
    pendingSessions: new Map(),
    restTimers: new Set(),
  };
}

/** 组件级单例：整个生命周期共用一组表（与原来的 7 个 `useRef` 等价）。 */
export function useApprovalBridge(): ApprovalBridge {
  const bridge = useRef<ApprovalBridge | null>(null);
  if (!bridge.current) {
    bridge.current = createApprovalBridge();
  }
  return bridge.current;
}
