/**
 * relay 帧信封 —— 与 `crates/hermix-server/src/relay_proto.rs` 对齐。
 *
 * **凭据为什么在帧里**：App 不直接向 hermix 发 HTTP 请求，它发 WS 帧
 * （`手机 → relay → hermix`），所以 `Authorization` / `X-Hermix-Signature`
 * 这两个请求头没有落脚的地方。解法是放进帧的 `auth` 字段，由 **hermix 在 PC
 * 侧**验；relay 只透传、不解释（与它"不校验 token 真伪""字节级透明"两条
 * 硬约束一致）。
 *
 * `sessionId` 不是可选装饰：PC 用它重建逻辑路径 `/v1/ws-stream/{sid}` 再验
 * 签名，于是**签名与会话绑定**——A 会话的签名无法拿去冒用 B。改这个字段的
 * 语义等于改鉴权模型。
 *
 * ⚠️ 这里的 protoVersion 必须与 relay_proto.rs 的 `PROTO_VERSION` 一致。
 */

/** 与 `relay_proto::PROTO_VERSION` 必须相同。 */
export const PROTO_VERSION = 2

export type FrameTag = 'hello' | 'route' | 'ack' | 'close'

/** 帧里携带的 hermix 侧凭据（relay 不解释，hermix 验）。 */
export interface FrameAuth {
  /** `hmxd_*` 设备 token */
  bearer: string
  /** `X-Hermix-Signature` 的完整值 */
  signature: string
}

export interface Envelope {
  protoVersion: number
  tag: FrameTag
  tokenHash?: string
  nodeId?: string
  sessionId?: string
  auth?: FrameAuth
  /** 密封信封的 JSON（见 `SealedFrame`）。v2 起不再是明文内层帧。 */
  payload?: string
}

/**
 * 密封信封 —— `payload` 里的实际内容，四个字段全部对 relay 明文可见。
 *
 * 字段名必须与 `crates/hermix-server/src/relay_proto.rs::SealedFrame`、
 * `apps/relay/src/proto.js::isSealed` 三处同名同拼写，否则 relay 认不出
 * 受保护的帧，会把合法密文当明文拒掉。
 */
export interface SealedFrame {
  /** base64：内层帧 AES-256-GCM 密文（含 tag）。 */
  ct: string
  /** base64：12 字节 nonce。 */
  nonce: string
  /** base64：65 字节未压缩 P-256 ephemeral 公钥。 */
  ep: string
  /** base64：帧号，进 HKDF salt，防跨帧重放。 */
  eid: string
}

/** snake_case → camelCase：Rust 侧字段是 snake_case，JS 侧按 camelCase 用。 */
function toEnvelope(wire: any): Envelope {
  return {
    protoVersion: wire.proto_version,
    tag: wire.tag,
    tokenHash: wire.token_hash,
    nodeId: wire.node_id,
    sessionId: wire.session_id,
    auth: wire.auth,
    payload: wire.payload,
  }
}

function fromEnvelope(e: Envelope): Record<string, unknown> {
  // 可选字段缺省就不出现在 JSON 里——少一个字段就少一处两端可能不一致的地方。
  const out: Record<string, unknown> = { proto_version: e.protoVersion, tag: e.tag }
  if (e.tokenHash !== undefined) out.token_hash = e.tokenHash
  if (e.nodeId !== undefined) out.node_id = e.nodeId
  if (e.sessionId !== undefined) out.session_id = e.sessionId
  if (e.auth !== undefined) out.auth = e.auth
  if (e.payload !== undefined) out.payload = e.payload
  return out
}

/** 解析一帧。脏数据返回 null（不断连接）；版本漂移单独标出来。 */
export function parseEnvelope(raw: string): { ok: true; envelope: Envelope } | { ok: false; reason: string; versionMismatch: boolean } {
  let wire: any
  try {
    wire = JSON.parse(raw)
  } catch {
    return { ok: false, reason: 'not json', versionMismatch: false }
  }
  if (typeof wire !== 'object' || wire === null) {
    return { ok: false, reason: 'not an object', versionMismatch: false }
  }
  if (typeof wire.proto_version !== 'number') {
    return { ok: false, reason: 'missing proto_version', versionMismatch: false }
  }
  if (wire.proto_version !== PROTO_VERSION) {
    // 单独标出：版本漂移要在握手期就报，而不是等消息「莫名消失」。
    return {
      ok: false,
      reason: `proto_version ${wire.proto_version} != ${PROTO_VERSION}`,
      versionMismatch: true,
    }
  }
  if (!['hello', 'route', 'ack', 'close'].includes(wire.tag)) {
    return { ok: false, reason: `unknown tag ${wire.tag}`, versionMismatch: false }
  }
  // v2：ROUTE 的 payload 必须是密封信封。明文帧一律拒——静默放行等于
  // E2EE 被无声关闭，且失败形态会退化成「消息莫名消失」。
  // 判定口径必须与 `apps/relay/src/proto.js::isSealed` 一致。
  if (wire.tag === 'route' && !isSealed(wire.payload)) {
    return { ok: false, reason: 'plaintext route frame rejected (E2EE required)', versionMismatch: false }
  }
  return { ok: true, envelope: toEnvelope(wire) }
}

/** payload 是否是密封信封（四个字段齐全）。与 relay 的 `isSealed` 同口径。 */
export function isSealed(payload: unknown): boolean {
  if (typeof payload !== 'string' || payload.length === 0) return false
  let inner: any
  try {
    inner = JSON.parse(payload)
  } catch {
    return false
  }
  if (typeof inner !== 'object' || inner === null) return false
  return (
    typeof inner.ct === 'string' &&
    typeof inner.nonce === 'string' &&
    typeof inner.ep === 'string' &&
    typeof inner.eid === 'string'
  )
}

export function encodeEnvelope(e: Envelope): string {
  return JSON.stringify(fromEnvelope(e))
}

/** 手机的登记帧：把 relay 凭据的哈希与目标电脑报给 relay。 */
export function helloFrame(tokenHash: string, nodeId: string): Envelope {
  return { protoVersion: PROTO_VERSION, tag: 'hello', tokenHash, nodeId }
}
