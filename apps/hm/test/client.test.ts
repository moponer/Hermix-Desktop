/**
 * 客户端侧的不变量：这些是「改错了不会立刻报错、但线上会出事」的地方。
 */
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { computeSignature, newNonce, signatureHeader } from '../core/signature'
import { encodeEnvelope, parseEnvelope, PROTO_VERSION } from '../core/envelope'

const TOKEN = 'hmxd_test_token'
const SID = 'im:device:dev-aaa'
const PATH = `/v1/ws-stream/${SID}`

test('签名 header 形状正确且可被 Rust 侧解析', async () => {
  const h = await signatureHeader(TOKEN, 'GET', PATH)
  const m = /^t=(\d+),n=([0-9a-f]{32}),s=([0-9a-f]{64})$/.exec(h)
  assert.ok(m, `header 形状不对：${h}`)
  // t / n / s 三个字段的顺序与类型都要与 request_signature.rs 一致
  assert.ok(Number(m![1]) > 1_600_000_000)
})

test('每次请求 nonce 都不同（服务端按 nonce 去重，重放会被判 BadSignature）', async () => {
  const a = await signatureHeader(TOKEN, 'GET', PATH)
  const b = await signatureHeader(TOKEN, 'GET', PATH)
  const nonce = (h: string) => /n=([0-9a-f]{32})/.exec(h)![1]
  assert.notEqual(nonce(a), nonce(b))
  assert.notEqual(newNonce(), newNonce())
})

test('同一份 nonce 两次算出同一签名（签名只依赖 token+method+path+t+nonce）', async () => {
  const args = [TOKEN, 'GET', PATH, 1700000000, 'fixed'] as const
  assert.equal(await computeSignature(...args), await computeSignature(...args))
})

test('换 session_id 就换签名路径 —— 冒用别的会话不可能通过', async () => {
  const mine = await computeSignature(TOKEN, 'GET', '/v1/ws-stream/im:device:dev-aaa', 1700000000, 'n')
  const theirs = await computeSignature(TOKEN, 'GET', '/v1/ws-stream/im:device:dev-bbb', 1700000000, 'n')
  assert.notEqual(mine, theirs, '不同会话的路径必须产出不同签名')
})

test('带 auth 的帧往返后 auth 完整（hermix 在 PC 侧靠它验）', async () => {
  const sig = await signatureHeader(TOKEN, 'GET', PATH)
  const wire = encodeEnvelope({
    protoVersion: PROTO_VERSION,
    tag: 'route',
    sessionId: SID,
    auth: { bearer: TOKEN, signature: sig },
    // v2 起 payload 必须是密封信封，否则 parseEnvelope 直接拒——这正是
    // 「明文帧不许静默通过」的验证点之一。
    payload: JSON.stringify({
      ct: Buffer.from('{"type":"user_message"}', 'utf8').toString('base64'),
      nonce: Buffer.alloc(12, 7).toString('base64'),
      ep: Buffer.alloc(65, 4).toString('base64'),
      eid: Buffer.from('frame-1', 'utf8').toString('base64'),
    }),
  })
  const back = parseEnvelope(wire)
  assert.ok(back.ok)
  assert.equal(back.envelope.sessionId, SID)
  assert.equal(back.envelope.auth?.bearer, TOKEN)
  assert.equal(back.envelope.auth?.signature, sig)
})

test('明文 route 帧被拒（v2 起 E2EE 是硬要求）', () => {
  const wire = encodeEnvelope({
    protoVersion: PROTO_VERSION,
    tag: 'route',
    sessionId: SID,
    payload: '{"type":"user_message"}',
  })
  const back = parseEnvelope(wire)
  assert.equal(back.ok, false)
  if (!back.ok) {
    assert.match(back.reason, /plaintext/)
    assert.equal(back.versionMismatch, false, '是格式问题不是版本问题')
  }
})
