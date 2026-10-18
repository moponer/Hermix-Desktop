/**
 * 补发的顺序控制。
 *
 * 盯的是一个真实竞态：重连后 PC 同时推实时帧（seq 4、5）和补发（seq 1..3），
 * 两条流到达顺序没有保证。实时帧先到就把进度推到 5，补发里的旧事件会被
 * 「比进度旧」过滤掉 —— 手机永远看不到断连那一段。
 */
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { SyncOrdering, isSyncResult } from '../core/syncOrder'

const frame = (seq: number, content = `m${seq}`) =>
  JSON.stringify({ type: 'assistant.message', seq, content })

function collector() {
  const got: string[] = []
  return { got, deliver: (t: string) => got.push(t) }
}

test('按 seq 去重：重复投递只送达一次', () => {
  const { got, deliver } = collector()
  const o = new SyncOrdering(deliver)
  o.offer(frame(1))
  o.offer(frame(2))
  o.offer(frame(2))
  o.offer(frame(1))
  assert.deepEqual(got, [frame(1), frame(2)])
  assert.equal(o.lastSeqSeen, 2)
})

test('补发期间实时帧排队，结果落地后按序放出', () => {
  const { got, deliver } = collector()
  const o = new SyncOrdering(deliver)
  o.begin()
  // 实时帧先到 —— 不能立刻送达，否则它会把进度推高
  o.offer(frame(4))
  o.offer(frame(5))
  assert.deepEqual(got, [], '补发在飞时实时帧必须排队')

  o.applyResult({ events: [{ seq: 1, type: 'assistant.message' }, { seq: 3, type: 'tool.start' }] })
  // 1、3 是补发，4、5 是攒下的实时帧，最终顺序按到达批次：补发先、实时后
  assert.deepEqual(
    got.map((t) => JSON.parse(t).seq),
    [1, 3, 4, 5],
    '断连期间的 1/3 不能被 4/5 挤掉',
  )
  assert.equal(o.lastSeqSeen, 5)
})

test('补发里比进度旧的事件被跳过（重叠区间不重复渲染）', () => {
  const { got, deliver } = collector()
  const o = new SyncOrdering(deliver)
  o.offer(frame(1))
  o.offer(frame(2))
  got.length = 0
  // PC 按 since_seq=2 补发，但多带了一条 2（边界包含关系可能对不齐）
  o.applyResult({ events: [{ seq: 2, type: 'assistant.message' }, { seq: 3, type: 'assistant.message' }] })
  assert.deepEqual(got.map((t) => JSON.parse(t).seq), [3])
})

test('truncated 必须回调 —— 缺口要看得见', () => {
  let truncatedShown = 0
  const o = new SyncOrdering(
    () => {},
    () => {
      truncatedShown += 1
    },
  )
  o.applyResult({ events: [], truncated: true })
  assert.equal(truncatedShown, 1)
  o.applyResult({ events: [], truncated: false })
  assert.equal(truncatedShown, 1, '没有缺口就不该打扰用户')
})

test('end() 放开队列：PC 不回 sync.result 时实时帧不能一直被扣着', () => {
  const { got, deliver } = collector()
  const o = new SyncOrdering(deliver)
  o.begin()
  o.offer(frame(7))
  assert.deepEqual(got, [])
  o.end() // 模拟超时
  assert.deepEqual(got, [frame(7)], '超时后必须放行')
  // 之后再来实时帧照常处理
  o.offer(frame(8))
  assert.equal(got.length, 2)
})

test('end() 幂等：没在补发时不重复放行', () => {
  const { got, deliver } = collector()
  const o = new SyncOrdering(deliver)
  o.offer(frame(1))
  o.end()
  o.end()
  assert.equal(got.length, 1)
})

test('isSyncResult 只认 sync.result，坏的 JSON 不算', () => {
  assert.equal(isSyncResult('{"type":"sync.result","events":[]}'), true)
  assert.equal(isSyncResult('{"type":"assistant.message"}'), false)
  assert.equal(isSyncResult('not json'), false)
})
