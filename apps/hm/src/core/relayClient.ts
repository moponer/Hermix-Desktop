/**
 * 到 relay 的 WS 客户端：一条连接给 relay 选路，帧里带 hermix 凭据。
 *
 * ## 两条链路的分工（别混）
 *
 * - `relay 凭据 hrel_*` → **HTTP 握手头**。relay 认的是**连接**，不是帧，
 *   所以它必须随 `connect` 的请求发出去，只在帧里带会被 401。
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

function relayWsUrl(base: string, kind: 'phone' | 'node'): string {
  const b = base.trim().replace(/\/+$/, '')
  if (b.startsWith('ws://')) return `${b}/relay/${kind}`
  if (b.startsWith('http://')) return `ws://${b.slice(7)}/relay/${kind}`
  return `wss://${b}/relay/${kind}`
}

/**
 * 连上 relay 并持续收发，直到 `close()` 被调用。
 *
 * 不自己返回：连接断了要按退避重连（PC 可能还没起来 / relay 可能重启了），
 * 那是这个循环的职责，调用方只管发帧。
 */
export class RelayClient {
  private ws: WebSocket | null = null
  private closed = false
  private attempt = 0
  private readonly opts: RelayClientOptions

  constructor(opts: RelayClientOptions) {
    this.opts = opts
  }

  start(): void {
    this.connect()
  }

  close(): void {
    this.closed = true
    this.ws?.close()
    this.ws = null
  }

  /** 往当前会话发一帧内层文本（会连带签名）。未连上时返回 false。 */
  send(payload: string): boolean {
    const ws = this.ws
    if (!ws || ws.readyState !== WebSocket.OPEN) return false
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
    this.ws?.send(encodeEnvelope(env))
  }

  private connect(): void {
    if (this.closed) return
    this.opts.onState?.('connecting')
    // uni-app 的 WebSocket 在 H5 下就是浏览器 WebSocket；小程序/App 端由运行时
    // 映射到原生实现。两边都支持自定义 header，但只有 H5 一定支持 —— 所以
    // 握手头缺失时要显式报出来，而不是连上后被 relay 401 然后一脸茫然。
    let ws: WebSocket
    try {
      ws = new WebSocket(relayWsUrl(this.opts.baseUrl, 'phone'))
    } catch (e) {
      this.scheduleRetry(String(e))
      return
    }
    this.ws = ws

    ws.onopen = () => {
      this.attempt = 0
      this.opts.onState?.('open')
      const hello: Envelope = {
        ...helloFrame('', this.opts.nodeId),
        // relay 认连接上的凭据；hash 由 relay 侧自行算，这里只报身份
      }
      ws.send(encodeEnvelope(hello))
    }
    ws.onmessage = (ev: MessageEvent) => {
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
    }
    ws.onerror = () => {
      this.opts.onState?.('error')
    }
    ws.onclose = () => {
      this.ws = null
      this.opts.onState?.('closed')
      this.scheduleRetry('relay 连接断开')
    }
  }

  private scheduleRetry(reason: string): void {
    if (this.closed) return
    const wait = backoff(this.attempt)
    this.attempt += 1
    this.opts.onError?.(`${reason}，${Math.round(wait / 1000)}s 后重连`)
    setTimeout(() => this.connect(), wait)
  }
}
