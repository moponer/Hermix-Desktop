/**
 * 手机侧的出站帧重放窗口。
 *
 * ## 为什么需要它
 *
 * 威胁模型里 relay 运营方是**潜在攻击者**（PC 侧入站已经有 `ReplayGuard`，
 * 见 `crates/hermix-server/src/relay_transport.rs`）。缺失的是反方向：
 * relay 可以把一条旧的 `approval.request` 或 `turn.done` 原样重投给手机，
 * 手机没有去重就会重复渲染、审批按钮可以重复点。
 *
 * ## 为什么用 `eid` 而不是新加序号
 *
 * 信封的 `eid` 已经是发送方每次随机生成的 16 字节，并且同时进 HKDF salt
 * （「重复会削弱跨帧重放防护」本身就是它的设计意图）。重投的帧是**字节级
 * 复制**——relay 没有密钥，无法重新封装出新 `eid`——所以 `eid` 天然就是
 * 这条链路上稳定的帧身份。相比之下新增单调序号要处理「重连后计数器归零」
 * 的 epoch 问题，还得改协议版本，收益为零。
 *
 * 窗口有界（默认 512 条 / 10 分钟）：手机是长连设备，无界集合就是内存泄漏。
 */
export class ReplayWindow {
  private readonly seen = new Set<string>()
  private queue: { eid: string; at: number }[] = []

  constructor(
    private readonly cap: number = 512,
    private readonly ttlMs: number = 600_000,
    private readonly now: () => number = () => Date.now(),
  ) {}

  /**
   * 首次出现的 `eid` 返回 `true` 并登记；重复或空值返回 `false`。
   *
   * 调用方必须在**解密之前**调用：解不开的帧也要占位，否则攻击者可以拿
   * 垃圾帧把窗口刷满（这里是内存上限保护，不是认证）。
   */
  accept(eid: string): boolean {
    const t = this.now()
    this.evict(t)
    if (!eid || this.seen.has(eid)) return false
    this.seen.add(eid)
    this.queue.push({ eid, at: t })
    while (this.queue.length > this.cap) {
      const old = this.queue.shift()
      if (old) this.seen.delete(old.eid)
    }
    return true
  }

  /** 当前窗口内的条目数（测试与诊断用）。 */
  get size(): number {
    return this.seen.size
  }

  private evict(t: number): void {
    while (this.queue.length > 0 && t - this.queue[0].at > this.ttlMs) {
      const old = this.queue.shift()
      if (old) this.seen.delete(old.eid)
    }
  }
}
