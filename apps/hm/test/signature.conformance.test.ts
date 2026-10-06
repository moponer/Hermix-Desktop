/**
 * 跨语言契约自检：在 Node 里跑 TS 侧实现，与 Rust 侧固定向量比对。
 *
 * 这条测试存在的唯一理由：签名算法横跨 Rust（hermix-server）与 TS（App）两个
 * 工具链，两边都跑 round-trip 是没用的——**自己算的能被自己验**，两边算法
 * 一起错照样绿。唯一能钉住线格式的是同一个常量。
 *
 * 权威定义在 Rust：`crates/hermix-server/src/request_signature.rs` 的
 * `cross_language_signature_vector`。改任一侧必须同步改另一侧。
 *
 *   node --test test/signature.conformance.test.ts
 *   （或经由 npm run test:conformance）
 */
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { webcrypto } from 'node:crypto'
import { computeSignature, newNonce, CONFORMANCE_VECTOR as VECTOR } from '../core/signature'

// Node 18+ 的 globalThis.crypto 在部分版本下没有 subtle，统一注入
if (!globalThis.crypto?.subtle) {
  Object.defineProperty(globalThis, 'crypto', { value: webcrypto, configurable: true })
}

test('TS 侧签名与 Rust 固定向量逐字一致', async () => {
  const got = await computeSignature(VECTOR.token, VECTOR.method, VECTOR.path, VECTOR.t, VECTOR.nonce)
  assert.equal(
    got,
    VECTOR.sig,
    '与 hermix-server 的 cross_language_signature_vector 不一致 —— 两端签名会互相拒绝',
  )
})

test('path 变了签名必须变（签名覆盖 METHOD\\npath）', async () => {
  const other = await computeSignature(VECTOR.token, VECTOR.method, '/v1/ws-stream/s2', VECTOR.t, VECTOR.nonce)
  assert.notEqual(other, VECTOR.sig, '签名必须绑定 path —— relay 重写 path 就会全废')
})

test('method 变了签名必须变', async () => {
  const other = await computeSignature(VECTOR.token, 'POST', VECTOR.path, VECTOR.t, VECTOR.nonce)
  assert.notEqual(other, VECTOR.sig)
})

test('nonce 每次都不同（服务端按 nonce 去重，重放会被判 BadSignature）', () => {
  const a = newNonce()
  const b = newNonce()
  assert.notEqual(a, b)
  assert.match(a, /^[0-9a-f]{32}$/)
})
