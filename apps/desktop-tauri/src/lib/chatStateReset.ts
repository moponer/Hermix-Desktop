// lib/chatStateReset.ts
//
// 原子重置 ChatView 的"对话中"状态，专门给模型/Provider 切换路径调用。
//
// 背景：主人在下拉里选新模型时，原本只有
// `ChatView.tsx:1204-1212 setSelectedModelAndPersist` 改两件事
// （`selectedModel` store + `updateSessionMetadata` PATCH），完全不重置：
//   - 上家在跑的 streamingSessionId / streaming
//   - 在途审批（approvalResolversRef / timers）
//   - streamError / streamPhase / runningTool / subAgents / acui
//   - 队列里的 messageQueue（drain 时还不带 overrides，后端按错模型）
// 这些脏状态会带到下一轮，新轮 send 时后端以错模型请求 → 400 →
// "模型不可用或参数错误" + "对话因后端错误中断"（截图根因）。
//
// 设计：纯函数 + 副作用封装。`cancelStream` 复用 useStreamChat.cancel，
// 其余 setter/refs 由 App.tsx 已持有的引用直接传入。

import type { MutableRefObject } from "react";
import type { StreamPhase } from "../hooks/useStreamChat";

/** useStreamChat 暴露的 ws 发送原语（cancel / send 内部也用它）。 */
export type WsSendRaw = (payload: string) => boolean;

/**
 * 模型/Provider 切换的 ChatView 状态重置依赖。
 * 大多数字段 App.tsx 已经持有；这里集中声明，供 ChatView 在切换处调。
 */
export interface ChatStateResetDeps {
  /** useStreamChat.cancel —— 取消进行中流，会关 WS + 触发 onCancel 链路。 */
  cancelStream: () => void;
  /** WS 原始 send，用于把 in-flight 审批以 denied 主动拒绝。 */
  wsSendRaw: WsSendRaw;
  /** React state setters / 清除函数。 */
  clearStreamError: () => void;
  setStreamPhase: (phase: StreamPhase | null) => void;
  setRunningToolCount: (n: number) => void;
  setSubAgents: (
    updater: (prev: Record<string, unknown>) => Record<string, unknown>,
  ) => void;
  /** 同步 refs（imperatively cleared）。 */
  approvalResolversRef: MutableRefObject<
    Map<string, (decision: unknown) => void>
  >;
  approvalRetryTimersRef: MutableRefObject<
    Map<string, ReturnType<typeof setInterval>>
  >;
  approvalTimeoutTimersRef: MutableRefObject<
    Map<string, ReturnType<typeof setTimeout>>
  >;
  pendingApprovalSessionsRef: MutableRefObject<Map<string, string>>;
  clarifyResolversRef: MutableRefObject<
    Map<string, Array<(answer: string) => void>>
  >;
  runningToolRef: MutableRefObject<Map<string, string>>;
  currentAssistantIdRef: MutableRefObject<string | null>;
  /** ACUI 卡片栈重置。 */
  acuiReset: () => void;
}

/**
 * 原子地重置 ChatView 的所有"对话中"派生状态。
 *
 * 调用时机：用户在下拉里选了新模型（即便旧流还在跑）。
 *
 * 执行顺序（顺序敏感）：
 * 1. cancelStream —— useStreamChat.cancel。会触发注册到 hook 的
 *    `onCancel`（App.tsx:2139 把所有 in-flight 审批以 denied 发后端 +
 *    close WS + setStreamingSessionId(null)）。
 * 2. 清 React state：clearStreamError / setStreamPhase(null) /
 *    setRunningToolCount(0) / setSubAgents({}) /
 *    currentAssistantIdRef.current = null / acui.reset()。
 * 3. 兜底清理 Ref —— cancel 路径只会在 settleRef.current 非 null 时
 *    触发 onCancel；如果流已结束但 Ref 残留（极端 race），这里补一刀。
 *
 * 不动：
 * - `selectedModel` —— 调用者切完模型后才轮到它（顺序保证）。
 * - `messageQueue` —— 队列里的待发消息由 Phase C 修复（drain 时带
 *   overrides），不在本函数范围。
 * - 后端 `sessions.model` —— 已由 setSelectedModelAndPersist 同步 PATCH。
 */
export function resetChatStateForModelSwitch(deps: ChatStateResetDeps): void {
  // 1) cancel 进行中的流。
  deps.cancelStream();

  // 2) 清 UI / 派生 state。
  deps.clearStreamError();
  deps.setStreamPhase(null);
  deps.runningToolRef.current.clear();
  deps.setRunningToolCount(0);
  deps.setSubAgents(() => ({}));
  deps.currentAssistantIdRef.current = null;
  deps.acuiReset();

  // 3) 兜底清理 Ref（cancel 已清过；这里 catch 在 cancel 路径未触发的 race）。
  // approvalResolversRef 在 cancel 的 onCancel 里已经被 forEach deny +
  // clear，此处再 .clear() 是幂等的；这里重新发 deny 是为防 cancel
  // 因 settleRef 为 null 而跳过 onCancel 的极端 race。
  deps.approvalResolversRef.current.forEach((_resolver, requestId) => {
    const sent = deps.wsSendRaw(
      JSON.stringify({
        type: "approval_response",
        request_id: requestId,
        decision: "denied",
      }),
    );
    if (!sent) {
      // ws 已 close（cancel 刚走完），这是预期的；但日志保留便于调试。
      console.warn(
        `[chatReset] ws not open, drop deny id=${requestId} (race ok)`,
      );
    }
  });
  deps.approvalResolversRef.current.clear();
  deps.approvalRetryTimersRef.current.forEach((t) => clearInterval(t));
  deps.approvalRetryTimersRef.current.clear();
  deps.approvalTimeoutTimersRef.current.forEach((t) => clearTimeout(t));
  deps.approvalTimeoutTimersRef.current.clear();
  deps.pendingApprovalSessionsRef.current.clear();
  deps.clarifyResolversRef.current.forEach((resolvers) =>
    resolvers.forEach((resolve) => resolve("")),
  );
  deps.clarifyResolversRef.current.clear();
}
