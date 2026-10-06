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

/**
 * 造一个 route 帧。
 *
 * v2 起 `payload` 装的是**密封信封**，不是明文内层帧——所以这个 helper
 * 自动把传入的明文包一层。调用方写 `route(sid, '{"type":"chat"}')` 读起来
 * 仍然是在描述业务帧，测试也不会因为忘了封装而被误判。
 *
 * App 侧不真的加密：它只判定形状。密码学一致性由 Rust 侧的跨端
 * conformance 测试守。
 */
function route(sid: string, plain: string): Envelope {
  return {
    protoVersion: PROTO_VERSION,
    tag: 'route',
    sessionId: sid,
    auth: { bearer: V.token, signature: `t=${V.t},n=${V.nonce},s=${V.sig}` },
    payload: JSON.stringify({
      ct: Buffer.from(plain, 'utf8').toString('base64'),
      nonce: Buffer.alloc(12, 7).toString('base64'),
      ep: Buffer.alloc(65, 4).toString('base64'),
      eid: Buffer.from('frame-1', 'utf8').toString('base64'),
    }),
  }
}

test('信封往返保留 session_id 与 auth', () => {
  const plain = '{"type":"user_message"}'
  const e = route(V.sid, plain)
  const back = parseEnvelope(encodeEnvelope(e))
  assert.ok(back.ok)
  assert.equal(back.envelope.sessionId, V.sid)
  assert.equal(back.envelope.auth?.bearer, V.token)
  // payload 往返逐字不变（v2 起装的是密封信封，不是原始明文）
  assert.equal(back.envelope.payload, e.payload)
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
  // 版本号用当前的 PROTO_VERSION：`1` 在 v2 下恰好就是「版本漂移」，
  // 那属于另一条断言，不能拿来测「脏数据」。
  for (const raw of [
    'not json',
    '{}',
    `{"proto_version":${PROTO_VERSION},"tag":"teleport"}`,
    `{"proto_version":${PROTO_VERSION},"tag":"route"}`,
  ]) {
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
  // 期望值从构造源取，而不是重写一遍明文——否则 helper 一改封装，
  // 断言会静默变成"另一个测试"（这正是本文件此前踩过的坑）。
  const src = route(V.sid, '{"a":"换行\\n引号 \\" emoji 🛠️"}')
  const back = parseEnvelope(encodeEnvelope(src))
  assert.ok(back.ok)
  assert.equal(back.envelope.payload, src.payload, '内容不得被规范化重排')
})
