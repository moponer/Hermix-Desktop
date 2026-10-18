/**
 * 出站帧重放窗口。
 *
 * 盯的是一条**威胁模型**：relay 运营方是潜在攻击者（PC 侧入站已有
 * `ReplayGuard`）。它没有密钥，所以重投只能是字节级复制 —— `eid` 不变。
 * 这里验的是「同一个 eid 只放行一次」，以及窗口的有界性：
 * 无界集合在长连设备上就是内存泄漏。
 */
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ReplayWindow } from '../core/replay'

test('首次出现放行，重投同一 eid 拒收', () => {
  const w = new ReplayWindow()
  assert.equal(w.accept('eid-a'), true, '第一次必须放行')
  assert.equal(w.accept('eid-a'), false, '同一条帧重投必须拒收')
  assert.equal(w.accept('eid-b'), true, '不同 eid 不受影响')
})

test('空 eid 一律拒收（缺字段的帧不能当新帧放进窗口）', () => {
  const w = new ReplayWindow()
  assert.equal(w.accept(''), false)
  assert.equal(w.size, 0, '拒收的帧不该占窗口')
})

test('超过容量按 FIFO 淘汰，最旧的 eid 重新变成「新帧」', () => {
  const w = new ReplayWindow(3)
  for (const id of ['a', 'b', 'c']) assert.equal(w.accept(id), true)
  assert.equal(w.size, 3)
  assert.equal(w.accept('d'), true, '超出容量仍要放行新帧')
  assert.equal(w.size, 3, '窗口必须保持有界')
  // b 是最旧的一条，被 a 的再次出现挤掉了。
  assert.equal(w.accept('a'), true, '被淘汰的 eid 当新帧处理')
  assert.equal(w.accept('b'), true, 'b 此时已被挤掉')
  // d 仍在窗口内 —— 它比 a/b 晚，只能由更新的帧挤掉。
  assert.equal(w.accept('d'), false, '窗口内的 eid 仍是重放')
})

test('超过 TTL 的 eid 会被清出窗口', () => {
  let now = 1_000_000
  const w = new ReplayWindow(512, 600_000, () => now)
  assert.equal(w.accept('old'), true)
  now += 600_001
  assert.equal(w.accept('old'), true, '过期之后同一 eid 允许再次通过')
  assert.equal(w.size, 1, '只留下刚放行的那条')
})

test('TTL 内未过期的不受影响', () => {
  let now = 1_000_000
  const w = new ReplayWindow(512, 600_000, () => now)
  assert.equal(w.accept('x'), true)
  now += 599_999
  assert.equal(w.accept('x'), false, 'TTL 边界内仍然是重放')
})
