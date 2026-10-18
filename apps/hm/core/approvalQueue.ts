/**
 * 审批请求的**消息侧**队列。
 *
 * ## 为什么要有它
 *
 * 审批本来也是一种消息（`approval.request` 帧），但手机端只把它当 REST 资源：
 * 审批页按固定间隔轮询 `/v1/sessions/{sid}/approvals/pending`，`reduce` 见到
 * 审批帧直接 `return state` 丢掉。后果有三个：
 *
 * 1. **补发对它无效**。重连后 `sync` 会把断连期间的 `approval.request` 补回来，
 *    帧到了、解析了、又被丢掉，UI 仍然要等下一轮轮询——而那时 PC 侧可能已超时。
 * 2. **看到得晚**。审批从「已在帧里」变成「等下一次轮询」，白等一个轮询周期。
 * 3. 帧里带了 REST 不带的字段（`can_persist`），这些信息也被丢在地上。
 *
 * 所以这里把帧收下来：页面订阅它，收到新请求就**立刻**拉一次，而不是等轮询。
 *
 * ## 为什么不做成真正的状态源
 *
 * 审批的权威状态（还剩几秒、是否已超时）在服务端，`remain_secs` / `expired`
 * 只有 REST 给。帧只说明「有这么一条请求」。让帧当唯一真相源，就得在手机上
 * 维护一份会与服务端漂移的计时——那正是前面几批在避免的东西。
 *
 * 结论：**帧负责「有没有」，REST 负责「是什么、还剩多久」**。两者不打架。
 */

/** 从帧上能知道的最小信息。 */
export interface ApprovalHint {
  requestId: string
  tool: string
  /** 会话 id：审批决策要按会话提交，帧里有就带上。 */
  sid?: string
  /** 帧到达时刻（本机时钟），只用于判断新旧，不参与过期计算。 */
  receivedAt: number
}

type Listener = (hint: ApprovalHint) => void

const pending = new Map<string, ApprovalHint>()
const listeners = new Set<Listener>()

/**
 * 收一条审批帧。
 *
 * 重复投递（relay 重投 / 补发与实时重叠）按 `requestId` 幂等——但**仍会通知**
 * 订阅者：重连后补回来的那条请求，页面正需要被叫醒去拉一次。
 */
export function noteApprovalRequest(hint: ApprovalHint): void {
  pending.set(hint.requestId, hint)
  for (const fn of listeners) {
    try {
      fn(hint)
    } catch {
      // 订阅者是 UI；它抛错不该连累其它订阅者，也不该让帧处理中断。
    }
  }
}

/** 已经处理过（或服务端已不再返回）的请求，从队列里摘掉。 */
export function forgetApproval(requestId: string): void {
  pending.delete(requestId)
}

/** 当前已知的待批请求（诊断 / 页面挂载时补一次）。 */
export function knownApprovals(): ApprovalHint[] {
  return [...pending.values()]
}

export function clearApprovals(): void {
  pending.clear()
}

/** 订阅新到达的审批请求，返回退订函数。 */
export function onApprovalRequest(fn: Listener): () => void {
  listeners.add(fn)
  return () => {
    listeners.delete(fn)
  }
}
