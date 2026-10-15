/**
 * 到 relay 的 WS 客户端：一条连接给 relay 选路，帧里带 hermix 凭据。
 *
 * ## 两条链路的分工（别混）
 *
 * - `relay 凭据 hrel_*` → **HTTP 握手头**。relay 认的是**连接**，不是帧，
 *   所以它必须随 `connect` 的请求发出去，只在帧里带会被 401。
 *   relay 侧只认 `Authorization: Bearer`（`auth.js` 的 `bearerFromHeaders`，
 *   `?token=` 查询通道已被刻意删除），所以必须走 `uni.connectSocket({header})`。
 * - `hmxd_* 设备凭据 + 签名` → **帧内 `auth` 字段**。App 不直接向 hermix 发
 *   HTTP，中间那跳是 relay，请求头没有落脚的地方；凭据由 hermix 在 PC 侧验
 *   （`mobile_ws::verify_frame_auth`），relay 只透传。
 *
 * ## session_id 是鉴权的一部分
 *
 * 签名覆盖 `METHOD\npath`，path 由 `session_id` 重建（`/v1/ws-stream/{sid}`），
 * 所以**改 session_id 就等于改要签的路径**，旧签名立刻失效。这不是"路由
 * 参数"，是鉴权模型的一部分。
 */
import { signatureHeader } from './signature'
import { encodeEnvelope, parseEnvelope, helloFrame, PROTO_VERSION, type Envelope } from './envelope'

export interface RelayClientOptions {
  /** relay 根地址，如 `https://relay.example.com` */
  baseUrl: string
  /** relay 凭据 `hrel_*`（握手头用） */
  relayToken: string
  /** hermix 设备 token `hmxd_*`（帧内 auth 用） */
  deviceToken: string
  /** 目标会话 `im:device:{device_id}` */
  sessionId: string
  /** 本机标识 */
  nodeId: string
  onFrame: (payload: string) => void
  onState?: (s: 'connecting' | 'open' | 'closed' | 'error') => void
  onError?: (message: string) => void
}

/** 首连 / 重连的等待。默认 2s 起，上限 30s（手机端不必等 PC 起来）。 */
function backoff(attempt: number): number {
  const base = 2000
  return Math.min(base * Math.pow(2, Math.min(attempt, 16)), 30000)
}

/**
 * 当前是不是浏览器环境（H5 / 小程序 webview）。
 *
 * 存在的理由：relay 凭据只走 **HTTP 握手头**（`Authorization: Bearer`），
 * 而浏览器的 `WebSocket` 按规范**无法设置自定义头**。于是 H5 下每一次都必然
 * 被 relay 401——而浏览器又把状态码吞成通用错误，`onError` 里根本看不到
 * "401"。结果就是所有 H5 尝试都落进「连不上中转，确认手机能上网」：网络其实
 * 好好的，用户却被引去查一个完全错误的方向。识别出来才说得上真话。
 */
export function isBrowserEnv(): boolean {
  try {
    const g = globalThis as { document?: unknown; window?: unknown }
    return typeof g.document !== 'undefined' && typeof g.window !== 'undefined'
  } catch {
    return false
  }
}

/** 浏览器里跑必然失败时的唯一诚实说法。 */
export const BROWSER_UNSUPPORTED_HINT =
  '这一步只能在手机 App 里做：relay 凭据必须随 WebSocket 握手头发出，' +
  '而浏览器的 WebSocket 带不了自定义头，一定会被中转拒绝。' +
  '这不是网络问题——请用 Hermix M App 扫码配对。'

/**
 * 拼 relay 的 WS 地址。导出给配对流程复用（配对也是一条 phone 连接）。
 *
 * ⚠️ **`https://` 分支不是可选的**：`parsePairingUrl` 原样保留二维码里的
 * scheme，而配对二维码永远是 `https://relay/pair?…`（明文信道不带凭据，
 * 见 `parsePairingUrl` 的收窄正则）。漏掉这一支会拼出
 * `wss://https://relay/relay/phone` 这种畸形地址，**连不上且看不出原因**——
 * 表现为「连不上中转」，而中转、凭据、网络全都是好的。
 * 必须与 Rust 侧 `relay_client::relay_ws_url` 逐分支对齐。
 */
export function relayWsUrl(base: string, kind: 'phone' | 'node'): string {
  const b = base.trim().replace(/\/+$/, '')
  if (b.startsWith('wss://')) return `${b}/relay/${kind}`
  if (b.startsWith('ws://')) return `${b}/relay/${kind}`
  if (b.startsWith('https://')) return `wss://${b.slice('https://'.length)}/relay/${kind}`
  if (b.startsWith('http://')) return `ws://${b.slice('http://'.length)}/relay/${kind}`
  return `wss://${b}/relay/${kind}` // 裸 host 补 wss：外网默认走 TLS
}

/**
 * 连上 relay 并持续收发，直到 `close()` 被调用。
 *
 * 不自己返回：连接断了要按退避重连（PC 可能还没起来 / relay 可能重启了），
 * 那是这个循环的职责，调用方只管发帧。
 */
export class RelayClient {
  private task: UniApp.SocketTask | null = null
  /** 自己记连通性：`SocketTask` 不暴露 `readyState`。 */
  private open = false
  private closed = false
  private attempt = 0
  private readonly opts: RelayClientOptions

  constructor(opts: RelayClientOptions) {
    this.opts = opts
  }

  /** 外部可查：连接已永久关闭（401 / 用户主动 close）。 */
  get isClosed(): boolean {
    return this.closed
  }

  start(): void {
    this.connect()
  }

  close(): void {
    this.closed = true
    this.open = false
    this.task?.close({})
    this.task = null
  }

  /** 往当前会话发一帧内层文本（会连带签名）。未连上时返回 false。 */
  send(payload: string): boolean {
    if (!this.task || !this.open) return false
    this.sendEnvelope(payload).catch((e) => this.opts.onError?.(String(e)))
    return true
  }

  private async sendEnvelope(payload: string): Promise<void> {
    const sig = await signatureHeader(
      this.opts.deviceToken,
      'GET',
      `/v1/ws-stream/${this.opts.sessionId}`,
    )
    const env: Envelope = {
      protoVersion: PROTO_VERSION,
      tag: 'route',
      sessionId: this.opts.sessionId,
      auth: { bearer: this.opts.deviceToken, signature: sig },
      payload,
    }
    this.task?.send({ data: encodeEnvelope(env) })
  }

  private connect(): void {
    if (this.closed) return
    this.opts.onState?.('connecting')
    // 必须用 `uni.connectSocket` 而不是全局 `WebSocket`。
    //
    // relay 的凭据 `hrel_*` 只认 **HTTP 握手头**：`Authorization: Bearer …`
    // （relay `server.js` 的 upgrade handler 直接 401，`auth.js` 的
    // `bearerFromHeaders` 只读这个头，`?token=` 查询通道已被刻意删除）。
    // 浏览器的 `new WebSocket(url)` **无法设置自定义头**，所以那条写法必然 401。
    // `uni.connectSocket` 在 App / 小程序端由运行时映射到原生实现，能带头。
    //
    // 交付目标只有 App（Android）：`uni.connectSocket` 在那里由运行时映射到
    // 原生实现，能带头。H5 不在范围内。
    if (!this.opts.relayToken.trim()) {
      this.scheduleRetry('没填 relay 凭据（hrel_*），无法连接')
      return
    }
    let task: UniApp.SocketTask
    try {
      task = uni.connectSocket({
        url: relayWsUrl(this.opts.baseUrl, 'phone'),
        header: { Authorization: `Bearer ${this.opts.relayToken.trim()}` },
        complete: () => undefined,
      })
    } catch (e) {
      this.scheduleRetry(String(e))
      return
    }
    this.task = task

    task.onOpen(() => {
      this.open = true
      this.attempt = 0
      this.opts.onState?.('open')
      const hello: Envelope = {
        ...helloFrame('', this.opts.nodeId, this.opts.sessionId),
      }
      task.send({ data: encodeEnvelope(hello) })
    })
    task.onMessage((ev) => {
      const parsed = parseEnvelope(typeof ev.data === 'string' ? ev.data : '')
      if (!parsed.ok) {
        if (parsed.versionMismatch) {
          this.opts.onError?.('relay 协议版本与 hermix 不一致，请两端一起升级')
        }
        return
      }
      const env = parsed.envelope
      if (env.tag === 'route' && env.payload !== undefined) {
        this.opts.onFrame(env.payload)
      }
    })
    task.onError((e) => {
      this.open = false
      const msg = String((e as { errMsg?: string })?.errMsg ?? '')
      // 401 = 凭据被撤销。永远停重试——重试同一条被撤的 token 没意义。
      if (/401|unauthorized/i.test(msg)) {
        this.closed = true
        this.opts.onState?.('error')
        this.opts.onError?.('relay 拒绝了这条连接（401）：凭据已被撤销，请重新扫码配对')
        return
      }
      // 浏览器环境是「必然失败」而非「可能失败」：握手头发不出去，100% 被 401，
      // 而浏览器把状态码吞掉了。归到通用文案等于让人去查网络。
      if (isBrowserEnv()) {
        this.closed = true
        this.opts.onState?.('error')
        this.opts.onError?.(BROWSER_UNSUPPORTED_HINT)
        return
      }
      this.opts.onState?.('error')
      this.scheduleRetry('relay 连接错误')
    })
    task.onClose((res?: { code?: number }) => {
      if (this.closed) return
      this.open = false
      this.task = null
      this.opts.onState?.('closed')
      // relay 的关闭码要说人话（apps/relay/src/server.js）：4009 = 这台电脑
      // 已经连着另一台手机，重连解决不了，得先在电脑端撤销原设备。
      // code 缺失（部分平台不透出）时回落通用文案，照常退避重连。
      const reason =
        res?.code === 4009
          ? '这台电脑已连接另一台手机：请先在电脑端「设备管理」撤销原设备'
          : 'relay 连接断开'
      this.scheduleRetry(reason)
    })
  }

  private scheduleRetry(reason: string): void {
    if (this.closed) return
    const wait = backoff(this.attempt)
    this.attempt += 1
    this.opts.onError?.(`${reason}，${Math.round(wait / 1000)}s 后重连`)
    setTimeout(() => this.connect(), wait)
  }
}
