/**
 * App 端缺失的 Web 编码 API 补丁。
 *
 * uni-app 的 App（Android/iOS）JS 运行时基于 V8，但不带浏览器的
 * `TextEncoder` / `TextDecoder` / `btoa` / `atob` 全局。core 里的 relay
 * 帧加解密、请求签名、配对都依赖它们——且 `crypto.ts` 在**模块顶层**就立刻
 * 调用 `new TextEncoder()`，缺一个全局就会让整个应用在加载阶段抛
 * `ReferenceError` → 白屏（`createInstanceContext failed`）。
 *
 * 本模块必须在任何 core 模块被 import 之前先求值：`main.js` 首行静态
 * `import './core/encoding'` 即可（ESM 按声明顺序求值）。H5 / 小程序里
 * 这些 API 已存在，条件判断只补缺失项，不做覆盖。
 */

const g = globalThis as Record<string, unknown>

// ES2022 `Object.hasOwn` polyfill。@noble/curves、@noble/hashes 的 utils.js
// 在模块顶层就调它，而 App 端老 V8 没有 —— 缺它整条 crypto 链加载即炸，
// 表现为启动图永远不关（转圈）。语义与规范一致：只判自有属性。
if (typeof g.Object !== 'object' || typeof (g.Object as { hasOwn?: unknown }).hasOwn !== 'function') {
  ;(Object as unknown as { hasOwn: (o: object, k: PropertyKey) => boolean }).hasOwn = (o, k) =>
    Object.prototype.hasOwnProperty.call(o, k)
}

/** UTF-8 编码。入参是本应交给 `TextEncoder` 的字符串。 */
function utf8Encode(s: string): Uint8Array {
  const out: number[] = []
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i)
    if (c < 0x80) {
      out.push(c)
    } else if (c < 0x800) {
      out.push(0xc0 | (c >> 6), 0x80 | (c & 0x3f))
    } else if (c >= 0xd800 && c <= 0xdbff) {
      const c2 = s.charCodeAt(++i)
      const cp = 0x10000 + ((c - 0xd800) << 10) + (c2 - 0xdc00)
      out.push(
        0xf0 | (cp >> 18),
        0x80 | ((cp >> 12) & 0x3f),
        0x80 | ((cp >> 6) & 0x3f),
        0x80 | (cp & 0x3f),
      )
    } else {
      out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f))
    }
  }
  return new Uint8Array(out)
}

/** UTF-8 解码。入参为本应交给 `TextDecoder` 的字节序列。 */
function utf8Decode(input: Uint8Array | ArrayBuffer): string {
  const b = input instanceof Uint8Array ? input : new Uint8Array(input)
  let out = ''
  for (let i = 0; i < b.length; ) {
    const c = b[i++]
    if (c < 0x80) {
      out += String.fromCharCode(c)
    } else if (c < 0xe0) {
      out += String.fromCharCode(((c & 0x1f) << 6) | (b[i++] & 0x3f))
    } else if (c < 0xf0) {
      out += String.fromCharCode(
        ((c & 0x0f) << 12) | ((b[i++] & 0x3f) << 6) | (b[i++] & 0x3f),
      )
    } else {
      const cp =
        ((c & 0x07) << 18) |
        ((b[i++] & 0x3f) << 12) |
        ((b[i++] & 0x3f) << 6) |
        (b[i++] & 0x3f)
      const u = cp - 0x10000
      out += String.fromCharCode(0xd800 + (u >> 10), 0xdc00 + (u & 0x3ff))
    }
  }
  return out
}

const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'

/** latin1 字符串 → base64。 */
function bytesToBase64(bin: string): string {
  let out = ''
  for (let i = 0; i < bin.length; i += 3) {
    const n = (bin.charCodeAt(i) << 16) | ((bin.charCodeAt(i + 1) || 0) << 8) | (bin.charCodeAt(i + 2) || 0)
    out += B64[(n >> 18) & 63] + B64[(n >> 12) & 63]
    out += i + 1 < bin.length ? B64[(n >> 6) & 63] : '='
    out += i + 2 < bin.length ? B64[n & 63] : '='
  }
  return out
}

/** base64 → latin1 字符串。 */
function base64ToBytes(b64: string): string {
  const clean = b64.replace(/=+$/, '')
  let out = ''
  for (let i = 0; i < clean.length; i += 4) {
    const n1 = B64.indexOf(clean[i])
    const n2 = B64.indexOf(clean[i + 1])
    const n3 = B64.indexOf(clean[i + 2])
    const n4 = B64.indexOf(clean[i + 3])
    out += String.fromCharCode((n1 << 2) | (n2 >> 4))
    if (n3 !== -1) out += String.fromCharCode(((n2 & 15) << 4) | (n3 >> 2))
    if (n4 !== -1) out += String.fromCharCode(((n3 & 3) << 6) | n4)
  }
  return out
}

if (typeof g.TextEncoder === 'undefined') {
  g.TextEncoder = class {
    encode(input = ''): Uint8Array {
      return utf8Encode(String(input))
    }
    encodeInto() {
      throw new Error('encodeInto is not supported by the App-side TextEncoder shim')
    }
  }
}

if (typeof g.TextDecoder === 'undefined') {
  g.TextDecoder = class {
    decode(input: Uint8Array | ArrayBuffer): string {
      return utf8Decode(input)
    }
  }
}

if (typeof g.btoa === 'undefined') {
  g.btoa = bytesToBase64
}

if (typeof g.atob === 'undefined') {
  g.atob = base64ToBytes
}
