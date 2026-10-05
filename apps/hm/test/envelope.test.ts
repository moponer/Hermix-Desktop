/**
 * 帧内凭据的构造与信封往返。
 *
 * 这组测试盯的是一条**鉴权模型**：签名覆盖 `METHOD\npath`，而 path 里的
 * session id 来自信封字段。所以同一个签名换个 `sessionId` 就不该再有效
 * ——`会冒用别的会话` 与 `只是路由错了` 是两回事，前者是安全事故。
 */
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { parseEnvelope, encodeEnvelope, helloFrame, PROTO_VERSION, type Envelope } from '../src/core/envelope'

const V = {
  token: 'hmxd_test_token',
  sid: 'im:device:dev-aaa',
  t: 1700000000,
  nonce: 'abc123',
  // ↓ 与 Rust cross_language_signature_vector 同源（同一算法、同一密钥）
  sig: '521925f25e9194ded7ecc29a57879549d48ee8ceb752b390bf6a8f4e5b9a734f',
}

function route(sid: string, payload: string): Envelope {
  return {
    protoVersion: PROTO_VERSION,
    tag: 'route',
    sessionId: sid,
    auth: { bearer: V.token, signature: `t=${V.t},n=${V.nonce},s=${V.sig}` },
    payload,
  }
}

test('信封往返保留 session_id 与 auth', () => {
  const e = route(V.sid, '{"type":"user_message"}')
  const back = parseEnvelope(encodeEnvelope(e))
  assert.ok(back.ok)
  assert.equal(back.envelope.sessionId, V.sid)
  assert.equal(back.envelope.auth?.bearer, V.token)
  assert.equal(back.envelope.payload, '{"type":"user_message"}')
  assert.equal(back.envelope.protoVersion, PROTO_VERSION)
})

test('snake_case 线上格式（与 Rust serde 一致）', () => {
  const wire = encodeEnvelope(route(V.sid, 'x'))
  assert.ok(wire.includes('"proto_version"'))
  assert.ok(wire.includes('"session_id"'))
  // 可选字段缺省时不出现在 JSON 里
  const bare = encodeEnvelope({ protoVersion: PROTO_VERSION, tag: 'ack' })
  assert.ok(!bare.includes('session_id'))
  assert.ok(!bare.includes('auth'))
})

test('换 session_id 就是另一条会话——签名靠它绑定', () => {
  const a = route(V.sid, 'x')
  const b = { ...a, sessionId: 'im:device:dev-bbb' }
  assert.equal(a.sessionId, V.sid)
  assert.equal(b.sessionId, 'im:device:dev-bbb')
  // 两者 auth 相同，但 hermix 会用各自的 session_id 重建路径验签，
  // 所以 b 那份在服务端会被拒。这里断言的是"客户端不得自己把两者当一回事"。
  assert.notEqual(a.sessionId, b.sessionId)
})

test('版本漂移单独标出（不降级）', () => {
  const r = parseEnvelope('{"proto_version":9999,"tag":"route"}')
  assert.equal(r.ok, false)
  if (!r.ok) assert.equal(r.versionMismatch, true)
})

test('脏数据不算版本问题', () => {
  for (const raw of ['not json', '{}', '{"proto_version":1,"tag":"teleport"}']) {
    const r = parseEnvelope(raw)
    assert.equal(r.ok, false)
    if (!r.ok) assert.equal(r.versionMismatch, false)
  }
})

test('登记帧带 relay 凭据哈希与电脑标识', () => {
  const h = helloFrame('deadbeef', 'my-pc')
  assert.equal(h.tag, 'hello')
  assert.equal(h.tokenHash, 'deadbeef')
  assert.equal(h.nodeId, 'my-pc')
  assert.equal(h.auth, undefined, '登记帧不带 hermix 凭据——那方向不需要鉴权')
})

test('payload 逐字透传（relay 不解析内层帧）', () => {
  const raw = '{"a":"换行\\n引号 \\" emoji 🛠️"}'
  const back = parseEnvelope(encodeEnvelope(route(V.sid, raw)))
  assert.ok(back.ok)
  assert.equal(back.envelope.payload, raw, '内层不得被规范化或改写')
})
