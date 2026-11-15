/**
 * 补发结果的**顺序控制**。纯逻辑，不碰网络、不碰 UI，便于单测。
 *
 * ## 它解决的那个竞态
 *
 * 重连后 PC 会同时做两件事：继续推实时帧（seq 4、5），并把断连期间漏掉的
 * 打包回一条 `sync.result`（seq 1..3）。两条流走不同的路径，到达顺序没有
 * 保证。若实时帧先到并把 `lastSeq` 推到 5，补发里的 1..3 就会被「比进度
 * 旧」的判断过滤掉 —— 手机永远看不到那一段，而这正是要修的 bug。
 *
 * 做法：补发在飞时先把实时帧**排队**，结果落地后再按序放出。
 *
 * ## 为什么要有 `end()`
 *
 * PC 侧版本较老时不认识 `sync`，会把请求整帧丢弃且不回错误。没有超时兜底，
 * 实时帧会一直堆在队列里不显示 —— 比不补发更糟。`end()` 就是那条逃生口。
 */
export interface SyncResult {
  events?: unknown[]
  truncated?: boolean
}

/** 这条明文是不是补发结果。解析失败一律当普通帧（交给 UI 自己判）。 */
export function isSyncResult(text: string): boolean {
  try {
    return (JSON.parse(text) as { type?: string })?.type === 'sync.result'
  } catch {
    return false
  }
}

function seqOf(text: string): number | null {
  try {
    const seq = (JSON.parse(text) as { seq?: unknown })?.seq
    return typeof seq === 'number' ? seq : null
  } catch {
    return null
  }
}

export class SyncOrdering {
  private lastSeq = 0
  private pending = false
  private queued: string[] = []

  constructor(
    private readonly deliver: (text: string) => void,
    private readonly onTruncated: () => void = () => {},
  ) {}

  /** 已应用的最大序号。补发请求要把它带回给 PC。 */
  get lastSeqSeen(): number {
    return this.lastSeq
  }

  /** 补发请求已发出，进入排队态。 */
  begin(): void {
    this.pending = true
  }

  /** 补发取消（超时 / 发不出去）：放开队列，别把实时帧扣着。 */
  end(): void {
    if (!this.pending) return
    this.pending = false
    this.flush()
  }

  /** 实时帧：补发在飞时排队，否则按 seq 去重后立即送达。 */
  offer(text: string): void {
    if (this.pending) {
      this.queued.push(text)
      return
    }
    const seq = seqOf(text)
    if (seq !== null) {
      if (seq <= this.lastSeq) return // relay 重投 / 与补发重叠
      this.lastSeq = seq
    }
    this.deliver(text)
  }

  /** 补发结果落地：先按序补，再放行攒下的实时帧。 */
  applyResult(v: SyncResult): void {
    const wasPending = this.pending
    this.pending = false
    const events = Array.isArray(v.events) ? v.events : []
    for (const ev of events) {
      const seq = (ev as { seq?: unknown })?.seq
      if (typeof seq === 'number') {
        if (seq <= this.lastSeq) continue
        this.lastSeq = seq
      }
      this.deliver(JSON.stringify(ev))
    }
    if (wasPending) this.flush()
    // 环形日志覆盖掉的那一段真的没了 —— 明确告知，不假装完整。
    if (v.truncated) this.onTruncated()
  }

  private flush(): void {
    const queued = this.queued
    this.queued = []
    for (const text of queued) this.offer(text)
  }
}
