/**
 * relay E2EE 帧加密的往返与对抗测试。
 *
 * ## 配对不变量
 *
 * 发送方 phone 与接收方 pc 各持一个长期静态密钥对。加密与解密用的是
 * **同一个身份（接收方 pc）** 的两侧：
 *   seal: ECDH(ephemeral_private, pc.public)  → 下发 ephemeral_public
 *   open: ECDH(pc.private, ephemeral_public)
 * `receiverPublic` 与 `receiverPrivate` 必须同源。写混了不会报明确的错，
 * 只会让 open 永远抛 "operation-specific reason"——于一屏 `assert.rejects`
 * 全部假通过。所以下面第一条是**基线**：它坏了，其余断言立刻失去意义。
 *
 * 另：`phone` / `pc` 而不是 `node` / `device`——`node` 会和 Node 全局撞名。
 */
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { generateIdentity, importPublicKey, sealBody, openBody } from '../core/crypto'

const enc = new TextEncoder()
const dec = new TextDecoder()

/** 手机 → PC。固定方向，让 seal / open 的身份不可能搞混。 */
async function phoneToPc() {
  const phone = await generateIdentity()
  const pc = await generateIdentity()
  return { phone, pc, pcPub: await importPublicKey(pc.publicKey) }
}

const HEADER = enc.encode(JSON.stringify({ v: 2, type: 'route' }))

test('基线：手机 → PC 密封后 PC 能解开（下面所有 rejects 用例都建立在它之上）', async () => {
  const { phone, pc, pcPub } = await phoneToPc()
  const frameId = enc.encode('frame-0001')
  const { sealed, ephemeralPublic } = await sealBody(enc.encode('把 C 盘清一下'), {
    receiverPublic: pcPub, frameId, header: HEADER, direction: 'd2n',
  })
  const plain = await openBody(sealed, {
    receiverPrivate: pc.privateKey,
    ephemeralPublic: await importPublicKey(ephemeralPublic),
    frameId, header: HEADER, direction: 'd2n',
  })
  assert.equal(dec.decode(plain), '把 C 盘清一下')
})

test('PC → 手机：反方向同样通', async () => {
  const phone = await generateIdentity()
  const pc = await generateIdentity()
  const frameId = enc.encode('frame-0002')
  const { sealed, ephemeralPublic } = await sealBody(enc.encode('已完成'), {
    receiverPublic: await importPublicKey(phone.publicKey),
    frameId, header: HEADER, direction: 'n2d',
  })
  const plain = await openBody(sealed, {
    receiverPrivate: phone.privateKey,
    ephemeralPublic: await importPublicKey(ephemeralPublic),
    frameId, header: HEADER, direction: 'n2d',
  })
  assert.equal(dec.decode(plain), '已完成')
})

test('密文里不含明文（relay 拿到也读不到内容）', async () => {
  const { pcPub } = await phoneToPc()
  const secret = 'password=hunter2'
  const { sealed } = await sealBody(enc.encode(secret), {
    receiverPublic: pcPub, frameId: enc.encode('f3'), header: HEADER, direction: 'd2n',
  })
  const asText = dec.decode(sealed.ciphertext)
  assert.ok(!asText.includes('hunter2'), '密文泄漏了明文片段')
  assert.ok(!asText.includes('password'), '密文泄漏了明文字段名')
  assert.ok(sealed.ciphertext.length > secret.length, 'AEAD 密文应长于明文')
})

test('header 被改 → AAD 校验失败（relay 看得见 header，改它不该被接受）', async () => {
  const { pc, pcPub } = await phoneToPc()
  const frameId = enc.encode('f4')
  const { sealed, ephemeralPublic } = await sealBody(enc.encode('x'), {
    receiverPublic: pcPub, frameId, header: HEADER, direction: 'd2n',
  })
  const tampered = enc.encode(JSON.stringify({ v: 2, type: 'route', seq: 999 }))
  await assert.rejects(openBody(sealed, {
    receiverPrivate: pc.privateKey,
    ephemeralPublic: await importPublicKey(ephemeralPublic),
    frameId, header: tampered, direction: 'd2n',
  }))
})

test('frameId 不同 → key 不同，解不开（防跨帧重放）', async () => {
  const { pc, pcPub } = await phoneToPc()
  const { sealed, ephemeralPublic } = await sealBody(enc.encode('secret'), {
    receiverPublic: pcPub, frameId: enc.encode('frame-A'), header: HEADER, direction: 'd2n',
  })
  await assert.rejects(openBody(sealed, {
    receiverPrivate: pc.privateKey,
    ephemeralPublic: await importPublicKey(ephemeralPublic),
    frameId: enc.encode('frame-B'), header: HEADER, direction: 'd2n',
  }))
})

test('方向不同 → key 不同（同一条密文不能反向解读）', async () => {
  const { pc, pcPub } = await phoneToPc()
  const frameId = enc.encode('f6')
  const { sealed, ephemeralPublic } = await sealBody(enc.encode('secret'), {
    receiverPublic: pcPub, frameId, header: HEADER, direction: 'd2n',
  })
  await assert.rejects(openBody(sealed, {
    receiverPrivate: pc.privateKey,
    ephemeralPublic: await importPublicKey(ephemeralPublic),
    frameId, header: HEADER, direction: 'n2d', // 方向反了
  }))
})

test('密文被篡改 → tag 校验失败', async () => {
  const { pc, pcPub } = await phoneToPc()
  const frameId = enc.encode('f7')
  const { sealed, ephemeralPublic } = await sealBody(enc.encode('secret'), {
    receiverPublic: pcPub, frameId, header: HEADER, direction: 'd2n',
  })
  const bad = new Uint8Array(sealed.ciphertext)
  bad[0] ^= 0xff
  await assert.rejects(openBody({ nonce: sealed.nonce, ciphertext: bad }, {
    receiverPrivate: pc.privateKey,
    ephemeralPublic: await importPublicKey(ephemeralPublic),
    frameId, header: HEADER, direction: 'd2n',
  }))
})

test('每帧 ephemeral 都不同（否则 key 可复用，前向保密失效）', async () => {
  const { pcPub } = await phoneToPc()
  const args = { receiverPublic: pcPub, frameId: enc.encode('same'), header: HEADER, direction: 'd2n' as const }
  const a = await sealBody(enc.encode('same plaintext'), args)
  const b = await sealBody(enc.encode('same plaintext'), args)
  assert.ok(!Buffer.from(a.ephemeralPublic).equals(Buffer.from(b.ephemeralPublic)))
  assert.ok(!Buffer.from(a.sealed.ciphertext).equals(Buffer.from(b.sealed.ciphertext)))
})
