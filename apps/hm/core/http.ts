/**
 * 唯一出网口。
 *
 * 为什么不用裸 `fetch`：三个硬伤，每一个都对应过一次线上现象。
 *
 * 1. **没有超时。** `fetch` 不带默认超时，DNS 黑洞 / 弱网时能挂到系统层的
 *    超时（几十秒）。配对屏在 `onLoad` 里 await 它，用户看到的就是「一直
 *    转圈」——这正是首屏卡死的直接成因。
 * 2. **看不见自定义响应头。** `x-hermix-cert-sha256` 不是 CORS 简单响应头，
 *    浏览器只有在服务端给了 `Access-Control-Expose-Headers` 时才让 JS 读
 *    到。裸 `fetch` 拿到 `undefined`，**看起来像「relay 没有这个头」，其实
 *    是「这个环境看不见它」**。把两者混同，就会把用户引去查一个根本不
 *    存在的证书问题。
 * 3. **App / 小程序端没有全局 `fetch`。** `uni.request` 才是全端都有的那个。
 *
 * 所以：统一走 `uni.request`，**显式超时**，并且如实区分
 * 「连不上」与「连上了但字段不可见」。
 *
 * ## 本模块的函数不 reject
 *
 * 永远 resolve，由 `reachable` 字段告诉调用方请求有没有真的到达对端。
 * 网络失败是**常态**（离线、弱网、relay 挂了），不是异常——把它做成
 * 异常就注定有人写 `catch` 里吞掉，然后界面永远停在 loading。
 */

/** 默认超时。配对是一次交互内的往返，8s 足够；再久就是真的有问题了。 */
const DEFAULT_TIMEOUT_MS = 8000

export interface HttpOptions {
  url: string
  method?: 'GET' | 'POST'
  data?: unknown
  header?: Record<string, string>
  timeout?: number
}

export interface HttpResult {
  /** 请求到达了对端且拿到响应（不代表业务成功，看 statusCode）。 */
  reachable: boolean
  statusCode: number
  data: unknown
  /** 响应头，已统一小写键名。跨域环境下只含该环境允许暴露的那些。 */
  headers: Record<string, string>
  /** 不可达时的机器可读原因，用于文案分支。 */
  fail?: 'timeout' | 'network'
}

/** 响应头键名归一化：不同平台给的大小写不一致。 */
function lowerKeys(h: Record<string, string> | undefined): Record<string, string> {
  const out: Record<string, string> = {}
  if (!h) return out
  for (const k of Object.keys(h)) out[k.toLowerCase()] = h[k]
  return out
}

/**
 * 发一个请求。**不抛异常** —— 失败也 resolve，用 `reachable:false` 表示。
 */
export function request(opts: HttpOptions): Promise<HttpResult> {
  return new Promise((resolve) => {
    let settled = false
    const done = (r: HttpResult) => {
      if (settled) return
      settled = true
      resolve(r)
    }
    try {
      uni.request({
        url: opts.url,
        method: opts.method ?? 'GET',
        data: opts.data as never,
        header: opts.header,
        timeout: opts.timeout ?? DEFAULT_TIMEOUT_MS,
        success: (res) =>
          done({
            reachable: true,
            statusCode: res.statusCode,
            data: res.data,
            headers: lowerKeys(res.header as Record<string, string>),
          }),
        // 连对端都没到：DNS / TLS / 断网 / 超时。分不清是哪种，但**必须**
        // 和「到了但字段看不见」分开报——这两件事的排查方向完全不同。
        fail: (err) =>
          done({
            reachable: false,
            statusCode: 0,
            data: null,
            headers: {},
            fail: /timeout/i.test(String(err?.errMsg ?? '')) ? 'timeout' : 'network',
          }),
      })
    } catch (e) {
      // uni.request 本身抛（参数非法等）也归到「不可达」，不外泄异常。
      done({ reachable: false, statusCode: 0, data: null, headers: {}, fail: 'network' })
    }
  })
}

/** 取一个响应头（大小写不敏感）。取不到返回 `null`，不返回假值。 */
export function header(res: HttpResult, name: string): string | null {
  return res.headers[name.toLowerCase()] ?? null
}
