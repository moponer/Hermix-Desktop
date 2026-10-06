/**
 * relay 帧加密 —— App 侧（P-256 ECDH + AES-256-GCM）
 *
 * 与 `crates/hermix-server/src/relay_proto.rs` 的 Rust 侧互为镜像，
 * 帧格式与密钥派生必须逐字节一致。改动任何一处都要同步另一处。
 *
 * 为什么是 P-256 而不是 X25519：WebCrypto 只支持 NIST 曲线，没有
 * Curve25519。选 P-256 意味着本文件**零新增依赖**——`subtle.deriveBits`
 * 与 `subtle.deriveKey` 都是浏览器原生。Rust 侧补一个 `p256` crate 即可。
 * 同理选 AES-GCM：WebCrypto 没有 ChaCha20。
 *
 * ## ECDH 的固定侧永远是「接收方」
 *
 * 发送方 S（手机）与接收方 R（PC）各持一个长期静态密钥对，配对时交换公钥。
 * 加密与解密用的是**同一个身份 R** 的两侧：
 *
 *   seal（S 做）：`ECDH(ephemeral_private, R.public)`  → 下发 `ephemeral_public`
 *   open（R 做）：`ECDH(R.private, ephemeral_public)`
 *
 * 所以 `sealBody` 的 `receiverPublic` 与 `openBody` 的 `receiverPrivate`
 * **必须来自同一个身份**。参数故意用同名 `receiver*` 就是为了让这一点
 * 在调用处一眼可见——写混了不会报错，只会让 open 永远抛 "operation-specific
 * reason"，进而把一屏 `assert.rejects` 变成假通过。
 *
 * 每帧一个 ephemeral → 单帧密钥泄露不影响其他帧，这就是前向保密的最小
 * 实现。不做 Double Ratchet：单向控制流不是双向持续会话。
 *
 * 运行环境依赖全局 `crypto.subtle`。部分小程序运行时没有 SubtleCrypto，
 * 落地前需先在真机确认（见对应决策笔记的 Consequences）。
 */

const KEY_BITS = 256;
/** AES-GCM 标准 nonce 长度。96 位，不可与 key 复用。 */
const NONCE_BYTES = 12;
/**
 * HKDF info 前缀，保证两个方向派生出的 key 不相同。
 *
 * WebCrypto 的 `HkdfParams.info` 只接受 BufferSource，不接受 string，所以
 * 预编码成字节。Rust 侧必须用**同样的字节**（`b"hermix/frame/v1/d2n"`）。
 */
const INFO: Record<Direction, Uint8Array> = {
  d2n: new TextEncoder().encode('hermix/frame/v1/d2n'),
  n2d: new TextEncoder().encode('hermix/frame/v1/n2d'),
};

export type Direction = 'd2n' | 'n2d';

export interface Identity {
  /** 未压缩 P-256 SPKI，65 字节。进二维码的就是它。 */
  publicKey: Uint8Array;
  privateKey: CryptoKey;
}

export interface SealedBody {
  nonce: Uint8Array;
  ciphertext: Uint8Array;
}

/** 密钥交换载荷的 `type`。必须与 `relay_crypto.rs::KEY_EXCHANGE_TYPE` 同字。 */
export const KEY_EXCHANGE_TYPE = 'key.exchange';

/** 配对后发给 PC 的密钥交换帧的**明文**内容。机密性来自外层 seal。 */
export interface KeyExchangePayload {
  type: typeof KEY_EXCHANGE_TYPE;
  /** 本机静态公钥，base64。 */
  pk: string;
  device_id: string;
}

/** 密封信封 —— 线路上 `payload` 的实际形状。 */
export interface SealedEnvelope {
  ct: string;
  nonce: string;
  ep: string;
  eid: string;
}

function b64(bytes: Uint8Array): string {
  let s = '';
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
}

function subtle(): SubtleCrypto {
  const c = globalThis.crypto;
  if (!c?.subtle) {
    throw new Error('WebCrypto SubtleCrypto unavailable — this runtime cannot do relay E2EE');
  }
  return c.subtle;
}

function randomBytes(n: number): Uint8Array {
  return globalThis.crypto.getRandomValues(new Uint8Array(n));
}

/** 生成一方身份（长期静态密钥对）。配对时只交换 publicKey。 */
export async function generateIdentity(): Promise<Identity> {
  const pair = (await subtle().generateKey(
    { name: 'ECDH', namedCurve: 'P-256' },
    false, // 静态私钥不可导出
    ['deriveBits', 'deriveKey'],
  )) as CryptoKeyPair;
  return {
    publicKey: new Uint8Array(await subtle().exportKey('raw', pair.publicKey)),
    privateKey: pair.privateKey,
  };
}

/** 导入 65 字节未压缩 SPKI 公钥。对端静态公钥与帧头里的 ephemeral 都走它。 */
export async function importPublicKey(raw: Uint8Array): Promise<CryptoKey> {
  return subtle().importKey('raw', raw as BufferSource, { name: 'ECDH', namedCurve: 'P-256' }, true, []);
}

/** shared secret → content key。salt 绑帧号，info 绑方向。 */
async function contentKey(
  bits: ArrayBuffer,
  salt: Uint8Array,
  direction: Direction,
): Promise<CryptoKey> {
  const material = await subtle().importKey('raw', bits, 'HKDF', false, ['deriveKey']);
  return subtle().deriveKey(
    { name: 'HKDF', hash: 'SHA-256', salt: salt as BufferSource, info: INFO[direction] as BufferSource },    material,
    { name: 'AES-GCM', length: KEY_BITS },
    false,
    ['encrypt', 'decrypt'],
  );
}

export interface FrameCryptoContext {
  /** 帧号 / 消息 id。进 HKDF salt，两端必须一致。 */
  frameId: Uint8Array;
  /** 帧头序列化。作为 AES-GCM 的 AAD——relay 看得见 header，改它即解密失败。 */
  header: Uint8Array;
  direction: Direction;
}

/** 加密一帧的 body。产生新的 ephemeral 密钥对，其公钥随帧头明文下发。 */
export async function sealBody(
  plaintext: Uint8Array,
  ctx: FrameCryptoContext & { receiverPublic: CryptoKey },
): Promise<{ sealed: SealedBody; ephemeralPublic: Uint8Array }> {
  const eph = (await subtle().generateKey(
    { name: 'ECDH', namedCurve: 'P-256' },
    true,
    ['deriveBits', 'deriveKey'],
  )) as CryptoKeyPair;
  const bits = await subtle().deriveBits({ name: 'ECDH', public: ctx.receiverPublic }, eph.privateKey, KEY_BITS);
  const key = await contentKey(bits, ctx.frameId, ctx.direction);
  const nonce = randomBytes(NONCE_BYTES);
  const ciphertext = new Uint8Array(
    await subtle().encrypt(
      { name: 'AES-GCM', iv: nonce as BufferSource, additionalData: ctx.header as BufferSource },
      key,
      plaintext as BufferSource,
    ),
  );
  return {
    sealed: { nonce, ciphertext },
    ephemeralPublic: new Uint8Array(await subtle().exportKey('raw', eph.publicKey)),
  };
}

/** 解密一帧的 body。用**自己的静态私钥** + 发送方的 ephemeral 公钥。 */
export async function openBody(
  sealed: SealedBody,
  ctx: FrameCryptoContext & { receiverPrivate: CryptoKey; ephemeralPublic: CryptoKey },
): Promise<Uint8Array> {
  const bits = await subtle().deriveBits(
    { name: 'ECDH', public: ctx.ephemeralPublic },
    ctx.receiverPrivate,
    KEY_BITS,
  );
  const key = await contentKey(bits, ctx.frameId, ctx.direction);
  return new Uint8Array(
    await subtle().decrypt(
      { name: 'AES-GCM', iv: sealed.nonce as BufferSource, additionalData: ctx.header as BufferSource },
      key,
      sealed.ciphertext as BufferSource,
    ),
  );
}

/**
 * 手机侧构造密钥交换帧：用**PC 的公钥**把 `[device_id, pk_ph]` 封一层。
 *
 * 这是 E2EE 的信任建立点：手机扫码已经拿到 `pk_pc`（二维码里带的），
 * 用它封自己的公钥，relay 就学不到 `pk_ph`——否则 relay 拿着 `pk_ph`
 * 能解开 PC 发给手机的**全部**帧。手法与 TLS 交换证书同构。
 *
 * 每次冷启动都要重发一次：WebCrypto 私钥不可导出，重启后是新的
 * `pk_ph`，PC 侧 `learn_peer` 是 upsert，覆盖旧的即可。
 */
export async function buildKeyExchangeEnvelope(opts: {
  pcPublic: CryptoKey;
  deviceId: string;
  ownPublic: Uint8Array;
  frameId: Uint8Array;
  header: Uint8Array;
}): Promise<{ payload: SealedEnvelope; ephemeralPublic: string }> {
  const plain = new TextEncoder().encode(
    JSON.stringify({
      type: KEY_EXCHANGE_TYPE,
      pk: b64(opts.ownPublic),
      device_id: opts.deviceId,
    } satisfies KeyExchangePayload),
  );
  // 手机 → PC，方向 d2n。
  const { sealed, ephemeralPublic } = await sealBody(plain, {
    receiverPublic: opts.pcPublic,
    frameId: opts.frameId,
    header: opts.header,
    direction: 'd2n',
  });
  return {
    payload: {
      ct: b64(sealed.ciphertext),
      nonce: b64(sealed.nonce),
      ep: b64(ephemeralPublic),
      eid: b64(opts.frameId),
    },
    ephemeralPublic: b64(ephemeralPublic),
  };
}