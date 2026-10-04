/**
 * 凭据与配对。
 *
 * ⚠️ **凭据落盘位置是个未决项，不是已解决的问题**：原设计（Kotlin）用
 * `EncryptedSharedPreferences`（Android Keystore 背书），换到 uni-app 后
 * JS 层只有 `uni.setStorageSync`（**明文**）。存 API key 与 `hmxd_*` 设备
 * token 到明文存储里，是这套技术栈换来的真实代价，不能默默咽下。
 * 三个待选：
 *   a) 接受明文（不可接受：token 泄漏 = 任意远程会话 + 任意工具授权）
 *   b) 走 UTS 原生插件把 Keystore 调用透出来（引入原生工程，抵消跨端收益）
 *   c) 凭据不落手机，改由 PC 侧保存并在需要时代为调用（与"手机持有 TTS
 *      key"的设计冲突，且把音频路径塞回 relay）
 * 本文件先把接口留出来，**不假装已经安全**。
 */
import { computeSignature, signatureHeader, newNonce } from './signature'

const KEY_DEVICE_TOKEN = 'hermix.deviceToken'
const KEY_RELAY_TOKEN = 'hermix.relayToken'
const KEY_ASR_KEY = 'hermix.asr.apiKey'
const KEY_TTS_KEY = 'hermix.tts.apiKey'

function get(k: string): string {
  try {
    return uni.getStorageSync(k) || ''
  } catch {
    return ''
  }
}
function set(k: string, v: string): void {
  uni.setStorageSync(k, v)
}

export const deviceToken = () => get(KEY_DEVICE_TOKEN)
export const relayToken = () => get(KEY_RELAY_TOKEN)

/** ⚠️ 明文存储，见文件头。 */
export function setVoiceKeys(asrKey: string, ttsKey: string): void {
  if (asrKey) set(KEY_ASR_KEY, asrKey)
  if (ttsKey) set(KEY_TTS_KEY, ttsKey)
}
export const asrKey = () => get(KEY_ASR_KEY)
export const ttsKey = () => get(KEY_TTS_KEY)

/**
 * 取 relay 证书的 SHA-256 指纹（十六进制）。
 *
 * H5 里没有"读 TLS 证书"的标准 API —— 浏览器 JS 出于安全刻意不暴露它。
 * 所以这里只能用一条**它自己声明的**探测端点取指纹（relay 在
 * `X-Hermix-Cert-Sha256` 响应头里回自己的证书指纹）。
 * **这个前提必须成立**，否则屏 1 展示的指纹是自证的、毫无安全价值。
 * 非 H5 目标（Android/iOS 原生壳）要用原生 TLS 校验取真实证书指纹。
 */
export async function fingerprintOf(baseUrl: string): Promise<string | null> {
  const res = await fetch(`${baseUrl.replace(/\/+$/, '')}/health`)
  const fp = res.headers.get('x-hermix-cert-sha256')
  return fp && /^[0-9a-f]{64}$/i.test(fp) ? fp.toLowerCase() : null
}

/** 用 6 位配对码换设备 token。 */
export async function completePairing(opts: { relayUrl: string; code: string }): Promise<void> {
  const base = opts.relayUrl.replace(/\/+$/, '')
  const res = await fetch(`${base}/api/pairing/complete`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ code: opts.code }),
  })
  if (!res.ok) throw new Error(`配对失败：HTTP ${res.status}`)
  const data = (await res.json()) as { device_id?: string; device_token?: string }
  if (!data.device_token) throw new Error('配对响应里没有 device_token')
  set(KEY_DEVICE_TOKEN, data.device_token)
}

export { computeSignature, signatureHeader, newNonce }
