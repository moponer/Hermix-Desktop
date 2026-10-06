'use strict';

/**
 * relay 行为测试。`node --test`。
 *
 * 重点钉三条性质（它们坏了都不会报错，只会"连上但没反应"）：
 * 1. payload **原样**透传（不解析、不改写、不重排字段）；
 * 2. 版本不符在握手/首帧就拒，不降级；
 * 3. 路由按 node 隔离，A 手机收不到 B 的消息。
 */

const test = require('node:test');
const assert = require('node:assert');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { WebSocket } = require('ws');

const { createRelay } = require('../src/server');
const { openRegistry, hashToken, bearerFromHeaders } = require('../src/auth');
const { Router } = require('../src/router');
const { parseEnvelope, buildEnvelope, isSealed, PROTO_VERSION, TAG } = require('../src/proto');

function tmpDb() {
  return path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'relay-test-')), 'relay.db');
}

/**
 * 造一个**形状正确**的密封信封。
 *
 * relay 不解密也不需要解密——它只判定"这一帧是不是受保护的"，所以
 * 这里塞什么密文内容都无所谓。真正的密码学一致性由
 * `crates/hermix-server/src/relay_crypto.rs` 的跨端 conformance 测试守。
 */
function sealedPayload(body = 'hello') {
  return JSON.stringify({
    ct: Buffer.from(body, 'utf8').toString('base64'),
    nonce: Buffer.alloc(12, 7).toString('base64'),
    ep: Buffer.alloc(65, 4).toString('base64'),
    eid: Buffer.from('frame-1', 'utf8').toString('base64'),
  });
}

// ---------------------------------------------------------------- proto.js

test('proto: 版本不符被单独标出（不降级）', () => {
  const bad = JSON.stringify({ proto_version: PROTO_VERSION + 1, tag: 'route' });
  const r = parseEnvelope(bad);
  assert.strictEqual(r.ok, false);
  assert.strictEqual(r.versionMismatch, true, '必须能区分"版本漂移"与"脏数据"');
});

test('proto: 脏数据不算版本问题', () => {
  // 版本号写成当前的 PROTO_VERSION：`1` 在 v2 下恰好就是"版本漂移"，
  // 那属于另一条断言（第 50 行），不能拿来测"脏数据"。
  for (const raw of [
    'not json',
    '{}',
    `{"proto_version":${PROTO_VERSION},"tag":"teleport"}`,
    `{"proto_version":${PROTO_VERSION},"tag":"route"}`,
  ]) {
    const r = parseEnvelope(raw);
    assert.strictEqual(r.ok, false);
    assert.strictEqual(r.versionMismatch, false, '脏数据不该被误报成版本漂移: ' + raw);
  }
});

test('proto: 可选字段缺省时不出现在 JSON 里', () => {
  const s = buildEnvelope(TAG.ACK);
  assert.ok(!s.includes('token_hash'));
  assert.ok(!s.includes('node_id'));
  assert.ok(!s.includes('payload'));
});

test('proto: 密封 payload 往返逐字节不变', () => {
  const raw = sealedPayload('{"a":"换行\n引号 \" emoji 🛠️"}');
  const env = { proto_version: PROTO_VERSION, tag: 'route', payload: raw };
  const back = parseEnvelope(JSON.stringify(env));
  assert.strictEqual(back.envelope.payload, raw);
});

test('proto: 明文 route 帧被拒（v2 起 E2EE 是硬要求）', () => {
  // 不静默降级 —— 静默放行等于 E2EE 被无声关闭。
  const env = { proto_version: PROTO_VERSION, tag: 'route', payload: '{"type":"chat","text":"hi"}' };
  const r = parseEnvelope(JSON.stringify(env));
  assert.strictEqual(r.ok, false);
  assert.match(r.reason, /plaintext/);
  assert.strictEqual(r.versionMismatch, false, '是格式问题，不是版本问题');
});

test('proto: 非 route 帧不要求密文', () => {
  // ack / close / hello 由 relay 自己在明文侧构造，不走密封信封。
  for (const tag of [TAG.ACK, TAG.CLOSE]) {
    const r = parseEnvelope(JSON.stringify({ proto_version: PROTO_VERSION, tag }));
    assert.strictEqual(r.ok, true, tag + ' 不该被要求密文');
  }
});

test('isSealed: 字段不全一律判为明文（不做部分接受）', () => {
  assert.strictEqual(isSealed(sealedPayload()), true);
  for (const missing of ['ct', 'nonce', 'ep', 'eid']) {
    const o = JSON.parse(sealedPayload());
    delete o[missing];
    assert.strictEqual(isSealed(JSON.stringify(o)), false, '缺 ' + missing + ' 仍被判为密封');
  }
  assert.strictEqual(isSealed(''), false);
  assert.strictEqual(isSealed('not json'), false);
  assert.strictEqual(isSealed(null), false);
});

// ---------------------------------------------------------------- auth.js

test('auth: 明文不落库，只有哈希', () => {
  const db = tmpDb();
  const reg = openRegistry(db);
  const token = reg.register({ nodeId: 'pc-1', role: 'phone' });
  assert.ok(token.startsWith('hrel_'));
  // 库里能查到哈希，但查不到明文
  const row = reg.list('pc-1')[0];
  assert.strictEqual(row.token_hash, hashToken(token));
  assert.ok(!JSON.stringify(row).includes(token));
  reg.close();
});

test('auth: 撤销后立即失效', () => {
  const reg = openRegistry(tmpDb());
  const token = reg.register({ nodeId: 'pc-1', role: 'phone' });
  assert.ok(reg.authenticate(token));
  assert.strictEqual(reg.revokeToken(token), true);
  assert.strictEqual(reg.authenticate(token), null, '撤销后必须立刻拒绝');
  // 重复撤销返回 false，幂等
  assert.strictEqual(reg.revokeToken(token), false);
  reg.close();
});

test('auth: 角色必须合法', () => {
  const reg = openRegistry(tmpDb());
  assert.throws(() => reg.register({ nodeId: 'pc-1', role: 'god' }), /role/);
  assert.throws(() => reg.register({ nodeId: '', role: 'phone' }), /nodeId/);
  reg.close();
});

test('auth: 只认 Authorization 头，query 传参一律不认', () => {
  assert.strictEqual(bearerFromHeaders({ authorization: 'Bearer abc' }), 'abc');
  assert.strictEqual(bearerFromHeaders({ authorization: 'bearer   abc  ' }), 'abc');
  assert.strictEqual(bearerFromHeaders({ authorization: 'Basic abc' }), null);
  assert.strictEqual(bearerFromHeaders({}), null);
  assert.strictEqual(bearerFromHeaders({ authorization: 'Bearer ' }), null);
  assert.strictEqual(bearerFromHeaders({}), null, 'query 通道已移除');
});

// -------------------------------------------------------------- router.js

/** 假连接：只记录被发出去的帧。 */
function fakeConn() {
  const sent = [];
  return {
    sent,
    sendFrame: (env) => sent.push(env),
    sendRaw: (text) => sent.push(text),
  };
}

test('router: 手机 → 电脑，按 node 隔离', () => {
  const events = [];
  const r = new Router({ audit: { write: (...a) => events.push(a) } });
  const nodeA = fakeConn();
  r.attachNode('pc-A', nodeA);
  r.attachPhone('pc-A', 'hash-phone-1', fakeConn());

  const payload = sealedPayload('hello');
  r.phoneToNode('pc-A', 'hash-phone-1', { tag: 'route', payload });
  assert.strictEqual(nodeA.sent.length, 1);
  assert.strictEqual(nodeA.sent[0].payload, payload, 'payload 不得被改写');
});

test('router: 没有电脑在线时如实报告，不静默丢弃', () => {
  const r = new Router({ audit: { write: () => {} } });
  r.attachPhone('pc-B', 'h', fakeConn());
  assert.strictEqual(r.phoneToNode('pc-B', 'h', { tag: 'route' }), 'no_node');
});

test('router: 电脑 → 多台手机全部投递（不负载均衡）', () => {
  const r = new Router({ audit: { write: () => {} } });
  r.attachNode('pc-C', fakeConn());
  const p1 = fakeConn();
  const p2 = fakeConn();
  r.attachPhone('pc-C', 'h1', p1);
  r.attachPhone('pc-C', 'h2', p2);
  assert.strictEqual(r.nodeToPhones('pc-C', { tag: 'route', payload: sealedPayload('x') }), 2);
  assert.strictEqual(p1.sent.length, 1);
  assert.strictEqual(p2.sent.length, 1, '审批卡不能只出现在"某台"手机上');
});

test('router: 不同 node 的手机互不可见', () => {
  const r = new Router({ audit: { write: () => {} } });
  const nodeA = fakeConn();
  const nodeB = fakeConn();
  r.attachNode('pc-A', nodeA);
  r.attachNode('pc-B', nodeB);
  const phoneA = fakeConn();
  r.attachPhone('pc-A', 'hash-a', phoneA);
  // pc-B 的帧只到 pc-B
  r.nodeToPhones('pc-B', { tag: 'route', payload: sealedPayload('only-B') });
  assert.strictEqual(phoneA.sent.length, 0, 'A 手机不得收到 B 的消息');
  assert.strictEqual(nodeB.sent.length, 0);
});

test('router: 全部断开后槽位被回收', () => {
  const r = new Router({ audit: { write: () => {} } });
  const node = fakeConn();
  r.attachNode('pc-D', node);
  r.detach(node);
  assert.strictEqual(r.snapshot().nodes.length, 0, '空槽位必须回收，否则长跑进程攒内存');
});

// --------------------------------------------------------- 端到端（真 WS）

test('端到端: 手机 ↔ 电脑 经 relay 往返一帧', async () => {
  const db = tmpDb();
  const relay = createRelay({ dbPath: db });
  const phoneToken = relay.registry.register({ nodeId: 'e2e', role: 'phone' });
  const nodeToken = relay.registry.register({ nodeId: 'e2e', role: 'node' });

  await new Promise((r) => relay.listen(0, r));
  const { port } = relay.server.address();

  const open = (path, token) =>
    new Promise((resolve, reject) => {
      const ws = new WebSocket(`ws://127.0.0.1:${port}${path}`, {
        headers: { Authorization: `Bearer ${token}` },
      });
      ws.once('open', () => resolve(ws));
      ws.once('error', reject);
      ws.once('unexpected-response', (_req, res) => reject(new Error(`HTTP ${res.statusCode}`)));
    });

  const node = await open('/relay/node', nodeToken);
  const phone = await open('/relay/phone', phoneToken);

  // 两端先各收一个 ACK
  await new Promise((r) => setTimeout(r, 60));

  const payload = sealedPayload('{"type":"user_message","content":"手机 → 电脑"}');
  phone.send(JSON.stringify({ proto_version: PROTO_VERSION, tag: 'route', payload }));

  const got = await new Promise((resolve) => {
    node.once('message', (data) => resolve(JSON.parse(data.toString('utf8'))));
    setTimeout(() => resolve(null), 2000);
  });
  assert.ok(got, '电脑侧应收到帧');
  assert.strictEqual(got.payload, payload, 'payload 必须逐字不变');
  assert.strictEqual(got.tag, 'route');

  // 反向
  const back = sealedPayload(JSON.stringify({ type: 'assistant_message', content: '电脑 → 手机' }));
  node.send(JSON.stringify({ proto_version: PROTO_VERSION, tag: 'route', payload: back }));
  const gotBack = await new Promise((resolve) => {
    phone.once('message', (data) => resolve(JSON.parse(data.toString('utf8'))));
    setTimeout(() => resolve(null), 2000);
  });
  assert.ok(gotBack);
  assert.strictEqual(gotBack.payload, back);

  node.close();
  phone.close();
  relay.close();
});

test('端到端: 未登记的 token 被 401 挡在门外', async () => {
  const relay = createRelay({ dbPath: tmpDb() });
  await new Promise((r) => relay.listen(0, r));
  const { port } = relay.server.address();

  const status = await new Promise((resolve) => {
    const ws = new WebSocket(`ws://127.0.0.1:${port}/relay/phone`, {
      headers: { Authorization: 'Bearer hrel_bogus' },
    });
    ws.once('unexpected-response', (_req, res) => resolve(res.statusCode));
    ws.once('open', () => resolve('opened'));
    ws.once('error', () => resolve('error'));
  });
  assert.strictEqual(status, 401, '未登记 token 绝不能连上');
  relay.close();
});

test('端到端: 角色不匹配被拒（手机不能占电脑的槽）', async () => {
  const relay = createRelay({ dbPath: tmpDb() });
  const phoneToken = relay.registry.register({ nodeId: 'mix', role: 'phone' });
  await new Promise((r) => relay.listen(0, r));
  const { port } = relay.server.address();

  const status = await new Promise((resolve) => {
    // 用手机 token 去连电脑入口
    const ws = new WebSocket(`ws://127.0.0.1:${port}/relay/node`, {
      headers: { Authorization: `Bearer ${phoneToken}` },
    });
    ws.once('unexpected-response', (_req, res) => resolve(res.statusCode));
    ws.once('open', () => resolve('opened'));
    ws.once('error', () => resolve('error'));
  });
  assert.strictEqual(status, 403);
  relay.close();
});
