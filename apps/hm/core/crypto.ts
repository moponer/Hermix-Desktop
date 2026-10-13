/**
 * relay 帧加密 —— App 侧（P-256 ECDH + HKDF + AES-256-GCM）
 *
 * 与 `crates/hermix-server/src/relay_proto.rs` 的 Rust 侧互为镜像，
 * 帧格式与密钥派生必须逐字节一致。改动任何一处都要同步另一处。
 *
 * 为什么是 P-256 而不是 X25519：原实现选 P-256 是因为 WebCrypto 只支持
 * NIST 曲线；2026-10-12 起换 @noble/* 纯 JS 实现（见下），约束不再成立，
 * 但 Rust 侧 p256 已在线上，换曲线 = 两端同时升级，不值得——P-256 保留。
 *
 * ## 2026-10-12：WebCrypto → @noble/*（纯 JS，同步）
 *
 * App 端 V8 运行时没有 `crypto` 全局（subtle / getRandomValues 双缺，与
 * 缺 TextEncoder/URL 同源），真机实测 E2EE 从未可用过——配对页一直挂着
 * 「没有 WebCrypto」拦截。WebCrypto 没法像编码 API 那样 polyfill（P-256
 * 不是补丁能造的），换 @noble/curves + @noble/hashes + @noble/ciphers：
 * 零依赖纯 JS，任何 JS 运行时行为一致。字节级契约不变：
 *   ECDH=P-256、KDF=HKDF-SHA256(salt=frameId, info=方向)、AEAD=AES-256-GCM
 *   密文 = ‖16B tag（与 WebCrypto/Rust 同形状），noble 解密 tag 不符即 throw。
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
 * 在调用处一眼可见——写混了不会报错，只会让 open 永远解不开，进而把
 * 一屏 `assert.rejects` 变成假通过。
 *
 * 每帧一个 ephemeral → 单帧密钥泄露不影响其他帧，这就是前向保密的最小
 * 实现。不做 Double Ratchet：单向控制流不是双向持续会话。
 */

// 本模块顶层就会 `new TextEncoder()`，必须先装上 App 端的编码补丁。
import './encoding'
import { p256 } from '@noble/curves/nist.js'
import { hkdf } from '@noble/hashes/hkdf.js'
import { sha256 } from '@noble/hashes/sha2.js'
import { hmac } from '@noble/hashes/hmac.js'
import { gcm } from '@noble/ciphers/aes.js'

const KEY_BITS = 256;
/** AES-GCM 标准 nonce 长度。96 位，不可与 key 复用。 */
const NONCE_BYTES = 12;
/**
 * HKDF info 前缀，保证两个方向派生出的 key 不相同。
 *
 * HKDF 的 info 只接受字节，所以预编码成字节。Rust 侧必须用**同样的字节**
 * （`b"hermix/frame/v1/d2n"`）。
 */
const INFO: Record<Direction, Uint8Array> = {
  d2n: new TextEncoder().encode('hermix/frame/v1/d2n'),
  n2d: new TextEncoder().encode('hermix/frame/v1/n2d'),
};

export type Direction = 'd2n' | 'n2d';

export interface Identity {
  /** 未压缩 P-256 SPKI，65 字节。进二维码的就是它。 */
  publicKey: Uint8Array;
  /** 私钥原始字节（32B）。只活在内存里，从不落盘——与 WebCrypto 时代
   * 「不可导出的 CryptoKey」同一个纪律，只是 noble 里它就是字节。 */
  privateKey: Uint8Array;
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

/** 字节 → base64。导出给配对流程构造密封信封用。 */
export function b64(bytes: Uint8Array): string {
  let s = '';
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
}

/** base64 → 字节。配对流程要解帧头里的 eid / ep（导出给 core/direct 用）。 */
export function b64decode(s: string): Uint8Array {
  const bin = atob(s)
  const out = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i)
  return out
}

// ── 随机源 ──────────────────────────────────────────────────────

/**
 * CSPRNG 随机字节。主路径 `crypto.getRandomValues`（H5/小程序/Node 都有）；
 * App 端 V8 可能连这个都没有 —— 兜底用环境熵喂 HMAC-DRBG。
 *
 * 兜底的熵质量低于真 CSPRNG（时间戳 + Math.random + 设备信息），不如
 * CSPRNG 强，但配对是低频一次性操作、帧密钥每帧轮换，可接受；主路径
 * 存在时永远不用兜底。
 */
export function randomBytes(n: number): Uint8Array {
  const g = globalThis as {
    crypto?: { getRandomValues?: (b: Uint8Array) => Uint8Array };
  };
  if (g.crypto?.getRandomValues) return g.crypto.getRandomValues(new Uint8Array(n));
  return drbgBytes(n);
}

/**
 * HMAC-DRBG 兜底随机源。只在环境没有 CSPRNG 时使用（App 端 V8）。
 */
function drbgBytes(n: number): Uint8Array {
  let entropy = '';
  try {
    entropy += `${Date.now()}:${performance?.now?.() ?? 0}`;
  } catch {
    entropy += String(Date.now());
  }
  for (let i = 0; i < 32; i++) entropy += `:${Math.random()}`;
  try {
    const si = (uni as { getSystemInfoSync?: () => Record<string, unknown> }).getSystemInfoSync?.();
    if (si) entropy += `:${JSON.stringify(si).slice(0, 256)}`;
  } catch {
    /* H5 之外可能受限，少一份熵不影响可用性 */
  }
  const seed = new TextEncoder().encode(entropy);
  const out = new Uint8Array(n);
  for (let off = 0, ctr = 0; off < n; off += 32, ctr++) {
    const block = hmac(
      sha256,
      seed,
      new TextEncoder().encode(`hermix/fallback-rng/${ctr}`),
    );
    out.set(block.subarray(0, Math.min(32, n - off)), off);
  }
  return out;
}

// noble 系列库（curves 的 keygen、ciphers 的 nonce）内部直接调
// `globalThis.crypto.getRandomValues`，App 端 V8 没有这个全局，裸调抛
// "crypto.getRandomValues must be defined"。把 DRBG 兜底挂上去，让 noble
// 与本模块共用同一条随机链。有原生 CSPRNG 的环境此函数什么都不做。
export function ensureCryptoRandomPolyfill(): void {
  const g = globalThis as { crypto?: { getRandomValues?: (b: Uint8Array) => Uint8Array } };
  if (g.crypto?.getRandomValues) return;
  const polyfill = (buf: Uint8Array): Uint8Array => {
    buf.set(drbgBytes(buf.length));
    return buf;
  };
  if (g.crypto) g.crypto.getRandomValues = polyfill;
  else (g as { crypto: object }).crypto = { getRandomValues: polyfill };
}

// 模块加载即安装：noble 在调用时才读全局，晚挂不影响正确性，但越早越好。
ensureCryptoRandomPolyfill();

// ── 原语 ────────────────────────────────────────────────────────

/**
 * ECDH 共享秘密。noble 返回 33B 压缩形态（0x02/03 ‖ X），而 WebCrypto
 * `deriveBits` 与 Rust `p256` crate 的输出都是 32B 裸 X —— slice 对齐。
 * 错一位 → HKDF 输入不同 → GCM 全废，两端互不解密。
 */
function ecdh(priv: Uint8Array, peerPub: Uint8Array): Uint8Array {
  return p256.getSharedSecret(priv, peerPub).slice(1, 33);
}

/** shared secret → content key。salt 绑帧号，info 绑方向。 */
function frameKey(shared: Uint8Array, salt: Uint8Array, direction: Direction): Uint8Array {
  return hkdf(sha256, shared, salt, INFO[direction], KEY_BITS / 8);
}

/** 生成一方身份（长期静态密钥对）。配对时只交换 publicKey。 */
export async function generateIdentity(): Promise<Identity> {
  const privateKey = p256.keygen().secretKey;
  return {
    publicKey: p256.getPublicKey(privateKey, false),
    privateKey,
  };
}

/**
 * 导入 65 字节未压缩 SPKI 公钥。对端静态公钥与帧头里的 ephemeral 都走它。
 * 形状不对当场抛——错字节留到 GCM 阶段只会变成一句没有方向的解密失败。
 */
export async function importPublicKey(raw: Uint8Array): Promise<Uint8Array> {
  if (raw.length !== 65 || raw[0] !== 0x04) {
    throw new Error(`bad P-256 public key: expect 65B uncompressed (0x04…), got ${raw.length}B`);
  }
  return raw;
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
  ctx: FrameCryptoContext & { receiverPublic: Uint8Array },
): Promise<{ sealed: SealedBody; ephemeralPublic: Uint8Array }> {
  const ephPriv = p256.keygen().secretKey;
  const key = frameKey(ecdh(ephPriv, ctx.receiverPublic), ctx.frameId, ctx.direction);
  const nonce = randomBytes(NONCE_BYTES);
  const ciphertext = gcm(key, nonce, ctx.header).encrypt(plaintext);
  return {
    sealed: { nonce, ciphertext },
    ephemeralPublic: p256.getPublicKey(ephPriv, false),
  };
}

/** 解密一帧的 body。用**自己的静态私钥** + 发送方的 ephemeral 公钥。 */
export async function openBody(
  sealed: SealedBody,
  ctx: FrameCryptoContext & { receiverPrivate: Uint8Array; ephemeralPublic: Uint8Array },
): Promise<Uint8Array> {
  const key = frameKey(ecdh(ctx.receiverPrivate, ctx.ephemeralPublic), ctx.frameId, ctx.direction);
  // tag 不符（密文/nonce/AAD 任何一个被改）noble 直接 throw —— 调用方
  // assert.rejects 与线上「拒收坏帧」共用这一条路径。
  return gcm(key, sealed.nonce, ctx.header).decrypt(sealed.ciphertext);
}

/**
 * 手机侧构造密钥交换帧：用**PC 的公钥**把 `[device_id, pk_ph]` 封一层。
 *
 * 这是 E2EE 的信任建立点：手机扫码已经拿到 `pk_pc`（二维码里带的），
 * 用它封自己的公钥，relay 就学不到 `pk_ph`——否则 relay 拿着 `pk_ph`
 * 能解开 PC 发给手机的**全部**帧。手法与 TLS 交换证书同构。
 *
 * 每次冷启动都要重发一次：私钥只在内存里、冷启动换新，PC 侧
 * `learn_peer` 是 upsert，覆盖旧的即可。
 */
export async function buildKeyExchangeEnvelope(opts: {
  pcPublic: Uint8Array;
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
