/**
 * 会话连接：手机 ↔ 电脑的常驻链路（配对之后的**全部**流量走这里）。
 *
 * ## 为什么单独一个文件
 *
 * 配对（`direct.ts`）是一次性的、自己开一条 socket；会话是长连、要收发两向、
 * 每帧还要过 E2EE。把这些摊在页面里会让 `pages/chat` 变成一堆传输细节的
 * 堆积处，而页面本该只管「把明文交给 UI」。这里对外只暴露两个口子：
 * `onPlain`（入站明文）与 `send`（出站明文）。
 *
 * ## 一帧出去要经过的四道手续
 *
 * 1. **明文 → JSON**：`{"type":"send","text":…}`。这是 hermix 的
 *    `WsClientMessage`（`#[serde(tag="type")]`），PC 侧 `handle_ws_stream`
 *    直接反序列化，字段名错了整轮静默——没有任何错误码会回来。
 * 2. **密封**：`sealBody(…, {receiverPublic: PC 公钥})`。relay 从 v2 起拒收
 *    明文 route 帧（`apps/relay/src/proto.js::isSealed`），裸发会被丢。
 * 3. **包信封**：`{ct, nonce, ep, eid}`，`RelayClient` 塞进 envelope 的 payload。
 * 4. **签名**：`RelayClient.sendEnvelope` 自动带 `auth`（hmxd 设备凭据 + 签名）。
 *
 * ## 冷启动必须重发 key.exchange
 *
 * 私钥只在内存里，冷启动就是新的一把，而 PC 侧存的旧公钥随之作废。不重发的
 * 症状是「发得出去、收不到回复」：PC 解不开手机新发的帧，且不会报错。
 * `E2eeCtx::learn_peer` 是 upsert，所以重发是安全覆盖而非累积。
 */
import { activeDevice } from './pairing'
import {
  b64,
  b64decode,
  buildKeyExchangeEnvelope,
  generateIdentity,
  importPublicKey,
  openBody,
  randomBytes,
  sealBody,
  type Identity,
} from './crypto'
import { RelayClient } from './relayClient'
import { PROTO_VERSION } from './envelope'
import { ReplayWindow } from './replay'
import { SyncOrdering, isSyncResult, type SyncResult } from './syncOrder'

/**
 * 等 `sync.result` 的上限。
 *
 * 超时不代表成功，只是**不再阻塞实时帧**：PC 侧版本较老时不认识 `sync`，
 * 会把请求整帧丢弃且不回错误，没有这个兜底，重连后所有实时帧都会堆在
 * `pendingLive` 里不显示——比不补发更糟。
 */
const SYNC_TIMEOUT_MS = 3000

/** 会话 id。PC 侧按 `session_id` 拆包，格式必须与它一致。 */
export function chatSessionId(deviceId: string): string {
  return `im:device:${deviceId}`
}

/**
 * 帧头（AAD）。与 Rust 侧 `Envelope::aad()` 逐字节一致：
 * `v={ver};tag={tag:?};sid={sid};nid={nid}`，`tag` 用变体名 `Route`。
 *
 * `nid` 恒为空串——手机发往 PC 的 envelope 不带 node_id，PC 也按空串算 AAD。
 * 配对回信这条路已经实跑通过（拿到 device_token），所以这不是推断。
 */
export function chatFrameHeader(sid: string): Uint8Array {
  return new TextEncoder().encode(`v=${PROTO_VERSION};tag=Route;sid=${sid};nid=`)
}

export interface SealedEnvelope {
  ct: string
  nonce: string
  ep: string
  eid: string
}

export interface ChatSessionHandlers {
  /** 进来一帧**明文** JSON。UI 状态只能从这里变。 */
  onPlain: (json: string) => void
  onState: (s: 'connecting' | 'open' | 'closed' | 'error') => void
  onError: (message: string) => void
}

export class ChatSession {
  private client: RelayClient | null = null
  private identity: Identity | null = null
  private pcPublic: Uint8Array | null = null
  private deviceId = ''
  private sid = ''
  private ready = false
  /** 连接进行中（start 已调用，onOpen 尚未触发）。 */
  private connecting = false
  /** 等连接就绪的 promise；resolve 时 this.ready 必为 true。 */
  private startDeferred: { resolve: () => void; reject: (e: unknown) => void } | null = null
  private readonly h: ChatSessionHandlers
  /** 出站帧重放窗口（relay 是威胁模型里的潜在攻击者）。 */
  private readonly replay = new ReplayWindow()
  /** 补发结果的顺序控制（实时帧与补发流的竞态由它兜）。 */
  private readonly order: SyncOrdering
  private syncTimer: ReturnType<typeof setTimeout> | null = null

  constructor(handlers: ChatSessionHandlers) {
    this.h = handlers
    this.order = new SyncOrdering(
      (text) => this.h.onPlain(text),
      // 环形日志覆盖掉的那一段真的没了。明确告知用户，不假装完整。
      () => this.h.onError('断连期间的部分历史已超出缓存上限丢失，建议回电脑查看完整记录。'),
    )
  }

  /** 当前是否具备收发条件（已建连 + 密钥已交换）。 */
  get connected(): boolean {
    return this.ready
  }

  /** 是否正在连接过程中（可区分「已断开」和「正在建连」）。 */
  get isConnecting(): boolean {
    return this.connecting
  }

  /**
   * 等连接就绪的 Promise——resolve 时 this.ready == true。
   * 幂等：已起过（this.client 存在）时返回已存在的 deferred promise。
   */
  start(): Promise<void> {
    // this.client 非空不代表可用：destroyClient() 后 client 还在但已 closed。
    // 此时 _doStart 必须重建。
    if (this.client && !this.client.isClosed) {
      return this.startDeferred
        ? new Promise((r) => this.startDeferred!.resolve)
        : Promise.resolve()
    }
    return this._doStart()
  }

  private _doStart(): Promise<void> {
    this.connecting = true
    return new Promise((resolve, reject) => {
      this.startDeferred = { resolve, reject }
      const dev = activeDevice()
      if (!dev?.token || !dev.relayBase || !dev.relayToken || !dev.pcPublicKey || !dev.id) {
        this.h.onError('这台手机还没配对，或配对信息不全。请重新扫码配对。')
        this.h.onState('error')
        this.connecting = false
        resolve()
        return
      }

      // 每次冷启动一把新私钥（内存里，不落盘）。
      void (async () => {
        try {
          this.identity = await generateIdentity()
          this.pcPublic = await importPublicKey(b64decode(dev.pcPublicKey))
          this.deviceId = dev.id
          this.sid = chatSessionId(dev.id)

          this.client = new RelayClient({
            baseUrl: dev.relayBase,
            relayToken: dev.relayToken,
            deviceToken: dev.token,
            sessionId: this.sid,
            nodeId: '',
            onFrame: (payload) => void this.onSealedFrame(payload),
            onState: (s) => {
              if (s === 'open') void this.onOpen()
              else {
                this.ready = false
                this.h.onState(s)
              }
            },
            onError: (m) => {
              // 401（平台透出状态码）与「已被中转拒绝」（App 端判活探测的结论）
              // 都意味着凭据已死：销毁 client，下次 start() 会重建（用新 token）。
              if (/401|凭据已被撤销|已被中转拒绝/i.test(m)) this.destroyClient()
              this.h.onError(m)
            },
          })
          this.client.start()
        } catch (e) {
          reject(e)
        }
      })()
    })
  }

  stop(): void {
    this.client?.close()
    this.client = null
    this.ready = false
    this.connecting = false
    // 若有未完成的 start() promise，让它立即 resolve，避免等待已失效的连接。
    this.startDeferred?.resolve()
    this.startDeferred = null
  }

  /**
   * 收到不可恢复错误后销毁 client。下次 start() 会重建。
   * 401（撤销）时用：旧 token 作废，不重建就不换新 token。
   */
  destroyClient(): void {
    this.client?.close()
    this.client = null
    this.ready = false
    this.connecting = false
    this.startDeferred?.resolve()
    this.startDeferred = null
  }

  /** 建连成功后立刻补一次密钥交换——PC 得先学到这把新公钥才回得了帧。 */
  private async onOpen(): Promise<void> {
    if (!this.identity || !this.pcPublic || !this.deviceId) return
    const { payload } = await buildKeyExchangeEnvelope({
      pcPublic: this.pcPublic,
      deviceId: this.deviceId,
      ownPublic: this.identity.publicKey,
      frameId: randomBytes(16),
      header: chatFrameHeader(this.sid),
    })
    if (!this.client?.send(JSON.stringify(payload))) {
      this.h.onError('密钥交换没能发出去（连接可能已断），稍后会自动重连。')
      return
    }
    this.ready = true
    this.connecting = false
    this.startDeferred?.resolve()
    this.startDeferred = null
    this.h.onState('open')
    // 建连即补：把上一次连接到现在漏掉的事件要回来。丢包/切网/后台被挂起
    // 都会走到这里，而 PC 侧只在日志里留着它们——不主动要就永远看不到。
    void this.requestSync()
  }

  /** 入站：密封信封 → 明文 JSON。解不开就丢这一帧，不打断整条会话。 */
  private async onSealedFrame(rawPayload: string): Promise<void> {
    if (!this.identity) return
    let sf: SealedEnvelope
    try {
      sf = JSON.parse(rawPayload) as SealedEnvelope
    } catch {
      return
    }
    // 重投的帧是字节级复制（relay 没有密钥，封不出新 `eid`），所以 eid 相同
    // 就是同一条旧帧。必须**在解密前**判：否则 relay 重投一条旧
    // `approval.request` 会让审批按钮重新可点。
    if (!this.replay.accept(sf.eid)) return
    try {
      const plain = await openBody(
        { nonce: b64decode(sf.nonce), ciphertext: b64decode(sf.ct) },
        {
          frameId: b64decode(sf.eid),
          header: chatFrameHeader(this.sid),
          direction: 'n2d',
          receiverPrivate: this.identity.privateKey,
          ephemeralPublic: await importPublicKey(b64decode(sf.ep)),
        },
      )
      this.dispatch(new TextDecoder().decode(plain))
    } catch {
      // 解不开的常见原因是密钥没换对（PC 侧还没学到本机新公钥）。
      // 这种帧丢就丢了，下一次 key.exchange 之后自然恢复。
    }
  }

  /**
   * 明文 → UI 的唯一入口。补发回信在这里被拆开，不交给 UI 当普通帧。
   *
   * 拆开是必须的：`sync.result` 里的 `events[]` 每一条都是**当时**下发的
   * 完整帧（同 `assistant.message` 形状），而手机 UI 只认单条事件。
   */
  private dispatch(text: string): void {
    if (isSyncResult(text)) {
      try {
        this.order.applyResult(JSON.parse(text) as SyncResult)
      } catch {
        // 解析失败就当没收到，等下次重连再要一次。
      }
      return
    }
    this.order.offer(text)
  }

  /** 补发请求。失败不致命：下一次重连会再要一次。 */
  private async requestSync(): Promise<void> {
    if (!this.client || !this.identity || !this.pcPublic) return
    this.order.begin()
    // 兜底：PC 没回 sync.result（旧版本 / 帧丢了）时不能一直卡着——
    // 那样后续所有实时帧都会被扣在队列里不显示。
    this.syncTimer = setTimeout(() => {
      this.syncTimer = null
      this.order.end()
    }, SYNC_TIMEOUT_MS)
    const ok = await this.sendPlain({ type: 'sync', since_seq: this.order.lastSeqSeen })
    if (!ok) {
      // 发不出去就直接放开，别等超时。
      if (this.syncTimer) clearTimeout(this.syncTimer)
      this.syncTimer = null
      this.order.end()
    }
  }

  /**
   * 发一条用户消息。未就绪返回 false，由调用方决定怎么提示。
   *
   * 明文形态必须是 `WsClientMessage::Send`：`{"type":"send","text":…}`。
   * PC 侧用 `#[serde(tag="type", rename_all="snake_case")]` 解析，字段名不对
   * 不会回错误码，整轮就静默消失——所以这个字符串由 Rust 侧的枚举定义。
   */
  async send(text: string): Promise<boolean> {
    return this.sendPlain({ type: 'send', text })
  }

  /** 封一帧明文上行。`send` 与补发请求共用，密封细节只写一次。 */
  private async sendPlain(obj: unknown): Promise<boolean> {
    if (!this.client || !this.identity || !this.pcPublic || !this.ready) return false
    const plain = new TextEncoder().encode(JSON.stringify(obj))
    // frameId 同时进 HKDF salt 与信封的 `eid`，两处必须同一份。
    const frameId = randomBytes(16)
    const { sealed, ephemeralPublic } = await sealBody(plain, {
      receiverPublic: this.pcPublic,
      frameId,
      header: chatFrameHeader(this.sid),
      direction: 'd2n',
    })
    const envelope: SealedEnvelope = {
      ct: b64(sealed.ciphertext),
      nonce: b64(sealed.nonce),
      ep: b64(ephemeralPublic),
      eid: b64(frameId),
    }
    return this.client.send(JSON.stringify(envelope))
  }
}