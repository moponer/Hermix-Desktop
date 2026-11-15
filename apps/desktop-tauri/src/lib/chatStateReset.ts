// lib/chatStateReset.ts
//
// 原子重置 ChatView 的"对话中"状态，专门给模型/Provider 切换路径调用。
//
// 背景：切换模型只改 `selectedModel` store + PATCH 是不够的，必须同时
// 重置本组件持有之外的 in-flight 状态，否则脏状态会带到下一轮：
//   - 上家在跑的 streamingSessionId / streaming
//   - 在途审批（approvalResolversRef / timers）
//   - streamError / streamPhase / runningTool / subAgents / acui
//   - 队列里的 messageQueue（drain 时还不带 overrides，后端按错模型）
// 这些脏状态会让新轮 send 以错误模型请求后端 → 400。
//
// 设计：纯函数 + 副作用封装。`cancelStream` 复用 useStreamChat.cancel，
// 其余 setter/refs 由 App.tsx 已持有的引用直接传入。

import type { StreamPhase } from "../hooks/useStreamChat";
import type { ApprovalBridge } from "../hooks/useApprovalBridge";

/** useStreamChat 暴露的 ws 发送原语（cancel / send 内部也用它）。 */
export type WsSendRaw = (payload: string) => boolean;

/**
 * 一个 plain ref-like 容器（`{ current: T }`）—— 接受 MutableRefObject
 * （useRef 的初值形式）或 RefObject（nullable 形式），传入方无需 cast。
 */
export interface RefLike<T> {
  current: T;
}

/**
 * 模型/Provider 切换的 ChatView 状态重置依赖。
 * 大多数字段 App.tsx 已经持有；这里集中声明，供 ChatView 在切换处调。
 */
export interface ChatStateResetDeps {
  /**
   * useStreamChat.cancel —— 取消进行中流，会关 WS + 触发 onCancel 链路。
   *
   * 传 `activeSessionId` 是必需的：不传时 `cancel` 会跳过归属守卫
   * （useStreamChat.ts:778），于是**在会话 B 里切模型会连带杀掉会话 A
   * 正在跑的流**，并把 A 的待审批全部 deny 掉。A 的流与用户在 B 里的操作
   * 毫无关系，不该被牵连。
   */
  cancelStream: (sessionId?: string) => boolean;
  /**
   * 用户当前所在会话 —— cancel 的归属锚点，见上。
   * `null` 表示当前没有活跃会话（例如刚清空列表），此时不做归属
   * 限制，与旧行为一致。
   */
  activeSessionId?: string | null;
  /** WS 原始 send，用于把 in-flight 审批以 denied 主动拒绝。 */
  wsSendRaw: WsSendRaw;
  /** React state setters / 清除函数。 */
  clearStreamError: () => void;
  setStreamPhase: (phase: StreamPhase | null) => void;
  setRunningToolCount: (n: number) => void;
  setSubAgents: (
    updater: (prev: Record<string, unknown>) => Record<string, unknown>,
  ) => void;
  /**
   * 审批桥接（`hooks/useApprovalBridge`）——直接传桥接对象，不再包
   * `RefLike`。此前这里收 4 个 ref，调用方还得 `as unknown as` 矫正类型
   * （resolver 签名对不上）；现在类型直通，谎言消失。
   */
  approval: Pick<
    ApprovalBridge,
    "resolvers" | "retryTimers" | "timeoutTimers" | "pendingSessions"
  >;
  clarifyResolversRef: RefLike<
    Map<string, Array<(answer: string) => void>>
  >;
  runningToolRef: RefLike<Map<string, string>>;
  currentAssistantIdRef: RefLike<string | null>;
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
 * - `messageQueue` —— 队列里的待发消息在 drain 时带 overrides，不在本函数范围。
 * - 后端 `sessions.model` —— 由 setSelectedModelAndPersist 同步 PATCH。
 */
export function resetChatStateForModelSwitch(deps: ChatStateResetDeps): void {
  // 1) cancel 进行中的流 —— 只针对用户当前所在会话。
  //
  // `undefined` means "no guard" to `useStreamChat.cancel`, but `null`
  // means "no active session". Passing `null` straight through would be
  // read as `undefined` there and skip the ownership check, which is the
  // bug this call site is fixing. Normalize: no active session → no live
  // stream of ours to cancel, so skip the call entirely.
  if (deps.activeSessionId) {
    deps.cancelStream(deps.activeSessionId);
  }

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
  deps.approval.resolvers.forEach((_resolver, requestId) => {
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
  deps.approval.resolvers.clear();
  deps.approval.retryTimers.forEach((t) => clearInterval(t));
  deps.approval.retryTimers.clear();
  deps.approval.timeoutTimers.forEach((t) => clearTimeout(t));
  deps.approval.timeoutTimers.clear();
  deps.approval.pendingSessions.clear();
  deps.clarifyResolversRef.current.forEach((resolvers) =>
    resolvers.forEach((resolve) => resolve("")),
  );
  deps.clarifyResolversRef.current.clear();
}
