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
import { relayBaseUrl, setRelayBaseUrl } from './clock'

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

// ── 多桌面（2026-10-12）───────────────────────────────────────────
// 一台手机可以配对多台电脑。设备清单 + 「当前设备」指针，切换 = 改指针：
//   hermix.devices      = DeviceRec[]
//   hermix.activeDevice = 设备 id
// 下面四个 KEY_* 单值键是旧单机模型的遗留，只在**迁移**时读一次，
// 迁移完成后即删除——事实单一归属，不留双真相源。
const KEY_DEVICES = 'hermix.devices'
const KEY_ACTIVE = 'hermix.activeDevice'

/** 一台已配对的电脑。relayBase/relayToken/pcPublicKey 都** per-device**：
 * 两台电脑可以走不同 relay，E2EE 信任锚更是每台各一把。 */
export interface DeviceRec {
  /** PC 侧设备 id（`hm-xxxx`），也是清单内的唯一键。 */
  id: string
  /** 设备凭据 `hmxd_*`，仅配对成功那刻明文下发一次。 */
  token: string
  /** 用户起的名字（配对流程里输入），列表展示与切换面板都用它。 */
  name: string
  /** 该 PC 所在的 relay 地址。 */
  relayBase: string
  /** 该 relay 的参与者凭据 `hrel_*`。 */
  relayToken: string
  /** 这台 PC 的 E2EE 静态公钥（base64）。 */
  pcPublicKey: string
  addedAt: string
}

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

/**
 * 旧单机键 → devices[0]，只在清单键不存在时跑一次，迁完即删旧键。
 * 已配对用户升级后无感：activeDevice 直接指向原设备，token/凭据原样保留。
 */
function migrateLegacyIfNeeded(): void {
  if (get(KEY_DEVICES)) return
  const token = get(KEY_DEVICE_TOKEN)
  if (!token) {
    set(KEY_DEVICES, '[]')
    return
  }
  const rec: DeviceRec = {
    id: get(KEY_DEVICE_ID) || 'dev-legacy',
    token,
    name: '我的电脑',
    relayBase: relayBaseUrl() ?? '',
    relayToken: get(KEY_RELAY_TOKEN),
    pcPublicKey: get(KEY_E2EE_PC_PUBLIC),
    addedAt: new Date().toISOString(),
  }
  set(KEY_DEVICES, JSON.stringify([rec]))
  set(KEY_ACTIVE, rec.id)
  for (const k of [KEY_DEVICE_TOKEN, KEY_DEVICE_ID, KEY_RELAY_TOKEN, KEY_E2EE_PC_PUBLIC]) {
    try {
      uni.removeStorageSync(k)
    } catch {
      /* 单项失败不影响迁移主体 */
    }
  }
}

function loadDevices(): DeviceRec[] {
  migrateLegacyIfNeeded()
  try {
    const parsed = JSON.parse(get(KEY_DEVICES) || '[]') as DeviceRec[]
    return Array.isArray(parsed) ? parsed : []
  } catch {
    return []
  }
}
function saveDevices(list: DeviceRec[]): void {
  set(KEY_DEVICES, JSON.stringify(list))
}

/** 全部已配对的电脑（切换面板 / 设备管理页直接渲染这个）。 */
export const listDevices = loadDevices

/** 当前生效的设备。指针失效（被删/脏数据）时回落到第一台。 */
export function activeDevice(): DeviceRec | null {
  const list = loadDevices()
  const id = get(KEY_ACTIVE)
  return list.find((d) => d.id === id) ?? list[0] ?? null
}

/**
 * 切换当前电脑。只改指针 + 同步 relay 镜像键——原设备的凭据原样留在清单里，
 * 切回来不用重新配对。返回 false 表示 id 不在清单里（面板数据过期）。
 */
export function switchDevice(id: string): boolean {
  const rec = loadDevices().find((d) => d.id === id)
  if (!rec) return false
  set(KEY_ACTIVE, id)
  if (rec.relayBase) setRelayBaseUrl(rec.relayBase)
  return true
}

/** 给设备改名（设备管理页）。返回 false = 设备不存在。 */
export function renameDevice(id: string, name: string): boolean {
  const list = loadDevices()
  const rec = list.find((d) => d.id === id)
  if (!rec) return false
  rec.name = name.trim() || rec.name
  saveDevices(list)
  return true
}

/**
 * 移除一台电脑。删的是当前设备时自动切到剩余的第一台（并同步 relay 镜像）；
 * 删到最后一台 = 回到未配对状态。
 */
export function removeDevice(id: string): void {
  const list = loadDevices().filter((d) => d.id !== id)
  saveDevices(list)
  if (get(KEY_ACTIVE) !== id) return
  const next = list[0]
  if (next) {
    set(KEY_ACTIVE, next.id)
    if (next.relayBase) setRelayBaseUrl(next.relayBase)
  } else {
    try {
      uni.removeStorageSync(KEY_ACTIVE)
    } catch {
      /* 已是未配对态 */
    }
  }
}

export function deviceToken(): string {
  return activeDevice()?.token ?? ''
}
export function deviceId(): string {
  return activeDevice()?.id ?? ''
}
export function relayToken(): string {
  return activeDevice()?.relayToken ?? ''
}
/** 当前设备的 E2EE 信任锚。多设备后每台各一把，切机随之切换。 */
export function pcPublicKey(): string {
  return activeDevice()?.pcPublicKey ?? ''
}

/**
 * 清空本机全部配对与凭据（设置页「清空凭据并重新配对」用）。
 *
 * 多设备后清的是**整个设备清单**，不只是当前那台。必须连
 * `hermix.e2ee.publicKey` 一起清：E2EE 私钥本来就不可导出、冷启动
 * 就是新的一把，留着旧公钥只会让用户以为自己还连着旧会话。
 */
export function clearCredentials(): void {
  for (const k of [
    KEY_DEVICES,
    KEY_ACTIVE,
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
  const v = t.trim()
  // 镜像键只服务「未配对手填」的场景；已配对时真身在 active 设备记录里。
  set(KEY_RELAY_TOKEN, v)
  const list = loadDevices()
  const cur = activeDevice()
  const rec = cur ? list.find((d) => d.id === cur.id) : null
  if (rec) {
    rec.relayToken = v
    saveDevices(list)
  }
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
// 配对走 `core/direct.ts`：手机扫码拿到 relay 地址 + 手机凭据(hrel_) + 配对码 +
// PC 公钥(k)，经 relay 中转用密封帧完成绑定（见 `direct.ts` 文件头）。这是
// 2026-10-10 拍板的唯一路径——不要求手机与电脑同网、不比对证书指纹、不依赖
// 电脑开入站端口。`POST /api/pairing/complete` 与 `/pair` 落地页是旧直连时代的
// 残留，已退役未删（PC 侧仍编译保留，移动端不再调用）。

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
 * ## PC 公钥：从配对二维码的 `k` 参数而来，配对时落盘
 *
 * 2026-10-10 起配对全走 relay，二维码 URL 是
 * `{relay}/pair?c=码&t=手机凭据&k=PC公钥`（`pairing.rs` 生成），`k` 即
 * `node_public_key`。`core/direct.ts::parsePairingUrl` 把 `k` 解成
 * `pcPublicKey`，`pages/pairing/index.vue` 在配对成功后通过
 * `storePairedDevice({ ..., pcPublicKey })` 把它落盘。
 *
 * 落盘的是公钥（本来就公开），泄漏不构成风险；私钥仍在 WebCrypto 里，冷启动
 * 换新。聊天连接的密封帧信任锚就是它——重发 `key.exchange` 之前手机就能直接
 * seal 给 PC，比"等首帧协商"少一次往返。
 */
export async function storePairedDevice(opts: {
  deviceId: string
  deviceToken: string
  pcPublicKey?: string
  /** 配对流程里用户起的名字；缺省回退「我的电脑」。 */
  name?: string
  /** 该 PC 所在的 relay；缺省取当前镜像（配对页在调它之前已 setRelayBaseUrl）。 */
  relayBase?: string
  /** 该 relay 的凭据；缺省取当前镜像。 */
  relayToken?: string
}): Promise<void> {
  const list = loadDevices()
  const rec: DeviceRec = {
    id: opts.deviceId,
    token: opts.deviceToken,
    name: opts.name?.trim() || '我的电脑',
    relayBase: opts.relayBase ?? relayBaseUrl() ?? '',
    relayToken: opts.relayToken ?? get(KEY_RELAY_TOKEN),
    pcPublicKey: opts.pcPublicKey ?? '',
    addedAt: new Date().toISOString(),
  }
  // 同一台电脑重新配对 = 原地更新（token 换新），不重复进清单。
  const idx = list.findIndex((d) => d.id === rec.id)
  if (idx >= 0) list[idx] = rec
  else list.push(rec)
  saveDevices(list)
  set(KEY_ACTIVE, rec.id)
  if (rec.relayBase) setRelayBaseUrl(rec.relayBase)
  await generateOwnIdentity()
}

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
