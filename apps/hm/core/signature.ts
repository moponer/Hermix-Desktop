/**
 * 请求签名 —— 必须与 `crates/hermix-server/src/request_signature.rs` 逐字一致。
 *
 * 线格式（Rust 侧是权威定义）：
 * ```text
 * X-Hermix-Signature: t=<unix_sec>,n=<hex nonce>,s=<hex hmac>
 * sig = HMAC_SHA256(device_token, METHOD + "\n" + path + "\n" + t + "\n" + nonce)
 * ```
 *
 * 跨语言契约向量由 Rust 的 `cross_language_signature_vector` 测试钉死：
 *   token=`hmxd_test_token` method=`GET` path=`/v1/ws-stream/s1`
 *   t=`1700000000` nonce=`abc123` → 见 CONFORMANCE_VECTOR
 *
 * **为什么值得这么较真**：签名的作用是"token 泄漏 ≠ 无限使用"。算法在
 * 两端各错一次时，round-trip 测试照样绿（自己算的能被自己验）；唯一能钉住
 * 线格式的就是这个固定向量。所以 `CONFORMANCE_VECTOR` 改动时，Rust 侧那个
 * 测试必须一起改。
 *
 * ## 2026-10-12：WebCrypto → @noble/hashes
 *
 * App 端 V8 没有 `crypto` 全局（subtle/getRandomValues 双缺），原 WebCrypto
 * HMAC 路径在真机上必炸——与 crypto.ts 的 E2EE 是同一个坑，一起换 noble。
 * `hmac(sha256,…)` 已在本仓库测试里与 Rust 固定向量逐字比对通过。
 * 函数**保持 async**：调用方（relayClient/http）一律 await，签名不动调用面。
 */

// App 端没有全局 TextEncoder，装上补丁再用。
import './encoding'
import { hmac } from '@noble/hashes/hmac.js'
import { sha256 } from '@noble/hashes/sha2.js'
import { randomBytes } from './crypto'

/**
 * 跨语言契约向量。**从 Rust 抄来的字面量**，权威定义在
 * `crates/hermix-server/src/request_signature.rs::cross_language_signature_vector`。
 *
 * 本仓库里只此一处（测试从这儿 import，不再各抄一份）——两个地方各写一个
 * 常量，迟早有一份忘了同步，而那时候没人会知道该信哪个。
 * 真正的跨语言钉扎靠那个 Rust 测试：它把本文件算出的值与 Rust 算出的值
 * 放在同一个常量上比对。
 */
export const CONFORMANCE_VECTOR = {
  token: 'hmxd_test_token',
  method: 'GET',
  path: '/v1/ws-stream/s1',
  t: 1700000000,
  nonce: 'abc123',
  // 由 Rust `cross_language_signature_vector` 给出的权威值。
  // 这一条同时验证了两侧：Rust 手写 HMAC（sha2）与 TS 的 noble HMAC
  // 在同一固定向量上算出同一个值 —— 两个独立实现互为佐证。
  sig: '521925f25e9194ded7ecc29a57879549d48ee8ceb752b390bf6a8f4e5b9a734f',
} as const

function toHex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('')
}

/**
 * 算签名。
 *
 * @param token 设备 token 明文（`hmxd_*`）
 * @param method HTTP 方法，必须与实际发出的请求一致
 * @param path 请求路径（含 query），**必须与实际发出的完全一致** ——
 *   relay 只做字节透传，不重写；它一旦重写，签名就废了。
 * @param t unix 秒
 * @param nonce 一次性随机串（**每次请求都要新的**：服务端按 nonce 去重，
 *   重放同一个 nonce 会被判 BadSignature）
 */
export async function computeSignature(
  token: string,
  method: string,
  path: string,
  t: number,
  nonce: string,
): Promise<string> {
  // 密钥是 device token 的**原始 UTF-8 字节**，不是它的 hex —— 与 Rust 侧
  // `compute_signature` 的 key 保持一致，不要"顺手"先 hex 解码。
  const key = new TextEncoder().encode(token)
  const message = `${method}\n${path}\n${t}\n${nonce}`
  return toHex(hmac(sha256, key, new TextEncoder().encode(message)))
}

/** 16 字节随机 nonce 的 hex。 */
export function newNonce(): string {
  return toHex(randomBytes(16))
}

/**
 * 造出可直接放进请求头的 `X-Hermix-Signature` 值。
 *
 * `path` 传**实际要请求的 path**，不要传"逻辑路径"：签名是覆盖在字节上的。
 */
export async function signatureHeader(
  token: string,
  method: string,
  path: string,
): Promise<string> {
  const t = Math.floor(Date.now() / 1000)
  const nonce = newNonce()
  const s = await computeSignature(token, method, path, t, nonce)
  return `t=${t},n=${nonce},s=${s}`
}

/** 自检：与 Rust 的固定向量比对。开发期跑一次，省掉"两端一起错还都绿"。 */
export async function verifyConformance(): Promise<boolean> {
  const got = await computeSignature(
    CONFORMANCE_VECTOR.token,
    CONFORMANCE_VECTOR.method,
    CONFORMANCE_VECTOR.path,
    CONFORMANCE_VECTOR.t,
    CONFORMANCE_VECTOR.nonce,
  )
  return got === CONFORMANCE_VECTOR.sig
}
