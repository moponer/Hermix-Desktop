/**
 * 直连配对 —— 手机 ↔ **桌面端本机** 的 TLS 监听器（默认 `:8443`）。
 *
 * ## 为什么先做这条路
 *
 * 手机端原先打的是 `POST {relay}/api/pairing/complete`，而 relay 里根本没有这个
 * 路由（`apps/relay/src/server.js` 只有 9 条路由），必然 404。补齐那条链路要跨
 * relay + Rust + hm 三端，因为配对码的判定权在 PC 的 `PairingSessions::consume`，
 * 而 PC 只出站连 relay，relay 只能转发不能自己签发 token。
 *
 * 但**局域网直连的整条链路早就是通的**：
 * `POST /api/pairing/sessions`（桌面端设置页「手机端」渠道在调）返回的
 * `pairing_url` 就是 `https://{lan_ip}:8443/pair?c={code}&fp={fingerprint}`，
 * 同一个监听器上已经挂着 `POST /api/pairing/complete`。手机只要照着 `/pair`
 * 落地页做同一件事即可 —— 落地页的 JS 就是这个文件头的契约说明。
 *
 * ## 这个文件里的每个字段都来自 Rust 侧，不是猜的
 *
 * `POST /api/pairing/complete` 的请求体（`pairing.rs::CompletePairingBody`）：
 * ```json
 * { "pairing_code": "123456", "device_name": "…", "cert_fingerprint": "<64 hex>" }
 * ```
 * 成功返回 `{ device_id, device_token }`，token 仅此一次明文下发。
 *
 * `cert_fingerprint` **必填且必须与桌面端本机 TLS 指纹一致**，不符返 403 ——
 * 这一步不是形式主义：同网段 MITM 只要出示自签证书 + 克隆 `/pair` 页就能把配对码
 * 换成 device_token（`pairing.rs:533-537` 的注释记着这段历史）。
 */
import { request } from './http'

/** 配对码位数。服务端 `mint_code` 固定 6 位。 */
const CODE_LEN = 6

export interface DirectTarget {
  /** 去掉 `/pair?...` 的基地址，如 `https://192.168.1.7:8443` */
  baseUrl: string
  code: string
  /** 桌面端本机 TLS 证书指纹（64 位 hex），由二维码携带。 */
  fingerprint: string
}

export interface DirectCompleteResult {
  deviceId: string
  deviceToken: string
}

/**
 * 解析桌面端二维码内容（= `QrPayload.pairing_url`）。
 *
 * 只接受 `https` —— 配对依赖加密面，桌面端在关掉 TLS 时根本发不出二维码
 * （`PairingSessions::create` 会直接返回 Err），这里放行 http 等于给中间人开门。
 * 解析失败返回 `null`，由调用方给出「扫到的不是 hermix 配对码」的提示。
 */
export function parsePairingUrl(raw: string): DirectTarget | null {
  let u: URL
  try {
    u = new URL(raw.trim())
  } catch {
    return null
  }
  if (u.protocol !== 'https:') return null
  if (!/\/pair\/?$/.test(u.pathname)) return null

  const code = (u.searchParams.get('c') ?? '').replace(/\D/g, '')
  if (code.length !== CODE_LEN) return null

  const fp = (u.searchParams.get('fp') ?? '').toLowerCase()
  if (!/^[0-9a-f]{64}$/.test(fp)) return null

  return { baseUrl: u.origin, code, fingerprint: fp }
}

/** 指纹短显：与桌面端设置页「手机端」那一行显示的格式一致（前 16 位）。 */
export function shortFingerprint(fp: string): string {
  return `${fp.slice(0, 16)}…${fp.slice(-4)}`
}

/** 桌面上给设备起的名字。取不到系统信息就退回一个不空的常量。 */
export function defaultDeviceName(): string {
  try {
    const info = uni.getSystemInfoSync()
    const brand = (info.deviceBrand || '').trim()
    const model = (info.deviceModel || '').trim()
    const name = [brand, model].filter(Boolean).join(' ')
    if (name) return `Hermix M · ${name}`
  } catch {
    /* 取不到就用下面的兜底，不该因为一个装饰字段失败 */
  }
  return 'Hermix M'
}

/** 十六进制编码，与 Rust 侧 `to_hex` / relay `hashToken` 一致。 */
function toHex(buf: ArrayBuffer): string {
  return Array.from(new Uint8Array(buf))
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('')
}

/**
 * `sha256(hrel_*)` 的小写 hex。
 *
 * 手机在配对时把这个哈希报给电脑端，**不是明文凭据**。电脑端据此在「撤销设备」
 * 时能让 relay 把这台手机的连接断掉——否则撤销只发生在 hermix 侧，那台手机
 * 发不出帧却仍在收广播帧，撤销按钮就成了摆设。
 *
 * 与 relay 的 `hashToken`（`crypto.createHash('sha256').update(token,'utf8')`）
 * 必须逐字节一致：两边都是 `sha256(utf8(token))` 的小写 hex。
 */
export async function relayTokenHash(token: string): Promise<string> {
  const data = new TextEncoder().encode(token.trim())
  return toHex(await crypto.subtle.digest('SHA-256', data))
}

/**
 * 直连完成配对。失败时 throw，文案按 HTTP 状态分档——
 * 403 与 404 的排查方向完全不同，混成一句「配对失败」是最难查的一类故障。
 */
export async function completeDirect(opts: {
  target: DirectTarget
  deviceName: string
  /** 本机的 relay 凭据；没有就传空串（电脑端会记为「这台设备没绑中转」）。 */
  relayToken?: string
}): Promise<DirectCompleteResult> {
  const { target, deviceName, relayToken = '' } = opts
  const relayHash = relayToken.trim() ? await relayTokenHash(relayToken) : ''

  const res = await request({
    url: `${target.baseUrl}/api/pairing/complete`,
    method: 'POST',
    header: { 'content-type': 'application/json' },
    data: {
      pairing_code: target.code,
      device_name: deviceName,
      cert_fingerprint: target.fingerprint,
      // 空串 = 这台手机没配中转。电脑端据此不再对它做 relay 侧撤销。
      relay_token_hash: relayHash,
    },
    // 局域网往返很快，但 TLS 握手 + DPAPI 解密可能慢一点，给宽一点。
    timeout: 15000,
  })

  if (!res.reachable) {
    throw new Error(
      res.fail === 'timeout'
        ? '连不上电脑。确认手机和电脑在同一个 Wi-Fi。'
        : '连不上电脑。确认手机和电脑在同一个 Wi-Fi，且这台电脑上的 hermix 正在运行。',
    )
  }
  switch (res.statusCode) {
    case 200:
      break
    case 400:
      throw new Error('请求缺字段，请重新扫一次桌面端的二维码。')
    case 403:
      throw new Error('证书指纹不匹配——不是这台电脑，或中间有东西在拦。请重新扫码。')
    case 429:
      throw new Error('错误次数过多，在电脑上重新生成配对码再试。')
    case 503:
      throw new Error('这台电脑的设备注册表不可用，检查一下 hermix 的启动日志。')
    default:
      throw new Error(`配对失败（HTTP ${res.statusCode}）`)
  }

  const data = (res.data ?? {}) as { device_id?: string; device_token?: string }
  if (!data.device_token || !data.device_id) {
    throw new Error('配对响应里没有设备凭据，桌面端可能不是最新版本。')
  }
  return { deviceId: data.device_id, deviceToken: data.device_token }
}
