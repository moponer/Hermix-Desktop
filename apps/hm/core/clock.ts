/**
 * 设备时钟偏差检查 + relay 基地址。
 *
 * ## 为什么必须有
 * 请求签名覆盖 `METHOD\npath\nt\nnonce`，`t` 是**客户端**时钟的 unix 秒，
 * 服务端只接受 ±300 秒（`request_signature.rs::TIMESTAMP_WINDOW_SECS`）。
 * 时钟偏了 → 全部 401 → 用户看到「配对成功了但连不上」。这类故障极难自查，
 * 所以要在配对**之前**把偏差摆到台面上，而不是等连不上再让人猜。
 *
 * ## 为什么「测不到」也是一种要展示的结果
 *
 * 探针用响应里的 `Date` 头当服务端时间基准。但 `Date` **不是 CORS 简单响应头**：
 * H5 目标下浏览器不会把它交给 JS，除非 relay 明确 `Access-Control-Expose-Headers`。
 * 所以：
 * - App / 小程序端（原生请求，无 CORS）→ 测得到；
 * - H5 → 读不到 `Date`，返回 `header-hidden`。
 *
 * 把 `header-hidden` 谎报成「偏差 0」等于让用户以为设备时间没问题，
 * 然后在配对那一步被 401 打脸。**测不到就说测不到。**
 */
import { request, header } from './http'

const SKEW_PROBE_PATH = '/health'

/** 超过这个秒数就在配对屏上给警告（服务端硬窗是 ±300s，但 30s 就该提醒了）。 */
export const SKEW_WARN_SECS = 30

/** 时钟偏差测不到时的原因。分三种，排查方向完全不同。 */
export type SkewUnmeasurable = 'no-relay' | 'unreachable' | 'header-hidden'

export interface SkewProbe {
  /** 偏差秒数，正数 = 设备快；null = 测不到，看 `reason`。 */
  skew: number | null
  reason?: SkewUnmeasurable
}

/**
 * 量出设备时钟相对服务端的偏差。
 *
 * 走 core/http 的显式超时请求——原先用裸 `fetch`，没有超时，弱网下这一句
 * 能把配对屏的 loading 挂到天荒地老。
 */
export async function checkClockSkew(baseUrl?: string): Promise<SkewProbe> {
  const base = baseUrl ?? relayBaseUrl()
  if (!base) return { skew: null, reason: 'no-relay' }

  const t0 = Date.now()
  const res = await request({ url: `${base}${SKEW_PROBE_PATH}`, method: 'GET' })
  const t1 = Date.now()
  if (!res.reachable) return { skew: null, reason: 'unreachable' }

  const date = header(res, 'date')
  if (!date) return { skew: null, reason: 'header-hidden' }

  const serverMs = Date.parse(date)
  if (Number.isNaN(serverMs)) return { skew: null, reason: 'header-hidden' }

  // 折半 RTT：请求发出到响应头到达，只走了半个来回
  const serverAtArrival = serverMs + (t1 - t0) / 2
  return { skew: Math.round((t0 - serverAtArrival) / 1000) }
}

/**
 * relay 基地址。
 *
 * 显式读配置而不是散落各处硬编码——协议层因此**保持 base-URL 可替换**：
 * 换 relay 是改这里，不动业务代码。
 */
export function relayBaseUrl(): string | null {
  try {
    return uni.getStorageSync('hermix.relayBaseUrl') || null
  } catch {
    return null
  }
}

export function setRelayBaseUrl(url: string): void {
  uni.setStorageSync('hermix.relayBaseUrl', url.replace(/\/+$/, ''))
}
