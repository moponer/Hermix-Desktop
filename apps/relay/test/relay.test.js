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

const http = require('node:http');
const { createRelay } = require('../src/server');
const { openRegistry, hashToken, bearerFromHeaders, deriveNodeIdFromPublicKey } = require('../src/auth');
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

// ------------------------------------------------- server.js 签发手机凭据

/**
 * 起一个真 relay 实例，用 fetch 打它的 HTTP 面。
 *
 * 这里**必须**用真 HTTP 而不直接调函数：要验的正是「没有 Bearer 就 401」
 * 与「只有 node 角色能签发」这两条边界，而它们发生在中间件与路由层。
 */
async function withRelay(fn) {
  const { server, registry } = createRelay({ dbPath: tmpDb() });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const base = `http://127.0.0.1:${server.address().port}`;
  try {
    return await fn({ base, registry });
  } finally {
    await new Promise((resolve) => server.close(resolve));
    registry.close();
  }
}

test('issue-phone: 无凭据被 401', async () => {
  await withRelay(async ({ base }) => {
    const r = await fetch(`${base}/api/relay/issue-phone`, { method: 'POST' });
    assert.strictEqual(r.status, 401);
  });
});

// ------------------------------------------------------------ enroll-node

// 与 Rust 侧 `relay_settings::derive_node_id` 共用的固定向量（sha256(65 字节
// 原始公钥) 前 4 字节 hex）。两边任一漂移必有一边红——见 hermix-server 测试
// `node_id_derivation_matches_relay_implementation`。
const ENROLL_KEY_B64 =
  'BAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKissLS4vMDEyMzQ1Njc4OTo7PD0+P0A=';
const ENROLL_EXPECTED_NODE = 'hermix-0ed3a6ab';

test('enroll-node: node_id 由公钥推导，与 Rust 同向量', () => {
  assert.strictEqual(deriveNodeIdFromPublicKey(ENROLL_KEY_B64), ENROLL_EXPECTED_NODE);
  for (const bad of [
    '',
    'not base64!!',
    Buffer.alloc(64, 4).toString('base64'), // 少一个字节
    Buffer.alloc(65, 1).toString('base64'), // 前缀不是 0x04
  ]) {
    assert.strictEqual(deriveNodeIdFromPublicKey(bad), null, `非法输入必须拒: ${bad}`);
  }
});

test('enroll-node: 匿名签发一条 node 凭据，且真的能连上 /relay/node', async () => {
  await withRelay(async ({ base, registry }) => {
    const r = await fetch(`${base}/api/relay/enroll-node`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ public_key: ENROLL_KEY_B64 }),
    });
    assert.strictEqual(r.status, 200);
    const { token, node_id } = await r.json();
    assert.strictEqual(node_id, ENROLL_EXPECTED_NODE, 'node_id 必须由 relay 从公钥推导');
    assert.ok(token.startsWith('hrel_'));

    const who = registry.authenticate(token);
    assert.ok(who, '签出来的凭据必须立刻可用');
    assert.strictEqual(who.role, 'node');

    // 端到端：凭据必须能完成 WS 握手，不只是过了 authenticate。
    const ws = new WebSocket(`${base.replace('http', 'ws')}/relay/node`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    const opened = await new Promise((resolve) => {
      ws.once('open', () => resolve(true));
      ws.once('unexpected-response', (_q, res) => resolve(res.statusCode));
      ws.once('error', () => resolve(false));
    });
    assert.strictEqual(opened, true, 'enroll 出的凭据必须能连上 /relay/node');
    ws.close();
  });
});

test('enroll-node: 同一身份重复签发被 409（先到先得）', async () => {
  await withRelay(async ({ base }) => {
    const first = await fetch(`${base}/api/relay/enroll-node`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ public_key: ENROLL_KEY_B64 }),
    });
    assert.strictEqual(first.status, 200);

    const second = await fetch(`${base}/api/relay/enroll-node`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ public_key: ENROLL_KEY_B64 }),
    });
    assert.strictEqual(second.status, 409, '持公钥者不能在真电脑离线时补签占槽');
  });
});

test('enroll-node: 客户端自报 node_id 一律无效', async () => {
  await withRelay(async ({ base }) => {
    // 只认 public_key 推导：多带一个 node_id 字段改变不了归属。
    const r = await fetch(`${base}/api/relay/enroll-node`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ public_key: ENROLL_KEY_B64, node_id: 'hermix-attacker' }),
    });
    const { node_id } = await r.json();
    assert.strictEqual(node_id, ENROLL_EXPECTED_NODE);
  });
});

test('enroll-node: 非法公钥被 400', async () => {
  await withRelay(async ({ base }) => {
    const r = await fetch(`${base}/api/relay/enroll-node`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ public_key: 'garbage' }),
    });
    assert.strictEqual(r.status, 400);
  });
});


test('issue-phone: 凭据无效被 401', async () => {
  await withRelay(async ({ base }) => {
    const r = await fetch(`${base}/api/relay/issue-phone`, {
      method: 'POST',
      headers: { authorization: 'Bearer hrel_nope' },
    });
    assert.strictEqual(r.status, 401);
  });
});

test('issue-phone: node 凭据换到同 node 名下的手机凭据', async () => {
  await withRelay(async ({ base, registry }) => {
    const nodeToken = registry.register({ nodeId: 'pc-1', role: 'node' });
    const r = await fetch(`${base}/api/relay/issue-phone`, {
      method: 'POST',
      headers: { authorization: `Bearer ${nodeToken}` },
      body: JSON.stringify({ label: '我的手机' }),
    });
    assert.strictEqual(r.status, 200);
    const { token, node_id } = await r.json();
    assert.strictEqual(node_id, 'pc-1', '必须挂在请求方自己的 node 名下');
    assert.ok(token.startsWith('hrel_'), '明文只此一次下发');

    // 签发出来的东西必须**真的能用**，否则等于给了一个假凭证
    const who = registry.authenticate(token);
    assert.ok(who, '新凭据必须能通过校验');
    assert.strictEqual(who.role, 'phone');
    assert.strictEqual(who.nodeId, 'pc-1');
  });
});

test('issue-phone: 手机凭据不能再换手机凭据（防凭据扩散）', async () => {
  await withRelay(async ({ base, registry }) => {
    const phoneToken = registry.register({ nodeId: 'pc-1', role: 'phone' });
    const r = await fetch(`${base}/api/relay/issue-phone`, {
      method: 'POST',
      headers: { authorization: `Bearer ${phoneToken}` },
    });
    assert.strictEqual(r.status, 403);
  });
});

test('revoke-phone: 按哈希撤销并断连，凭据随即失效', async () => {
  await withRelay(async ({ base, registry }) => {
    const nodeToken = registry.register({ nodeId: 'pc-1', role: 'node' });
    const issued = await (
      await fetch(`${base}/api/relay/issue-phone`, {
        method: 'POST',
        headers: { authorization: `Bearer ${nodeToken}` },
      })
    ).json();
    const { hashToken } = require('../src/auth');
    const hash = hashToken(issued.token);
    assert.ok(registry.isActiveHash(hash), '撤销前必须有效');

    const r = await fetch(`${base}/api/relay/revoke-phone`, {
      method: 'POST',
      headers: { authorization: `Bearer ${nodeToken}`, 'content-type': 'application/json' },
      body: JSON.stringify({ token_hash: hash }),
    });
    assert.strictEqual(r.status, 200);
    const body = await r.json();
    assert.strictEqual(body.revoked, true);
    assert.ok(!registry.isActiveHash(hash), '撤销后必须立刻失效');
  });
});

test('revoke-phone: 手机凭据不能撤销别人', async () => {
  await withRelay(async ({ base, registry }) => {
    const phoneToken = registry.register({ nodeId: 'pc-1', role: 'phone' });
    const r = await fetch(`${base}/api/relay/revoke-phone`, {
      method: 'POST',
      headers: { authorization: `Bearer ${phoneToken}`, 'content-type': 'application/json' },
      body: JSON.stringify({ token_hash: 'a'.repeat(64) }),
    });
    assert.strictEqual(r.status, 403);
  });
});

test('revoke-phone: 哈希格式不对被 400，不当成"没找到"', async () => {
  await withRelay(async ({ base, registry }) => {
    const nodeToken = registry.register({ nodeId: 'pc-1', role: 'node' });
    const r = await fetch(`${base}/api/relay/revoke-phone`, {
      method: 'POST',
      headers: { authorization: `Bearer ${nodeToken}`, 'content-type': 'application/json' },
      body: JSON.stringify({ token_hash: 'not-a-hash' }),
    });
    assert.strictEqual(r.status, 400);
  });
});

test('revoke-phone: 撤销一个本机没有的哈希是幂等 200，不让 hermix 侧失败', async () => {
  // hermix 里撤销设备时，手机可能压根没走过 relay（只做过局域网直连）。
  // 那种情况下 relay 侧"没记录"是正常的，不该把整个撤销操作变成失败。
  await withRelay(async ({ base, registry }) => {
    const nodeToken = registry.register({ nodeId: 'pc-1', role: 'node' });
    const r = await fetch(`${base}/api/relay/revoke-phone`, {
      method: 'POST',
      headers: { authorization: `Bearer ${nodeToken}`, 'content-type': 'application/json' },
      body: JSON.stringify({ token_hash: 'b'.repeat(64) }),
    });
    assert.strictEqual(r.status, 200);
    assert.strictEqual((await r.json()).revoked, false);
  });
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
  r.attachNode('pc-A', 'hash-node-A', nodeA);
  r.attachPhone('pc-A', 'hash-phone-1', fakeConn());

  const payload = sealedPayload('hello');
  r.phoneToNode('pc-A', 'hash-phone-1', { tag: 'route', payload });
  assert.strictEqual(nodeA.sent.length, 1);
  assert.strictEqual(nodeA.sent[0].payload, payload, 'payload 不得被改写');
});

test('router: 手机帧无条件盖章 token_hash（手机自报的不可信）', () => {
  // PC 侧的配对错误预算按「帧来自哪条手机凭据」分桶。手机若能自报哈希，
  // 穷举者每帧换一个假哈希就能绕开预算——章必须由 relay 盖。
  const r = new Router({ audit: { write: () => {} } });
  const node = fakeConn();
  r.attachNode('pc-A', 'hash-node-A', node);
  r.attachPhone('pc-A', 'hash-phone-1', fakeConn());

  r.phoneToNode('pc-A', 'hash-phone-1', { tag: 'route', payload: sealedPayload('x') });
  assert.strictEqual(node.sent[0].token_hash, 'hash-phone-1', '缺失时补盖');

  r.phoneToNode('pc-A', 'hash-phone-1', {
    tag: 'route', payload: sealedPayload('y'), token_hash: 'FAKE',
  });
  assert.strictEqual(node.sent[1].token_hash, 'hash-phone-1', '自报的假值必须被覆盖');
});

test('router: 没有电脑在线时如实报告，不静默丢弃', () => {
  const r = new Router({ audit: { write: () => {} } });
  r.attachPhone('pc-B', 'h', fakeConn());
  assert.strictEqual(r.phoneToNode('pc-B', 'h', { tag: 'route' }), 'no_node');
});

test('router: 电脑 → 当前那台手机投递（N:1）', () => {
  const r = new Router({ audit: { write: () => {} } });
  r.attachNode('pc-C', 'hash-node-C', fakeConn());
  const p1 = fakeConn();
  r.attachPhone('pc-C', 'h1', p1);
  assert.strictEqual(r.nodeToPhones('pc-C', { tag: 'route', payload: sealedPayload('x') }), 1);
  assert.strictEqual(p1.sent.length, 1);
});

test('router: 第二台手机被拒（每台桌面同时只能被一台手机连着）', () => {
  // N:1 的核心断言。手机端可以管多台桌面，但**每台桌面同时只允许一台手机**
  // 接入——不约束就等于一台电脑的会话内容同时流向任意多台手机。
  const r = new Router({ audit: { write: () => {} } });
  r.attachNode('pc-C', 'hash-node-C', fakeConn());
  const p1 = fakeConn();
  const p2 = fakeConn();
  assert.strictEqual(r.attachPhone('pc-C', 'h1', p1), 'attached');
  assert.strictEqual(r.attachPhone('pc-C', 'h2', p2), 'conflict', '第二台手机必须被拒');
  assert.strictEqual(
    r.nodeToPhones('pc-C', { tag: 'route', payload: sealedPayload('x') }),
    1,
    '被拒的那台不得收到任何东西',
  );
  assert.strictEqual(p1.sent.length, 1, '原手机正常收');
  assert.strictEqual(p2.sent.length, 0);
});

test('router: 同一凭据重连可以顶掉自己的旧连接（断线重连不是换人）', () => {
  const r = new Router({ audit: { write: () => {} } });
  const first = fakeConn();
  r.attachPhone('pc-H', 'hash-self', first);
  const second = fakeConn();
  assert.strictEqual(r.attachPhone('pc-H', 'hash-self', second), 'replaced');
  assert.strictEqual(r.slots.get('pc-H').phoneConns.get('hash-self'), second);
});

test('router: 不同 node 的手机互不可见', () => {
  const r = new Router({ audit: { write: () => {} } });
  const nodeA = fakeConn();
  const nodeB = fakeConn();
  r.attachNode('pc-A', 'hash-node-A', nodeA);
  r.attachNode('pc-B', 'hash-node-B', nodeB);
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
  r.attachNode('pc-D', 'hash-node-D', node);
  r.detach(node);
  assert.strictEqual(r.snapshot().nodes.length, 0, '空槽位必须回收，否则长跑进程攒内存');
});

// ── 顶号与撤销：这两个是安全语义，单独钉住 ──────────────────────

test('router: 别的凭据不能顶掉在线的电脑', () => {
  // node_id 只有 32 位熵且公开可见（印在二维码与设置页上），所以"拿着同一个
  // node_id 的另一条凭据来抢"不是假想。原实现无条件替换，等于任何人都能把
  // 受害者的电脑永久挤下线。
  const r = new Router({ audit: { write: () => {} } });
  const real = fakeConn();
  assert.strictEqual(r.attachNode('pc-E', 'hash-real', real), 'attached');

  const attacker = fakeConn();
  assert.strictEqual(
    r.attachNode('pc-E', 'hash-attacker', attacker),
    'conflict',
    '不同凭据必须被拒',
  );
  assert.strictEqual(r.snapshot().nodes[0].node_connected, true);
  assert.strictEqual(r.slots.get('pc-E').nodeConn, real, '真连接必须原封不动');
});

test('router: 同一凭据重连可以顶掉自己的旧连接', () => {
  const r = new Router({ audit: { write: () => {} } });
  const first = fakeConn();
  r.attachNode('pc-F', 'hash-f', first);
  const second = fakeConn();
  assert.strictEqual(r.attachNode('pc-F', 'hash-f', second), 'replaced');
  assert.strictEqual(r.slots.get('pc-F').nodeConn, second, '出站重连必须能接管');
});

test('router: dropPhone 断连后空出的槽可以给新手机用', () => {
  const r = new Router({ audit: { write: () => {} } });
  const closed = [];
  const p1 = { ...fakeConn(), close: (code, why) => closed.push([code, why]) };
  r.attachNode('pc-G', 'hash-g', fakeConn());
  r.attachPhone('pc-G', 'hash-p1', p1);

  assert.strictEqual(r.dropPhone('hash-p1'), 1);
  assert.strictEqual(closed.length, 1, '撤销必须**立刻断连**，否则它还在收广播');
  assert.strictEqual(r.dropPhone('hash-p1'), 0, '重复撤销幂等');

  // 换手机的完整路径：先撤销 → 槽空出来 → 新手机才接得上。
  const p2 = fakeConn();
  assert.strictEqual(r.attachPhone('pc-G', 'hash-p2', p2), 'attached');
  r.nodeToPhones('pc-G', { tag: 'route', payload: sealedPayload('after') });
  assert.strictEqual(p2.sent.length, 1, '新手机接管后开始收这台电脑的帧');
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

test('端到端: 转发必须原样带过 session_id 与 auth', async () => {
  // 这条是回归守卫。relay 的转发白名单一度漏了 session_id / auth，
  // 后果不是握手失败而是「连上了但消息莫名消失」：
  //   · PC 按 session_id 拆包（relay_client.rs），缺了就整帧丢弃；
  //   · hermix 侧在重建的 `/v1/ws-stream/{sid}` 上复验签名，缺 auth 直接拒；
  //   · 更底层，密封帧的 AAD 把 session_id 算进去了，丢它连解密都失败。
  // 三层都在丢字段时静默失败，所以必须钉死。
  const db = tmpDb();
  const relay = createRelay({ dbPath: db });
  const phoneToken = relay.registry.register({ nodeId: 'fwd', role: 'phone' });
  const nodeToken = relay.registry.register({ nodeId: 'fwd', role: 'node' });

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
  await new Promise((r) => setTimeout(r, 60));

  const payload = sealedPayload('{"type":"pairing.request"}');
  const auth = { bearer: 'hmxd_test', signature: 'sig-blob' };
  phone.send(
    JSON.stringify({
      proto_version: PROTO_VERSION,
      tag: 'route',
      session_id: 'im:device:dev-1',
      auth,
      payload,
    }),
  );

  const got = await new Promise((resolve) => {
    node.once('message', (data) => resolve(JSON.parse(data.toString('utf8'))));
    setTimeout(() => resolve(null), 2000);
  });
  assert.ok(got, '电脑侧应收到帧');
  assert.strictEqual(got.session_id, 'im:device:dev-1', 'session_id 必须原样带过（拆包与 AAD 都依赖它）');
  assert.deepStrictEqual(got.auth, auth, 'auth 必须原样带过（签名在会话上复验）');
  assert.strictEqual(got.payload, payload);

  // 反向同样要带过去：PC → 手机靠 auth 里的签名授权。
  const backAuth = { bearer: 'hmxd_test', signature: 'sig-back' };
  node.send(
    JSON.stringify({
      proto_version: PROTO_VERSION,
      tag: 'route',
      session_id: 'im:device:dev-1',
      auth: backAuth,
      payload,
    }),
  );
  const gotBack = await new Promise((resolve) => {
    phone.once('message', (data) => resolve(JSON.parse(data.toString('utf8'))));
    setTimeout(() => resolve(null), 2000);
  });
  assert.ok(gotBack);
  assert.strictEqual(gotBack.session_id, 'im:device:dev-1');
  assert.deepStrictEqual(gotBack.auth, backAuth);

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

// ------------------------------------------------------- 安装页 / APK 下载

function get(url) {
  return new Promise((resolve, reject) => {
    http.get(url, (res) => {
      const chunks = [];
      res.on('data', (c) => chunks.push(c));
      res.on('end', () =>
        resolve({ status: res.statusCode, headers: res.headers, body: Buffer.concat(chunks).toString('utf8'), raw: res }),
      );
    }).on('error', reject);
  });
}

async function getJson(url) {
  const r = await get(url);
  return { ...r, body: JSON.parse(r.body) };
}

test('安装页: / 出页面，/api/app-info 如实报"未上传"', async () => {
  const relay = createRelay({ dbPath: tmpDb() });
  await new Promise((r) => relay.listen(0, r));
  const { port } = relay.server.address();
  try {
    const page = await get(`http://127.0.0.1:${port}/`);
    assert.strictEqual(page.status, 200);
    assert.ok(page.body.includes('Hermix M'), '页面应含产品名');
    assert.ok(page.body.includes('/download/hermix.apk'), '页面应给出下载入口');

    const info = await getJson(`http://127.0.0.1:${port}/api/app-info`);
    assert.strictEqual(info.status, 200);
    assert.strictEqual(info.body.available, false, '没有 APK 时必须说没有，不能给假按钮');

    const dl = await get(`http://127.0.0.1:${port}/download/hermix.apk`);
    assert.strictEqual(dl.status, 404, '没有 APK 时下载应 404');
  } finally {
    relay.close();
  }
});

test('安装页: 放了 APK 就能下，且带内容类型与指纹头', async () => {
  const fs2 = require('node:fs');
  const path2 = require('node:path');
  const publicDir = path2.join(__dirname, '..', 'public');
  const apk = path2.join(publicDir, 'hermix.apk');
  fs2.mkdirSync(publicDir, { recursive: true });
  const payload = Buffer.from('fake-apk-bytes');
  fs2.writeFileSync(apk, payload);
  const relay = createRelay({ dbPath: tmpDb() });
  await new Promise((r) => relay.listen(0, r));
  const { port } = relay.server.address();
  try {
    const info = await getJson(`http://127.0.0.1:${port}/api/app-info`);
    assert.strictEqual(info.body.available, true);
    assert.strictEqual(info.body.size, payload.length);
    const { createHash } = require('node:crypto');
    assert.strictEqual(info.body.sha256, createHash('sha256').update(payload).digest('hex'));

    const dl = await get(`http://127.0.0.1:${port}/download/hermix.apk`);
    assert.strictEqual(dl.status, 200);
    assert.strictEqual(dl.raw.headers['content-type'], 'application/vnd.android.package-archive');
    assert.ok(dl.raw.headers['x-checksum-sha256'], '应带校验头供客户端核对');
  } finally {
    relay.close();
    fs2.rmSync(apk, { force: true });
  }
});
