'use strict';

/**
 * 线路格式的 relay 侧镜像 —— **只放 tag 与版本号，帧定义不在这里**。
 *
 * 真值在 `crates/hermix-server/src/relay_proto.rs`（Rust 侧）。本文件存在的
 * 唯一理由是让 relay 能识别外层信封的类别；内层是什么 relay 一概不看。
 *
 * ⚠️ 改任何 tag 或 PROTO_VERSION，必须同步改 Rust 那一份。两处不一致的表现
 * 是「能连上但消息莫名消失」，不是握手失败——所以 handshake 阶段会比对版本。
 *
 * 为什么 relay 连内层帧都不解析：签名覆盖 `METHOD\npath`，relay 一旦重组请求
 * （改路径、改方法、剥 header）手机端的 `X-Hermix-Signature` 就全废。而
 * hermix 侧的授权判定本来就不经过 relay（见 README「信任边界」）。
 */

/** 与 `relay_proto.rs` 的 `PROTO_VERSION` 必须相同。 */
const PROTO_VERSION = 2;

/** 与 `relay_proto.rs` 的 `FrameTag` 必须同名同拼写（小写）。 */
const TAG = Object.freeze({
  HELLO: 'hello',
  ROUTE: 'route',
  ACK: 'ack',
  CLOSE: 'close',
});

const TAGS = new Set(Object.values(TAG));

/**
 * 解析一帧外层信封。
 *
 * 解析失败返回 null 而不抛：脏数据不该把连接炸掉，调用方跳过即可。
 * 但 `protoVersion` 不符要单独报出来，让握手期就能发现版本漂移。
 *
 * @param {string|Buffer} raw
 * @returns {{ok: true, envelope: object} | {ok: false, reason: string, versionMismatch: boolean}}
 */
function parseEnvelope(raw) {
  let text;
  try {
    text = typeof raw === 'string' ? raw : raw.toString('utf8');
  } catch {
    return { ok: false, reason: 'not utf-8', versionMismatch: false };
  }
  let envelope;
  try {
    envelope = JSON.parse(text);
  } catch {
    return { ok: false, reason: 'not json', versionMismatch: false };
  }
  if (typeof envelope !== 'object' || envelope === null) {
    return { ok: false, reason: 'not an object', versionMismatch: false };
  }
  if (typeof envelope.proto_version !== 'number') {
    return { ok: false, reason: 'missing proto_version', versionMismatch: false };
  }
  if (envelope.proto_version !== PROTO_VERSION) {
    // 单独标出来：版本漂移要在握手期就报，而不是等消息莫名其妙消失。
    return {
      ok: false,
      reason: `proto_version ${envelope.proto_version} != ${PROTO_VERSION}`,
      versionMismatch: true,
    };
  }
  if (!TAGS.has(envelope.tag)) {
    return { ok: false, reason: `unknown tag ${envelope.tag}`, versionMismatch: false };
  }
  // v2：ROUTE 的 payload 必须是密封信封。明文帧一律拒——**不能静默放行**，
  // 静默降级等于 E2EE 被无声关闭，而且失败形态会退化成「消息莫名消失」。
  if (envelope.tag === TAG.ROUTE && !isSealed(envelope.payload)) {
    return { ok: false, reason: 'plaintext route frame rejected (E2EE required)', versionMismatch: false };
  }
  return { ok: true, envelope };
}

/**
 * payload 是否是密封信封（`ct` / `nonce` / `ep` / `eid` 四个字段齐全）。
 *
 * relay 读不懂密文也**不需要**读懂——它只判定"这一帧是不是受保护的"。
 * 字段不全一律当明文拒，不做部分接受。
 *
 * ⚠️ 字段名必须与 `crates/hermix-server/src/relay_proto.rs::SealedFrame`
 *    和 `apps/hm/src/core/envelope.ts` 三处同名同拼写。
 */
function isSealed(payload) {
  if (typeof payload !== 'string' || payload.length === 0) return false;
  let inner;
  try {
    inner = JSON.parse(payload);
  } catch {
    return false;
  }
  if (typeof inner !== 'object' || inner === null) return false;
  return (
    typeof inner.ct === 'string' &&
    typeof inner.nonce === 'string' &&
    typeof inner.ep === 'string' &&
    typeof inner.eid === 'string'
  );
}

/**
 * 序列化一个 relay 自己发出去的信封。
 *
 * relay 只主动发 ACK 与 CLOSE 两类；ROUTE 帧 relay **不生成也不改写**，
 * 只是原样把 payload 交给对面（见 router.js）。
 */
function buildEnvelope(tag, fields = {}) {
  const envelope = { proto_version: PROTO_VERSION, tag, ...fields };
  // 与 Rust 侧一致：可选字段缺省时不出现在 JSON 里，少一个字段就少一处
  // 两端可能漂移的地方。
  for (const key of Object.keys(envelope)) {
    if (key === 'proto_version' || key === 'tag') continue;
    if (envelope[key] === undefined || envelope[key] === null) {
      delete envelope[key];
    }
  }
  return JSON.stringify(envelope);
}

module.exports = { PROTO_VERSION, TAG, TAGS, parseEnvelope, buildEnvelope, isSealed };
