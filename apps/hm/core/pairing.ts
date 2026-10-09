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
import { generateIdentity, type Identity } from './crypto'
import { request, header } from './http'

const KEY_DEVICE_TOKEN = 'hermix.deviceToken'
const KEY_DEVICE_ID = 'hermix.deviceId'
const KEY_RELAY_TOKEN = 'hermix.relayToken'
const KEY_ASR_KEY = 'hermix.asr.apiKey'
const KEY_TTS_KEY = 'hermix.tts.apiKey'
/**
 * E2EE 静态公钥（base64 / 65B 未压缩 P-256）。
 *
 * **私钥永不落盘**：它只活在 WebCrypto 的 CryptoKey 里，进程内使用。落盘的
 * 只有公钥——公钥本来就是公开的，落盘是为了重启后不用重新配对。
 *
 * ⚠️ 与文件头同一个隐患：这里是 `uni.setStorageSync`（明文）。但暴露的只是
 * 公钥，泄漏不构成风险（保密性来自 PC 侧的私钥）。
 */
const KEY_E2EE_PUBLIC = 'hermix.e2ee.publicKey'
/**
 * 桌面端的 E2EE 静态公钥，来自配对二维码。
 *
 * 这是**加密帧的信任锚**：手机用它 seal 给 PC 的帧，PC 用私钥解。配对码只
 * 证明「这台电脑是我要连的那台」，公钥才是「以后我发的帧只有它解得开」——
 * 两者缺一不可，所以它在配对时就一并落盘，而不是等 WS 建连时再补。
 */
const KEY_E2EE_PC_PUBLIC = 'hermix.e2ee.pcPublicKey'

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
export const deviceId = () => get(KEY_DEVICE_ID)
export const relayToken = () => get(KEY_RELAY_TOKEN)

/**
 * 清空本机全部凭据（设置页「重新配对」用）。
 *
 * 必须连 `hermix.e2ee.publicKey` 一起清：E2EE 私钥本来就不可导出、冷启动
 * 就是新的一把，留着旧公钥只会让用户以为自己还连着旧会话。
 */
export function clearCredentials(): void {
  for (const k of [
    KEY_DEVICE_TOKEN,
    KEY_DEVICE_ID,
    KEY_RELAY_TOKEN,
    KEY_ASR_KEY,
    KEY_TTS_KEY,
    KEY_E2EE_PUBLIC,
    KEY_E2EE_PC_PUBLIC,
  ]) {
    try {
      uni.removeStorageSync(k)
    } catch {
      /* 单项失败不影响其余清理 */
    }
  }
}

/** 本机是否已完成配对（判据：拿到了设备 token）。 */
export function isPaired(): boolean {
  return deviceToken().length > 0
}

/**
 * 存 relay 凭据（`hrel_*`）。
 *
 * 与 `hmxd_*` 是**两套**凭据，别混：前者是 relay 的参与者凭据（决定这台连接
 * 被路由到哪台电脑），后者是 hermix 的设备凭据（决定这帧有没有权限被处理）。
 * 两者的签发方、存放位置、被谁验证都不同。
 *
 * ⚠️ 明文存储，见文件头。这里的值来自用户在桌面端复制粘贴，且它**只影响路由**：
 * 帧本身是 E2EE 密封的，帧内的授权由 `hmxd_*` 决定。
 */
export function setRelayToken(t: string): void {
  set(KEY_RELAY_TOKEN, t.trim())
}

export const hasRelayToken = () => relayToken().length > 0

/** ⚠️ 明文存储，见文件头。 */
export function setVoiceKeys(asrKey: string, ttsKey: string): void {
  if (asrKey) set(KEY_ASR_KEY, asrKey)
  if (ttsKey) set(KEY_TTS_KEY, ttsKey)
}
export const asrKey = () => get(KEY_ASR_KEY)
export const ttsKey = () => get(KEY_TTS_KEY)

/**
 * relay 身份的核对结果。**三种状态，不是一个布尔。**
 *
 * 改造前这里是 `Promise<string | null>`，而调用方把 `null` 一律渲染成
 * 「连不上这个 relay。检查域名解析与证书。」——于是 H5 下**每一次**都显示
 * 这句，因为 H5 读不到 `x-hermix-cert-sha256`（它不是 CORS 简单响应头，
 * relay 也没发 `Access-Control-Expose-Headers`）。用户被引导去查一个
 * 根本不存在的证书问题，而真正的原因「浏览器不让你看这个头」没人告诉他。
 *
 * 把「连不上」「连上了但字段不可见」「拿到了」分成三态，配对屏才能说人话。
 */
export type RelayIdentity =
  /** 拿到了指纹（relay 配了 `HERMIX_RELAY_CERT_SHA256`，且本环境允许读该头）。 */
  | { state: 'verified'; fingerprint: string }
  /** 请求成功，但没有这个头——要么 relay 没配，要么本环境不允许 JS 读。 */
  | { state: 'invisible' }
  /** 请求压根没到对端：断网 / DNS / TLS / 超时。 */
  | { state: 'unreachable'; timeout: boolean }

/**
 * 取 relay 证书的 SHA-256 指纹（十六进制）。
 *
 * ⚠️ H5 里没有「读 TLS 证书」的标准 API —— 浏览器 JS 出于安全刻意不暴露它。
 * 所以这里只能用一条**它自己声明的**探测端点取指纹（relay 在
 * `X-Hermix-Cert-Sha256` 响应头里回自己的证书指纹）。
 * **这个前提必须成立**，否则展示的指纹是自证的、毫无安全价值。
 * 非 H5 目标（Android/iOS 原生壳）要用原生 TLS 校验取真实证书指纹。
 *
 * 顺带说明：以上说的是「这个值的来源」；而「本环境能不能读到它」是另一
 * 件事，由 `state:'invisible'` 如实反映。
 */
export async function fingerprintOf(baseUrl: string): Promise<RelayIdentity> {
  const res = await request({ url: `${baseUrl.replace(/\/+$/, '')}/health`, method: 'GET' })
  if (!res.reachable) return { state: 'unreachable', timeout: res.fail === 'timeout' }
  const fp = header(res, 'x-hermix-cert-sha256')
  if (!fp || !/^[0-9a-f]{64}$/i.test(fp)) return { state: 'invisible' }
  return { state: 'verified', fingerprint: fp.toLowerCase() }
}

// ── 配对 ───────────────────────────────────────────────────────
//
// 配对本身走 `core/direct.ts`（直连桌面端本机 TLS 监听器）。
// 这里**不再有**「POST {relay}/api/pairing/complete」——relay 里从来没有这条
// 路由，保留它只会让用户撞 404。跨网络的 relay 中转是下一步的事，见
// docs/notes/proposed/feature/2026-10-09-mobile-ui-refactor 的 Consequences。

/** 本机运行时有 SubtleCrypto。E2EE 帧加密的前提，没它就不能配对。 */
export function hasSubtleCrypto(): boolean {
  return typeof globalThis.crypto?.subtle?.importKey === 'function'
}

/**
 * 落盘一次成功的配对结果。
 *
 * token 仅此一次明文下发（`pairing.rs::complete_pairing` 的注释原话），所以
 * 这一步落空了就只能重新配对——没有任何补救路径。
 *
 * 顺带把**本机 E2EE 身份**一起生成：WebCrypto 私钥不可导出，冷启动就是新的
 * 一把，而 PC 侧存的旧公钥会失效。所以每次连上都要重发一次 `key.exchange`，
 * 公钥现在先备好，等 WS 通了直接用。
 *
 * ## `pcPublicKey` 目前恒为 undefined —— 一个已知缺口
 *
 * `core/crypto.ts` 的注释说「手机扫码已经拿到 `pk_pc`（二维码里带的）」，
 * **但二维码 URL 里并没有它**：`pairing_url` 只有 `c` 与 `fp` 两个参数
 * （`pairing.rs:538`），`node_public_key` 只存在于 `QrPayload` 的 JSON 里，
 * `/pair` 落地页的 URL 没带、页面也没把它传给前端。所以直连这条路拿不到
 * PC 公钥，必须等 WS 建连时用 `key.exchange` 帧协商。
 *
 * 宁可不存：存一个空值会让 `pcPublicKey()` 看起来"有值"，后续 seal 时才会炸，
 * 那比现在明确地缺更难查。
 */
export async function storePairedDevice(opts: {
  deviceId: string
  deviceToken: string
  pcPublicKey?: string
}): Promise<void> {
  set(KEY_DEVICE_ID, opts.deviceId)
  set(KEY_DEVICE_TOKEN, opts.deviceToken)
  if (opts.pcPublicKey) set(KEY_E2EE_PC_PUBLIC, opts.pcPublicKey)
  await generateOwnIdentity()
}

/** 桌面端 E2EE 公钥（base64，未压缩 P-256 65B）。没配对时是空串。 */
export const pcPublicKey = () => get(KEY_E2EE_PC_PUBLIC)

// ── E2EE 身份 ────────────────────────────────────────────────────

/**
 * 生成本机的 E2EE 身份。
 *
 * **私钥永不落盘，而且无法持久化**——WebCrypto 的 CryptoKey 不可导出，
 * 所以手机每次冷启动都是一把新私钥。这不是缺陷，是更安全的形态：设备上
 * 根本没有可被偷的私钥。
 *
 * 代价是 PC 侧存的旧公钥会失效，**每次冷启动必须重发一次 `key.exchange`**
 * 让 PC 覆盖（`E2eeCtx::learn_peer` 是 upsert）。少这一步的症状是
 * PC 发出的帧手机解不开。
 *
 * 落盘的只有公钥——它本来就是扫码要传的东西，泄漏不构成风险。
 */
export async function generateOwnIdentity(): Promise<Identity> {
  const id = await generateIdentity()
  set(KEY_E2EE_PUBLIC, bytesToB64(id.publicKey))
  return id
}

/** 本机公钥（base64），供配对时随码带给 PC。 */
export async function ownPublicKeyB64(): Promise<string> {
  const id = await generateOwnIdentity()
  return bytesToB64(id.publicKey)
}

function bytesToB64(bytes: Uint8Array): string {
  let s = ''
  for (const b of bytes) s += String.fromCharCode(b)
  return btoa(s)
}

export { computeSignature, signatureHeader, newNonce }
