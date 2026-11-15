/**
 * 一次性跨端向量生成器。
 *
 *   cd apps/hm && npx tsx dump-vector.ts
 *
 * 输出贴进 `crates/hermix-server/src/relay_crypto.rs` 的 conformance 测试。
 * 保留在仓库里是为了下次改协议时能重新出一组基线。
 *
 * 为什么需要它：WebCrypto **不能导入 ECDH 私钥**，所以「App 用固定向量解密」
 * 这条路根本不存在。可行的只有让 App 暴露两个公开量——
 *   ① 它算出的 ECDH shared secret（hex）
 *   ② 它的 ephemeral 公钥
 * Rust 拿固定私钥与 ② 比对 ①，即可证明两端 ECDH 的 x 坐标提取一致；
 * 同一组 nonce/密文再走一遍 open_body，就证明了 HKDF + AES-GCM + AAD 一致。
 */
import { webcrypto } from 'node:crypto'

const c = webcrypto as Crypto
const b64 = (b: Uint8Array) => Buffer.from(b).toString('base64')
const unb64 = (s: string) => new Uint8Array(Buffer.from(s, 'base64'))

// Rust 侧 `Identity::from_seed(&[2u8; 32]).public_key()` 的输出。
const PC_PUB = 'BFUPRxAD89+Xw99QaseX9nIfsaH7e49vg9IkSYplyI4kE2CT1wEuUJpzcVy9CwCjzA/0tcAbP/oZarH7MnA2uOY='

const FRAME_ID = new TextEncoder().encode('frame-0001')
const HEADER = new TextEncoder().encode(JSON.stringify({ v: 2, type: 'route' }))
const PLAIN = new TextEncoder().encode('把 C 盘清一下')
const INFO = new TextEncoder().encode('hermix/frame/v1/d2n')

async function main() {
  const receiverPublic = await c.subtle.importKey(
    'raw', unb64(PC_PUB), { name: 'ECDH', namedCurve: 'P-256' }, true, [])

  const eph = (await c.subtle.generateKey(
    { name: 'ECDH', namedCurve: 'P-256' }, true, ['deriveBits', 'deriveKey'])) as CryptoKeyPair
  const ephPub = new Uint8Array(await c.subtle.exportKey('raw', eph.publicKey))

  // ① 共享密钥 —— Rust 侧要用固定私钥与 ephPub 复算同一个值
  const shared = await c.subtle.deriveBits({ name: 'ECDH', public: receiverPublic }, eph.privateKey, 256)

  // ② 用同一个 shared 走 HKDF → AES-GCM，密封
  const material = await c.subtle.importKey('raw', shared, 'HKDF', false, ['deriveKey'])
  const key = await c.subtle.deriveKey(
    { name: 'HKDF', hash: 'SHA-256', salt: FRAME_ID, info: INFO },
    material, { name: 'AES-GCM', length: 256 }, false, ['encrypt'])
  const nonce = c.getRandomValues(new Uint8Array(12))
  const ct = new Uint8Array(await c.subtle.encrypt(
    { name: 'AES-GCM', iv: nonce, additionalData: HEADER }, key, PLAIN))

  console.log(`APP_EPH_PUB=${b64(ephPub)}`)
  console.log(`APP_SHARED=${b64(new Uint8Array(shared))}`)
  console.log(`APP_NONCE=${b64(nonce)}`)
  console.log(`APP_CT=${b64(ct)}`)
  console.log(`PC_PUB=${PC_PUB}`)
}

main().catch((e) => { console.error(e); process.exit(1) })
