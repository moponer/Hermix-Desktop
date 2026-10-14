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
  private readonly h: ChatSessionHandlers

  constructor(handlers: ChatSessionHandlers) {
    this.h = handlers
  }

  /** 当前是否具备收发条件（已建连 + 密钥已交换）。 */
  get connected(): boolean {
    return this.ready
  }

  /**
   * 起连接。**幂等**：已起过就直接返回，避免页面 onShow 反复调时叠连接。
   */
  async start(): Promise<void> {
    if (this.client) return
    const dev = activeDevice()
    if (!dev?.token || !dev.relayBase || !dev.relayToken || !dev.pcPublicKey || !dev.id) {
      this.h.onError('这台手机还没配对，或配对信息不全。请重新扫码配对。')
      this.h.onState('error')
      return
    }

    // 每次冷启动一把新私钥（内存里，不落盘）。
    this.identity = await generateIdentity()
    this.pcPublic = await importPublicKey(b64decode(dev.pcPublicKey))
    // deviceId 与 sid 都在这里定死：后面 onOpen / onSealedFrame 都可能晚于这次
    // 读取才跑（重连有退避），那时再查 activeDevice() 拿到的是别的状态。
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
          // 连接一断就撤销就绪标记：否则界面会显示「已连接」而实际发不出去。
          this.ready = false
          this.h.onState(s)
        }
      },
      onError: (m) => this.h.onError(m),
    })
    this.client.start()
  }

  stop(): void {
    this.client?.close()
    this.client = null
    this.ready = false
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
    this.h.onState('open')
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
      this.h.onPlain(new TextDecoder().decode(plain))
    } catch {
      // 解不开的常见原因是密钥没换对（PC 侧还没学到本机新公钥）。
      // 这种帧丢就丢了，下一次 key.exchange 之后自然恢复。
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
    if (!this.client || !this.identity || !this.pcPublic || !this.ready) return false
    const plain = new TextEncoder().encode(JSON.stringify({ type: 'send', text }))
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