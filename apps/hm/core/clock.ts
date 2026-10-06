/**
 * 设备时钟偏差检查。
 *
 * 为什么必须有：请求签名覆盖 `METHOD\npath\nt\nnonce`，`t` 是**客户端**时钟的
 * unix 秒，服务端只接受 ±300 秒（`request_signature.rs::TIMESTAMP_WINDOW_SECS`）。
 * 时钟偏了 → 全部 401 → 用户看到"配对成功了但连不上"。这类故障极难自查，
 * 所以要在配对**之前**就把偏差摆到台面上，而不是等连不上再让人猜。
 *
 * 测量方式：向 hermix 的一个 GET 端点发请求，比对 `Date` 响应头与本地时钟。
 * 往返时间要折半——`Date` 头只有秒级精度，量出来的差值天然含半个 RTT。
 */

const SKEW_PROBE_PATH = '/health'

/** 超过这个秒数就在屏 1 上给警告（服务端硬窗是 ±300s，但 30s 就该提醒了）。 */
export const SKEW_WARN_SECS = 30

/**
 * 量出设备时钟相对服务端的偏差（秒，正数 = 设备快）。
 *
 * 探不到（离线 / relay 未通）时返回 `null` —— 那不是"时钟没问题"，
 * 调用方要能区分"没量到"和"量到偏差 0"。
 */
export async function checkClockSkew(baseUrl?: string): Promise<number | null> {
  const base = baseUrl ?? relayBaseUrl()
  if (!base) return null
  const t0 = Date.now()
  try {
    const res = await fetch(`${base}${SKEW_PROBE_PATH}`, { method: 'GET' })
    const t1 = Date.now()
    const date = res.headers.get('date')
    if (!date) return null
    const serverMs = Date.parse(date)
    if (Number.isNaN(serverMs)) return null
    // 折半 RTT：请求发出到响应头到达，只走了半个来回
    const serverAtArrival = serverMs + (t1 - t0) / 2
    return Math.round((t0 - serverAtArrival) / 1000)
  } catch {
    return null
  }
}

/**
 * relay 基地址。
 *
 * 显式读配置而不是散落各处硬编码——待拍板 4 已定「relay 不做直连降级」，
 * 协议层因此**保持 base-URL 可替换**：换 relay 是改这里，不动业务代码。
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
