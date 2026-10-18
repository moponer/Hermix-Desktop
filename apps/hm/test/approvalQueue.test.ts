/**
 * 审批帧的接收队列。
 *
 * 盯的是「审批也是消息」这句：帧到了、被解析了、然后被 reducer 丢掉，
 * UI 只能另开一条 REST 轮询去发现它。这里把帧收下来，让审批页立刻被叫醒。
 */
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  noteApprovalRequest,
  forgetApproval,
  knownApprovals,
  clearApprovals,
  onApprovalRequest,
} from '../core/approvalQueue'

const hint = (requestId: string, tool = 'terminal') => ({
  requestId,
  tool,
  sid: 'im:device:dev-1',
  receivedAt: 1,
})

test('收到的审批请求能被列出', () => {
  clearApprovals()
  noteApprovalRequest(hint('r1'))
  noteApprovalRequest(hint('r2', 'write_file'))
  assert.deepEqual(
    knownApprovals().map((x) => x.requestId),
    ['r1', 'r2'],
  )
})

test('同一个 requestId 重复到达只留一条（relay 重投 / 补发重叠）', () => {
  clearApprovals()
  noteApprovalRequest(hint('r1'))
  noteApprovalRequest(hint('r1'))
  assert.equal(knownApprovals().length, 1)
})

test('重复到达仍然通知订阅者：补回来的那条正需要把页面叫醒', () => {
  clearApprovals()
  let hits = 0
  const off = onApprovalRequest(() => {
    hits += 1
  })
  noteApprovalRequest(hint('r1'))
  noteApprovalRequest(hint('r1'))
  assert.equal(hits, 2, '幂等是队列的事，通知不是')
  off()
})

test('退订后不再收到通知', () => {
  clearApprovals()
  let hits = 0
  const off = onApprovalRequest(() => {
    hits += 1
  })
  noteApprovalRequest(hint('r1'))
  off()
  noteApprovalRequest(hint('r2'))
  assert.equal(hits, 1)
})

test('某个订阅者抛错不影响其它订阅者', () => {
  clearApprovals()
  let hits = 0
  const off1 = onApprovalRequest(() => {
    throw new Error('ui boom')
  })
  const off2 = onApprovalRequest(() => {
    hits += 1
  })
  assert.doesNotThrow(() => noteApprovalRequest(hint('r1')))
  assert.equal(hits, 1)
  off1()
  off2()
})

test('决策完成后摘掉，不再被列出', () => {
  clearApprovals()
  noteApprovalRequest(hint('r1'))
  forgetApproval('r1')
  assert.equal(knownApprovals().length, 0)
  // 摘掉不存在的 id 也不该炸
  forgetApproval('nope')
})
